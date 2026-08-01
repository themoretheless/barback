//! The EventKit side of the nearest-meeting feature.
//!
//! This is the only module that touches `objc2-event-kit`. Nothing of type
//! `EKEvent` escapes it: everything crosses into [`crate::meeting`] as plain
//! Rust, which is what keeps the overlap rules testable without a Mac.

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, Sel};
use objc2::msg_send_id;
use objc2_event_kit::{
    EKAuthorizationStatus, EKCalendar, EKCalendarType, EKEntityType, EKEvent, EKEventAvailability,
    EKEventStatus, EKEventStore, EKParticipant, EKParticipantStatus, EKParticipantType,
};
use objc2_foundation::{
    NSArray, NSCalendar, NSCalendarOptions, NSCalendarUnit, NSDate, NSError, NSPredicate, NSString,
};

use crate::meeting::{
    Access, Availability, EventStatus, Hm, Instant, Now, RawEvent, SelfStatus,
};

/// How far back to look, so a long meeting that started this morning is still
/// found. The forward edge is the end of tomorrow, computed on the local
/// calendar rather than by adding seconds.
const LOOKBACK: f64 = 12.0 * 3600.0;

pub fn new_store() -> Retained<EKEventStore> {
    unsafe { EKEventStore::new() }
}

pub fn access() -> Access {
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    match status {
        EKAuthorizationStatus::FullAccess => Access::FullAccess,
        EKAuthorizationStatus::WriteOnly => Access::WriteOnly,
        EKAuthorizationStatus::Denied => Access::Denied,
        EKAuthorizationStatus::Restricted => Access::Restricted,
        _ => Access::NotDetermined,
    }
}

/// Raises the system permission prompt and reports the answer back by messaging
/// `target` on the main thread.
///
/// The completion block is delivered on an EventKit queue, so it must not touch
/// AppKit or any main-thread-only object. It therefore carries nothing across
/// the boundary except the choice of selector, and `target` is a bare pointer:
/// `Retained<Controller>` is not `Send`, and releasing it on an EventKit thread
/// would be exactly the thing `MainThreadOnly` forbids. This is sound only
/// because the controller is leaked for the lifetime of the process.
pub fn request_access(
    store: &EKEventStore,
    target: *const NSObject,
    granted: Sel,
    denied: Sel,
) {
    let target = target as usize;
    let handler = RcBlock::new(move |ok: Bool, _err: *mut NSError| {
        let target = target as *const NSObject;
        if target.is_null() {
            return;
        }
        let sel = if ok.as_bool() { granted } else { denied };
        unsafe {
            use objc2_foundation::NSObjectNSThreadPerformAdditions;
            (*target).performSelectorOnMainThread_withObject_waitUntilDone(sel, None, false);
        }
    });

    let raw: *mut Block<dyn Fn(Bool, *mut NSError)> =
        &*handler as *const Block<dyn Fn(Bool, *mut NSError)> as *mut _;
    unsafe { store.requestFullAccessToEventsWithCompletion(raw) };

    // EventKit is expected to copy the block, but the binding takes a bare
    // pointer and nothing in the type system says so. Dropping the RcBlock here
    // would be a use-after-free if that expectation is ever wrong, so leak it.
    // The prompt can only be raised while the status is NotDetermined, so this
    // happens at most a handful of times per process.
    std::mem::forget(handler);
}

fn instant_of(date: &NSDate) -> Instant {
    let secs = unsafe { date.timeIntervalSince1970() };
    if secs.is_finite() {
        secs as Instant
    } else {
        0
    }
}

fn date_at(instant: Instant) -> Retained<NSDate> {
    unsafe { NSDate::dateWithTimeIntervalSince1970(instant as f64) }
}

fn start_of_day(cal: &NSCalendar, date: &NSDate) -> Retained<NSDate> {
    unsafe { cal.startOfDayForDate(date) }
}

