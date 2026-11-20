//! Windows & Tiling: how windows snap in Floating mode and how they are laid
//! out in Tiling mode, with a live miniature of the result.

use orchard_config::{NewWindowPlacement, Settings, SplitDirection, TilingMode, TilingSettings};

use iced::widget::column;
use orchard_ui::{choices, form};

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
    let tiling_on = s.windows.tiling_mode == TilingMode::Tiling;

    let mode = form::group([
        form::custom_row(art::tiling_preview(t, tiling_on)),
        form::popup_row(
            "Window layout",
            some("Floating windows overlap freely; Tiling arranges them automatically"),
            choices([("floating", "Floating"), ("tiling", "Tiling")]),
            if tiling_on { "tiling" } else { "floating" },
            |v| {
                edit(move |s| {
                    let tiling = v == "tiling";
                    s.windows.tiling_mode = if tiling { TilingMode::Tiling } else { TilingMode::Floating };
                    if tiling {
                        s.stage_manager.enabled = false;
                    }
                })
            },
        ),
        form::toggle_row(
            "Show switcher in menu bar",
            some("Change the layout from the menu bar"),
            s.menu_bar.show_tiling,
            |v| edit(move |s| s.menu_bar.show_tiling = v),
        ),
    ]);

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

    let dir = match t.split_direction {
        SplitDirection::Auto => "auto",
        SplitDirection::Horizontal => "horizontal",
        SplitDirection::Vertical => "vertical",
    };
    let place = match t.new_window {
        NewWindowPlacement::Focused => "focused",
        NewWindowPlacement::Last => "last",
    };
    let layout = form::group([
        form::popup_row(
            "New windows",
            some("Where a window opens in the layout"),
            choices([("focused", "Next to the focused window"), ("last", "At the end of the layout")]),
            place,
            |v| {
                edit(move |s| {
                    s.tiling.new_window =
                        if v == "last" { NewWindowPlacement::Last } else { NewWindowPlacement::Focused }
                })
            },
        ),
        form::popup_row(
            "Split direction",
            some("Automatic splits along the longer side"),
            choices([("auto", "Automatic"), ("horizontal", "Side by side"), ("vertical", "Stacked")]),
            dir,
            |v| {
                edit(move |s| {
                    s.tiling.split_direction = match v.as_str() {
                        "horizontal" => SplitDirection::Horizontal,
                        "vertical" => SplitDirection::Vertical,
                        _ => SplitDirection::Auto,
                    }
                })
            },
        ),
        form::slider_row(
            "Split ratio",
            some("Share of the space the existing window keeps"),
            0.3..=0.7,
            t.split_ratio as f64,
            0.05,
            Some(("Smaller", "Larger")),
            |v| edit(move |s| s.tiling.split_ratio = ((v * 20.0).round() / 20.0) as f32),
        ),
        form::toggle_row(
            "Smart gaps",
            some("No gaps when a display has just one window"),
            t.smart_gaps,
            |v| edit(move |s| s.tiling.smart_gaps = v),
        ),
    ]);

    column![
        form::section("Layout"),
        mode,
        form::section("Gaps"),
        gaps,
        form::section("Floating — Snapping"),
        floating,
        form::section("Tiling — Layout"),
        layout,
    ]
    .into()
}
