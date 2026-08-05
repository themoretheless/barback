use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// A start or end instant as calendars actually express them.
///
/// RFC 5545 allows four shapes and the REST APIs map onto the same four, so the
/// distinction is kept instead of collapsing everything to UTC on ingest. An
/// all-day event and an event at midnight UTC are not the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTime {
    /// Date-only value (`VALUE=DATE`), i.e. an all-day event.
    Date(NaiveDate),
    /// Wall-clock time with no zone attached ("floating" in RFC 5545 terms).
    Floating(NaiveDateTime),
    /// Wall-clock time in a named IANA zone (`TZID=`).
    Zoned { naive: NaiveDateTime, tz: Tz },
    /// Absolute instant (the trailing `Z` form).
    Utc(DateTime<Utc>),
}

impl EventTime {
    /// Best-effort absolute instant.
    ///
    /// Floating times are interpreted as UTC. For a zoned time that falls in a
    /// DST gap the next valid instant is used; for an ambiguous time the earlier
    /// of the two is used.
    pub fn to_utc(&self) -> DateTime<Utc> {
        match self {
            EventTime::Date(d) => d
                .and_hms_opt(0, 0, 0)
                .expect("midnight is always valid")
                .and_utc(),
            EventTime::Floating(ndt) => ndt.and_utc(),
            EventTime::Utc(dt) => *dt,
            EventTime::Zoned { naive, tz } => match tz.from_local_datetime(naive) {
                chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
                chrono::LocalResult::Ambiguous(earlier, _) => earlier.with_timezone(&Utc),
                chrono::LocalResult::None => {
                    // DST gap: walk forward until the local time exists again.
                    let mut probe = *naive;
                    for _ in 0..24 {
                        probe += ChronoDuration::minutes(15);
                        if let chrono::LocalResult::Single(dt) = tz.from_local_datetime(&probe) {
                            return dt.with_timezone(&Utc);
                        }
                    }
                    naive.and_utc()
                }
            },
        }
    }

    pub fn is_date(&self) -> bool {
        matches!(self, EventTime::Date(_))
    }

    /// IANA zone name, when the value carries one.
    pub fn timezone(&self) -> Option<&'static str> {
        match self {
            EventTime::Zoned { tz, .. } => Some(tz.name()),
            EventTime::Utc(_) => Some("UTC"),
            _ => None,
        }
    }
}

impl From<DateTime<Utc>> for EventTime {
    fn from(dt: DateTime<Utc>) -> Self {
        EventTime::Utc(dt)
    }
}

