//! Server address validation and `/.well-known/hearfolio` compatibility checks.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const API_VERSION: u64 = 1;

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct OAuthEndpoints {
    pub authorize_url: String,
    pub token_url: String,
    pub revoke_url: String,
}
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct UploadLimits {
    pub multipart_threshold_bytes: u64,
    pub part_size_bytes: u64,
}
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Discovery {
    #[serde(default)]
    pub product: Option<String>,
    pub name: String,
    #[serde(default)]
    pub server_version: String,
    pub api_version: u64,
    pub min_app_version: String,
    pub oauth: OAuthEndpoints,
    pub upload: UploadLimits,
}

/// Plain HTTP is accepted only where traffic cannot leave the user's machine or LAN.
fn local_host(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(name) => {
            let name = name.to_ascii_lowercase();
            name == "localhost"
                || [
                    ".localhost",
                    ".local",
                    ".test",
                    ".lan",
                    ".internal",
                    ".home.arpa",
                ]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        }
        url::Host::Ipv4(ip) => {
            let ip = IpAddr::V4(*ip);
            ip.is_loopback()
                || matches!(ip, IpAddr::V4(v4) if v4.is_private() || v4.is_link_local())
        }
        url::Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// `cloud.example.com`, `https://cloud.example.com/`, `http://192.168.1.5:9000`
/// → canonical base URL without a trailing slash. Credentials, queries and
/// fragments are rejected; public hosts require HTTPS.
pub fn normalize_server_url(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > 2048 || trimmed.chars().any(char::is_whitespace) {
        return Err("errors.orgUrlInvalid".into());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let url = url::Url::parse(&with_scheme).map_err(|_| "errors.orgUrlInvalid")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("errors.orgUrlInvalid".into());
    }
    let host = url.host().ok_or("errors.orgUrlInvalid")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("errors.orgUrlInvalid".into());
    }
    if url.scheme() == "http" && !local_host(&host) {
        return Err("errors.orgUrlInsecure".into());
    }
    let mut base = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        base.push_str(&format!(":{port}"));
    }
    let path = url.path().trim_end_matches('/');
    base.push_str(path);
    Ok(base)
}

/// The API origin to call: the server publishes its token endpoint under its
/// public URL, which may differ from what the user typed (e.g. an IP).
pub fn api_base(server: &str, discovery: &Discovery) -> String {
    discovery
        .oauth
        .token_url
        .strip_suffix("/api/v1/oauth/token")
        .and_then(|base| normalize_server_url(base).ok())
        .unwrap_or_else(|| server.to_owned())
}

fn parse_version(value: &str) -> Option<(u64, u64, u64, bool)> {
    let value = value.trim().trim_start_matches('v');
    let (core, pre) = match value.split_once('-') {
        Some((core, pre)) => (core, !pre.is_empty()),
        None => (value, false),
    };
    let core = core.split('+').next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch, pre))
}

/// `installed >= minimum` by SemVer precedence (a pre-release sorts first).
pub fn version_at_least(installed: &str, minimum: &str) -> bool {
    match (parse_version(installed), parse_version(minimum)) {
        (Some(a), Some(b)) => {
            (a.0, a.1, a.2) > (b.0, b.1, b.2)
                || ((a.0, a.1, a.2) == (b.0, b.1, b.2) && (!a.3 || b.3))
        }
        // An unparsable minimum is the server's mistake; do not lock users out over it.
        (_, None) => true,
        (None, Some(_)) => false,
    }
}

