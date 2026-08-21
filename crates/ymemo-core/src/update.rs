//! Update check: ask GitHub whether a newer release exists.
//!
//! **It only ever tells the user.** Nothing is downloaded and nothing is installed — the app
//! points at the release page and the user decides. Installing behind someone's back would
//! mean elevation on Windows and root on Linux, and on Linux the package manager is the right
//! owner of that anyway.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Version without the tag's `v`, e.g. `0.10.0`.
    pub version: String,
    /// The release page, which is what the user is sent to.
    pub url: String,
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

/// Pulls the version and page out of the API response.
///
/// `/releases/latest` never returns drafts or prereleases, so anything that arrives here is
/// meant for users.
fn parse_release(body: &serde_json::Value) -> Result<Release> {
    let tag = body["tag_name"]
        .as_str()
        .with_context(|| t!("core.update_check_failed"))?;
    let url = body["html_url"].as_str().unwrap_or("").to_string();
    Ok(Release { version: tag.trim_start_matches('v').to_string(), url })
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
