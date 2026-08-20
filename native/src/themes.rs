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
pub fn presets() -> &'static [Theme] {
    &PRESETS
}

/// Looks up a preset by its display name (exact match).
pub fn by_name(name: &str) -> Option<&'static Theme> {
    PRESETS.iter().find(|t| t.name == name)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ansi_256_low_indices_use_theme_colors() {
        let theme = by_name("Nord").unwrap();
        assert_eq!(ansi_256(0, theme), theme.black);
        assert_eq!(ansi_256(9, theme), theme.bright_red);
        assert_eq!(ansi_256(15, theme), theme.bright_white);
    }
}
