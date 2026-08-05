//! Small CLI for exercising the calendar integrations by hand.
//!
//! ```text
//! barback providers
//! barback oauth-url --provider google --client-id ID --redirect-uri URL
//! barback list --provider apple-icloud --user ada@icloud.com --days 7
//! barback list --provider ics-feed --base-url https://example.com/feed.ics
//! ```
//!
//! Credentials are read from `BARBACK_PASSWORD` / `BARBACK_TOKEN` by
//! preference. The equivalent flags exist but put the secret in the process
//! list, where anyone on the machine can read it.

use std::collections::HashMap;
use std::process::ExitCode;

use barback::calendar::providers::{AuthKind, Engine, ProviderConfig, ProviderKind};
use barback::calendar::{Auth, Result, TimeRange};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        print_usage();
        return ExitCode::FAILURE;
    };

    let flags = parse_flags(&args[1..]);

    let outcome = match command {
        "providers" => {
            print_providers();
            Ok(())
        }
        "oauth-url" => print_oauth_url(&flags),
        "list" => list_events(&flags).await,
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            return ExitCode::FAILURE;
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "usage:
  barback providers
      List the supported calendar services.

  barback oauth-url --provider <slug> --client-id <id> --redirect-uri <url>
      Print the authorization URL to open in a browser.

  barback list --provider <slug> [options]
      List upcoming events.

      --base-url <url>    Required for nextcloud, zimbra and ics-feed.
      --user <name>       Username for CalDAV providers.
      --password <pw>     Prefer the BARBACK_PASSWORD environment variable.
      --token <token>     Prefer the BARBACK_TOKEN environment variable.
      --calendar <id>     Default: every calendar in the account.
      --days <n>          Window size, default 7."
    );
}

fn print_providers() {
    for kind in ProviderKind::ALL {
        let preset = kind.preset();
        let engine = match preset.engine {
            Engine::Google => "Google Calendar API",
            Engine::Graph => "Microsoft Graph",
            Engine::CalDav => "CalDAV",
            Engine::Ics => "iCalendar feed (read-only)",
        };
        let auth = match preset.auth {
            AuthKind::OAuth2 => "OAuth2",
            AuthKind::AppPassword => "app-specific password",
            AuthKind::Password => "username + password",
            AuthKind::Anonymous => "none",
        };

        println!("{:<14} {}", preset.slug, preset.display_name);
        println!("{:<14} engine: {engine}, auth: {auth}", "");
        match preset.base_url {
            Some(url) => println!("{:<14} endpoint: {url}", ""),
            None if preset.base_url_required => {
                println!("{:<14} endpoint: must be supplied with --base-url", "")
            }
            None => {}
        }
        println!("{:<14} {}", "", preset.notes);
        println!();
    }
}

