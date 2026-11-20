//! General › Software Update: checks dnf (system packages) and Flatpak
//! (apps & runtimes) for updates, lists exactly what will change, and
//! installs everything with live progress — laid out like macOS.

use std::{
    collections::HashMap,
    io::Read,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

use iced::widget::{column, container, row, text, Column, Space};
use iced::{Alignment, Length};
use orchard_ui::{controls, form, metrics, palette, palette::from_rgb};

use crate::{
    pages::{act, tile, El},
    sys::{self, Bg, Wake},
};

// ---------------------------------------------------------------- Data

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pkg {
    /// Display name (package name, or Flatpak app name).
    pub name: String,
    /// Secondary line: rpm summary or Flatpak application ID.
    pub detail: String,
    /// Installed version (may be empty, e.g. for Flatpak runtimes).
    pub old: String,
    pub new: String,
    /// Download size in bytes, if known.
    pub size: Option<u64>,
    /// Repository / remote the update comes from.
    pub origin: String,
}

/// Result of checking one backend. `None` = tool not installed.
type Source = Option<Result<Vec<Pkg>, String>>;

#[derive(Clone, Default)]
pub struct UpdateInfo {
    dnf: Source,
    flatpak: Source,
    checked: Option<SystemTime>,
}

impl UpdateInfo {
    fn list(src: &Source) -> &[Pkg] {
        match src {
            Some(Ok(v)) => v,
            _ => &[],
        }
    }
    fn dnf_pkgs(&self) -> &[Pkg] {
        Self::list(&self.dnf)
    }
    fn flatpak_pkgs(&self) -> &[Pkg] {
        Self::list(&self.flatpak)
    }
    fn count(&self) -> usize {
        self.dnf_pkgs().len() + self.flatpak_pkgs().len()
    }
    fn total_size(&self) -> u64 {
        self.dnf_pkgs().iter().chain(self.flatpak_pkgs()).filter_map(|p| p.size).sum()
    }
    fn needs_restart(&self) -> bool {
        self.dnf_pkgs().iter().any(|p| restart_pkg(&p.name))
    }
}

/// Packages whose update only takes full effect after a reboot.
fn restart_pkg(name: &str) -> bool {
    let base = name.rsplit_once('.').map(|(b, _)| b).unwrap_or(name);
    base.starts_with("kernel")
        || base.starts_with("linux-firmware")
        || matches!(base, "glibc" | "systemd" | "systemd-libs" | "dbus" | "dbus-broker" | "mesa-dri-drivers")
        || base.starts_with("nvidia")
        || base.starts_with("akmod-")
}

#[derive(Default)]
struct Progress {
    /// What is running ("System packages", "Apps").
    phase: String,
    /// Last line of tool output.
    line: String,
    /// Overall fraction 0..1, if the tools report counts.
    frac: Option<f32>,
}

struct Install {
    progress: Arc<Mutex<Progress>>,
    done: Bg<Result<(), String>>,
    restart: bool,
}

#[derive(Default)]
pub struct UpdatesState {
    info: Bg<UpdateInfo>,
    /// Next check should refresh repository metadata (user pressed Check Now).
    force_refresh: bool,
    show_details: bool,
    install: Option<Install>,
    /// Outcome of the last install: Ok(restart needed) / Err(message).
    result: Option<Result<bool, String>>,
}

// ---------------------------------------------------------------- Checking

/// Parse `name\tarch\tevr\tdownloadsize\trepoid\tsummary` lines from
/// `dnf repoquery`, filling in installed versions from `installed`.
fn parse_dnf(out: &str, installed: &HashMap<String, String>) -> Vec<Pkg> {
    let mut v: Vec<Pkg> = out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 5 || f[0].is_empty() {
                return None;
            }
            let key = format!("{}.{}", f[0], f[1]);
            Some(Pkg {
                old: installed.get(&key).cloned().unwrap_or_default(),
                name: key,
                new: f[2].to_string(),
                size: f[3].trim().parse().ok(),
                origin: f[4].to_string(),
                detail: f.get(5).map(|s| s.trim().to_string()).unwrap_or_default(),
            })
        })
        .collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v.dedup_by(|a, b| a.name == b.name);
    v
}

