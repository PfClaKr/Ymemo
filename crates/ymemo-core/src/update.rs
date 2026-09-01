//! Update check: ask GitHub whether a newer release exists.
//!
//! **It only ever tells the user.** Nothing is downloaded and nothing is installed — the app
//! hands a link to the browser and the user decides. Installing behind someone's back would
//! mean elevation on Windows and root on Linux, and on Linux the package manager is the right
//! owner of that anyway.
//!
//! What it does do is *name the file*. A release carries a .deb, a .rpm, a Windows installer
//! and one apk per Android ABI, and pointing at the release page left the user to work out
//! which of the seven was theirs — a question nobody can answer for their own phone. The
//! build knows its own operating system and architecture, so [`Release::asset_url`] is the
//! file for the machine asking, and the page stays as the fallback.
//!
//! This is also the **only** request Ymemo makes to a server of anyone's, so it is worth being
//! precise about what it costs: a GET to api.github.com carrying nothing but the request
//! itself. No vault data, no device id, no identifier of any kind — but the machine's IP does
//! reach GitHub, which is why the whole thing can be switched off (`update_check` in the
//! desktop settings) and why the README says so plainly.

use std::time::Duration;

use anyhow::{Context, Result};
use ymemo_i18n::t;

/// Where releases are published. The repository is public, so the request is unauthenticated;
/// GitHub allows 60 an hour per IP, and the app asks about once a day.
const LATEST_URL: &str = "https://api.github.com/repos/PfClaKr/Ymemo/releases/latest";

/// GitHub rejects requests without one.
const USER_AGENT: &str = concat!("Ymemo/", env!("CARGO_PKG_VERSION"));

/// Kept short: this runs in the background and nobody is waiting for it.
const TIMEOUT: Duration = Duration::from_secs(10);

/// A published release, as much of it as the app shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Release {
    /// Version without the tag's `v`, e.g. `0.10.0`.
    pub version: String,
    /// The release page, which is where the user is sent when nothing below matched.
    pub url: String,
    /// The one file this build should download, when the release carries it.
    ///
    /// A release publishes a .deb, a .rpm, a Windows installer and three Android apks, and
    /// the page shows all of them at once. The app already knows which operating system it
    /// is, and on Android which ABI it was compiled for, so it can name the file instead of
    /// asking the user to recognise theirs. Empty when nothing matched — an unknown Linux
    /// packaging, a build from source — and then [`Release::url`] is all there is.
    pub asset_url: String,
    /// File name of [`Release::asset_url`], so the app can say what it is about to hand over.
    pub asset_name: String,
}

impl Release {
    /// Where the update button should go: the file for this build, or the page.
    pub fn download_url(&self) -> &str {
        if self.asset_url.is_empty() {
            &self.url
        } else {
            &self.asset_url
        }
    }
}

/// Returns the newest release when it is newer than `current`, `None` when it is not.
///
/// Errors are the caller's to ignore quietly: being offline, or GitHub being unreachable, is
/// not something to interrupt anyone over.
pub fn check(current: &str) -> Result<Option<Release>> {
    let latest = fetch_latest()?;
    Ok(is_newer(&latest.version, current).then_some(latest))
}

/// The latest release as GitHub reports it, whatever the local version is.
fn fetch_latest() -> Result<Release> {
    let mut res = ureq::get(LATEST_URL)
        .header("User-Agent", USER_AGENT)
        // Pin the API version, so a future default cannot reshape the response.
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .config()
        .timeout_global(Some(TIMEOUT))
        .build()
        .call()
        .with_context(|| t!("core.update_check_failed"))?;
    let body: serde_json::Value = res.body_mut().read_json().with_context(|| t!("core.update_check_failed"))?;
    parse_release(&body)
}

