//! swarm-term: daemon sessions in the navigation palette (Go to Anything,
//! ⌘P or ⇧⌘P), next to the open tabs: open sessions by what their programs
//! report ("needs you"), detached sessions on this Mac and connected hosts to
//! reattach, "New session on …" for each host, and "New session in …" for
//! the folders sessions use and for a typed path, and "Open localhost:…" for
//! ports that programs in sessions listen on. Type to filter; Return opens.

use fuzzy_match::{FuzzyMatchResult, match_indices_case_insensitive};
use ordered_float::OrderedFloat;
use warpui::elements::{Align, ConstrainedBox, Flex, Highlight, ParentElement, Shrinkable, Text};
use warpui::fonts::{Properties, Weight};
use warpui::{AppContext, Element, Entity, SingletonEntity};

use crate::appearance::Appearance;
use crate::search::command_palette::mixer::CommandPaletteItemAction;
use crate::search::command_palette::render_util::render_search_item_icon;
use crate::search::data_source::{Query, QueryResult};
use crate::search::mixer::{DataSourceRunErrorWrapper, SyncDataSource};
use crate::search::result_renderer::ItemHighlightState;
use crate::ui_components::icons::Icon;
use crate::workspace::WorkspaceAction;

pub struct DataSource;

/// Whether `text` reads as an ssh destination: `user@host` or a dotted host
/// name, with no spaces (so ordinary searches don't offer to connect).
fn looks_like_host(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(char::is_whitespace)
        && (text.contains('@') || text.contains('.'))
        && !text.starts_with(['-', '.', '@'])
        && text.chars().all(|c| c.is_ascii_alphanumeric() || "@.-_".contains(c))
}

impl Entity for DataSource {
    type Event = ();
}

impl SyncDataSource for DataSource {
    type Action = CommandPaletteItemAction;

