use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use reqwest::RequestBuilder;
use serde::Deserialize;
use tokio::sync::RwLock;

use super::error::{CalendarError, Result};

/// Refresh this long before the server-stated expiry, so a token does not
/// expire while a request is in flight.
const REFRESH_SKEW: Duration = Duration::seconds(60);

/// OAuth2 endpoints and client registration for one provider.
#[derive(Debug, Clone)]
pub struct OAuth2Config {
    pub client_id: String,
    /// Absent for public clients using PKCE.
    pub client_secret: Option<String>,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Default)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl TokenSet {
    pub fn from_access_token(token: impl Into<String>) -> Self {
        TokenSet {
            access_token: token.into(),
            refresh_token: None,
            expires_at: None,
        }
    }

    fn is_fresh(&self) -> bool {
        if self.access_token.is_empty() {
            return false;
        }
        match self.expires_at {
            // No stated expiry: assume the caller knows what it handed us.
            None => true,
            Some(exp) => Utc::now() + REFRESH_SKEW < exp,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// An OAuth2 authorization-code client with automatic refresh.
///
/// Shared behind an `Arc` because a single token set usually backs several
/// provider instances for the same account.
#[derive(Debug)]
pub struct OAuth2 {
    config: OAuth2Config,
    tokens: RwLock<TokenSet>,
    http: reqwest::Client,
}

impl OAuth2 {
    pub fn new(config: OAuth2Config, tokens: TokenSet, http: reqwest::Client) -> Arc<Self> {
        Arc::new(OAuth2 {
            config,
            tokens: RwLock::new(tokens),
            http,
        })
    }

    pub fn config(&self) -> &OAuth2Config {
        &self.config
    }

    /// URL to send the user to in a browser. `state` must be verified when the
    /// redirect comes back.
    pub fn authorize_url(&self, state: &str) -> String {
        let mut url = self.config.auth_url.clone();
        let sep = if url.contains('?') { '&' } else { '?' };
        url.push(sep);
        let scopes = self.config.scopes.join(" ");
        let params = [
            ("client_id", self.config.client_id.as_str()),
            ("redirect_uri", self.config.redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", scopes.as_str()),
            ("state", state),
            // Google only returns a refresh token when both are set.
            ("access_type", "offline"),
            ("prompt", "consent"),
        ];
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    k,
                    percent_encoding::utf8_percent_encode(v, percent_encoding::NON_ALPHANUMERIC)
                )
            })
            .collect();
        url.push_str(&query.join("&"));
        url
    }

    /// Exchanges the code from the redirect for a token set.
    pub async fn exchange_code(&self, code: &str) -> Result<()> {
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("client_id", self.config.client_id.clone()),
            ("redirect_uri", self.config.redirect_uri.clone()),
        ];
        if let Some(secret) = &self.config.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let new_tokens = self.post_token(&form, None).await?;
        *self.tokens.write().await = new_tokens;
        Ok(())
    }

    /// Current access token, refreshing first if it is expired or close to it.
    pub async fn access_token(&self) -> Result<String> {
        {
            let tokens = self.tokens.read().await;
            if tokens.is_fresh() {
                return Ok(tokens.access_token.clone());
            }
        }

        let mut tokens = self.tokens.write().await;
        // Another task may have refreshed while this one waited for the lock.
        if tokens.is_fresh() {
            return Ok(tokens.access_token.clone());
        }

        let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
            CalendarError::Auth(
                "access token expired and no refresh token is available; re-authorize".into(),
            )
        })?;

        let mut form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.clone()),
            ("client_id", self.config.client_id.clone()),
        ];
        if let Some(secret) = &self.config.client_secret {
            form.push(("client_secret", secret.clone()));
        }

        // Refresh responses routinely omit the refresh token; keep the old one.
        let refreshed = self.post_token(&form, Some(refresh_token)).await?;
        *tokens = refreshed;
        Ok(tokens.access_token.clone())
    }

    /// Replaces the stored tokens, e.g. after loading them from a keychain.
    pub async fn set_tokens(&self, tokens: TokenSet) {
        *self.tokens.write().await = tokens;
    }

    /// Snapshot of the current tokens, for persisting them.
    pub async fn tokens(&self) -> TokenSet {
        self.tokens.read().await.clone()
    }

    async fn post_token(
        &self,
        form: &[(&str, String)],
        fallback_refresh_token: Option<String>,
    ) -> Result<TokenSet> {
        let response = self
            .http
            .post(&self.config.token_url)
            .form(form)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            let detail = serde_json::from_str::<TokenErrorResponse>(&body)
                .ok()
                .and_then(|e| e.error_description.or(e.error))
                .unwrap_or_else(|| body.clone());
            return Err(CalendarError::Auth(format!("token endpoint: {detail}")));
        }

        let parsed: TokenResponse = serde_json::from_str(&body)?;
        Ok(TokenSet {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token.or(fallback_refresh_token),
            expires_at: parsed
                .expires_in
                .map(|secs| Utc::now() + Duration::seconds(secs)),
        })
    }
}