/// Pulls the version, the page and this build's own file out of the API response.
///
/// `/releases/latest` never returns drafts or prereleases, so anything that arrives here is
/// meant for users.
fn parse_release(body: &serde_json::Value) -> Result<Release> {
    let tag = body["tag_name"]
        .as_str()
        .with_context(|| t!("core.update_check_failed"))?;
    let url = body["html_url"].as_str().unwrap_or("").to_string();

    let (asset_name, asset_url) = body["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some((a["name"].as_str()?, a["browser_download_url"].as_str()?)))
        .find(|(name, _)| is_for_this_build(name))
        .map(|(name, url)| (name.to_string(), url.to_string()))
        .unwrap_or_default();

    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        url,
        asset_url,
        asset_name,
    })
}

/// Whether a release asset is the one this build should be updated with.
///
/// Matched on the name the release workflow gives it. Everything here is decided at compile
/// time except which of the two Linux packages the machine wants, and that is the one thing
/// the binary cannot know about itself.
fn is_for_this_build(name: &str) -> bool {
    if cfg!(target_os = "windows") {
        return name.ends_with("-setup-x86_64.exe");
    }
    if cfg!(target_os = "android") {
        // The library is built once per ABI, so the architecture this code was compiled for
        // *is* the device's — no need to ask Android which apk it wants.
        let abi = match std::env::consts::ARCH {
            "aarch64" => "arm64-v8a",
            "arm" => "armeabi-v7a",
            "x86_64" => "x86_64",
            _ => return false,
        };
        return name.ends_with(&format!("-android-{abi}.apk"));
    }
    if cfg!(target_os = "linux") {
        return match linux_package_suffix() {
            Some(suffix) => name.ends_with(suffix),
            None => false,
        };
    }
    false
}

/// `.deb` or `.rpm` for this machine, or `None` when it is neither — a build from source, a
/// distribution that is not one of the two families — in which case handing over a package
/// it cannot install would be worse than showing the page.
fn linux_package_suffix() -> Option<&'static str> {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let family = |needle: &str| {
        os_release.lines().any(|line| {
            let Some(value) = line
                .strip_prefix("ID=")
                .or_else(|| line.strip_prefix("ID_LIKE="))
            else {
                return false;
            };
            value.trim_matches('"').split_whitespace().any(|id| id == needle)
        })
    };
    if family("debian") || family("ubuntu") {
        return Some(".deb");
    }
    if family("fedora") || family("rhel") || family("centos") {
        return Some(".rpm");
    }
    // Older releases carry no usable ID_LIKE; the package databases are the fallback.
    if std::path::Path::new("/etc/debian_version").exists() {
        return Some(".deb");
    }
    if std::path::Path::new("/etc/redhat-release").exists() {
        return Some(".rpm");
    }
    None
}

/// Whether `candidate` is a later version than `current`.
///
/// Compared field by field as numbers, because a string comparison puts 0.10.0 *before* 0.9.0.
/// Anything unparseable in a field counts as 0, and a trailing suffix (`0.9.0-rc1`) is ignored
/// rather than guessed at — releases the app is told about are never prereleases anyway.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fields(candidate) > fields(current)
}