fn print_oauth_url(flags: &HashMap<String, String>) -> Result<()> {
    let kind = provider_kind(flags)?;
    let Some((auth_url, _token_url, scopes)) = kind.oauth_endpoints() else {
        return Err(config_error(format!(
            "{} does not use OAuth2",
            kind.preset().display_name
        )));
    };

    let client_id = required(flags, "client-id")?;
    let redirect_uri = required(flags, "redirect-uri")?;

    let config = barback::calendar::OAuth2Config {
        client_id,
        client_secret: flags.get("client-secret").cloned(),
        auth_url: auth_url.to_string(),
        token_url: _token_url.to_string(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        redirect_uri,
    };
    let client = barback::calendar::OAuth2::new(
        config,
        barback::calendar::TokenSet::default(),
        reqwest::Client::new(),
    );

    println!("{}", client.authorize_url("barback-cli"));
    Ok(())
}

async fn list_events(flags: &HashMap<String, String>) -> Result<()> {
    let kind = provider_kind(flags)?;
    let provider = kind.connect(build_config(kind, flags)?)?;

    let days: i64 = flags
        .get("days")
        .map(|d| d.parse::<i64>())
        .transpose()
        .map_err(|e| config_error(format!("--days must be a number: {e}")))?
        .unwrap_or(7);
    let range = TimeRange::days_from_now(days);

    let calendars = match flags.get("calendar") {
        Some(id) => vec![id.clone()],
        None => provider
            .list_calendars()
            .await?
            .into_iter()
            .map(|c| {
                println!("calendar: {} ({})", c.name, c.id);
                c.id
            })
            .collect(),
    };

    for calendar_id in calendars {
        let mut events = provider.list_events(&calendar_id, range).await?;
        events.sort_by_key(|e| e.start.to_utc());

        println!("\n{} event(s) in {calendar_id}", events.len());
        for event in events {
            let when = if event.all_day() {
                event
                    .start
                    .to_utc()
                    .format("%Y-%m-%d (all day)")
                    .to_string()
            } else {
                event
                    .start
                    .to_utc()
                    .format("%Y-%m-%d %H:%M UTC")
                    .to_string()
            };
            let recurring = if event.recurrence.is_empty() {
                ""
            } else {
                " [recurring]"
            };
            println!(
                "  {when}  {}{recurring}",
                event.summary.as_deref().unwrap_or("(no title)")
            );
        }
    }

    Ok(())
}

fn build_config(kind: ProviderKind, flags: &HashMap<String, String>) -> Result<ProviderConfig> {
    let preset = kind.preset();

    let token = flags
        .get("token")
        .cloned()
        .or_else(|| std::env::var("BARBACK_TOKEN").ok());
    let password = flags
        .get("password")
        .cloned()
        .or_else(|| std::env::var("BARBACK_PASSWORD").ok());

    let auth = match preset.auth {
        AuthKind::OAuth2 => Auth::Bearer(token.ok_or_else(|| {
            config_error(
                "an access token is required: set BARBACK_TOKEN or pass --token. \
                 Use `barback oauth-url` to obtain one.",
            )
        })?),
        AuthKind::AppPassword | AuthKind::Password => {
            let user = required(flags, "user")?;
            let password = password.ok_or_else(|| {
                config_error("a password is required: set BARBACK_PASSWORD or pass --password")
            })?;
            Auth::basic(user, password)
        }
        AuthKind::Anonymous => match (flags.get("user"), password) {
            (Some(user), Some(password)) => Auth::basic(user, password),
            _ => Auth::None,
        },
    };

    let mut config = ProviderConfig::new(auth);
    if let Some(base_url) = flags.get("base-url") {
        config = config.with_base_url(base_url);
    }
    Ok(config)
}

fn provider_kind(flags: &HashMap<String, String>) -> Result<ProviderKind> {
    let slug = required(flags, "provider")?;
    ProviderKind::from_slug(&slug).ok_or_else(|| {
        config_error(format!(
            "unknown provider {slug:?}; run `barback providers` to see the list"
        ))
    })
}

fn required(flags: &HashMap<String, String>, name: &str) -> Result<String> {
    flags
        .get(name)
        .cloned()
        .ok_or_else(|| config_error(format!("--{name} is required")))
}

fn config_error(message: impl Into<String>) -> barback::calendar::CalendarError {
    barback::calendar::CalendarError::Config(message.into())
}

/// Parses `--key value` and `--flag` pairs. Bare flags map to an empty string.
fn parse_flags(args: &[String]) -> HashMap<String, String> {
    let mut flags = HashMap::new();
    let mut index = 0;

    while index < args.len() {
        let Some(name) = args[index].strip_prefix("--") else {
            index += 1;
            continue;
        };
        let value = match args.get(index + 1) {
            Some(next) if !next.starts_with("--") => {
                index += 1;
                next.clone()
            }
            _ => String::new(),
        };
        flags.insert(name.to_string(), value);
        index += 1;
    }

    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_key_value_pairs() {
        let flags = parse_flags(&args(&["--provider", "google", "--days", "14"]));
        assert_eq!(flags.get("provider").map(String::as_str), Some("google"));
        assert_eq!(flags.get("days").map(String::as_str), Some("14"));
    }

    #[test]
    fn bare_flags_get_an_empty_value() {
        let flags = parse_flags(&args(&["--verbose", "--provider", "google"]));
        assert_eq!(flags.get("verbose").map(String::as_str), Some(""));
        assert_eq!(flags.get("provider").map(String::as_str), Some("google"));
    }

    #[test]
    fn values_containing_spaces_survive() {
        let flags = parse_flags(&args(&["--user", "ada lovelace"]));
        assert_eq!(flags.get("user").map(String::as_str), Some("ada lovelace"));
    }

    #[test]
    fn unknown_provider_is_a_clear_error() {
        let flags = parse_flags(&args(&["--provider", "nope"]));
        let err = provider_kind(&flags).expect_err("should be rejected");
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn oauth_provider_without_a_token_is_rejected() {
        // Guard against a stray token in the developer's environment.
        unsafe { std::env::remove_var("BARBACK_TOKEN") };
        let flags = parse_flags(&args(&["--provider", "google"]));
        let err = build_config(ProviderKind::Google, &flags)
            .err()
            .expect("a token is required");
        assert!(err.to_string().contains("access token"));
    }

    #[test]
    fn caldav_provider_needs_a_user() {
        unsafe { std::env::set_var("BARBACK_PASSWORD", "pw") };
        let flags = parse_flags(&args(&["--provider", "apple-icloud"]));
        let err = build_config(ProviderKind::AppleICloud, &flags)
            .err()
            .expect("a user is required");
        assert!(err.to_string().contains("--user"));
        unsafe { std::env::remove_var("BARBACK_PASSWORD") };
    }

    #[test]
    fn ics_feed_works_without_credentials() {
        let flags = parse_flags(&args(&["--base-url", "https://example.com/f.ics"]));
        let config = build_config(ProviderKind::IcsFeed, &flags).unwrap();
        assert!(matches!(config.auth, Auth::None));
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://example.com/f.ics")
        );
    }
}