/// Fallback parser for `dnf check-update` / `dnf list --upgrades` tables
/// (`name.arch  version  repo`).
fn parse_dnf_table(out: &str, installed: &HashMap<String, String>) -> Vec<Pkg> {
    let mut v: Vec<Pkg> = out
        .lines()
        .take_while(|l| !l.starts_with("Obsoleting"))
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() != 3 || !f[0].contains('.') || !f[1].chars().next()?.is_ascii_digit() {
                return None;
            }
            Some(Pkg {
                old: installed.get(f[0]).cloned().unwrap_or_default(),
                name: f[0].to_string(),
                new: f[1].to_string(),
                size: None,
                origin: f[2].to_string(),
                detail: String::new(),
            })
        })
        .collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

fn check_dnf(refresh: bool) -> Source {
    if !sys::which("dnf") {
        return None;
    }
    let installed: HashMap<String, String> = sys::output(
        "rpm",
        &["-qa", "--qf", "%{NAME}.%{ARCH}\t%|EPOCH?{%{EPOCH}:}:{}|%{VERSION}-%{RELEASE}\n"],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| l.split_once('\t'))
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();

    let qf = "%{name}\t%{arch}\t%{evr}\t%{downloadsize}\t%{repoid}\t%{summary}\n";
    let mut args = vec!["repoquery", "--upgrades", "--latest-limit=1", "-q", "--qf", qf];
    if refresh {
        args.push("--refresh");
    }
    match sys::run("dnf", &args) {
        Ok(out) => Some(Ok(parse_dnf(&out, &installed))),
        Err(first_err) => {
            // Older dnf / unusual setups: `check-update` exits 100 when updates exist.
            let mut cmd = Command::new("dnf");
            cmd.args(["check-update", "-q"]).env("LC_ALL", "C");
            if refresh {
                cmd.arg("--refresh");
            }
            match cmd.output() {
                Ok(o) if matches!(o.status.code(), Some(0) | Some(100)) => {
                    Some(Ok(parse_dnf_table(&String::from_utf8_lossy(&o.stdout), &installed)))
                }
                _ => Some(Err(first_err)),
            }
        }
    }
}

/// Parse a Flatpak human size such as "57.5 MB", "1,2 GB", "812 kB".
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().replace('\u{a0}', " ");
    let (num, unit) = s.split_once(' ').unwrap_or((&s, "B"));
    let n: f64 = num.replace(',', ".").parse().ok()?;
    let mul = match unit.trim().to_ascii_lowercase().as_str() {
        "b" | "bytes" => 1.0,
        "kb" => 1e3,
        "kib" => 1024.0,
        "mb" => 1e6,
        "mib" => 1024.0 * 1024.0,
        "gb" => 1e9,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * mul) as u64)
}

/// Parse `flatpak remote-ls --updates --columns=ref,name,version,download-size,origin`.
fn parse_flatpak(out: &str, installed: &HashMap<String, String>) -> Vec<Pkg> {
    let mut v: Vec<(bool, Pkg)> = out
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 5 || !f[0].contains('/') {
                return None;
            }
            let r#ref = f[0];
            let parts: Vec<&str> = r#ref.split('/').collect();
            let is_app = parts.first() == Some(&"app");
            let id = parts.get(1).copied().unwrap_or(r#ref);
            let branch = parts.get(3).copied().unwrap_or_default();
            let name = if f[1].trim().is_empty() { id.to_string() } else { f[1].trim().to_string() };
            let ver = |v: &str| if v.trim().is_empty() { branch.to_string() } else { v.trim().to_string() };
            let old = installed.get(r#ref).map(|s| ver(s)).unwrap_or_default();
            Some((
                is_app,
                Pkg {
                    name,
                    detail: id.to_string(),
                    old,
                    new: ver(f[2]),
                    size: parse_size(f[3]),
                    origin: f[4].trim().to_string(),
                },
            ))
        })
        .collect();
    // Apps first, then runtimes; alphabetical within each.
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
    v.dedup_by(|a, b| a.1.detail == b.1.detail && a.1.new == b.1.new);
    v.into_iter().map(|(_, p)| p).collect()
}

