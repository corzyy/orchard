//! orchard-settings: System Settings for Orchard, built with orchard-ui.

mod about;
mod pages;
mod sys;

use std::cell::RefCell;
use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, text, text_input, Column};
use iced::{event, keyboard, mouse, window, Alignment, Element, Event, Length, Subscription, Task};
use orchard_config::{Settings, ThemeMode};
use orchard_ui::appearance::{resolve_dark, Mode};
use orchard_ui::style::Selection;
use orchard_ui::{controls, fonts, form, metrics, motion, palette, style, window as chrome, Appearance, TransitionDir};

use crate::pages::{Action, Page, PageState, ALL_PAGES, SIDEBAR};
use crate::sys::Wake;

/// Settings are written this long after the last change.
const SAVE_DELAY: Duration = Duration::from_millis(180);

#[derive(Debug, Clone)]
pub enum Message {
    Go(Page),
    Back,
    Forward,
    Search(String),
    FocusSearch,
    /// Run a page action against the settings / page state.
    Do(Action),
    /// A background worker finished something.
    Wake,
    /// Periodic refresh of time-limited data (device lists, volumes…).
    Tick,
    Save(u64),
    Scrolled(f32),
    Close,
}

struct App {
    settings: Settings,
    saved: Settings,
    save_serial: u64,
    compositor_live: Option<bool>,
    styled_key: (ThemeMode, [u8; 3]),

    page: Page,
    back: Vec<Page>,
    forward: Vec<Page>,
    transition_seq: u64,
    transition_dir: TransitionDir,
    search: String,
    user: (String, String),
    scrolled: bool,

    /// Borrowed mutably while building the view (pages start loads lazily).
    st: RefCell<PageState>,
    wake: Wake,
}

fn search_id() -> text_input::Id {
    text_input::Id::new("sidebar-search")
}

fn content_id() -> scrollable::Id {
    scrollable::Id::new("content")
}

fn theme_mode(m: ThemeMode) -> Mode {
    match m {
        ThemeMode::Auto => Mode::Auto,
        ThemeMode::Light => Mode::Light,
        ThemeMode::Dark => Mode::Dark,
    }
}

impl App {
    fn new(page: Page) -> (Self, Task<Message>) {
        let settings = Settings::load();
        let key = (settings.appearance.mode, settings.appearance.accent_rgb());
        orchard_ui::set_appearance(Appearance::new(resolve_dark(theme_mode(key.0)), key.1));
        motion::set_reduced(settings.appearance.reduce_motion);
        let app = Self {
            saved: settings.clone(),
            settings,
            save_serial: 0,
            compositor_live: None,
            styled_key: key,
            page,
            back: Vec::new(),
            forward: Vec::new(),
            transition_seq: 0,
            transition_dir: TransitionDir::Fade,
            search: String::new(),
            user: current_user(),
            scrolled: false,
            st: RefCell::new(PageState::default()),
            wake: sys::wake_channel(),
        };
        (app, Task::none())
    }

    /// Restyle when the appearance settings change. Keyed on the *mode*, not
    /// the resolved value: resolving `Auto` spawns gsettings.
    fn update_theme(&mut self) {
        motion::set_reduced(self.settings.appearance.reduce_motion);
        let key = (self.settings.appearance.mode, self.settings.appearance.accent_rgb());
        if self.styled_key != key {
            self.styled_key = key;
            orchard_ui::set_appearance(Appearance::new(resolve_dark(theme_mode(key.0)), key.1));
        }
    }

    fn navigate(&mut self, msg: &Message) -> Task<Message> {
        let (target, dir) = match *msg {
            Message::Go(p) if p != self.page => {
                let dir = if p.sidebar_item() == self.page {
                    TransitionDir::Forward
                } else if self.page.sidebar_item() == p {
                    TransitionDir::Back
                } else {
                    TransitionDir::Fade
                };
                self.back.push(self.page);
                self.forward.clear();
                (p, dir)
            }
            Message::Back => match self.back.pop() {
                Some(p) => {
                    self.forward.push(self.page);
                    (p, TransitionDir::Back)
                }
                None => return Task::none(),
            },
            Message::Forward => match self.forward.pop() {
                Some(p) => {
                    self.back.push(self.page);
                    (p, TransitionDir::Forward)
                }
                None => return Task::none(),
            },
            _ => return Task::none(),
        };
        self.page = target;
        self.transition_seq = self.transition_seq.wrapping_add(1);
        self.transition_dir = dir;
        self.scrolled = false;
        scrollable::snap_to(content_id(), scrollable::RelativeOffset::START)
    }

