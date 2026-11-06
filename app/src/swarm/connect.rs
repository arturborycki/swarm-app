//! The connect sheet (`docs/DESIGN.md` §9b "Connect to host", Connect
//! mockup): pick a host from `~/.ssh/config` or type `user@host`, and the
//! connection is checked step by step: ssh access, swarmd (installed if it's
//! missing), lingering on systemd hosts, and the host's sessions. A failure
//! says what went wrong and how to fix it; success leads to the session
//! picker for that host.

use std::cell::RefCell;
use std::collections::HashMap;

use warp_core::ui::Icon;
use warp_core::ui::theme::Fill;
use warp_core::ui::theme::color::internal_colors;
use warp_terminal::local_tty::daemon::{self, DaemonStatus, HostCheck};
use warpui::clipboard::ClipboardContent;
use warpui::color::ColorU;
use warpui::elements::{
    Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, DispatchEventResult, DropShadow, Element, Empty, EventHandler, Expanded, Fill as ElementFill,
    Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Radius, ScrollbarWidth,
    Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, Entity, SingletonEntity as _, TypedActionView, View, ViewContext, ViewHandle};

use super::widgets::{ButtonStyle, button};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};

const WIDTH: f32 = 760.;
const HOSTS_WIDTH: f32 = 280.;
const HEIGHT: f32 = 440.;

pub enum SwarmConnectEvent {
    Close,
    /// Connected: show `host`'s sessions.
    ChooseSessions(String),
    /// Connected to a host with no sessions: start one there.
    NewSession(String),
    /// Run a suggested fix in a new local tab, where ssh can ask the user for a password.
    RunInTerminal(String),
}

#[derive(Debug, Clone)]
pub enum SwarmConnectAction {
    Check(String),
    Retry,
    Install,
    SetAtLogin(bool),
    ChooseSessions,
    NewSession,
    CopyFix,
    RunFix,
    Cancel,
}

/// A step of the checklist.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Todo,
    Running,
    Done,
    /// Works, with a caveat (lingering off).
    Warn,
    /// Needs the user's go-ahead (install swarmd).
    Ask,
    Failed,
}

struct Step {
    mark: Mark,
    text: String,
    detail: String,
}

impl Step {
    fn new(mark: Mark, text: impl Into<String>, detail: impl Into<String>) -> Self {
        Step { mark, text: text.into(), detail: detail.into() }
    }
}

enum Outcome {
    /// Nothing picked yet.
    Idle,
    Checking,
    Connected(DaemonStatus),
    NeedsInstall { os: String, installable: bool },
    Installing,
    /// What failed, and a fix: an explanation and maybe a command to copy.
    Failed { message: String, fix: Option<(String, Option<String>)> },
}

pub struct SwarmConnect {
    field: ViewHandle<EditorView>,
    /// Aliases from `~/.ssh/config` and remembered hosts, with `user@hostname`.
    hosts: Vec<(String, Option<String>)>,
    host: Option<String>,
    steps: Vec<Step>,
    outcome: Outcome,
    /// The start mode picked for the host's daemon.
    at_login: bool,
    /// Bumped per check, so results of an older one are dropped.
    generation: u64,
    scroll: ClippedScrollStateHandle,
    mouse: RefCell<HashMap<String, MouseStateHandle>>,
}