fn add_days(cal: &NSCalendar, date: &NSDate, days: isize) -> Option<Retained<NSDate>> {
    unsafe {
        cal.dateByAddingUnit_value_toDate_options(
            NSCalendarUnit::Day,
            days,
            date,
            NSCalendarOptions::empty(),
        )
    }
}

/// "Now" plus the local day boundaries, derived from `NSCalendar` so that the
/// 23 and 25 hour days land in the right place.
pub fn now_snapshot() -> Now {
    let now_date = unsafe { NSDate::now() };
    let instant = instant_of(&now_date);
    let cal = unsafe { NSCalendar::currentCalendar() };

    let today = start_of_day(&cal, &now_date);
    let today_start = instant_of(&today);
    let tomorrow_start = add_days(&cal, &today, 1)
        .map(|d| instant_of(&d))
        .unwrap_or(today_start + 86_400);
    let day_after_start = add_days(&cal, &today, 2)
        .map(|d| instant_of(&d))
        .unwrap_or(today_start + 2 * 86_400);

    Now {
        instant,
        today_start,
        tomorrow_start,
        day_after_start,
    }
}

fn local_hm(cal: &NSCalendar, date: &NSDate) -> Hm {
    let comps =
        unsafe { cal.components_fromDate(NSCalendarUnit::Hour | NSCalendarUnit::Minute, date) };
    let h = unsafe { comps.hour() };
    let m = unsafe { comps.minute() };
    (h.clamp(0, 23) as u8, m.clamp(0, 59) as u8)
}

/// `EKEvent::startDate` and friends are typed non-optional by the bindings even
/// though Objective-C can hand back nil, and a non-optional `msg_send_id!` would
/// abort on that. Ask for an `Option` instead.
fn opt_date(event: &EKEvent, sel_start: bool) -> Option<Retained<NSDate>> {
    unsafe {
        if sel_start {
            msg_send_id![event, startDate]
        } else {
            msg_send_id![event, endDate]
        }
    }
}

fn opt_string(object: &AnyObject, selector: &str) -> Option<String> {
    let value: Option<Retained<NSString>> = unsafe {
        match selector {
            "title" => msg_send_id![object, title],
            "eventIdentifier" => msg_send_id![object, eventIdentifier],
            "calendarItemIdentifier" => msg_send_id![object, calendarItemIdentifier],
            "calendarItemExternalIdentifier" => msg_send_id![object, calendarItemExternalIdentifier],
            _ => None,
        }
    };
    value.map(|s| s.to_string())
}

fn status_of(event: &EKEvent) -> EventStatus {
    match unsafe { event.status() } {
        EKEventStatus::Confirmed => EventStatus::Confirmed,
        EKEventStatus::Tentative => EventStatus::Tentative,
        EKEventStatus::Canceled => EventStatus::Canceled,
        _ => EventStatus::Unknown,
    }
}

fn availability_of(event: &EKEvent) -> Availability {
    match unsafe { event.availability() } {
        EKEventAvailability::Busy => Availability::Busy,
        EKEventAvailability::Free => Availability::Free,
        EKEventAvailability::Tentative => Availability::Tentative,
        EKEventAvailability::Unavailable => Availability::Unavailable,
        _ => Availability::NotSupported,
    }
}

fn self_status_of(participant: &EKParticipant) -> SelfStatus {
    match unsafe { participant.participantStatus() } {
        EKParticipantStatus::Accepted => SelfStatus::Accepted,
        EKParticipantStatus::Declined => SelfStatus::Declined,
        EKParticipantStatus::Tentative => SelfStatus::Tentative,
        EKParticipantStatus::Delegated => SelfStatus::Delegated,
        _ => SelfStatus::NeedsAction,
    }
}

