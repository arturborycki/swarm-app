//! swarm-term: behavior and branding of the Swarm app built on Warp's engine.
//!
//! Swarm uses Warp's terminal model, blocks, input editor and renderer, but is
//! its own product: its own name, first-run flow and session UI (see
//! `docs/DESIGN.md` §9b in the swarm-term repo). Fork-specific switches live
//! here so the diff against upstream stays easy to find.

#[cfg(unix)]
pub mod connect;
#[cfg(unix)]
pub mod picker;
#[cfg(unix)]
pub mod sessions;
#[cfg(unix)]
pub mod widgets;

/// Swarm's own window chrome (title bar, session menus) instead of Warp's.
pub const SWARM_CHROME: bool = true;

/// Hide Warp features that need Warp's servers (session sharing). Off in unit
/// tests, which still cover those features.
pub const HIDE_WARP_SERVICES: bool = SWARM_CHROME && !cfg!(test);

/// The product name shown in menus and titles.
pub const APP_NAME: &str = "Swarm";

/// Where Swarm's source, docs and issues live (the Help menu).
pub const REPO_URL: &str = "https://github.com/arturborycki/swarm-term";

/// Swarm opens straight into a terminal: no Warp welcome, onboarding or login
/// screen. Warp's cloud sign-in stays available but opt-in.
pub const SKIP_WARP_ONBOARDING: bool = true;

/// Warp features that need Warp's servers, never turned on in Swarm.
pub const DISABLED_WARP_FEATURES: &[crate::features::FeatureFlag] =
    &[crate::features::FeatureFlag::CreatingSharedSessions];

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
    /// The ssh host alias the session lives on; `None` for this Mac.
    pub host: Option<String>,
    /// The tab group to put the tab in (`META_GROUP`).
    pub group: Option<String>,
}

#[cfg(unix)]
impl ReattachTab {
    /// The tab for a daemon session. Sessions this app started already have
    /// Warp's integration; an idle zsh, bash or fish started elsewhere gets it
    /// set up; anything else (a running program) opens without it.
    pub fn for_session(host: Option<&str>, session: &warp_terminal::local_tty::daemon::DaemonSession) -> Self {
        use warp_terminal::local_tty::daemon::META_SESSION_ID;
        let warp_session_id = session.ui.get(META_SESSION_ID).and_then(|v| v.parse().ok());
        let program = session
            .command
            .first()
            .map(|argv0| argv0.rsplit('/').next().unwrap_or(argv0).trim_start_matches('-').to_string());
        let warpify_shell = program
            .filter(|p| matches!(p.as_str(), "zsh" | "bash" | "fish"))
            .filter(|_| warp_session_id.is_none() && session.foreground.is_none());
        ReattachTab {
            session_id: session.id.clone(),
            warp_session_id,
            warpify_shell,
            name: session.name.clone(),
            host: host.map(str::to_string),
            group: session.ui.get(warp_terminal::local_tty::daemon::META_GROUP).filter(|g| !g.is_empty()).cloned(),
        }
    }

    /// The tab's title: the session's name, else the host alias for a remote
    /// session (its shell's title is unreliable after a replay), else the
    /// shell's own title.
    pub fn title(&self) -> Option<String> {
        self.name.clone().or_else(|| self.host.clone())
    }

    /// Whether the tab will show Warp blocks (otherwise it shows raw output).
    pub fn has_warp_integration(&self) -> bool {
        self.warp_session_id.is_some() || self.warpify_shell.is_some()
    }
}

thread_local! {
    /// The group each session was last stored under, so only changes go out.
    static STORED_GROUPS: std::cell::RefCell<std::collections::HashMap<u64, String>> = Default::default();
}

/// Stores every open tab's group on its daemon sessions, sending only what
/// changed since the last call. Runs whenever the app saves its state.
pub fn sync_session_groups(ctx: &warpui::AppContext) {
    use warpui::SingletonEntity as _;
    if !use_daemon_sessions() {
        return;
    }
    let mut current = std::collections::HashMap::new();
    for (_, workspace) in crate::workspace::WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
        workspace.as_ref(ctx).swarm_session_groups(&mut current, ctx);
    }
    let mut changed: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
    STORED_GROUPS.with(|stored| {
        let mut stored = stored.borrow_mut();
        for (id, group) in current {
            let previous = stored.get(&id).map(String::as_str).unwrap_or("");
            if previous != group {
                changed.entry(group.clone()).or_default().push(id);
                stored.insert(id, group);
            }
        }
    });
    #[cfg(unix)]
    for (group, ids) in changed {
        warp_terminal::local_tty::daemon::group_sessions(ids, group);
    }
}

