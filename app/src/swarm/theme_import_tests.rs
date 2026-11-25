use super::*;

const ITERM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Ansi 1 Color</key>
  <dict>
    <key>Blue Component</key><real>0.0</real>
    <key>Green Component</key><real>0.0</real>
    <key>Red Component</key><real>1</real>
  </dict>
  <key>Background Color</key>
  <dict>
    <key>Blue Component</key><real>0.1</real>
    <key>Color Space</key><string>sRGB</string>
    <key>Green Component</key><real>0.1</real>
    <key>Red Component</key><real>0.1</real>
  </dict>
  <key>Foreground Color</key>
  <dict>
    <key>Blue Component</key><real>0.9</real>
    <key>Green Component</key><real>0.9</real>
    <key>Red Component</key><real>0.9</real>
  </dict>
</dict>
</plist>"#;

const GHOSTTY: &str = "# A theme\npalette = 0=#1d1f21\npalette = 4=81a2be\nbackground = #fafafa\nforeground = 383a42\ncursor-color = #526eff\nfont-size = 12\n";

#[test]
fn iterm_preset_colors() {
    let colors = parse(ITERM.as_bytes()).unwrap();
    assert_eq!(colors.background, Some([26, 26, 26]));
    assert_eq!(colors.foreground, Some([230, 230, 230]));
    assert_eq!(colors.ansi[1], Some([255, 0, 0]));
    assert_eq!(colors.ansi[0], None);
    assert_eq!(colors.cursor, None);
}

#[test]
fn ghostty_theme_colors() {
    let colors = parse(GHOSTTY.as_bytes()).unwrap();
    assert_eq!(colors.background, Some([0xfa, 0xfa, 0xfa]));
    assert_eq!(colors.foreground, Some([0x38, 0x3a, 0x42]));
    assert_eq!(colors.cursor, Some([0x52, 0x6e, 0xff]));
    assert_eq!(colors.ansi[0], Some([0x1d, 0x1f, 0x21]));
    assert_eq!(colors.ansi[4], Some([0x81, 0xa2, 0xbe]));
}

#[test]
fn files_without_colors_are_refused() {
    assert!(parse(b"font-family = Menlo\n").is_err());
}

#[test]
fn yaml_has_every_color_and_reads_as_a_theme() {
    let yaml = to_yaml("Light One", &parse(GHOSTTY.as_bytes()).unwrap());
    assert!(yaml.starts_with("name: Light One\naccent: '#526eff'\nbackground: '#fafafa'\nforeground: '#383a42'\ndetails: lighter\ncursor: '#526eff'\n"));
    assert!(yaml.contains("  normal:\n    black: '#1d1f21'\n"));
    assert!(yaml.contains("    blue: '#81a2be'\n"));
    assert!(yaml.contains("  bright:\n    black: '#7f7f7f'\n"), "unset colors are xterm's");
    let theme: warp_core::ui::theme::WarpTheme = serde_yaml::from_str(&yaml).unwrap();
    let dark = to_yaml("Dark", &parse(ITERM.as_bytes()).unwrap());
    assert!(dark.contains("details: darker\n"));
    let _: warp_core::ui::theme::WarpTheme = serde_yaml::from_str(&dark).unwrap();
    let _ = theme;
}
