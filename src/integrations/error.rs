use std::time::Duration;

pub type Result<T> = std::result::Result<T, CalendarError>;

#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
    #[error("transport error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("{provider} returned HTTP {status}: {body}")]
    Status {
        provider: &'static str,
        status: u16,
        body: String,
    },

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("{provider} does not support {operation}")]
    Unsupported {
        provider: &'static str,
        operation: &'static str,
    },

    #[error("could not parse {kind}: {detail}")]
    Parse { kind: &'static str, detail: String },

    #[error("malformed XML: {0}")]
    Xml(#[from] quick_xml::Error),

    #[error("malformed JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict, the item changed on the server: {0}")]
    Conflict(String),

    #[error("rate limited by the server")]
    RateLimited { retry_after: Option<Duration> },

    #[error("misconfigured provider: {0}")]
    Config(String),

    #[error("calendar discovery failed: {0}")]
    Discovery(String),
}

impl CalendarError {
    pub fn parse(kind: &'static str, detail: impl Into<String>) -> Self {
        CalendarError::Parse {
            kind,
            detail: detail.into(),
        }
    }

    /// True when retrying the same request later has a realistic chance of succeeding.
    pub fn is_transient(&self) -> bool {
        match self {
            CalendarError::RateLimited { .. } => true,
            CalendarError::Http(e) => e.is_timeout() || e.is_connect(),
            CalendarError::Status { status, .. } => *status >= 500,
            _ => false,
        }
    }
}
