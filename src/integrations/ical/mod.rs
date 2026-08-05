//! iCalendar (RFC 5545) support: the syntax layer plus the mapping between
//! `VEVENT` and [`Event`].
//!
//! Recurrence rules are carried through verbatim. Expanding a series into
//! instances is deliberately not attempted here; where a provider can expand
//! server-side (Google `singleEvents`, Graph `calendarView`) the engine asks it
//! to.

pub mod syntax;
mod timezone;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;

use super::error::{CalendarError, Result};
use super::model::{
    Attendee, Event, EventStatus, EventTime, ParticipationStatus, Person, Reminder, ReminderMethod,
    Transparency, new_uid,
};
use syntax::{Component, Prop, escape_text, parse, write};

const PRODID: &str = "-//barback//calendar//EN";

/// Extracts every `VEVENT` from an iCalendar stream.
///
/// `VEVENT`s nested in a `VCALENDAR` and stray top-level ones are both
/// accepted, because real-world feeds produce both.
pub fn parse_events(ics: &str) -> Result<Vec<Event>> {
    let roots = parse(ics)?;
    let mut events = Vec::new();
    for root in &roots {
        if root.name.eq_ignore_ascii_case("VEVENT") {
            events.push(event_from_component(root)?);
            continue;
        }
        for child in root.children_named("VEVENT") {
            events.push(event_from_component(child)?);
        }
    }
    Ok(events)
}

/// Serializes one event as a complete `VCALENDAR` document, ready to `PUT` to a
/// CalDAV collection.
pub fn to_ics(event: &Event) -> String {
    let mut calendar = Component::new("VCALENDAR");
    calendar.push_prop(Prop::new("VERSION", "2.0"));
    calendar.push_prop(Prop::new("PRODID", PRODID));
    calendar.push_prop(Prop::new("CALSCALE", "GREGORIAN"));

    // A zone is only preserved when it can actually matter, i.e. for a
    // recurring series where DST shifts individual instances. Single events are
    // written as absolute UTC, which needs no VTIMEZONE and cannot drift.
    let keep_zone = !event.recurrence.is_empty();
    if keep_zone {
        for tz in zones_used(event) {
            calendar
                .children
                .push(timezone::vtimezone(tz, event_year(event)));
        }
    }

    calendar.children.push(vevent(event, keep_zone));
    write(&calendar)
}

fn event_year(event: &Event) -> i32 {
    event.start.to_utc().year()
}

fn zones_used(event: &Event) -> Vec<Tz> {
    let mut zones = Vec::new();
    for t in [Some(&event.start), event.end.as_ref()]
        .into_iter()
        .flatten()
    {
        if let EventTime::Zoned { tz, .. } = t
            && !zones.contains(tz)
        {
            zones.push(*tz);
        }
    }
    zones
}

