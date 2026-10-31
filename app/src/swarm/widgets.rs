//! Small pieces shared by Swarm's popovers (the session picker, the connect
//! sheet).

use warp_core::ui::theme::Fill;
use warp_core::ui::theme::color::internal_colors;
use warpui::color::ColorU;
use warpui::elements::{Border, Container, CornerRadius, Element, Hoverable, MouseStateHandle, Radius, Text};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;

use crate::appearance::Appearance;

#[derive(Clone, Copy)]
pub enum ButtonStyle {
    /// Filled with the accent color: the sheet's main action.
    Primary,
    /// Outlined.
    Plain,
    /// Accent-colored text, no outline.
    Link,
}

/// A button that dispatches `action` when clicked; dimmed and inert when not
/// `enabled`.
pub fn button<A: warpui::Action + Clone>(
    appearance: &Appearance,
    mouse: MouseStateHandle,
    label: String,
    style: ButtonStyle,
    enabled: bool,
    action: A,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let font = appearance.ui_font_family();
    let accent = theme.accent();
    let main = theme.main_text_color(theme.background());
    let clear = Fill::Solid(ColorU::transparent_black());
    let hoverable = Hoverable::new(mouse, move |state| {
        let hovered = enabled && state.is_hovered();
        let hover_fill = if hovered { internal_colors::fg_overlay_2(theme) } else { clear };
        let (background, border, color) = match style {
            ButtonStyle::Primary if enabled => {
                (if hovered { accent.with_opacity(90) } else { accent }, clear, Fill::Solid(ColorU::white()))
            }
            ButtonStyle::Link => (hover_fill, clear, if enabled { accent } else { theme.disabled_ui_text_color() }),
            _ => (
                hover_fill,
                internal_colors::fg_overlay_3(theme),
                if enabled { main } else { theme.disabled_ui_text_color() },
            ),
        };
        Container::new(
            Text::new_inline(label.clone(), font, 12.)
                .with_color(color.into())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        )
        .with_background(background)
        .with_border(Border::all(1.).with_border_fill(border))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .with_padding_left(12.)
        .with_padding_right(12.)
        .with_padding_top(6.)
        .with_padding_bottom(6.)
        .finish()
    });
    if enabled {
        hoverable
            .with_cursor(Cursor::PointingHand)
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .finish()
    } else {
        hoverable.finish()
    }
}
