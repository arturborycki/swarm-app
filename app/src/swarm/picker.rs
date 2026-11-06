//! The session picker (`docs/DESIGN.md` §9b, Picker mockup): the running
//! sessions on one host that this app doesn't have open, filtered as you
//! type. Open one, several or all of them, start a new one, or terminate them.
//!
//! Keys: ↑↓ move, Space (with an empty filter) or ⇧↩ select, ↩ open the
//! selection (or the highlighted row), ⌘↩ open every match, ⌘⌫ (with an
//! empty filter) terminate them, Esc close. A row's right-click menu renames
//! it, copies its attach command or terminates it.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use warp_core::ui::Icon;
use warp_core::ui::theme::Fill;
use warp_core::ui::theme::color::internal_colors;
use warp_editor::editor::NavigationKey;
use warp_terminal::local_tty::daemon::{self, DaemonSession};
use pathfinder_geometry::vector::vec2f;
use warpui::clipboard::ClipboardContent;
use warpui::elements::{
    Border, ChildAnchor, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, DispatchEventResult, DropShadow, Element, EventHandler, Expanded,
    Fill as ElementFill, Flex, MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning, ParentElement,
    PositionedElementAnchor, PositionedElementOffsetBounds, Radius, SavePosition, ScrollbarWidth, Shrinkable, Stack,
    Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, Entity, SingletonEntity as _, TypedActionView, View, ViewContext, ViewHandle};
use warpui::elements::Hoverable;

use super::ReattachTab;
use super::widgets::ButtonStyle;
use super::sessions::{SessionMark, SwarmSessions};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys, SingleLineEditorOptions, TextOptions};
use crate::menu::{Event as MenuEvent, Menu, MenuItemFields};

const WIDTH: f32 = 620.;
const LIST_MAX_HEIGHT: f32 = 320.;
const SESSION_COLUMN: f32 = 170.;
const RUNNING_COLUMN: f32 = 110.;
const ACTIVITY_COLUMN: f32 = 70.;

pub enum SwarmPickerEvent {
    Close,
    /// Open these sessions as tabs.
    Open(Vec<ReattachTab>),
    /// Start a session on this host (`None`: this Mac).
    NewSession(Option<String>),
}

#[derive(Debug, Clone)]
pub enum SwarmPickerAction {
    PickHost(Option<String>),
    /// Toggle a session's checkbox.
    Toggle(String),
    /// Open just this session (a double-click on its row).
    OpenOne(String),
    OpenSelected,
    OpenAll,
    NewSession,
    /// Terminate the checked sessions.
    Terminate,
    ShowRowMenu(String),
    StartRename(String),
    CopyAttachCommand(String),
    TerminateOne(String),
}

pub struct SwarmPicker {
    filter: ViewHandle<EditorView>,
    query: String,
    host: Option<String>,
    selected: BTreeSet<String>,
    /// The keyboard's row in the filtered list.
    cursor: usize,
    note: Option<String>,
    scroll: ClippedScrollStateHandle,
    mouse: RefCell<HashMap<String, MouseStateHandle>>,
    row_menu: ViewHandle<Menu<SwarmPickerAction>>,
    /// The session whose right-click menu is open.
    row_menu_for: Option<String>,
    rename: ViewHandle<EditorView>,
    /// The session being renamed in place.
    renaming: Option<String>,
}

