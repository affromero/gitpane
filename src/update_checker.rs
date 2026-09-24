use std::time::Duration;

const GITHUB_API_URL: &str = "https://api.github.com/repos/affromero/gitpane/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Check GitHub for a newer release. Returns `Some(tag)` if a newer version
/// exists, `None` if current is up-to-date (or on any error — fail silently).
pub(crate) fn check_latest() -> Option<String> {
    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_body(Some(Duration::from_secs(10)))
            .build(),
    );

    let body = agent
        .get(GITHUB_API_URL)
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", concat!("gitpane/", env!("CARGO_PKG_VERSION")))
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;

    release_update(&body, CURRENT_VERSION)
}

fn release_update(body: &str, current_version: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    let tag = body.get("tag_name")?.as_str()?;

    let remote = parse_semver(tag)?;
    let local = parse_semver(current_version)?;

    if remote > local {
        Some(tag.trim_start_matches('v').to_string())
    } else {
        None
    }
}

/// Parse a version string like "v0.2.0" or "0.2.0" into (major, minor, patch).
fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim_start_matches('v');
    let mut parts = s.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_response_only_offers_newer_versions() {
        for (tag, current, expected) in [
            ("v1.0.0", "0.2.0", Some("1.0.0")),
            ("1.3.0", "1.2.9", Some("1.3.0")),
            ("1.2.10", "1.2.9", Some("1.2.10")),
            ("0.2.0", "0.2.0", None),
            ("0.1.0", "0.2.0", None),
            ("1.9.9", "2.0.0", None),
            ("1.2.99", "1.3.0", None),
            ("invalid", "0.2.0", None),
            ("1.2.0", "invalid", None),
        ] {
            let body = serde_json::json!({ "tag_name": tag }).to_string();
            assert_eq!(
                release_update(&body, current).as_deref(),
                expected,
                "{tag} vs {current}"
            );
        }
        for body in [
            "not JSON",
            "{}",
            r#"{"tag_name":42}"#,
            r#"{"tag_name":null}"#,
        ] {
            assert_eq!(release_update(body, "0.2.0"), None, "{body}");
        }
    }

    #[test]
    fn test_handles_v_prefix() {
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn test_handles_malformed_tag() {
        assert_eq!(parse_semver("not-a-version"), None);
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver(""), None);
    }
}
