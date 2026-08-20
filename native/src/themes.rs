//! Theme presets transliterated from `src/renderer/stores/theme-presets.ts`.
//!
//! Colors are stored as `u32` in `0xRRGGBB` form, matching gpui's `rgb(0x...)`
//! constructor.

/// A terminal color theme, mirroring the TypeScript `ThemeConfig` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    pub black: u32,
    pub red: u32,
    pub green: u32,
    pub yellow: u32,
    pub blue: u32,
    pub magenta: u32,
    pub cyan: u32,
    pub white: u32,
    pub bright_black: u32,
    pub bright_red: u32,
    pub bright_green: u32,
    pub bright_yellow: u32,
    pub bright_blue: u32,
    pub bright_magenta: u32,
    pub bright_cyan: u32,
    pub bright_white: u32,
    pub ui_background: u32,
    pub ui_surface: u32,
    pub ui_border: u32,
    pub ui_accent: u32,
    pub ui_text: u32,
    pub ui_text_muted: u32,
}

pub const TOKYO_NIGHT: Theme = Theme {
    name: "Tokyo Night",
    background: 0x1a1b26,
    foreground: 0xc0caf5,
    cursor: 0xc0caf5,
    selection: 0x33467c,
    black: 0x15161e,
    red: 0xf7768e,
    green: 0x9ece6a,
    yellow: 0xe0af68,
    blue: 0x7aa2f7,
    magenta: 0xbb9af7,
    cyan: 0x7dcfff,
    white: 0xa9b1d6,
    bright_black: 0x414868,
    bright_red: 0xf7768e,
    bright_green: 0x9ece6a,
    bright_yellow: 0xe0af68,
    bright_blue: 0x7aa2f7,
    bright_magenta: 0xbb9af7,
    bright_cyan: 0x7dcfff,
    bright_white: 0xc0caf5,
    ui_background: 0x1a1b26,
    ui_surface: 0x24283b,
    ui_border: 0x414868,
    ui_accent: 0x7aa2f7,
    ui_text: 0xc0caf5,
    ui_text_muted: 0x565f89,
};

pub const DRACULA: Theme = Theme {
    name: "Dracula",
    background: 0x282a36,
    foreground: 0xf8f8f2,
    cursor: 0xf8f8f2,
    selection: 0x44475a,
    black: 0x21222c,
    red: 0xff5555,
    green: 0x50fa7b,
    yellow: 0xf1fa8c,
    blue: 0xbd93f9,
    magenta: 0xff79c6,
    cyan: 0x8be9fd,
    white: 0xf8f8f2,
    bright_black: 0x6272a4,
    bright_red: 0xff6e6e,
    bright_green: 0x69ff94,
    bright_yellow: 0xffffa5,
    bright_blue: 0xd6acff,
    bright_magenta: 0xff92df,
    bright_cyan: 0xa4ffff,
    bright_white: 0xffffff,
    ui_background: 0x282a36,
    ui_surface: 0x343746,
    ui_border: 0x44475a,
    ui_accent: 0xbd93f9,
    ui_text: 0xf8f8f2,
    ui_text_muted: 0x6272a4,
};

pub const CATPPUCCIN_MOCHA: Theme = Theme {
    name: "Catppuccin Mocha",
    background: 0x1e1e2e,
    foreground: 0xcdd6f4,
    cursor: 0xf5e0dc,
    selection: 0x45475a,
    black: 0x45475a,
    red: 0xf38ba8,
    green: 0xa6e3a1,
    yellow: 0xf9e2af,
    blue: 0x89b4fa,
    magenta: 0xf5c2e7,
    cyan: 0x94e2d5,
    white: 0xbac2de,
    bright_black: 0x585b70,
    bright_red: 0xf38ba8,
    bright_green: 0xa6e3a1,
    bright_yellow: 0xf9e2af,
    bright_blue: 0x89b4fa,
    bright_magenta: 0xf5c2e7,
    bright_cyan: 0x94e2d5,
    bright_white: 0xa6adc8,
    ui_background: 0x1e1e2e,
    ui_surface: 0x313244,
    ui_border: 0x45475a,
    ui_accent: 0x89b4fa,
    ui_text: 0xcdd6f4,
    ui_text_muted: 0x6c7086,
};

