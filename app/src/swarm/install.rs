//! "Install and Restart": installs a signed update in place. The DMG must match
//! the SHA-256 and Ed25519 signature the release feed gives for it, checked
//! against the release key built in here; its Swarm.app is then copied next to
//! the running one, checked, swapped in atomically and relaunched. Sessions live
//! in swarmd, so they keep running across the restart.
//!
//! The download doesn't come through a browser, so it isn't quarantined and
//! Gatekeeper doesn't stop the new version on launch: the release signature is
//! what vouches for it.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::io::Write as _;

use super::updates::{Signed, Update, signed_message};

/// The release key's Ed25519 public key, base64 (see scripts/sign-update.sh).
const UPDATE_PUBLIC_KEY: &str = "3vFlICRfFgqFcyxPaszs2Ri/aLv9HpyuL+ZJaWyHm2Y=";

const BUNDLE_ID: &str = "dev.swarm.Swarm";

/// Prefix of the update copied next to the installed app, which after the swap
/// holds the previous version until the next launch removes it.
const STAGED_PREFIX: &str = ".Swarm-update-";

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Largest update downloaded (a release DMG is about 150 MB).
const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;

/// An update copied next to the installed app, ready to swap in.
pub struct Staged {
    staged: PathBuf,
    bundle: PathBuf,
    version: String,
}

impl Staged {
    pub fn version(&self) -> &str {
        &self.version
    }
}

/// An update downloaded in the background and not installed yet. Quitting Swarm installs it.
static PENDING: std::sync::Mutex<Option<Staged>> = std::sync::Mutex::new(None);

/// Keeps `staged` to install later, replacing any earlier one.
pub fn set_pending(staged: Staged) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(staged);
}

/// The version of the update waiting to be installed, if any.
pub fn pending_version() -> Option<String> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|s| s.version.clone())
}

/// Takes the update waiting to be installed.
pub fn take_pending() -> Option<Staged> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Whether to start Swarm again once it has quit (with `true`: without coming
/// to the front), and when [`install`] asked. A quit the user cancelled
/// doesn't count later: only a quit soon after asking starts Swarm again.
static RELAUNCH: std::sync::Mutex<Option<(bool, std::time::Instant)>> = std::sync::Mutex::new(None);
const RELAUNCH_WITHIN: Duration = Duration::from_secs(120);

/// Swaps in the waiting update as Swarm quits. After [`install`], the new
/// version is then started; otherwise the next launch is the new version.
pub fn install_pending_on_quit() {
    let relaunch = RELAUNCH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .filter(|(_, asked)| asked.elapsed() < RELAUNCH_WITHIN)
        .map(|(background, _)| background);
    let Some(staged) = take_pending() else { return };
    match swap(&staged) {
        Ok(()) => log::info!("Installed Swarm {} on quit", staged.version),
        Err(e) => log::warn!("Couldn't install Swarm {} on quit: {e:#}", staged.version),
    }
    // Swapped or not, the user asked for Swarm to come back.
    if let Some(background) = relaunch
        && let Err(e) = open_after_exit(&staged.bundle, background)
    {
        log::warn!("Couldn't start Swarm again: {e:#}");
    }
}

/// The running Swarm.app, if this process runs from one that can replace itself.
pub fn installed_bundle() -> Result<PathBuf> {
    let bundle = PathBuf::from(warp_core::macos::get_bundle_path()?);
    ensure!(
        bundle.extension().is_some_and(|ext| ext == "app") && plist_value(&bundle, "CFBundleIdentifier")? == BUNDLE_ID,
        "this copy of Swarm isn't running from Swarm.app"
    );
    // macOS runs a quarantined app that wasn't moved out of Downloads from a read-only copy.
    ensure!(
        !bundle.to_string_lossy().contains("/AppTranslocation/"),
        "macOS runs this copy of Swarm from a temporary read-only location. Move Swarm.app to Applications and open it from there."
    );
    let parent = bundle.parent().context("Swarm.app has no folder")?;
    ensure!(
        writable(&bundle) && writable(parent),
        "you can't change {} with your account.",
        parent.display()
    );
    Ok(bundle)
}

/// Downloads `update`, checks it and stages it next to the installed app.
pub async fn download(update: &Update) -> Result<Staged> {
    let signed = update.signed.as_ref().context("this release isn't signed for installing")?;
    verify_signature(&update.version, signed)?;
    let bundle = installed_bundle()?;
    let dir = download_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let dmg = dir.join(format!("Swarm-{}.dmg", update.version));
    let sha256 = fetch(&update.url, &dmg).await?;
    ensure!(
        sha256 == signed.sha256,
        "the download doesn't match the release (SHA-256 {sha256}, expected {})",
        signed.sha256
    );
    let version = update.version.clone();
    tokio::task::spawn_blocking(move || {
        let staged = stage(&dmg, &bundle, &version);
        let _ = std::fs::remove_dir_all(dmg.parent().unwrap_or(&dmg));
        staged
    })
    .await
    .context("the install task stopped")?
}

fn swap(staged: &Staged) -> Result<()> {
    let from = CString::new(staged.staged.as_os_str().as_bytes())?;
    let to = CString::new(staged.bundle.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error()).context("swap in the new Swarm.app");
    }
    Ok(())
}

