//! Tiny canvas-painted UI icons: drawn with the same native quad/path API
//! as the git graph. No icon fonts, no emoji; always theme-colored.

use gpui::prelude::*;
use gpui::px;

#[derive(Debug, Clone, Copy)]
pub enum Icon {
    /// Stacked rows — the projects/tabs list.
    Projects,
    /// Trunk with a branch and three nodes.
    GitBranch,
    /// Folder with a tab.
    Files,
    /// Cup with handle and steam; filled body = keep-awake is holding.
    Coffee { filled: bool },
    /// Handset outline with a home strip; filled body = companion serving.
    Phone { filled: bool },
    /// Three linked nodes (share-to icon); filled nodes = shared with at
    /// least one peer right now, hollow = not shared.
    Share { active: bool },
    /// Two overlapping screens — another Mac's terminals, seen from here.
    Peers,
    /// Pushpin — a project kept out of the recents cap; filled head = it
    /// is pinned right now, hollow = it is not.
    Pin { filled: bool },
}

/// Icons that ship as real SVG assets rather than being drawn by hand.
///
/// Everything in this file started as quads and strokes, which is fine for
/// a coffee cup and wrong for anything with a recognised silhouette: the
/// hand-drawn pin read as a nail, because "a rectangle on a stalk" is what
/// you get when you approximate a shape instead of using it. These come
/// from Lucide (see `assets/LICENSE-lucide.txt`).
///
/// Embedded with `include_bytes!` rather than shipped as loose files, so
/// there is nothing for `bundle.sh` to copy and nothing to go missing from
/// an installed `.app`.
/// Every asset the UI can ask for. One list, so the `load` table, `list`
/// and the tests cannot drift apart — a path in the code and not here
/// paints nothing, silently.
pub const ASSET_PATHS: &[&str] = &[
    "icons/pin.svg",
    "icons/pin-filled.svg",
    "icons/projects.svg",
    "icons/git-branch.svg",
    "icons/files.svg",
    "icons/coffee.svg",
    "icons/coffee-filled.svg",
    "icons/phone.svg",
    "icons/phone-filled.svg",
    "icons/share.svg",
    "icons/share-active.svg",
    "icons/peers.svg",
];

pub struct Assets;

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        let bytes: &'static [u8] = match path {
            "icons/pin.svg" => include_bytes!("../assets/icons/pin.svg"),
            "icons/pin-filled.svg" => include_bytes!("../assets/icons/pin-filled.svg"),
            "icons/projects.svg" => include_bytes!("../assets/icons/projects.svg"),
            "icons/git-branch.svg" => include_bytes!("../assets/icons/git-branch.svg"),
            "icons/files.svg" => include_bytes!("../assets/icons/files.svg"),
            "icons/coffee.svg" => include_bytes!("../assets/icons/coffee.svg"),
            "icons/coffee-filled.svg" => include_bytes!("../assets/icons/coffee-filled.svg"),
            "icons/phone.svg" => include_bytes!("../assets/icons/phone.svg"),
            "icons/phone-filled.svg" => include_bytes!("../assets/icons/phone-filled.svg"),
            "icons/share.svg" => include_bytes!("../assets/icons/share.svg"),
            "icons/share-active.svg" => include_bytes!("../assets/icons/share-active.svg"),
            "icons/peers.svg" => include_bytes!("../assets/icons/peers.svg"),
            _ => return Ok(None),
        };
        Ok(Some(std::borrow::Cow::Borrowed(bytes)))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        Ok(ASSET_PATHS.iter().map(|p| (*p).into()).collect())
    }
}

/// The asset each icon draws from. A `filled`/`active` variant is its own
/// file rather than a fill applied at paint time, because `svg()` renders
/// the asset as a MASK — the file decides what is solid, the caller only
/// decides the colour.
fn asset_for(kind: Icon) -> &'static str {
    match kind {
        Icon::Projects => "icons/projects.svg",
        Icon::GitBranch => "icons/git-branch.svg",
        Icon::Files => "icons/files.svg",
        Icon::Coffee { filled: true } => "icons/coffee-filled.svg",
        Icon::Coffee { filled: false } => "icons/coffee.svg",
        Icon::Phone { filled: true } => "icons/phone-filled.svg",
        Icon::Phone { filled: false } => "icons/phone.svg",
        Icon::Share { active: true } => "icons/share-active.svg",
        Icon::Share { active: false } => "icons/share.svg",
        // Two overlapping panels, which is what the hand-drawn one drew
        // and what this needs to say: another Mac's terminals, seen from
        // here. Outline only — unlike Coffee/Phone/Share it carries no
        // on/off state, so a filled variant would imply one.
        Icon::Peers => "icons/peers.svg",
        Icon::Pin { filled: true } => "icons/pin-filled.svg",
        Icon::Pin { filled: false } => "icons/pin.svg",
    }
}

