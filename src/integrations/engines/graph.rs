//! Microsoft Graph, covering Outlook.com, Microsoft 365 and Exchange Online.
//!
//! Reads go through `calendarView`, which expands recurring series into
//! occurrences server-side.
//!
//! Writes do **not** support recurrence. Graph models recurrence as a
//! structured `patternedRecurrence` object rather than `RRULE` text, and
//! translating between the two loses information in both directions (`BYSETPOS`
//! combinations, `EXDATE` lists, non-Gregorian rules). Sending a recurring
//! event here returns [`CalendarError::Unsupported`] instead of silently
//! writing a single occurrence.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
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

pub const DEFAULT_BASE: &str = "https://graph.microsoft.com/v1.0/me";
const PAGE_SIZE: &str = "250";
/// Asks Graph to return every `dateTime` already converted to UTC, so the
/// Windows zone names it would otherwise use never have to be mapped.
const PREFER_UTC: &str = "outlook.timezone=\"UTC\"";

pub struct MicrosoftGraph {
    http: HttpClient,
    base: String,
}

impl MicrosoftGraph {
    pub fn new(http: HttpClient) -> Self {
        MicrosoftGraph::with_base_url(http, DEFAULT_BASE)
    }

    /// Points the client at a different Graph root. Needed for the national
    /// clouds (`graph.microsoft.us`, `graph.microsoft.de`) and for testing
    /// against a stub. Include the `/me` suffix.
    pub fn with_base_url(http: HttpClient, base: impl Into<String>) -> Self {
        MicrosoftGraph {
            http,
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    /// Follows `@odata.nextLink` until the collection is exhausted.
    async fn collect_pages<T: for<'de> Deserialize<'de>>(
        &self,
        first: reqwest::RequestBuilder,
    ) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut request = Some(first);

        while let Some(current) = request.take() {
            let page: OdataPage<T> = self.http.send(current).await?.json().await?;
            items.extend(page.value);
            if let Some(next) = page.next_link {
                request = Some(self.http.get(&next).header("Prefer", PREFER_UTC));
            }
        }

        Ok(items)
    }
}

#[async_trait]
impl CalendarProvider for MicrosoftGraph {
    fn name(&self) -> &'static str {
        "microsoft-graph"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::FULL
    }

    async fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let request = self
            .http
            .get(&format!("{}/calendars", self.base))
            .query(&[("$top", PAGE_SIZE)]);
        let entries: Vec<GraphCalendar> = self.collect_pages(request).await?;
        Ok(entries.into_iter().map(Calendar::from).collect())
    }

    async fn list_events(&self, calendar_id: &str, range: TimeRange) -> Result<Vec<Event>> {
        let url = format!(
            "{}/calendars/{}/calendarView",
            self.base,
            encode(calendar_id)
        );
        let request = self.http.get(&url).header("Prefer", PREFER_UTC).query(&[
            ("startDateTime", range.start.to_rfc3339()),
            ("endDateTime", range.end.to_rfc3339()),
            ("$top", PAGE_SIZE.to_string()),
            ("$orderby", "start/dateTime".to_string()),
        ]);

        let items: Vec<GraphEvent> = self.collect_pages(request).await?;
        items
            .into_iter()
            .map(|item| item.into_event(calendar_id))
            .collect()
    }

    async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Event> {
        let url = format!("{}/events/{}", self.base, encode(event_id));
        let request = self.http.get(&url).header("Prefer", PREFER_UTC);
        let item: GraphEvent = self.http.send(request).await?.json().await?;
        item.into_event(calendar_id)
    }

    async fn create_event(&self, calendar_id: &str, event: &Event) -> Result<Event> {
        reject_recurrence(event)?;
        let url = format!("{}/calendars/{}/events", self.base, encode(calendar_id));
        let request = self
            .http
            .request(Method::POST, &url)
            .json(&GraphEvent::from_event(event));
        let created: GraphEvent = self.http.send(request).await?.json().await?;
        created.into_event(calendar_id)
    }

    async fn update_event(&self, calendar_id: &str, event: &Event) -> Result<Event> {
        reject_recurrence(event)?;
        if event.id.is_empty() {
            return Err(CalendarError::Config(
                "update_event needs the Graph event id in `id`".into(),
            ));
        }
        let url = format!("{}/events/{}", self.base, encode(&event.id));
        let mut request = self
            .http
            .request(Method::PATCH, &url)
            .json(&GraphEvent::from_event(event));
        if let Some(etag) = &event.etag {
            request = request.header(IF_MATCH, etag);
        }
        let updated: GraphEvent = self.http.send(request).await?.json().await?;
        updated.into_event(calendar_id)
    }

    async fn delete_event(&self, _calendar_id: &str, event_id: &str) -> Result<()> {
        let url = format!("{}/events/{}", self.base, encode(event_id));
        self.http
            .send(self.http.request(Method::DELETE, &url))
            .await?;
        Ok(())
    }
}

