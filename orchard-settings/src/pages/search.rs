//! Spotlight (⌘Space): what it searches, how the file index refreshes and
//! whether the previous search is kept when it reopens.

use iced::widget::column;
use orchard_config::{SearchSettings, Settings};
use orchard_ui::{choices, form};

use crate::pages::{edit, some, El};

pub fn search(s: &Settings) -> El {
    let q = &s.search;
    let minutes = SearchSettings::INDEX_MINUTES
        .iter()
        .map(|m| (m.to_string(), format!("Every {m} minutes")))
        .collect::<Vec<_>>();
    let current = q.index_minutes.clamp(1, 24 * 60).to_string();

    let rows = vec![
        form::toggle_row(
            "Applications",
            some("Installed apps, ranked by what you use most"),
            q.show_apps,
            |v| edit(move |s| s.search.show_apps = v),
        ),
        form::toggle_row(
            "System Settings",
            some("Settings panes such as Wi‑Fi, Displays and Keyboard"),
            q.show_settings,
            |v| edit(move |s| s.search.show_settings = v),
        ),
        form::toggle_row(
            "System actions",
            some("Lock Screen, Sleep, Restart, Shut Down and the like"),
            q.show_actions,
            |v| edit(move |s| s.search.show_actions = v),
        ),
        form::toggle_row(
            "Web search",
            some("Offer a DuckDuckGo search when nothing else matches"),
            q.show_web,
            |v| edit(move |s| s.search.show_web = v),
        ),
    ];

    column![
        form::section("Results"),
        form::group(rows),
        form::section("Files"),
        form::group([
            form::toggle_row(
                "Search files and folders",
                some("Include files from your home folder, newest and most used first"),
                q.show_files,
                |v| edit(move |s| s.search.show_files = v),
            ),
            form::popup_row(
                "Refresh file index",
                some("Spotlight rebuilds its index in the background"),
                choices(minutes),
                &current,
                |v| {
                    edit(move |s| {
                        if let Ok(m) = v.parse() {
                            s.search.index_minutes = m;
                        }
                    })
                },
            ),
        ]),
        form::section("Behaviour"),
        form::group([form::toggle_row(
            "Remember last search",
            some("Reopen Spotlight with the previous search selected"),
            q.keep_query,
            |v| edit(move |s| s.search.keep_query = v),
        )]),
        form::footnote("Open Spotlight from anywhere with ⌘Space."),
    ]
    .into()
}