pub const NORD: Theme = Theme {
    name: "Nord",
    background: 0x2e3440,
    foreground: 0xd8dee9,
    cursor: 0xd8dee9,
    selection: 0x434c5e,
    black: 0x3b4252,
    red: 0xbf616a,
    green: 0xa3be8c,
    yellow: 0xebcb8b,
    blue: 0x81a1c1,
    magenta: 0xb48ead,
    cyan: 0x88c0d0,
    white: 0xe5e9f0,
    bright_black: 0x4c566a,
    bright_red: 0xbf616a,
    bright_green: 0xa3be8c,
    bright_yellow: 0xebcb8b,
    bright_blue: 0x81a1c1,
    bright_magenta: 0xb48ead,
    bright_cyan: 0x8fbcbb,
    bright_white: 0xeceff4,
    ui_background: 0x2e3440,
    ui_surface: 0x3b4252,
    ui_border: 0x4c566a,
    ui_accent: 0x88c0d0,
    ui_text: 0xd8dee9,
    ui_text_muted: 0x4c566a,
};

pub const SOLARIZED_DARK: Theme = Theme {
    name: "Solarized Dark",
    background: 0x002b36,
    foreground: 0x839496,
    cursor: 0x839496,
    selection: 0x073642,
    black: 0x073642,
    red: 0xdc322f,
    green: 0x859900,
    yellow: 0xb58900,
    blue: 0x268bd2,
    magenta: 0xd33682,
    cyan: 0x2aa198,
    white: 0xeee8d5,
    bright_black: 0x586e75,
    bright_red: 0xcb4b16,
    bright_green: 0x586e75,
    bright_yellow: 0x657b83,
    bright_blue: 0x839496,
    bright_magenta: 0x6c71c4,
    bright_cyan: 0x93a1a1,
    bright_white: 0xfdf6e3,
    ui_background: 0x002b36,
    ui_surface: 0x073642,
    ui_border: 0x586e75,
    ui_accent: 0x268bd2,
    ui_text: 0x839496,
    ui_text_muted: 0x586e75,
};

pub const SOLARIZED_LIGHT: Theme = Theme {
    name: "Solarized Light",
    background: 0xfdf6e3,
    foreground: 0x657b83,
    cursor: 0x657b83,
    selection: 0xeee8d5,
    black: 0x073642,
    red: 0xdc322f,
    green: 0x859900,
    yellow: 0xb58900,
    blue: 0x268bd2,
    magenta: 0xd33682,
    cyan: 0x2aa198,
    white: 0xeee8d5,
    bright_black: 0x002b36,
    bright_red: 0xcb4b16,
    bright_green: 0x586e75,
    bright_yellow: 0x657b83,
    bright_blue: 0x839496,
    bright_magenta: 0x6c71c4,
    bright_cyan: 0x93a1a1,
    bright_white: 0xfdf6e3,
    ui_background: 0xfdf6e3,
    ui_surface: 0xeee8d5,
    ui_border: 0x93a1a1,
    ui_accent: 0x268bd2,
    ui_text: 0x657b83,
    ui_text_muted: 0x93a1a1,
};

pub const GRUVBOX_DARK: Theme = Theme {
    name: "Gruvbox Dark",
    background: 0x282828,
    foreground: 0xebdbb2,
    cursor: 0xebdbb2,
    selection: 0x504945,
    black: 0x282828,
    red: 0xcc241d,
    green: 0x98971a,
    yellow: 0xd79921,
    blue: 0x458588,
    magenta: 0xb16286,
    cyan: 0x689d6a,
    white: 0xa89984,
    bright_black: 0x928374,
    bright_red: 0xfb4934,
    bright_green: 0xb8bb26,
    bright_yellow: 0xfabd2f,
    bright_blue: 0x83a598,
    bright_magenta: 0xd3869b,
    bright_cyan: 0x8ec07c,
    bright_white: 0xebdbb2,
    ui_background: 0x282828,
    ui_surface: 0x3c3836,
    ui_border: 0x504945,
    ui_accent: 0xfabd2f,
    ui_text: 0xebdbb2,
    ui_text_muted: 0x928374,
};