impl SwarmPicker {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let filter = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_text(Some(13.), appearance),
                    // ↑↓ move through the list instead of the text.
                    propagate_and_no_op_vertical_navigation_keys: PropagateAndNoOpNavigationKeys::Always,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Filter by name, folder or command", ctx);
            editor
        });
        ctx.subscribe_to_view(&filter, |me, editor, event, ctx| match event {
            EditorEvent::Edited(_) => {
                let text = editor.as_ref(ctx).buffer_text(ctx);
                // Space in an empty filter selects the highlighted row.
                if text == " " {
                    editor.update(ctx, |editor, ctx| editor.clear_buffer(ctx));
                    me.toggle_cursor_row(ctx);
                    return;
                }
                me.query = text;
                me.cursor = 0;
                ctx.notify();
            }
            // ⌘⌫ in an empty filter terminates the selection (or the highlighted
            // row). The editor reports it before the edit, so `query` is still
            // the text it had.
            EditorEvent::DeleteAllLeft if me.query.is_empty() => me.terminate_selected(ctx),
            EditorEvent::Navigate(NavigationKey::Up) => me.move_cursor(-1, ctx),
            EditorEvent::Navigate(NavigationKey::Down) => me.move_cursor(1, ctx),
            EditorEvent::ShiftEnter => me.toggle_cursor_row(ctx),
            EditorEvent::Enter => me.open_selected(ctx),
            EditorEvent::CmdEnter => me.open_all(ctx),
            EditorEvent::Escape => ctx.emit(SwarmPickerEvent::Close),
            _ => {}
        });
        let row_menu = ctx.add_typed_action_view(|_| Menu::new().with_drop_shadow());
        ctx.subscribe_to_view(&row_menu, |me, _, event, ctx| {
            if let MenuEvent::Close { .. } = event {
                me.row_menu_for = None;
                if me.renaming.is_none() {
                    ctx.focus(&me.filter);
                }
                ctx.notify();
            }
        });
        let rename = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            EditorView::single_line(
                SingleLineEditorOptions { text: TextOptions::ui_text(Some(13.), appearance), ..Default::default() },
                ctx,
            )
        });
        ctx.subscribe_to_view(&rename, |me, editor, event, ctx| match event {
            EditorEvent::Enter => {
                let name = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
                me.finish_rename(Some(name), ctx);
            }
            EditorEvent::Escape | EditorEvent::Blurred => me.finish_rename(None, ctx),
            _ => {}
        });
        let sessions = SwarmSessions::handle(ctx);
        ctx.subscribe_to_model(&sessions, |me, _, _, ctx| {
            // Drop selections of sessions that went away or were opened.
            let rows: BTreeSet<String> = me.rows(ctx).into_iter().map(|s| s.id).collect();
            me.selected.retain(|id| rows.contains(id));
            ctx.notify();
        });
        SwarmPicker {
            filter,
            query: String::new(),
            host: None,
            selected: BTreeSet::new(),
            cursor: 0,
            note: None,
            scroll: Default::default(),
            mouse: Default::default(),
            row_menu,
            row_menu_for: None,
            rename,
            renaming: None,
        }
    }

    /// Resets the picker for `host` (`None`: this Mac) and focuses the filter.
    pub fn open_on(&mut self, host: Option<String>, ctx: &mut ViewContext<Self>) {
        self.host = host;
        self.selected.clear();
        self.cursor = 0;
        self.note = None;
        self.query.clear();
        self.filter.update(ctx, |editor, ctx| editor.clear_buffer(ctx));
        ctx.focus(&self.filter);
        ctx.notify();
    }

    /// This Mac, then each followed host, with how many sessions each offers.
    fn hosts(&self, app: &AppContext) -> Vec<(Option<String>, usize)> {
        let model = SwarmSessions::as_ref(app);
        let mut hosts = vec![None];
        hosts.extend(model.followed_hosts().into_iter().map(Some));
        hosts.into_iter().map(|h| (h.clone(), openable(model.sessions_on(h.as_deref())).count())).collect()
    }

    /// The sessions on the picked host this app doesn't have open, filtered.
    fn rows(&self, app: &AppContext) -> Vec<DaemonSession> {
        let query = self.query.trim().to_lowercase();
        openable(SwarmSessions::as_ref(app).sessions_on(self.host.as_deref()))
            .filter(|s| {
                query.is_empty()
                    || [s.display_name(), s.cwd.as_deref().unwrap_or_default(), s.foreground.as_deref().unwrap_or_default()]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    /// The host has no sessions at all, open or not, and nothing is being filtered.
    fn has_no_sessions(&self, app: &AppContext) -> bool {
        let sessions = SwarmSessions::as_ref(app);
        let host = self.host.as_deref();
        self.query.trim().is_empty() && sessions.is_loaded(host) && sessions.sessions_on(host).is_empty()
    }

    fn move_cursor(&mut self, by: isize, ctx: &mut ViewContext<Self>) {
        let len = self.rows(ctx).len();
        if len > 0 {
            self.cursor = (self.cursor as isize + by).clamp(0, len as isize - 1) as usize;
            ctx.notify();
        }
    }

    fn toggle_cursor_row(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(row) = self.rows(ctx).get(self.cursor) {
            self.toggle(row.id.clone(), ctx);
        }
    }

    fn toggle(&mut self, id: String, ctx: &mut ViewContext<Self>) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        ctx.notify();
    }

    fn open(&mut self, sessions: Vec<DaemonSession>, ctx: &mut ViewContext<Self>) {
        if sessions.is_empty() {
            return;
        }
        let tabs = sessions.iter().map(|s| ReattachTab::for_session(self.host.as_deref(), s)).collect();
        ctx.emit(SwarmPickerEvent::Open(tabs));
    }

    /// The checked sessions, or the highlighted one when none is checked.
    fn open_selected(&mut self, ctx: &mut ViewContext<Self>) {
        let rows = self.rows(ctx);
        if self.has_no_sessions(ctx) {
            ctx.emit(SwarmPickerEvent::NewSession(self.host.clone()));
            return;
        }
        let picked: Vec<_> = if self.selected.is_empty() {
            rows.get(self.cursor).cloned().into_iter().collect()
        } else {
            rows.into_iter().filter(|s| self.selected.contains(&s.id)).collect()
        };
        self.open(picked, ctx);
    }

    fn open_all(&mut self, ctx: &mut ViewContext<Self>) {
        let rows = self.rows(ctx);
        self.open(rows, ctx);
    }

    /// Terminates the checked rows, or the highlighted one, after a confirmation.
    fn terminate_selected(&mut self, ctx: &mut ViewContext<Self>) {
        let ids = if self.selected.is_empty() {
            self.rows(ctx).get(self.cursor).map(|row| BTreeSet::from([row.id.clone()])).unwrap_or_default()
        } else {
            self.selected.clone()
        };
        self.terminate(&ids, ctx);
    }

    /// Terminates `ids` (of the listed sessions) after a confirmation.
    fn terminate(&mut self, ids: &BTreeSet<String>, ctx: &mut ViewContext<Self>) {
        use warpui::modals::{AlertDialogWithCallbacks, ModalButton};
        let picked: Vec<DaemonSession> = self.rows(ctx).into_iter().filter(|s| ids.contains(&s.id)).collect();
        let Some(first) = picked.first() else { return };
        let (title, body) = match picked.len() {
            1 => (
                format!("Terminate {}?", first.display_name()),
                "Its process gets SIGHUP, as when a terminal window closes. Anything running in it stops.",
            ),
            n => (
                format!("Terminate {n} sessions?"),
                "Their processes get SIGHUP, as when a terminal window closes. Anything running in them stops.",
            ),
        };
        let host = self.host.clone();
        let ids: Vec<String> = picked.iter().map(|s| s.id.clone()).collect();
        let dialog = AlertDialogWithCallbacks::for_view(
            title,
            body,
            vec![
                ModalButton::for_view("Terminate", move |me: &mut Self, ctx| {
                    let Some(daemons) = daemon::daemons() else { return };
                    me.selected.retain(|id| !ids.contains(id));
                    let (host, ids) = (host.clone(), ids.clone());
                    let rx = daemons.run(async move {
                        for id in ids {
                            daemon::terminate_session(host.as_deref(), &id).await?;
                        }
                        anyhow::Ok(())
                    });
                    let _ = ctx.spawn(async move { rx.recv().await }, |me, result, ctx| {
                        me.note = match result {
                            Ok(Ok(())) => None,
                            Ok(Err(e)) => Some(format!("{e:#}")),
                            Err(_) => Some("The daemon connection closed.".into()),
                        };
                        ctx.notify();
                    });
                }),
                ModalButton::for_view("Cancel", |_, _| {}),
            ],
            |_, _| {},
        )
        .without_dont_ask_again();
        ctx.show_native_platform_modal(dialog);
    }

    fn show_row_menu(&mut self, id: String, ctx: &mut ViewContext<Self>) {
        let items = vec![
            MenuItemFields::new("Rename…").with_on_select_action(SwarmPickerAction::StartRename(id.clone())).into_item(),
            MenuItemFields::new("Copy Attach Command")
                .with_on_select_action(SwarmPickerAction::CopyAttachCommand(id.clone()))
                .into_item(),
            MenuItemFields::new("Terminate…").with_on_select_action(SwarmPickerAction::TerminateOne(id.clone())).into_item(),
        ];
        self.row_menu.update(ctx, |menu, ctx| menu.set_items(items, ctx));
        self.row_menu_for = Some(id);
        self.note = None;
        ctx.focus(&self.row_menu);
        ctx.notify();
    }

    fn start_rename(&mut self, id: String, ctx: &mut ViewContext<Self>) {
        let Some(session) = self.rows(ctx).into_iter().find(|s| s.id == id) else { return };
        let current = session.name.clone().unwrap_or_default();
        self.rename.update(ctx, |editor, ctx| {
            editor.set_buffer_text(&current, ctx);
            editor.select_all(ctx);
        });
        self.renaming = Some(id);
        ctx.focus(&self.rename);
        ctx.notify();
    }

    /// Ends renaming: `Some(name)` saves it (an empty name goes back to the
    /// shell's title), `None` cancels.
    fn finish_rename(&mut self, name: Option<String>, ctx: &mut ViewContext<Self>) {
        let Some(id) = self.renaming.take() else { return };
        ctx.focus(&self.filter);
        ctx.notify();
        let (Some(name), Some(daemons)) = (name, daemon::daemons()) else { return };
        let host = self.host.clone();
        let name = (!name.is_empty()).then_some(name);
        let rx = daemons.run(async move { daemon::rename_session(host.as_deref(), &id, name).await });
        let _ = ctx.spawn(async move { rx.recv().await }, |me, result, ctx| {
            if let Ok(Err(e)) = result {
                me.note = Some(format!("Couldn't rename: {e:#}"));
                ctx.notify();
            }
        });
    }

    fn copy_attach_command(&mut self, id: &str, ctx: &mut ViewContext<Self>) {
        let command = match &self.host {
            Some(host) => format!("swarmctl attach --host {host} {id}"),
            None => format!("swarmctl attach {id}"),
        };
        ctx.clipboard().write(ClipboardContent::plain_text(command.clone()));
        self.note = Some(format!("Copied: {command}"));
        ctx.notify();
    }

    fn mouse_for(&self, key: &str) -> MouseStateHandle {
        self.mouse.borrow_mut().entry(key.to_string()).or_default().clone()
    }
}

/// Running sessions no window of this app has open.
fn openable(sessions: &[DaemonSession]) -> impl Iterator<Item = &DaemonSession> {
    let me = daemon::client_name();
    sessions.iter().filter(move |s| s.is_running() && !s.clients.iter().any(|c| c.name == me))
}

impl Entity for SwarmPicker {
    type Event = SwarmPickerEvent;
}

impl TypedActionView for SwarmPicker {
    type Action = SwarmPickerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            SwarmPickerAction::PickHost(host) => {
                let host = host.clone();
                self.open_on(host, ctx);
            }
            SwarmPickerAction::Toggle(id) => self.toggle(id.clone(), ctx),
            SwarmPickerAction::OpenOne(id) => {
                let picked = self.rows(ctx).into_iter().filter(|s| &s.id == id).collect();
                self.open(picked, ctx);
            }
            SwarmPickerAction::OpenSelected => self.open_selected(ctx),
            SwarmPickerAction::OpenAll => self.open_all(ctx),
            SwarmPickerAction::NewSession => ctx.emit(SwarmPickerEvent::NewSession(self.host.clone())),
            SwarmPickerAction::Terminate => {
                let ids = self.selected.clone();
                self.terminate(&ids, ctx);
            }
            SwarmPickerAction::ShowRowMenu(id) => self.show_row_menu(id.clone(), ctx),
            SwarmPickerAction::StartRename(id) => self.start_rename(id.clone(), ctx),
            SwarmPickerAction::CopyAttachCommand(id) => self.copy_attach_command(id, ctx),
            SwarmPickerAction::TerminateOne(id) => self.terminate(&BTreeSet::from([id.clone()]), ctx),
        }
    }
}