impl SwarmConnect {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let field = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions { text: TextOptions::ui_text(Some(13.), appearance), ..Default::default() },
                ctx,
            );
            editor.set_placeholder_text("me@build-01", ctx);
            editor
        });
        ctx.subscribe_to_view(&field, |me, field, event, ctx| match event {
            EditorEvent::Enter => {
                let host = field.as_ref(ctx).buffer_text(ctx).trim().to_string();
                if !host.is_empty() && !host.contains(char::is_whitespace) {
                    me.check(host, ctx);
                }
            }
            EditorEvent::Escape => ctx.emit(SwarmConnectEvent::Close),
            _ => {}
        });
        SwarmConnect {
            field,
            hosts: Vec::new(),
            host: None,
            steps: Vec::new(),
            outcome: Outcome::Idle,
            at_login: false,
            generation: 0,
            scroll: Default::default(),
            mouse: Default::default(),
        }
    }

    /// Resets the sheet, then checks `host` right away if given.
    pub fn open(&mut self, host: Option<String>, ctx: &mut ViewContext<Self>) {
        self.generation += 1;
        self.host = None;
        self.steps.clear();
        self.outcome = Outcome::Idle;
        self.field.update(ctx, |field, ctx| field.clear_buffer(ctx));
        self.load_hosts(ctx);
        ctx.focus(&self.field);
        if let Some(host) = host {
            self.check(host, ctx);
        }
        ctx.notify();
    }

    /// The hosts list, then each alias's `user@hostname` (`ssh -G` reads the
    /// config only; nothing connects until a host is picked).
    fn load_hosts(&mut self, ctx: &mut ViewContext<Self>) {
        let mut aliases = daemon::ssh_hosts();
        for host in daemon::known_hosts() {
            if !aliases.contains(&host) {
                aliases.push(host);
            }
        }
        self.hosts = aliases.iter().map(|a| (a.clone(), None)).collect();
        let Some(daemons) = daemon::daemons() else { return };
        let rx = daemons.run(async move {
            let mut found = Vec::new();
            for alias in aliases {
                let destination = daemon::ssh_destination(&alias).await;
                found.push((alias, destination));
            }
            found
        });
        let _ = ctx.spawn(async move { rx.recv().await }, |me, found, ctx| {
            if let Ok(found) = found {
                me.hosts = found;
                ctx.notify();
            }
        });
    }

    /// Checks `host`: ssh, swarmd, lingering, sessions.
    fn check(&mut self, host: String, ctx: &mut ViewContext<Self>) {
        let Some(daemons) = daemon::daemons() else { return };
        self.generation += 1;
        let generation = self.generation;
        self.host = Some(host.clone());
        self.steps = vec![
            Step::new(Mark::Running, "SSH access", format!("ssh {host}")),
            Step::new(Mark::Todo, "Check swarmd", ""),
            Step::new(Mark::Todo, "List sessions", ""),
        ];
        self.outcome = Outcome::Checking;
        ctx.notify();
        let probe = host.clone();
        let rx = daemons.run(async move { daemon::check_host(&probe).await });
        let _ = ctx.spawn(async move { rx.recv().await }, move |me, check, ctx| {
            if me.generation != generation {
                return;
            }
            match check {
                Ok(HostCheck::Ready) => {
                    me.steps[0] = Step::new(Mark::Done, "SSH access", "Signed in with your SSH keys");
                    me.steps[1] = Step::new(Mark::Running, "Starting swarmd", "");
                    me.connect(host, ctx);
                }
                Ok(HostCheck::NeedsInstall { os, installable }) => {
                    me.steps[0] = Step::new(Mark::Done, "SSH access", "Signed in with your SSH keys");
                    me.steps[1] = if installable {
                        Step::new(Mark::Ask, "swarmd not found", "Needs to be installed once")
                    } else {
                        Step::new(Mark::Failed, "swarmd not found", format!("This app has no swarmd for {os}"))
                    };
                    me.outcome = Outcome::NeedsInstall { os, installable };
                }
                Ok(HostCheck::Unreachable(message)) => {
                    // The error itself goes in the panel below.
                    me.steps[0] = Step::new(Mark::Failed, "SSH access failed", format!("ssh {host}"));
                    me.outcome = Outcome::Failed { fix: ssh_fix(&host, &message), message };
                }
                Err(_) => {}
            }
            ctx.notify();
        });
    }

    /// Connects to `host`'s daemon (starting it), then reports on it.
    fn connect(&mut self, host: String, ctx: &mut ViewContext<Self>) {
        let Some(daemons) = daemon::daemons() else { return };
        let generation = self.generation;
        let target = host.clone();
        let rx = daemons.run(async move {
            let status = daemon::daemon_status(Some(&target)).await;
            let linger = daemon::linger(&target).await;
            (status, linger)
        });
        let _ = ctx.spawn(async move { rx.recv().await }, move |me, result, ctx| {
            if me.generation != generation {
                return;
            }
            match result {
                Ok((Ok(status), linger)) => {
                    me.steps[1] = Step::new(Mark::Done, format!("swarmd {} running", status.version), status.os.clone());
                    let sessions = Step::new(
                        Mark::Done,
                        match status.sessions {
                            1 => "1 session found".to_string(),
                            n => format!("{n} sessions found"),
                        },
                        format!("{} running, {} exited", status.running, status.sessions - status.running),
                    );
                    me.steps.truncate(2);
                    match linger {
                        Some(true) => me.steps.push(Step::new(
                            Mark::Done,
                            "Lingering enabled",
                            format!("Sessions survive when you log out of {host}"),
                        )),
                        Some(false) => me.steps.push(Step::new(
                            Mark::Warn,
                            "Lingering is off",
                            format!("Sessions end when you log out of {host}; run loginctl enable-linger there"),
                        )),
                        None => {}
                    }
                    me.steps.push(sessions);
                    me.at_login = status.at_login;
                    me.outcome = Outcome::Connected(status);
                }
                Ok((Err(e), _)) => {
                    let message = format!("{e:#}");
                    me.steps[1] = Step::new(Mark::Failed, "swarmd didn't start", message.clone());
                    me.outcome = Outcome::Failed { message, fix: None };
                }
                Err(_) => {}
            }
            ctx.notify();
        });
    }

    fn install(&mut self, ctx: &mut ViewContext<Self>) {
        let (Some(daemons), Some(host)) = (daemon::daemons(), self.host.clone()) else { return };
        let generation = self.generation;
        self.steps[1] = Step::new(Mark::Running, "Installing swarmd", "Copying to ~/.local/share/swarm/bin");
        self.outcome = Outcome::Installing;
        ctx.notify();
        let target = host.clone();
        let rx = daemons.run(async move { daemon::install_swarmd(&target).await });
        let _ = ctx.spawn(async move { rx.recv().await }, move |me, result, ctx| {
            if me.generation != generation {
                return;
            }
            match result {
                Ok(Ok(())) => me.check(host, ctx),
                Ok(Err(e)) => {
                    let message = format!("{e:#}");
                    me.steps[1] = Step::new(Mark::Failed, "Installing swarmd failed", message.clone());
                    me.outcome = Outcome::Failed { message, fix: None };
                    ctx.notify();
                }
                Err(_) => {}
            }
        });
    }

    fn choose_sessions(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(host) = self.apply_start_mode(ctx) {
            ctx.emit(SwarmConnectEvent::ChooseSessions(host));
        }
    }

    fn new_session(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(host) = self.apply_start_mode(ctx) {
            ctx.emit(SwarmConnectEvent::NewSession(host));
        }
    }

    /// Saves the chosen start mode, returning the connected host.
    fn apply_start_mode(&mut self, ctx: &mut ViewContext<Self>) -> Option<String> {
        let (Outcome::Connected(status), Some(host)) = (&self.outcome, self.host.clone()) else { return None };
        if self.at_login != status.at_login
            && let Some(daemons) = daemon::daemons()
        {
            let (target, at_login) = (host.clone(), self.at_login);
            let rx = daemons.run(async move { daemon::set_start_at_login(Some(&target), at_login).await });
            let _ = ctx.spawn(async move { rx.recv().await }, |_, result, _| {
                if let Ok(Err(e)) = result {
                    log::warn!("swarm: couldn't change the start mode: {e:#}");
                }
            });
        }
        Some(host)
    }

    fn mouse_for(&self, key: &str) -> MouseStateHandle {
        self.mouse.borrow_mut().entry(key.to_string()).or_default().clone()
    }
}