/// Lower is better. A meeting mirrored from a subscription loses to the real
/// account it came from, and birthdays are not meetings.
fn calendar_rank(kind: EKCalendarType) -> u32 {
    match kind {
        EKCalendarType::CalDAV | EKCalendarType::Exchange => 0,
        EKCalendarType::Local => 1,
        EKCalendarType::Subscription => 2,
        _ => 3,
    }
}

fn is_birthday_calendar(calendar: Option<&EKCalendar>) -> bool {
    calendar
        .map(|c| unsafe { c.r#type() } == EKCalendarType::Birthday)
        .unwrap_or(false)
}

fn attendee_summary(event: &EKEvent) -> (Option<SelfStatus>, usize) {
    let attendees: Option<Retained<NSArray<EKParticipant>>> = unsafe { event.attendees() };
    let Some(attendees) = attendees else {
        return (None, 0);
    };

    let mut mine = None;
    let mut others = 0usize;
    for participant in attendees.iter() {
        if unsafe { participant.isCurrentUser() } {
            mine = Some(self_status_of(&participant));
            continue;
        }
        // Rooms and projectors must not make a solo block look like a meeting.
        if unsafe { participant.participantType() } == EKParticipantType::Person {
            others += 1;
        }
    }
    (mine, others)
}

fn is_organizer(event: &EKEvent) -> bool {
    unsafe { event.organizer() }
        .map(|p| unsafe { p.isCurrentUser() })
        .unwrap_or(false)
}

fn convert(event: &EKEvent, cal: &NSCalendar) -> Option<RawEvent> {
    let start_date = opt_date(event, true)?;
    let end_date = opt_date(event, false)?;
    let start = instant_of(&start_date);
    let end = instant_of(&end_date);

    let calendar = unsafe { event.calendar() };
    if is_birthday_calendar(calendar.as_deref()) {
        return None;
    }

    let as_object: &AnyObject = event.as_ref();
    // eventIdentifier is shared by every occurrence of a recurring series, so it
    // is not a key on its own.
    let base_id = opt_string(as_object, "eventIdentifier")
        .or_else(|| opt_string(as_object, "calendarItemIdentifier"))
        .unwrap_or_else(|| format!("{start}"));

    let (self_status, attendee_count) = attendee_summary(event);

    Some(RawEvent {
        id: format!("{base_id}#{start}"),
        external_id: opt_string(as_object, "calendarItemExternalIdentifier"),
        title: opt_string(as_object, "title").unwrap_or_default(),
        calendar: calendar
            .as_deref()
            .map(|c| unsafe { c.title() }.to_string())
            .unwrap_or_default(),
        calendar_rank: calendar
            .as_deref()
            .map(|c| calendar_rank(unsafe { c.r#type() }))
            .unwrap_or(3),
        start,
        end,
        start_hm: local_hm(cal, &start_date),
        end_hm: local_hm(cal, &end_date),
        all_day: unsafe { event.isAllDay() },
        status: status_of(event),
        availability: availability_of(event),
        self_status,
        is_organizer: is_organizer(event),
        attendee_count,
    })
}

/// Reads every event intersecting `[now - 12h, end of tomorrow)`.
///
/// The predicate matches events that *overlap* the window rather than only those
/// starting inside it, which is what makes an already running meeting visible.
pub fn fetch(store: &EKEventStore, now: Now) -> Vec<RawEvent> {
    let from = date_at(now.instant - LOOKBACK as Instant);
    let to = date_at(now.day_after_start);

    let predicate: Retained<NSPredicate> =
        unsafe { store.predicateForEventsWithStartDate_endDate_calendars(&from, &to, None) };
    let events = unsafe { store.eventsMatchingPredicate(&predicate) };

    let cal = unsafe { NSCalendar::currentCalendar() };
    events
        .iter()
        .filter_map(|event| convert(&event, &cal))
        .collect()
}

/// EventKit hands out snapshots. After a change notification the cached objects
/// are stale and the whole query has to run again.
pub fn reset(store: &EKEventStore) {
    unsafe { store.reset() };
}