pub fn check_compatibility(discovery: &Discovery, app_version: &str) -> Result<(), String> {
    if discovery
        .product
        .as_deref()
        .is_some_and(|product| product != "hearfolio")
    {
        return Err("errors.orgNotHearfolio".into());
    }
    if discovery.api_version != API_VERSION {
        return Err(if discovery.api_version > API_VERSION {
            "errors.orgAppOutdated".into()
        } else {
            "errors.orgServerOutdated".into()
        });
    }
    if !version_at_least(app_version, &discovery.min_app_version) {
        return Err(format!(
            "errors.orgAppOutdated|{}",
            discovery.min_app_version
        ));
    }
    for endpoint in [
        &discovery.oauth.authorize_url,
        &discovery.oauth.token_url,
        &discovery.oauth.revoke_url,
    ] {
        normalize_server_url(endpoint).map_err(|_| "errors.orgDiscovery")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery(api_version: u64, min: &str) -> Discovery {
        Discovery {
            product: Some("hearfolio".into()),
            name: "Hearfolio".into(),
            server_version: "1.0.0".into(),
            api_version,
            min_app_version: min.into(),
            oauth: OAuthEndpoints {
                authorize_url: "https://cloud.example.com/oauth/authorize".into(),
                token_url: "https://cloud.example.com/api/v1/oauth/token".into(),
                revoke_url: "https://cloud.example.com/api/v1/oauth/revoke".into(),
            },
            upload: UploadLimits {
                multipart_threshold_bytes: 16 << 20,
                part_size_bytes: 8 << 20,
            },
        }
    }

    #[test]
    fn server_urls_are_normalized_and_validated() {
        for (input, expected) in [
            ("cloud.example.com", "https://cloud.example.com"),
            ("https://cloud.example.com/", "https://cloud.example.com"),
            (
                "  HTTPS://Cloud.Example.com:8443/hearfolio/ ",
                "https://cloud.example.com:8443/hearfolio",
            ),
            ("http://localhost:9000", "http://localhost:9000"),
            ("http://127.0.0.1:9000/", "http://127.0.0.1:9000"),
            ("http://192.168.1.20", "http://192.168.1.20"),
            ("http://nas.local:8080", "http://nas.local:8080"),
            ("http://[::1]:9000", "http://[::1]:9000"),
        ] {
            assert_eq!(
                normalize_server_url(input).as_deref(),
                Ok(expected),
                "{input}"
            );
        }
        for (input, error) in [
            ("", "errors.orgUrlInvalid"),
            ("ftp://cloud.example.com", "errors.orgUrlInvalid"),
            (
                "https://user:pass@cloud.example.com",
                "errors.orgUrlInvalid",
            ),
            ("https://cloud.example.com/?a=1", "errors.orgUrlInvalid"),
            ("https://cloud.example.com/#x", "errors.orgUrlInvalid"),
            ("https://cloud example.com", "errors.orgUrlInvalid"),
            ("http://cloud.example.com", "errors.orgUrlInsecure"),
            ("http://8.8.8.8", "errors.orgUrlInsecure"),
        ] {
            assert_eq!(
                normalize_server_url(input),
                Err(error.to_string()),
                "{input}"
            );
        }
    }

    #[test]
    fn versions_compare_by_semver_precedence() {
        assert!(version_at_least("0.10.0", "0.10.0"));
        assert!(version_at_least("0.11.2", "0.10.0"));
        assert!(version_at_least("1.0.0", "0.99.99"));
        assert!(!version_at_least("0.9.9", "0.10.0"));
        assert!(!version_at_least("0.10.0-beta.1", "0.10.0"));
        assert!(version_at_least("0.10.0", "0.10.0-beta.1"));
        assert!(version_at_least("v0.10.0", "0.10"));
        assert!(version_at_least("0.10.0", "garbage"));
        assert!(!version_at_least("garbage", "0.10.0"));
    }

    #[test]
    fn compatibility_reports_a_specific_reason() {
        assert_eq!(
            check_compatibility(&discovery(1, "0.10.0"), "0.10.0"),
            Ok(())
        );
        assert_eq!(
            check_compatibility(&discovery(1, "0.12.0"), "0.10.0"),
            Err("errors.orgAppOutdated|0.12.0".into())
        );
        assert_eq!(
            check_compatibility(&discovery(2, "0.1.0"), "0.10.0"),
            Err("errors.orgAppOutdated".into())
        );
        assert_eq!(
            check_compatibility(&discovery(0, "0.1.0"), "0.10.0"),
            Err("errors.orgServerOutdated".into())
        );
        let mut other = discovery(1, "0.1.0");
        other.product = Some("other".into());
        assert_eq!(
            check_compatibility(&other, "0.10.0"),
            Err("errors.orgNotHearfolio".into())
        );
        let mut insecure = discovery(1, "0.1.0");
        insecure.oauth.token_url = "http://cloud.example.com/api/v1/oauth/token".into();
        assert_eq!(
            check_compatibility(&insecure, "0.10.0"),
            Err("errors.orgDiscovery".into())
        );
    }

    #[test]
    fn api_base_follows_the_published_public_url() {
        let mut d = discovery(1, "0.1.0");
        assert_eq!(
            api_base("http://10.0.0.2:9000", &d),
            "https://cloud.example.com"
        );
        d.oauth.token_url = "https://weird.example.com/token".into();
        assert_eq!(api_base("http://10.0.0.2:9000", &d), "http://10.0.0.2:9000");
    }
}