/// A fix for an ssh failure, from ssh's message: what to do, and maybe a
/// command to copy.
fn ssh_fix(host: &str, message: &str) -> Option<(String, Option<String>)> {
    let m = message.to_lowercase();
    if m.contains("permission denied") {
        // ssh lists what the server accepts: "Permission denied (publickey,password)".
        if m.contains("password") || m.contains("keyboard-interactive") {
            Some((
                format!(
                    "Swarm signs in with SSH keys, and {host} didn't accept yours. Set your key up once in a \
                     terminal, typing {host}'s password when ssh asks, then connect again:"
                ),
                Some(key_setup_command(host)),
            ))
        } else {
            Some((
                format!(
                    "{host} only accepts SSH keys, and didn't accept yours. Load your key with this command. If \
                     that doesn't help, ask {host}'s administrator to add your public key (~/.ssh/*.pub)."
                ),
                Some("ssh-add".to_string()),
            ))
        }
    } else if m.contains("host key verification failed") {
        Some((
            format!("{host}'s host key isn't trusted yet. Connect once in a terminal to check and accept it:"),
            Some(format!("ssh {}", shell_words::quote(host))),
        ))
    } else if m.contains("could not resolve hostname") {
        Some((format!("{host} isn't a name ssh can find. Check the spelling, or add it to ~/.ssh/config."), None))
    } else if m.contains("connection refused") {
        Some((format!("Nothing answers on {host}'s ssh port. Is sshd running there?"), None))
    } else if m.contains("timed out") {
        Some((format!("{host} didn't answer. Check the network or VPN, then retry."), None))
    } else {
        None
    }
}