/// Installs the staged update as Swarm quits and starts it once this process
/// has exited. The caller then quits the app. Nothing changes on disk before
/// the quit goes ahead: if the user cancels it, the update waits for the next
/// quit. With `background`, the new version opens without coming to the front.
pub fn install(staged: Staged, background: bool) -> Result<()> {
    ensure!(staged.staged.exists(), "the downloaded update is gone; download it again");
    set_pending(staged);
    *RELAUNCH.lock().unwrap_or_else(|e| e.into_inner()) = Some((background, std::time::Instant::now()));
    Ok(())
}

/// Opens `bundle` once this process has exited.
fn open_after_exit(bundle: &Path, background: bool) -> Result<()> {
    let open_flags = if background { "-n -g" } else { "-n" };
    let wait_then_open = format!(
        "while kill -0 {} 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open {open_flags} \"$@\"",
        std::process::id()
    );
    // `open` starts the app without this process's environment; an instance
    // with its own SWARM_* directories must come back to the same daemon.
    let env_args = std::env::vars()
        .filter(|(name, _)| name.starts_with("SWARM_"))
        .flat_map(|(name, value)| ["--env".to_string(), format!("{name}={value}")]);
    Command::new("/bin/sh")
        .arg("-c")
        .arg(wait_then_open)
        .arg("sh")
        .args(env_args)
        .arg(bundle)
        .spawn()
        .context("start the new version")?;
    Ok(())
}

