//! orchard-settings: System Settings for the Orchard desktop.
//!
//! Orchard's own options are stored in `~/.config/orchard/settings.json` and
//! applied live (the compositor is told to reload over IPC). System options
//! (Wi‑Fi, Bluetooth, sound, time, …) go through the usual Linux tools:
//! nmcli, bluetoothctl, pactl, timedatectl, xdg-mime.

mod about_window;
mod icons;
mod pages;
mod style;
mod sys;
mod widgets;

use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Margin, Rect, Sense, Stroke};
use orchard_config::Settings;

use crate::{icons::Icons, style::pal, widgets::rgb};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Page {
    Wifi,
    Bluetooth,
    Network,
    Sound,
    General,
    About,
    DateTime,
    Language,
    LoginItems,
    DefaultApps,
    Storage,
    Appearance,
    Accessibility,
    MenuBar,
    DesktopDock,
    Displays,
    Wallpaper,
    LockScreen,
    Battery,
    Users,
    Keyboard,
    Mouse,
    Trackpad,
}

use Page::*;

const SIDEBAR: &[&[Page]] = &[
    &[Wifi, Bluetooth, Network],
    &[Sound],
    &[General, Appearance, Accessibility, MenuBar, DesktopDock, Displays, Wallpaper],
    &[LockScreen, Battery, Users],
    &[Keyboard, Mouse, Trackpad],
];

const ALL_PAGES: &[Page] = &[
    Wifi, Bluetooth, Network, Sound, General, About, DateTime, Language, LoginItems, DefaultApps, Storage,
    Appearance, Accessibility, MenuBar, DesktopDock, Displays, Wallpaper, LockScreen, Battery, Users, Keyboard,
    Mouse, Trackpad,
];

impl Page {
    pub fn title(self) -> &'static str {
        match self {
            Wifi => "Wi‑Fi",
            Bluetooth => "Bluetooth",
            Network => "Network",
            Sound => "Sound",
            General => "General",
            About => "About",
            DateTime => "Date & Time",
            Language => "Language & Region",
            LoginItems => "Login Items",
            DefaultApps => "Default Apps",
            Storage => "Storage",
            Appearance => "Appearance",
            Accessibility => "Accessibility",
            MenuBar => "Menu Bar",
            DesktopDock => "Desktop & Dock",
            Displays => "Displays",
            Wallpaper => "Wallpaper",
            LockScreen => "Lock Screen",
            Battery => "Battery",
            Users => "Users & Groups",
            Keyboard => "Keyboard",
            Mouse => "Mouse",
            Trackpad => "Trackpad",
        }
    }

    pub fn icon(self) -> (&'static str, Color32) {
        let blue = rgb([0x0A, 0x84, 0xFF]);
        let gray = rgb([0x8E, 0x8E, 0x93]);
        let black = rgb([0x2C, 0x2C, 0x2E]);
        match self {
            Wifi => ("network-wireless", blue),
            Bluetooth => ("bluetooth", blue),
            Network => ("network-workgroup", blue),
            Sound => ("audio-volume-high", rgb([0xFF, 0x37, 0x5F])),
            General => ("emblem-system", gray),
            About => ("computer", gray),
            DateTime => ("preferences-system-time", blue),
            Language => ("preferences-desktop-locale", blue),
            LoginItems => ("system-run", gray),
            DefaultApps => ("view-app-grid", rgb([0xAF, 0x52, 0xDE])),
            Storage => ("drive-harddisk", gray),
            Appearance => ("preferences-desktop-appearance", black),
            Accessibility => ("preferences-desktop-accessibility", blue),
            MenuBar => ("open-menu", gray),
            DesktopDock => ("view-app-grid", black),
            Displays => ("video-display", blue),
            Wallpaper => ("preferences-desktop-wallpaper", rgb([0x32, 0xAD, 0xE6])),
            LockScreen => ("system-lock-screen", black),
            Battery => ("battery-full", rgb([0x30, 0xD1, 0x58])),
            Users => ("system-users", blue),
            Keyboard => ("input-keyboard", gray),
            Mouse => ("input-mouse", gray),
            Trackpad => ("input-touchpad", gray),
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Wifi => "wireless wlan internet network ssid",
            Bluetooth => "devices headphones pair",
            Network => "ethernet vpn ip proxy wired lan",
            Sound => "volume audio output input microphone speakers mute",
            General => "about software system",
            About => "system hardware cpu memory version hostname name computer",
            DateTime => "clock time zone ntp 24-hour",
            Language => "locale region format",
            LoginItems => "autostart startup",
            DefaultApps => "browser mail web terminal file manager open with",
            Storage => "disk space drive",
            Appearance => "dark light mode accent colour color theme",
            Accessibility => "reduce motion transparency animation",
            MenuBar => "clock control center battery percentage date seconds",
            DesktopDock => "dock magnification minimize hot corners mission control windows title bar focus",
            Displays => "screen monitor resolution scale hidpi",
            Wallpaper => "background desktop picture",
            LockScreen => "lock idle screensaver password",
            Battery => "power energy profile performance",
            Users => "account password user group",
            Keyboard => "layout repeat shortcuts keys caps",
            Mouse => "pointer speed scroll natural acceleration",
            Trackpad => "touchpad tap click gestures swipe",
        }
    }

    /// Sub pages of General show it highlighted in the sidebar.
    fn sidebar_parent(self) -> Page {
        match self {
            About | DateTime | Language | LoginItems | DefaultApps | Storage => General,
            p => p,
        }
    }
}

