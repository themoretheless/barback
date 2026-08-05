//! The supported calendar services and how to connect to each.
//!
//! There are ten entries but only four engines, because the calendar world is
//! not ten separate APIs: Google and Microsoft each have their own REST API,
//! everything else worth integrating speaks CalDAV, and anything that speaks
//! neither exposes a plain `.ics` feed.
//!
//! Base URLs are starting points, not guarantees. Self-hosted software is
//! deployed at whatever path the operator chose, and even the hosted services
//! occasionally move theirs, which is why [`ProviderConfig::base_url`] always
//! wins over the preset.

use std::fmt;

use url::Url;

use super::auth::Auth;
use super::engines::caldav::CalDav;
use super::engines::google::GoogleCalendar;
use super::engines::graph::MicrosoftGraph;
use super::engines::ics::IcsFeed;
use super::error::{CalendarError, Result};
use super::http::{HttpClient, client};
use super::provider::BoxedProvider;

/// The protocol implementation backing a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Google Calendar API v3.
    Google,
    /// Microsoft Graph.
    Graph,
    /// CalDAV, RFC 4791.
    CalDav,
    /// Read-only iCalendar subscription.
    Ics,
}

/// What the provider expects in [`ProviderConfig::auth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// OAuth2 authorization code with refresh.
    OAuth2,
    /// Username plus an app-specific password. The account password will not
    /// work; these services require a generated one when 2FA is on.
    AppPassword,
    /// Username plus the account password, or an app password where the
    /// operator has configured one.
    Password,
    /// Public feed, no credentials.
    Anonymous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Google,
    Microsoft,
    AppleICloud,
    Fastmail,
    Yahoo,
    Zoho,
    Nextcloud,
    Zimbra,
    MailboxOrg,
    IcsFeed,
}

