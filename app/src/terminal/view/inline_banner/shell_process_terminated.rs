use warpui::Element;
use warpui::elements::Text;

use warpui::elements::MouseStateHandle;

use super::{
    InlineBannerButtonState, InlineBannerContent, InlineBannerIcon, InlineBannerStyle,
    InlineBannerTextButton, InlineBannerTextButtonVariant, render_inline_block_list_banner,
};
use crate::appearance::Appearance;
use crate::terminal::view::TerminalAction;

/// swarm-term: a daemon session that ended with an error ("exit 1"), with
/// Restart (a new session in its place) and Close.
pub fn render_swarm_session_ended_banner(
    appearance: &Appearance,
    detail: &str,
    restart: MouseStateHandle,
    close: MouseStateHandle,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let text_color = theme.active_ui_text_color().into_solid();
    let button = |text: &str, on_click_event, mouse_state_handle, variant| InlineBannerTextButton {
        text: text.to_string(),
        text_color,
        button_state: InlineBannerButtonState { on_click_event, mouse_state_handle },
        font: Default::default(),
        position_id: None,
        variant,
    };
    render_inline_block_list_banner(
        InlineBannerStyle::LowPriority,
        appearance,
        InlineBannerContent {
            title: format!("Session ended ({detail})"),
            header_icon: Some(InlineBannerIcon {
                asset_path: "bundled/svg/warning.svg",
                aspect_ratio: 1.,
                color_override: Some(theme.ui_error_color()),
            }),
            content: Some(vec![Text::new(
                "Restart starts a new session in the same folder.",
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )]),
            buttons: vec![
                button(
                    "Restart",
                    TerminalAction::SwarmRestartSession,
                    restart,
                    InlineBannerTextButtonVariant::Primary,
                ),
                button(
                    "Close",
                    TerminalAction::SwarmCloseEndedSession,
                    close,
                    InlineBannerTextButtonVariant::Secondary,
                ),
            ],
            ..Default::default()
        },
    )
}

pub fn render_shell_process_terminated_banner(
    appearance: &Appearance,
    was_premature_termination: bool,
) -> Box<dyn Element> {
    if was_premature_termination {
        render_inline_block_list_banner(
            InlineBannerStyle::CallToAction,
            appearance,
            InlineBannerContent {
                title: "Shell process exited prematurely!".to_string(),
                header_icon: Some(InlineBannerIcon {
                    asset_path: "bundled/svg/warning.svg",
                    aspect_ratio: 1.,
                    color_override: Some(appearance.theme().foreground().into_solid()),
                }),
                content: Some(vec![Text::new(
                    "The output from Swarm's initialization script is visible above to assist with debugging.",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )]),
                ..Default::default()
            },
        )
    } else {
        render_inline_block_list_banner(
            InlineBannerStyle::LowPriority,
            appearance,
            InlineBannerContent {
                title: "Shell process exited".to_string(),
                header_icon: Some(InlineBannerIcon {
                    asset_path: "bundled/svg/info.svg",
                    aspect_ratio: 1.,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
    }
}
