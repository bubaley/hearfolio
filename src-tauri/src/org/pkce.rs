//! OAuth 2.0 authorization code + PKCE (RFC 7636, S256) for the native app.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub const CLIENT_ID: &str = "hearfolio-app";
pub const APP_REDIRECT_URI: &str = "hearfolio://oauth/callback";

fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

/// 48 random bytes → 64 characters of `[A-Za-z0-9_-]` (RFC 7636 allows 43–128).
pub fn generate_verifier() -> String {
    random_token(48)
}

/// Opaque CSRF `state` binding the browser round trip to this flow.
pub fn generate_state() -> String {
    random_token(32)
}

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
pub fn verify(verifier: &str, challenge_value: &str) -> bool {
    valid_verifier(verifier) && challenge(verifier) == challenge_value
}

pub fn valid_verifier(verifier: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

pub fn loopback_redirect(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

pub fn authorize_url(
    authorize_endpoint: &str,
    redirect_uri: &str,
    verifier: &str,
    state: &str,
    device_name: &str,
) -> Result<String, String> {
    if !valid_verifier(verifier) {
        return Err("errors.orgInvalid".into());
    }
    let mut url = url::Url::parse(authorize_endpoint).map_err(|_| "errors.orgDiscovery")?;
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", &challenge(verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("device_name", device_name);
    Ok(url.into())
}

#[derive(Debug, PartialEq)]
pub enum Callback {
    Code { code: String, state: String },
    Denied { state: Option<String> },
}

/// Parse `hearfolio://oauth/callback?code&state` or the loopback request target
/// (`/callback?code&state`). Anything else is not ours.
pub fn parse_callback(raw: &str) -> Option<Callback> {
    let url = if raw.starts_with('/') {
        url::Url::parse(&format!("http://127.0.0.1{raw}")).ok()?
    } else {
        url::Url::parse(raw).ok()?
    };
    let ours = match url.scheme() {
        "hearfolio" => url.host_str() == Some("oauth") && url.path() == "/callback",
        "http" => url.path() == "/callback",
        _ => false,
    };
    if !ours {
        return None;
    }
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let state = pairs
        .get("state")
        .cloned()
        .filter(|value| !value.is_empty());
    if pairs.contains_key("error") {
        return Some(Callback::Denied { state });
    }
    let code = pairs
        .get("code")
        .cloned()
        .filter(|value| !value.is_empty())?;
    Some(Callback::Code {
        code,
        state: state?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_and_challenge_follow_rfc_7636() {
        let verifier = generate_verifier();
        assert_eq!(verifier.len(), 64);
        assert!(valid_verifier(&verifier));
        let value = challenge(&verifier);
        assert_eq!(value.len(), 43);
        assert!(value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert!(verify(&verifier, &value));
        assert!(!verify(&generate_verifier(), &value));
        assert_ne!(generate_verifier(), verifier);
        assert_ne!(generate_state(), generate_state());
        // RFC 7636 appendix B test vector.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(!valid_verifier("short"));
        assert!(!valid_verifier(&"a".repeat(129)));
        assert!(!valid_verifier(&format!("{}+", "a".repeat(50))));
    }

    #[test]
    fn authorize_url_carries_every_parameter_encoded() {
        let url = authorize_url(
            "https://cloud.example.com/oauth/authorize",
            APP_REDIRECT_URI,
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "xyz",
            "MacBook Анны",
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs["client_id"], CLIENT_ID);
        assert_eq!(pairs["redirect_uri"], "hearfolio://oauth/callback");
        assert_eq!(
            pairs["code_challenge"],
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["state"], "xyz");
        assert_eq!(pairs["device_name"], "MacBook Анны");
    }

    #[test]
    fn callbacks_are_recognized_only_on_our_paths() {
        assert_eq!(
            parse_callback("hearfolio://oauth/callback?code=abc&state=s1"),
            Some(Callback::Code {
                code: "abc".into(),
                state: "s1".into()
            })
        );
        assert_eq!(
            parse_callback("/callback?code=abc&state=s1"),
            Some(Callback::Code {
                code: "abc".into(),
                state: "s1".into()
            })
        );
        assert_eq!(
            parse_callback("hearfolio://oauth/callback?error=access_denied&state=s1"),
            Some(Callback::Denied {
                state: Some("s1".into())
            })
        );
        assert_eq!(parse_callback("hearfolio://oauth/callback?code=abc"), None);
        assert_eq!(parse_callback("other-scheme://pair?code=1"), None);
        assert_eq!(
            parse_callback("hearfolio://other/callback?code=a&state=b"),
            None
        );
        assert_eq!(parse_callback("/favicon.ico"), None);
    }
}