/// Static description of a provider.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub slug: &'static str,
    pub display_name: &'static str,
    pub engine: Engine,
    pub auth: AuthKind,
    /// Preset endpoint, used when the config does not override it.
    pub base_url: Option<&'static str>,
    /// True when there is no sensible default and the caller must supply one.
    pub base_url_required: bool,
    /// Anything a caller needs to know before it will work.
    pub notes: &'static str,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 10] = [
        ProviderKind::Google,
        ProviderKind::Microsoft,
        ProviderKind::AppleICloud,
        ProviderKind::Fastmail,
        ProviderKind::Yahoo,
        ProviderKind::Zoho,
        ProviderKind::Nextcloud,
        ProviderKind::Zimbra,
        ProviderKind::MailboxOrg,
        ProviderKind::IcsFeed,
    ];

    pub fn from_slug(slug: &str) -> Option<ProviderKind> {
        ProviderKind::ALL
            .into_iter()
            .find(|kind| kind.preset().slug.eq_ignore_ascii_case(slug))
    }

    pub fn preset(self) -> Preset {
        match self {
            ProviderKind::Google => Preset {
                slug: "google",
                display_name: "Google Calendar",
                engine: Engine::Google,
                auth: AuthKind::OAuth2,
                base_url: None,
                base_url_required: false,
                notes: "Needs the https://www.googleapis.com/auth/calendar scope. \
                        Google only issues a refresh token when the authorization \
                        request carries access_type=offline and prompt=consent.",
            },
            ProviderKind::Microsoft => Preset {
                slug: "microsoft",
                display_name: "Microsoft Outlook / Microsoft 365",
                engine: Engine::Graph,
                auth: AuthKind::OAuth2,
                base_url: None,
                base_url_required: false,
                notes: "Needs the Calendars.ReadWrite and offline_access scopes. \
                        Covers Outlook.com, Microsoft 365 and Exchange Online. \
                        Writing recurring events is not supported.",
            },
            ProviderKind::AppleICloud => Preset {
                slug: "apple-icloud",
                display_name: "Apple iCloud Calendar",
                engine: Engine::CalDav,
                auth: AuthKind::AppPassword,
                base_url: Some("https://caldav.icloud.com/"),
                base_url_required: false,
                notes: "Requires an app-specific password from appleid.apple.com; \
                        the Apple ID password is rejected when 2FA is on.",
            },
            ProviderKind::Fastmail => Preset {
                slug: "fastmail",
                display_name: "Fastmail",
                engine: Engine::CalDav,
                auth: AuthKind::AppPassword,
                base_url: Some("https://caldav.fastmail.com/dav/"),
                base_url_required: false,
                notes: "Requires an app password scoped to CalDAV, created in \
                        Settings > Privacy & Security > Integrations.",
            },
            ProviderKind::Yahoo => Preset {
                slug: "yahoo",
                display_name: "Yahoo Calendar",
                engine: Engine::CalDav,
                auth: AuthKind::AppPassword,
                base_url: Some("https://caldav.calendar.yahoo.com/"),
                base_url_required: false,
                notes: "Requires an app password from Yahoo account security.",
            },
            ProviderKind::Zoho => Preset {
                slug: "zoho",
                display_name: "Zoho Calendar",
                engine: Engine::CalDav,
                auth: AuthKind::AppPassword,
                base_url: Some("https://calendar.zoho.com/caldav/"),
                base_url_required: false,
                notes: "Requires an application-specific password. Zoho serves \
                        regional data centres (zoho.eu, zoho.in) from different \
                        hosts, so set base_url explicitly outside the US region.",
            },
            ProviderKind::Nextcloud => Preset {
                slug: "nextcloud",
                display_name: "Nextcloud",
                engine: Engine::CalDav,
                auth: AuthKind::Password,
                base_url: None,
                base_url_required: true,
                notes: "Self-hosted: pass the instance URL, normally \
                        https://<host>/remote.php/dav/. An app password from \
                        Settings > Security is preferred over the login password.",
            },
            ProviderKind::Zimbra => Preset {
                slug: "zimbra",
                display_name: "Zimbra",
                engine: Engine::CalDav,
                auth: AuthKind::Password,
                base_url: None,
                base_url_required: true,
                notes: "Self-hosted: pass the server's DAV endpoint, commonly \
                        https://<host>/dav/. Deployments vary; discovery starts \
                        from whatever URL is supplied.",
            },
            ProviderKind::MailboxOrg => Preset {
                slug: "mailbox-org",
                display_name: "mailbox.org (Open-Xchange)",
                engine: Engine::CalDav,
                auth: AuthKind::AppPassword,
                base_url: Some("https://dav.mailbox.org/caldav/"),
                base_url_required: false,
                notes: "Open-Xchange based. Other OX hosts work through the same \
                        engine by overriding base_url.",
            },
            ProviderKind::IcsFeed => Preset {
                slug: "ics-feed",
                display_name: "iCalendar feed (webcal/.ics)",
                engine: Engine::Ics,
                auth: AuthKind::Anonymous,
                base_url: None,
                base_url_required: true,
                notes: "Read-only. Covers Proton Calendar exports, Calendly, \
                        public Google and Outlook share links, and any other \
                        published .ics URL.",
            },
        }
    }

    /// OAuth2 authorization and token endpoints plus the scopes this
    /// integration needs, for the providers that use OAuth2.
    pub fn oauth_endpoints(self) -> Option<(&'static str, &'static str, &'static [&'static str])> {
        match self {
            ProviderKind::Google => Some((
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                &["https://www.googleapis.com/auth/calendar"],
            )),
            ProviderKind::Microsoft => Some((
                "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
                "https://login.microsoftonline.com/common/oauth2/v2.0/token",
                &["Calendars.ReadWrite", "offline_access"],
            )),
            _ => None,
        }
    }

    /// Builds a live provider from credentials.
    pub fn connect(self, config: ProviderConfig) -> Result<BoxedProvider> {
        let preset = self.preset();
        let http_client = match config.http_client {
            Some(existing) => existing,
            None => client()?,
        };

        self.check_auth(&config.auth)?;
        let http = HttpClient::new(http_client, config.auth, preset.slug);

        Ok(match preset.engine {
            // These two have a single public address, so an override is only
            // for Graph's national clouds and for testing against a stub.
            Engine::Google => match config.base_url.as_deref() {
                Some(base) => Box::new(GoogleCalendar::with_base_url(http, base)),
                None => Box::new(GoogleCalendar::new(http)),
            },
            Engine::Graph => match config.base_url.as_deref() {
                Some(base) => Box::new(MicrosoftGraph::with_base_url(http, base)),
                None => Box::new(MicrosoftGraph::new(http)),
            },
            Engine::CalDav => {
                let base = self.resolve_base_url(config.base_url.as_deref())?;
                let url = Url::parse(&base).map_err(CalendarError::from)?;
                Box::new(CalDav::new(http, url, preset.slug))
            }
            Engine::Ics => {
                let base = self.resolve_base_url(config.base_url.as_deref())?;
                Box::new(IcsFeed::new(http, &base, config.display_name)?)
            }
        })
    }

    fn resolve_base_url(self, override_url: Option<&str>) -> Result<String> {
        let preset = self.preset();
        match (override_url, preset.base_url) {
            (Some(url), _) => Ok(url.to_string()),
            (None, Some(default)) => Ok(default.to_string()),
            (None, None) => Err(CalendarError::Config(format!(
                "{} requires an explicit base_url: {}",
                preset.display_name, preset.notes
            ))),
        }
    }

    /// Rejects credentials the engine cannot possibly use, so the failure is a
    /// clear config error rather than a 401 from the server.
    fn check_auth(self, auth: &Auth) -> Result<()> {
        let preset = self.preset();
        let ok = match (preset.engine, auth) {
            (Engine::Google | Engine::Graph, Auth::OAuth2(_) | Auth::Bearer(_)) => true,
            (Engine::CalDav, Auth::Basic { .. } | Auth::Bearer(_)) => true,
            // A private feed behind Basic auth is common enough to allow.
            (Engine::Ics, _) => true,
            _ => false,
        };

        if ok {
            return Ok(());
        }
        Err(CalendarError::Config(format!(
            "{} expects {} credentials",
            preset.display_name,
            match preset.auth {
                AuthKind::OAuth2 => "OAuth2 or bearer-token",
                AuthKind::AppPassword => "username plus app-specific password",
                AuthKind::Password => "username plus password",
                AuthKind::Anonymous => "no",
            }
        )))
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.preset().display_name)
    }
}

