//! Tiny canvas-painted UI icons: drawn with the same native quad/path API
//! as the git graph. No icon fonts, no emoji; always theme-colored.

use gpui::prelude::*;
use gpui::px;

#[derive(Clone, Copy)]
pub enum Icon {
    /// Stacked rows — the projects/tabs list.
    Projects,
    /// Trunk with a branch and three nodes.
    GitBranch,
    /// Folder with a tab.
    Files,
}

pub fn icon(kind: Icon, color: u32) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let x = f32::from(bounds.origin.x);
            let y = f32::from(bounds.origin.y);
            let color = gpui::rgb(color);
            let quad = |window: &mut gpui::Window, qx: f32, qy: f32, w: f32, h: f32| {
                window.paint_quad(gpui::fill(
                    gpui::Bounds {
                        origin: gpui::point(px(x + qx), px(y + qy)),
                        size: gpui::size(px(w), px(h)),
                    },
                    color,
                ));
            };
            let line = |window: &mut gpui::Window, x0: f32, y0: f32, x1: f32, y1: f32| {
                let mut builder = gpui::PathBuilder::stroke(px(1.5));
                builder.move_to(gpui::point(px(x + x0), px(y + y0)));
                builder.line_to(gpui::point(px(x + x1), px(y + y1)));
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            };
            match kind {
                Icon::Projects => {
                    quad(window, 2.0, 3.0, 12.0, 2.5);
                    quad(window, 2.0, 6.75, 12.0, 2.5);
                    quad(window, 2.0, 10.5, 12.0, 2.5);
                }
                Icon::GitBranch => {
                    line(window, 5.0, 4.5, 5.0, 12.0);
                    line(window, 5.0, 9.5, 10.5, 6.0);
                    quad(window, 3.5, 2.0, 3.0, 3.0);
                    quad(window, 3.5, 11.5, 3.0, 3.0);
                    quad(window, 9.5, 3.5, 3.0, 3.0);
                }
                Icon::Files => {
                    quad(window, 2.0, 4.0, 6.0, 2.0);
                    quad(window, 2.0, 6.0, 12.0, 7.0);
                }
            }
        },
    )
    .w(px(16.0))
    .h(px(16.0))
}
