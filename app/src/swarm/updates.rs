//! "Check for Updates…": compares this app's version with the latest release,
//! read from swarm-term.com's feed (`/releases/latest.json`, in the shape of
//! GitHub's "latest release" API: a `v<version>` tag and a `Swarm-<version>.dmg`
//! asset). The site serves it so the check works while the GitHub repository is
//! private. A release whose DMG the feed signs (`sha256` and `signature` on the
//! asset, from scripts/sign-update.sh) can be installed in place
//! (`swarm::install`); others are left to the user (the DMG) or Homebrew.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use version_compare::{Cmp, Version};

/// Where releases are published.
const DEFAULT_FEED: &str = "https://swarm-term.com/releases/latest.json";

/// Opened instead of an unsigned release's link when that link isn't on
/// Swarm's own site or GitHub repository.
const DOWNLOAD_PAGE: &str = "https://swarm-term.com/#download";

/// This app's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A published release newer than this app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    /// The DMG if the release has one, else the release page. Unless the DMG
    /// is signed, always on swarm-term.com or Swarm's GitHub repository.
    pub url: String,
    /// What the app needs to install the DMG itself; `None` if the feed doesn't sign it.
    pub signed: Option<Signed>,
}

/// A DMG's SHA-256 (hex) and the release key's Ed25519 signature (base64) over
/// [`signed_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub sha256: String,
    pub signature: String,
}

/// What the release key signs for `version`'s DMG, so a signature can't be
/// reused for another file or another version.
pub fn signed_message(version: &str, sha256: &str) -> String {
    format!("swarm-update:{version}:{sha256}")
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    signature: Option<String>,
}

/// Whether `version` hasn't been offered yet in this run (and is now), so a
/// quiet check offers each update once rather than every day.
pub fn claim_offer(version: &str) -> bool {
    static OFFERED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let mut offered = OFFERED.lock().unwrap_or_else(|e| e.into_inner());
    if offered.as_deref() == Some(version) {
        return false;
    }
    *offered = Some(version.to_string());
    true
}

/// Whether the background check is due (none yet in this run, or the last
/// one a day ago), and if so, marks it done now. Every window asks every
/// hour, so checks go on whichever windows are closed, and run once a day.
pub fn claim_daily_check() -> bool {
    const DAY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|at| at.elapsed() < DAY) {
        return false;
    }
    *last = Some(std::time::Instant::now());
    true
}

/// When the last check ran and what it found, for Copy Debug Info.
static LAST_CHECK: std::sync::Mutex<Option<(std::time::SystemTime, String)>> = std::sync::Mutex::new(None);

pub fn record_check(outcome: String) {
    *LAST_CHECK.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::SystemTime::now(), outcome));
}