fn check_flatpak() -> Source {
    if !sys::which("flatpak") {
        return None;
    }
    let installed: HashMap<String, String> = sys::output("flatpak", &["list", "--columns=ref,version"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (r, v) = l.split_once('\t').unwrap_or((l, ""));
            Some((r.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    Some(
        sys::run("flatpak", &["remote-ls", "--updates", "--columns=ref,name,version,download-size,origin"])
            .map(|out| parse_flatpak(&out, &installed)),
    )
}

fn check(refresh: bool) -> UpdateInfo {
    // Both backends hit the network; run them in parallel.
    let fp = std::thread::spawn(check_flatpak);
    let dnf = check_dnf(refresh);
    UpdateInfo {
        dnf,
        flatpak: fp.join().unwrap_or(None),
        checked: Some(SystemTime::now()),
    }
}

// ---------------------------------------------------------------- Installing

fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").map(|m| m.uid() == 0).unwrap_or(false)
}

/// Find an `n/m` counter in a line of dnf/flatpak output ("[ 3/40]", "Updating 1/2…").
fn parse_counter(line: &str) -> Option<f32> {
    line.split(|c: char| c.is_whitespace() || c == '[' || c == ']' || c == '…' || c == ':')
        .filter_map(|tok| {
            let (a, b) = tok.split_once('/')?;
            let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
            (b > 0 && a <= b).then(|| a as f32 / b as f32)
        })
        .next()
}

/// Run one updater, streaming its output into `progress`.
/// `span` is the slice of the overall progress bar this step covers.
fn run_streamed(
    cmd: &str,
    args: &[&str],
    phase: &str,
    span: (f32, f32),
    progress: &Arc<Mutex<Progress>>,
    wake: &Wake,
) -> Result<(), String> {
    if let Ok(mut p) = progress.lock() {
        p.phase = phase.to_string();
        p.line.clear();
        p.frac = Some(span.0);
    }
    wake.notify();

    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{cmd}: {e}"))?;

    let mut stderr = child.stderr.take();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });

    let mut last_line = String::new();
    if let Some(mut out) = child.stdout.take() {
        let mut buf = [0u8; 4096];
        let mut pending = Vec::new();
        let mut last_wake = Instant::now();
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            pending.extend_from_slice(&buf[..n]);
            // Tools redraw progress with '\r'; treat it as a line break too.
            while let Some(pos) = pending.iter().position(|&b| b == b'\n' || b == b'\r') {
                let line: Vec<u8> = pending.drain(..=pos).collect();
                let text = String::from_utf8_lossy(&line).trim().to_string();
                if text.is_empty() {
                    continue;
                }
                if let Ok(mut p) = progress.lock() {
                    if let Some(f) = parse_counter(&text) {
                        p.frac = Some(span.0 + (span.1 - span.0) * f);
                    }
                    p.line = text.clone();
                }
                last_line = text;
                if last_wake.elapsed() > Duration::from_millis(60) {
                    wake.notify();
                    last_wake = Instant::now();
                }
            }
        }
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    if cmd == "pkexec" {
        match status.code() {
            Some(126) => return Err("Authentication was cancelled.".into()),
            Some(127) => return Err("Not authorised to install system updates.".into()),
            _ => {}
        }
    }
    let msg = err
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(String::from)
        .unwrap_or(last_line);
    Err(if msg.is_empty() { format!("{phase}: {cmd} failed") } else { format!("{phase}: {msg}") })
}

fn install(info: &UpdateInfo, wake: &Wake) -> Install {
    let progress = Arc::new(Mutex::new(Progress::default()));
    let do_dnf = !info.dnf_pkgs().is_empty();
    let do_fp = !info.flatpak_pkgs().is_empty();
    let mut done = Bg::default();
    let p = progress.clone();
    let w = wake.clone();
    done.start(wake, move || {
        // Split the bar proportionally: system packages are usually the bulk.
        let mid = match (do_dnf, do_fp) {
            (true, true) => 0.75,
            (true, false) => 1.0,
            _ => 0.0,
        };
        let mut errors = Vec::new();
        if do_dnf {
            let r = if is_root() {
                run_streamed("dnf", &["upgrade", "-y"], "System packages", (0.0, mid), &p, &w)
            } else {
                run_streamed("pkexec", &["dnf", "upgrade", "-y"], "System packages", (0.0, mid), &p, &w)
            };
            if let Err(e) = r {
                errors.push(e);
            }
        }
        if do_fp {
            if let Err(e) = run_streamed(
                "flatpak",
                &["update", "-y", "--noninteractive"],
                "Apps",
                (mid, 1.0),
                &p,
                &w,
            ) {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("  ·  "))
        }
    });
    Install {
        progress,
        done,
        restart: info.needs_restart(),
    }
}

// ---------------------------------------------------------------- Formatting

fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} bytes")
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn ago(t: SystemTime) -> String {
    let s = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        _ => plural((s / 3600) as usize, "hour ago", "hours ago"),
    }
}

