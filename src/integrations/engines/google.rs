//! Google Calendar API v3.
//!
//! Reads ask the API to expand recurring series (`singleEvents=true`), so
//! `list_events` returns concrete instances rather than masters. Writes send
//! `RRULE` lines through unchanged, since Google's `recurrence` field is
//! iCalendar syntax.

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use chrono_tz::Tz;
use reqwest::Method;
use reqwest::header::IF_MATCH;
use serde::{Deserialize, Serialize};

use crate::integrations::error::{CalendarError, Result};
use crate::integrations::http::HttpClient;
use crate::integrations::model::{
    Attendee, Calendar, Capabilities, Event, EventStatus, EventTime, ParticipationStatus, Person,
    Reminder, ReminderMethod, TimeRange, Transparency,
};
use crate::integrations::provider::CalendarProvider;

pub const DEFAULT_BASE: &str = "https://www.googleapis.com/calendar/v3";
const PAGE_SIZE: &str = "250";
/// The API caps a single events page at 2500; 250 keeps individual responses
/// small without making pagination dominate.
const EVENT_PAGE_SIZE: &str = "250";

pub struct GoogleCalendar {
    http: HttpClient,
    base: String,
}

impl GoogleCalendar {
    pub fn new(http: HttpClient) -> Self {
        GoogleCalendar::with_base_url(http, DEFAULT_BASE)
    }