impl View for SwarmPicker {
    fn ui_name() -> &'static str {
        "SwarmSessionPicker"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font = appearance.ui_font_family();
        let mono = appearance.monospace_font_family();
        let main = theme.main_text_color(theme.background());
        let sub = theme.sub_text_color(theme.background());
        let accent = theme.accent();
        let rows = self.rows(app);
        let no_sessions = self.has_no_sessions(app);
        let selected = rows.iter().filter(|s| self.selected.contains(&s.id)).count();
        let host_name = self.host.clone().unwrap_or_else(|| "This Mac".to_string());

        let text = |s: String, size: f32, color: Fill| {
            Text::new_inline(s, font, size).with_color(color.into()).with_clip(ClipConfig::ellipsis()).finish()
        };
        let button = |label: String, key: &str, style: ButtonStyle, enabled: bool, action: SwarmPickerAction| {
            super::widgets::button(appearance, self.mouse_for(key), label, style, enabled, action)
        };

        // Header: title and one segment per host.
        let mut segments = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center).with_spacing(2.);
        for (host, count) in self.hosts(app) {
            let picked = host == self.host;
            let label = format!("{}  {count}", host.clone().unwrap_or_else(|| "This Mac".to_string()));
            let swatch_color: Fill = super::host_fill(theme, host.as_deref());
            let key = format!("host:{}", host.clone().unwrap_or_default());
            let action = SwarmPickerAction::PickHost(host);
            segments.add_child(
                Hoverable::new(self.mouse_for(&key), move |state| {
                    let swatch = Container::new(ConstrainedBox::new(warpui::elements::Empty::new().finish()).with_width(8.).with_height(8.).finish())
                        .with_background(swatch_color)
                        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(2.)))
                        .finish();
                    Container::new(
                        Flex::row()
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_spacing(6.)
                            .with_child(swatch)
                            .with_child(
                                Text::new_inline(label.clone(), font, 12.)
                                    .with_color(if picked { main.into() } else { sub.into() })
                                    .finish(),
                            )
                            .finish(),
                    )
                    .with_background(if picked || state.is_hovered() {
                        internal_colors::fg_overlay_3(theme)
                    } else {
                        Fill::Solid(warpui::color::ColorU::transparent_black())
                    })
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                    .with_padding_left(10.)
                    .with_padding_right(10.)
                    .with_padding_top(4.)
                    .with_padding_bottom(4.)
                    .finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
                .finish(),
            );
        }
        let header = Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Expanded::new(
                        1.,
                        Text::new_inline("Open sessions", font, 14.)
                            .with_color(main.into())
                            .with_style(Properties::default().weight(Weight::Semibold))
                            .finish(),
                    )
                    .finish(),
                )
                .with_child(
                    Container::new(segments.finish())
                        .with_background(internal_colors::fg_overlay_1(theme))
                        .with_border(Border::all(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
                        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                        .with_uniform_padding(2.)
                        .finish(),
                )
                .finish(),
        )
        .with_padding_left(14.)
        .with_padding_right(14.)
        .with_padding_top(12.)
        .with_padding_bottom(10.)
        .finish();

        let filter = Container::new(ChildView::new(&self.filter).finish())
            .with_background(internal_colors::fg_overlay_1(theme))
            .with_border(Border::all(1.).with_border_fill(accent))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
            .with_padding_left(10.)
            .with_padding_right(10.)
            .with_padding_top(6.)
            .with_padding_bottom(6.)
            .with_margin_left(14.)
            .with_margin_right(14.)
            .with_margin_bottom(10.)
            .finish();

        let columns = |check: Box<dyn Element>,
                       session: Box<dyn Element>,
                       folder: Box<dyn Element>,
                       running: Box<dyn Element>,
                       activity: Box<dyn Element>| {
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(ConstrainedBox::new(check).with_width(20.).finish())
                .with_child(ConstrainedBox::new(session).with_width(SESSION_COLUMN).finish())
                .with_child(Expanded::new(1., folder).finish())
                .with_child(ConstrainedBox::new(running).with_width(RUNNING_COLUMN).finish())
                .with_child(
                    ConstrainedBox::new(
                        Flex::row()
                            .with_main_axis_size(MainAxisSize::Max)
                            .with_main_axis_alignment(MainAxisAlignment::End)
                            .with_child(activity)
                            .finish(),
                    )
                    .with_width(ACTIVITY_COLUMN)
                    .finish(),
                )
                .finish()
        };
        let heading = |s: &str| {
            Text::new_inline(s.to_string(), font, 11.)
                .with_color(sub.into())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish()
        };
        let column_header = Container::new(columns(
            warpui::elements::Empty::new().finish(),
            heading("SESSION"),
            heading("FOLDER"),
            heading("RUNNING"),
            heading("ACTIVITY"),
        ))
        .with_padding_left(14.)
        .with_padding_right(14.)
        .with_padding_top(6.)
        .with_padding_bottom(6.)
        .with_border(Border::bottom(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
        .finish();

        let mut list = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        if rows.is_empty() {
            let message = if no_sessions {
                format!("No sessions on {host_name} yet.")
            } else if !SwarmSessions::as_ref(app).is_loaded(self.host.as_deref()) {
                format!("Looking for sessions on {host_name}…")
            } else if self.query.trim().is_empty() {
                format!("Every session on {host_name} is open.")
            } else {
                format!("No sessions match “{}”.", self.query.trim())
            };
            list.add_child(
                Container::new(warpui::prelude::Align::new(text(message, 13., sub)).finish())
                    .with_padding_top(28.)
                    .with_padding_bottom(28.)
                    .finish(),
            );
        }
        let me = daemon::client_name();
        for (index, s) in rows.iter().enumerate() {
            let checked = self.selected.contains(&s.id);
            let highlighted = index == self.cursor;
            let mark = if s.foreground.is_some() { SessionMark::Busy } else { SessionMark::Idle };
            let (icon, icon_color) = mark.icon(theme, self.host.as_deref());
            let elsewhere = s.clients.iter().any(|c| c.name != me);

            let checkbox = {
                let mut box_ = Container::new(if checked {
                    Icon::Check.to_warpui_icon(Fill::Solid(warpui::color::ColorU::white())).finish()
                } else {
                    warpui::elements::Empty::new().finish()
                })
                .with_border(Border::all(1.).with_border_fill(if checked { accent } else { internal_colors::fg_overlay_4(theme) }))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .with_uniform_padding(1.);
                if checked {
                    box_ = box_.with_background(accent);
                }
                ConstrainedBox::new(box_.finish()).with_width(15.).with_height(15.).finish()
            };
            let mut name = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(ConstrainedBox::new(icon.to_warpui_icon(icon_color).finish()).with_width(12.).with_height(12.).finish())
                .with_child(
                    Shrinkable::new(
                        1.,
                        if self.renaming.as_deref() == Some(s.id.as_str()) {
                            Container::new(ChildView::new(&self.rename).finish())
                                .with_background(internal_colors::fg_overlay_1(theme))
                                .with_border(Border::all(1.).with_border_fill(accent))
                                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                                .with_padding_left(4.)
                                .with_padding_right(4.)
                                .finish()
                        } else {
                            Text::new_inline(s.display_name().to_string(), font, 13.)
                                .with_color(main.into())
                                .with_style(Properties::default().weight(Weight::Semibold))
                                .with_clip(ClipConfig::ellipsis())
                                .finish()
                        },
                    )
                    .finish(),
                );
            if elsewhere {
                name.add_child(
                    Container::new(Text::new_inline("also open".to_string(), font, 10.).with_color(main.into()).finish())
                        .with_background(accent.with_opacity(30))
                        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                        .with_padding_left(5.)
                        .with_padding_right(5.)
                        .with_padding_top(1.)
                        .with_padding_bottom(1.)
                        .finish(),
                );
            }
            let cwd = s.cwd.as_deref().map(|c| if self.host.is_none() { super::home_relative(c) } else { c.to_string() });
            let mono_text = |s: String| {
                Text::new_inline(s, mono, 12.).with_color(sub.into()).with_clip(ClipConfig::ellipsis()).finish()
            };
            let running = s.foreground.clone().unwrap_or_else(|| {
                s.command.first().map(|p| p.rsplit('/').next().unwrap_or(p).trim_start_matches('-').to_string()).unwrap_or_default()
            });
            let row = columns(
                checkbox,
                name.finish(),
                mono_text(cwd.unwrap_or_default()),
                mono_text(running),
                text(super::ago(s.last_output_ms.max(s.last_input_ms)), 12., sub),
            );
            let (toggle, open) = (SwarmPickerAction::Toggle(s.id.clone()), SwarmPickerAction::OpenOne(s.id.clone()));
            let menu = SwarmPickerAction::ShowRowMenu(s.id.clone());
            let row_highlighted = highlighted || self.row_menu_for.as_deref() == Some(s.id.as_str());
            let row = Hoverable::new(self.mouse_for(&s.id), move |state| {
                    let background = if checked {
                        accent.with_opacity(25)
                    } else if row_highlighted || state.is_hovered() {
                        internal_colors::fg_overlay_2(theme)
                    } else {
                        Fill::Solid(warpui::color::ColorU::transparent_black())
                    };
                    Container::new(row)
                        .with_background(background)
                        .with_padding_left(14.)
                        .with_padding_right(14.)
                        .with_padding_top(8.)
                        .with_padding_bottom(8.)
                        .with_border(Border::bottom(1.).with_border_fill(internal_colors::fg_overlay_1(theme)))
                        .finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(toggle.clone()))
                .on_double_click(move |ctx, _, _| ctx.dispatch_typed_action(open.clone()))
                .on_right_click(move |ctx, _, _| ctx.dispatch_typed_action(menu.clone()))
                .finish();
            list.add_child(SavePosition::new(row, &row_position_id(&s.id)).finish());
        }
        let list = ConstrainedBox::new(
            ClippedScrollable::vertical(
                self.scroll.clone(),
                list.finish(),
                ScrollbarWidth::Custom(4.),
                theme.nonactive_ui_detail().into(),
                theme.active_ui_detail().into(),
                ElementFill::None,
            )
            .with_overlayed_scrollbar()
            .finish(),
        )
        .with_max_height(LIST_MAX_HEIGHT)
        .finish();

        let count = if selected > 0 { format!(" ({selected})") } else { String::new() };
        let mut footer = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.);
        if no_sessions {
            // Nothing to open or terminate: the only thing to do is start one.
            footer.add_child(Expanded::new(1., warpui::elements::Empty::new().finish()).finish());
            footer.add_child(button(format!("New session on {host_name}"), "new", ButtonStyle::Primary, true, SwarmPickerAction::NewSession));
        } else {
            footer.add_child(button(format!("+ New session on {host_name}"), "new", ButtonStyle::Link, true, SwarmPickerAction::NewSession));
            footer.add_child(Expanded::new(1., warpui::elements::Empty::new().finish()).finish());
            footer.add_child(button(format!("Terminate{count}"), "terminate", ButtonStyle::Plain, selected > 0, SwarmPickerAction::Terminate));
            footer.add_child(button("Open all".to_string(), "open_all", ButtonStyle::Plain, !rows.is_empty(), SwarmPickerAction::OpenAll));
            footer.add_child(button(format!("Open{count}"), "open", ButtonStyle::Primary, !rows.is_empty(), SwarmPickerAction::OpenSelected));
        }
        let footer = Container::new(footer.finish())
            .with_background(internal_colors::fg_overlay_1(theme))
            .with_border(Border::top(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
            .with_padding_left(14.)
            .with_padding_right(14.)
            .with_padding_top(10.)
            .with_padding_bottom(10.)
            .finish();
        let hint = self.note.clone().unwrap_or_else(|| {
            "↑↓ move · Space or ⇧↩ select · ↩ open · ⌘↩ open all matches · ⌘⌫ terminate · right-click for more"
                .to_string()
        });
        let hint = Container::new(text(hint, 11., sub))
            .with_border(Border::top(1.).with_border_fill(internal_colors::fg_overlay_1(theme)))
            .with_padding_left(14.)
            .with_padding_right(14.)
            .with_padding_top(7.)
            .with_padding_bottom(7.)
            .finish();

        let body = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(filter)
            .with_child(column_header)
            .with_child(list)
            .with_child(footer)
            .with_child(hint)
            .finish();
        let picker = EventHandler::new(
            ConstrainedBox::new(
                Container::new(body)
                    .with_background(internal_colors::neutral_1(theme))
                    .with_border(Border::all(1.).with_border_fill(internal_colors::fg_overlay_2(theme)))
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(12.)))
                    .with_drop_shadow(DropShadow::default())
                    .finish(),
            )
            .with_width(WIDTH)
            .finish(),
        )
        .on_left_mouse_down(|_, _, _| DispatchEventResult::StopPropagation)
        .finish();
        let mut stack = Stack::new().with_child(picker);
        if let Some(id) = self.row_menu_for.as_deref().filter(|id| rows.iter().any(|s| s.id == *id)) {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.row_menu).finish(),
                OffsetPositioning::offset_from_save_position_element(
                    row_position_id(id),
                    vec2f(40., -2.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::BottomLeft,
                    ChildAnchor::TopLeft,
                ),
            );
        }
        stack.finish()
    }
}

fn row_position_id(session_id: &str) -> String {
    format!("swarm_picker_row:{session_id}")
}
