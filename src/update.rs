//! Update check and self-update from GitHub releases.
//!
//! The latest release is read from the GitHub API, its `viewer3d.exe` (or the zip) is
//! downloaded, checked against the SHA-256 digest GitHub publishes for every asset, run
//! once with `--version` to make sure it starts and is the expected version, and then
//! swapped in: Windows lets a running exe be renamed, so the current one becomes
//! `viewer3d.exe.old` (deleted on the next start) and the new one takes its name.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

pub const REPO: &str = "valuce/3DViewer";
const EXE_ASSET: &str = "viewer3d.exe";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Parses `v1.2.3`, `1.2`, `1.2.3-beta` (the suffix is ignored).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let core = s.split(['-', '+', ' ']).next()?;
        let mut parts = core.split('.');
        let mut num = |required: bool| match parts.next() {
            Some(p) => p.parse::<u32>().ok(),
            None if !required => Some(0),
            None => None,
        };
        Some(Self(num(true)?, num(false)?, num(false)?))
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self(0, 0, 0))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    pub page: String,
    pub notes: String,
    pub asset: Option<Asset>,
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: Option<[u8; 32]>,
    pub zipped: bool,
}

fn parse_digest(d: &str) -> Option<[u8; 32]> {
    let hex = d.strip_prefix("sha256:")?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

pub fn parse_release(json: &[u8]) -> Result<Release> {
    let r: GhRelease = serde_json::from_slice(json).context("unexpected reply from GitHub")?;
    let version = Version::parse(&r.tag_name).with_context(|| format!("unrecognised release tag {}", r.tag_name))?;
    let lower = |a: &GhAsset| a.name.to_ascii_lowercase();
    let pick = r
        .assets
        .iter()
        .find(|a| lower(a) == EXE_ASSET)
        .or_else(|| r.assets.iter().find(|a| lower(a).ends_with(".exe")))
        .map(|a| (a, false))
        .or_else(|| r.assets.iter().find(|a| lower(a).ends_with(".zip")).map(|a| (a, true)));
    let asset = pick.map(|(a, zipped)| Asset {
        name: a.name.clone(),
        url: a.browser_download_url.clone(),
        size: a.size,
        sha256: a.digest.as_deref().and_then(parse_digest),
        zipped,
    });
    Ok(Release { version, page: r.html_url, notes: r.body.unwrap_or_default().trim().replace("\r\n", "\n"), asset })
}

/// Latest published release (the API skips drafts and pre-releases).
pub fn fetch_latest() -> Result<Release> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = crate::net::get_bytes(&url, "application/vnd.github+json", 4 << 20)?;
    parse_release(&body)
}

