use warp_core::settings::Setting;
use warpui::elements::{
    Border, Clipped, Container, CrossAxisAlignment, DropTarget, Element, Flex,
    Hoverable, ParentElement, SavePosition, Shrinkable, Stack, Text,
};
use warpui::presenter::ChildView;
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, SingletonEntity};

use super::common::{
    add_command_xray_overlay, add_input_suggestions_overlays, add_voltron_overlay,
    add_workflow_info_overlay, should_show_terminal_input_message_bar,
    wrap_input_with_terminal_padding_and_focus_handler,
};
use super::{Input, InputAction, InputDropTargetData};
use crate::appearance::Appearance;
use crate::context_chips::{ContextChipKind, spacing};
use crate::features::FeatureFlag;
use crate::settings::{AppEditorSettings, InputModeSettings};
use crate::terminal::block_list_settings::BlockListSettings;
use crate::terminal::block_list_viewport::InputMode;
use crate::terminal::settings::TerminalSettings;
use crate::terminal::view::TerminalAction;

impl Input {
    /// Swarm's context line over the input: the folder, and the git branch
    /// when there is one ("~/src/api · git:main"). `None` until the folder is known.
    fn swarm_context_line(&self, app: &AppContext) -> Option<String> {
        let prompt = self.prompt_type.as_ref(app);
        let chip = |kind: &ContextChipKind| {
            prompt
                .latest_chip_value(kind, app)
                .map(|value| value.to_string())
                .filter(|value| !value.trim().is_empty())
        };
        let folder = chip(&ContextChipKind::WorkingDirectory)?;
        Some(match chip(&ContextChipKind::ShellGitBranch) {
            Some(branch) => format!("{folder}  ·  git:{branch}"),
            None => folder,
        })
    }

