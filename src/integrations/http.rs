//! Thin HTTP layer shared by the engines: authentication, status-to-error
//! mapping, and a single place to set timeouts.

use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response, StatusCode};

use super::auth::Auth;
use super::error::{CalendarError, Result};

const USER_AGENT: &str = concat!("barback/", env!("CARGO_PKG_VERSION"));
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds the shared `reqwest::Client`. One client per process is enough; it
/// pools connections internally.
pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(CalendarError::from)
}

#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
    auth: Auth,
    provider: &'static str,
}

impl HttpClient {
    pub fn new(inner: reqwest::Client, auth: Auth, provider: &'static str) -> Self {
        HttpClient {
            inner,
            auth,
            provider,
        }
    }

    pub fn raw(&self) -> &reqwest::Client {
        &self.inner
    }

    pub fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.inner.request(method, url)
    }

    pub fn get(&self, url: &str) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Sends an authenticated request and turns non-2xx responses into typed
    /// errors. 3xx is treated as an error too, since `reqwest` already follows
    /// the redirects worth following.
    pub async fn send(&self, request: RequestBuilder) -> Result<Response> {
        let request = self.auth.apply(request).await?;
        let response = request.send().await?;
        self.check(response).await
    }

    async fn check(&self, response: Response) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);

        let url = response.url().to_string();
        // The body is the only place most providers explain themselves, and it
        // is bounded, so it is always read.
        let body = response.text().await.unwrap_or_default();
        let body = truncate(&body, 2000);

        Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => CalendarError::Auth(format!(
                "{} rejected the credentials for {url}: {body}",
                self.provider
            )),
            StatusCode::NOT_FOUND | StatusCode::GONE => CalendarError::NotFound(url),
            StatusCode::CONFLICT
            | StatusCode::PRECONDITION_FAILED
            | StatusCode::PRECONDITION_REQUIRED => {
                CalendarError::Conflict(format!("{url}: {body}"))
            }
            StatusCode::TOO_MANY_REQUESTS => CalendarError::RateLimited { retry_after },
            _ => CalendarError::Status {
                provider: self.provider,
                status: status.as_u16(),
                body,
            },
        })
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &value[..end], value.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        let value = "ы".repeat(100);
        let out = truncate(&value, 15);
        assert!(out.starts_with("ыыыыыыы"));
        assert!(out.contains("200 bytes total"));
    }

    #[test]
    fn short_bodies_are_untouched() {
        assert_eq!(truncate("ok", 10), "ok");
    }

    #[test]
    fn client_builds() {
        assert!(client().is_ok());
    }
}
