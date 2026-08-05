//! Read-only iCalendar feed subscription (`webcal://` / `https://…/basic.ics`).
//!
//! This is the fallback that covers everything without an API: Proton Calendar
//! exports, Calendly, conference and sports schedules, university timetables,
//! and the public share links Google and Outlook themselves hand out.
//!
//! There is no server-side filtering, so the whole feed is fetched and filtered
//! locally.

use async_trait::async_trait;
use url::Url;

use crate::integrations::error::{CalendarError, Result};
use crate::integrations::http::HttpClient;
use crate::integrations::ical;
use crate::integrations::model::{Calendar, Capabilities, Event, TimeRange};
use crate::integrations::provider::CalendarProvider;

pub struct IcsFeed {
    http: HttpClient,
    url: Url,
    name: Option<String>,
}

impl IcsFeed {
    /// `url` may use the `webcal` scheme, which is rewritten to `https`.
    pub fn new(http: HttpClient, url: &str, name: Option<String>) -> Result<Self> {
        Ok(IcsFeed {
            http,
            url: normalize_url(url)?,
            name,
        })
    }

    async fn fetch(&self) -> Result<String> {
        let response = self.http.send(self.http.get(self.url.as_str())).await?;
        Ok(response.text().await?)
    }

    fn unsupported(operation: &'static str) -> CalendarError {
        CalendarError::Unsupported {
            provider: "ics-feed",
            operation,
        }
    }
}

#[async_trait]
impl CalendarProvider for IcsFeed {
    fn name(&self) -> &'static str {
        "ics-feed"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    async fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let body = self.fetch().await?;
        let components = ical::syntax::parse(&body)?;
        let calendar = components
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case("VCALENDAR"));

        let name = self
            .name
            .clone()
            .or_else(|| calendar.and_then(|c| c.prop_text("X-WR-CALNAME")))
            .unwrap_or_else(|| self.url.to_string());

        Ok(vec![Calendar {
            id: self.url.to_string(),
            name,
            description: calendar.and_then(|c| c.prop_text("X-WR-CALDESC")),
            timezone: calendar.and_then(|c| c.prop_text("X-WR-TIMEZONE")),
            color: None,
            read_only: true,
            primary: true,
        }])
    }

    async fn list_events(&self, _calendar_id: &str, range: TimeRange) -> Result<Vec<Event>> {
        let body = self.fetch().await?;
        let mut events = ical::parse_events(&body)?;

        for event in &mut events {
            event.calendar_id = self.url.to_string();
            if event.id.is_empty() {
                event.id = event.uid.clone();
            }
        }

        // Recurring masters are kept regardless of where their DTSTART falls:
        // without expanding the rule there is no way to tell whether an
        // instance lands in the window, and dropping them would silently hide
        // every weekly meeting older than the range.
        events.retain(|e| !e.recurrence.is_empty() || e.overlaps(&range));
        Ok(events)
    }

    async fn get_event(&self, _calendar_id: &str, event_id: &str) -> Result<Event> {
        let body = self.fetch().await?;
        ical::parse_events(&body)?
            .into_iter()
            .find(|e| e.uid == event_id)
            .map(|mut e| {
                e.id = e.uid.clone();
                e.calendar_id = self.url.to_string();
                e
            })
            .ok_or_else(|| CalendarError::NotFound(format!("no VEVENT with UID {event_id}")))
    }

    async fn create_event(&self, _calendar_id: &str, _event: &Event) -> Result<Event> {
        Err(Self::unsupported("creating events"))
    }

    async fn update_event(&self, _calendar_id: &str, _event: &Event) -> Result<Event> {
        Err(Self::unsupported("updating events"))
    }

    async fn delete_event(&self, _calendar_id: &str, _event_id: &str) -> Result<()> {
        Err(Self::unsupported("deleting events"))
    }
}