fn summary(info: &UpdateInfo) -> String {
    let mut parts = Vec::new();
    let (d, f) = (info.dnf_pkgs().len(), info.flatpak_pkgs().len());
    if d > 0 {
        parts.push(plural(d, "system package", "system packages"));
    }
    if f > 0 {
        parts.push(plural(f, "app or runtime", "apps & runtimes"));
    }
    let mut s = parts.join(", ");
    let size = info.total_size();
    if size > 0 {
        s.push_str(&format!(" · {}", fmt_size(size)));
    }
    s
}

// ---------------------------------------------------------------- UI

const GREEN: [u8; 3] = [0x30, 0xD1, 0x58];
const BLUE: [u8; 3] = [0x0A, 0x84, 0xFF];
const GRAY: [u8; 3] = [0x8E, 0x8E, 0x93];

/// Big status row at the top of the card: icon tile, title, subtitle and
/// trailing buttons.
fn status_row(icon: &str, color: [u8; 3], title: String, sub: String, trailing: Option<El>) -> El {
    let trailing: El = trailing.unwrap_or_else(|| Space::with_width(0).into());
    form::header_row(
        tile(icon, from_rgb(color), metrics::icon::HEADER),
        title,
        (!sub.is_empty()).then_some(sub),
        trailing,
    )
}

/// One package line in the Details list.
fn pkg_row(pk: &Pkg) -> El {
    let p = palette();
    let ver = if pk.old.is_empty() || pk.old == pk.new {
        pk.new.clone()
    } else {
        format!("{}  →  {}", pk.old, pk.new)
    };
    let meta = match pk.size {
        Some(s) if s > 0 => format!("{} · {}", fmt_size(s), pk.origin),
        _ => pk.origin.clone(),
    };
    row![
        column![
            text(pk.name.clone()).size(13).color(p.text),
            text(orchard_ui::text::truncate(&pk.detail, 60)).size(11).color(p.text_secondary),
        ]
        .spacing(2)
        .width(Length::FillPortion(3)),
        column![
            text(ver).size(12).color(p.text_secondary),
            text(meta).size(11).color(p.text_tertiary),
        ]
        .spacing(2)
        .align_x(Alignment::End)
        .width(Length::FillPortion(2)),
    ]
    .spacing(metrics::space::L)
    .align_y(Alignment::Center)
    .padding([metrics::ROW_PAD_V, metrics::ROW_INSET])
    .into()
}