struct App {
    settings: Settings,
    saved: Settings,
    dirty_at: Option<Instant>,
    page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    search: String,
    icons: Icons,
    styled: Option<(bool, [u8; 3])>,
    last_dark_check: Instant,
    st: pages::State,
    /// None = unknown yet, Some(false) = compositor didn't accept reload
    compositor_live: Option<bool>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> App {
        style::install_fonts(&cc.egui_ctx);
        let settings = Settings::load();
        let norm = |s: &str| s.to_lowercase().replace([' ', '\u{2011}', '-'], "");
        let start = std::env::args()
            .nth(1)
            .and_then(|a| {
                let want = norm(&a);
                ALL_PAGES.iter().copied().find(|p| norm(p.title()) == want)
            })
            .unwrap_or(Appearance);
        let mut app = App {
            saved: settings.clone(),
            settings,
            dirty_at: None,
            page: start,
            back: Vec::new(),
            forward: Vec::new(),
            search: String::new(),
            icons: Icons::default(),
            styled: None,
            last_dark_check: Instant::now() - Duration::from_secs(10),
            st: pages::State::default(),
            compositor_live: None,
        };
        app.restyle(&cc.egui_ctx);
        app
    }

    fn go(&mut self, page: Page) {
        if page != self.page {
            self.back.push(self.page);
            self.forward.clear();
            self.page = page;
        }
    }

