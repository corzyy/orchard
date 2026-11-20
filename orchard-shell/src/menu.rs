//! Drop-down menus (menu bar menus, Dock context menus).

use std::time::{Duration, Instant};

use tiny_skia::{BlendMode, FillRule, Pixmap, Transform};

use crate::{
    anim::{self, ease_out_cubic, scale_alpha},
    draw::{fill_rounded, paint, rounded_rect, stroke_rounded},
    text::{Fonts, Weight},
    theme::Theme,
};

const OPEN_ANIM: Duration = Duration::from_millis(85);

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Launch(String),
    ActivateApp(String),
    ActivateWindow(u64),
    QuitApp(String),
    HideApp(String),
    Wm(&'static str),
    System(String),
    TogglePin(String),
    OpenTrash,
    LogOut,
    /// macOS-style confirmation dialog (Shut Down, Restart, Log Out, Sleep)
    Confirm(crate::dialog::DialogKind),
    /// click an item of an app's global menu
    DbusMenu { service: String, path: String, id: i32 },
    /// run a GTK app's menu action (org.gtk.Actions)
    GtkMenu { bus: String, act: crate::gtkmenu::Activate },
    /// desktop icon actions
    DesktopOpen(std::path::PathBuf),
    DesktopTrash(std::path::PathBuf),
    DesktopNewFolder,
    DesktopSort(crate::desktop::SortBy),
    CopyText(String),
    SetTilingMode(orchard_config::TilingMode),
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub shortcut: String,
    pub action: Option<Action>,
    pub separator: bool,
    /// opens a submenu when hovered
    pub submenu: Vec<MenuItem>,
    /// checkbox / radio item
    pub checked: Option<bool>,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: Action) -> Self {
        MenuItem {
            label: label.into(),
            shortcut: String::new(),
            action: Some(action),
            separator: false,
            submenu: Vec::new(),
            checked: None,
        }
    }
    pub fn with_shortcut(mut self, s: &str) -> Self {
        self.shortcut = s.into();
        self
    }
    pub fn with_checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }
    pub fn disabled(label: impl Into<String>) -> Self {
        MenuItem {
            label: label.into(),
            shortcut: String::new(),
            action: None,
            separator: false,
            submenu: Vec::new(),
            checked: None,
        }
    }
    pub fn separator() -> Self {
        MenuItem {
            label: String::new(),
            shortcut: String::new(),
            action: None,
            separator: true,
            submenu: Vec::new(),
            checked: None,
        }
    }
}

pub const ITEM_H: f32 = 22.0;
pub const SEP_H: f32 = 9.0;
const PAD: f32 = 5.0;
const FONT: f32 = 13.0;

#[derive(Debug, Clone)]
pub struct Menu {
    pub items: Vec<MenuItem>,
    /// top-left in surface (output) logical coords
    pub x: f32,
    pub y: f32,
    pub hovered: Option<usize>,
    /// open submenu (of item `child_of`)
    pub child: Option<Box<Menu>>,
    pub child_of: Option<usize>,
    pub opened: Instant,
}

impl Menu {
    pub fn new(items: Vec<MenuItem>, x: f32, y: f32) -> Menu {
        Menu {
            items,
            x,
            y,
            hovered: None,
            child: None,
            child_of: None,
            opened: Instant::now(),
        }
    }

    pub fn open_progress(&self) -> f32 {
        if anim::reduced() {
            return 1.0;
        }
        let t = (self.opened.elapsed().as_secs_f32() / OPEN_ANIM.as_secs_f32()).clamp(0.0, 1.0);
        ease_out_cubic(t)
    }

    pub fn is_animating(&self) -> bool {
        if anim::reduced() {
            return false;
        }
        self.opened.elapsed() < OPEN_ANIM || self.child.as_ref().map(|c| c.is_animating()).unwrap_or(false)
    }

    fn hoverable(it: &MenuItem) -> bool {
        !it.separator && (it.action.is_some() || !it.submenu.is_empty())
    }