/// Creates a key if there's none, installs it on `host` (ssh-copy-id asks for the password, and
/// does nothing if the key is already there) and loads it, so that non-interactive ssh can use it.
fn key_setup_command(host: &str) -> String {
    format!(
        "test -e ~/.ssh/id_ed25519.pub || test -e ~/.ssh/id_rsa.pub || test -e ~/.ssh/id_ecdsa.pub || ssh-keygen -t ed25519; ssh-copy-id {} && ssh-add",
        shell_words::quote(host)
    )
}

/// ssh's message, with the `host: ` prefix unless ssh already starts with it.
fn failure_text(host: &str, message: &str) -> String {
    if message.starts_with(&format!("{host}:")) {
        message.to_string()
    } else {
        format!("{host}: {message}")
    }
}

impl Entity for SwarmConnect {
    type Event = SwarmConnectEvent;
}

impl TypedActionView for SwarmConnect {
    type Action = SwarmConnectAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            SwarmConnectAction::Check(host) => self.check(host.clone(), ctx),
            SwarmConnectAction::Retry => {
                if let Some(host) = self.host.clone() {
                    self.check(host, ctx);
                }
            }
            SwarmConnectAction::Install => self.install(ctx),
            SwarmConnectAction::SetAtLogin(at_login) => {
                self.at_login = *at_login;
                ctx.notify();
            }
            SwarmConnectAction::ChooseSessions => self.choose_sessions(ctx),
            SwarmConnectAction::NewSession => self.new_session(ctx),
            SwarmConnectAction::CopyFix => {
                if let Outcome::Failed { fix: Some((_, Some(command))), .. } = &self.outcome {
                    ctx.clipboard().write(ClipboardContent::plain_text(command.clone()));
                }
            }
            SwarmConnectAction::RunFix => {
                if let Outcome::Failed { fix: Some((_, Some(command))), .. } = &self.outcome {
                    ctx.emit(SwarmConnectEvent::RunInTerminal(command.clone()));
                }
            }
            SwarmConnectAction::Cancel => ctx.emit(SwarmConnectEvent::Close),
        }
    }
}

impl View for SwarmConnect {
    fn ui_name() -> &'static str {
        "SwarmConnectSheet"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font = appearance.ui_font_family();
        let mono = appearance.monospace_font_family();
        let main = theme.main_text_color(theme.background());
        let sub = theme.sub_text_color(theme.background());
        let accent = theme.accent();
        let colors = theme.terminal_colors().normal;
        let clear = Fill::Solid(ColorU::transparent_black());
        let text = |s: String, size: f32, color: Fill| {
            Text::new_inline(s, font, size).with_color(color.into()).with_clip(ClipConfig::ellipsis()).finish()
        };
        let bold = |s: String, size: f32, color: Fill| {
            Text::new_inline(s, font, size)
                .with_color(color.into())
                .with_style(Properties::default().weight(Weight::Semibold))
                .with_clip(ClipConfig::ellipsis())
                .finish()
        };
        let wrapped = |s: String, color: Fill| Text::new(s, font, 12.).with_color(color.into()).finish();