pub const ONE_DARK: Theme = Theme {
    name: "One Dark",
    background: 0x282c34,
    foreground: 0xabb2bf,
    cursor: 0x528bff,
    selection: 0x3e4451,
    black: 0x282c34,
    red: 0xe06c75,
    green: 0x98c379,
    yellow: 0xe5c07b,
    blue: 0x61afef,
    magenta: 0xc678dd,
    cyan: 0x56b6c2,
    white: 0xabb2bf,
    bright_black: 0x5c6370,
    bright_red: 0xe06c75,
    bright_green: 0x98c379,
    bright_yellow: 0xe5c07b,
    bright_blue: 0x61afef,
    bright_magenta: 0xc678dd,
    bright_cyan: 0x56b6c2,
    bright_white: 0xffffff,
    ui_background: 0x282c34,
    ui_surface: 0x21252b,
    ui_border: 0x3e4451,
    ui_accent: 0x61afef,
    ui_text: 0xabb2bf,
    ui_text_muted: 0x5c6370,
};

pub const MONOKAI: Theme = Theme {
    name: "Monokai",
    background: 0x272822,
    foreground: 0xf8f8f2,
    cursor: 0xf8f8f0,
    selection: 0x49483e,
    black: 0x272822,
    red: 0xf92672,
    green: 0xa6e22e,
    yellow: 0xf4bf75,
    blue: 0x66d9ef,
    magenta: 0xae81ff,
    cyan: 0xa1efe4,
    white: 0xf8f8f2,
    bright_black: 0x75715e,
    bright_red: 0xf92672,
    bright_green: 0xa6e22e,
    bright_yellow: 0xf4bf75,
    bright_blue: 0x66d9ef,
    bright_magenta: 0xae81ff,
    bright_cyan: 0xa1efe4,
    bright_white: 0xf9f8f5,
    ui_background: 0x272822,
    ui_surface: 0x3e3d32,
    ui_border: 0x49483e,
    ui_accent: 0xa6e22e,
    ui_text: 0xf8f8f2,
    ui_text_muted: 0x75715e,
};

pub const MONOKAI_PRO_SPECTRUM: Theme = Theme {
    name: "Monokai Pro Spectrum",
    background: 0x222222,
    foreground: 0xf7f1ff,
    cursor: 0xf7f1ff,
    selection: 0x403e41,
    black: 0x363537,
    red: 0xfc618d,
    green: 0x7bd88f,
    yellow: 0xfce566,
    blue: 0x5ad4e6,
    magenta: 0x948ae3,
    cyan: 0x5ad4e6,
    white: 0xbab6c0,
    bright_black: 0x69676c,
    bright_red: 0xfc618d,
    bright_green: 0x7bd88f,
    bright_yellow: 0xfce566,
    bright_blue: 0x5ad4e6,
    bright_magenta: 0x948ae3,
    bright_cyan: 0x5ad4e6,
    bright_white: 0xf7f1ff,
    ui_background: 0x222222,
    ui_surface: 0x2d2a2e,
    ui_border: 0x403e41,
    ui_accent: 0x948ae3,
    ui_text: 0xf7f1ff,
    ui_text_muted: 0x8b888f,
};

pub const ROSE_PINE: Theme = Theme {
    name: "Rose Pine",
    background: 0x191724,
    foreground: 0xe0def4,
    cursor: 0x524f67,
    selection: 0x2a283e,
    black: 0x26233a,
    red: 0xeb6f92,
    green: 0x31748f,
    yellow: 0xf6c177,
    blue: 0x9ccfd8,
    magenta: 0xc4a7e7,
    cyan: 0xebbcba,
    white: 0xe0def4,
    bright_black: 0x6e6a86,
    bright_red: 0xeb6f92,
    bright_green: 0x31748f,
    bright_yellow: 0xf6c177,
    bright_blue: 0x9ccfd8,
    bright_magenta: 0xc4a7e7,
    bright_cyan: 0xebbcba,
    bright_white: 0xe0def4,
    ui_background: 0x191724,
    ui_surface: 0x1f1d2e,
    ui_border: 0x26233a,
    ui_accent: 0xc4a7e7,
    ui_text: 0xe0def4,
    ui_text_muted: 0x6e6a86,
};

