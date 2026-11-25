//! View → Import Theme…: turns an iTerm2 color preset (`.itermcolors`) or a Ghostty theme file
//! into a theme of Swarm's own (YAML in the themes folder).

use std::path::Path;

use anyhow::{Context, Result, bail};

type Rgb = [u8; 3];

/// The colors a theme file sets; what it leaves out comes from xterm's defaults.
#[derive(Debug, Default, PartialEq)]
pub struct Colors {
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub ansi: [Option<Rgb>; 16],
}

/// xterm's 16 colors, for any a file doesn't set.
const XTERM: [Rgb; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

const NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

/// Reads the theme file at `path`: its name (the file's) and the theme as YAML.
pub fn import(path: &Path) -> Result<(String, String)> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let colors = parse(&bytes)?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported".to_string());
    Ok((name.clone(), to_yaml(&name, &colors)))
}

/// An iTerm2 preset (a property list) or a Ghostty theme (`key = value` lines).
pub fn parse(bytes: &[u8]) -> Result<Colors> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let colors = if head.contains("<plist") || head.contains("<?xml") || bytes.starts_with(b"bplist") {
        from_iterm(bytes)?
    } else {
        from_ghostty(&String::from_utf8_lossy(bytes))
    };
    if colors.background.is_none() && colors.foreground.is_none() && colors.ansi.iter().all(Option::is_none) {
        bail!("this isn't an iTerm2 color preset or a Ghostty theme: it sets no colors");
    }
    Ok(colors)
}

fn from_iterm(bytes: &[u8]) -> Result<Colors> {
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes)).context("read the .itermcolors file")?;
    let dict = value.as_dictionary().context("the .itermcolors file isn't a dictionary")?;
    let color = |key: &str| -> Option<Rgb> {
        let c = dict.get(key)?.as_dictionary()?;
        let component = |name: &str| -> Option<u8> {
            let v = c.get(name)?;
            let f = v.as_real().or_else(|| v.as_signed_integer().map(|i| i as f64))?;
            Some((f.clamp(0.0, 1.0) * 255.0).round() as u8)
        };
        Some([component("Red Component")?, component("Green Component")?, component("Blue Component")?])
    };
    let mut colors = Colors {
        background: color("Background Color"),
        foreground: color("Foreground Color"),
        cursor: color("Cursor Color"),
        ..Default::default()
    };
    for (i, slot) in colors.ansi.iter_mut().enumerate() {
        *slot = color(&format!("Ansi {i} Color"));
    }
    Ok(colors)
}

fn from_ghostty(text: &str) -> Colors {
    let mut colors = Colors::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "background" => colors.background = hex(value),
            "foreground" => colors.foreground = hex(value),
            "cursor-color" => colors.cursor = hex(value),
            "palette" => {
                if let Some((index, color)) = value.split_once('=')
                    && let Ok(index) = index.trim().parse::<usize>()
                    && index < 16
                {
                    colors.ansi[index] = hex(color.trim());
                }
            }
            _ => {}
        }
    }
    colors
}

fn hex(value: &str) -> Option<Rgb> {
    let digits = value.trim_start_matches('#');
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

fn hex_string(c: Rgb) -> String {
    format!("'#{:02x}{:02x}{:02x}'", c[0], c[1], c[2])
}

fn is_dark(c: Rgb) -> bool {
    let [r, g, b] = c.map(f64::from);
    0.2126 * r + 0.7152 * g + 0.0722 * b < 128.0
}

/// The theme in Swarm's YAML format.
pub fn to_yaml(name: &str, colors: &Colors) -> String {
    let ansi: Vec<Rgb> = colors.ansi.iter().zip(XTERM).map(|(c, default)| c.unwrap_or(default)).collect();
    let background = colors.background.unwrap_or(ansi[0]);
    let foreground = colors.foreground.unwrap_or(ansi[7]);
    let mut yaml = format!(
        "name: {}\naccent: {}\nbackground: {}\nforeground: {}\ndetails: {}\n",
        serde_yaml::to_string(name).unwrap_or_default().trim_start_matches("---").trim(),
        hex_string(colors.cursor.unwrap_or(ansi[4])),
        hex_string(background),
        hex_string(foreground),
        if is_dark(background) { "darker" } else { "lighter" },
    );
    if let Some(cursor) = colors.cursor {
        yaml.push_str(&format!("cursor: {}\n", hex_string(cursor)));
    }
    yaml.push_str("terminal_colors:\n");
    for (group, offset) in [("normal", 0), ("bright", 8)] {
        yaml.push_str(&format!("  {group}:\n"));
        for (i, color_name) in NAMES.iter().enumerate() {
            yaml.push_str(&format!("    {color_name}: {}\n", hex_string(ansi[offset + i])));
        }
    }
    yaml
}

#[cfg(test)]
#[path = "theme_import_tests.rs"]
mod tests;