/// The last check's time and outcome, if one ran.
pub fn last_check() -> Option<(std::time::SystemTime, String)> {
    LAST_CHECK.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The feed URL: `SWARM_UPDATE_FEED` (a GitHub "latest release" API URL, or
/// anything answering with the same JSON) or the default.
fn feed() -> String {
    std::env::var("SWARM_UPDATE_FEED").unwrap_or_else(|_| DEFAULT_FEED.to_string())
}

/// The latest release if it's newer than `current`. `None` when there is no
/// newer one, or nothing is published yet (the feed answers 404).
pub async fn check(current: &str) -> Result<Option<Update>> {
    let response = reqwest::Client::new()
        .get(feed())
        .header("User-Agent", format!("Swarm/{current}"))
        .header("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .context("couldn't reach the release feed")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        bail!("the release feed answered {}", response.status());
    }
    let body = response.text().await.context("read the release feed")?;
    newer_release(&body, current)
}

/// The update `body` (a GitHub release as JSON) offers over `current`, if any.
fn newer_release(body: &str, current: &str) -> Result<Option<Update>> {
    let release: Release = serde_json::from_str(body).context("the release feed isn't a GitHub release")?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let version = release.tag_name.trim_start_matches('v').to_string();
    let (Some(latest), Some(ours)) = (Version::from(&version), Version::from(current)) else {
        bail!("can't compare release {:?} with {current}", release.tag_name);
    };
    if latest.compare(ours) != Cmp::Gt {
        return Ok(None);
    }
    // The app opens or downloads this link: only ever https.
    let https = |url: &str| url.starts_with("https://");
    let dmg = format!("Swarm-{version}.dmg");
    let Some(asset) = release.assets.into_iter().find(|a| a.name == dmg) else {
        if !https(&release.html_url) {
            bail!("the release feed's link isn't https");
        }
        return Ok(Some(Update { version, url: ours_or_download_page(release.html_url), signed: None }));
    };
    let signed = match (asset.sha256, asset.signature) {
        (Some(sha256), Some(signature)) => Some(Signed { sha256: sha256.to_ascii_lowercase(), signature }),
        _ => None,
    };
    if !https(&asset.browser_download_url) {
        bail!("the release feed's download link isn't https");
    }
    // A signed DMG is checked before it's used; an unsigned one is opened in
    // the browser for the user to install, so only from a place that's Swarm's.
    let url = match signed {
        Some(_) => asset.browser_download_url,
        None => ours_or_download_page(asset.browser_download_url),
    };
    Ok(Some(Update { version, url, signed }))
}

/// `link` if it's on swarm-term.com or in Swarm's GitHub repository, else the
/// site's download section.
fn ours_or_download_page(link: String) -> String {
    let ours = url::Url::parse(&link).is_ok_and(|u| {
        u.scheme() == "https"
            && u.port().is_none()
            && match u.host_str() {
                Some("swarm-term.com") => true,
                Some("github.com") => u.path().starts_with("/arturborycki/swarm-term/"),
                _ => false,
            }
    });
    if ours {
        link
    } else {
        DOWNLOAD_PAGE.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &str) -> String {
        format!(r#"{{"tag_name": "{tag}", "html_url": "https://example.com/r/{tag}", "assets": [{assets}]}}"#)
    }

    #[test]
    fn newer_release_offers_its_dmg() {
        let body = release(
            "v0.2.0",
            r#"{"name": "Swarm-0.2.0.dmg", "browser_download_url": "https://example.com/Swarm-0.2.0.dmg"}"#,
        );
        assert_eq!(
            newer_release(&body, "0.1.0").unwrap(),
            Some(Update { version: "0.2.0".into(), url: DOWNLOAD_PAGE.into(), signed: None }),
            "an unsigned DMG elsewhere than Swarm's site is not opened"
        );
        let ours = release(
            "v0.2.0",
            r#"{"name": "Swarm-0.2.0.dmg", "browser_download_url": "https://swarm-term.com/download/Swarm-0.2.0.dmg"}"#,
        );
        assert_eq!(newer_release(&ours, "0.1.0").unwrap().unwrap().url, "https://swarm-term.com/download/Swarm-0.2.0.dmg");
    }

    #[test]
    fn only_swarm_links_are_opened() {
        let page = |l: &str| ours_or_download_page(l.to_string());
        assert_eq!(page("https://github.com/arturborycki/swarm-term/releases/tag/v1"), "https://github.com/arturborycki/swarm-term/releases/tag/v1");
        assert_eq!(page("https://github.com/someone/else/releases"), DOWNLOAD_PAGE);
        assert_eq!(page("https://swarm-term.com.evil.example/x"), DOWNLOAD_PAGE);
        assert_eq!(page("https://swarm-term.com:8443/x"), DOWNLOAD_PAGE);
        assert_eq!(page("https://user@evil.example/swarm-term.com"), DOWNLOAD_PAGE);
    }

    #[test]
    fn signed_dmg_carries_its_hash_and_signature() {
        let body = release(
            "v0.2.0",
            r#"{"name": "Swarm-0.2.0.dmg", "browser_download_url": "https://example.com/u", "sha256": "ABC1", "signature": "c2ln"}"#,
        );
        let update = newer_release(&body, "0.1.0").unwrap().unwrap();
        assert_eq!(update.signed, Some(Signed { sha256: "abc1".into(), signature: "c2ln".into() }));
        assert_eq!(update.url, "https://example.com/u", "a signed DMG is checked, wherever it's from");
        assert_eq!(signed_message(&update.version, "abc1"), "swarm-update:0.2.0:abc1");
    }

    #[test]
    fn release_without_dmg_offers_its_page() {
        let update = newer_release(&release("v0.1.10", ""), "0.1.9").unwrap().unwrap();
        assert_eq!(update.url, DOWNLOAD_PAGE);
    }

    #[test]
    fn same_or_older_release_is_no_update() {
        assert_eq!(newer_release(&release("v0.1.0", ""), "0.1.0").unwrap(), None);
        assert_eq!(newer_release(&release("v0.0.9", ""), "0.1.0").unwrap(), None);
    }

    #[test]
    fn prereleases_and_bad_feeds() {
        let pre = r#"{"tag_name": "v9.0.0", "html_url": "x", "prerelease": true}"#;
        assert_eq!(newer_release(pre, "0.1.0").unwrap(), None);
        assert!(newer_release("not json", "0.1.0").is_err());
    }
}