    fn restyle(&mut self, ctx: &egui::Context) {
        // Auto mode follows gsettings; re-check every few seconds
        let need_check = self.styled.is_none() || self.last_dark_check.elapsed() > Duration::from_secs(3);
        if !need_check {
            return;
        }
        self.last_dark_check = Instant::now();
        let dark = style::resolve_dark(self.settings.appearance.mode);
        let key = (dark, self.settings.appearance.accent.rgb());
        if self.styled != Some(key) {
            style::apply(ctx, key.0, key.1);
            self.styled = Some(key);
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.settings.save() {
            eprintln!("orchard-settings: saving failed: {e}");
            return;
        }
        // apps follow along through the compositor (session::sync_app_appearance),
        // which writes Orchard's own dconf database, never another desktop's
        self.saved = self.settings.clone();
        let reply = orchard_config::ipc_request(&serde_json::json!({"request": "reload_settings"}));
        self.compositor_live = Some(matches!(reply, Ok(ref v) if v["event"] == "ok"));
        self.styled = None; // re-evaluate appearance
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.add_space(6.0);
        // search field
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
        ui.painter().rect_filled(rect, 7, if p.dark { Color32::from_white_alpha(16) } else { Color32::from_black_alpha(14) });
        self.icons
            .paint(ui, "system-search", Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), vec2(13.0, 13.0)), p.text2);
        let edit_rect = Rect::from_min_max(pos2(rect.left() + 26.0, rect.top() + 4.0), pos2(rect.right() - 6.0, rect.bottom() - 4.0));
        ui.put(
            edit_rect,
            egui::TextEdit::singleline(&mut self.search)
                .frame(false)
                .hint_text(egui::RichText::new("Search").color(p.text2))
                .margin(Margin::symmetric(0, 1)),
        );
        ui.add_space(10.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let q = self.search.trim().to_lowercase();
            if !q.is_empty() {
                let hits: Vec<Page> = ALL_PAGES
                    .iter()
                    .copied()
                    .filter(|pg| pg.title().to_lowercase().contains(&q) || pg.keywords().contains(&q))
                    .collect();
                if hits.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| widgets::secondary(ui, "No Results"));
                }
                for pg in hits {
                    self.sidebar_item(ui, pg);
                }
                return;
            }
            // account card
            let name = self.st.user_name();
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
            if self.page == Users {
                ui.painter().rect_filled(rect, 7, p.hover);
            }
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(6.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
            let tex = self.st.avatar(ui.ctx());
            widgets::avatar(&mut child, 34.0, &name, tex.as_ref());
            child.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(egui::RichText::new(&name).font(style::bold(13.0)));
                ui.label(egui::RichText::new("Orchard Account").size(11.0).color(p.text2));
            });
            if resp.clicked() {
                self.go(Users);
            }
            ui.add_space(8.0);
            for section in SIDEBAR {
                for &pg in *section {
                    self.sidebar_item(ui, pg);
                }
                ui.add_space(12.0);
            }
        });
    }

    fn sidebar_item(&mut self, ui: &mut egui::Ui, pg: Page) {
        let p = pal();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
        let selected = self.page.sidebar_parent() == pg || self.page == pg;
        if selected {
            ui.painter().rect_filled(rect, 6, p.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 6, p.hover);
        }
        let (icon, color) = pg.icon();
        let tile = Rect::from_min_size(pos2(rect.left() + 6.0, rect.center().y - 10.0), vec2(20.0, 20.0));
        widgets::tile(ui, &mut self.icons, tile, color, icon);
        ui.painter().text(
            pos2(rect.left() + 34.0, rect.center().y),
            Align2::LEFT_CENTER,
            pg.title(),
            FontId::proportional(13.0),
            if selected { Color32::WHITE } else { p.text },
        );
        if resp.clicked() {
            self.go(pg);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        let mut x = rect.left() + 14.0;
        for (i, enabled) in [(0, !self.back.is_empty()), (1, !self.forward.is_empty())] {
            let r = Rect::from_center_size(pos2(x + 11.0, rect.center().y), vec2(24.0, 24.0));
            let resp = ui.interact(r, ui.id().with(("nav", i)), if enabled { Sense::click() } else { Sense::hover() });
            if enabled && resp.hovered() {
                ui.painter().rect_filled(r, 6, p.hover);
            }
            let color = if enabled { p.text2 } else { p.text3 };
            if i == 0 {
                widgets::chevron_left(ui, r.center(), color);
            } else {
                widgets::chevron_right(ui, r.center(), color);
            }
            if resp.clicked() {
                if i == 0 {
                    if let Some(prev) = self.back.pop() {
                        self.forward.push(self.page);
                        self.page = prev;
                    }
                } else if let Some(next) = self.forward.pop() {
                    self.back.push(self.page);
                    self.page = next;
                }
            }
            x += 26.0;
        }
        ui.painter().text(
            pos2(x + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            self.page.title(),
            style::bold(15.0),
            p.text,
        );
    }
}

impl App {
    fn update_ui(&mut self, ctx: &egui::Context) {
        self.restyle(ctx);
        let p = pal();
        let start_page = self.page;

        // keyboard: ⌘F / Ctrl+F focuses search, ⌘[ / ⌘] navigate
        if ctx.input(|i| (i.modifiers.command || i.modifiers.mac_cmd) && i.key_pressed(egui::Key::W)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::SidePanel::left("sidebar")
            .exact_width(232.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(10, 8)))
            .show(ctx, |ui| self.sidebar(ui));

        if self.page != start_page {
            // Re-run this frame immediately so sidebar highlight, toolbar, and
            // content all render the new page atomically without a 1-frame mismatch.
            ctx.request_discard("page changed");
            return;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.content))
            .show(ctx, |ui| {
                // hairline between sidebar and content
                let r = ui.max_rect();
                ui.painter().vline(r.left(), r.y_range(), Stroke::new(1.0_f32, p.group_border));
                self.toolbar(ui);
                if self.page != start_page {
                    ctx.request_discard("page changed");
                    return;
                }
                if self.compositor_live == Some(false) {
                    ui.horizontal(|ui| {
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new(
                                "Saved. The running Orchard is too old to apply changes live; they take effect at next login.",
                            )
                            .size(11.5)
                            .color(p.text2),
                        );
                    });
                }
                let page = self.page;
                let mut nav = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt(page)
                    .show(ui, |ui| {
                        // ScrollArea's id_salt only scopes the scroll state, not its
                        // content Ui; push_id gives each page its own widget IDs so
                        // toggles/popups don't inherit state/animations from the
                        // previous page.
                        ui.push_id(page, |ui| {
                            let avail = ui.available_width();
                            let w = (avail - 48.0).min(640.0);
                            let pad = ((avail - w) / 2.0).max(0.0);
                            ui.horizontal_top(|ui| {
                                ui.add_space(pad);
                                ui.vertical(|ui| {
                                    ui.set_width(w);
                                    ui.add_space(4.0);
                                    let mut cx = pages::Cx {
                                        s: &mut self.settings,
                                        icons: &mut self.icons,
                                        st: &mut self.st,
                                        nav: &mut nav,
                                    };
                                    pages::show(ui, page, &mut cx);
                                    ui.add_space(24.0);
                                });
                            });
                        });
                    });
                if let Some(n) = nav {
                    self.go(n);
                    if self.page != start_page {
                        ctx.request_discard("page changed");
                    }
                }
            });

        // debounce saving so sliders don't hammer the compositor
        if self.settings != self.saved {
            let since = *self.dirty_at.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_millis(220) {
                self.save();
                self.dirty_at = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
        } else {
            self.dirty_at = None;
        }
        if self.settings.appearance != self.saved.appearance || self.styled.is_none() {
            let dark = style::resolve_dark(self.settings.appearance.mode);
            let key = (dark, self.settings.appearance.accent.rgb());
            if self.styled != Some(key) {
                style::apply(ctx, key.0, key.1);
                self.styled = Some(key);
            }
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        pal().content.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_ui(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.settings != self.saved {
            self.save();
        }
    }
}

fn main() -> eframe::Result<()> {
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("--about-device" | "--about" | "about-device")
    ) {
        return about_window::run();
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("System Settings")
            .with_app_id("org.orchard.settings")
            .with_inner_size([900.0, 660.0])
            .with_min_inner_size([760.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("System Settings", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