/// Everything needed to instantiate one provider.
#[derive(Default)]
pub struct ProviderConfig {
    pub auth: Auth,
    /// Overrides the preset endpoint. Required for self-hosted software and for
    /// ICS feeds, where there is no such thing as a default.
    pub base_url: Option<String>,
    /// Name to show for an ICS feed when the feed itself does not carry one.
    pub display_name: Option<String>,
    /// Reuse an existing client instead of building one. Worth doing when
    /// connecting several providers, so they share a connection pool.
    pub http_client: Option<reqwest::Client>,
}

impl ProviderConfig {
    pub fn new(auth: Auth) -> Self {
        ProviderConfig {
            auth,
            ..Default::default()
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_has_a_unique_slug() {
        let mut slugs: Vec<&str> = ProviderKind::ALL.iter().map(|k| k.preset().slug).collect();
        slugs.sort_unstable();
        let count = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), count, "duplicate slug in the registry");
        assert_eq!(count, 10);
    }

    #[test]
    fn slugs_round_trip() {
        for kind in ProviderKind::ALL {
            assert_eq!(ProviderKind::from_slug(kind.preset().slug), Some(kind));
        }
        assert_eq!(
            ProviderKind::from_slug("APPLE-ICLOUD"),
            Some(ProviderKind::AppleICloud)
        );
        assert_eq!(ProviderKind::from_slug("nope"), None);
    }

