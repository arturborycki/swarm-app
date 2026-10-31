//! The daemons' sessions as an app-wide model, kept current by background
//! watchers so the title bar, sidebar and session menus can render them. The
//! local daemon is always watched; a remote host once the user connects to it.

use std::collections::{BTreeMap, BTreeSet};

use warp_terminal::local_tty::daemon::{self, DaemonSession};
use warpui::accessibility::{AccessibilityContent, WarpA11yRole};
use warpui::{Entity, ModelContext, SingletonEntity};

type Host = Option<String>;

pub struct SwarmSessions {
    by_host: BTreeMap<Host, Vec<DaemonSession>>,
    /// Things the user should look at (sessions that failed, connections that
    /// were lost), by (host, session id): what to tell them.
    attention: BTreeMap<(Host, String), String>,
    /// Daemon session ids whose tab is reconnecting, as last announced.
    reconnecting: BTreeSet<String>,
    tx: async_channel::Sender<(Host, Vec<DaemonSession>)>,
}

/// A session's state as tabs and the sidebar show it: each has its own shape
/// as well as a color, so it reads without color too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMark {
    /// The shell is waiting at a prompt (●).
    Idle,
    /// A command is running in the foreground (◐).
    Busy,
    /// It ended with an error, or its connection was lost for good (!).
    Attention,
    /// The session's process has ended (○).
    Exited,
    /// The tab lost its connection to the daemon and is retrying (⟳).
    Reconnecting,
}

impl SessionMark {
    /// The mark's shape and color, as tabs and sidebar rows draw it.
    pub fn icon(
        self,
        theme: &warp_core::ui::theme::WarpTheme,
    ) -> (warp_core::ui::Icon, warp_core::ui::theme::Fill) {
        use warp_core::ui::Icon;
        let colors = theme.terminal_colors().normal;
        match self {
            SessionMark::Idle => (Icon::CircleFilled, colors.blue.into()),
            SessionMark::Busy => (Icon::CircleHalf, colors.blue.into()),
            SessionMark::Attention => (Icon::AlertCircle, colors.red.into()),
            SessionMark::Exited => (Icon::CircleRing, theme.disabled_ui_text_color()),
            SessionMark::Reconnecting => (Icon::Refresh, colors.yellow.into()),
        }
    }
}

/// What a tab shows for its daemon session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    pub mark: SessionMark,
    /// Another client (another window, `swarmctl attach`) is attached too.
    pub elsewhere: bool,
    /// The remote host; `None` on this Mac.
    pub host: Option<String>,
    /// The foreground command, while one runs.
    pub command: Option<String>,
    /// The session's shell (its program's name).
    pub shell: Option<String>,
    /// Its working directory, `~`-relative on this Mac.
    pub cwd: Option<String>,
    /// Columns × rows.
    pub size: (u16, u16),
    /// The other clients attached to it ("Swarm on mini-2", "swarmctl").
    pub others: Vec<String>,
    /// It printed something since its tab was last shown.
    pub unseen: bool,
}

impl SessionStatus {
    /// The status in words, for VoiceOver: "devbox, running psql, also open elsewhere".
    pub fn spoken(&self) -> String {
        let mut text = self.host.clone().unwrap_or_else(|| "This Mac".to_string());
        text.push_str(", ");
        match (self.mark, &self.command) {
            (SessionMark::Busy, Some(command)) => text.push_str(&format!("running {command}")),
            (SessionMark::Busy, None) => text.push_str("running a command"),
            (SessionMark::Idle, _) => text.push_str("idle"),
            (SessionMark::Attention, _) => text.push_str("needs attention"),
            (SessionMark::Exited, _) => text.push_str("ended"),
            (SessionMark::Reconnecting, _) => text.push_str("reconnecting"),
        }
        if self.elsewhere {
            text.push_str(", also open elsewhere");
        }
        text
    }
}