fn vevent(event: &Event, keep_zone: bool) -> Component {
    let mut c = Component::new("VEVENT");

    let uid = if event.uid.is_empty() {
        new_uid()
    } else {
        event.uid.clone()
    };
    c.push_prop(Prop::new("UID", uid));
    c.push_prop(Prop::new("DTSTAMP", format_utc(Utc::now())));

    c.push_prop(date_time_prop("DTSTART", &event.start, keep_zone));
    if let Some(end) = &event.end {
        c.push_prop(date_time_prop("DTEND", end, keep_zone));
    }

    if let Some(summary) = &event.summary {
        c.push_prop(Prop::new("SUMMARY", escape_text(summary)));
    }
    if let Some(description) = &event.description {
        c.push_prop(Prop::new("DESCRIPTION", escape_text(description)));
    }
    if let Some(location) = &event.location {
        c.push_prop(Prop::new("LOCATION", escape_text(location)));
    }
    if let Some(url) = &event.url {
        c.push_prop(Prop::new("URL", url));
    }

    c.push_prop(Prop::new(
        "STATUS",
        match event.status {
            EventStatus::Confirmed => "CONFIRMED",
            EventStatus::Tentative => "TENTATIVE",
            EventStatus::Cancelled => "CANCELLED",
        },
    ));
    c.push_prop(Prop::new(
        "TRANSP",
        match event.transparency {
            Transparency::Opaque => "OPAQUE",
            Transparency::Transparent => "TRANSPARENT",
        },
    ));

    if let Some(organizer) = &event.organizer
        && let Some(email) = &organizer.email
    {
        let mut p = Prop::new("ORGANIZER", format!("mailto:{email}"));
        if let Some(name) = &organizer.display_name {
            p = p.with_param("CN", name.clone());
        }
        c.push_prop(p);
    }

    for attendee in &event.attendees {
        let Some(email) = &attendee.person.email else {
            continue;
        };
        let mut p = Prop::new("ATTENDEE", format!("mailto:{email}"));
        if let Some(name) = &attendee.person.display_name {
            p = p.with_param("CN", name.clone());
        }
        p = p.with_param(
            "ROLE",
            if attendee.optional {
                "OPT-PARTICIPANT"
            } else {
                "REQ-PARTICIPANT"
            },
        );
        p = p.with_param(
            "PARTSTAT",
            match attendee.status {
                ParticipationStatus::NeedsAction => "NEEDS-ACTION",
                ParticipationStatus::Accepted => "ACCEPTED",
                ParticipationStatus::Declined => "DECLINED",
                ParticipationStatus::Tentative => "TENTATIVE",
                ParticipationStatus::Delegated => "DELEGATED",
            },
        );
        if attendee.resource {
            p = p.with_param("CUTYPE", "RESOURCE");
        }
        c.push_prop(p);
    }

    // Recurrence lines are stored raw ("RRULE:FREQ=WEEKLY"), so they are parsed
    // back into name/value rather than re-derived.
    for line in &event.recurrence {
        if let Ok(prop) = syntax::parse_content_line(line) {
            c.push_prop(prop);
        }
    }
    if let Some(rid) = &event.recurring_event_id {
        c.push_prop(Prop::new("RECURRENCE-ID", rid));
    }

    for reminder in &event.reminders {
        let mut alarm = Component::new("VALARM");
        alarm.push_prop(Prop::new(
            "ACTION",
            match reminder.method {
                ReminderMethod::Popup => "DISPLAY",
                ReminderMethod::Email => "EMAIL",
            },
        ));
        alarm.push_prop(
            Prop::new("TRIGGER", format_trigger(reminder.minutes_before))
                .with_param("RELATED", "START"),
        );
        alarm.push_prop(Prop::new("DESCRIPTION", "Reminder"));
        c.children.push(alarm);
    }

    c
}

fn date_time_prop(name: &str, time: &EventTime, keep_zone: bool) -> Prop {
    match time {
        EventTime::Date(d) => {
            Prop::new(name, d.format("%Y%m%d").to_string()).with_param("VALUE", "DATE")
        }
        EventTime::Floating(ndt) => Prop::new(name, ndt.format("%Y%m%dT%H%M%S").to_string()),
        EventTime::Zoned { naive, tz } if keep_zone => {
            Prop::new(name, naive.format("%Y%m%dT%H%M%S").to_string()).with_param("TZID", tz.name())
        }
        other => Prop::new(name, format_utc(other.to_utc())),
    }
}