    #[test]
    fn presets_are_internally_consistent() {
        for kind in ProviderKind::ALL {
            let preset = kind.preset();
            assert!(!preset.notes.is_empty(), "{} has no notes", preset.slug);
            // Either there is a default endpoint or the caller must supply one.
            assert_eq!(
                preset.base_url.is_none(),
                preset.base_url_required || matches!(preset.engine, Engine::Google | Engine::Graph),
                "{} has an inconsistent base_url policy",
                preset.slug
            );
            if let Some(url) = preset.base_url {
                assert!(
                    Url::parse(url).is_ok(),
                    "{} has an invalid base_url",
                    preset.slug
                );
            }
            assert_eq!(
                kind.oauth_endpoints().is_some(),
                preset.auth == AuthKind::OAuth2,
                "{} disagrees about OAuth2",
                preset.slug
            );
        }
    }

    #[test]
    fn caldav_providers_outnumber_the_rest() {
        let caldav = ProviderKind::ALL
            .iter()
            .filter(|k| k.preset().engine == Engine::CalDav)
            .count();
        assert_eq!(caldav, 7);
    }

    #[test]
    fn oauth_scopes_are_present_for_oauth_providers() {
        let (auth_url, token_url, scopes) = ProviderKind::Google.oauth_endpoints().unwrap();
        assert!(auth_url.starts_with("https://"));
        assert!(token_url.starts_with("https://"));
        assert!(scopes.contains(&"https://www.googleapis.com/auth/calendar"));

        let (_, _, scopes) = ProviderKind::Microsoft.oauth_endpoints().unwrap();
        assert!(scopes.contains(&"offline_access"));
    }

    #[test]
    fn google_rejects_basic_credentials() {
        let err = ProviderKind::Google
            .connect(ProviderConfig::new(Auth::basic("ada", "hunter2")))
            .err()
            .expect("basic auth must be rejected");
        assert!(matches!(err, CalendarError::Config(_)));
    }

    #[test]
    fn caldav_rejects_missing_credentials() {
        let err = ProviderKind::AppleICloud
            .connect(ProviderConfig::new(Auth::None))
            .err()
            .expect("anonymous CalDAV must be rejected");
        assert!(matches!(err, CalendarError::Config(_)));
    }

    #[test]
    fn self_hosted_providers_demand_a_base_url() {
        let err = ProviderKind::Nextcloud
            .connect(ProviderConfig::new(Auth::basic("ada", "app-password")))
            .err()
            .expect("a missing base_url must be rejected");
        match err {
            CalendarError::Config(message) => assert!(message.contains("base_url")),
            other => panic!("expected a config error, got {other:?}"),
        }
    }

    #[test]
    fn hosted_caldav_providers_connect_from_the_preset_alone() {
        for kind in [
            ProviderKind::AppleICloud,
            ProviderKind::Fastmail,
            ProviderKind::Yahoo,
            ProviderKind::Zoho,
            ProviderKind::MailboxOrg,
        ] {
            let provider = kind
                .connect(ProviderConfig::new(Auth::basic("ada", "app-password")))
                .unwrap_or_else(|e| panic!("{} failed to connect: {e}", kind.preset().slug));
            assert_eq!(provider.name(), kind.preset().slug);
            assert!(provider.capabilities().write);
        }
    }

    #[test]
    fn ics_feed_connects_and_is_read_only() {
        let provider = ProviderKind::IcsFeed
            .connect(
                ProviderConfig::new(Auth::None)
                    .with_base_url("webcal://example.com/feed.ics")
                    .with_display_name("Bundesliga"),
            )
            .unwrap();
        assert_eq!(provider.name(), "ics-feed");
        assert!(!provider.capabilities().write);
    }

    #[test]
    fn base_url_override_wins_over_the_preset() {
        let provider = ProviderKind::Zoho
            .connect(
                ProviderConfig::new(Auth::basic("ada", "pw"))
                    .with_base_url("https://calendar.zoho.eu/caldav/"),
            )
            .unwrap();
        assert_eq!(provider.name(), "zoho");
    }

    #[test]
    fn oauth_providers_connect_with_a_bearer_token() {
        for kind in [ProviderKind::Google, ProviderKind::Microsoft] {
            let provider = kind
                .connect(ProviderConfig::new(Auth::Bearer("token".into())))
                .unwrap();
            assert!(provider.capabilities().write);
        }
    }
}