pub const KANAGAWA: Theme = Theme {
    name: "Kanagawa",
    background: 0x1f1f28,
    foreground: 0xdcd7ba,
    cursor: 0xc8c093,
    selection: 0x2d4f67,
    black: 0x16161d,
    red: 0xc34043,
    green: 0x76946a,
    yellow: 0xc0a36e,
    blue: 0x7e9cd8,
    magenta: 0x957fb8,
    cyan: 0x6a9589,
    white: 0xc8c093,
    bright_black: 0x727169,
    bright_red: 0xe82424,
    bright_green: 0x98bb6c,
    bright_yellow: 0xe6c384,
    bright_blue: 0x7fb4ca,
    bright_magenta: 0x938aa9,
    bright_cyan: 0x7aa89f,
    bright_white: 0xdcd7ba,
    ui_background: 0x1f1f28,
    ui_surface: 0x2a2a37,
    ui_border: 0x54546d,
    ui_accent: 0x7e9cd8,
    ui_text: 0xdcd7ba,
    ui_text_muted: 0x727169,
};

pub const EVERFOREST: Theme = Theme {
    name: "Everforest",
    background: 0x2d353b,
    foreground: 0xd3c6aa,
    cursor: 0xd3c6aa,
    selection: 0x475258,
    black: 0x343f44,
    red: 0xe67e80,
    green: 0xa7c080,
    yellow: 0xdbbc7f,
    blue: 0x7fbbb3,
    magenta: 0xd699b6,
    cyan: 0x83c092,
    white: 0xd3c6aa,
    bright_black: 0x859289,
    bright_red: 0xe67e80,
    bright_green: 0xa7c080,
    bright_yellow: 0xdbbc7f,
    bright_blue: 0x7fbbb3,
    bright_magenta: 0xd699b6,
    bright_cyan: 0x83c092,
    bright_white: 0xd3c6aa,
    ui_background: 0x2d353b,
    ui_surface: 0x343f44,
    ui_border: 0x475258,
    ui_accent: 0xa7c080,
    ui_text: 0xd3c6aa,
    ui_text_muted: 0x859289,
};

/// All built-in presets, in the same order as `builtinThemes` in the TS file.
const PRESETS: [Theme; 13] = [
    TOKYO_NIGHT,
    DRACULA,
    CATPPUCCIN_MOCHA,
    NORD,
    SOLARIZED_DARK,
    SOLARIZED_LIGHT,
    GRUVBOX_DARK,
    ONE_DARK,
    MONOKAI,
    MONOKAI_PRO_SPECTRUM,
    ROSE_PINE,
    KANAGAWA,
    EVERFOREST,
];

/// Returns all built-in theme presets, in the same order as the TS file.
#[cfg_attr(not(test), allow(dead_code))]
pub fn presets() -> &'static [Theme] {
    &PRESETS
}

/// Custom themes imported at runtime (leaked: themes are tiny and few).
fn customs() -> &'static std::sync::Mutex<Vec<&'static Theme>> {
    static CUSTOM: std::sync::OnceLock<std::sync::Mutex<Vec<&'static Theme>>> =
        std::sync::OnceLock::new();
    CUSTOM.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Every selectable theme: built-ins plus imported customs.
pub fn all_themes() -> Vec<&'static Theme> {
    let mut all: Vec<&'static Theme> = PRESETS.iter().collect();
    all.extend(customs().lock().unwrap().iter().copied());
    all
}

/// Parse `#rrggbb` (or `rrggbb`) into 0xRRGGBB.
fn parse_hex(value: &serde_json::Value) -> Option<u32> {
    let s = value.as_str()?.trim_start_matches('#');
    (s.len() == 6)
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
}

/// Import a theme from the old app's export format (ThemeConfig JSON with
/// `#rrggbb` strings). Returns the registered static theme.
pub fn import_custom(json: &serde_json::Value) -> Result<&'static Theme, String> {
    let get = |key: &str| -> Result<u32, String> {
        json.get(key)
            .and_then(parse_hex)
            .ok_or_else(|| format!("invalid theme: missing or malformed \"{key}\""))
    };
    let name = json
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("invalid theme: missing \"name\"")?
        .to_string();
    if by_name(&name).is_some() {
        return Err(format!("a theme named \"{name}\" already exists"));
    }
    let theme = Theme {
        name: Box::leak(name.into_boxed_str()),
        background: get("background")?,
        foreground: get("foreground")?,
        cursor: get("cursor")?,
        selection: get("selection")?,
        black: get("black")?,
        red: get("red")?,
        green: get("green")?,
        yellow: get("yellow")?,
        blue: get("blue")?,
        magenta: get("magenta")?,
        cyan: get("cyan")?,
        white: get("white")?,
        bright_black: get("brightBlack")?,
        bright_red: get("brightRed")?,
        bright_green: get("brightGreen")?,
        bright_yellow: get("brightYellow")?,
        bright_blue: get("brightBlue")?,
        bright_magenta: get("brightMagenta")?,
        bright_cyan: get("brightCyan")?,
        bright_white: get("brightWhite")?,
        ui_background: get("uiBackground")?,
        ui_surface: get("uiSurface")?,
        ui_border: get("uiBorder")?,
        ui_accent: get("uiAccent")?,
        ui_text: get("uiText")?,
        ui_text_muted: get("uiTextMuted")?,
    };
    let leaked: &'static Theme = Box::leak(Box::new(theme));
    customs().lock().unwrap().push(leaked);
    Ok(leaked)
}

