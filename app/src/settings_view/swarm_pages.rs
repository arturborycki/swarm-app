//! swarm-term: the Daemon and Hosts settings pages (docs/DESIGN.md §9b).
//!
//! Daemon: this Mac's `swarmd` (status, start mode, restart, logs).
//! Hosts: the remote hosts Swarm has connected to, each with its daemon's
//! status and start mode, reinstall and remove.

use warpui::elements::{Container, CrossAxisAlignment, Element, Flex, MouseStateHandle, ParentElement, Shrinkable};
use warpui::platform::Cursor;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{AppContext, Entity, SingletonEntity as _, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle, SettingsWidget,
    build_toggle_element, render_separator, render_sub_header,
};
use crate::appearance::Appearance;

#[cfg(unix)]
use warp_terminal::local_tty::daemon::{self, DaemonStatus, HostCheck};

/// A value fetched in the background.
#[derive(Clone)]
enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

#[cfg(unix)]
fn daemons_or_err() -> Result<&'static daemon::Daemons, String> {
    daemon::daemons().ok_or_else(|| "Daemon sessions are off (SWARM_DAEMON=0).".to_string())
}

/// `swarmd <version> on <host> (<os>)` and the session counts.
#[cfg(unix)]
fn describe(status: &DaemonStatus) -> (String, String) {
    let sessions = match status.sessions {
        1 => "1 session".to_string(),
        n => format!("{n} sessions"),
    };
    (
        format!("Running: swarmd {} on {} ({})", status.version, status.hostname, status.os),
        format!("{sessions}: {} running, {} detached", status.running, status.detached),
    )
}

fn button(
    appearance: &Appearance,
    mouse_state: MouseStateHandle,
    label: &str,
    enabled: bool,
    on_click: impl Fn(&mut warpui::EventContext) + 'static,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let mut button = appearance
        .ui_builder()
        .button(ButtonVariant::Secondary, mouse_state)
        .with_style(UiComponentStyles { font_size: Some(12.), ..Default::default() })
        .with_hovered_styles(UiComponentStyles { background: Some(theme.surface_3().into()), ..Default::default() })
        .with_text_label(label.to_owned());
    if !enabled {
        button = button.disabled();
    }
    let element = button.build().with_cursor(Cursor::PointingHand);
    if enabled { element.on_click(move |ctx, _, _| on_click(ctx)).finish() } else { element.finish() }
}

fn line(appearance: &Appearance, text: String, sub: bool) -> Box<dyn Element> {
    let theme = appearance.theme();
    let color = if sub { theme.sub_text_color(theme.background()) } else { theme.active_ui_text_color() };
    Container::new(
        appearance
            .ui_builder()
            .paragraph(text)
            .with_style(UiComponentStyles {
                font_color: Some(color.into()),
                font_size: Some(if sub { 12. } else { 13. }),
                ..Default::default()
            })
            .build()
            .finish(),
    )
    .with_padding_bottom(6.)
    .finish()
}

