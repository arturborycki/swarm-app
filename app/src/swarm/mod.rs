//! swarm-term: behavior and branding of the Swarm app built on Warp's engine.
//!
//! Swarm uses Warp's terminal model, blocks, input editor and renderer, but is
//! its own product: its own name, first-run flow and session UI (see
//! `docs/DESIGN.md` §9b in the swarm-term repo). Fork-specific switches live
//! here so the diff against upstream stays easy to find.

#[cfg(unix)]
pub mod sessions;

/// Swarm's own window chrome (title bar, session menus) instead of Warp's.
pub const SWARM_CHROME: bool = true;

/// The product name shown in menus and titles.
pub const APP_NAME: &str = "Swarm";

/// Swarm opens straight into a terminal: no Warp welcome, onboarding or login
/// screen. Warp's cloud sign-in stays available but opt-in.
pub const SKIP_WARP_ONBOARDING: bool = true;

/// A running daemon session to open as a tab at launch.
#[derive(Debug, Clone)]
pub struct ReattachTab {
    pub session_id: String,
    /// The shell's Warp session id, when it already has Warp's integration.
    pub warp_session_id: Option<u64>,
    /// For an idle shell without it: the shell to set Warp's integration up in.
    pub warpify_shell: Option<String>,
    /// The session's name, shown as the tab title.
    pub name: Option<String>,
}

#[cfg(unix)]
impl ReattachTab {
    /// The tab for a daemon session. Sessions this app started already have
    /// Warp's integration; an idle zsh, bash or fish started elsewhere gets it
    /// set up; anything else (a running program) opens without it.
    pub fn for_session(session: &warp_terminal::local_tty::daemon::DaemonSession) -> Self {
        use warp_terminal::local_tty::daemon::META_SESSION_ID;
        let warp_session_id = session.ui.get(META_SESSION_ID).and_then(|v| v.parse().ok());
        let program = session
            .command
            .first()
            .map(|argv0| argv0.rsplit('/').next().unwrap_or(argv0).trim_start_matches('-').to_string());
        let warpify_shell = program
            .filter(|p| matches!(p.as_str(), "zsh" | "bash" | "fish"))
            .filter(|_| warp_session_id.is_none() && session.foreground.is_none());
        ReattachTab { session_id: session.id.clone(), warp_session_id, warpify_shell, name: session.name.clone() }
    }

    /// Whether the tab will show Warp blocks (otherwise it shows raw output).
    pub fn has_warp_integration(&self) -> bool {
        self.warp_session_id.is_some() || self.warpify_shell.is_some()
    }
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
        .map(|s| ReattachTab::for_session(&s))
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
        use warp_terminal::local_tty::daemon::REATTACH_WARPIFY_SHELL_ENV;
        options.env_vars.insert(REATTACH_SESSION_ENV.into(), tab.session_id.clone().into());
        if let Some(id) = tab.warp_session_id {
            options.env_vars.insert(REATTACH_WARP_ID_ENV.into(), id.to_string().into());
        }
        if let Some(shell) = &tab.warpify_shell {
            options.env_vars.insert(REATTACH_WARPIFY_SHELL_ENV.into(), shell.clone().into());
        }
    }
    options
}
