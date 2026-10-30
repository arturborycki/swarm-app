//! swarm-term: behavior and branding of the Swarm app built on Warp's engine.
//!
//! Swarm uses Warp's terminal model, blocks, input editor and renderer, but is
//! its own product: its own name, first-run flow and session UI (see
//! `docs/DESIGN.md` §9b in the swarm-term repo). Fork-specific switches live
//! here so the diff against upstream stays easy to find.

/// Swarm opens straight into a terminal: no Warp welcome, onboarding or login
/// screen. Warp's cloud sign-in stays available but opt-in.
pub const SKIP_WARP_ONBOARDING: bool = true;

/// A running daemon session to open as a tab at launch.
#[derive(Debug, Clone)]
pub struct ReattachTab {
    pub session_id: String,
    pub warp_session_id: u64,
}

/// Whether shells live in `swarmd` sessions. Then Warp's own window/tab
/// restore is off: the daemon's sessions are the tabs to bring back.
pub fn use_daemon_sessions() -> bool {
    #[cfg(unix)]
    return warp_terminal::local_tty::daemon::enabled();
    #[cfg(not(unix))]
    false
}

/// Sessions that were open as tabs when the app last quit, in tab order.
pub fn reattachable_tabs() -> Vec<ReattachTab> {
    #[cfg(unix)]
    return warp_terminal::local_tty::daemon::reattachable_sessions()
        .into_iter()
        .map(|s| ReattachTab { session_id: s.session_id, warp_session_id: s.warp_session_id })
        .collect();
    #[cfg(not(unix))]
    Vec::new()
}

/// Options for a tab that attaches to `tab`'s session instead of starting a
/// shell. The keys are consumed before anything reaches a shell.
pub fn reattach_terminal_options(tab: &ReattachTab) -> crate::pane_group::NewTerminalOptions {
    let mut options = crate::pane_group::NewTerminalOptions { hide_homepage: true, ..Default::default() };
    #[cfg(unix)]
    {
        use warp_terminal::local_tty::daemon::{REATTACH_SESSION_ENV, REATTACH_WARP_ID_ENV};
        options.env_vars.insert(REATTACH_SESSION_ENV.into(), tab.session_id.clone().into());
        options.env_vars.insert(REATTACH_WARP_ID_ENV.into(), tab.warp_session_id.to_string().into());
    }
    options
}