fn fields(version: &str) -> (u32, u32, u32) {
    let mut parts = version
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_fields_as_numbers() {
        // The case a string comparison gets wrong.
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));

        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.9.1", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.9.0"));
        assert!(!is_newer("0.8.9", "0.9.0"));
    }

    #[test]
    fn tolerates_tags_and_odd_shapes() {
        assert!(is_newer("v0.10.0", "0.9.0")); // the tag's v
        assert!(is_newer("1.0", "0.9.0")); // missing patch
        assert!(!is_newer("junk", "0.1.0")); // unparseable is not an update
        assert!(!is_newer("0.9.0-rc1", "0.9.0")); // suffix ignored, so not newer
    }

    #[test]
    fn reads_the_api_response() {
        let body = serde_json::json!({
            "tag_name": "v0.10.0",
            "html_url": "https://github.com/PfClaKr/Ymemo/releases/tag/v0.10.0",
            "name": "v0.10.0",
        });
        let release = parse_release(&body).unwrap();
        assert_eq!(release.version, "0.10.0");
        assert!(release.url.ends_with("/v0.10.0"));
    }

    /// The names a real release carries, so the picker is tested against the set it will
    /// actually see rather than one asset at a time.
    fn release_body() -> serde_json::Value {
        let names = [
            "ymemo-0.10.0-1.fc44.x86_64.rpm",
            "ymemo-0.10.0-android-arm64-v8a.apk",
            "ymemo-0.10.0-android-armeabi-v7a.apk",
            "ymemo-0.10.0-android-x86_64.apk",
            "ymemo-ffi-ios.zip",
            "ymemo-setup-x86_64.exe",
            "ymemo_0.10.0_amd64.deb",
        ];
        serde_json::json!({
            "tag_name": "v0.10.0",
            "html_url": "https://github.com/PfClaKr/Ymemo/releases/tag/v0.10.0",
            "assets": names.iter().map(|n| serde_json::json!({
                "name": n,
                "browser_download_url": format!("https://example.invalid/{n}"),
            })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn picks_one_asset_and_only_one() {
        let release = parse_release(&release_body()).unwrap();
        let names = [
            "ymemo-0.10.0-1.fc44.x86_64.rpm",
            "ymemo-0.10.0-android-arm64-v8a.apk",
            "ymemo-0.10.0-android-armeabi-v7a.apk",
            "ymemo-0.10.0-android-x86_64.apk",
            "ymemo-ffi-ios.zip",
            "ymemo-setup-x86_64.exe",
            "ymemo_0.10.0_amd64.deb",
        ];
        // Whatever this test is compiled for, at most one name may match — two would make
        // the choice arbitrary, which is the thing being removed.
        assert!(names.iter().filter(|n| is_for_this_build(n)).count() <= 1);

        if release.asset_name.is_empty() {
            // No packaging this build recognises; the page has to carry it.
            assert_eq!(release.download_url(), release.url);
        } else {
            assert!(names.contains(&release.asset_name.as_str()));
            assert_eq!(release.download_url(), release.asset_url);
            // The ios library is a build input, never something a user installs.
            assert_ne!(release.asset_name, "ymemo-ffi-ios.zip");
        }
    }

    #[test]
    fn a_release_without_assets_falls_back_to_the_page() {
        let release = parse_release(&serde_json::json!({
            "tag_name": "v0.10.0",
            "html_url": "https://example.invalid/page",
        }))
        .unwrap();
        assert!(release.asset_url.is_empty());
        assert_eq!(release.download_url(), "https://example.invalid/page");
    }

    #[test]
    fn never_offers_another_platform_its_file() {
        // The three that are wrong for every platform this crate builds for.
        assert!(!is_for_this_build("ymemo-ffi-ios.zip"));
        assert!(!is_for_this_build("SOURCE_CODE.tar.gz"));
        assert!(!is_for_this_build(""));
    }

    #[test]
    fn a_response_without_a_tag_is_an_error() {
        assert!(parse_release(&serde_json::json!({ "html_url": "x" })).is_err());
    }

    /// Hits the real API. Skipped unless YMEMO_NET_TESTS is set, so the suite stays offline
    /// by default.
    #[test]
    fn fetches_the_real_latest_release() {
        if std::env::var_os("YMEMO_NET_TESTS").is_none() {
            eprintln!("skip: set YMEMO_NET_TESTS=1 to check against api.github.com");
            return;
        }
        let release = fetch_latest().expect("the API should answer");
        assert!(!release.version.is_empty());
        assert!(release.url.starts_with("https://github.com/"));

        // The whole path, not just the request: an ancient local version must be told there
        // is something newer, and an impossible one must not.
        assert!(check("0.0.1").unwrap().is_some());
        assert_eq!(check("999.0.0").unwrap(), None);
    }
}