fn reject_recurrence(event: &Event) -> Result<()> {
    if event.recurrence.is_empty() {
        return Ok(());
    }
    Err(CalendarError::Unsupported {
        provider: "microsoft-graph",
        operation: "writing recurring events (RRULE has no lossless Graph equivalent)",
    })
}

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[derive(Debug, Deserialize)]
struct OdataPage<T> {
    #[serde(default = "Vec::new")]
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink", default)]
    next_link: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphCalendar {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    hex_color: Option<String>,
    #[serde(default)]
    is_default_calendar: bool,
    #[serde(default = "default_true")]
    can_edit: bool,
}

fn default_true() -> bool {
    true
}

impl From<GraphCalendar> for Calendar {
    fn from(entry: GraphCalendar) -> Self {
        Calendar {
            name: entry.name.unwrap_or_else(|| entry.id.clone()),
            id: entry.id,
            description: None,
            // Graph exposes the mailbox time zone through a separate endpoint,
            // not on the calendar resource.
            timezone: None,
            color: entry.hex_color,
            read_only: !entry.can_edit,
            primary: entry.is_default_calendar,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphDateTime {
    date_time: String,
    time_zone: String,
}

impl GraphDateTime {
    fn into_event_time(self, all_day: bool, field: &'static str) -> Result<EventTime> {
        // Graph pads to seven fractional digits; the format string tolerates
        // any number of them, including none.
        let naive = NaiveDateTime::parse_from_str(&self.date_time, "%Y-%m-%dT%H:%M:%S%.f")
            .map_err(|e| CalendarError::parse(field, format!("{:?}: {e}", self.date_time)))?;

        if all_day {
            return Ok(EventTime::Date(naive.date()));
        }

        Ok(match self.time_zone.as_str() {
            "UTC" | "utc" => EventTime::Utc(naive.and_utc()),
            other => match other.parse::<Tz>() {
                Ok(tz) => EventTime::Zoned { naive, tz },
                // Windows zone names ("W. Europe Standard Time") land here.
                // Requests set Prefer: outlook.timezone="UTC", so this is a
                // fallback rather than the usual path.
                Err(_) => EventTime::Floating(naive),
            },
        })
    }

    fn from_event_time(time: &EventTime) -> Self {
        match time {
            EventTime::Date(d) => GraphDateTime {
                date_time: d
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is valid")
                    .format("%Y-%m-%dT%H:%M:%S")
                    .to_string(),
                time_zone: "UTC".to_string(),
            },
            other => GraphDateTime {
                date_time: other.to_utc().format("%Y-%m-%dT%H:%M:%S").to_string(),
                time_zone: "UTC".to_string(),
            },
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphBody {
    content_type: String,
    content: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphLocation {
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphEmailAddress {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    address: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphRecipient {
    email_address: GraphEmailAddress,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphResponseStatus {
    #[serde(default)]
    response: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphAttendee {
    email_address: GraphEmailAddress,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    attendee_type: Option<String>,
    #[serde(default, skip_serializing)]
    status: Option<GraphResponseStatus>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphEvent {
    #[serde(default, skip_serializing)]
    id: String,
    #[serde(rename = "iCalUId", default, skip_serializing)]
    ical_uid: Option<String>,
    #[serde(rename = "@odata.etag", default, skip_serializing)]
    etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<GraphBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<GraphLocation>,
    start: GraphDateTime,
    end: GraphDateTime,
    #[serde(default)]
    is_all_day: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    show_as: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_cancelled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    organizer: Option<GraphRecipient>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attendees: Vec<GraphAttendee>,
    #[serde(default, skip_serializing)]
    series_master_id: Option<String>,
    #[serde(default)]
    is_reminder_on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reminder_minutes_before_start: Option<i64>,
    #[serde(default, skip_serializing)]
    web_link: Option<String>,
    #[serde(default, skip_serializing)]
    created_date_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing)]
    last_modified_date_time: Option<DateTime<Utc>>,
}

impl GraphEvent {
    fn into_event(self, calendar_id: &str) -> Result<Event> {
        let all_day = self.is_all_day;
        let start = self.start.into_event_time(all_day, "event start")?;
        let end = self.end.into_event_time(all_day, "event end").ok();

        let status = if self.is_cancelled.unwrap_or(false) {
            EventStatus::Cancelled
        } else if self.show_as.as_deref() == Some("tentative") {
            EventStatus::Tentative
        } else {
            EventStatus::Confirmed
        };

        let reminders = match (self.is_reminder_on, self.reminder_minutes_before_start) {
            (true, Some(minutes)) => vec![Reminder {
                method: ReminderMethod::Popup,
                minutes_before: minutes,
            }],
            _ => Vec::new(),
        };

        Ok(Event {
            uid: self.ical_uid.clone().unwrap_or_else(|| self.id.clone()),
            id: self.id,
            etag: self.etag,
            calendar_id: calendar_id.to_string(),
            summary: self.subject,
            description: self.body.map(|b| b.content).filter(|c| !c.is_empty()),
            location: self.location.and_then(|l| l.display_name),
            start,
            end,
            status,
            transparency: match self.show_as.as_deref() {
                Some("free") | Some("workingElsewhere") => Transparency::Transparent,
                _ => Transparency::Opaque,
            },
            organizer: self.organizer.map(|o| Person {
                email: o.email_address.address,
                display_name: o.email_address.name,
            }),
            attendees: self
                .attendees
                .into_iter()
                .map(|a| Attendee {
                    person: Person {
                        email: a.email_address.address,
                        display_name: a.email_address.name,
                    },
                    status: match a.status.and_then(|s| s.response).as_deref() {
                        Some("accepted") | Some("organizer") => ParticipationStatus::Accepted,
                        Some("declined") => ParticipationStatus::Declined,
                        Some("tentativelyAccepted") => ParticipationStatus::Tentative,
                        _ => ParticipationStatus::NeedsAction,
                    },
                    optional: a.attendee_type.as_deref() == Some("optional"),
                    organizer: false,
                    resource: a.attendee_type.as_deref() == Some("resource"),
                })
                .collect(),
            // calendarView returns expanded occurrences, so a rule is never
            // present on the way in.
            recurrence: Vec::new(),
            recurring_event_id: self.series_master_id,
            reminders,
            url: self.web_link,
            created: self.created_date_time,
            updated: self.last_modified_date_time,
        })
    }

    fn from_event(event: &Event) -> Self {
        let reminder = event.reminders.first();
        GraphEvent {
            id: String::new(),
            ical_uid: None,
            etag: None,
            subject: event.summary.clone(),
            body: event.description.as_ref().map(|content| GraphBody {
                content_type: "text".to_string(),
                content: content.clone(),
            }),
            location: event.location.as_ref().map(|name| GraphLocation {
                display_name: Some(name.clone()),
            }),
            start: GraphDateTime::from_event_time(&event.start),
            end: GraphDateTime::from_event_time(event.end.as_ref().unwrap_or(&event.start)),
            is_all_day: event.all_day(),
            show_as: Some(
                match (event.status, event.transparency) {
                    (EventStatus::Tentative, _) => "tentative",
                    (_, Transparency::Transparent) => "free",
                    _ => "busy",
                }
                .to_string(),
            ),
            is_cancelled: None,
            organizer: event.organizer.as_ref().map(|p| GraphRecipient {
                email_address: GraphEmailAddress {
                    name: p.display_name.clone(),
                    address: p.email.clone(),
                },
            }),
            attendees: event
                .attendees
                .iter()
                .map(|a| GraphAttendee {
                    email_address: GraphEmailAddress {
                        name: a.person.display_name.clone(),
                        address: a.person.email.clone(),
                    },
                    attendee_type: Some(
                        if a.resource {
                            "resource"
                        } else if a.optional {
                            "optional"
                        } else {
                            "required"
                        }
                        .to_string(),
                    ),
                    status: None,
                })
                .collect(),
            series_master_id: None,
            is_reminder_on: reminder.is_some(),
            reminder_minutes_before_start: reminder.map(|r| r.minutes_before),
            web_link: None,
            created_date_time: None,
            last_modified_date_time: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    #[test]
    fn parses_a_calendar_collection() {
        let json = r##"{"value":[
            {"id":"AAA=","name":"Calendar","isDefaultCalendar":true,"canEdit":true,
             "hexColor":"#0078d4"},
            {"id":"BBB=","name":"Team","canEdit":false}
        ]}"##;
        let page: OdataPage<GraphCalendar> = serde_json::from_str(json).unwrap();
        let calendars: Vec<Calendar> = page.value.into_iter().map(Calendar::from).collect();

        assert_eq!(calendars[0].name, "Calendar");
        assert!(calendars[0].primary);
        assert!(!calendars[0].read_only);
        assert!(calendars[1].read_only);
    }

    #[test]
    fn parses_an_occurrence_returned_in_utc() {
        let json = r#"{
            "id":"AAMkAD==","iCalUId":"040000008200E0","@odata.etag":"W/\"CQAAABYAAAA\"",
            "subject":"Sync","body":{"contentType":"text","content":"agenda"},
            "location":{"displayName":"Room 3"},
            "start":{"dateTime":"2026-08-03T10:00:00.0000000","timeZone":"UTC"},
            "end":{"dateTime":"2026-08-03T11:00:00.0000000","timeZone":"UTC"},
            "isAllDay":false,"showAs":"busy","isCancelled":false,
            "organizer":{"emailAddress":{"name":"Ada","address":"ada@example.com"}},
            "attendees":[{"emailAddress":{"name":"Bob","address":"bob@example.com"},
                          "type":"optional","status":{"response":"accepted"}}],
            "seriesMasterId":"AAMkMASTER==",
            "isReminderOn":true,"reminderMinutesBeforeStart":15,
            "webLink":"https://outlook.office365.com/calendar/item/1"
        }"#;
        let event = serde_json::from_str::<GraphEvent>(json)
            .unwrap()
            .into_event("AAA=")
            .unwrap();

        assert_eq!(event.id, "AAMkAD==");
        assert_eq!(event.uid, "040000008200E0");
        assert_eq!(event.etag.as_deref(), Some("W/\"CQAAABYAAAA\""));
        assert_eq!(event.description.as_deref(), Some("agenda"));
        assert_eq!(event.location.as_deref(), Some("Room 3"));
        assert_eq!(
            event.start,
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap())
        );
        assert_eq!(event.transparency, Transparency::Opaque);
        assert!(event.attendees[0].optional);
        assert_eq!(event.attendees[0].status, ParticipationStatus::Accepted);
        assert_eq!(event.recurring_event_id.as_deref(), Some("AAMkMASTER=="));
        assert_eq!(event.reminders[0].minutes_before, 15);
    }

    #[test]
    fn all_day_event_becomes_a_date() {
        let json = r#"{"id":"e","isAllDay":true,
            "start":{"dateTime":"2026-08-03T00:00:00.0000000","timeZone":"UTC"},
            "end":{"dateTime":"2026-08-04T00:00:00.0000000","timeZone":"UTC"}}"#;
        let event = serde_json::from_str::<GraphEvent>(json)
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
    fn windows_zone_names_degrade_to_floating() {
        let json = r#"{"id":"e",
            "start":{"dateTime":"2026-08-03T12:00:00.0000000","timeZone":"W. Europe Standard Time"},
            "end":{"dateTime":"2026-08-03T13:00:00.0000000","timeZone":"W. Europe Standard Time"}}"#;
        let event = serde_json::from_str::<GraphEvent>(json)
            .unwrap()
            .into_event("cal")
            .unwrap();
        assert!(matches!(event.start, EventTime::Floating(_)));
    }

    #[test]
    fn iana_zone_names_are_kept() {
        let json = r#"{"id":"e",
            "start":{"dateTime":"2026-08-03T12:00:00.0000000","timeZone":"Europe/Berlin"},
            "end":{"dateTime":"2026-08-03T13:00:00.0000000","timeZone":"Europe/Berlin"}}"#;
        let event = serde_json::from_str::<GraphEvent>(json)
            .unwrap()
            .into_event("cal")
            .unwrap();
        assert_eq!(
            event.start.to_utc().to_rfc3339(),
            "2026-08-03T10:00:00+00:00"
        );
    }

    #[test]
    fn cancelled_and_free_events_map_correctly() {
        let json = r#"{"id":"e","isCancelled":true,"showAs":"free",
            "start":{"dateTime":"2026-08-03T10:00:00","timeZone":"UTC"},
            "end":{"dateTime":"2026-08-03T11:00:00","timeZone":"UTC"}}"#;
        let event = serde_json::from_str::<GraphEvent>(json)
            .unwrap()
            .into_event("cal")
            .unwrap();
        assert_eq!(event.status, EventStatus::Cancelled);
        assert_eq!(event.transparency, Transparency::Transparent);
    }

    #[test]
    fn unparseable_start_is_an_error() {
        let json = r#"{"id":"e","start":{"dateTime":"nonsense","timeZone":"UTC"},
                       "end":{"dateTime":"nonsense","timeZone":"UTC"}}"#;
        assert!(
            serde_json::from_str::<GraphEvent>(json)
                .unwrap()
                .into_event("cal")
                .is_err()
        );
    }

    #[test]
    fn serialized_payload_omits_server_owned_fields() {
        let event = Event::new(
            "Sync",
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
        );
        let json = serde_json::to_value(GraphEvent::from_event(&event)).unwrap();

        assert!(json.get("id").is_none());
        assert!(json.get("@odata.etag").is_none());
        assert!(json.get("webLink").is_none());
        assert_eq!(json["subject"], "Sync");
        assert_eq!(json["start"]["dateTime"], "2026-08-03T10:00:00");
        assert_eq!(json["start"]["timeZone"], "UTC");
        assert_eq!(json["showAs"], "busy");
        assert_eq!(json["isReminderOn"], false);
    }

    #[test]
    fn zoned_start_is_converted_to_utc_on_write() {
        let naive = NaiveDate::from_ymd_opt(2026, 8, 3)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let event = Event::new(
            "Sync",
            EventTime::Zoned {
                naive,
                tz: chrono_tz::Europe::Berlin,
            },
            EventTime::Zoned {
                naive,
                tz: chrono_tz::Europe::Berlin,
            },
        );
        let json = serde_json::to_value(GraphEvent::from_event(&event)).unwrap();
        assert_eq!(json["start"]["dateTime"], "2026-08-03T10:00:00");
    }

    #[tokio::test]
    async fn recurring_writes_are_refused_rather_than_flattened() {
        let engine = MicrosoftGraph::new(HttpClient::new(
            reqwest::Client::new(),
            crate::integrations::auth::Auth::None,
            "microsoft-graph",
        ));
        let mut event = Event::new(
            "Standup",
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
            EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 15, 0).unwrap()),
        );
        event.recurrence = vec!["RRULE:FREQ=DAILY".into()];

        let err = engine.create_event("cal", &event).await.unwrap_err();
        assert!(matches!(err, CalendarError::Unsupported { .. }));
    }

    #[test]
    fn pagination_link_is_read() {
        let json = r#"{"value":[],"@odata.nextLink":"https://graph.microsoft.com/next"}"#;
        let page: OdataPage<GraphEvent> = serde_json::from_str(json).unwrap();
        assert_eq!(
            page.next_link.as_deref(),
            Some("https://graph.microsoft.com/next")
        );
    }
}