    /// After anything that may have changed the settings: restyle and
    /// schedule a debounced save.
    fn settings_touched(&mut self) -> Task<Message> {
        if self.settings == self.saved {
            return Task::none();
        }
        self.update_theme();
        self.save_serial += 1;
        let serial = self.save_serial;
        Task::perform(tokio::time::sleep(SAVE_DELAY), move |_| Message::Save(serial))
    }

    fn save_now(&mut self) {
        if self.settings == self.saved {
            return;
        }
        // only write what was changed here: the Control Center, the menu bar
        // or an editor may have changed other fields since we loaded the file
        let merged = orchard_config::merge_settings(&self.saved, &self.settings, &Settings::load());
        if let Err(e) = merged.save() {
            eprintln!("orchard-settings: save failed: {e}");
            return;
        }
        self.settings = merged;
        self.saved = self.settings.clone();
        let reply = orchard_config::ipc_request(&serde_json::json!({"request": "reload_settings"}));
        self.compositor_live = Some(matches!(reply, Ok(ref v) if v["event"] == "ok"));
    }

    fn poll(&mut self) -> Task<Message> {
        pages::poll(self.page, &mut self.settings, self.st.get_mut(), &self.wake);
        let focus = self.st.get_mut().focus.take();
        let save = self.settings_touched();
        match focus {
            Some(id) => Task::batch([save, text_input::focus(id)]),
            None => save,
        }
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Go(_) | Message::Back | Message::Forward => self.navigate(&msg),
            Message::Search(q) => {
                self.search = q;
                Task::none()
            }
            Message::FocusSearch => text_input::focus(search_id()),
            Message::Do(Action(f)) => {
                f(&mut self.settings, self.st.get_mut(), &self.wake);
                self.poll()
            }
            Message::Wake => self.poll(),
            Message::Tick => {
                // pick up changes made elsewhere while nothing is waiting to be saved
                if self.settings == self.saved {
                    let disk = Settings::load();
                    if disk != self.saved {
                        self.settings = disk.clone();
                        self.saved = disk;
                        self.update_theme();
                    }
                }
                self.poll()
            }
            Message::Save(serial) => {
                if serial == self.save_serial {
                    self.save_now();
                }
                Task::none()
            }
            Message::Scrolled(y) => {
                self.scrolled = y > 0.5;
                Task::none()
            }
            Message::Close => {
                self.save_now();
                window::get_latest().and_then(window::close)
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            sys::wake_subscription(Message::Wake),
            iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick),
            keyboard::on_key_press(shortcut),
            event::listen_with(|ev, _status, _id| match ev {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Back)) => Some(Message::Back),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Forward)) => Some(Message::Forward),
                Event::Window(window::Event::CloseRequested) => Some(Message::Close),
                _ => None,
            }),
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        let p = palette();
        let page = {
            let mut st = self.st.borrow_mut();
            pages::show(self.page, &self.settings, &mut st, &self.wake)
        };

        let mut body = Column::new();
        if self.compositor_live == Some(false) {
            body = body.push(form::note("Saved. Restart Orchard for changes to take effect.", p.warning));
        }
        body = body.push(page);

        chrome::frame(
            self.sidebar(),
            chrome::toolbar(
                Some(chrome::nav_buttons(
                    (!self.back.is_empty()).then_some(Message::Back),
                    (!self.forward.is_empty()).then_some(Message::Forward),
                )),
                self.page.title(),
                None,
                self.scrolled,
            ),
            chrome::page_transition(
                self.transition_seq,
                self.transition_dir,
                chrome::scroll(form::page(body), content_id(), Message::Scrolled),
            ),
        )
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let p = palette();
        let search = controls::search_field(search_id(), "Search Settings", &self.search, Message::Search, None);

        let item = |pg: Page, selected: bool| {
            let (icon, color) = pg.icon();
            chrome::sidebar_item(
                pages::tile(icon, color, metrics::icon::SIDEBAR),
                pg.title(),
                selected,
                Selection::Accent,
                Message::Go(pg),
            )
        };

        let q = self.search.trim().to_lowercase();
        let mut list = Column::new();
        if !q.is_empty() {
            let hits: Vec<Page> = ALL_PAGES
                .iter()
                .copied()
                .filter(|pg| pg.title().to_lowercase().contains(&q) || pg.keywords().contains(&q))
                .collect();
            if hits.is_empty() {
                list = list.push(
                    container(text("No Results").size(13).color(p.text_secondary))
                        .center_x(Length::Fill)
                        .padding([metrics::space::XL + 4.0, 0.0]),
                );
            }
            for pg in hits {
                list = list.push(item(pg, self.page == pg));
            }
        } else {
            list = list.push(self.account_card());
            let active = self.page.sidebar_item();
            for (heading, section) in SIDEBAR.iter() {
                list = list.push(chrome::sidebar_heading(*heading));
                for &pg in *section {
                    list = list.push(item(pg, active == pg));
                }
            }
        }

        chrome::sidebar(Some(search), list)
    }

    /// The account card at the top of the sidebar (like the Apple Account entry).
    fn account_card(&self) -> Element<'_, Message> {
        let p = palette();
        let initials: String = self
            .user
            .0
            .split_whitespace()
            .filter_map(|w| w.chars().next())
            .take(2)
            .flat_map(char::to_uppercase)
            .collect();
        let avatar = container(text(initials).size(13).font(fonts::semibold()).color(iced::Color::WHITE))
            .center(34)
            .style(move |_| container::Style {
                background: Some(iced::Background::Gradient(iced::Gradient::Linear(
                    iced::gradient::Linear::new(iced::Radians(std::f32::consts::PI))
                        .add_stop(0.0, orchard_ui::palette::mix(p.accent, iced::Color::WHITE, 0.28))
                        .add_stop(1.0, p.accent),
                ))),
                border: style::no_border(17.0),
                ..Default::default()
            });
        button(
            row![
                avatar,
                column![
                    text(self.user.0.clone()).size(13).font(fonts::semibold()).color(p.text),
                    text(self.user.1.clone()).size(11).color(p.text_secondary),
                ]
                .spacing(1)
                .width(Length::Fill),
                orchard_ui::icons::symbol(orchard_ui::icons::Symbol::ChevronRight, 11.0, p.text_tertiary),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .on_press(Message::Go(Page::About))
        .padding([8, 8])
        .width(Length::Fill)
        .style(style::row(9.0))
        .into()
    }
}