pub fn releases_page() -> String {
    format!("https://github.com/{REPO}/releases")
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

fn exe_path() -> Result<PathBuf> {
    std::env::current_exe().context("cannot locate the running program")
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

const CANCELLED: &str = "cancelled";

/// Downloads, verifies and installs `asset`. Returns the path of the installed exe
/// (the same path the running program was started from).
pub fn download_and_install(asset: &Asset, expect: Version, progress: &Progress) -> Result<PathBuf> {
    progress.total.store(asset.size, Relaxed);
    let mut data = Vec::with_capacity(asset.size as usize);
    let mut hasher = Sha256::new();
    crate::net::get(&asset.url, "application/octet-stream", |chunk, total| {
        if progress.cancel.load(Relaxed) {
            bail!(CANCELLED);
        }
        hasher.update(chunk);
        data.extend_from_slice(chunk);
        if let Some(t) = total {
            progress.total.store(t, Relaxed);
        }
        progress.done.store(data.len() as u64, Relaxed);
        if data.len() as u64 > asset.size + (1 << 20) {
            bail!("the download is larger than announced");
        }
        Ok(())
    })?;
    if data.len() as u64 != asset.size {
        bail!("incomplete download: {} of {} bytes", data.len(), asset.size);
    }
    if let Some(want) = asset.sha256
        && hasher.finalize().as_slice() != want
    {
        bail!("checksum mismatch, the download is damaged");
    }
    let exe = if asset.zipped { extract_exe(&data)? } else { data };
    if !exe.starts_with(b"MZ") {
        bail!("the downloaded file is not a Windows program");
    }

    let current = exe_path()?;
    let staged = sibling(&current, ".new");
    std::fs::write(&staged, &exe)
        .with_context(|| format!("cannot write next to {} (no write access to the program folder?)", current.display()))?;
    if let Err(e) = verify_binary(&staged, expect) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }
    let old = sibling(&current, ".old");
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(&current, &old) {
        let _ = std::fs::remove_file(&staged);
        return Err(e).context("cannot move the running program aside");
    }
    if let Err(e) = std::fs::rename(&staged, &current) {
        let _ = std::fs::rename(&old, &current);
        return Err(e).context("cannot put the new version in place");
    }
    Ok(current)
}

fn extract_exe(zip: &[u8]) -> Result<Vec<u8>> {
    let mut ar = zip::ZipArchive::new(std::io::Cursor::new(zip)).context("damaged zip")?;
    let mut found = None;
    for i in 0..ar.len() {
        if ar.by_index(i)?.name()?.to_ascii_lowercase().ends_with(".exe") {
            found = Some(i);
            break;
        }
    }
    let mut f = ar.by_index(found.context("no .exe in the zip")?)?;
    let mut out = Vec::with_capacity(f.size() as usize);
    std::io::copy(&mut f, &mut out)?;
    Ok(out)
}

/// Runs the downloaded program with `--version` and checks what it reports.
fn verify_binary(path: &Path, expect: Version) -> Result<()> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("the new version does not start")?;
    let start = Instant::now();
    while child.try_wait()?.is_none() {
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            bail!("the new version does not respond");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    if let Some(mut s) = child.stdout.take() {
        s.read_to_string(&mut out)?;
    }
    let reported = out.trim().strip_prefix("3DViewer ").and_then(Version::parse);
    if reported != Some(expect) {
        bail!("the downloaded program reports {:?} instead of version {expect}", out.trim());
    }
    Ok(())
}

/// Deletes `viewer3d.exe.old` / `.new` left by a previous update. The old process may
/// still be exiting, so this retries in the background for a few seconds.
pub fn cleanup_leftovers() {
    let Ok(exe) = exe_path() else { return };
    let files: Vec<PathBuf> = [".old", ".new"].iter().map(|s| sibling(&exe, s)).filter(|p| p.exists()).collect();
    if files.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..50 {
            if files.iter().all(|f| std::fs::remove_file(f).is_ok() || !f.exists()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

pub enum State {
    Idle,
    Checking,
    UpToDate(Version),
    Available(Release),
    Downloading(Release, Arc<Progress>),
    /// Installed; takes effect after a restart. Holds the exe to start.
    Installed(Release, PathBuf),
    Failed(String),
}

enum Msg {
    Checked(Result<Release>),
    Installed(Result<PathBuf>),
}

/// Background update checker / installer driven from the UI thread.
pub struct Updater {
    pub state: State,
    /// The current check was started automatically (stay quiet unless there is news).
    pub silent: bool,
    rx: Option<mpsc::Receiver<Msg>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self { state: State::Idle, silent: true, rx: None }
    }
}

impl Updater {
    pub fn busy(&self) -> bool {
        matches!(self.state, State::Checking | State::Downloading(..))
    }

    pub fn check(&mut self, ctx: &egui::Context, silent: bool) {
        if self.busy() || matches!(self.state, State::Installed(..)) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Checked(fetch_latest()));
            ctx.request_repaint();
        });
        self.rx = Some(rx);
        self.silent = silent;
        self.state = State::Checking;
    }

    pub fn install(&mut self, ctx: &egui::Context) {
        let State::Available(rel) = &self.state else { return };
        let Some(asset) = rel.asset.clone() else { return };
        let rel = rel.clone();
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let (ctx, p, version) = (ctx.clone(), progress.clone(), rel.version);
        std::thread::spawn(move || {
            let r = download_and_install(&asset, version, &p);
            let _ = tx.send(Msg::Installed(r));
            ctx.request_repaint();
        });
        self.rx = Some(rx);
        self.silent = false;
        self.state = State::Downloading(rel, progress);
    }

    pub fn cancel(&mut self) {
        if let State::Downloading(_, p) = &self.state {
            p.cancel.store(true, Relaxed);
        }
    }

    /// Picks up results from the worker thread. Returns true when a newer version was found.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let Ok(msg) = rx.try_recv() else { return false };
        self.rx = None;
        match msg {
            Msg::Checked(Ok(rel)) if rel.version > Version::current() => {
                self.state = State::Available(rel);
                return true;
            }
            Msg::Checked(Ok(rel)) => self.state = State::UpToDate(rel.version),
            Msg::Checked(Err(e)) => self.state = State::Failed(format!("{e:#}")),
            Msg::Installed(r) => {
                let rel = match std::mem::replace(&mut self.state, State::Idle) {
                    State::Downloading(rel, _) => rel,
                    _ => return false,
                };
                self.state = match r {
                    Ok(path) => State::Installed(rel, path),
                    Err(e) if format!("{e:#}").contains(CANCELLED) => State::Available(rel),
                    Err(e) => State::Failed(format!("{e:#}")),
                };
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert_eq!(Version::parse("v0.3.0"), Some(Version(0, 3, 0)));
        assert_eq!(Version::parse("1.2"), Some(Version(1, 2, 0)));
        assert_eq!(Version::parse("2.0.1-beta"), Some(Version(2, 0, 1)));
        assert_eq!(Version::parse("latest"), None);
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
    }

    #[test]
    fn release_json_picks_exe_and_digest() {
        let json = br#"{
            "tag_name": "v9.1.0", "html_url": "https://github.com/x/y/releases/tag/v9.1.0", "body": "Notes\r\nline",
            "assets": [
                {"name": "3DViewer-9.1.0-windows-x64.zip", "browser_download_url": "https://e/z.zip", "size": 10, "digest": null},
                {"name": "viewer3d.exe", "browser_download_url": "https://e/v.exe", "size": 20,
                 "digest": "sha256:7908f333875e6ca1ea0ee7d9a5d58efe36acc5be33a172405cc6a7ad26401471"}
            ]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, Version(9, 1, 0));
        assert_eq!(r.notes, "Notes\nline");
        let a = r.asset.unwrap();
        assert_eq!(a.name, "viewer3d.exe");
        assert!(!a.zipped);
        assert_eq!(a.sha256.unwrap()[0], 0x79);
    }

    #[test]
    fn release_json_falls_back_to_zip() {
        let json = br#"{"tag_name": "0.4", "html_url": "u", "assets": [
            {"name": "build.zip", "browser_download_url": "https://e/b.zip", "size": 5}]}"#;
        let a = parse_release(json).unwrap().asset.unwrap();
        assert!(a.zipped);
        assert!(a.sha256.is_none());
    }

    /// Hits the real GitHub API; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_latest_release() {
        let r = fetch_latest().unwrap();
        assert!(r.version >= Version(0, 2, 0));
        assert!(r.asset.is_some());
    }

    /// Downloads the latest release asset (follows GitHub's redirect to its CDN) and
    /// checks it against the digest from the API.
    #[test]
    #[ignore]
    fn live_download_matches_digest() {
        let a = fetch_latest().unwrap().asset.unwrap();
        let want = a.sha256.expect("GitHub publishes asset digests");
        let mut hasher = Sha256::new();
        let mut n = 0u64;
        crate::net::get(&a.url, "application/octet-stream", |c, _| {
            hasher.update(c);
            n += c.len() as u64;
            Ok(())
        })
        .unwrap();
        assert_eq!(n, a.size);
        assert_eq!(hasher.finalize().as_slice(), want);
    }

    /// The freshly built viewer answers `--version` the way the updater expects.
    #[test]
    #[ignore]
    fn built_exe_reports_its_version() {
        let test_exe = std::env::current_exe().unwrap();
        let exe = test_exe.ancestors().map(|d| d.join("viewer3d.exe")).find(|p| p.is_file()).expect("run `cargo build` first");
        verify_binary(&exe, Version::current()).unwrap();
        assert!(verify_binary(&exe, Version(99, 0, 0)).is_err());
    }
}