fn details(info: &UpdateInfo) -> El {
    let mut col = Column::new();
    let dnf = info.dnf_pkgs();
    if !dnf.is_empty() {
        col = col
            .push(form::section(format!("System Packages ({})", dnf.len())))
            .push(form::group(dnf.iter().map(pkg_row)));
    }
    let fp = info.flatpak_pkgs();
    if !fp.is_empty() {
        col = col
            .push(form::section(format!("Flatpak Apps & Runtimes ({})", fp.len())))
            .push(form::group(fp.iter().map(pkg_row)));
    }
    col.into()
}

fn check_now(enabled: bool) -> El {
    controls::push_button(
        "Check Now",
        enabled.then(|| {
            act(|_, st, _| {
                st.updates.force_refresh = true;
                st.updates.info.invalidate();
            })
        }),
    )
    .into()
}

pub fn software_update(st: &mut UpdatesState, wake: &Wake) -> El {
    let p = palette();
    // Finish an install: record the outcome and re-check.
    if let Some(inst) = &mut st.install {
        inst.done.poll();
        if let Some(r) = inst.done.value.take() {
            st.result = Some(r.map(|_| inst.restart));
            st.install = None;
            st.info.invalidate();
        }
    }
    if st.install.is_none() {
        let refresh = std::mem::take(&mut st.force_refresh);
        st.info.ensure(wake, Duration::from_secs(30 * 60), move || check(refresh));
    }
    let checking = st.info.busy();
    let info = st.info.value.clone();
    let os = crate::pages::system::os_name();

    let main: Vec<El> = if let Some(inst) = &st.install {
        // ---- Installing
        let (phase, line, frac) = inst
            .progress
            .lock()
            .map(|p| (p.phase.clone(), p.line.clone(), p.frac))
            .unwrap_or_default();
        let title = if phase.is_empty() { "Installing Updates…".to_string() } else { format!("Installing Updates — {phase}") };
        vec![
            status_row("software-update-available", BLUE, title, "Don't turn off your computer.".into(), None),
            form::custom_row(
                column![
                    controls::progress(frac.unwrap_or(0.0)),
                    text(orchard_ui::text::truncate(&line, 90)).size(11).color(p.text_secondary),
                ]
                .spacing(8),
            ),
        ]
    } else if let Some(info) = info.as_ref() {
        let n = info.count();
        let last = info.checked.map(ago).unwrap_or_default();
        if n == 0 {
            let title = if checking { "Checking for updates…" } else { "Your computer is up to date" };
            let sub = if os.is_empty() { format!("Last checked {last}") } else { format!("{os} · Last checked {last}") };
            vec![status_row("software-update-available", GREEN, title.into(), sub, Some(check_now(!checking)))]
        } else {
            let title = if n == 1 { "1 Update Available".to_string() } else { format!("{n} Updates Available") };
            let snapshot = info.clone();
            let update_now = controls::primary_button(
                "Update Now",
                (!checking).then(|| {
                    act(move |_, st, wake| {
                        st.updates.result = None;
                        st.updates.install = Some(install(&snapshot, wake));
                    })
                }),
            );
            let note = if info.needs_restart() {
                format!("Restart required · Last checked {last}")
            } else {
                format!("Last checked {last}")
            };
            vec![
                status_row(
                    "software-update-urgent",
                    BLUE,
                    title,
                    summary(info),
                    Some(row![check_now(!checking), update_now].spacing(8).into()),
                ),
                container(
                    row![
                        text(note).size(11).color(p.text_tertiary).width(Length::Fill),
                        controls::link_button(
                            if st.show_details { "Hide Details" } else { "Show Details…" },
                            act(|_, st, _| st.updates.show_details = !st.updates.show_details),
                        ),
                    ]
                    .align_y(Alignment::Center),
                )
                .padding([metrics::ROW_PAD_V, metrics::ROW_INSET])
                .into(),
            ]
        }
    } else {
        // ---- First check still running
        vec![status_row("view-refresh", GRAY, "Checking for updates…".into(), os, None)]
    };

    let mut col = column![form::group(main)];

    // Errors from either backend.
    if let Some(info) = &info {
        if let Some(Err(e)) = &info.dnf {
            col = col.push(form::error(Some(format!("dnf: {e}"))));
        }
        if let Some(Err(e)) = &info.flatpak {
            col = col.push(form::error(Some(format!("Flatpak: {e}"))));
        }
        if info.dnf.is_none() && info.flatpak.is_none() {
            col = col.push(form::error(Some("Neither dnf nor Flatpak is installed.".into())));
        }
    }

    // Outcome of the last install.
    match &st.result {
        Some(Err(e)) => col = col.push(form::error(Some(e.clone()))),
        Some(Ok(true)) => {
            col = col.push(form::group([form::button_row(
                "Updates installed",
                Some("Restart your computer to finish installing system updates.".into()),
                "Restart Now",
                Some(act(|_, _, _| sys::spawn_detached("systemctl", &["reboot"]))),
            )]));
        }
        _ => {}
    }

    if st.show_details && st.install.is_none() {
        if let Some(info) = &info {
            col = col.push(details(info));
        }
    }
    col.push(Space::with_height(0)).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dnf_repoquery() {
        let mut inst = HashMap::new();
        inst.insert("7zip.x86_64".to_string(), "26.04-1.fc45".to_string());
        let out = "7zip\tx86_64\t26.06-1.fc45\t1843200\tupdates\tA file archiver\n\
                   ImageMagick\tx86_64\t1:7.1.2.36-1.fc45\t%{download_size}\tupdates\tImages\n\n";
        let v = parse_dnf(out, &inst);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "7zip.x86_64");
        assert_eq!(v[0].old, "26.04-1.fc45");
        assert_eq!(v[0].new, "26.06-1.fc45");
        assert_eq!(v[0].size, Some(1_843_200));
        assert_eq!(v[1].size, None);
        assert_eq!(v[1].detail, "Images");
    }

    #[test]
    fn parses_dnf_table() {
        let out = "Upgrades\n7zip.x86_64    26.06-1.fc45      updates\nImageMagick.x86_64  1:7.1.2.36-1.fc45 updates\n";
        let v = parse_dnf_table(out, &HashMap::new());
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].new, "1:7.1.2.36-1.fc45");
    }

    #[test]
    fn parses_flatpak() {
        let mut inst = HashMap::new();
        inst.insert("app/gg.norisk.Launcher/x86_64/stable".to_string(), "0.6.29".to_string());
        let out = "runtime/org.gnome.Platform/x86_64/51\tGNOME Application Platform version 51\t\t432,8 MB\tflathub\n\
                   app/gg.norisk.Launcher/x86_64/stable\tNoRisk Client Launcher\t0.6.31\t57.5 MB\tflathub\n";
        let v = parse_flatpak(out, &inst);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "NoRisk Client Launcher");
        assert_eq!(v[0].old, "0.6.29");
        assert_eq!(v[0].new, "0.6.31");
        assert_eq!(v[0].size, Some(57_500_000));
        assert_eq!(v[1].new, "51");
        assert_eq!(v[1].size, Some(432_800_000));
    }

    #[test]
    fn counters_and_sizes() {
        assert_eq!(parse_counter("[ 3/40] Upgrading foo"), Some(3.0 / 40.0));
        assert_eq!(parse_counter("Updating 1/2…"), Some(0.5));
        assert_eq!(parse_counter("Downloading 1.2 MiB/s"), None);
        assert_eq!(parse_size("812 kB"), Some(812_000));
        assert_eq!(parse_size("bogus"), None);
        assert!(restart_pkg("kernel-core.x86_64"));
        assert!(!restart_pkg("7zip.x86_64"));
    }
}
