//! Standalone compact "About Device" window, opened from the top bar's logo
//! menu (`orchard-settings --about-device`).

use std::time::{Duration, Instant};

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, Rect, Sense, Stroke, StrokeKind};
use orchard_config::Settings;

use crate::{
    pages::{load_about, AboutInfo},
    style::{self, bold, pal},
    sys::{self, Bg},
    widgets::{self, mix},
};

struct AboutApp {
    settings: Settings,
    styled: Option<(bool, [u8; 3])>,
    last_theme_check: Instant,
    info: Bg<AboutInfo>,
}

impl AboutApp {
    fn new(cc: &eframe::CreationContext) -> Self {
        style::install_fonts(&cc.egui_ctx);
        let settings = Settings::load();
        let mut info = Bg::default();
        info.start(&cc.egui_ctx, load_about);
        Self {
            settings,
            styled: None,
            last_theme_check: Instant::now() - Duration::from_secs(10),
            info,
        }
    }

    fn restyle(&mut self, ctx: &egui::Context) {
        let need_check = self.styled.is_none() || self.last_theme_check.elapsed() > Duration::from_secs(3);
        if !need_check {
            return;
        }
        self.last_theme_check = Instant::now();
        self.settings = Settings::load();
        let dark = style::resolve_dark(self.settings.appearance.mode);
        let key = (dark, self.settings.appearance.accent.rgb());
        if self.styled != Some(key) {
            style::apply(ctx, key.0, key.1);
            self.styled = Some(key);
        }
    }
}

impl eframe::App for AboutApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.restyle(ctx);
        self.info.every(ctx, 60.0, load_about);

        if ctx.input(|i| {
            i.key_pressed(egui::Key::Escape)
                || ((i.modifiers.command || i.modifiers.mac_cmd) && i.key_pressed(egui::Key::W))
        }) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let p = pal();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.content).inner_margin(Margin::symmetric(24, 20)))
            .show(ctx, |ui| {
                let Some(info) = self.info.value.clone() else {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                    });
                    return;
                };

                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    let is_laptop = matches!(
                        info.chassis.as_str(),
                        "laptop" | "notebook" | "convertible" | "portable" | "sub-notebook"
                    );
                    draw_device_illustration(ui, is_laptop);
                    ui.add_space(12.0);

                    let title = if info.hostname.is_empty() {
                        "This Device"
                    } else {
                        info.hostname.as_str()
                    };
                    ui.label(egui::RichText::new(title).font(bold(21.0)).color(p.text));
                    if !info.model.is_empty() {
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(&info.model).size(12.0).color(p.text2));
                    }
                });

                ui.add_space(18.0);

                // Two-column spec list (right-aligned label, left-aligned value)
                let mut rows: Vec<(&str, String)> = Vec::new();
                if !info.cpu.is_empty() {
                    rows.push(("Processor", clean_cpu(&info.cpu)));
                }
                if let Some(gpu) = info.gpus.first() {
                    rows.push(("Graphics", gpu.clone()));
                }
                if !info.memory.is_empty() {
                    rows.push(("Memory", info.memory.clone()));
                }
                if !info.os.is_empty() {
                    rows.push(("OS", info.os.clone()));
                }
                if !info.kernel.is_empty() {
                    rows.push(("Kernel", info.kernel.clone()));
                }
                rows.push(("Desktop", format!("Orchard {}", env!("CARGO_PKG_VERSION"))));
                if !info.uptime.is_empty() {
                    rows.push(("Uptime", info.uptime.clone()));
                }

                let avail = ui.available_width();
                let label_w = 88.0;
                let gap = 12.0;
                let val_w = (avail - label_w - gap).max(120.0);

                ui.spacing_mut().item_spacing.y = 6.0;
                for (label, value) in &rows {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        ui.allocate_ui_with_layout(
                            vec2(label_w, 16.0),
                            Layout::right_to_left(Align::Min),
                            |ui| {
                                ui.label(egui::RichText::new(*label).font(bold(12.0)).color(p.text));
                            },
                        );
                        ui.allocate_ui_with_layout(
                            vec2(val_w, 16.0),
                            Layout::left_to_right(Align::Min),
                            |ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(value).size(12.0).color(p.text2))
                                        .wrap_mode(egui::TextWrapMode::Truncate),
                                )
                                .on_hover_text(value);
                            },
                        );
                    });
                }

                // Bottom action + footer
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new("Orchard Desktop · Wayland").size(10.5).color(p.text3));
                    ui.add_space(10.0);
                    if widgets::button(ui, "More Info…").clicked() {
                        open_settings_about();
                    }
                });
            });
    }
}

