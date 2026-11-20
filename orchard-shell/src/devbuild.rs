//! Menu bar "Rebuild" item, shown only when the shell runs out of an Orchard source
//! checkout (`<repo>/target/release/orchard-shell`, e.g. after `dist/install.sh --dev`).
//!
//! Nothing is built automatically: the item shows when edited sources are newer than
//! the binaries built from them, and "Rebuild Desktop" runs `dist/dev.sh go`, which
//! builds the workspace at idle priority and swaps in whatever changed.
//!
//! "Changes waiting" comes from cargo's dep-info files (`target/release/<bin>.d`), so
//! it lists exactly the sources each binary was built from.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

/// (binary, what it is called in the menu)
const BINARIES: [(&str, &str, &str); 4] = [
    ("orchard-shell", "Menu Bar & Dock", "Menüleiste & Dock"),
    ("orchard", "Compositor", "Compositor"),
    ("orchard-settings", "System Settings", "Systemeinstellungen"),
    ("orchard-lock", "Lock Screen", "Sperrbildschirm"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Idle,
    Running,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DevBuild {
    pub root: PathBuf,
    /// menu names of the binaries whose sources changed since they were built
    pub pending: Vec<&'static str>,
    pub build: Build,
    /// the running compositor's binary was replaced: log out (or use nested) to apply
    pub compositor_stale: bool,
    german: bool,
}

impl DevBuild {
    /// `None` outside a source checkout: the item is hidden.
    pub fn detect() -> Option<DevBuild> {
        let root = std::env::var_os("ORCHARD_SRC").map(PathBuf::from).or_else(|| {
            let exe = std::env::current_exe().ok()?;
            Some(exe.parent()?.parent()?.parent()?.to_path_buf())
        })?;
        if !root.join("dist/dev.sh").is_file() || !root.join("Cargo.toml").is_file() {
            return None;
        }
        let german = std::env::var("LANG").map(|l| l.starts_with("de")).unwrap_or(false);
        let mut d = DevBuild {
            root,
            pending: Vec::new(),
            build: Build::Idle,
            compositor_stale: false,
            german,
        };
        d.refresh();
        Some(d)
    }

    fn t(&self, en: &'static str, de: &'static str) -> &'static str {
        if self.german {
            de
        } else {
            en
        }
    }

    /// Re-read pending changes and build state. Returns true if anything changed.
    pub fn refresh(&mut self) -> bool {
        let release = self.root.join("target/release");
        let pending: Vec<&'static str> = BINARIES
            .iter()
            .filter(|(bin, _, _)| self.root.join(bin).join("Cargo.toml").is_file())
            .filter(|(bin, _, _)| sources_newer(&self.root, &release, bin))
            .map(|(_, en, de)| if self.german { *de } else { *en })
            .collect();
        let build = read_status(&state_dir().join("dev-status"));
        let stale = compositor_stale();
        let changed = pending != self.pending || build != self.build || stale != self.compositor_stale;
        self.pending = pending;
        self.build = build;
        self.compositor_stale = stale;
        changed
    }

    /// Something worth the user's attention: draw the badge.
    pub fn badge(&self) -> Option<[u8; 4]> {
        match self.build {
            Build::Failed => Some([255, 69, 58, 255]),
            Build::Running => None,
            Build::Idle if !self.pending.is_empty() => Some([255, 159, 10, 255]),
            Build::Idle if self.compositor_stale => Some([10, 132, 255, 255]),
            Build::Idle => None,
        }
    }

    pub fn log_path(&self) -> PathBuf {
        state_dir().join("dev-build.log")
    }

    pub fn menu(&self) -> Vec<crate::menu::MenuItem> {
        use crate::menu::{Action, MenuItem};
        let t = |en, de| self.t(en, de);
        let q = |p: &Path| shell_quote(&p.to_string_lossy());
        let dev_sh = q(&self.root.join("dist/dev.sh"));
        let log = q(&self.log_path());

        let mut v = Vec::new();
        let status = match self.build {
            Build::Running => t("Building…", "Wird gebaut …").to_string(),
            Build::Failed => t("Last build failed", "Letzter Build fehlgeschlagen").to_string(),
            Build::Idle if self.pending.is_empty() => t("Up to date", "Aktuell").to_string(),
            Build::Idle => format!("{} {}", t("Changed:", "Geändert:"), self.pending.join(", ")),
        };
        v.push(MenuItem::disabled(status));
        v.push(MenuItem::separator());
        if self.build == Build::Running {
            v.push(MenuItem::disabled(t("Rebuild Desktop", "Desktop neu bauen")));
        } else {
            v.push(MenuItem::new(
                t("Rebuild Desktop", "Desktop neu bauen"),
                Action::System(format!("{dev_sh} go")),
            ));
        }
        if self.log_path().is_file() {
            v.push(MenuItem::new(
                t("Show Build Log", "Build-Protokoll anzeigen"),
                Action::Launch(format!("konsole -e less +G {log}")),
            ));
        }
        if self.compositor_stale {
            v.push(MenuItem::separator());
            v.push(MenuItem::disabled(t(
                "Compositor rebuilt: applies at next login",
                "Compositor neu gebaut: gilt ab nächster Anmeldung",
            )));
            v.push(MenuItem::new(
                t("Preview in Nested Window", "In verschachteltem Fenster testen"),
                Action::Launch(format!("{} run", q(&self.root.join("dist/nested.sh")))),
            ));
            v.push(MenuItem::new(t("Log Out to Apply", "Abmelden und anwenden"), Action::LogOut));
        }
        v
    }
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("orchard")
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Any in-repo source listed in `<bin>.d` is newer than `<bin>` (or it was never built).
fn sources_newer(root: &Path, release: &Path, bin: &str) -> bool {
    let Some(built) = mtime(&release.join(bin)) else { return true };
    let Ok(dep) = std::fs::read_to_string(release.join(format!("{bin}.d"))) else {
        return true;
    };
    dep_info_sources(&dep)
        .into_iter()
        .filter(|p| p.starts_with(root))
        .any(|p| mtime(&p).is_none_or(|m| m > built))
}

/// Parse a Makefile-style dep-info file: `target: dep dep\ with\ space ...`.
fn dep_info_sources(dep: &str) -> Vec<PathBuf> {
    let Some(line) = dep.lines().next() else { return Vec::new() };
    let Some((_, deps)) = line.split_once(": ") else { return Vec::new() };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = deps.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' => {
                if !cur.is_empty() {
                    out.push(PathBuf::from(std::mem::take(&mut cur)));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(PathBuf::from(cur));
    }
    out
}

/// `dist/dev.sh` writes `building <pid>`, `ok` or `failed`.
fn read_status(path: &Path) -> Build {
    let s = std::fs::read_to_string(path).unwrap_or_default();
    let mut it = s.split_whitespace();
    match it.next() {
        Some("building") => {
            let alive = it
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists());
            if alive {
                Build::Running
            } else {
                Build::Idle
            }
        }
        Some("failed") => Build::Failed,
        _ => Build::Idle,
    }
}

/// The compositor that started this shell runs a binary that has since been rebuilt.
fn compositor_stale() -> bool {
    let ppid = std::os::unix::process::parent_id();
    let comm = std::fs::read_to_string(format!("/proc/{ppid}/comm")).unwrap_or_default();
    if comm.trim() != "orchard" {
        return false;
    }
    std::fs::read_link(format!("/proc/{ppid}/exe"))
        .map(|p| p.to_string_lossy().ends_with(" (deleted)"))
        .unwrap_or(false)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dep_info_with_escaped_spaces() {
        let d = "/r/target/release/x: /r/a.rs /r/my\\ dir/b.rs /r/c.frag\n\n/r/a.rs:\n";
        assert_eq!(
            dep_info_sources(d),
            vec![PathBuf::from("/r/a.rs"), PathBuf::from("/r/my dir/b.rs"), PathBuf::from("/r/c.frag")]
        );
    }

    #[test]
    fn quotes_for_sh() {
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
    }
}
