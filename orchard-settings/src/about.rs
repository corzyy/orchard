//! Standalone compact "About Device" window (`orchard-settings --about-device`).

use iced::alignment::Horizontal;
use iced::keyboard::{self, key::Named, Key};
use iced::widget::{column, container, row, text, Space};
use iced::{Alignment, Element, Length, Size, Subscription, Task};
use orchard_config::Settings;
use orchard_ui::appearance::resolve_dark;
use orchard_ui::text::truncate;
use orchard_ui::{controls, fonts, palette, style, Appearance};

use crate::pages::{
    art,
    system::{load_about, AboutInfo},
};
use crate::sys;

const W: f32 = 320.0;
const H: f32 = 410.0;

#[derive(Debug, Clone)]
enum Message {
    Loaded(Option<AboutInfo>),
    MoreInfo,
    Close,
}

#[derive(Default)]
struct About {
    info: Option<AboutInfo>,
}

impl About {
    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Loaded(info) => {
                self.info = info;
                Task::none()
            }
            Message::MoreInfo => {
                let exe = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.to_str().map(String::from))
                    .unwrap_or_else(|| "orchard-settings".into());
                sys::spawn_detached(&exe, &["system"]);
                Task::none()
            }
            Message::Close => iced::exit(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        keyboard::on_key_press(|key, _| matches!(key, Key::Named(Named::Escape)).then_some(Message::Close))
    }

    fn view(&self) -> Element<'_, Message> {
        let p = palette();
        let body: Element<'_, Message> = match &self.info {
            None => container(text("Loading…").size(13).color(p.text_secondary)).center(Length::Fill).into(),
            Some(info) => {
                let is_laptop = matches!(
                    info.chassis.as_str(),
                    "laptop" | "notebook" | "convertible" | "portable" | "sub-notebook"
                );
                let title = if info.hostname.is_empty() { "This Device" } else { info.hostname.as_str() };
                let mut c = column![
                    Space::with_height(10),
                    art::device(is_laptop),
                    text(truncate(title, 28)).size(20).font(fonts::display_bold()).color(p.text),
                ]
                .spacing(4)
                .align_x(Alignment::Center)
                .height(Length::Fill);
                if !info.model.is_empty() {
                    c = c.push(text(truncate(&info.model, 44)).size(12).color(p.text_secondary));
                }
                c = c.push(Space::with_height(14));
                for (k, v) in rows(info) {
                    c = c.push(
                        row![
                            text(k).size(12).font(fonts::bold()).color(p.text).width(92).align_x(Horizontal::Right),
                            text(truncate(&v, 30)).size(12).color(p.text_secondary),
                        ]
                        .spacing(12)
                        .padding([0, 12])
                        .width(Length::Fill),
                    );
                }
                c.into()
            }
        };

        container(
            column![
                body,
                controls::push_button("More Info…", Some(Message::MoreInfo)),
                Space::with_height(14),
                text("Orchard Desktop · Wayland").size(10.5).color(p.text_tertiary),
            ]
            .align_x(Alignment::Center),
        )
        .padding(iced::Padding { bottom: 10.0, ..Default::default() })
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::content)
        .into()
    }
}

/// The spec rows shown under the device, in display order.
fn rows(info: &AboutInfo) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
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
    rows
}

pub fn clean_cpu(raw: &str) -> String {
    raw.replace("(R)", "")
        .replace("(TM)", "")
        .replace(" CPU", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn run() -> iced::Result {
    let s = Settings::load();
    let dark = resolve_dark(crate::theme_mode(s.appearance.mode));
    orchard_ui::set_appearance(Appearance::new(dark, s.appearance.accent_rgb()));

    let mut window = orchard_ui::window_settings("org.orchard.about", (W, H), Some((W, H)));
    window.max_size = Some(Size::new(W, H));
    window.resizable = false;

    iced::application("About Device", About::update, About::view)
        .theme(|_| orchard_ui::theme())
        .default_font(fonts::text())
        .antialiasing(true)
        .subscription(About::subscription)
        .window(window)
        .run_with(|| (About::default(), Task::perform(sys::blocking(load_about), Message::Loaded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_cpu_strips_trademarks() {
        assert_eq!(
            clean_cpu("Intel(R) Core(TM) i7-12700K CPU (20 threads)"),
            "Intel Core i7-12700K (20 threads)"
        );
    }
}