    /// Top of item `idx` (logical).
    fn item_top(&self, idx: usize) -> f32 {
        let mut top = self.y + PAD;
        for it in self.items.iter().take(idx) {
            top += if it.separator { SEP_H } else { ITEM_H };
        }
        top
    }

    /// Pointer moved: update hover state and open / close submenus.
    /// Returns true if something changed.
    pub fn pointer_moved(&mut self, fonts: &Fonts, x: f32, y: f32, surface_w: f32, surface_h: f32) -> bool {
        if let Some(child) = self.child.as_mut() {
            if child.contains_any(fonts, x, y) {
                return child.pointer_moved(fonts, x, y, surface_w, surface_h);
            }
        }
        let h = self.hit(fonts, x, y);
        if h.is_none() && self.child.is_some() {
            // moving towards the submenu across a gap keeps it open
            return false;
        }
        let mut changed = h != self.hovered;
        self.hovered = h;
        match h {
            Some(i) if !self.items[i].submenu.is_empty() => {
                if self.child_of != Some(i) {
                    let (w, _) = self.size(fonts);
                    let mut child = Menu::new(self.items[i].submenu.clone(), self.x + w - 4.0, self.item_top(i) - PAD);
                    // open to the left if there is no room on the right
                    let (cw, _) = child.size(fonts);
                    if child.x + cw > surface_w - 4.0 {
                        child.x = self.x - cw + 4.0;
                    }
                    child.y = child.y.min(surface_h - child.size(fonts).1 - 4.0).max(0.0);
                    self.child = Some(Box::new(child));
                    self.child_of = Some(i);
                    changed = true;
                }
            }
            Some(_) => {
                if self.child.take().is_some() {
                    self.child_of = None;
                    changed = true;
                }
            }
            None => {}
        }
        changed
    }

    /// The action of the item under the pointer, searching open submenus first.
    pub fn action_at(&self, fonts: &Fonts, x: f32, y: f32) -> Option<Action> {
        if let Some(child) = &self.child {
            if child.contains_any(fonts, x, y) {
                return child.action_at(fonts, x, y);
            }
        }
        let i = self.hit(fonts, x, y)?;
        self.items[i].action.clone()
    }

    /// Inside this menu or any open submenu.
    pub fn contains_any(&self, fonts: &Fonts, x: f32, y: f32) -> bool {
        self.contains(fonts, x, y) || self.child.as_ref().map(|c| c.contains_any(fonts, x, y)).unwrap_or(false)
    }

    /// Rects of this menu and its open submenus with fade alpha (for the frosted backdrop).
    pub fn backdrop_rects(&self, fonts: &Fonts) -> Vec<(f32, f32, f32, f32, f32)> {
        let (w, h) = self.size(fonts);
        let p = self.open_progress();
        let dy = -(1.0 - p) * 3.0;
        let mut v = vec![(self.x, self.y + dy, w, h, p)];
        if let Some(c) = &self.child {
            v.extend(c.backdrop_rects(fonts));
        }
        v
    }

    pub fn size(&self, fonts: &Fonts) -> (f32, f32) {
        let mut w: f32 = 180.0;
        let mut h = 2.0 * PAD;
        for it in &self.items {
            if it.separator {
                h += SEP_H;
            } else {
                let lw = fonts.measure(&it.label, FONT, Weight::Regular)
                    + fonts.measure(&it.shortcut, FONT, Weight::Regular)
                    + if it.submenu.is_empty() { 64.0 } else { 84.0 };
                w = w.max(lw);
                h += ITEM_H;
            }
        }
        (w, h)
    }

    /// Clamp the menu inside the surface.
    pub fn clamp(&mut self, fonts: &Fonts, surface_w: f32, surface_h: f32) {
        let (w, h) = self.size(fonts);
        self.x = self.x.min(surface_w - w - 4.0).max(4.0);
        self.y = self.y.min(surface_h - h - 4.0).max(0.0);
    }