    fn run_query(
        &self,
        query: &Query,
        app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        let term = query.text.trim();
        let mut items = Vec::new();
        #[cfg(unix)]
        {
            use crate::swarm::sessions::SwarmSessions;
            use warp_terminal::local_tty::daemon;
            let sessions = SwarmSessions::as_ref(app);
            let mut hosts: Vec<Option<String>> = vec![None];
            hosts.extend(daemon::known_hosts().into_iter().map(Some));
            // "rename pane to <name>": names the focused pane's session.
            if let Some(name) = term.strip_prefix("rename pane to").or_else(|| term.strip_prefix("Rename pane to")) {
                let target = crate::swarm::PANE_RENAME_TARGET.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some((warp_session_id, current)) = target {
                    let name = name.trim();
                    let (label, new) = if name.is_empty() {
                        ("Clear the pane's name".to_string(), None)
                    } else {
                        (format!("Rename pane to {name}"), Some(name.to_string()))
                    };
                    items.push((label, format!("now {current}"), WorkspaceAction::SwarmRenamePane { warp_session_id, name: new }));
                }
            }
            // "move pane …": the tabs the focused pane can move into.
            if term.to_lowercase().starts_with("move") {
                let targets = crate::swarm::PANE_MOVE_TARGETS.lock().unwrap_or_else(|e| e.into_inner()).clone();
                for (position, (pane_group, title)) in targets.into_iter().enumerate() {
                    items.push((
                        format!("Move pane to {title}"),
                        format!("tab {}", position + 1),
                        WorkspaceAction::SwarmMovePaneToTab(pane_group),
                    ));
                }
            }
            // A host that isn't in ~/.ssh/config: `user@host` or a host name.
            if looks_like_host(term) {
                items.push((
                    format!("Connect to {term}"),
                    "over ssh".to_string(),
                    WorkspaceAction::SwarmConnectHost(term.to_string()),
                ));
            }
            // A typed port ("3000", ":3000", "port 3000"): forward it from
            // each connected host and open it.
            if let Some(port) = typed_port(term) {
                for host in sessions.followed_hosts() {
                    items.push((
                        format!("Forward port {port} from {host}"),
                        format!("open localhost:{port}"),
                        WorkspaceAction::SwarmForwardPort { host, port },
                    ));
                }
            }
            for (host, remote, local) in daemon::port_forwards() {
                items.push((
                    format!("Stop forwarding {host}:{remote}"),
                    format!("localhost:{local}"),
                    WorkspaceAction::SwarmStopForward { host, port: remote },
                ));
            }
            // Ports programs in sessions listen on, such as a dev server:
            // open it, forwarded first when it's on another host.
            for (host, id, p) in daemon::detected_ports() {
                let name = sessions
                    .sessions_on(host.as_deref())
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.display_name().to_string())
                    .unwrap_or_default();
                let (label, detail) = match &host {
                    None => (format!("Open localhost:{}", p.port), format!("{name} · {}", p.program)),
                    Some(h) => (
                        format!("Open port {} from {name}", p.port),
                        format!("{h} · {} · forwarded to localhost", p.program),
                    ),
                };
                items.push((label, detail, WorkspaceAction::SwarmOpenPort { host, port: p.port }));
            }
            // A typed folder on this Mac: "~/src/api" or "/tmp".
            if let Some(folder) = typed_folder(term) {
                items.push((
                    format!("New session in {term}"),
                    "this Mac".to_string(),
                    WorkspaceAction::SwarmNewSessionIn { host: None, cwd: folder },
                ));
            }
            for host in &hosts {
                let place = host.clone().unwrap_or_else(|| "this Mac".to_string());
                // Open sessions whose programs report something, by state:
                // typing "needs you" or "done" finds them.
                for session in sessions.sessions_on(host.as_deref()) {
                    let Some(warp_id) = session.ui.get(daemon::META_SESSION_ID).and_then(|v| v.parse::<u64>().ok())
                    else {
                        continue;
                    };
                    let Some(status) = sessions.status_for(warp_id) else { continue };
                    let Some(program) = status.program.clone() else { continue };
                    if session.clients.is_empty() {
                        continue;
                    }
                    items.push((
                        format!("Go to {} · {program}", status.display_name()),
                        place.clone(),
                        WorkspaceAction::SwarmShowSession(warp_id),
                    ));
                }
                // Folders the host's sessions are in, for a new session there.
                let mut folders: Vec<String> =
                    sessions.sessions_on(host.as_deref()).iter().filter_map(|s| s.cwd.clone()).collect();
                folders.sort();
                folders.dedup();
                for folder in folders {
                    let shown = match host {
                        None => crate::swarm::home_relative(&folder),
                        Some(_) => crate::swarm::remote_home_relative(&folder),
                    };
                    if shown == "~" {
                        continue;
                    }
                    items.push((
                        format!("New session in {shown}"),
                        place.clone(),
                        WorkspaceAction::SwarmNewSessionIn { host: host.clone(), cwd: folder },
                    ));
                }
                for session in sessions.sessions_on(host.as_deref()) {
                    if !session.is_running() || !session.clients.is_empty() {
                        continue;
                    }
                    let tab = crate::swarm::ReattachTab::for_session(host.as_deref(), session);
                    let name = session.name.clone().unwrap_or_else(|| session.title.clone());
                    let what = session.foreground.clone().unwrap_or_else(|| "idle".to_string());
                    let label = format!("Reattach {name}");
                    let detail = format!("{place} · {} · {what}", session.cwd.as_deref().unwrap_or("~"));
                    items.push((label, detail, WorkspaceAction::SwarmReattach(tab)));
                }
                items.push((
                    format!("New session on {place}"),
                    String::new(),
                    WorkspaceAction::SwarmNewSession { host: host.clone() },
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = app;
        Ok(items
            .into_iter()
            .filter_map(|(label, detail, action)| {
                let mut match_result = if term.is_empty() {
                    FuzzyMatchResult { score: 0, matched_indices: Vec::new() }
                } else {
                    match_indices_case_insensitive(&label, term)?
                };
                // Sessions that need you, then running sessions to reattach,
                // come before starting new ones.
                if matches!(action, WorkspaceAction::SwarmShowSession(_) | WorkspaceAction::SwarmOpenPort { .. }) {
                    match_result.score += 2;
                }
                if matches!(action, WorkspaceAction::SwarmReattach(_)) {
                    match_result.score += 1;
                }
                if matches!(action, WorkspaceAction::SwarmConnectHost(_)) {
                    match_result.score += 1000;
                }
                Some(QueryResult::from(SearchItem { label, detail, action, match_result }))
            })
            .collect())
    }
}

/// `term` as a port number: "3000", ":3000" or "port 3000".
fn typed_port(term: &str) -> Option<u16> {
    let digits = term.strip_prefix("port").map(str::trim_start).unwrap_or(term);
    let digits = digits.strip_prefix(':').unwrap_or(digits);
    if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u16>().ok().filter(|p| *p > 0)
}

/// `term` as a folder on this Mac, if it's a path ("~/src", "/tmp") to an
/// existing folder.
fn typed_folder(term: &str) -> Option<String> {
    let path = if let Some(rest) = term.strip_prefix('~') {
        let home = std::env::var("HOME").ok()?;
        format!("{home}{rest}")
    } else if term.starts_with('/') {
        term.to_string()
    } else {
        return None;
    };
    std::path::Path::new(&path).is_dir().then_some(path)
}

#[derive(Debug)]
struct SearchItem {
    label: String,
    detail: String,
    action: WorkspaceAction,
    match_result: FuzzyMatchResult,
}

impl crate::search::item::SearchItem for SearchItem {
    type Action = CommandPaletteItemAction;

    fn render_icon(&self, highlight_state: ItemHighlightState, appearance: &Appearance) -> Box<dyn Element> {
        let color = appearance.theme().foreground().into_solid();
        render_search_item_icon(appearance, Icon::Navigation, color, highlight_state)
    }

    fn render_item(&self, highlight_state: ItemHighlightState, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let bg = highlight_state.container_background_fill(appearance).unwrap_or_else(|| theme.surface_2());
        let text_color = theme.main_text_color(bg).into_solid();
        let label = Text::new_inline(self.label.clone(), appearance.ui_font_family(), appearance.monospace_font_size())
            .with_color(theme.sub_text_color(bg).into_solid())
            .with_single_highlight(
                Highlight::new().with_properties(Properties::default().weight(Weight::Bold)).with_foreground_color(text_color),
                self.match_result.matched_indices.clone(),
            )
            .finish();
        let mut row = Flex::row().with_child(Shrinkable::new(1., Align::new(label).left().finish()).finish());
        if !self.detail.is_empty() {
            row.add_child(
                Text::new_inline(self.detail.clone(), appearance.ui_font_family(), appearance.monospace_font_size() - 1.)
                    .with_color(theme.hint_text_color(bg).into_solid())
                    .finish(),
            );
        }
        ConstrainedBox::new(row.finish()).with_height(40.).finish()
    }

    fn score(&self) -> OrderedFloat<f64> {
        OrderedFloat::from(self.match_result.score as f64)
    }

    fn accept_result(&self) -> Self::Action {
        CommandPaletteItemAction::Swarm(Box::new(self.action.clone()))
    }

    fn execute_result(&self) -> Self::Action {
        self.accept_result()
    }

    fn accessibility_label(&self) -> String {
        format!("Selected {}.", self.label)
    }

    fn accessibility_help_message(&self) -> Option<String> {
        Some("Press enter to open this session.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{looks_like_host, typed_folder, typed_port};

    #[test]
    fn typed_ports() {
        assert_eq!(typed_port("3000"), Some(3000));
        assert_eq!(typed_port(":8080"), Some(8080));
        assert_eq!(typed_port("port 5173"), Some(5173));
        for no in ["", "0", "70000", "abc", "30 00", "port"] {
            assert_eq!(typed_port(no), None, "{no}");
        }
    }

    #[test]
    fn typed_folders() {
        assert_eq!(typed_folder("/tmp").as_deref(), Some("/tmp"));
        assert!(typed_folder("~").is_some());
        assert!(typed_folder("/no/such/folder/here").is_none());
        assert!(typed_folder("tmp").is_none());
    }

    #[test]
    fn host_like_queries() {
        for yes in ["me@devbox", "build.example.com", "me@10.0.0.5", "ci-01.internal"] {
            assert!(looks_like_host(yes), "{yes}");
        }
        for no in ["", "palette", "new session", "@host", ".hidden", "a;b@c", "-oProxyCommand=x"] {
            assert!(!looks_like_host(no), "{no}");
        }
    }
}