    /// Renders the terminal mode input when `FeatureFlag::AgentView` is enabled and there is no
    /// active agent view.
    pub(super) fn render_terminal_input(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let menu_positioning = self.menu_positioning(app);

        let model = self.model.lock();

        // We should likely rework this stack to not need to use `with_constrain_absolute_children`,
        // by reworking the positioning of the children to not depend on this.
        let mut stack = Stack::new().with_constrain_absolute_children();

        let vim_state = self.editor.as_ref(app).vim_state(app);
        let app_editor_settings = AppEditorSettings::as_ref(app);
        let show_vim_status = vim_state.is_some() && *app_editor_settings.vim_status_bar.value();
        let input_mode = *InputModeSettings::as_ref(app).input_mode.value();

        let mut column = Flex::column();

        if matches!(input_mode, InputMode::PinnedToBottom | InputMode::Waterfall)
            && let Some(banner) = self.render_input_banner(appearance, app, input_mode, false)
        {
            column.add_child(
                Container::new(banner)
                    .with_margin_top(spacing::UDI_CHIP_MARGIN)
                    .finish(),
            );
        }

        // Swarm: a plain context line (folder · git branch) over a `❯` prompt,
        // as in the main window mockup, instead of Warp's context chips.
        let swarm_context = (crate::swarm::SWARM_CHROME && model.block_list().is_bootstrapped())
            .then(|| self.swarm_context_line(app))
            .flatten();
        let prompt_elements = match &swarm_context {
            Some(context) => Container::new(
                Text::new_inline(context.clone(), appearance.ui_font_family(), 11.)
                    .with_color(crate::swarm::look::quiet_accent(appearance.theme()))
                    .with_clip(ClipConfig::start())
                    .finish(),
            )
            .with_padding_top(10.)
            .finish(),
            None => self
                .prompt_render_helper
                .render_universal_developer_input_prompt(&model, appearance, app),
        };

        column.add_child(prompt_elements);

        let terminal_spacing = TerminalSettings::as_ref(app)
            .terminal_input_spacing(appearance.line_height_ratio(), app);
        let input_box = self.render_input_box(show_vim_status, appearance, app);
        let input_box = if swarm_context.is_some() {
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(
                    Container::new(
                        Text::new_inline(
                            "❯".to_string(),
                            appearance.monospace_font_family(),
                            appearance.monospace_font_size(),
                        )
                        .with_color(appearance.theme().accent().into())
                        .finish(),
                    )
                    .with_margin_right(8.)
                    .finish(),
                )
                .with_child(Shrinkable::new(1., input_box).finish())
                .finish()
        } else {
            input_box
        };
        column.add_child(
            Container::new(input_box)
                .with_margin_top(
                    terminal_spacing.prompt_to_editor_padding
                        * spacing::UDI_PROMPT_BOTTOM_PADDING_FACTOR,
                )
                .finish(),
        );

        if should_show_terminal_input_message_bar(&model, app) {
            column.add_child(
                Clipped::new(ChildView::new(&self.terminal_input_message_bar).finish()).finish(),
            );
        } else if !(matches!(input_mode, InputMode::PinnedToTop)
            && self
                .suggestions_mode_model
                .as_ref(app)
                .is_inline_menu_open())
        {
            column.add_child(
                Container::new(Flex::row().finish())
                    .with_margin_bottom(8.)
                    .finish(),
            );
        }

        if matches!(input_mode, InputMode::PinnedToTop)
            && let Some(banner) = self.render_input_banner(appearance, app, input_mode, false)
        {
            column.add_child(
                Container::new(banner)
                    .with_margin_bottom(spacing::UDI_CHIP_MARGIN)
                    .finish(),
            );
        }

        stack.add_child(wrap_input_with_terminal_padding_and_focus_handler(
            self.focus_handle
                .as_ref()
                .is_some_and(|h| h.is_active_session(app)),
            column.finish(),
            false,
        ));

        if let Some(selected_workflow_state) = self.workflows_state.selected_workflow_state.as_ref()
            && selected_workflow_state.should_show_more_info_view
        {
            add_workflow_info_overlay(
                &mut stack,
                selected_workflow_state,
                self.size_info(app).pane_height_px().as_f32(),
                menu_positioning,
            );
        }

        let is_focused = self.focus_handle.as_ref().is_none_or(|h| h.is_focused(app));
        if self.is_voltron_open && is_focused {
            add_voltron_overlay(&mut stack, &self.voltron_view, menu_positioning);
        }

        if is_focused {
            add_input_suggestions_overlays(self, &mut stack, appearance, menu_positioning, app);
        }

        if let Some(token_description) = &self.command_x_ray_description {
            add_command_xray_overlay(
                self,
                &mut stack,
                token_description,
                appearance,
                menu_positioning,
                app,
            );
        }

        let drop_target = DropTarget::new(
            SavePosition::new(stack.finish(), &self.status_free_input_save_position_id()).finish(),
            InputDropTargetData::new(self.weak_view_handle.clone()),
        )
        .finish();

        let hoverable_input = Hoverable::new(self.hoverable_handle.clone(), |_| drop_target)
            .on_hover(|is_hovered, ctx, _app, _position| {
                ctx.dispatch_typed_action(InputAction::SetUDIHovered(is_hovered));
            })
            .on_middle_click(|ctx, _app, _position| {
                ctx.dispatch_typed_action(TerminalAction::MiddleClickOnInput)
            })
            .finish();

        let show_block_dividers = *BlockListSettings::as_ref(app).show_block_dividers.value();

        let input = if show_block_dividers {
            Container::new(hoverable_input)
                .with_border(
                    Border::top(1.)
                        .with_border_color(styles::default_border_color(appearance.theme())),
                )
                .finish()
        } else {
            hoverable_input
        };

        let mut column = Flex::column();
        let is_slash_commands = self.suggestions_mode_model.as_ref(app).is_slash_commands();
        let is_conversation_menu = self
            .suggestions_mode_model
            .as_ref(app)
            .is_conversation_menu();
        let is_prompts_menu = self.suggestions_mode_model.as_ref(app).is_prompts_menu();
        let is_skill_menu = self.suggestions_mode_model.as_ref(app).is_skill_menu();
        let is_inline_history_menu = self
            .suggestions_mode_model
            .as_ref(app)
            .is_inline_history_menu();
        let is_repos_menu = self.suggestions_mode_model.as_ref(app).is_repos_menu();
        let hide_menu = self
            .inline_terminal_menu_positioner
            .as_ref(app)
            .should_hide_inline_menu_for_pane_size(app);
        match input_mode {
            InputMode::PinnedToBottom => {
                column.add_children(
                    [
                        if hide_menu {
                            None
                        } else if is_slash_commands {
                            Some(ChildView::new(&self.inline_slash_commands_view).finish())
                        } else if is_prompts_menu {
                            Some(ChildView::new(&self.inline_prompts_menu_view).finish())
                        } else if is_conversation_menu {
                            Some(ChildView::new(&self.inline_conversation_menu_view).finish())
                        } else if FeatureFlag::ListSkills.is_enabled() && is_skill_menu {
                            Some(ChildView::new(&self.inline_skill_selector_view).finish())
                        } else if is_inline_history_menu {
                            Some(ChildView::new(&self.inline_history_menu_view).finish())
                        } else if is_repos_menu {
                            Some(ChildView::new(&self.inline_repos_menu_view).finish())
                        } else {
                            None
                        },
                        Some(ChildView::new(&self.agent_status_view).finish()),
                        Some(input),
                    ]
                    .into_iter()
                    .flatten(),
                );
            }
            InputMode::PinnedToTop => {
                column.add_children(
                    [
                        Some(input),
                        Some(ChildView::new(&self.agent_status_view).finish()),
                        if hide_menu {
                            None
                        } else if is_slash_commands {
                            Some(ChildView::new(&self.inline_slash_commands_view).finish())
                        } else if is_prompts_menu {
                            Some(ChildView::new(&self.inline_prompts_menu_view).finish())
                        } else if is_conversation_menu {
                            Some(ChildView::new(&self.inline_conversation_menu_view).finish())
                        } else if FeatureFlag::ListSkills.is_enabled() && is_skill_menu {
                            Some(ChildView::new(&self.inline_skill_selector_view).finish())
                        } else if is_inline_history_menu {
                            Some(ChildView::new(&self.inline_history_menu_view).finish())
                        } else if is_repos_menu {
                            Some(ChildView::new(&self.inline_repos_menu_view).finish())
                        } else {
                            None
                        },
                    ]
                    .into_iter()
                    .flatten(),
                );
            }
            InputMode::Waterfall => {
                let should_render_below = self
                    .inline_terminal_menu_positioner
                    .as_ref(app)
                    .should_render_inline_menu_below_input();

                if !hide_menu {
                    if is_slash_commands && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_slash_commands_view).finish());
                    } else if is_prompts_menu && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_prompts_menu_view).finish());
                    } else if is_conversation_menu && !should_render_below {
                        column.add_child(
                            ChildView::new(&self.inline_conversation_menu_view).finish(),
                        );
                    } else if FeatureFlag::ListSkills.is_enabled()
                        && is_skill_menu
                        && !should_render_below
                    {
                        column.add_child(ChildView::new(&self.inline_skill_selector_view).finish());
                    } else if is_inline_history_menu && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_history_menu_view).finish());
                    } else if is_repos_menu && !should_render_below {
                        column.add_child(ChildView::new(&self.inline_repos_menu_view).finish());
                    }
                }

                column.add_children([ChildView::new(&self.agent_status_view).finish(), input]);

                if !hide_menu {
                    if is_slash_commands && should_render_below {
                        column.add_child(ChildView::new(&self.inline_slash_commands_view).finish());
                    } else if is_prompts_menu && should_render_below {
                        column.add_child(ChildView::new(&self.inline_prompts_menu_view).finish());
                    } else if is_conversation_menu && should_render_below {
                        column.add_child(
                            ChildView::new(&self.inline_conversation_menu_view).finish(),
                        );
                    } else if FeatureFlag::ListSkills.is_enabled()
                        && is_skill_menu
                        && should_render_below
                    {
                        column.add_child(ChildView::new(&self.inline_skill_selector_view).finish());
                    } else if is_inline_history_menu && should_render_below {
                        column.add_child(ChildView::new(&self.inline_history_menu_view).finish());
                    } else if is_repos_menu && should_render_below {
                        column.add_child(ChildView::new(&self.inline_repos_menu_view).finish());
                    }
                }
            }
        }

        SavePosition::new(column.finish(), &self.save_position_id()).finish()
    }
}

pub mod styles {
    use pathfinder_color::ColorU;
    use warp_core::ui::theme::WarpTheme;

    pub fn default_border_color(theme: &WarpTheme) -> ColorU {
        theme.outline().into()
    }
}