pub enum SwarmSessionsEvent {
    Changed,
    /// Something needs the user's attention: (title, body) for a notification.
    Attention { title: String, body: String },
    /// A session printed output nobody has seen yet: the window showing it
    /// marks it seen, others show its tab as having news.
    Output { daemon_id: String, warp_session_id: u64 },
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
        let _ = ctx.spawn_stream_local(link_rx, |me, _, ctx| me.on_link_change(ctx), |_, _| {});
        // Tabs that gave up reconnecting.
        let (lost_tx, lost_rx) = async_channel::unbounded::<(Host, String)>();
        daemon::on_link_lost(move |host, id| {
            let _ = lost_tx.try_send((host, id));
        });
        let _ = ctx.spawn_stream_local(lost_rx, Self::on_link_lost, |_, _| {});
        // Sessions printing while their tab is in the background.
        let (output_tx, output_rx) = async_channel::unbounded::<String>();
        daemon::on_unseen_output(move |id| {
            let _ = output_tx.try_send(id);
        });
        let _ = ctx.spawn_stream_local(output_rx, Self::on_unseen_output, |_, _| {});
        let me = SwarmSessions {
            by_host: BTreeMap::new(),
            attention: BTreeMap::new(),
            reconnecting: BTreeSet::new(),
            tx,
        };
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
            let was_running = previous.iter().any(|p| p.id == s.id && p.is_running()) && !daemon::was_terminated(&s.id);
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
            announce(format!("{title}: {body}"), ctx);
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
        announce(format!("Connection lost: {body}"), ctx);
        ctx.emit(SwarmSessionsEvent::Attention { title: "Connection lost".to_string(), body });
        ctx.emit(SwarmSessionsEvent::Changed);
    }

    /// Tabs started or stopped reconnecting: tell VoiceOver which.
    fn on_link_change(&mut self, ctx: &mut ModelContext<Self>) {
        let now: BTreeSet<String> = daemon::reconnecting_sessions().into_iter().collect();
        for id in now.difference(&self.reconnecting) {
            announce(format!("{} lost its connection; reconnecting", self.spoken_name(id)), ctx);
        }
        for id in self.reconnecting.difference(&now) {
            // A tab that gave up is announced as a lost connection instead.
            if !self.attention.keys().any(|(_, a)| a == id) {
                announce(format!("{} reconnected", self.spoken_name(id)), ctx);
            }
        }
        self.reconnecting = now;
        ctx.emit(SwarmSessionsEvent::Changed);
    }

    fn spoken_name(&self, id: &str) -> String {
        self.by_host
            .iter()
            .find_map(|(host, sessions)| {
                let s = sessions.iter().find(|s| s.id == id)?;
                let name = s.name.clone().unwrap_or_else(|| s.title.clone());
                Some(match host {
                    Some(h) => format!("{name} on {h}"),
                    None => name,
                })
            })
            .unwrap_or_else(|| "A session".to_string())
    }

    /// How many things need the user's attention (the Dock badge).
    pub fn attention_count(&self) -> usize {
        self.attention.len()
    }

    /// Running local sessions no client is attached to, oldest first.
    pub fn detached(&self) -> impl Iterator<Item = &DaemonSession> {
        self.sessions_on(None).iter().filter(|s| s.is_running() && s.clients.is_empty())
    }

    /// The status of the session behind a terminal, by its Warp session id;
    /// `None` for a terminal that isn't a session this app follows.
    pub fn status_for(&self, warp_session_id: u64) -> Option<SessionStatus> {
        let (host, session) = self.by_host.iter().find_map(|(host, sessions)| {
            let s = sessions.iter().find(|s| {
                s.ui.get(daemon::META_SESSION_ID).and_then(|v| v.parse::<u64>().ok()) == Some(warp_session_id)
            })?;
            Some((host, s))
        })?;
        let needs_attention = self.attention.contains_key(&(host.clone(), session.id.clone()));
        let mark = if daemon::is_reconnecting(&session.id) {
            SessionMark::Reconnecting
        } else if needs_attention {
            SessionMark::Attention
        } else if !session.is_running() {
            SessionMark::Exited
        } else if session.foreground.is_some() {
            SessionMark::Busy
        } else {
            SessionMark::Idle
        };
        // This app is one client of the session; any other is someone else.
        let me = daemon::client_name();
        let mut others: Vec<String> = session.clients.iter().map(|c| c.name.clone()).collect();
        if let Some(i) = others.iter().position(|n| *n == me) {
            others.remove(i);
        }
        let cwd = session.cwd.clone().map(|cwd| if host.is_none() { super::home_relative(&cwd) } else { cwd });
        let shell = session.command.first().map(|program| {
            let name = program.rsplit('/').next().unwrap_or(program);
            name.trim_start_matches('-').to_string()
        });
        Some(SessionStatus {
            mark,
            elsewhere: session.is_running() && !others.is_empty(),
            host: host.clone(),
            command: session.foreground.clone(),
            shell,
            cwd,
            size: (session.size.cols, session.size.rows),
            others,
            unseen: daemon::has_unseen_output(&session.id),
        })
    }

    /// The session behind a terminal and its host, by Warp session id.
    pub fn session_for(&self, warp_session_id: u64) -> Option<(Host, &DaemonSession)> {
        self.by_host.iter().find_map(|(host, sessions)| {
            let s = sessions.iter().find(|s| {
                s.ui.get(daemon::META_SESSION_ID).and_then(|v| v.parse::<u64>().ok()) == Some(warp_session_id)
            })?;
            Some((host.clone(), s))
        })
    }

    /// The daemon's id for the session behind a terminal, by Warp session id.
    pub fn daemon_id_for(&self, warp_session_id: u64) -> Option<String> {
        self.by_host
            .values()
            .flatten()
            .find(|s| s.ui.get(daemon::META_SESSION_ID).and_then(|v| v.parse::<u64>().ok()) == Some(warp_session_id))
            .map(|s| s.id.clone())
    }

    fn on_unseen_output(&mut self, daemon_id: String, ctx: &mut ModelContext<Self>) {
        let warp_session_id = self
            .by_host
            .values()
            .flatten()
            .find(|s| s.id == daemon_id)
            .and_then(|s| s.ui.get(daemon::META_SESSION_ID)?.parse::<u64>().ok());
        if let Some(warp_session_id) = warp_session_id {
            ctx.emit(SwarmSessionsEvent::Output { daemon_id, warp_session_id });
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

    /// The remote hosts being followed, in name order.
    pub fn followed_hosts(&self) -> Vec<String> {
        self.by_host.keys().flatten().cloned().collect()
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

/// Has VoiceOver read `text` out (a no-op when no screen reader is running).
fn announce(text: String, ctx: &mut ModelContext<SwarmSessions>) {
    ctx.emit_a11y_content(AccessibilityContent::new_without_help(text, WarpA11yRole::UserAction));
}