fn hex_string(color: u32) -> String {
    format!("#{color:06x}")
}

/// Export in the old app's format (round-trips through import_custom).
pub fn export_json(theme: &Theme) -> serde_json::Value {
    serde_json::json!({
        "name": theme.name,
        "background": hex_string(theme.background),
        "foreground": hex_string(theme.foreground),
        "cursor": hex_string(theme.cursor),
        "selection": hex_string(theme.selection),
        "black": hex_string(theme.black),
        "red": hex_string(theme.red),
        "green": hex_string(theme.green),
        "yellow": hex_string(theme.yellow),
        "blue": hex_string(theme.blue),
        "magenta": hex_string(theme.magenta),
        "cyan": hex_string(theme.cyan),
        "white": hex_string(theme.white),
        "brightBlack": hex_string(theme.bright_black),
        "brightRed": hex_string(theme.bright_red),
        "brightGreen": hex_string(theme.bright_green),
        "brightYellow": hex_string(theme.bright_yellow),
        "brightBlue": hex_string(theme.bright_blue),
        "brightMagenta": hex_string(theme.bright_magenta),
        "brightCyan": hex_string(theme.bright_cyan),
        "brightWhite": hex_string(theme.bright_white),
        "uiBackground": hex_string(theme.ui_background),
        "uiSurface": hex_string(theme.ui_surface),
        "uiBorder": hex_string(theme.ui_border),
        "uiAccent": hex_string(theme.ui_accent),
        "uiText": hex_string(theme.ui_text),
        "uiTextMuted": hex_string(theme.ui_text_muted),
    })
}

/// Looks up a preset by its display name (exact match).
pub fn by_name(name: &str) -> Option<&'static Theme> {
    PRESETS.iter().find(|t| t.name == name).or_else(|| {
        customs()
            .lock()
            .unwrap()
            .iter()
            .copied()
            .find(|t| t.name == name)
    })
}

/// The default theme (Tokyo Night, matching the TS default).
pub fn default_theme() -> &'static Theme {
    &PRESETS[0]
}

/// Resolves an 8-bit (256-color) palette index to a `0xRRGGBB` color.
///
/// - 0-15: the theme's ANSI colors.
/// - 16-231: the standard xterm 6x6x6 color cube
///   (levels 0, 95, 135, 175, 215, 255).
/// - 232-255: the standard xterm grayscale ramp (8 + 10 * n).
pub fn ansi_256(index: u8, theme: &Theme) -> u32 {
    match index {
        0 => theme.black,
        1 => theme.red,
        2 => theme.green,
        3 => theme.yellow,
        4 => theme.blue,
        5 => theme.magenta,
        6 => theme.cyan,
        7 => theme.white,
        8 => theme.bright_black,
        9 => theme.bright_red,
        10 => theme.bright_green,
        11 => theme.bright_yellow,
        12 => theme.bright_blue,
        13 => theme.bright_magenta,
        14 => theme.bright_cyan,
        15 => theme.bright_white,
        16..=231 => {
            let i = u32::from(index) - 16;
            let level = |c: u32| if c == 0 { 0 } else { 55 + 40 * c };
            let r = level(i / 36);
            let g = level((i / 6) % 6);
            let b = level(i % 6);
            (r << 16) | (g << 8) | b
        }
        232..=255 => {
            let gray = 8 + 10 * (u32::from(index) - 232);
            (gray << 16) | (gray << 8) | gray
        }
    }
}

