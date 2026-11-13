//! Hearfolio Cloud API v1 over blocking reqwest. Errors become `errors.org*`
//! keys for the UI; response bodies and tokens are never logged or surfaced.
use super::upload::Failure;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

pub fn client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(None)
            .user_agent(concat!("Hearfolio/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::custom(redirect_policy))
            .build()
            .expect("HTTP client")
    })
}

/// Redirects may only stay on the same host and never downgrade HTTPS: the
/// `Organization-Key` header and request bodies must not reach another server.
fn redirect_policy(attempt: reqwest::redirect::Attempt) -> reqwest::redirect::Action {
    let allowed = attempt
        .previous()
        .last()
        .is_some_and(|from| redirect_allowed(from, attempt.url()));
    if attempt.previous().len() >= 3 || !allowed {
        attempt.stop()
    } else {
        attempt.follow()
    }
}
pub(crate) fn redirect_allowed(from: &url::Url, to: &url::Url) -> bool {
    from.host_str() == to.host_str()
        && (from.scheme() == to.scheme() || from.scheme() == "http" && to.scheme() == "https")
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    Offline,
    Status(u16),
    Invalid,
}

impl ApiError {
    pub fn key(&self) -> String {
        match self {
            ApiError::Offline => "errors.orgOffline".into(),
            ApiError::Invalid => "errors.orgResponse".into(),
            ApiError::Status(401) => "errors.orgUnauthorized".into(),
            ApiError::Status(403) => "errors.orgForbidden".into(),
            ApiError::Status(404) => "errors.orgNotFound".into(),
            ApiError::Status(409) => "errors.orgConflict".into(),
            ApiError::Status(413) => "errors.orgTooLarge".into(),
            ApiError::Status(429) => "errors.orgRateLimited".into(),
            ApiError::Status(status) if *status >= 500 => "errors.orgServer".into(),
            ApiError::Status(status) => format!("errors.orgRequest|HTTP {status}"),
        }
    }
    /// How the upload queue treats this error.
    pub fn failure(&self) -> Failure {
        match self {
            ApiError::Offline => Failure::Offline,
            ApiError::Status(408 | 425 | 429) | ApiError::Invalid => Failure::Transient(self.key()),
            ApiError::Status(status) if *status >= 500 => Failure::Transient(self.key()),
            _ => Failure::Permanent(self.key()),
        }
    }
}

pub fn from_reqwest(error: &reqwest::Error) -> ApiError {
    if let Some(status) = error.status() {
        return ApiError::Status(status.as_u16());
    }
    if error.is_decode() {
        return ApiError::Invalid;
    }
    // Connect failures, DNS, timeouts and dropped bodies: the network, not the server.
    ApiError::Offline
}

pub struct Api<'a> {
    pub base: &'a str,
    pub token: &'a str,
    pub organization_key: Option<&'a str>,
}

impl Api<'_> {
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let mut request = client()
            .request(method, format!("{}/api/v1{path}", self.base))
            .header("Authorization", format!("Token {}", self.token))
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(45));
        if let Some(key) = self.organization_key {
            request = request.header("Organization-Key", key);
        }
        request
    }
    fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<T, ApiError> {
        let response = request.send().map_err(|error| from_reqwest(&error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ApiError::Status(status.as_u16()));
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return serde_json::from_value(Value::Null).map_err(|_| ApiError::Invalid);
        }
        response.json::<T>().map_err(|error| from_reqwest(&error))
    }
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.send(self.request(reqwest::Method::GET, path))
    }
    pub fn post<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, ApiError> {
        self.send(self.request(reqwest::Method::POST, path).json(body))
    }
    /// POST that also returns the error envelope's `details` (e.g. `reason`).
    pub fn post_detailed<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, (ApiError, Value)> {
        let response = self
            .request(reqwest::Method::POST, path)
            .json(body)
            .send()
            .map_err(|error| (from_reqwest(&error), Value::Null))?;
        let status = response.status();
        if !status.is_success() {
            let envelope: Value = response.json().unwrap_or(Value::Null);
            return Err((
                ApiError::Status(status.as_u16()),
                envelope["error"]["details"].clone(),
            ));
        }
        response
            .json::<T>()
            .map_err(|error| (from_reqwest(&error), Value::Null))
    }
    pub fn delete(&self, path: &str) -> Result<(), ApiError> {
        self.send::<Value>(self.request(reqwest::Method::DELETE, path))
            .map(|_| ())
    }
}

pub fn discovery(server: &str) -> Result<super::server::Discovery, String> {
    let response = client()
        .get(format!("{server}/.well-known/hearfolio"))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .map_err(|error| match from_reqwest(&error) {
            ApiError::Offline => "errors.orgUnreachable".to_owned(),
            other => other.key(),
        })?;
    if !response.status().is_success() {
        return Err("errors.orgNotHearfolio".into());
    }
    response
        .json::<super::server::Discovery>()
        .map_err(|_| "errors.orgNotHearfolio".into())
}

/// RFC 6749 token response or error body.
pub fn exchange_code(
    token_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Value, String> {
    let response = client()
        .post(token_url)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("client_id", super::pkce::CLIENT_ID),
        ])
        .send()
        .map_err(|error| from_reqwest(&error).key())?;
    let status = response.status();
    let body: Value = response.json().unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(match body["error"].as_str() {
            Some("invalid_grant") => "errors.orgAuthExpired".into(),
            Some("rate_limited") => "errors.orgRateLimited".into(),
            _ => "errors.orgAuthFailed".into(),
        });
    }
    if body["access_token"].as_str().is_none_or(str::is_empty) {
        return Err("errors.orgResponse".into());
    }
    Ok(body)
}

pub fn revoke(revoke_url: &str, token: &str) -> Result<(), ApiError> {
    let response = client()
        .post(revoke_url)
        .header("Authorization", format!("Token {token}"))
        .timeout(Duration::from_secs(15))
        .send()
        .map_err(|error| from_reqwest(&error))?;
    match response.status().as_u16() {
        200..=299 | 401 => Ok(()),
        status => Err(ApiError::Status(status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redirects_stay_on_the_host_and_never_downgrade() {
        let url = |value: &str| url::Url::parse(value).unwrap();
        assert!(redirect_allowed(
            &url("http://a.test/x"),
            &url("https://a.test/y")
        ));
        assert!(redirect_allowed(
            &url("https://a.test/x"),
            &url("https://a.test/y")
        ));
        assert!(!redirect_allowed(
            &url("https://a.test/x"),
            &url("http://a.test/y")
        ));
        assert!(!redirect_allowed(
            &url("https://a.test/x"),
            &url("https://evil.test/y")
        ));
    }

    #[test]
    fn statuses_map_to_queue_failures() {
        assert_eq!(ApiError::Offline.failure(), Failure::Offline);
        assert!(matches!(
            ApiError::Status(503).failure(),
            Failure::Transient(_)
        ));
        assert!(matches!(
            ApiError::Status(429).failure(),
            Failure::Transient(_)
        ));
        assert_eq!(
            ApiError::Status(401).failure(),
            Failure::Permanent("errors.orgUnauthorized".into())
        );
        assert_eq!(ApiError::Status(422).key(), "errors.orgRequest|HTTP 422");
    }
}