/// Removes the previous version left by an update and any unfinished download.
pub fn remove_leftovers() {
    let _ = std::fs::remove_dir_all(download_dir());
    let Ok(bundle) = warp_core::macos::get_bundle_path().map(PathBuf::from) else { return };
    let Some(Ok(entries)) = bundle.parent().map(std::fs::read_dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(STAGED_PREFIX) {
            log::info!("Removing {}", entry.path().display());
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn verify_signature(version: &str, signed: &Signed) -> Result<()> {
    let engine = base64::engine::general_purpose::STANDARD;
    let key = engine.decode(UPDATE_PUBLIC_KEY).expect("UPDATE_PUBLIC_KEY is base64");
    let signature = engine.decode(signed.signature.trim()).context("the release signature isn't base64")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(signed_message(version, &signed.sha256).as_bytes(), &signature)
        .ok()
        .context("the release isn't signed with Swarm's release key")
}

/// Downloads `url` to `path`, returning its SHA-256 (hex).
async fn fetch(url: &str, path: &Path) -> Result<String> {
    let mut response = reqwest::Client::new()
        .get(url)
        .header("User-Agent", format!("Swarm/{}", super::updates::CURRENT))
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .context("couldn't reach the download")?
        .error_for_status()
        .context("couldn't download the update")?;
    if response.content_length().is_some_and(|len| len > MAX_DOWNLOAD) {
        bail!("the download is larger than an update can be");
    }
    let mut file = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut got: u64 = 0;
    while let Some(chunk) = response.chunk().await.context("the download stopped")? {
        got += chunk.len() as u64;
        if got > MAX_DOWNLOAD {
            bail!("the download is larger than an update can be");
        }
        hasher.update(&chunk);
        file.write_all(&chunk).context("save the download")?;
    }
    file.sync_all()?;
    Ok(hex::encode(hasher.finalize()))
}

/// Copies the app out of `dmg` next to `bundle` and checks the copy.
fn stage(dmg: &Path, bundle: &Path, version: &str) -> Result<Staged> {
    let parent = bundle.parent().context("Swarm.app has no folder")?;
    let staged = parent.join(format!("{STAGED_PREFIX}{version}.app"));
    let _ = std::fs::remove_dir_all(&staged);
    let mountpoint = dmg.with_extension("mnt");
    run(Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-noautoopen", "-readonly", "-autofsck", "-mountpoint"])
        .arg(&mountpoint)
        .arg(dmg))
    .context("open the DMG")?;
    let copied = run(Command::new("/usr/bin/ditto").arg(mountpoint.join("Swarm.app")).arg(&staged))
        .context("copy Swarm.app out of the DMG");
    let _ = run(Command::new("/usr/bin/hdiutil").arg("detach").arg(&mountpoint).arg("-force"));
    let checked = copied.and_then(|()| check_staged(&staged, bundle, version));
    if checked.is_err() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    checked.map(|()| Staged { staged, bundle: bundle.to_path_buf(), version: version.to_string() })
}

fn check_staged(staged: &Path, bundle: &Path, version: &str) -> Result<()> {
    let id = plist_value(staged, "CFBundleIdentifier")?;
    ensure!(id == BUNDLE_ID, "the DMG holds {id}, not Swarm");
    let staged_version = plist_value(staged, "CFBundleShortVersionString")?;
    ensure!(staged_version == version, "the DMG holds Swarm {staged_version}, not {version}");
    run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(staged))
        .context("the new Swarm.app's code signature doesn't verify")?;
    // A Developer ID-signed Swarm only updates to one signed by the same team
    // (ad-hoc builds have none, and may update to a signed one).
    if let Some(team) = team_id(bundle)? {
        let staged_team = team_id(staged)?;
        ensure!(
            staged_team.as_deref() == Some(team.as_str()),
            "the new Swarm.app isn't signed by this app's developer ({team})"
        );
    }
    Ok(())
}

/// The Apple team that signed `bundle`; `None` for an ad-hoc signature.
fn team_id(bundle: &Path) -> Result<Option<String>> {
    let output = Command::new("/usr/bin/codesign").arg("-dv").arg(bundle).output()?;
    ensure!(output.status.success(), "can't read the code signature of {}", bundle.display());
    // codesign -d writes its report to stderr.
    Ok(String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .filter(|team| *team != "not set")
        .map(str::to_string))
}

fn plist_value(bundle: &Path, key: &str) -> Result<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", &format!("Print :{key}")])
        .arg(bundle.join("Contents/Info.plist"))
        .output()?;
    ensure!(output.status.success(), "{} has no {key}", bundle.display());
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run(command: &mut Command) -> Result<()> {
    let output = command.output().with_context(|| format!("run {command:?}"))?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn writable(path: &Path) -> bool {
    CString::new(path.as_os_str().as_bytes())
        // SAFETY: a valid NUL-terminated path.
        .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0)
}

fn download_dir() -> PathBuf {
    warp_core::paths::cache_dir().join("swarm-update")
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
