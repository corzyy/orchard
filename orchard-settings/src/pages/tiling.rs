//! Windows: how windows snap to screen edges and corners, with a live
//! miniature of the result.

use orchard_config::{Settings, TilingSettings};

use iced::widget::column;
use orchard_ui::form;

use crate::pages::{art, edit, some, El};

fn gap_slider(label: &'static str, sub: &'static str, v: i32, set: fn(&mut TilingSettings, i32)) -> El {
    form::slider_row(
        label,
        some(sub),
        0.0..=TilingSettings::GAP_MAX as f64,
        v as f64,
        1.0,
        Some(("None", "Wide")),
        move |x| edit(move |s| set(&mut s.tiling, x.round() as i32)),
    )
}

pub fn tiling(s: &Settings) -> El {
    let t = &s.tiling;

    let preview = form::group([form::custom_row(art::tiling_preview(t))]);

    let gaps = form::group([
        gap_slider("Outer gap", "Margin between windows and the screen edges", t.outer_gap, |t, v| t.outer_gap = v),
        gap_slider("Inner gap", "Space between neighbouring windows", t.inner_gap, |t, v| t.inner_gap = v),
    ]);

    let mut snap_rows = vec![form::toggle_row(
        "Snap windows to screen edges",
        some("Drag a window to an edge or corner to tile it"),
        t.snap_to_edges,
        |v| edit(move |s| s.tiling.snap_to_edges = v),
    )];
    if t.snap_to_edges {
        snap_rows.extend([
            form::toggle_row(
                "Show drop preview",
                some("A translucent outline shows where the window will land"),
                t.snap_preview,
                |v| edit(move |s| s.tiling.snap_preview = v),
            ),
            form::toggle_row(
                "Corners tile to quarters",
                some("Otherwise corners behave like the nearest edge"),
                t.snap_corners,
                |v| edit(move |s| s.tiling.snap_corners = v),
            ),
            form::slider_row(
                "Edge sensitivity",
                some("How close to the edge the pointer must get"),
                8.0..=120.0,
                t.edge_zone as f64,
                1.0,
                Some(("Precise", "Generous")),
                |v| edit(move |s| s.tiling.edge_zone = v.round() as i32),
            ),
        ]);
    }
    let floating = form::group(snap_rows);

    column![
        form::section("Preview"),
        preview,
        form::section("Snapping"),
        floating,
        form::section("Gaps"),
        gaps,
    ]
    .into()
}
