//! Swarm follows two macOS display settings that Warp's themes don't: the
//! accent color (System Settings → Appearance) and Increase Contrast
//! (Accessibility → Display). Both are read whenever the theme is refreshed,
//! which happens when either of them changes.

use warp_core::ui::theme::{Fill, WarpTheme};

/// `theme` as Swarm draws it: with the system accent color, unless the user
/// chose Multicolor (each app's own color, so the theme's accent stays), and
/// with stronger text and outlines when Increase Contrast is on.
pub fn adjust_theme(theme: WarpTheme) -> WarpTheme {
    warp_core::ui::theme::set_increased_contrast(increase_contrast());
    match system_accent() {
        Some(accent) => theme.with_accent(Fill::Solid(accent)),
        None => theme,
    }
}

/// Increase Contrast is on (`SWARM_INCREASE_CONTRAST=1` forces it, to check
/// the look without changing the system setting).
fn increase_contrast() -> bool {
    std::env::var_os("SWARM_INCREASE_CONTRAST").is_some_and(|v| v == "1") || system_increase_contrast()
}

#[cfg(target_os = "macos")]
fn system_increase_contrast() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
}

#[cfg(not(target_os = "macos"))]
fn system_increase_contrast() -> bool {
    false
}

/// The accent color chosen in System Settings; `None` for Multicolor.
#[cfg(target_os = "macos")]
fn system_accent() -> Option<warpui::color::ColorU> {
    use objc2_app_kit::{NSColor, NSColorSpace};
    use objc2_foundation::{NSUserDefaults, ns_string};
    // Multicolor leaves the key unset; a chosen color stores its number there.
    NSUserDefaults::standardUserDefaults().objectForKey(ns_string!("AppleAccentColor"))?;
    let color = NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let channel = |value: f64| (value.clamp(0., 1.) * 255.).round() as u8;
    Some(warpui::color::ColorU::new(
        channel(color.redComponent()),
        channel(color.greenComponent()),
        channel(color.blueComponent()),
        255,
    ))
}

#[cfg(not(target_os = "macos"))]
fn system_accent() -> Option<warpui::color::ColorU> {
    None
}
