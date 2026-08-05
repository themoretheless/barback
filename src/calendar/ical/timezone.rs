//! Generates `VTIMEZONE` components from the IANA database.
//!
//! Only needed when an event keeps a named zone instead of being flattened to
//! UTC, which here means recurring events: a weekly 09:00 Berlin meeting must
//! stay at 09:00 across the DST boundary, and a server cannot honour `TZID`
//! without the matching `VTIMEZONE`.
//!
//! Transitions are emitted explicitly rather than as `RRULE`s. That is valid
//! RFC 5545 and avoids having to reverse-engineer a rule out of the tz
//! database, at the cost of covering only a bounded window of years.

use chrono::{Duration, NaiveDate, NaiveDateTime, Offset, TimeZone};
use chrono_tz::{OffsetComponents, Tz};

use super::syntax::{Component, Prop};

/// Years before the event covered by the generated transitions.
const YEARS_BEFORE: i32 = 1;
/// Years after the event covered by the generated transitions. A recurring
/// event that outlives this window falls back to the last emitted offset,
/// which is what clients do with a truncated VTIMEZONE anyway.
const YEARS_AFTER: i32 = 10;

struct Transition {
    /// Local wall time at which the new offset takes effect.
    local_start: NaiveDateTime,
    offset_from: i32,
    offset_to: i32,
    is_dst: bool,
}

pub fn vtimezone(tz: Tz, around_year: i32) -> Component {
    let mut component = Component::new("VTIMEZONE");
    component.push_prop(Prop::new("TZID", tz.name()));

    let from_year = around_year - YEARS_BEFORE;
    let to_year = around_year + YEARS_AFTER;
    let transitions = find_transitions(tz, from_year, to_year);

    if transitions.is_empty() {
        // A zone with no DST still needs one subcomponent to be well-formed.
        let start = NaiveDate::from_ymd_opt(from_year, 1, 1)
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let offset = offset_seconds(tz, start);
        component.children.push(subcomponent(&Transition {
            local_start: start + Duration::seconds(offset as i64),
            offset_from: offset,
            offset_to: offset,
            is_dst: false,
        }));
        return component;
    }

    for transition in &transitions {
        component.children.push(subcomponent(transition));
    }
    component
}

fn subcomponent(t: &Transition) -> Component {
    let mut c = Component::new(if t.is_dst { "DAYLIGHT" } else { "STANDARD" });
    c.push_prop(Prop::new(
        "DTSTART",
        t.local_start.format("%Y%m%dT%H%M%S").to_string(),
    ));
    c.push_prop(Prop::new("TZOFFSETFROM", format_offset(t.offset_from)));
    c.push_prop(Prop::new("TZOFFSETTO", format_offset(t.offset_to)));
    c
}

/// Walks the window one day at a time and, whenever the UTC offset changes,
/// narrows the transition down to the minute.
fn find_transitions(tz: Tz, from_year: i32, to_year: i32) -> Vec<Transition> {
    let Some(start_date) = NaiveDate::from_ymd_opt(from_year, 1, 1) else {
        return Vec::new();
    };
    let Some(end_date) = NaiveDate::from_ymd_opt(to_year, 1, 1) else {
        return Vec::new();
    };

    let mut transitions = Vec::new();
    let mut probe = start_date.and_hms_opt(12, 0, 0).expect("noon is valid");
    let mut previous_offset = offset_seconds(tz, probe);

    while probe.date() < end_date {
        let next = probe + Duration::days(1);
        let next_offset = offset_seconds(tz, next);

        if next_offset != previous_offset {
            let instant = narrow_to_minute(tz, probe, previous_offset);
            transitions.push(Transition {
                local_start: instant + Duration::seconds(next_offset as i64),
                offset_from: previous_offset,
                offset_to: next_offset,
                is_dst: is_dst(tz, instant + Duration::minutes(1)),
            });
            previous_offset = next_offset;
        }

        probe = next;
    }

    transitions
}

/// Given a day known to contain an offset change, returns the UTC instant of
/// the first minute with the new offset.
fn narrow_to_minute(tz: Tz, day_start: NaiveDateTime, previous_offset: i32) -> NaiveDateTime {
    let mut low = 0i64;
    let mut high = 24 * 60i64;
    while low < high {
        let mid = (low + high) / 2;
        let candidate = day_start + Duration::minutes(mid);
        if offset_seconds(tz, candidate) == previous_offset {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    day_start + Duration::minutes(low)
}

fn offset_seconds(tz: Tz, utc: NaiveDateTime) -> i32 {
    tz.offset_from_utc_datetime(&utc).fix().local_minus_utc()
}

fn is_dst(tz: Tz, utc: NaiveDateTime) -> bool {
    !tz.offset_from_utc_datetime(&utc).dst_offset().is_zero()
}

fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    let secs = abs % 60;
    if secs == 0 {
        format!("{sign}{hours:02}{minutes:02}")
    } else {
        format!("{sign}{hours:02}{minutes:02}{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::ical::syntax::write;

    #[test]
    fn berlin_has_two_transitions_per_year() {
        let transitions = find_transitions(chrono_tz::Europe::Berlin, 2026, 2028);
        // 2026 and 2027, spring forward and fall back.
        assert_eq!(transitions.len(), 4);
        assert_eq!(transitions[0].offset_from, 3600);
        assert_eq!(transitions[0].offset_to, 7200);
        assert!(transitions[0].is_dst);
        assert!(!transitions[1].is_dst);
    }

    #[test]
    fn spring_forward_lands_on_the_right_local_time() {
        let transitions = find_transitions(chrono_tz::Europe::Berlin, 2026, 2027);
        let spring = &transitions[0];
        // 01:00 UTC becomes 03:00 local when the clocks jump forward.
        assert_eq!(
            spring.local_start.format("%Y%m%dT%H%M%S").to_string(),
            "20260329T030000"
        );
    }

    #[test]
    fn zone_without_dst_still_emits_a_subcomponent() {
        let c = vtimezone(chrono_tz::Asia::Tokyo, 2026);
        assert_eq!(c.children.len(), 1);
        assert_eq!(c.children[0].name, "STANDARD");
        assert_eq!(
            c.children[0].prop_text("TZOFFSETTO").as_deref(),
            Some("+0900")
        );
    }

    #[test]
    fn utc_is_handled() {
        let c = vtimezone(chrono_tz::UTC, 2026);
        assert_eq!(c.prop_text("TZID").as_deref(), Some("UTC"));
        assert_eq!(
            c.children[0].prop_text("TZOFFSETFROM").as_deref(),
            Some("+0000")
        );
    }

    #[test]
    fn generated_component_is_well_formed() {
        let out = write(&vtimezone(chrono_tz::Europe::Berlin, 2026));
        assert!(out.starts_with("BEGIN:VTIMEZONE\r\n"));
        assert!(out.ends_with("END:VTIMEZONE\r\n"));
        assert!(out.contains("BEGIN:DAYLIGHT"));
        assert!(out.contains("BEGIN:STANDARD"));
    }

    #[test]
    fn formats_half_hour_offsets() {
        assert_eq!(format_offset(5 * 3600 + 30 * 60), "+0530");
        assert_eq!(format_offset(-(3 * 3600 + 30 * 60)), "-0330");
        assert_eq!(format_offset(0), "+0000");
    }
}
