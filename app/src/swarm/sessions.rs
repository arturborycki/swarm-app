//! The daemons' sessions as an app-wide model, kept current by background
//! watchers so the title bar, sidebar and session menus can render them. The
//! local daemon is always watched; a remote host once the user connects to it.

use std::collections::BTreeMap;

use warp_terminal::local_tty::daemon::{self, DaemonSession};
use warpui::{Entity, ModelContext, SingletonEntity};

type Host = Option<String>;

pub struct SwarmSessions {
    by_host: BTreeMap<Host, Vec<DaemonSession>>,
    tx: async_channel::Sender<(Host, Vec<DaemonSession>)>,
}

pub enum SwarmSessionsEvent {
    Changed,
}

impl SwarmSessions {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let _ = ctx.spawn_stream_local(rx, Self::on_sessions, |_, _| {});
        let me = SwarmSessions { by_host: BTreeMap::new(), tx };
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
        if self.by_host.get(&host) != Some(&sessions) {
            self.by_host.insert(host, sessions);
            ctx.emit(SwarmSessionsEvent::Changed);
        }
    }

    /// Running local sessions no client is attached to, oldest first.
    pub fn detached(&self) -> impl Iterator<Item = &DaemonSession> {
        self.sessions_on(None).iter().filter(|s| s.is_running() && s.clients.is_empty())
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