impl From<NaiveDate> for EventTime {
    fn from(d: NaiveDate) -> Self {
        EventTime::Date(d)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventStatus {
    #[default]
    Confirmed,
    Tentative,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transparency {
    /// Blocks free/busy time.
    #[default]
    Opaque,
    /// Does not block free/busy time.
    Transparent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParticipationStatus {
    #[default]
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    Delegated,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Person {
    pub email: Option<String>,
    pub display_name: Option<String>,
}

impl Person {
    pub fn from_email(email: impl Into<String>) -> Self {
        Person {
            email: Some(email.into()),
            display_name: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Attendee {
    pub person: Person,
    pub status: ParticipationStatus,
    pub optional: bool,
    pub organizer: bool,
    pub resource: bool,
}

impl Attendee {
    pub fn required(email: impl Into<String>) -> Self {
        Attendee {
            person: Person::from_email(email),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderMethod {
    Popup,
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reminder {
    pub method: ReminderMethod,
    /// Minutes before the start of the event.
    pub minutes_before: i64,
}

/// A calendar collection on some provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Calendar {
    /// Provider-native identifier. For CalDAV this is the collection URL.
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// IANA zone name the provider considers the calendar's default.
    pub timezone: Option<String>,
    pub color: Option<String>,
    pub read_only: bool,
    pub primary: bool,
}

/// A single event, used both for reads and as the payload for writes.
///
/// On create, `id` and `etag` are ignored and filled in from the server
/// response. On update, `etag` is sent as an optimistic-concurrency guard where
/// the provider supports one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Provider-native identifier. For CalDAV this is the resource href.
    pub id: String,
    /// iCalendar UID. Stable across providers, unlike `id`.
    pub uid: String,
    pub etag: Option<String>,
    pub calendar_id: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    pub start: EventTime,
    pub end: Option<EventTime>,
    pub status: EventStatus,
    pub transparency: Transparency,
    pub organizer: Option<Person>,
    pub attendees: Vec<Attendee>,
    /// Raw recurrence lines (`RRULE:...`, `RDATE:...`, `EXDATE:...`), passed
    /// through untouched. Expansion is deliberately out of scope.
    pub recurrence: Vec<String>,
    /// Set on an instance of a recurring series, pointing at the master event.
    pub recurring_event_id: Option<String>,
    pub reminders: Vec<Reminder>,
    pub url: Option<String>,
    pub created: Option<DateTime<Utc>>,
    pub updated: Option<DateTime<Utc>>,
}

impl Event {
    pub fn new(summary: impl Into<String>, start: EventTime, end: EventTime) -> Self {
        Event {
            id: String::new(),
            uid: new_uid(),
            etag: None,
            calendar_id: String::new(),
            summary: Some(summary.into()),
            description: None,
            location: None,
            start,
            end: Some(end),
            status: EventStatus::default(),
            transparency: Transparency::default(),
            organizer: None,
            attendees: Vec::new(),
            recurrence: Vec::new(),
            recurring_event_id: None,
            reminders: Vec::new(),
            url: None,
            created: None,
            updated: None,
        }
    }

    pub fn all_day(&self) -> bool {
        self.start.is_date()
    }

    /// End instant, falling back to a sensible default when the provider omits
    /// one: one day for all-day events, otherwise a zero-length event.
    pub fn end_or_default(&self) -> DateTime<Utc> {
        match &self.end {
            Some(e) => e.to_utc(),
            None if self.all_day() => self.start.to_utc() + ChronoDuration::days(1),
            None => self.start.to_utc(),
        }
    }

    pub fn overlaps(&self, range: &TimeRange) -> bool {
        self.start.to_utc() < range.end && self.end_or_default() > range.start
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn with_attendees(mut self, attendees: Vec<Attendee>) -> Self {
        self.attendees = attendees;
        self
    }
}

/// Half-open UTC window `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeRange {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        TimeRange { start, end }
    }

    pub fn days_from_now(days: i64) -> Self {
        let now = Utc::now();
        TimeRange {
            start: now,
            end: now + ChronoDuration::days(days),
        }
    }
}

/// What a given provider can actually do, so callers can branch without
/// probing with a request that is going to fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub read: bool,
    pub write: bool,
    pub delete: bool,
    /// The account exposes more than one calendar collection.
    pub multiple_calendars: bool,
    /// Attendee lists round-trip through create/update.
    pub attendees: bool,
    /// Server-side reminders round-trip through create/update.
    pub reminders: bool,
    /// Optimistic concurrency via ETag / change key.
    pub etags: bool,
}

impl Capabilities {
    pub const READ_ONLY: Capabilities = Capabilities {
        read: true,
        write: false,
        delete: false,
        multiple_calendars: false,
        attendees: false,
        reminders: false,
        etags: false,
    };

    pub const FULL: Capabilities = Capabilities {
        read: true,
        write: true,
        delete: true,
        multiple_calendars: true,
        attendees: true,
        reminders: true,
        etags: true,
    };
}

/// Generates an RFC 5545 UID.
///
/// Not a UUID: there is no `uuid` dependency and iCalendar only requires global
/// uniqueness, which monotonic nanos plus a process-local counter provides for
/// a single writer.
pub fn new_uid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{}@barback", nanos, seq, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_day_event_is_not_midnight_utc() {
        let d = NaiveDate::from_ymd_opt(2026, 8, 3).unwrap();
        let ev = Event::new(
            "offsite",
            EventTime::Date(d),
            EventTime::Date(NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()),
        );
        assert!(ev.all_day());
        assert_eq!(ev.start.timezone(), None);
    }

    #[test]
    fn zoned_time_converts_through_its_zone() {
        let naive = NaiveDate::from_ymd_opt(2026, 8, 3)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let t = EventTime::Zoned {
            naive,
            tz: chrono_tz::Europe::Berlin,
        };
        // Berlin is UTC+2 in August.
        assert_eq!(t.to_utc().to_rfc3339(), "2026-08-03T10:00:00+00:00");
    }

    #[test]
    fn dst_gap_resolves_forward() {
        // 02:30 on the European spring-forward night does not exist in Berlin.
        let naive = NaiveDate::from_ymd_opt(2026, 3, 29)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        let t = EventTime::Zoned {
            naive,
            tz: chrono_tz::Europe::Berlin,
        };
        assert_eq!(t.to_utc().to_rfc3339(), "2026-03-29T01:00:00+00:00");
    }

    #[test]
    fn overlap_is_half_open() {
        let start = Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap();
        let ev = Event::new("standup", EventTime::Utc(start), EventTime::Utc(end));

        // A window that ends exactly when the event starts does not overlap.
        let before = TimeRange::new(start - ChronoDuration::hours(1), start);
        assert!(!ev.overlaps(&before));

        let touching = TimeRange::new(start, end);
        assert!(ev.overlaps(&touching));
    }

    #[test]
    fn uids_are_unique() {
        let a = new_uid();
        let b = new_uid();
        assert_ne!(a, b);
    }
}
