//! swarm-term: daemon sessions in the navigation palette (⇧⌘P), next to the
//! open tabs: detached sessions on this Mac and connected hosts to reattach,
//! and "New session on …" for each host. Type to filter; Return opens.

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
            // A host that isn't in ~/.ssh/config: `user@host` or a host name.
            if looks_like_host(term) {
                items.push((
                    format!("Connect to {term}"),
                    "over ssh".to_string(),
                    WorkspaceAction::SwarmConnectHost(term.to_string()),
                ));
            }
            for host in &hosts {
                let place = host.clone().unwrap_or_else(|| "this Mac".to_string());
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
                // Running sessions to reattach come before starting new ones.
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
    use super::looks_like_host;

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