fn buttons_row(children: Vec<Box<dyn Element>>) -> Box<dyn Element> {
    Container::new(
        Flex::row()
            .with_spacing(8.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_children(children)
            .finish(),
    )
    .with_padding_top(6.)
    .with_padding_bottom(12.)
    .finish()
}

// ---------------------------------------------------------------------------
// Daemon

#[derive(Clone, Debug)]
pub enum DaemonPageAction {
    SetAtLogin(bool),
    SetRestore(bool),
    SetSpill(bool),
    Restart,
    OpenLogs,
    Refresh,
}

pub struct DaemonPageView {
    page: PageType<Self>,
    #[cfg(unix)]
    status: Load<DaemonStatus>,
    /// What's being done right now ("Restarting…"), or the last action's error.
    note: Option<String>,
}

impl DaemonPageView {
    pub fn new(ctx: &mut ViewContext<DaemonPageView>) -> Self {
        let mut me = DaemonPageView {
            page: PageType::new_monolith(DaemonWidget::default(), Some("Daemon"), false),
            #[cfg(unix)]
            status: Load::Loading,
            note: None,
        };
        #[cfg(unix)]
        {
            // Session counts come from the live sessions model.
            let sessions = crate::swarm::sessions::SwarmSessions::handle(ctx);
            ctx.subscribe_to_model(&sessions, |_, _, _, ctx| ctx.notify());
        }
        me.refresh(ctx);
        me
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        #[cfg(unix)]
        {
            let daemons = match daemons_or_err() {
                Ok(d) => d,
                Err(e) => {
                    self.status = Load::Failed(e);
                    return;
                }
            };
            let rx = daemons.run(async { daemon::daemon_status(None).await });
            let _ = ctx.spawn(async move { rx.recv().await }, |me, result, ctx| {
                me.status = match result {
                    Ok(Ok(status)) => Load::Ready(status),
                    Ok(Err(e)) => Load::Failed(format!("{e:#}")),
                    Err(_) => Load::Failed("The daemon connection closed.".into()),
                };
                ctx.notify();
            });
        }
        #[cfg(not(unix))]
        let _ = ctx;
    }

    /// Runs `task` on the daemon runtime with `note` shown, then refreshes.
    #[cfg(unix)]
    fn run_task<F>(&mut self, note: &str, task: F, ctx: &mut ViewContext<Self>)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Ok(daemons) = daemons_or_err() else { return };
        self.note = Some(note.to_string());
        ctx.notify();
        let rx = daemons.run(task);
        let _ = ctx.spawn(async move { rx.recv().await }, |me, result, ctx| {
            me.note = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("{e:#}")),
                Err(_) => Some("The daemon connection closed.".into()),
            };
            me.refresh(ctx);
            ctx.notify();
        });
    }
}

impl Entity for DaemonPageView {
    type Event = SettingsPageEvent;
}

impl TypedActionView for DaemonPageView {
    type Action = DaemonPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            #[cfg(unix)]
            DaemonPageAction::SetAtLogin(at_login) => {
                let at_login = *at_login;
                let note = if at_login { "Setting the daemon to start at login…" } else { "Setting the daemon to start with Swarm…" };
                self.run_task(note, async move { daemon::set_start_at_login(None, at_login).await }, ctx);
            }
            #[cfg(unix)]
            DaemonPageAction::SetRestore(on) => {
                let on = *on;
                let note = if on { "Turning on restore after restart…" } else { "Turning off restore after restart…" };
                self.run_task(note, async move { daemon::set_restore_after_reboot(None, on).await }, ctx);
            }
            #[cfg(unix)]
            DaemonPageAction::SetSpill(on) => {
                let on = *on;
                let note = if on { "Turning on keeping scrollback on disk…" } else { "Turning off keeping scrollback on disk…" };
                self.run_task(note, async move { daemon::set_spill_to_disk(None, on).await }, ctx);
            }
            #[cfg(unix)]
            DaemonPageAction::Restart => {
                self.run_task("Restarting the daemon…", async { daemon::restart_daemon(None).await }, ctx);
            }
            #[cfg(unix)]
            DaemonPageAction::OpenLogs => {
                let dir = daemon::daemon_log_dir();
                let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                if let Err(e) = command::blocking::Command::new(opener).arg(&dir).spawn() {
                    self.note = Some(format!("Couldn't open {}: {e}", dir.display()));
                    ctx.notify();
                }
            }
            DaemonPageAction::Refresh => {
                self.note = None;
                self.refresh(ctx);
            }
            #[cfg(not(unix))]
            _ => {}
        }
    }
}

impl View for DaemonPageView {
    fn ui_name() -> &'static str {
        "SwarmDaemonPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Default)]
struct DaemonWidget {
    at_login_switch: SwitchStateHandle,
    restore_switch: SwitchStateHandle,
    spill_switch: SwitchStateHandle,
    restart: MouseStateHandle,
    logs: MouseStateHandle,
    refresh: MouseStateHandle,
}

impl SettingsWidget for DaemonWidget {
    type View = DaemonPageView;

