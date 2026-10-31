//! The daemons' sessions as an app-wide model, kept current by background
//! watchers so the title bar, sidebar and session menus can render them. The
//! local daemon is always watched; a remote host once the user connects to it.

use std::collections::BTreeMap;

use warp_terminal::local_tty::daemon::{self, DaemonSession};
use warpui::{Entity, ModelContext, SingletonEntity};

type Host = Option<String>;

pub struct SwarmSessions {
    by_host: BTreeMap<Host, Vec<DaemonSession>>,
    /// Things the user should look at (sessions that failed, connections that
    /// were lost), by (host, session id): what to tell them.
    attention: BTreeMap<(Host, String), String>,
    tx: async_channel::Sender<(Host, Vec<DaemonSession>)>,
}

/// A session's state as the sidebar shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMark {
    /// A command is running in the foreground.
    Busy,
    /// The session's process has ended.
    Exited,
    /// Another client (another window, `swarmctl attach`) is attached too.
    AttachedElsewhere,
    /// The tab lost its connection to the daemon and is retrying.
    Reconnecting,
}

pub enum SwarmSessionsEvent {
    Changed,
    /// Something needs the user's attention: (title, body) for a notification.
    Attention { title: String, body: String },
}

impl SwarmSessions {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let _ = ctx.spawn_stream_local(rx, Self::on_sessions, |_, _| {});
        // Tabs losing and regaining their connection.
        let (link_tx, link_rx) = async_channel::unbounded::<()>();
        daemon::on_link_change(move || {
            let _ = link_tx.try_send(());
        });
        let _ = ctx.spawn_stream_local(link_rx, |_, _, ctx| ctx.emit(SwarmSessionsEvent::Changed), |_, _| {});
        // Tabs that gave up reconnecting.
        let (lost_tx, lost_rx) = async_channel::unbounded::<(Host, String)>();
        daemon::on_link_lost(move |host, id| {
            let _ = lost_tx.try_send((host, id));
        });
        let _ = ctx.spawn_stream_local(lost_rx, Self::on_link_lost, |_, _| {});
        let me = SwarmSessions { by_host: BTreeMap::new(), attention: BTreeMap::new(), tx };
        if super::use_daemon_sessions() {
            me.start_watching(None);
        }
        me
    }

    /// Starts following `host`'s sessions (no-op if already followed).
    pub fn watch_host(&mut self, host: String, ctx: &mut ModelContext<Self>) {
        let key = Some(host);
        if self.by_host.contains_key(&key) {
            return;
        }
        self.by_host.insert(key.clone(), Vec::new());
        self.start_watching(key);
        ctx.emit(SwarmSessionsEvent::Changed);
    }

    fn start_watching(&self, host: Host) {
        let tx = self.tx.clone();
        let key = host.clone();
        daemon::watch_sessions(host, move |sessions| tx.try_send((key.clone(), sessions)).is_ok());
    }

    fn on_sessions(&mut self, (host, sessions): (Host, Vec<DaemonSession>), ctx: &mut ModelContext<Self>) {
        if self.by_host.get(&host) == Some(&sessions) {
            return;
        }
        let place = host.clone().unwrap_or_else(|| "this Mac".to_string());
        let previous = self.by_host.get(&host).cloned().unwrap_or_default();
        let mut news = Vec::new();
        for s in &sessions {
            let was_running = previous.iter().any(|p| p.id == s.id && p.is_running());
            if let (true, daemon::SessionState::Exited { code, signal, .. }) = (was_running, &s.state) {
                let failed = match (code, signal) {
                    (Some(c), _) => (*c != 0).then(|| format!("exited with code {c}")),
                    (None, Some(sig)) => Some(format!("was killed by signal {sig}")),
                    _ => None,
                };
                if let Some(how) = failed {
                    let name = s.name.clone().unwrap_or_else(|| s.title.clone());
                    let body = format!("{name} on {place} {how}.");
                    log::info!("swarm: needs attention: {body}");
                    self.attention.insert((host.clone(), s.id.clone()), body.clone());
                    news.push(("Session ended".to_string(), body));
                }
            }
        }
        // Attention ends when the session is gone, or (a lost connection) when
        // a client is attached to it again.
        self.attention.retain(|(h, id), _| {
            *h != host || sessions.iter().any(|s| &s.id == id && (!s.is_running() || s.clients.is_empty()))
        });
        self.by_host.insert(host, sessions);
        for (title, body) in news {
            ctx.emit(SwarmSessionsEvent::Attention { title, body });
        }
        ctx.emit(SwarmSessionsEvent::Changed);
    }

    fn on_link_lost(&mut self, (host, id): (Host, String), ctx: &mut ModelContext<Self>) {
        let place = host.clone().unwrap_or_else(|| "this Mac".to_string());
        let name = self
            .sessions_on(host.as_deref())
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.name.clone())
            .unwrap_or_else(|| "A session".to_string());
        let body = format!("{name} on {place}: couldn't reconnect. It may still be running there.");
        self.attention.insert((host, id), body.clone());
        ctx.emit(SwarmSessionsEvent::Attention { title: "Connection lost".to_string(), body });
        ctx.emit(SwarmSessionsEvent::Changed);
    }

    /// How many things need the user's attention (the Dock badge).
    pub fn attention_count(&self) -> usize {
        self.attention.len()
    }

    /// Running local sessions no client is attached to, oldest first.
    pub fn detached(&self) -> impl Iterator<Item = &DaemonSession> {
        self.sessions_on(None).iter().filter(|s| s.is_running() && s.clients.is_empty())
    }

    /// What the sidebar marks the session behind a terminal with, by its Warp
    /// session id; `None` for an idle shell or a session this app doesn't follow.
    pub fn mark_for(&self, warp_session_id: u64) -> Option<SessionMark> {
        let session = self.by_host.values().flatten().find(|s| {
            s.ui.get(daemon::META_SESSION_ID).and_then(|v| v.parse::<u64>().ok()) == Some(warp_session_id)
        })?;
        if daemon::is_reconnecting(&session.id) {
            Some(SessionMark::Reconnecting)
        } else if !session.is_running() {
            Some(SessionMark::Exited)
        } else if session.clients.len() > 1 {
            Some(SessionMark::AttachedElsewhere)
        } else if session.foreground.is_some() {
            Some(SessionMark::Busy)
        } else {
            None
        }
    }

    /// The daemon's name for the session behind a terminal, by Warp session id.
    pub fn name_for(&self, warp_session_id: u64) -> Option<String> {
        self.by_host
            .values()
            .flatten()
            .find(|s| s.ui.get(daemon::META_SESSION_ID).and_then(|v| v.parse::<u64>().ok()) == Some(warp_session_id))?
            .name
            .clone()
    }

    /// `host`'s sessions as last seen (`None`: the local daemon).
    pub fn sessions_on(&self, host: Option<&str>) -> &[DaemonSession] {
        self.by_host.get(&host.map(str::to_string)).map(Vec::as_slice).unwrap_or(&[])
    }
}

impl Entity for SwarmSessions {
    type Event = SwarmSessionsEvent;
}

impl SingletonEntity for SwarmSessions {}
