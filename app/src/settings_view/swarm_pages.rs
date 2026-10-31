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
            DaemonPageAction::Restart => {
                self.run_task("Restarting the daemon…", async { daemon::restart_daemon(None).await }, ctx);
            }
            #[cfg(unix)]
            DaemonPageAction::OpenLogs => {
                let dir = daemon::daemon_log_dir();
                let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                if let Err(e) = std::process::Command::new(opener).arg(&dir).spawn() {
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
    restart: MouseStateHandle,
    logs: MouseStateHandle,
    refresh: MouseStateHandle,
}

impl SettingsWidget for DaemonWidget {
    type View = DaemonPageView;

    fn search_terms(&self) -> &str {
        "daemon swarmd sessions start at login restart logs background"
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
        "hosts remote ssh servers swarmd install reinstall start at login remove"
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
