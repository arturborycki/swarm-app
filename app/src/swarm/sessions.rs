//! The daemon's sessions as an app-wide model, kept current by a background
//! watcher so the title bar, sidebar and picker can render them.

use warp_terminal::local_tty::daemon::{self, DaemonSession};
use warpui::{Entity, ModelContext, SingletonEntity};

pub struct SwarmSessions {
    sessions: Vec<DaemonSession>,
}

pub enum SwarmSessionsEvent {
    Changed,
}

impl SwarmSessions {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        if super::use_daemon_sessions() {
            daemon::watch_sessions(move |sessions| tx.try_send(sessions).is_ok());
        }
        let _ = ctx.spawn_stream_local(rx, Self::on_sessions, |_, _| {});
        SwarmSessions { sessions: Vec::new() }
    }

    fn on_sessions(&mut self, sessions: Vec<DaemonSession>, ctx: &mut ModelContext<Self>) {
        if sessions != self.sessions {
            self.sessions = sessions;
            ctx.emit(SwarmSessionsEvent::Changed);
        }
    }

    /// Running sessions no client is attached to, oldest first.
    pub fn detached(&self) -> impl Iterator<Item = &DaemonSession> {
        self.sessions.iter().filter(|s| s.is_running() && s.clients.is_empty())
    }
}

impl Entity for SwarmSessions {
    type Event = SwarmSessionsEvent;
}

impl SingletonEntity for SwarmSessions {}
