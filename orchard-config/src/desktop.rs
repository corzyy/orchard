//! Minimal `.desktop` file handling shared by the compositor (autostart) and
//! System Settings (Login Items, Default Apps).

use std::path::{Path, PathBuf};

/// The `[Desktop Entry]` group of a desktop file, as key/value pairs.
#[derive(Debug, Clone, Default)]
pub struct DesktopEntry {
    pub path: PathBuf,
    /// file name, e.g. `org.kde.konsole.desktop`
    pub id: String,
    pub keys: Vec<(String, String)>,
}

impl DesktopEntry {
    pub fn parse(path: &Path) -> Option<DesktopEntry> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut keys = Vec::new();
        let mut in_main = false;
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                in_main = l == "[Desktop Entry]";
                continue;
            }
            if !in_main || l.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = l.split_once('=') {
                keys.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        Some(DesktopEntry {
            id: path.file_name()?.to_string_lossy().into_owned(),
            path: path.to_path_buf(),
            keys,
        })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn flag(&self, key: &str) -> bool {
        self.get(key).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false)
    }

    /// Name, localised for `$LANG` when available.
    pub fn name(&self) -> String {
        let lang = std::env::var("LANG").unwrap_or_default();
        let short = lang.split(['_', '.']).next().unwrap_or_default();
        let full = lang.split('.').next().unwrap_or_default();
        self.get(&format!("Name[{full}]"))
            .or_else(|| self.get(&format!("Name[{short}]")))
            .or_else(|| self.get("Name"))
            .unwrap_or(&self.id)
            .to_string()
    }

    pub fn hidden(&self) -> bool {
        self.flag("Hidden") || self.get("X-GNOME-Autostart-enabled").map(|v| v == "false").unwrap_or(false)
    }

    /// `X-systemd-skip=true`: a native systemd user unit starts this program in
    /// systemd-managed sessions, so the autostart entry is only a fallback.
    pub fn systemd_skip(&self) -> bool {
        self.flag("X-systemd-skip")
    }

    pub fn no_display(&self) -> bool {
        self.flag("NoDisplay")
    }

    fn list(&self, key: &str) -> Vec<String> {
        self.get(key)
            .map(|v| v.split(';').filter(|s| !s.is_empty()).map(String::from).collect())
            .unwrap_or_default()
    }

    pub fn mime_types(&self) -> Vec<String> {
        self.list("MimeType")
    }

    pub fn categories(&self) -> Vec<String> {
        self.list("Categories")
    }

    /// Exec line without field codes (%u, %F, …).
    pub fn command(&self) -> Option<String> {
        let exec = self.get("Exec")?;
        let mut out = String::new();
        let mut chars = exec.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '%' {
                match chars.next() {
                    Some('%') => out.push('%'),
                    _ => {}
                }
            } else {
                out.push(c);
            }
        }
        Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    /// Should this autostart entry run in an Orchard session?
    pub fn autostarts_in_orchard(&self) -> bool {
        if self.hidden() {
            return false;
        }
        if self.list("NotShowIn").iter().any(|d| d == "Orchard") {
            return false;
        }
        // Orchard ships its own built-in polkit authentication agent in orchard-shell;
        // skip external DE polkit agents so they don't race to claim the session.
        if self.command().map(|c| c.contains("polkit")).unwrap_or(false)
            && std::env::var_os("ORCHARD_NO_POLKIT").is_none()
        {
            return false;
        }
        let only = self.list("OnlyShowIn");
        if !only.is_empty() && !only.iter().any(|d| d == "Orchard") {
            return false;
        }
        if let Some(try_exec) = self.get("TryExec") {
            if !command_exists(try_exec) {
                return false;
            }
        }
        true
    }
}

fn command_exists(cmd: &str) -> bool {
    if cmd.starts_with('/') {
        return Path::new(cmd).exists();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn user_autostart_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("autostart")
}

fn system_autostart_dirs() -> Vec<PathBuf> {
    std::env::var("XDG_CONFIG_DIRS")
        .unwrap_or_else(|_| "/etc/xdg".into())
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|d| PathBuf::from(d).join("autostart"))
        .collect()
}

fn desktop_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|e| e == "desktop").unwrap_or(false)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[derive(Debug, Clone)]
pub struct AutostartItem {
    pub entry: DesktopEntry,
    /// a file with this id exists in ~/.config/autostart
    pub in_user_dir: bool,
    /// a file with this id exists in a system autostart dir
    pub in_system_dir: bool,
}

/// All autostart entries, with user files overriding system ones (XDG spec).
pub fn autostart_items() -> Vec<AutostartItem> {
    let mut items: Vec<AutostartItem> = Vec::new();
    for dir in system_autostart_dirs().iter().rev() {
        for p in desktop_files(dir) {
            if let Some(e) = DesktopEntry::parse(&p) {
                items.retain(|i| i.entry.id != e.id);
                items.push(AutostartItem {
                    entry: e,
                    in_user_dir: false,
                    in_system_dir: true,
                });
            }
        }
    }
    for p in desktop_files(&user_autostart_dir()) {
        if let Some(e) = DesktopEntry::parse(&p) {
            let in_system = items.iter().any(|i| i.entry.id == e.id);
            items.retain(|i| i.entry.id != e.id);
            items.push(AutostartItem {
                entry: e,
                in_user_dir: true,
                in_system_dir: in_system,
            });
        }
    }
    items
}

/// Directories holding application `.desktop` files, most important first.
pub fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("applications")];
    let data = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in data.split(':').filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(d).join("applications"));
    }
    for extra in ["/var/lib/flatpak/exports/share/applications", "/usr/share/applications"] {
        let p = PathBuf::from(extra);
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    }
    let fp = home().join(".local/share/flatpak/exports/share/applications");
    if !dirs.contains(&fp) {
        dirs.push(fp);
    }
    dirs
}

/// Installed applications (first occurrence of each id wins).
pub fn applications() -> Vec<DesktopEntry> {
    let mut out: Vec<DesktopEntry> = Vec::new();
    for dir in application_dirs() {
        for p in desktop_files(&dir) {
            if let Some(e) = DesktopEntry::parse(&p) {
                if e.get("Type").map(|t| t == "Application").unwrap_or(true) && !out.iter().any(|o| o.id == e.id) {
                    out.push(e);
                }
            }
        }
    }
    out.sort_by_key(|e| e.name().to_lowercase());
    out
}