    fn search_terms(&self) -> &str {
        "daemon swarmd sessions start at login restart logs background scrollback disk spill history"
    }

    fn render(&self, view: &DaemonPageView, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let mut column = Flex::column();
        column.add_child(line(
            appearance,
            "Your sessions live in swarmd, a background daemon, so they keep running when Swarm is closed.".into(),
            true,
        ));

        #[cfg(unix)]
        {
            let restore = match &view.status {
                Load::Ready(status) => status.restore_after_reboot,
                _ => None,
            };
            let spill = match &view.status {
                Load::Ready(status) => status.spill_to_disk,
                _ => None,
            };
            let (headline, detail, at_login, ready) = match &view.status {
                Load::Loading => ("Checking the daemon…".to_string(), String::new(), false, false),
                Load::Ready(status) => {
                    let sessions = crate::swarm::sessions::SwarmSessions::as_ref(app).sessions_on(None);
                    let live = DaemonStatus {
                        sessions: sessions.len(),
                        running: sessions.iter().filter(|s| s.is_running()).count(),
                        detached: sessions.iter().filter(|s| s.is_running() && s.clients.is_empty()).count(),
                        ..status.clone()
                    };
                    let (h, d) = describe(&live);
                    (h, d, status.at_login, true)
                }
                Load::Failed(e) => (format!("Not reachable: {e}"), String::new(), false, false),
            };
            column.add_child(render_sub_header(appearance, "Status", None));
            column.add_child(line(appearance, headline, false));
            if !detail.is_empty() {
                column.add_child(line(appearance, detail, true));
            }
            column.add_child(buttons_row(vec![
                button(appearance, self.restart.clone(), "Restart daemon", ready && view.note.is_none(), |ctx| {
                    ctx.dispatch_typed_action(DaemonPageAction::Restart)
                }),
                button(appearance, self.logs.clone(), "Open logs", true, |ctx| {
                    ctx.dispatch_typed_action(DaemonPageAction::OpenLogs)
                }),
                button(appearance, self.refresh.clone(), "Refresh", true, |ctx| {
                    ctx.dispatch_typed_action(DaemonPageAction::Refresh)
                }),
            ]));
            if let Some(note) = &view.note {
                column.add_child(line(appearance, note.clone(), true));
            }
            column.add_child(render_separator(appearance));

            column.add_child(render_sub_header(appearance, "Start mode", None));
            let switch = appearance
                .ui_builder()
                .switch(self.at_login_switch.clone())
                .check(at_login)
                .build()
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(DaemonPageAction::SetAtLogin(!at_login)))
                .finish();
            column.add_child(build_toggle_element(
                line(appearance, "Start the daemon at login".into(), false),
                switch,
                appearance,
                Some(
                    "Off: the daemon starts with Swarm and keeps running after you quit. On: it also starts when you log in, so swarmctl and remote connections find your sessions after a restart."
                        .into(),
                ),
            ));
            if let Some(restore) = restore {
                let switch = appearance
                    .ui_builder()
                    .switch(self.restore_switch.clone())
                    .check(restore)
                    .build()
                    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(DaemonPageAction::SetRestore(!restore)))
                    .finish();
                column.add_child(build_toggle_element(
                    line(appearance, "Restore sessions after a restart".into(), false),
                    switch,
                    appearance,
                    Some(
                        "When the Mac restarts, programs in your sessions end. With this on, each session comes back as a new shell with the same name, folder, group and tab position."
                            .into(),
                    ),
                ));
            }
            if let Some(spill) = spill {
                column.add_child(render_separator(appearance));
                column.add_child(render_sub_header(appearance, "Scrollback", None));
                let switch = appearance
                    .ui_builder()
                    .switch(self.spill_switch.clone())
                    .check(spill)
                    .build()
                    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(DaemonPageAction::SetSpill(!spill)))
                    .finish();
                column.add_child(build_toggle_element(
                    line(appearance, "Keep scrollback beyond 16 MB on disk".into(), false),
                    switch,
                    appearance,
                    Some(
                        "Each session keeps its last 16 MB of output in memory. With this on, older output is kept on disk too, up to 256 MB per session, encrypted with a key only that session holds, and deleted with the session. See it all with swarmctl dump. Turning this off deletes it."
                            .into(),
                    ),
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = view;
        column.finish()
    }
}

impl SettingsPageMeta for DaemonPageView {
    fn section() -> SettingsSection {
        SettingsSection::Daemon
    }