/// ⌘/Ctrl shortcuts: W closes, [ ] navigate, F searches.
fn shortcut(key: keyboard::Key, mods: keyboard::Modifiers) -> Option<Message> {
    if !(mods.logo() || mods.control()) {
        return None;
    }
    match key.as_ref() {
        keyboard::Key::Character("w") => Some(Message::Close),
        keyboard::Key::Character("[") => Some(Message::Back),
        keyboard::Key::Character("]") => Some(Message::Forward),
        keyboard::Key::Character("f") => Some(Message::FocusSearch),
        _ => None,
    }
}

/// Full name (GECOS) and login of the current user, read without subprocesses.
fn current_user() -> (String, String) {
    let login = std::env::var("USER").unwrap_or_default();
    let gecos = std::fs::read_to_string("/etc/passwd").ok().and_then(|pw| {
        pw.lines().find_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            (f.len() > 4 && f[0] == login).then(|| f[4].split(',').next().unwrap_or("").trim().to_string())
        })
    });
    let name = gecos.filter(|g| !g.is_empty()).unwrap_or_else(|| login.clone());
    (name, "Local Account".into())
}

fn main() -> iced::Result {
    let arg1 = std::env::args().nth(1);
    if matches!(arg1.as_deref(), Some("--about-device" | "--about" | "about-device")) {
        return about::run();
    }
    let start_page = arg1.as_deref().and_then(Page::from_arg).unwrap_or(Page::Appearance);

    iced::application("System Settings", App::update, App::view)
        .theme(|_| orchard_ui::theme())
        .default_font(fonts::text())
        .antialiasing(true)
        .subscription(App::subscription)
        .exit_on_close_request(false)
        .window(orchard_ui::window_settings("org.orchard.settings", (940.0, 720.0), Some((760.0, 520.0))))
        .run_with(move || App::new(start_page))
}