/// Strip noisy trademark tokens from `/proc/cpuinfo` model strings so they fit
/// cleanly in the compact About window.
fn clean_cpu(raw: &str) -> String {
    raw.replace("(R)", "")
        .replace("(TM)", "")
        .replace(" CPU", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn open_settings_about() {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "orchard-settings".into());
    sys::spawn_detached(&exe, &["about"]);
}

/// Minimal vector illustration of a laptop or desktop display.
fn draw_device_illustration(ui: &mut egui::Ui, is_laptop: bool) {
    let p = pal();
    let (rect, _) = ui.allocate_exact_size(vec2(124.0, 86.0), Sense::hover());
    let painter = ui.painter();

    let body = if p.dark {
        Color32::from_rgb(0x6E, 0x6E, 0x73)
    } else {
        Color32::from_rgb(0xC7, 0xC7, 0xCC)
    };
    let body_hi = if p.dark {
        Color32::from_rgb(0x8E, 0x8E, 0x93)
    } else {
        Color32::from_rgb(0xE5, 0xE5, 0xEA)
    };
    let bezel = Color32::from_rgb(0x1C, 0x1C, 0x1E);
    let screen_top = mix(p.accent, Color32::from_rgb(0x1D, 0x26, 0x3B), 0.35);
    let screen_bot = mix(p.accent, Color32::from_rgb(0x0E, 0x11, 0x1A), 0.68);

    if is_laptop {
        // Lid / display housing
        let lid = Rect::from_center_size(pos2(rect.center().x, rect.top() + 38.0), vec2(96.0, 62.0));
        painter.rect_filled(lid.translate(vec2(0.0, 2.0)), CornerRadius::same(7), Color32::from_black_alpha(35));
        painter.rect_filled(lid, CornerRadius::same(7), body_hi);
        painter.rect_stroke(lid, CornerRadius::same(7), Stroke::new(1.0_f32, body), StrokeKind::Inside);

        let bezel_rect = lid.shrink(2.0);
        painter.rect_filled(bezel_rect, CornerRadius::same(5), bezel);

        let screen = bezel_rect.shrink(3.5);
        paint_screen_wallpaper(painter, screen, screen_top, screen_bot, p.accent);

        // Camera dot
        painter.circle_filled(pos2(lid.center().x, bezel_rect.top() + 2.0), 1.0, Color32::from_white_alpha(60));

        // Base deck
        let base = Rect::from_center_size(pos2(rect.center().x, lid.bottom() + 4.5), vec2(116.0, 7.0));
        painter.rect_filled(
            base.translate(vec2(0.0, 1.5)),
            CornerRadius { nw: 1, ne: 1, sw: 4, se: 4 },
            Color32::from_black_alpha(35),
        );
        painter.rect_filled(base, CornerRadius { nw: 1, ne: 1, sw: 4, se: 4 }, body_hi);
        painter.rect_stroke(
            base,
            CornerRadius { nw: 1, ne: 1, sw: 4, se: 4 },
            Stroke::new(0.8_f32, body),
            StrokeKind::Inside,
        );
        // Thumb notch
        let notch = Rect::from_center_size(pos2(base.center().x, base.top() + 1.5), vec2(18.0, 2.5));
        painter.rect_filled(notch, CornerRadius::same(1), body);
    } else {
        // Desktop monitor
        let display = Rect::from_center_size(pos2(rect.center().x, rect.top() + 36.0), vec2(104.0, 66.0));
        painter.rect_filled(display.translate(vec2(0.0, 2.0)), CornerRadius::same(7), Color32::from_black_alpha(35));
        painter.rect_filled(display, CornerRadius::same(7), body_hi);
        painter.rect_stroke(display, CornerRadius::same(7), Stroke::new(1.0_f32, body), StrokeKind::Inside);

        let bezel_rect = display.shrink(2.0);
        painter.rect_filled(bezel_rect, CornerRadius::same(5), bezel);

        let screen = bezel_rect.shrink(3.5);
        paint_screen_wallpaper(painter, screen, screen_top, screen_bot, p.accent);

        // Stand neck
        let neck = Rect::from_center_size(pos2(rect.center().x, display.bottom() + 6.5), vec2(20.0, 13.0));
        painter.rect_filled(neck, CornerRadius::same(1), body);

        // Stand foot
        let foot = Rect::from_center_size(pos2(rect.center().x, neck.bottom() + 1.5), vec2(44.0, 4.0));
        painter.rect_filled(foot.translate(vec2(0.0, 1.0)), CornerRadius::same(2), Color32::from_black_alpha(30));
        painter.rect_filled(foot, CornerRadius::same(2), body_hi);
        painter.rect_stroke(foot, CornerRadius::same(2), Stroke::new(0.7_f32, body), StrokeKind::Inside);
    }
}

fn paint_screen_wallpaper(painter: &egui::Painter, screen: Rect, top: Color32, bot: Color32, accent: Color32) {
    painter.rect_filled(screen, CornerRadius::same(3), bot);
    let upper = Rect::from_min_max(screen.min, pos2(screen.max.x, screen.center().y + 4.0));
    painter.rect_filled(
        upper,
        CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 },
        top,
    );
    // Soft glowing horizon orb
    painter.circle_filled(
        pos2(screen.center().x + 12.0, screen.center().y + 4.0),
        screen.height() * 0.42,
        accent.gamma_multiply(0.38),
    );
    // Subtle top menu bar strip inside screen
    let bar = Rect::from_min_max(screen.min, pos2(screen.max.x, screen.top() + 3.0));
    painter.rect_filled(
        bar,
        CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 },
        Color32::from_white_alpha(45),
    );
    // Subtle dock pill inside screen
    let dock = Rect::from_center_size(pos2(screen.center().x, screen.bottom() - 4.0), vec2(28.0, 3.5));
    painter.rect_filled(dock, CornerRadius::same(2), Color32::from_white_alpha(90));
    // Tiny Orchard label watermark
    painter.text(
        pos2(screen.center().x, screen.center().y - 1.0),
        Align2::CENTER_CENTER,
        "Orchard",
        FontId::proportional(10.0),
        Color32::from_white_alpha(200),
    );
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("About Device")
            .with_app_id("org.orchard.about")
            .with_inner_size([320.0, 420.0])
            .with_resizable(false)
            .with_maximize_button(false),
        ..Default::default()
    };
    eframe::run_native("About Device", options, Box::new(|cc| Ok(Box::new(AboutApp::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_cpu_strips_trademarks_and_extra_spaces() {
        assert_eq!(
            clean_cpu("Intel(R) Core(TM) i7-12700K CPU (20 threads)"),
            "Intel Core i7-12700K (20 threads)"
        );
        assert_eq!(
            clean_cpu("AMD Ryzen 9 7950X 16-Core Processor (32 threads)"),
            "AMD Ryzen 9 7950X 16-Core Processor (32 threads)"
        );
    }
}