fn format_utc(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

fn format_trigger(minutes_before: i64) -> String {
    match minutes_before {
        0 => "PT0S".to_string(),
        n if n > 0 => format!("-PT{n}M"),
        n => format!("PT{}M", -n),
    }
}

fn event_from_component(c: &Component) -> Result<Event> {
    let start_prop = c
        .prop("DTSTART")
        .ok_or_else(|| CalendarError::parse("VEVENT", "missing DTSTART"))?;
    let start = parse_date_time(start_prop)?;

    let end = match c.prop("DTEND") {
        Some(p) => Some(parse_date_time(p)?),
        None => match c.prop("DURATION") {
            // DTEND and DURATION are mutually exclusive per RFC 5545; when only
            // DURATION is present it is applied to the start.
            Some(p) => parse_duration(&p.value).map(|d| shift(&start, d)),
            None => None,
        },
    };

    let mut attendees = Vec::new();
    for p in c.props_named("ATTENDEE") {
        attendees.push(attendee_from_prop(p));
    }

    let mut recurrence = Vec::new();
    for name in ["RRULE", "RDATE", "EXDATE", "EXRULE"] {
        for p in c.props_named(name) {
            let mut line = p.name.clone();
            for (k, v) in &p.params {
                line.push(';');
                line.push_str(k);
                line.push('=');
                line.push_str(v);
            }
            line.push(':');
            line.push_str(&p.value);
            recurrence.push(line);
        }
    }

    let reminders = c
        .children_named("VALARM")
        .filter_map(reminder_from_alarm)
        .collect();

    let status = match c.prop_text("STATUS").as_deref() {
        Some("TENTATIVE") => EventStatus::Tentative,
        Some("CANCELLED") => EventStatus::Cancelled,
        _ => EventStatus::Confirmed,
    };

    let transparency = match c.prop_text("TRANSP").as_deref() {
        Some("TRANSPARENT") => Transparency::Transparent,
        _ => Transparency::Opaque,
    };

    let organizer = c.prop("ORGANIZER").map(|p| Person {
        email: Some(strip_mailto(&p.value)),
        display_name: p.param("CN").map(str::to_string),
    });

    Ok(Event {
        id: String::new(),
        uid: c.prop_text("UID").unwrap_or_else(new_uid),
        etag: None,
        calendar_id: String::new(),
        summary: c.prop_text("SUMMARY"),
        description: c.prop_text("DESCRIPTION"),
        location: c.prop_text("LOCATION"),
        start,
        end,
        status,
        transparency,
        organizer,
        attendees,
        recurrence,
        recurring_event_id: c.prop_text("RECURRENCE-ID"),
        reminders,
        url: c.prop_text("URL"),
        created: c.prop("CREATED").and_then(|p| parse_utc_stamp(&p.value)),
        updated: c
            .prop("LAST-MODIFIED")
            .or_else(|| c.prop("DTSTAMP"))
            .and_then(|p| parse_utc_stamp(&p.value)),
    })
}

fn attendee_from_prop(p: &Prop) -> Attendee {
    Attendee {
        person: Person {
            email: Some(strip_mailto(&p.value)),
            display_name: p.param("CN").map(str::to_string),
        },
        status: match p.param("PARTSTAT") {
            Some("ACCEPTED") => ParticipationStatus::Accepted,
            Some("DECLINED") => ParticipationStatus::Declined,
            Some("TENTATIVE") => ParticipationStatus::Tentative,
            Some("DELEGATED") => ParticipationStatus::Delegated,
            _ => ParticipationStatus::NeedsAction,
        },
        optional: matches!(p.param("ROLE"), Some("OPT-PARTICIPANT")),
        organizer: matches!(p.param("ROLE"), Some("CHAIR")),
        resource: matches!(p.param("CUTYPE"), Some("RESOURCE") | Some("ROOM")),
    }
}

fn reminder_from_alarm(alarm: &Component) -> Option<Reminder> {
    let trigger = alarm.prop("TRIGGER")?;
    // Absolute triggers (VALUE=DATE-TIME) cannot be expressed as "minutes
    // before start" without the start, and are rare; they are skipped.
    if matches!(trigger.param("VALUE"), Some("DATE-TIME")) {
        return None;
    }
    let duration = parse_duration(&trigger.value)?;
    let method = match alarm.prop_text("ACTION").as_deref() {
        Some("EMAIL") => ReminderMethod::Email,
        _ => ReminderMethod::Popup,
    };
    Some(Reminder {
        method,
        minutes_before: -duration.num_minutes(),
    })
}

fn strip_mailto(value: &str) -> String {
    value
        .strip_prefix("mailto:")
        .or_else(|| value.strip_prefix("MAILTO:"))
        .unwrap_or(value)
        .to_string()
}

fn shift(time: &EventTime, delta: Duration) -> EventTime {
    match time {
        EventTime::Date(d) => EventTime::Date(*d + delta),
        EventTime::Floating(ndt) => EventTime::Floating(*ndt + delta),
        EventTime::Zoned { naive, tz } => EventTime::Zoned {
            naive: *naive + delta,
            tz: *tz,
        },
        EventTime::Utc(dt) => EventTime::Utc(*dt + delta),
    }
}

/// Parses a `DTSTART`/`DTEND` property into the shape it actually carries.
pub fn parse_date_time(prop: &Prop) -> Result<EventTime> {
    let value = prop.value.trim();

    if matches!(prop.param("VALUE"), Some("DATE")) || value.len() == 8 {
        let d = NaiveDate::parse_from_str(value, "%Y%m%d")
            .map_err(|e| CalendarError::parse("DATE", format!("{value:?}: {e}")))?;
        return Ok(EventTime::Date(d));
    }

    if let Some(stripped) = value.strip_suffix('Z') {
        let ndt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S")
            .map_err(|e| CalendarError::parse("DATE-TIME", format!("{value:?}: {e}")))?;
        return Ok(EventTime::Utc(ndt.and_utc()));
    }

    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .map_err(|e| CalendarError::parse("DATE-TIME", format!("{value:?}: {e}")))?;

    match prop.param("TZID").and_then(resolve_tzid) {
        Some(tz) => Ok(EventTime::Zoned { naive, tz }),
        // An unresolvable TZID degrades to floating rather than failing the
        // whole feed over one unknown zone name.
        None => Ok(EventTime::Floating(naive)),
    }
}

/// Resolves a `TZID` parameter to an IANA zone.
///
/// Handles the `/mozilla.org/20050126_1/Europe/Berlin` style prefixes that
/// several clients emit. Windows zone names ("W. Europe Standard Time") are not
/// mapped and fall through to `None`.
fn resolve_tzid(tzid: &str) -> Option<Tz> {
    if let Ok(tz) = tzid.parse::<Tz>() {
        return Some(tz);
    }
    // Try progressively shorter suffixes: ".../Europe/Berlin" -> "Europe/Berlin".
    let parts: Vec<&str> = tzid.split('/').collect();
    for start in 1..parts.len() {
        if let Ok(tz) = parts[start..].join("/").parse::<Tz>() {
            return Some(tz);
        }
    }
    None
}

fn parse_utc_stamp(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim().trim_end_matches('Z');
    NaiveDateTime::parse_from_str(trimmed, "%Y%m%dT%H%M%S")
        .ok()
        .map(|ndt| ndt.and_utc())
}

/// Parses the RFC 5545 duration subset used by `DURATION` and `TRIGGER`.
///
/// Grammar: `[+-]P[nW][nD][T[nH][nM][nS]]`.
pub fn parse_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    let (negative, rest) = match value.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let rest = rest.strip_prefix('P').or_else(|| rest.strip_prefix('p'))?;

    let mut total = Duration::zero();
    let mut number = String::new();
    let mut in_time = false;
    let mut saw_unit = false;

    for ch in rest.chars() {
        match ch {
            'T' | 't' => {
                in_time = true;
                number.clear();
            }
            '0'..='9' => number.push(ch),
            unit => {
                let n: i64 = number.parse().ok()?;
                number.clear();
                let part = match (unit.to_ascii_uppercase(), in_time) {
                    ('W', _) => Duration::weeks(n),
                    ('D', _) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
                total += part;
                saw_unit = true;
            }
        }
    }

    if !saw_unit {
        return None;
    }
    Some(if negative { -total } else { total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\n\
VERSION:2.0\r\n\
BEGIN:VEVENT\r\n\
UID:abc-123\r\n\
DTSTAMP:20260801T090000Z\r\n\
DTSTART;TZID=Europe/Berlin:20260803T120000\r\n\
DTEND;TZID=Europe/Berlin:20260803T130000\r\n\
SUMMARY:Weekly sync\\, all hands\r\n\
DESCRIPTION:Line one\\nLine two\r\n\
LOCATION:Room 3\r\n\
STATUS:CONFIRMED\r\n\
TRANSP:OPAQUE\r\n\
ORGANIZER;CN=Ada:mailto:ada@example.com\r\n\
ATTENDEE;CN=Bob;ROLE=OPT-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:bob@example.com\r\n\
RRULE:FREQ=WEEKLY;BYDAY=MO\r\n\
BEGIN:VALARM\r\n\
ACTION:DISPLAY\r\n\
TRIGGER;RELATED=START:-PT15M\r\n\
END:VALARM\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn parses_a_realistic_event() {
        let events = parse_events(SAMPLE).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];

        assert_eq!(e.uid, "abc-123");
        assert_eq!(e.summary.as_deref(), Some("Weekly sync, all hands"));
        assert_eq!(e.description.as_deref(), Some("Line one\nLine two"));
        assert_eq!(e.location.as_deref(), Some("Room 3"));
        assert_eq!(e.status, EventStatus::Confirmed);
        assert_eq!(
            e.start,
            EventTime::Zoned {
                naive: NaiveDate::from_ymd_opt(2026, 8, 3)
                    .unwrap()
                    .and_hms_opt(12, 0, 0)
                    .unwrap(),
                tz: chrono_tz::Europe::Berlin,
            }
        );
        assert_eq!(e.start.to_utc().to_rfc3339(), "2026-08-03T10:00:00+00:00");
        assert_eq!(
            e.organizer.as_ref().unwrap().email.as_deref(),
            Some("ada@example.com")
        );
        assert_eq!(e.attendees.len(), 1);
        assert!(e.attendees[0].optional);
        assert_eq!(e.attendees[0].status, ParticipationStatus::Accepted);
        assert_eq!(e.recurrence, vec!["RRULE:FREQ=WEEKLY;BYDAY=MO"]);
        assert_eq!(e.reminders.len(), 1);
        assert_eq!(e.reminders[0].minutes_before, 15);
        assert_eq!(
            e.updated,
            Some(Utc.with_ymd_and_hms(2026, 8, 1, 9, 0, 0).unwrap())
        );
    }

    #[test]
    fn round_trips_through_ics() {
        let original = &parse_events(SAMPLE).unwrap()[0];
        let reparsed = &parse_events(&to_ics(original)).unwrap()[0];

        assert_eq!(reparsed.uid, original.uid);
        assert_eq!(reparsed.summary, original.summary);
        assert_eq!(reparsed.description, original.description);
        assert_eq!(reparsed.location, original.location);
        assert_eq!(reparsed.recurrence, original.recurrence);
        assert_eq!(reparsed.attendees, original.attendees);
        assert_eq!(reparsed.reminders, original.reminders);
        // The event recurs, so the named zone is preserved rather than
        // flattened to UTC.
        assert_eq!(reparsed.start, original.start);
    }

    #[test]
    fn non_recurring_zoned_event_is_written_as_utc() {
        let naive = NaiveDate::from_ymd_opt(2026, 8, 3)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let ev = Event::new(
            "one off",
            EventTime::Zoned {
                naive,
                tz: chrono_tz::Europe::Berlin,
            },
            EventTime::Zoned {
                naive: naive + Duration::hours(1),
                tz: chrono_tz::Europe::Berlin,
            },
        );
        let ics = to_ics(&ev);
        assert!(ics.contains("DTSTART:20260803T100000Z"), "{ics}");
        assert!(!ics.contains("BEGIN:VTIMEZONE"));

        // The absolute instant survives even though the zone name does not.
        let back = &parse_events(&ics).unwrap()[0];
        assert_eq!(back.start.to_utc(), ev.start.to_utc());
    }

    #[test]
    fn recurring_zoned_event_carries_a_vtimezone() {
        let naive = NaiveDate::from_ymd_opt(2026, 8, 3)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let mut ev = Event::new(
            "standup",
            EventTime::Zoned {
                naive,
                tz: chrono_tz::Europe::Berlin,
            },
            EventTime::Zoned {
                naive: naive + Duration::minutes(15),
                tz: chrono_tz::Europe::Berlin,
            },
        );
        ev.recurrence = vec!["RRULE:FREQ=DAILY".into()];
        let ics = to_ics(&ev);
        assert!(ics.contains("BEGIN:VTIMEZONE"), "{ics}");
        assert!(ics.contains("TZID:Europe/Berlin"), "{ics}");
        assert!(
            ics.contains("DTSTART;TZID=Europe/Berlin:20260803T120000"),
            "{ics}"
        );
    }

    #[test]
    fn all_day_event_uses_date_values() {
        let ev = Event::new(
            "holiday",
            EventTime::Date(NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()),
            EventTime::Date(NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()),
        );
        let ics = to_ics(&ev);
        assert!(ics.contains("DTSTART;VALUE=DATE:20260803"), "{ics}");
        let back = &parse_events(&ics).unwrap()[0];
        assert!(back.all_day());
    }

    #[test]
    fn duration_replaces_a_missing_dtend() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\n\
                   DTSTART:20260803T100000Z\r\nDURATION:PT1H30M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let e = &parse_events(ics).unwrap()[0];
        assert_eq!(
            e.end.as_ref().unwrap().to_utc(),
            Utc.with_ymd_and_hms(2026, 8, 3, 11, 30, 0).unwrap()
        );
    }

    #[test]
    fn parses_duration_forms() {
        assert_eq!(parse_duration("PT15M"), Some(Duration::minutes(15)));
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(parse_duration("P2W"), Some(Duration::weeks(2)));
        assert_eq!(parse_duration("PT0S"), Some(Duration::zero()));
        assert_eq!(parse_duration("garbage"), None);
        assert_eq!(parse_duration("P"), None);
    }

    #[test]
    fn resolves_prefixed_tzids() {
        assert_eq!(
            resolve_tzid("/mozilla.org/20050126_1/Europe/Berlin"),
            Some(chrono_tz::Europe::Berlin)
        );
        assert_eq!(
            resolve_tzid("Europe/Berlin"),
            Some(chrono_tz::Europe::Berlin)
        );
        assert_eq!(resolve_tzid("W. Europe Standard Time"), None);
    }

    #[test]
    fn unknown_tzid_degrades_to_floating_instead_of_failing() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\n\
                   DTSTART;TZID=W. Europe Standard Time:20260803T120000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let e = &parse_events(ics).unwrap()[0];
        assert!(matches!(e.start, EventTime::Floating(_)));
    }

    #[test]
    fn event_without_dtstart_is_rejected() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(parse_events(ics).is_err());
    }

    #[test]
    fn absolute_alarm_trigger_is_skipped_not_misread() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20260803T100000Z\r\n\
                   BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;VALUE=DATE-TIME:20260803T090000Z\r\n\
                   END:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let e = &parse_events(ics).unwrap()[0];
        assert!(e.reminders.is_empty());
    }

    #[test]
    fn handles_multiple_events_in_one_feed() {
        let ics = "BEGIN:VCALENDAR\r\n\
                   BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260803T100000Z\r\nEND:VEVENT\r\n\
                   BEGIN:VEVENT\r\nUID:b\r\nDTSTART:20260804T100000Z\r\nEND:VEVENT\r\n\
                   END:VCALENDAR\r\n";
        assert_eq!(parse_events(ics).unwrap().len(), 2);
    }
}