    fn on_page_selected(&mut self, _allow_steal_focus: bool, ctx: &mut ViewContext<Self>) {
        self.refresh(ctx);
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        crate::swarm::use_daemon_sessions()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<DaemonPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<DaemonPageView>) -> Self {
        SettingsPageViewHandle::SwarmDaemon(view_handle)
    }
}

// ---------------------------------------------------------------------------
// Hosts

#[derive(Clone, Debug)]
pub enum HostsPageAction {
    SetAtLogin { host: String, at_login: bool },
    SetLinger { host: String, on: bool },
    Install(String),
    Remove(String),
    Refresh,
}

/// What a remote host's check found.
#[cfg(unix)]
#[derive(Clone)]
enum HostState {
    Ready(DaemonStatus),
    NeedsInstall { os: String, installable: bool },
    Unreachable(String),
}

#[derive(Default, Clone)]
struct HostRowMouse {
    at_login: SwitchStateHandle,
    linger: SwitchStateHandle,
    install: MouseStateHandle,
    remove: MouseStateHandle,
}

struct HostRow {
    host: String,
    #[cfg(unix)]
    state: Load<HostState>,
    note: Option<String>,
    busy: bool,
    mouse: HostRowMouse,
}

pub struct HostsPageView {
    page: PageType<Self>,
    rows: Vec<HostRow>,
}

impl HostsPageView {
    pub fn new(ctx: &mut ViewContext<HostsPageView>) -> Self {
        let me = HostsPageView {
            page: PageType::new_monolith(HostsWidget::default(), Some("Hosts"), false),
            rows: Vec::new(),
        };
        // Hosts are checked (over ssh) when the page is opened, not at launch.
        let _ = ctx;
        me
    }

    /// Re-reads the remembered hosts and checks each one in the background.
    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        #[cfg(unix)]
        {
            let old: Vec<_> = std::mem::take(&mut self.rows);
            for host in daemon::known_hosts() {
                let mouse = old.iter().find(|r| r.host == host).map(|r| r.mouse.clone()).unwrap_or_default();
                self.rows.push(HostRow { host: host.clone(), state: Load::Loading, note: None, busy: false, mouse });
                self.check(host, ctx);
            }
            ctx.notify();
        }
        #[cfg(not(unix))]
        let _ = ctx;
    }

    #[cfg(unix)]
    fn check(&mut self, host: String, ctx: &mut ViewContext<Self>) {
        let Ok(daemons) = daemons_or_err() else { return };
        let target = host.clone();
        let rx = daemons.run(async move {
            match daemon::check_host(&target).await {
                HostCheck::Ready => daemon::daemon_status(Some(&target))
                    .await
                    .map(HostState::Ready)
                    .map_err(|e| format!("{e:#}")),
                HostCheck::NeedsInstall { os, installable } => Ok(HostState::NeedsInstall { os, installable }),
                HostCheck::Unreachable(e) => Ok(HostState::Unreachable(e)),
            }
        });
        let _ = ctx.spawn(async move { rx.recv().await }, move |me, result, ctx| {
            if let Some(row) = me.rows.iter_mut().find(|r| r.host == host) {
                row.state = match result {
                    Ok(Ok(state)) => Load::Ready(state),
                    Ok(Err(e)) => Load::Failed(e),
                    Err(_) => Load::Failed("The daemon connection closed.".into()),
                };
                ctx.notify();
            }
        });
    }