/// The tab color for sessions on `host`: always the same for a host name.
pub fn host_color(host: &str) -> warp_core::ui::theme::AnsiColorIdentifier {
    use warp_core::ui::theme::AnsiColorIdentifier::*;
    // No red: it reads as an error.
    const COLORS: [warp_core::ui::theme::AnsiColorIdentifier; 5] = [Cyan, Magenta, Yellow, Green, Blue];
    let hash = host.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    COLORS[hash as usize % COLORS.len()]
}

/// Whether a tab holds a remote daemon session. Those can't be dragged out to
/// a new window: their in-band command channel stays with the old window.
pub fn pane_group_is_remote(pane_group: &crate::pane_group::PaneGroup, app: &warpui::AppContext) -> bool {
    #[cfg(unix)]
    return pane_group.terminal_views(app).iter().any(|view| {
        view.as_ref(app)
            .active_block_session_id()
            .and_then(|id| warp_terminal::local_tty::daemon::session_is_remote(id.as_u64()))
            == Some(true)
    });
    #[cfg(not(unix))]
    {
        let _ = (pane_group, app);
        false
    }
}

/// The `META_GROUP` value for a tab group.
pub fn group_key(group: &crate::workspace::tab_group::TabGroup) -> String {
    group.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("#{}", group.id.0))
}

/// True the first time Swarm runs (for this state dir), and only once: the
/// first-run sheet is shown then. Records that it ran.
pub fn claim_first_run() -> bool {
    #[cfg(unix)]
    {
        let marker = swarm_client_state_dir().join("first-run-done");
        if marker.exists() {
            return false;
        }
        let _ = std::fs::create_dir_all(swarm_client_state_dir());
        std::fs::write(&marker, b"").is_ok()
    }
    #[cfg(not(unix))]
    false
}

#[cfg(unix)]
fn swarm_client_state_dir() -> std::path::PathBuf {
    warp_terminal::local_tty::daemon::state_dir()
}

static REMOTE_TABS_CLAIMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True only for the first caller in this run: remote sessions are reopened
/// once, in the first window.
pub fn claim_remote_tab_reopen() -> bool {
    !REMOTE_TABS_CLAIMED.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// Settings pages for what Swarm itself does (no Warp account, AI, cloud,
/// teams, referrals or Drive).
pub fn shows_settings_section(section: crate::settings_view::SettingsSection) -> bool {
    use crate::settings_view::SettingsSection::*;
    matches!(section, Sessions | Daemon | Hosts | Appearance | Features | Keybindings | Warpify | EditorAndCodeReview | Privacy | About)
}

/// Whether shells live in `swarmd` sessions. Then Warp's own window/tab
/// restore is off: the daemon's sessions are the tabs to bring back.
pub fn use_daemon_sessions() -> bool {
    // Unit tests drive Warp's own terminals, not daemon sessions.
    #[cfg(test)]
    return false;
    #[cfg(all(unix, not(test)))]
    return warp_terminal::local_tty::daemon::enabled();
    #[cfg(not(unix))]
    false
}

/// "3m ago" for a time in ms since the epoch.
pub fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(ms);
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// `path` with this Mac's home folder shown as `~`.
pub fn home_relative(path: &str) -> String {
    match dirs::home_dir().and_then(|home| std::path::Path::new(path).strip_prefix(home).ok().map(|r| r.display().to_string())) {
        Some(rest) if rest.is_empty() => "~".to_string(),
        Some(rest) => format!("~/{rest}"),
        None => path.to_string(),
    }
}

/// After a wake or a network change: remote tabs whose link died reconnect
/// now rather than when ssh notices.
pub fn check_daemon_links() {
    #[cfg(unix)]
    if let Some(daemons) = warp_terminal::local_tty::daemon::daemons() {
        daemons.check_links();
    }
}

/// Sessions that were open as tabs when the app last quit, in tab order.
pub fn reattachable_tabs() -> Vec<ReattachTab> {
    #[cfg(unix)]
    return warp_terminal::local_tty::daemon::reattachable_sessions()
        .into_iter()
        .map(|s| ReattachTab::for_session(None, &s))
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
        if let Some(host) = &tab.host {
            options.env_vars.insert(warp_terminal::local_tty::daemon::REATTACH_HOST_ENV.into(), host.clone().into());
        }
    }
    options
}