/// How a provider authenticates its requests.
#[derive(Debug, Clone, Default)]
pub enum Auth {
    #[default]
    None,
    /// Username plus password or, for iCloud/Fastmail/Yahoo, an app-specific
    /// password. Used by every CalDAV provider here.
    Basic {
        username: String,
        password: String,
    },
    /// A pre-obtained token with no refresh handling.
    Bearer(String),
    OAuth2(Arc<OAuth2>),
}

impl Auth {
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Auth::Basic {
            username: username.into(),
            password: password.into(),
        }
    }

    /// Attaches credentials to an outgoing request, refreshing OAuth tokens if
    /// needed.
    pub async fn apply(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        Ok(match self {
            Auth::None => request,
            Auth::Basic { username, password } => request.basic_auth(username, Some(password)),
            Auth::Bearer(token) => request.bearer_auth(token),
            Auth::OAuth2(client) => request.bearer_auth(client.access_token().await?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OAuth2Config {
        OAuth2Config {
            client_id: "client-123".into(),
            client_secret: Some("secret".into()),
            auth_url: "https://accounts.example.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.example.com/token".into(),
            scopes: vec!["https://www.googleapis.com/auth/calendar".into()],
            redirect_uri: "http://localhost:8080/callback".into(),
        }
    }

    #[test]
    fn authorize_url_encodes_scopes_and_redirect() {
        let client = OAuth2::new(config(), TokenSet::default(), reqwest::Client::new());
        let url = client.authorize_url("xyz");
        assert!(url.starts_with("https://accounts.example.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=client%2D123"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback"));
        assert!(url.contains("state=xyz"));
        assert!(url.contains("access_type=offline"));
    }

    #[test]
    fn token_without_expiry_is_treated_as_fresh() {
        let tokens = TokenSet::from_access_token("abc");
        assert!(tokens.is_fresh());
    }

    #[test]
    fn token_inside_skew_window_is_stale() {
        let tokens = TokenSet {
            access_token: "abc".into(),
            refresh_token: Some("r".into()),
            expires_at: Some(Utc::now() + Duration::seconds(30)),
        };
        assert!(!tokens.is_fresh());
    }

    #[test]
    fn empty_token_is_never_fresh() {
        assert!(!TokenSet::default().is_fresh());
    }

    #[tokio::test]
    async fn refresh_without_refresh_token_is_an_auth_error() {
        let expired = TokenSet {
            access_token: "abc".into(),
            refresh_token: None,
            expires_at: Some(Utc::now() - Duration::seconds(10)),
        };
        let client = OAuth2::new(config(), expired, reqwest::Client::new());
        let err = client.access_token().await.unwrap_err();
        assert!(matches!(err, CalendarError::Auth(_)));
    }
}