    /// Runs `task` for `host` with `note` shown on its row, then re-checks it.
    #[cfg(unix)]
    fn run_task<F>(&mut self, host: &str, note: &str, task: F, ctx: &mut ViewContext<Self>)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Ok(daemons) = daemons_or_err() else { return };
        let Some(row) = self.rows.iter_mut().find(|r| r.host == host) else { return };
        row.busy = true;
        row.note = Some(note.to_string());
        ctx.notify();
        let host = host.to_string();
        let rx = daemons.run(task);
        let _ = ctx.spawn(async move { rx.recv().await }, move |me, result, ctx| {
            if let Some(row) = me.rows.iter_mut().find(|r| r.host == host) {
                row.busy = false;
                row.note = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(format!("{e:#}")),
                    Err(_) => Some("The daemon connection closed.".into()),
                };
                row.state = Load::Loading;
            }
            me.check(host.clone(), ctx);
            ctx.notify();
        });
    }
}

impl Entity for HostsPageView {
    type Event = SettingsPageEvent;
}

impl TypedActionView for HostsPageView {
    type Action = HostsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            #[cfg(unix)]
            HostsPageAction::SetAtLogin { host, at_login } => {
                let (target, at_login) = (host.clone(), *at_login);
                let note = if at_login { "Setting the daemon to start at login…" } else { "Setting the daemon to start on demand…" };
                self.run_task(host, note, async move { daemon::set_start_at_login(Some(&target), at_login).await }, ctx);
            }
            #[cfg(unix)]
            HostsPageAction::SetLinger { host, on } => {
                let (target, on) = (host.clone(), *on);
                let note = if on { "Turning lingering on…" } else { "Turning lingering off…" };
                self.run_task(host, note, async move { daemon::set_linger(&target, on).await }, ctx);
            }
            #[cfg(unix)]
            HostsPageAction::Install(host) => {
                let target = host.clone();
                let running = self
                    .rows
                    .iter()
                    .any(|r| r.host == *host && matches!(r.state, Load::Ready(HostState::Ready(_))));
                self.run_task(
                    host,
                    "Installing swarmd…",
                    async move {
                        daemon::install_swarmd(&target).await?;
                        // Pick up the new binary; sessions keep running.
                        if running {
                            daemon::restart_daemon(Some(&target)).await?;
                        }
                        Ok(())
                    },
                    ctx,
                );
            }
            #[cfg(unix)]
            HostsPageAction::Remove(host) => {
                daemon::forget_host(host);
                self.rows.retain(|r| r.host != *host);
                ctx.notify();
            }
            HostsPageAction::Refresh => self.refresh(ctx),
            #[cfg(not(unix))]
            _ => {}
        }
    }
}

impl View for HostsPageView {
    fn ui_name() -> &'static str {
        "SwarmHostsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Default)]
struct HostsWidget {
    refresh: MouseStateHandle,
}

impl SettingsWidget for HostsWidget {
    type View = HostsPageView;

    fn search_terms(&self) -> &str {
        "hosts remote ssh servers swarmd install reinstall start at login remove lingering logout"
    }