/// Rewrites `webcal://` (and its rarely-seen TLS variant) to `https://`.
fn normalize_url(url: &str) -> Result<Url> {
    let trimmed = url.trim();
    let rewritten = match trimmed.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("webcal") => {
            format!("https://{rest}")
        }
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("webcals") => {
            format!("https://{rest}")
        }
        _ => trimmed.to_string(),
    };
    Url::parse(&rewritten).map_err(CalendarError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::auth::Auth;
    use crate::integrations::model::EventTime;
    use chrono::{TimeZone, Utc};

    fn feed(url: &str) -> IcsFeed {
        let http = HttpClient::new(reqwest::Client::new(), Auth::None, "ics-feed");
        IcsFeed::new(http, url, None).unwrap()
    }

    #[test]
    fn rewrites_webcal_scheme() {
        assert_eq!(
            normalize_url("webcal://example.com/feed.ics")
                .unwrap()
                .as_str(),
            "https://example.com/feed.ics"
        );
        assert_eq!(
            normalize_url("WEBCAL://example.com/f.ics")
                .unwrap()
                .as_str(),
            "https://example.com/f.ics"
        );
        assert_eq!(
            normalize_url("https://example.com/f.ics").unwrap().as_str(),
            "https://example.com/f.ics"
        );
    }

    #[test]
    fn rejects_nonsense_urls() {
        assert!(normalize_url("not a url").is_err());
    }

    #[test]
    fn is_declared_read_only() {
        let caps = feed("https://example.com/f.ics").capabilities();
        assert!(caps.read);
        assert!(!caps.write);
        assert!(!caps.delete);
    }

    #[tokio::test]
    async fn writes_are_refused_without_a_network_call() {
        let feed = feed("https://example.com/f.ics");
        let event = Event::new(
            "x",
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
        );
        assert!(matches!(
            feed.create_event("c", &event).await,
            Err(CalendarError::Unsupported { .. })
        ));
        assert!(matches!(
            feed.update_event("c", &event).await,
            Err(CalendarError::Unsupported { .. })
        ));
        assert!(matches!(
            feed.delete_event("c", "id").await,
            Err(CalendarError::Unsupported { .. })
        ));
    }

    /// The range filter is pure logic, so it is exercised directly rather than
    /// through a network round trip.
    fn filter(events: Vec<Event>, range: TimeRange) -> Vec<Event> {
        let mut events = events;
        events.retain(|e| !e.recurrence.is_empty() || e.overlaps(&range));
        events
    }

    #[test]
    fn filters_single_events_but_keeps_recurring_masters() {
        let ics = "BEGIN:VCALENDAR\r\n\
                   BEGIN:VEVENT\r\nUID:in\r\nDTSTART:20260803T100000Z\r\nDTEND:20260803T110000Z\r\nEND:VEVENT\r\n\
                   BEGIN:VEVENT\r\nUID:out\r\nDTSTART:20200101T100000Z\r\nDTEND:20200101T110000Z\r\nEND:VEVENT\r\n\
                   BEGIN:VEVENT\r\nUID:series\r\nDTSTART:20200101T090000Z\r\nDTEND:20200101T091500Z\r\n\
                   RRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n\
                   END:VCALENDAR\r\n";
        let events = ical::parse_events(ics).unwrap();
        let range = TimeRange::new(
            Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 8, 0, 0, 0).unwrap(),
        );

        let kept: Vec<String> = filter(events, range).into_iter().map(|e| e.uid).collect();
        assert_eq!(kept, vec!["in", "series"]);
    }

    #[test]
    fn reads_calendar_metadata_from_the_feed() {
        let ics = "BEGIN:VCALENDAR\r\nX-WR-CALNAME:Bundesliga\r\n\
                   X-WR-TIMEZONE:Europe/Berlin\r\nEND:VCALENDAR\r\n";
        let components = ical::syntax::parse(ics).unwrap();
        let calendar = &components[0];
        assert_eq!(
            calendar.prop_text("X-WR-CALNAME").as_deref(),
            Some("Bundesliga")
        );
        assert_eq!(
            calendar.prop_text("X-WR-TIMEZONE").as_deref(),
            Some("Europe/Berlin")
        );
    }
}
