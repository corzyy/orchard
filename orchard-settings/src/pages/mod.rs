//! The individual settings panes.

mod apps;
mod desktop;
mod general;
mod input;
mod network;
mod power;
mod sound;

pub use general::{load_about, AboutInfo};

use eframe::egui::{self, vec2, Sense, TextureHandle, Ui};
use orchard_config::Settings;

use crate::{
    icons::Icons,
    style::{bold, pal},
    sys::Bg,
    widgets::{self, group},
    Page,
};

pub struct Cx<'a> {
    pub s: &'a mut Settings,
    pub icons: &'a mut Icons,
    pub st: &'a mut State,
    pub nav: &'a mut Option<Page>,
}

#[derive(Default)]
pub struct State {
    user: Bg<general::UserInfo>,
    avatar: Option<Option<TextureHandle>>,
    pub net: network::NetState,
    pub sound: sound::SoundState,
    pub general: general::GeneralState,
    pub apps: apps::AppsState,
    pub desktop: desktop::DesktopState,
    pub power: power::PowerState,
    pub input: input::InputState,
}

impl State {
    pub fn user_name(&mut self) -> String {
        self.user.poll();
        if self.user.value.is_none() && !self.user.busy() {
            // cheap enough to do synchronously once
            self.user.value = Some(general::UserInfo::load());
        }
        self.user.value.as_ref().map(|u| u.display_name()).unwrap_or_default()
    }

    pub fn avatar(&mut self, ctx: &egui::Context) -> Option<TextureHandle> {
        self.avatar
            .get_or_insert_with(|| {
                let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
                let user = std::env::var("USER").unwrap_or_default();
                let candidates = [
                    home.join(".face.icon"),
                    home.join(".face"),
                    std::path::PathBuf::from(format!("/var/lib/AccountsService/icons/{user}")),
                ];
                let img = candidates.iter().find_map(|p| image::open(p).ok())?;
                let img = img.thumbnail_exact(128, 128).to_rgba8();
                let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img);
                Some(ctx.load_texture("avatar", ci, egui::TextureOptions::LINEAR))
            })
            .clone()
    }

    pub fn user(&mut self) -> general::UserInfo {
        self.user_name();
        self.user.value.clone().unwrap_or_default()
    }
}

/// The big icon + title + description card at the top of a pane.
pub fn hero(ui: &mut Ui, cx: &mut Cx, page: Page, desc: &str) {
    group(ui, |g| {
        g.custom(|ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
                let (icon, color) = page.icon();
                widgets::tile(ui, cx.icons, r, color, icon);
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(egui::RichText::new(page.title()).font(bold(15.0)));
                    ui.label(egui::RichText::new(desc).size(12.0).color(pal().text2));
                });
            });
        });
    });
}

pub fn show(ui: &mut Ui, page: Page, cx: &mut Cx) {
    match page {
        Page::Wifi => network::wifi(ui, cx),
        Page::Bluetooth => network::bluetooth(ui, cx),
        Page::Network => network::network(ui, cx),
        Page::Sound => sound::sound(ui, cx),
        Page::General => general::general(ui, cx),
        Page::About => general::about(ui, cx),
        Page::DateTime => general::date_time(ui, cx),
        Page::Language => general::language(ui, cx),
        Page::LoginItems => apps::login_items(ui, cx),
        Page::DefaultApps => apps::default_apps(ui, cx),
        Page::Storage => general::storage(ui, cx),
        Page::Users => general::users(ui, cx),
        Page::Appearance => desktop::appearance(ui, cx),
        Page::Accessibility => desktop::accessibility(ui, cx),
        Page::MenuBar => desktop::menu_bar(ui, cx),
        Page::DesktopDock => desktop::desktop_dock(ui, cx),
        Page::Displays => desktop::displays(ui, cx),
        Page::Wallpaper => desktop::wallpaper(ui, cx),
        Page::LockScreen => desktop::lock_screen(ui, cx),
        Page::Battery => power::battery(ui, cx),
        Page::Keyboard => input::keyboard(ui, cx),
        Page::Mouse => input::mouse(ui, cx),
        Page::Trackpad => input::trackpad(ui, cx),
    }
}

/// Red error text under a group, if any.
pub fn error_line(ui: &mut Ui, err: Option<String>) {
    if let Some(e) = err {
        ui.add_space(-6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(e).size(11.5).color(egui::Color32::from_rgb(0xFF, 0x45, 0x3A)));
        });
        ui.add_space(10.0);
    }
}