    /// Points the client at a different API root. Only needed for testing
    /// against a stub; the public API has one address.
    pub fn with_base_url(http: HttpClient, base: impl Into<String>) -> Self {
        GoogleCalendar {
            http,
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    fn calendar_url(&self, calendar_id: &str, suffix: &str) -> String {
        format!("{}/calendars/{}{suffix}", self.base, encode(calendar_id))
    }
}

#[async_trait]
impl CalendarProvider for GoogleCalendar {
    fn name(&self) -> &'static str {
        "google"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::FULL
    }

    async fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let mut calendars = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut request = self
                .http
                .get(&format!("{}/users/me/calendarList", self.base))
                .query(&[("maxResults", PAGE_SIZE), ("minAccessRole", "reader")]);
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token)]);
            }

            let page: CalendarListPage = self.http.send(request).await?.json().await?;
            calendars.extend(page.items.into_iter().map(Calendar::from));

            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(calendars)
    }

    async fn list_events(&self, calendar_id: &str, range: TimeRange) -> Result<Vec<Event>> {
        let url = self.calendar_url(calendar_id, "/events");
        let time_min = range.start.to_rfc3339();
        let time_max = range.end.to_rfc3339();

        let mut events = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut request = self.http.get(&url).query(&[
                ("timeMin", time_min.as_str()),
                ("timeMax", time_max.as_str()),
                // Expanding server-side is the only way to get correct
                // instances without reimplementing RRULE here.
                ("singleEvents", "true"),
                ("orderBy", "startTime"),
                ("showDeleted", "false"),
                ("maxResults", EVENT_PAGE_SIZE),
            ]);
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token)]);
            }

            let page: EventsPage = self.http.send(request).await?.json().await?;
            for item in page.items {
                events.push(item.into_event(calendar_id)?);
            }

            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        Ok(events)
    }

    async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Event> {
        let url = self.calendar_url(calendar_id, &format!("/events/{}", encode(event_id)));
        let item: GoogleEvent = self.http.send(self.http.get(&url)).await?.json().await?;
        item.into_event(calendar_id)
    }

    async fn create_event(&self, calendar_id: &str, event: &Event) -> Result<Event> {
        let url = self.calendar_url(calendar_id, "/events");
        let payload = GoogleEvent::from_event(event);
        let request = self.http.request(Method::POST, &url).json(&payload);
        let created: GoogleEvent = self.http.send(request).await?.json().await?;
        created.into_event(calendar_id)
    }

    async fn update_event(&self, calendar_id: &str, event: &Event) -> Result<Event> {
        if event.id.is_empty() {
            return Err(CalendarError::Config(
                "update_event needs the Google event id in `id`".into(),
            ));
        }
        let url = self.calendar_url(calendar_id, &format!("/events/{}", encode(&event.id)));
        let payload = GoogleEvent::from_event(event);
        let mut request = self.http.request(Method::PUT, &url).json(&payload);
        if let Some(etag) = &event.etag {
            request = request.header(IF_MATCH, etag);
        }
        let updated: GoogleEvent = self.http.send(request).await?.json().await?;
        updated.into_event(calendar_id)
    }

    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<()> {
        let url = self.calendar_url(calendar_id, &format!("/events/{}", encode(event_id)));
        self.http
            .send(self.http.request(Method::DELETE, &url))
            .await?;
        Ok(())
    }
}

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarListPage {
    #[serde(default)]
    items: Vec<GoogleCalendarEntry>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleCalendarEntry {
    id: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    time_zone: Option<String>,
    #[serde(default)]
    background_color: Option<String>,
    #[serde(default)]
    access_role: Option<String>,
    #[serde(default)]
    primary: bool,
}

impl From<GoogleCalendarEntry> for Calendar {
    fn from(entry: GoogleCalendarEntry) -> Self {
        let read_only = matches!(
            entry.access_role.as_deref(),
            Some("reader") | Some("freeBusyReader")
        );
        Calendar {
            name: entry.summary.unwrap_or_else(|| entry.id.clone()),
            id: entry.id,
            description: entry.description,
            timezone: entry.time_zone,
            color: entry.background_color,
            read_only,
            primary: entry.primary,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsPage {
    #[serde(default)]
    items: Vec<GoogleEvent>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleDateTime {
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_zone: Option<String>,
}

impl GoogleDateTime {
    fn into_event_time(self, field: &'static str) -> Result<EventTime> {
        if let Some(date) = self.date {
            let d = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map_err(|e| CalendarError::parse(field, format!("{date:?}: {e}")))?;
            return Ok(EventTime::Date(d));
        }

        let raw = self
            .date_time
            .ok_or_else(|| CalendarError::parse(field, "neither date nor dateTime present"))?;
        let parsed = DateTime::<FixedOffset>::parse_from_rfc3339(&raw)
            .map_err(|e| CalendarError::parse(field, format!("{raw:?}: {e}")))?;

        // Google sends both the offset and the zone name. Keeping the name
        // matters for recurring events, where the offset alone is not enough.
        match self
            .time_zone
            .as_deref()
            .and_then(|tz| tz.parse::<Tz>().ok())
        {
            Some(tz) => Ok(EventTime::Zoned {
                naive: parsed.with_timezone(&tz).naive_local(),
                tz,
            }),
            None => Ok(EventTime::Utc(parsed.with_timezone(&Utc))),
        }
    }

    fn from_event_time(time: &EventTime) -> Self {
        match time {
            EventTime::Date(d) => GoogleDateTime {
                date: Some(d.format("%Y-%m-%d").to_string()),
                ..Default::default()
            },
            EventTime::Zoned { naive, tz } => {
                let instant = EventTime::Zoned {
                    naive: *naive,
                    tz: *tz,
                }
                .to_utc();
                GoogleDateTime {
                    date_time: Some(instant.to_rfc3339()),
                    time_zone: Some(tz.name().to_string()),
                    ..Default::default()
                }
            }
            other => GoogleDateTime {
                date_time: Some(other.to_utc().to_rfc3339()),
                time_zone: Some("UTC".to_string()),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GooglePerson {
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleAttendee {
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    optional: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    resource: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    organizer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_status: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleReminderOverride {
    method: String,
    minutes: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleReminders {
    use_default: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    overrides: Vec<GoogleReminderOverride>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleEvent {
    #[serde(default, skip_serializing)]
    id: String,
    #[serde(rename = "iCalUID", default, skip_serializing_if = "Option::is_none")]
    ical_uid: Option<String>,
    #[serde(default, skip_serializing)]
    etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    start: GoogleDateTime,
    end: GoogleDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transparency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    organizer: Option<GooglePerson>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attendees: Vec<GoogleAttendee>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recurrence: Vec<String>,
    #[serde(default, skip_serializing)]
    recurring_event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reminders: Option<GoogleReminders>,
    #[serde(default, skip_serializing)]
    html_link: Option<String>,
    #[serde(default, skip_serializing)]
    created: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing)]
    updated: Option<DateTime<Utc>>,
}

impl GoogleEvent {
    fn into_event(self, calendar_id: &str) -> Result<Event> {
        let start = self.start.into_event_time("event start")?;
        let end = self.end.into_event_time("event end").ok();

        Ok(Event {
            uid: self.ical_uid.unwrap_or_else(|| self.id.clone()),
            id: self.id,
            etag: self.etag,
            calendar_id: calendar_id.to_string(),
            summary: self.summary,
            description: self.description,
            location: self.location,
            start,
            end,
            status: match self.status.as_deref() {
                Some("tentative") => EventStatus::Tentative,
                Some("cancelled") => EventStatus::Cancelled,
                _ => EventStatus::Confirmed,
            },
            transparency: match self.transparency.as_deref() {
                Some("transparent") => Transparency::Transparent,
                _ => Transparency::Opaque,
            },
            organizer: self.organizer.map(|p| Person {
                email: p.email,
                display_name: p.display_name,
            }),
            attendees: self
                .attendees
                .into_iter()
                .map(|a| Attendee {
                    person: Person {
                        email: a.email,
                        display_name: a.display_name,
                    },
                    status: match a.response_status.as_deref() {
                        Some("accepted") => ParticipationStatus::Accepted,
                        Some("declined") => ParticipationStatus::Declined,
                        Some("tentative") => ParticipationStatus::Tentative,
                        _ => ParticipationStatus::NeedsAction,
                    },
                    optional: a.optional,
                    organizer: a.organizer,
                    resource: a.resource,
                })
                .collect(),
            recurrence: self.recurrence,
            recurring_event_id: self.recurring_event_id,
            reminders: self
                .reminders
                .map(|r| {
                    r.overrides
                        .into_iter()
                        .map(|o| Reminder {
                            method: if o.method == "email" {
                                ReminderMethod::Email
                            } else {
                                ReminderMethod::Popup
                            },
                            minutes_before: o.minutes,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            url: self.html_link,
            created: self.created,
            updated: self.updated,
        })
    }

    fn from_event(event: &Event) -> Self {
        GoogleEvent {
            id: String::new(),
            ical_uid: if event.uid.is_empty() {
                None
            } else {
                Some(event.uid.clone())
            },
            etag: None,
            summary: event.summary.clone(),
            description: event.description.clone(),
            location: event.location.clone(),
            start: GoogleDateTime::from_event_time(&event.start),
            end: GoogleDateTime::from_event_time(event.end.as_ref().unwrap_or(&event.start)),
            status: Some(
                match event.status {
                    EventStatus::Confirmed => "confirmed",
                    EventStatus::Tentative => "tentative",
                    EventStatus::Cancelled => "cancelled",
                }
                .to_string(),
            ),
            transparency: Some(
                match event.transparency {
                    Transparency::Opaque => "opaque",
                    Transparency::Transparent => "transparent",
                }
                .to_string(),
            ),
            organizer: event.organizer.as_ref().map(|p| GooglePerson {
                email: p.email.clone(),
                display_name: p.display_name.clone(),
            }),
            attendees: event
                .attendees
                .iter()
                .map(|a| GoogleAttendee {
                    email: a.person.email.clone(),
                    display_name: a.person.display_name.clone(),
                    optional: a.optional,
                    resource: a.resource,
                    organizer: a.organizer,
                    response_status: Some(
                        match a.status {
                            ParticipationStatus::Accepted => "accepted",
                            ParticipationStatus::Declined => "declined",
                            ParticipationStatus::Tentative => "tentative",
                            _ => "needsAction",
                        }
                        .to_string(),
                    ),
                })
                .collect(),
            recurrence: event.recurrence.clone(),
            recurring_event_id: None,
            reminders: Some(GoogleReminders {
                // Sending an explicit (possibly empty) override list stops
                // Google from silently applying the calendar's defaults.
                use_default: event.reminders.is_empty(),
                overrides: event
                    .reminders
                    .iter()
                    .map(|r| GoogleReminderOverride {
                        method: match r.method {
                            ReminderMethod::Email => "email".to_string(),
                            ReminderMethod::Popup => "popup".to_string(),
                        },
                        minutes: r.minutes_before,
                    })
                    .collect(),
            }),
            html_link: None,
            created: None,
            updated: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parses_a_calendar_list_entry() {
        let json = r##"{"items":[
            {"id":"ada@example.com","summary":"Ada","timeZone":"Europe/Berlin",
             "backgroundColor":"#9fe1e7","accessRole":"owner","primary":true},
            {"id":"holidays","summary":"Holidays","accessRole":"reader"}
        ]}"##;
        let page: CalendarListPage = serde_json::from_str(json).unwrap();
        let calendars: Vec<Calendar> = page.items.into_iter().map(Calendar::from).collect();

        assert_eq!(calendars[0].name, "Ada");
        assert!(calendars[0].primary);
        assert!(!calendars[0].read_only);
        assert_eq!(calendars[0].timezone.as_deref(), Some("Europe/Berlin"));
        assert!(calendars[1].read_only);
    }

    #[test]
    fn parses_a_timed_event_keeping_its_zone() {
        let json = r#"{
            "id":"evt1","iCalUID":"evt1@google.com","etag":"\"p33\"",
            "summary":"Sync","location":"Room 3",
            "start":{"dateTime":"2026-08-03T12:00:00+02:00","timeZone":"Europe/Berlin"},
            "end":{"dateTime":"2026-08-03T13:00:00+02:00","timeZone":"Europe/Berlin"},
            "status":"confirmed","transparency":"opaque",
            "organizer":{"email":"ada@example.com","displayName":"Ada"},
            "attendees":[{"email":"bob@example.com","responseStatus":"accepted","optional":true}],
            "reminders":{"useDefault":false,"overrides":[{"method":"popup","minutes":15}]},
            "htmlLink":"https://calendar.google.com/event?eid=1"
        }"#;
        let event = serde_json::from_str::<GoogleEvent>(json)
            .unwrap()
            .into_event("ada@example.com")
            .unwrap();

        assert_eq!(event.id, "evt1");
        assert_eq!(event.uid, "evt1@google.com");
        assert_eq!(event.etag.as_deref(), Some("\"p33\""));
        assert_eq!(
            event.start,
            EventTime::Zoned {
                naive: NaiveDate::from_ymd_opt(2026, 8, 3)
                    .unwrap()
                    .and_hms_opt(12, 0, 0)
                    .unwrap(),
                tz: chrono_tz::Europe::Berlin,
            }
        );
        assert_eq!(
            event.start.to_utc().to_rfc3339(),
            "2026-08-03T10:00:00+00:00"
        );
        assert_eq!(event.attendees.len(), 1);
        assert!(event.attendees[0].optional);
        assert_eq!(event.attendees[0].status, ParticipationStatus::Accepted);
        assert_eq!(event.reminders[0].minutes_before, 15);
    }

    #[test]
    fn parses_an_all_day_event() {
        let json = r#"{"id":"e","start":{"date":"2026-08-03"},"end":{"date":"2026-08-04"}}"#;
        let event = serde_json::from_str::<GoogleEvent>(json)
            .unwrap()
            .into_event("cal")
            .unwrap();
        assert!(event.all_day());
        assert_eq!(
            event.start,
            EventTime::Date(NaiveDate::from_ymd_opt(2026, 8, 3).unwrap())
        );
    }

    #[test]
    fn unknown_zone_name_falls_back_to_the_offset() {
        let json = r#"{"id":"e",
            "start":{"dateTime":"2026-08-03T12:00:00+02:00","timeZone":"Mars/Olympus"},
            "end":{"dateTime":"2026-08-03T13:00:00+02:00"}}"#;
        let event = serde_json::from_str::<GoogleEvent>(json)
            .unwrap()
            .into_event("cal")
            .unwrap();
        assert_eq!(
            event.start.to_utc().to_rfc3339(),
            "2026-08-03T10:00:00+00:00"
        );
        assert!(matches!(event.start, EventTime::Utc(_)));
    }

    #[test]
    fn event_without_a_usable_start_is_rejected() {
        let json = r#"{"id":"e","start":{},"end":{}}"#;
        let result = serde_json::from_str::<GoogleEvent>(json)
            .unwrap()
            .into_event("cal");
        assert!(result.is_err());
    }

    #[test]
    fn missing_end_is_tolerated() {
        let json = r#"{"id":"e","start":{"dateTime":"2026-08-03T12:00:00Z"},"end":{}}"#;
        let event = serde_json::from_str::<GoogleEvent>(json)
            .unwrap()
            .into_event("cal")
            .unwrap();
        assert!(event.end.is_none());
    }

    #[test]
    fn serialized_payload_omits_server_owned_fields() {
        let event = Event::new(
            "Sync",
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
        );
        let json = serde_json::to_value(GoogleEvent::from_event(&event)).unwrap();

        assert!(json.get("id").is_none());
        assert!(json.get("etag").is_none());
        assert!(json.get("htmlLink").is_none());
        assert_eq!(json["summary"], "Sync");
        assert_eq!(json["start"]["dateTime"], "2026-08-03T10:00:00+00:00");
        assert_eq!(json["start"]["timeZone"], "UTC");
        assert_eq!(json["reminders"]["useDefault"], true);
    }

    #[test]
    fn all_day_payload_uses_date_not_datetime() {
        let event = Event::new(
            "Holiday",
            EventTime::Date(NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()),
            EventTime::Date(NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()),
        );
        let json = serde_json::to_value(GoogleEvent::from_event(&event)).unwrap();
        assert_eq!(json["start"]["date"], "2026-08-03");
        assert!(json["start"].get("dateTime").is_none());
    }

    #[test]
    fn explicit_reminders_disable_calendar_defaults() {
        let mut event = Event::new(
            "Sync",
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
        );
        event.reminders = vec![Reminder {
            method: ReminderMethod::Email,
            minutes_before: 30,
        }];
        let json = serde_json::to_value(GoogleEvent::from_event(&event)).unwrap();
        assert_eq!(json["reminders"]["useDefault"], false);
        assert_eq!(json["reminders"]["overrides"][0]["method"], "email");
        assert_eq!(json["reminders"]["overrides"][0]["minutes"], 30);
    }

    #[test]
    fn recurrence_lines_pass_through_untouched() {
        let mut event = Event::new(
            "Standup",
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 15, 0).unwrap()),
        );
        event.recurrence = vec!["RRULE:FREQ=WEEKLY;BYDAY=MO,TU".into()];
        let json = serde_json::to_value(GoogleEvent::from_event(&event)).unwrap();
        assert_eq!(json["recurrence"][0], "RRULE:FREQ=WEEKLY;BYDAY=MO,TU");
    }

    #[test]
    fn calendar_ids_are_escaped_in_urls() {
        let engine = GoogleCalendar::new(HttpClient::new(
            reqwest::Client::new(),
            crate::integrations::auth::Auth::None,
            "google",
        ));
        let url = engine.calendar_url("ada@example.com", "/events");
        assert_eq!(
            url,
            "https://www.googleapis.com/calendar/v3/calendars/ada%40example%2Ecom/events"
        );
    }
}