    fn render(&self, view: &HostsPageView, appearance: &Appearance, _app: &AppContext) -> Box<dyn Element> {
        let mut column = Flex::column();
        column.add_child(line(
            appearance,
            "Hosts you have connected to with Connect… in the title bar. Their sessions reopen as tabs when Swarm starts. Removing a host stops that; its sessions keep running there."
                .into(),
            true,
        ));
        column.add_child(buttons_row(vec![button(appearance, self.refresh.clone(), "Check again", true, |ctx| {
            ctx.dispatch_typed_action(HostsPageAction::Refresh)
        })]));
        if view.rows.is_empty() {
            column.add_child(line(appearance, "No hosts yet.".into(), false));
        }
        #[cfg(unix)]
        for row in &view.rows {
            column.add_child(render_separator(appearance));
            column.add_child(render_sub_header(appearance, row.host.clone(), None));
            let (headline, detail, at_login, install_label, can_install) = match &row.state {
                Load::Loading => ("Checking…".to_string(), None, None, "Reinstall swarmd", false),
                Load::Failed(e) => (format!("swarmd isn't responding: {e}"), None, None, "Reinstall swarmd", false),
                Load::Ready(HostState::Ready(status)) => {
                    let (h, d) = describe(status);
                    (h, Some(d), Some(status.at_login), "Reinstall swarmd", daemon::bundled_swarmd_dir(&status.os).is_some())
                }
                Load::Ready(HostState::NeedsInstall { os, installable }) => (
                    if *installable {
                        format!("swarmd isn't installed ({os}).")
                    } else {
                        format!("swarmd isn't installed, and this app has no swarmd for {os}.")
                    },
                    None,
                    None,
                    "Install swarmd",
                    *installable,
                ),
                Load::Ready(HostState::Unreachable(e)) => (format!("Can't connect: {e}"), None, None, "Reinstall swarmd", false),
            };
            column.add_child(line(appearance, headline, false));
            if let Some(detail) = detail {
                column.add_child(line(appearance, detail, true));
            }
            if let Some(at_login) = at_login {
                let host = row.host.clone();
                let busy = row.busy;
                let switch = appearance
                    .ui_builder()
                    .switch(row.mouse.at_login.clone())
                    .check(at_login)
                    .build()
                    .on_click(move |ctx, _, _| {
                        if !busy {
                            ctx.dispatch_typed_action(HostsPageAction::SetAtLogin { host: host.clone(), at_login: !at_login })
                        }
                    })
                    .finish();
                column.add_child(build_toggle_element(
                    line(appearance, "Start the daemon at login".into(), false),
                    switch,
                    appearance,
                    None,
                ));
            }
            let linger = match &row.state {
                Load::Ready(HostState::Ready(status)) => status.linger,
                _ => None,
            };
            if let Some(on) = linger {
                let host = row.host.clone();
                let busy = row.busy;
                let switch = appearance
                    .ui_builder()
                    .switch(row.mouse.linger.clone())
                    .check(on)
                    .build()
                    .on_click(move |ctx, _, _| {
                        if !busy {
                            ctx.dispatch_typed_action(HostsPageAction::SetLinger { host: host.clone(), on: !on })
                        }
                    })
                    .finish();
                column.add_child(build_toggle_element(
                    line(appearance, "Keep sessions running after I log out (lingering)".into(), false),
                    switch,
                    appearance,
                    None,
                ));
                if !on {
                    column.add_child(line(
                        appearance,
                        "Off: systemd may end your sessions when your last login to this host ends, for example when your Mac sleeps."
                            .into(),
                        true,
                    ));
                } else if at_login == Some(true) {
                    column.add_child(line(
                        appearance,
                        "The daemon here starts at login, so turning this off means systemd stops it, and every session, when you log out."
                            .into(),
                        true,
                    ));
                }
            }
            let (install_host, remove_host) = (row.host.clone(), row.host.clone());
            column.add_child(buttons_row(vec![
                button(appearance, row.mouse.install.clone(), install_label, can_install && !row.busy, move |ctx| {
                    ctx.dispatch_typed_action(HostsPageAction::Install(install_host.clone()))
                }),
                button(appearance, row.mouse.remove.clone(), "Remove", !row.busy, move |ctx| {
                    ctx.dispatch_typed_action(HostsPageAction::Remove(remove_host.clone()))
                }),
            ]));
            if let Some(note) = &row.note {
                column.add_child(line(appearance, note.clone(), true));
            }
        }
        Shrinkable::new(1., column.finish()).finish()
    }
}

impl SettingsPageMeta for HostsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Hosts
    }

    fn on_page_selected(&mut self, _allow_steal_focus: bool, ctx: &mut ViewContext<Self>) {
        self.refresh(ctx);
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        crate::swarm::use_daemon_sessions()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<HostsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<HostsPageView>) -> Self {
        SettingsPageViewHandle::SwarmHosts(view_handle)
    }
}