        // Left: the typed host and the hosts from ~/.ssh/config.
        let field = Container::new(ChildView::new(&self.field).finish())
            .with_background(internal_colors::fg_overlay_1(theme))
            .with_border(Border::all(1.).with_border_fill(internal_colors::fg_overlay_3(theme)))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(7.)))
            .with_padding_left(10.)
            .with_padding_right(10.)
            .with_padding_top(6.)
            .with_padding_bottom(6.)
            .finish();
        let mut hosts = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(2.);
        if self.hosts.is_empty() {
            hosts.add_child(Container::new(text("No hosts in ~/.ssh/config".into(), 12., sub)).with_uniform_padding(8.).finish());
        }
        for (alias, destination) in &self.hosts {
            let picked = self.host.as_deref() == Some(alias.as_str());
            let swatch: Fill = super::host_color(alias).to_ansi_color(&colors).into();
            let (alias_text, detail) = (alias.clone(), destination.clone().unwrap_or_default());
            let action = SwarmConnectAction::Check(alias.clone());
            hosts.add_child(
                Hoverable::new(self.mouse_for(&format!("host:{alias}")), move |state| {
                    let swatch = Container::new(ConstrainedBox::new(Empty::new().finish()).with_width(10.).with_height(10.).finish())
                        .with_background(swatch)
                        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(3.)))
                        .finish();
                    let mut lines = Flex::column().with_child(bold(alias_text.clone(), 13., main));
                    if !detail.is_empty() {
                        lines.add_child(Text::new_inline(detail.clone(), mono, 11.).with_color(sub.into()).with_clip(ClipConfig::ellipsis()).finish());
                    }
                    Container::new(
                        Flex::row()
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_spacing(10.)
                            .with_child(swatch)
                            .with_child(Shrinkable::new(1., lines.finish()).finish())
                            .finish(),
                    )
                    .with_background(if picked {
                        accent.with_opacity(25)
                    } else if state.is_hovered() {
                        internal_colors::fg_overlay_2(theme)
                    } else {
                        clear
                    })
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(7.)))
                    .with_padding_left(10.)
                    .with_padding_right(10.)
                    .with_padding_top(7.)
                    .with_padding_bottom(7.)
                    .finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
                .finish(),
            );
        }
        let hosts = ClippedScrollable::vertical(
            self.scroll.clone(),
            hosts.finish(),
            ScrollbarWidth::Custom(4.),
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            ElementFill::None,
        )
        .with_overlayed_scrollbar()
        .finish();
        let left = Container::new(
            Flex::column()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(8.)
                .with_child(text("Host or user@host — press Return".into(), 12., sub))
                .with_child(field)
                .with_child(Container::new(bold("FROM ~/.SSH/CONFIG".into(), 11., sub)).with_margin_top(6.).finish())
                .with_child(Shrinkable::new(1., hosts).finish())
                .finish(),
        )
        .with_uniform_padding(14.)
        .with_border(Border::right(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
        .finish();

        // Right: the checklist for the picked host.
        let host = self.host.clone().unwrap_or_default();
        let title = match &self.outcome {
            Outcome::Idle => "Choose a host".to_string(),
            Outcome::Checking => format!("Connecting to {host}…"),
            Outcome::Connected(_) => format!("Connected to {host}"),
            Outcome::NeedsInstall { .. } | Outcome::Installing => format!("Set up {host}"),
            Outcome::Failed { .. } => format!("Couldn’t connect to {host}"),
        };
        let mut right = Flex::column().with_main_axis_size(MainAxisSize::Max).with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        right.add_child(Container::new(bold(title, 15., main)).with_margin_bottom(12.).finish());
        if matches!(self.outcome, Outcome::Idle) {
            right.add_child(wrapped(
                "Pick a host on the left, or type one. Swarm checks ssh access, installs its daemon if the host \
                 doesn't have it, and lists the host's sessions."
                    .into(),
                sub,
            ));
        }
        for step in &self.steps {
            let (icon, color): (Option<Icon>, Fill) = match step.mark {
                Mark::Todo => (Some(Icon::CircleRing), theme.disabled_ui_text_color()),
                Mark::Running => (Some(Icon::Loading), sub),
                Mark::Done => (Some(Icon::Check), colors.green.into()),
                Mark::Warn => (Some(Icon::AlertTriangle), colors.yellow.into()),
                Mark::Ask => (Some(Icon::HelpCircle), accent),
                Mark::Failed => (Some(Icon::AlertCircle), colors.red.into()),
            };
            let mark = ConstrainedBox::new(icon.map(|i| i.to_warpui_icon(color).finish()).unwrap_or_else(|| Empty::new().finish()))
                .with_width(16.)
                .with_height(16.)
                .finish();
            let text_color = match step.mark {
                Mark::Todo => theme.disabled_ui_text_color(),
                Mark::Failed => colors.red.into(),
                _ => main,
            };
            let mut lines = Flex::column().with_child(bold(step.text.clone(), 13., text_color));
            if !step.detail.is_empty() {
                lines.add_child(Text::new(step.detail.clone(), font, 12.).with_color(sub.into()).finish());
            }
            right.add_child(
                Container::new(
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Start)
                        .with_spacing(10.)
                        .with_child(Container::new(mark).with_margin_top(1.).finish())
                        .with_child(Shrinkable::new(1., lines.finish()).finish())
                        .finish(),
                )
                .with_margin_bottom(10.)
                .finish(),
            );
        }

        let panel = |content: Box<dyn Element>, tint: Fill| {
            Container::new(content)
                .with_background(tint.with_opacity(18))
                .with_border(Border::all(1.).with_border_fill(tint.with_opacity(45)))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                .with_uniform_padding(12.)
                .with_margin_top(4.)
                .finish()
        };
        match &self.outcome {
            Outcome::Failed { message, fix } => {
                let mut column = Flex::column().with_spacing(6.).with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_child(wrapped(failure_text(&host, message), main));
                if let Some((explanation, command)) = fix {
                    column.add_child(wrapped(explanation.clone(), sub));
                    if let Some(command) = command {
                        column.add_child(
                            Container::new(Text::new(command.clone(), mono, 12.).with_color(main.into()).finish())
                                .with_background(internal_colors::fg_overlay_2(theme))
                                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(5.)))
                                .with_uniform_padding(6.)
                                .finish(),
                        );
                        column.add_child(
                            Flex::row()
                                .with_main_axis_size(MainAxisSize::Max)
                                .with_main_axis_alignment(MainAxisAlignment::End)
                                .with_spacing(8.)
                                .with_child(button(
                                    appearance,
                                    self.mouse_for("copy"),
                                    "Copy".into(),
                                    ButtonStyle::Plain,
                                    true,
                                    SwarmConnectAction::CopyFix,
                                ))
                                .with_child(button(
                                    appearance,
                                    self.mouse_for("run-fix"),
                                    "Open in Terminal".into(),
                                    ButtonStyle::Plain,
                                    true,
                                    SwarmConnectAction::RunFix,
                                ))
                                .finish(),
                        );
                    }
                }
                right.add_child(panel(column.finish(), colors.red.into()));
            }
            Outcome::NeedsInstall { os, installable } => {
                let body = if *installable {
                    format!(
                        "swarmd isn't installed on {host} ({os}). Swarm will copy swarmd, swarm-shim and swarmctl \
                         from this app to ~/.local/share/swarm/bin on {host}. No admin rights needed; remove it \
                         any time from Settings → Hosts."
                    )
                } else {
                    format!(
                        "swarmd isn't installed on {host}, and this build of Swarm has no swarmd for {os}. Build \
                         swarm-term there (cargo build -p swarmd -p swarmctl) and copy swarmd and swarm-shim to \
                         ~/.local/share/swarm/bin, then retry."
                    )
                };
                right.add_child(panel(wrapped(body, main), accent));
            }
            Outcome::Connected(status) => {
                let service = if status.os.to_lowercase().contains("mac") { "a launchd agent" } else { "a systemd user service" };
                let mut modes = Flex::column()
                    .with_spacing(6.)
                    .with_child(Container::new(bold(format!("Start swarmd on {host}"), 12., sub)).with_margin_bottom(2.).finish());
                for (at_login, name, detail) in [
                    (false, "When Swarm connects".to_string(), "keeps running after you disconnect".to_string()),
                    (true, format!("At login on {host}"), format!("installs {service}")),
                ] {
                    let checked = self.at_login == at_login;
                    let (name, detail) = (name.clone(), detail.clone());
                    modes.add_child(
                        Hoverable::new(self.mouse_for(&format!("mode:{at_login}")), move |_| {
                            let radio = if checked { Icon::CircleFilled } else { Icon::CircleRing };
                            Flex::row()
                                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                                .with_spacing(8.)
                                .with_child(
                                    ConstrainedBox::new(radio.to_warpui_icon(if checked { accent } else { sub }).finish())
                                        .with_width(14.)
                                        .with_height(14.)
                                        .finish(),
                                )
                                .with_child(bold(name.clone(), 12., main))
                                .with_child(Shrinkable::new(1., text(format!("— {detail}"), 12., sub)).finish())
                                .finish()
                        })
                        .with_cursor(Cursor::PointingHand)
                        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(SwarmConnectAction::SetAtLogin(at_login)))
                        .finish(),
                    );
                }
                right.add_child(
                    Container::new(modes.finish())
                        .with_border(Border::all(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
                        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                        .with_uniform_padding(12.)
                        .with_margin_top(4.)
                        .finish(),
                );
            }
            Outcome::Idle | Outcome::Checking | Outcome::Installing => {}
        }
        right.add_child(Expanded::new(1., Empty::new().finish()).finish());

        let mut actions = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::End)
            .with_spacing(8.)
            .with_child(button(appearance, self.mouse_for("cancel"), "Cancel".into(), ButtonStyle::Plain, true, SwarmConnectAction::Cancel));
        match &self.outcome {
            Outcome::Failed { .. } => actions.add_child(button(
                appearance,
                self.mouse_for("retry"),
                "Retry".into(),
                ButtonStyle::Primary,
                true,
                SwarmConnectAction::Retry,
            )),
            Outcome::NeedsInstall { installable, .. } => actions.add_child(if *installable {
                button(appearance, self.mouse_for("install"), "Install swarmd".into(), ButtonStyle::Primary, true, SwarmConnectAction::Install)
            } else {
                button(appearance, self.mouse_for("retry"), "Retry".into(), ButtonStyle::Primary, true, SwarmConnectAction::Retry)
            }),
            Outcome::Connected(status) if status.sessions == 0 => actions.add_child(button(
                appearance,
                self.mouse_for("new-session"),
                "New Session →".into(),
                ButtonStyle::Primary,
                true,
                SwarmConnectAction::NewSession,
            )),
            Outcome::Connected(_) => actions.add_child(button(
                appearance,
                self.mouse_for("choose"),
                "Choose Sessions →".into(),
                ButtonStyle::Primary,
                true,
                SwarmConnectAction::ChooseSessions,
            )),
            Outcome::Idle | Outcome::Checking | Outcome::Installing => {}
        }
        right.add_child(actions.finish());
        let right = Container::new(right.finish()).with_uniform_padding(16.).finish();

        let header = Container::new(
            Flex::column()
                .with_spacing(2.)
                .with_child(bold("Connect to a host".into(), 15., main))
                .with_child(text("Uses your SSH keys and ~/.ssh/config. Nothing runs as root.".into(), 12., sub))
                .finish(),
        )
        .with_padding_left(16.)
        .with_padding_right(16.)
        .with_padding_top(14.)
        .with_padding_bottom(12.)
        .with_border(Border::bottom(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
        .finish();
        let body = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(ConstrainedBox::new(left).with_width(HOSTS_WIDTH).finish())
            .with_child(Expanded::new(1., right).finish())
            .finish();
        let sheet = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(Expanded::new(1., body).finish())
            .finish();
        EventHandler::new(
            ConstrainedBox::new(
                Container::new(sheet)
                    .with_background(internal_colors::neutral_1(theme))
                    .with_border(Border::all(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(12.)))
                    .with_drop_shadow(DropShadow::default())
                    .finish(),
            )
            .with_width(WIDTH)
            .with_height(HEIGHT)
            .finish(),
        )
        .on_left_mouse_down(|_, _, _| DispatchEventResult::StopPropagation)
        .finish()
    }
}

#[cfg(test)]
#[path = "connect_tests.rs"]
mod tests;