/// Push `fg` a few shades away from `bg` when their perceived luminance is
/// too close to read — toward white on dark backgrounds, black on light.
/// Colors that already contrast pass through unchanged.
pub fn contrast_boost(fg: u32, bg: u32) -> u32 {
    fn luminance(color: u32) -> f32 {
        let r = ((color >> 16) & 0xff) as f32;
        let g = ((color >> 8) & 0xff) as f32;
        let b = (color & 0xff) as f32;
        0.299 * r + 0.587 * g + 0.114 * b
    }
    let diff = (luminance(fg) - luminance(bg)).abs();
    const MIN_DIFF: f32 = 80.0;
    if diff >= MIN_DIFF {
        return fg;
    }
    let target = if luminance(bg) < 128.0 { 255.0 } else { 0.0 };
    let t = ((MIN_DIFF - diff) / MIN_DIFF) * 0.7;
    let blend = |channel: u32| -> u32 {
        let v = channel as f32;
        (v + (target - v) * t).round().clamp(0.0, 255.0) as u32
    };
    (blend((fg >> 16) & 0xff) << 16) | (blend((fg >> 8) & 0xff) << 8) | blend(fg & 0xff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_boost_leaves_readable_pairs_alone() {
        assert_eq!(contrast_boost(0xffffff, 0x000000), 0xffffff);
        assert_eq!(contrast_boost(0x1a1b26, 0xc0caf5), 0x1a1b26);
    }

    #[test]
    fn contrast_boost_lightens_on_dark_and_darkens_on_light() {
        fn luminance(color: u32) -> f32 {
            let r = ((color >> 16) & 0xff) as f32;
            let g = ((color >> 8) & 0xff) as f32;
            let b = (color & 0xff) as f32;
            0.299 * r + 0.587 * g + 0.114 * b
        }
        // Muted gray on near-black must come out brighter.
        let boosted = contrast_boost(0x333333, 0x1a1a1a);
        assert!(luminance(boosted) > luminance(0x333333));
        // Pale gray on white must come out darker.
        let dimmed = contrast_boost(0xdddddd, 0xffffff);
        assert!(luminance(dimmed) < luminance(0xdddddd));
    }

    #[test]
    fn preset_count_matches_ts_file() {
        assert_eq!(presets().len(), 13);
    }

    #[test]
    fn spot_check_colors_against_ts_hex_values() {
        // theme-presets.ts: dracula.brightGreen = '#69ff94'
        assert_eq!(by_name("Dracula").unwrap().bright_green, 0x69ff94);
        // theme-presets.ts: kanagawa.brightRed = '#e82424'
        assert_eq!(by_name("Kanagawa").unwrap().bright_red, 0xe82424);
        // theme-presets.ts: solarizedLight.uiBorder = '#93a1a1'
        assert_eq!(by_name("Solarized Light").unwrap().ui_border, 0x93a1a1);
    }

    #[test]
    fn default_theme_is_tokyo_night() {
        assert_eq!(default_theme().name, "Tokyo Night");
        assert_eq!(default_theme().background, 0x1a1b26);
    }

    #[test]
    fn ansi_256_cube_corners_and_grayscale() {
        let theme = default_theme();
        // Cube corners.
        assert_eq!(ansi_256(16, theme), 0x000000);
        assert_eq!(ansi_256(231, theme), 0xffffff);
        // Grayscale ramp: 8 + 10 * (244 - 232) = 128 -> 0x808080.
        assert_eq!(ansi_256(244, theme), 0x808080);
    }

    #[test]
    fn custom_theme_import_export_round_trip() {
        let mut json = export_json(&DRACULA);
        json["name"] = serde_json::Value::String("Dracula Custom Test".to_string());
        let imported = import_custom(&json).expect("import");
        assert_eq!(imported.background, DRACULA.background);
        assert_eq!(imported.bright_green, DRACULA.bright_green);
        assert!(by_name("Dracula Custom Test").is_some());
        assert!(import_custom(&json).is_err(), "duplicate name rejected");
        let missing = serde_json::json!({"name": "Broken", "background": "#123456"});
        assert!(import_custom(&missing).is_err());
    }

    #[test]
    fn ansi_256_low_indices_use_theme_colors() {
        let theme = by_name("Nord").unwrap();
        assert_eq!(ansi_256(0, theme), theme.black);
        assert_eq!(ansi_256(9, theme), theme.bright_red);
        assert_eq!(ansi_256(15, theme), theme.bright_white);
    }
}