// ---------------------------------------------------------------------------
// Sessions

#[derive(Clone, Debug)]
pub enum SessionsPageAction {
    Terminate { host: Option<String>, id: String, name: String },
    Remove { host: Option<String>, id: String },
    RemoveExited,
}

/// Every session on this Mac and the followed hosts, live from the sessions
/// model, with terminate and remove.
pub struct SessionsPageView {
    page: PageType<Self>,
    note: Option<String>,
    /// Mouse states per session id (terminate, remove), kept across renders.
    mouse: std::cell::RefCell<std::collections::HashMap<String, (MouseStateHandle, MouseStateHandle)>>,
}

impl SessionsPageView {
    pub fn new(ctx: &mut ViewContext<SessionsPageView>) -> Self {
        #[cfg(unix)]
        {
            let sessions = crate::swarm::sessions::SwarmSessions::handle(ctx);
            ctx.subscribe_to_model(&sessions, |_, _, _, ctx| ctx.notify());
        }
        SessionsPageView {
            page: PageType::new_monolith(SessionsWidget::default(), Some("Sessions"), false),
            note: None,
            mouse: Default::default(),
        }
    }

    fn mouse_for(&self, id: &str) -> (MouseStateHandle, MouseStateHandle) {
        self.mouse.borrow_mut().entry(id.to_string()).or_default().clone()
    }

    /// Runs `task` on the daemon runtime, showing its error if it fails.
    #[cfg(unix)]
    fn run_task<F>(&mut self, task: F, ctx: &mut ViewContext<Self>)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let Ok(daemons) = daemons_or_err() else { return };
        let rx = daemons.run(task);
        let _ = ctx.spawn(async move { rx.recv().await }, |me, result, ctx| {
            me.note = match result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(format!("{e:#}")),
                Err(_) => Some("The daemon connection closed.".into()),
            };
            ctx.notify();
        });
    }
}

impl Entity for SessionsPageView {
    type Event = SettingsPageEvent;
}

impl TypedActionView for SessionsPageView {
    type Action = SessionsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            #[cfg(unix)]
            SessionsPageAction::Terminate { host, id, name } => {
                use warpui::modals::{AlertDialogWithCallbacks, ModalButton};
                let (host, id) = (host.clone(), id.clone());
                let dialog = AlertDialogWithCallbacks::for_view(
                    format!("Terminate {name}?"),
                    "Its process gets SIGHUP, as when a terminal window closes. Anything running in it stops.",
                    vec![
                        ModalButton::for_view("Terminate", move |me: &mut Self, ctx| {
                            let (host, id) = (host.clone(), id.clone());
                            me.run_task(async move { daemon::terminate_session(host.as_deref(), &id).await }, ctx);
                        }),
                        ModalButton::for_view("Cancel", |_, _| {}),
                    ],
                    |_, _| {},
                )
                .without_dont_ask_again();
                ctx.show_native_platform_modal(dialog);
            }
            #[cfg(unix)]
            SessionsPageAction::Remove { host, id } => {
                let (host, id) = (host.clone(), id.clone());
                self.run_task(async move { daemon::remove_session(host.as_deref(), &id).await }, ctx);
            }
            #[cfg(unix)]
            SessionsPageAction::RemoveExited => {
                let model = crate::swarm::sessions::SwarmSessions::as_ref(ctx);
                let mut exited = Vec::new();
                let mut hosts: Vec<Option<String>> = vec![None];
                hosts.extend(daemon::known_hosts().into_iter().map(Some));
                for host in hosts {
                    for s in model.sessions_on(host.as_deref()) {
                        if !s.is_running() {
                            exited.push((host.clone(), s.id.clone()));
                        }
                    }
                }
                self.run_task(
                    async move {
                        for (host, id) in exited {
                            daemon::remove_session(host.as_deref(), &id).await?;
                        }
                        Ok(())
                    },
                    ctx,
                );
            }
            #[cfg(not(unix))]
            _ => {}
        }
    }
}