    pub fn hit(&self, fonts: &Fonts, x: f32, y: f32) -> Option<usize> {
        let (w, _) = self.size(fonts);
        if x < self.x || x > self.x + w {
            return None;
        }
        let mut top = self.y + PAD;
        for (i, it) in self.items.iter().enumerate() {
            let h = if it.separator { SEP_H } else { ITEM_H };
            if y >= top && y < top + h {
                return Self::hoverable(it).then_some(i);
            }
            top += h;
        }
        None
    }

    pub fn contains(&self, fonts: &Fonts, x: f32, y: f32) -> bool {
        let (w, h) = self.size(fonts);
        x >= self.x && x <= self.x + w && y >= self.y && y <= self.y + h
    }

    pub fn paint(&self, pm: &mut Pixmap, scale: f32, fonts: &Fonts, theme: &Theme) {
        let s = scale;
        let (w, h) = self.size(fonts);
        let prog = self.open_progress();
        let y_off = -(1.0 - prog) * 3.0;
        let my = self.y + y_off;
        // soft shadow
        for (i, a) in [(6.0, 12u8), (3.0, 20), (1.0, 30)] {
            let sa = (a as f32 * prog).round() as u8;
            fill_rounded(pm, (self.x - i) * s, (my - i + 3.0) * s, (w + 2.0 * i) * s, (h + 2.0 * i) * s, (8.0 + i) * s, [0, 0, 0, sa]);
        }
        // Replace the interior rather than blending over the shadow so the
        // compositor's frosted-glass blur stays clear behind the menu tint.
        if let Some(p) = rounded_rect(self.x * s, my * s, w * s, h * s, 8.0 * s) {
            let mut bg = paint(scale_alpha(theme.menu_bg, prog));
            bg.blend_mode = BlendMode::Source;
            pm.fill_path(&p, &bg, FillRule::Winding, Transform::identity(), None);
        }
        stroke_rounded(pm, self.x * s + 0.5, my * s + 0.5, w * s - 1.0, h * s - 1.0, 8.0 * s, 1.0, scale_alpha(theme.menu_border, prog));
        let mut top = my + PAD;
        let px = FONT * s;
        for (i, it) in self.items.iter().enumerate() {
            if it.separator {
                fill_rounded(pm, (self.x + 10.0) * s, (top + SEP_H / 2.0) * s, (w - 20.0) * s, 1.0 * s, 0.0, scale_alpha(theme.menu_separator, prog));
                top += SEP_H;
                continue;
            }
            let selected = (self.hovered == Some(i) && Self::hoverable(it)) || self.child_of == Some(i);
            if selected {
                fill_rounded(pm, (self.x + PAD) * s, top * s, (w - 2.0 * PAD) * s, ITEM_H * s, 5.0 * s, scale_alpha(theme.menu_selection, prog));
            }
            let color = scale_alpha(
                if !Self::hoverable(it) {
                    theme.menu_text_disabled
                } else if selected {
                    theme.menu_selection_text
                } else {
                    theme.menu_text
                },
                prog,
            );
            let baseline = fonts.baseline_centered(top * s, ITEM_H * s, px, Weight::Regular);
            if it.checked == Some(true) {
                fonts.draw(pm, "✓", (self.x + 6.0) * s, baseline, px, Weight::Regular, color);
            }
            fonts.draw(pm, &it.label, (self.x + 20.0) * s, baseline, px, Weight::Regular, color);
            if !it.submenu.is_empty() {
                let aw = fonts.measure("›", px * 1.2, Weight::Bold);
                fonts.draw(pm, "›", (self.x + w - 12.0) * s - aw, baseline, px * 1.2, Weight::Bold, color);
            }
            if !it.shortcut.is_empty() {
                let sw = fonts.measure(&it.shortcut, px, Weight::Regular);
                let mut c = color;
                c[3] = (c[3] as u32 * 150 / 255) as u8;
                fonts.draw(pm, &it.shortcut, (self.x + w - 14.0) * s - sw, baseline, px, Weight::Regular, c);
            }
            top += ITEM_H;
        }
        if let Some(child) = &self.child {
            child.paint(pm, scale, fonts, theme);
        }
    }
}