pub fn icon(kind: Icon, color: u32) -> gpui::AnyElement {
    gpui::svg()
        .path(asset_for(kind))
        .w(px(16.0))
        .h(px(16.0))
        // `svg()` paints the asset as a mask in the TEXT colour, so the
        // file's own stroke and fill decide coverage and this decides the
        // colour.
        .text_color(gpui::rgb(color))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource;

    /// Every `Icon` there is, so the asset check below covers the whole
    /// enum rather than whichever variants someone remembered.
    const EVERY_ICON: &[Icon] = &[
        Icon::Projects,
        Icon::GitBranch,
        Icon::Files,
        Icon::Coffee { filled: true },
        Icon::Coffee { filled: false },
        Icon::Phone { filled: true },
        Icon::Phone { filled: false },
        Icon::Share { active: true },
        Icon::Share { active: false },
        Icon::Peers,
        Icon::Pin { filled: true },
        Icon::Pin { filled: false },
    ];

    #[test]
    fn every_icon_resolves_to_an_asset_that_exists() {
        // The failure this catches is silent: `svg()` given a path that
        // resolves to nothing paints NOTHING, so a typo or a renamed file
        // is an invisible control rather than a crash. Driven off the enum
        // so a new variant with no asset fails here.
        for kind in EVERY_ICON {
            let path = asset_for(*kind);
            let bytes = Assets
                .load(path)
                .expect("loading must not error")
                .unwrap_or_else(|| panic!("{kind:?} points at {path}, which is not embedded"));
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("<svg"), "{path} is not an svg");
        }
    }

    #[test]
    fn a_state_carrying_icon_actually_looks_different_in_each_state() {
        // Filled and hollow have to differ, or the control silently stops
        // reporting anything it is there to report.
        for (on, off) in [
            (
                Icon::Coffee { filled: true },
                Icon::Coffee { filled: false },
            ),
            (Icon::Phone { filled: true }, Icon::Phone { filled: false }),
            (Icon::Share { active: true }, Icon::Share { active: false }),
            (Icon::Pin { filled: true }, Icon::Pin { filled: false }),
        ] {
            let a = Assets.load(asset_for(on)).unwrap().unwrap();
            let b = Assets.load(asset_for(off)).unwrap().unwrap();
            assert_ne!(a, b, "{on:?} and {off:?} draw the same thing");
        }
    }

    #[test]
    fn the_asset_list_and_the_load_table_agree() {
        // `list` and `load` are two places one set of paths is written;
        // a path in one and not the other is a resource that either
        // cannot be enumerated or cannot be read.
        for path in ASSET_PATHS {
            assert!(
                Assets.load(path).unwrap().is_some(),
                "{path} is listed but not embedded"
            );
        }
        for kind in EVERY_ICON {
            assert!(
                ASSET_PATHS.contains(&asset_for(*kind)),
                "{kind:?} uses an asset missing from ASSET_PATHS"
            );
        }
    }

    #[test]
    fn legacy_pin_assets_still_load() {
        for path in ["icons/pin.svg", "icons/pin-filled.svg"] {
            let bytes = Assets
                .load(path)
                .expect("loading must not error")
                .unwrap_or_else(|| panic!("{path} is missing from the binary"));
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("<svg"), "{path} is not an svg: {text:?}");
            assert!(text.contains("<path"), "{path} has no path to draw");
        }
    }

    #[test]
    fn an_unknown_asset_is_absent_rather_than_an_error() {
        // `AssetSource` distinguishes "no such asset" from "loading
        // failed", and conflating them would turn a missing icon into a
        // startup error.
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    }
}