impl View for SessionsPageView {
    fn ui_name() -> &'static str {
        "SwarmSessionsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Default)]
struct SessionsWidget {
    remove_exited: MouseStateHandle,
}

fn size(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.0} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

impl SettingsWidget for SessionsWidget {
    type View = SessionsPageView;

    fn search_terms(&self) -> &str {
        "sessions terminate remove exited buffer scrollback kill"
    }

    fn render(&self, view: &SessionsPageView, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let mut column = Flex::column();
        column.add_child(line(
            appearance,
            "Every session the daemons keep, open in a tab or not. Terminating ends its program; removing also drops its scrollback.".into(),
            true,
        ));
        column.add_child(buttons_row(vec![button(appearance, self.remove_exited.clone(), "Remove Exited Sessions", true, |ctx| {
            ctx.dispatch_typed_action(SessionsPageAction::RemoveExited)
        })]));
        if let Some(note) = &view.note {
            column.add_child(line(appearance, note.clone(), true));
        }
        #[cfg(unix)]
        {
            let model = crate::swarm::sessions::SwarmSessions::as_ref(app);
            let mut hosts: Vec<Option<String>> = vec![None];
            hosts.extend(daemon::known_hosts().into_iter().map(Some));
            for host in hosts {
                let sessions = model.sessions_on(host.as_deref());
                if host.is_some() && sessions.is_empty() {
                    continue;
                }
                column.add_child(render_separator(appearance));
                column.add_child(render_sub_header(appearance, host.clone().unwrap_or_else(|| "This Mac".into()), None));
                if sessions.is_empty() {
                    column.add_child(line(appearance, "No sessions.".into(), true));
                }
                for s in sessions {
                    let name = s.name.clone().unwrap_or_else(|| s.title.clone());
                    let state = match &s.state {
                        daemon::SessionState::Running => {
                            s.foreground.clone().map(|f| format!("running {f}")).unwrap_or_else(|| "idle".into())
                        }
                        daemon::SessionState::Exited { code, signal, .. } => match (code, signal) {
                            (Some(c), _) => format!("exited {c}"),
                            (None, Some(sig)) => format!("killed by signal {sig}"),
                            _ => "exited".into(),
                        },
                    };
                    let attached = match s.clients.len() {
                        0 => "detached".to_string(),
                        1 => "open".to_string(),
                        n => format!("open in {n} places"),
                    };
                    let detail = format!(
                        "{} · {state} · {attached} · active {} · {} kept",
                        s.cwd.as_deref().unwrap_or("~"),
                        crate::swarm::ago(s.last_output_ms.max(s.last_input_ms)),
                        size(s.buffer_end - s.buffer_start),
                    );
                    let (terminate_mouse, remove_mouse) = view.mouse_for(&s.id);
                    let running = s.is_running();
                    let (h1, id1, n1) = (host.clone(), s.id.clone(), name.clone());
                    let (h2, id2) = (host.clone(), s.id.clone());
                    column.add_child(line(appearance, name, false));
                    column.add_child(line(appearance, detail, true));
                    column.add_child(buttons_row(vec![
                        button(appearance, terminate_mouse, "Terminate", running, move |ctx| {
                            ctx.dispatch_typed_action(SessionsPageAction::Terminate {
                                host: h1.clone(),
                                id: id1.clone(),
                                name: n1.clone(),
                            })
                        }),
                        button(appearance, remove_mouse, "Remove", !running, move |ctx| {
                            ctx.dispatch_typed_action(SessionsPageAction::Remove { host: h2.clone(), id: id2.clone() })
                        }),
                    ]));
                }
            }
        }
        #[cfg(not(unix))]
        let _ = app;
        column.finish()
    }
}

impl SettingsPageMeta for SessionsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Sessions
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        crate::swarm::use_daemon_sessions()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<SessionsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<SessionsPageView>) -> Self {
        SettingsPageViewHandle::SwarmSessions(view_handle)
    }
}
