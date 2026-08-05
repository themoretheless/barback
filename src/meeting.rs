//! Pure "which meeting is next" logic.
//!
//! Deliberately free of `objc2` and of anything macOS specific: `crate::calendar`
//! turns EventKit objects into [`RawEvent`] values, and every rule about
//! overlapping meetings lives here as plain Rust so it can be unit tested on any
//! host.
//!
//! Time is always absolute (`i64` seconds since the Unix epoch). Wall clock facts
//! that need a timezone database (start of the local day, the local hour and
//! minute of an event) are computed by the caller with `NSCalendar` and passed in.
//! That keeps this module DST safe without a date library: `t - t % 86400` is
//! simply wrong on the 23 and 25 hour days.

use std::cmp::Ordering;

/// Seconds since the Unix epoch, UTC.
pub type Instant = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventStatus {
    /// `EKEventStatus::None`: the backend does not report a status.
    Unknown,
    Confirmed,
    Tentative,
    Canceled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    NotSupported,
    Busy,
    Free,
    Tentative,
    Unavailable,
}

/// The current user's own answer to the invitation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfStatus {
    Accepted,
    NeedsAction,
    Tentative,
    Declined,
    Delegated,
}

/// Mirrors `EKAuthorizationStatus`, so the variants are named after it rather
/// than after what reads well in isolation.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    NotDetermined,
    Restricted,
    Denied,
    /// macOS 14+ can grant write-only access, which is useless for reading.
    WriteOnly,
    FullAccess,
}

/// Local hour and minute, precomputed by the caller.
pub type Hm = (u8, u8);

/// One event as read from the calendar store, with every Objective-C type already
/// converted away.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    /// Unique per *occurrence*. `EKEvent::eventIdentifier` is shared by every
    /// occurrence of a recurring series, so the bridge appends the start instant.
    pub id: String,
    /// iCalendar UID (`calendarItemExternalIdentifier`). Stable for the same
    /// invitation seen through two different accounts, which is what makes
    /// deduplication possible.
    pub external_id: Option<String>,
    pub title: String,
    pub calendar: String,
    pub calendar_rank: u32,
    pub start: Instant,
    pub end: Instant,
    pub start_hm: Hm,
    pub end_hm: Hm,
    pub all_day: bool,
    pub status: EventStatus,
    pub availability: Availability,
    /// `None` when the user has no attendee row at all, which is normal for
    /// events they created themselves.
    pub self_status: Option<SelfStatus>,
    pub is_organizer: bool,
    /// Human attendees other than the current user. Rooms and resources excluded.
    pub attendee_count: usize,
}

/// "Now", plus the local day boundaries the caller computed with `NSCalendar`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    pub instant: Instant,
    pub today_start: Instant,
    pub tomorrow_start: Instant,
    pub day_after_start: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// All-day events can never be the nearest meeting unless this is set.
    pub include_all_day: bool,
    /// Events the user marked as free time are noise by default.
    pub include_free: bool,
    /// Require at least one other human attendee.
    pub require_attendees: bool,
    /// An event starting within this many seconds outranks one already running.
    pub join_lead: i64,
    /// A meeting that started this recently still counts as "you are arriving".
    pub late_grace: i64,
    /// At or above this duration an event is context, not an appointment.
    pub long_block: i64,
    /// Floor for zero-length events so they do not blink out at their own start.
    pub min_visible_duration: i64,
    /// Start and end must agree within this many seconds to be the same meeting.
    pub dedup_tolerance: i64,
    /// Below this, show a countdown; above it, show a clock time.
    pub near_threshold: i64,
    pub status_title_max_chars: usize,
    pub menu_title_max_chars: usize,
    pub menu_max_rows: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            include_all_day: false,
            include_free: false,
            require_attendees: false,
            join_lead: 15 * 60,
            late_grace: 5 * 60,
            long_block: 3 * 60 * 60,
            min_visible_duration: 5 * 60,
            dedup_tolerance: 60,
            near_threshold: 60 * 60,
            status_title_max_chars: 20,
            menu_title_max_chars: 60,
            menu_max_rows: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Upcoming,
    Running,
}

/// The meeting the menu should lead with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub event_id: String,
    pub title: String,
    pub calendar: String,
    pub start: Instant,
    pub end: Instant,
    pub start_hm: Hm,
    pub end_hm: Hm,
    pub all_day: bool,
    pub phase: Phase,
    pub tier: u8,
    /// How many calendars this same meeting was found in.
    pub merged_count: usize,
    /// How many other eligible meetings overlap this one. This is the visible
    /// half of the overlap handling: the selection resolves the conflict, the
    /// count admits that there was one.
    pub conflicts: usize,
}

/// Everything the UI needs to know, permission states included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// The first query has not come back yet.
    Loading,
    /// Never asked. The prompt is only ever raised by an explicit click.
    NeedsPermission,
    Denied,
    Restricted,
    NoUpcoming,
    Meeting(Box<Selection>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    None,
    RequestAccess,
    OpenSettings,
}

/// The two or three lines at the top of the dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub primary: String,
    pub secondary: Option<String>,
    pub tertiary: Option<String>,
    pub tooltip: Option<String>,
    pub action: HeaderAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuRow {
    pub event_id: String,
    pub title: String,
    /// "10:00 - 10:30 - Work", already assembled.
    pub detail: String,
    pub tooltip: String,
    pub is_selected: bool,
    /// This row's interval intersects another row's.
    pub conflict: bool,
    /// Not accepted, or otherwise deprioritized.
    pub dimmed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuModel {
    pub state: State,
    pub header: Header,
    pub all_day: Vec<MenuRow>,
    pub today: Vec<MenuRow>,
    pub tomorrow: Vec<MenuRow>,
    /// Rows dropped because of `menu_max_rows`.
    /// Counted per section, so the note lands under the day it belongs to.
    pub today_truncated: usize,
    pub tomorrow_truncated: usize,
}

// ---------------------------------------------------------------------------
// Internal candidate
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    title: String,
    calendar: String,
    calendar_rank: u32,
    start: Instant,
    /// `max(end, start)`: providers do ship events that end before they start.
    end: Instant,
    /// `max(end, start + min_visible_duration)`. Drives the phase only, never the
    /// displayed times.
    eff_end: Instant,
    start_hm: Hm,
    end_hm: Hm,
    all_day: bool,
    availability: Availability,
    /// 0 accepted, 1 no answer yet, 2 tentative.
    participation: u8,
    /// The user's own attendee row, if there is one. Kept separate from
    /// `participation` because "no row at all" and "invited, not answered" rank
    /// the same but must not be labelled the same.
    self_status: Option<SelfStatus>,
    is_organizer: bool,
    /// Declined or delegated somewhere in the duplicate group.
    excluded: bool,
    attendee_count: usize,
    merged_count: usize,
}

impl Candidate {
    fn duration(&self) -> i64 {
        self.end.saturating_sub(self.start)
    }

    fn is_running(&self, now: Instant) -> bool {
        self.start <= now && now < self.eff_end
    }

    fn phase(&self, now: Instant) -> Phase {
        if self.is_running(now) {
            Phase::Running
        } else {
            Phase::Upcoming
        }
    }

    /// Compares the real interval, not the padded one: `eff_end` exists to keep
    /// a zero-length event visible, and using it here would invent conflicts
    /// between events that never actually intersect.
    fn overlaps(&self, other: &Candidate) -> bool {
        self.start < other.end && other.start < self.end
    }
}

fn participation_rank(ev: &RawEvent) -> (u8, bool) {
    match ev.self_status {
        Some(SelfStatus::Declined) => (3, true),
        Some(SelfStatus::Delegated) => (3, true),
        Some(SelfStatus::Accepted) => (0, false),
        Some(SelfStatus::Tentative) => (2, false),
        // No attendee row at all is the common case for self-created events.
        // Treating it as a decline would silently hide the user's own calendar.
        Some(SelfStatus::NeedsAction) | None => {
            if ev.is_organizer {
                (0, false)
            } else {
                (1, false)
            }
        }
    }
}

fn to_candidate(ev: &RawEvent, cfg: &Config) -> Candidate {
    let end = ev.end.max(ev.start);
    let eff_end = end.max(ev.start.saturating_add(cfg.min_visible_duration));
    let (participation, excluded) = participation_rank(ev);
    Candidate {
        id: ev.id.clone(),
        title: ev.title.clone(),
        calendar: ev.calendar.clone(),
        calendar_rank: ev.calendar_rank,
        start: ev.start,
        end,
        eff_end,
        start_hm: ev.start_hm,
        end_hm: ev.end_hm,
        all_day: ev.all_day,
        availability: ev.availability,
        participation,
        self_status: ev.self_status,
        is_organizer: ev.is_organizer,
        excluded,
        attendee_count: ev.attendee_count,
        merged_count: 1,
    }
}

/// Lowercased, whitespace-collapsed, reply-prefix-stripped title used only for
/// matching duplicates that carry no shared UID.
fn normalized_title(title: &str) -> String {
    let mut s: String = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    const PREFIXES: [&str; 10] = [
        "re:",
        "fwd:",
        "fw:",
        "accepted:",
        "declined:",
        "tentative:",
        "canceled:",
        "cancelled:",
        "updated:",
        "invitation:",
    ];
    loop {
        let trimmed = s.trim_start();
        let hit = PREFIXES.iter().find(|p| trimmed.starts_with(**p));
        match hit {
            Some(p) => s = trimmed[p.len()..].trim_start().to_string(),
            None => {
                s = trimmed.to_string();
                break;
            }
        }
    }
    s
}

/// Groups duplicates of the same meeting mirrored across accounts and collapses
/// each group into one candidate.
///
/// `externals` runs parallel to `cands`: the UID is only needed while grouping,
/// so it never becomes part of a candidate.
fn merge_duplicates(
    cands: Vec<Candidate>,
    externals: Vec<Option<String>>,
    cfg: &Config,
) -> Vec<Candidate> {
    // Grouping is order sensitive when a UID-less event could pair with more
    // than one neighbour, so fix the order before touching anything.
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by(|&a, &b| (cands[a].start, &cands[a].id).cmp(&(cands[b].start, &cands[b].id)));
    let externals: Vec<Option<String>> = order.iter().map(|&i| externals[i].clone()).collect();
    let cands: Vec<Candidate> = order.iter().map(|&i| cands[i].clone()).collect();

    let n = cands.len();
    let norm: Vec<String> = cands.iter().map(|c| normalized_title(&c.title)).collect();

    // Union-find over the (small) candidate list.
    let mut parent: Vec<usize> = (0..n).collect();
    // One representative UID per group, so a union can never bridge two groups
    // that carry different UIDs.
    let mut root_uid: Vec<Option<String>> = externals.clone();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let tol = cfg.dedup_tolerance;
            let times_agree = (cands[i].start - cands[j].start).abs() <= tol
                && (cands[i].end - cands[j].end).abs() <= tol;
            if !times_agree {
                continue;
            }
            let same = match (&externals[i], &externals[j]) {
                (Some(x), Some(y)) => x == y,
                // Fuzzy match only when at least one side has no UID.
                _ => !norm[i].is_empty() && norm[i] == norm[j],
            };
            if !same {
                continue;
            }
            let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
            if ri == rj {
                continue;
            }
            // Without this, one UID-less event sitting between two different
            // meetings with the same title merges all three transitively and a
            // real meeting vanishes from the menu.
            if let (Some(x), Some(y)) = (&root_uid[ri], &root_uid[rj])
                && x != y
            {
                continue;
            }
            let uid = root_uid[ri].take().or_else(|| root_uid[rj].take());
            parent[ri] = rj;
            root_uid[rj] = uid;
        }
    }

    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        let r = find(&mut parent, i);
        groups[r].push(i);
    }

    let mut out = Vec::new();
    for group in groups.into_iter().filter(|g| !g.is_empty()) {
        if group.len() == 1 {
            out.push(cands[group[0]].clone());
            continue;
        }

        // Winner: lowest calendar rank, then lowest id, but always prefer a
        // member that carries real times over an all-day mirror.
        let mut order: Vec<usize> = group.clone();
        order.sort_by(|&a, &b| {
            (cands[a].all_day, cands[a].calendar_rank, &cands[a].id).cmp(&(
                cands[b].all_day,
                cands[b].calendar_rank,
                &cands[b].id,
            ))
        });
        let winner = order[0];

        let excluded = group.iter().any(|&i| cands[i].excluded);
        // Take the answer from whichever copy the user actually replied to, so
        // the rank and the label it produces agree.
        let answered = *group
            .iter()
            .min_by_key(|&&i| (cands[i].participation, &cands[i].id))
            .unwrap_or(&winner);
        let attendee_count = group
            .iter()
            .map(|&i| cands[i].attendee_count)
            .max()
            .unwrap_or(0);
        // Subscribed mirrors routinely flatten everything to Free.
        let availability = if group
            .iter()
            .any(|&i| cands[i].availability == Availability::Busy)
        {
            Availability::Busy
        } else {
            cands[winner].availability
        };

        let mut merged = cands[winner].clone();
        merged.excluded = excluded;
        merged.participation = cands[answered].participation;
        merged.self_status = cands[answered].self_status;
        merged.is_organizer = cands[answered].is_organizer;
        merged.attendee_count = attendee_count;
        merged.availability = availability;
        merged.merged_count = group.len();
        out.push(merged);
    }

    out.sort_by(|a, b| (a.start, &a.id).cmp(&(b.start, &b.id)));
    out
}

fn is_eligible(c: &Candidate, cfg: &Config) -> bool {
    if c.excluded {
        return false;
    }
    if c.all_day && !cfg.include_all_day {
        return false;
    }
    match c.availability {
        Availability::Free if !cfg.include_free => return false,
        Availability::Unavailable => return false,
        _ => {}
    }
    if cfg.require_attendees && c.attendee_count == 0 {
        return false;
    }
    true
}

/// Filters, deduplicates and normalizes. The result is sorted by start time and
/// is independent of the input order.
fn prepare(events: &[RawEvent], now: Now, cfg: &Config) -> Vec<Candidate> {
    let mut cands = Vec::with_capacity(events.len());
    let mut externals = Vec::with_capacity(events.len());

    for ev in events {
        if ev.status == EventStatus::Canceled {
            continue;
        }
        let c = to_candidate(ev, cfg);
        // A finished meeting is dropped the instant it ends: it is pure noise and
        // it competes with the next one for a single line of text.
        if now.instant >= c.eff_end {
            continue;
        }
        // Nothing past the end of tomorrow can be "nearest" in a useful sense.
        if c.start >= now.day_after_start {
            continue;
        }
        cands.push(c);
        externals.push(ev.external_id.clone());
    }

    merge_duplicates(cands, externals, cfg)
}

fn tier(c: &Candidate, now: Instant, cfg: &Config) -> u8 {
    if c.all_day {
        return 5;
    }
    let running = c.is_running(now);
    let is_block = c.duration() >= cfg.long_block;

    if running && now.saturating_sub(c.start) <= cfg.late_grace && !is_block {
        // You just walked in, or you are late to it.
        0
    } else if !running && c.start.saturating_sub(now) <= cfg.join_lead {
        // The next transition is the only actionable information.
        1
    } else if running && !is_block {
        2
    } else if !running {
        3
    } else {
        // A day-long block is context, not an appointment. It must not squat in
        // the menu while three real meetings pass by.
        4
    }
}

fn rank_key(c: &Candidate, now: Instant, cfg: &Config) -> (u8, Instant, u8, bool, i64, u32) {
    let t = tier(c, now, cfg);
    let sort_instant = if c.is_running(now) {
        c.eff_end
    } else {
        c.start
    };
    (
        t,
        sort_instant,
        c.participation,
        // A real meeting beats a solo block at an exact tie.
        c.attendee_count == 0,
        // The shorter one is the tighter constraint: you can leave the workshop
        // for the standup, not the reverse.
        c.duration(),
        c.calendar_rank,
    )
}

fn cmp_candidates(a: &Candidate, b: &Candidate, now: Instant, cfg: &Config) -> Ordering {
    rank_key(a, now, cfg)
        .cmp(&rank_key(b, now, cfg))
        .then_with(|| a.id.cmp(&b.id))
}

fn to_selection(c: &Candidate, others: &[Candidate], now: Instant) -> Selection {
    let conflicts = others
        .iter()
        .filter(|o| o.id != c.id && !o.all_day && !c.all_day && c.overlaps(o))
        .count();
    Selection {
        event_id: c.id.clone(),
        title: c.title.clone(),
        calendar: c.calendar.clone(),
        start: c.start,
        end: c.end,
        start_hm: c.start_hm,
        end_hm: c.end_hm,
        all_day: c.all_day,
        phase: c.phase(now),
        tier: 0,
        merged_count: c.merged_count,
        conflicts,
    }
}

/// The nearest meeting, or `None` when nothing qualifies.
///
/// Pure and independent of the order of `events`. The app goes through
/// [`build_menu`], which needs the prepared list anyway; this is the narrow
/// entry point the selection rules are tested through.
#[cfg_attr(not(test), allow(dead_code))]
pub fn select_nearest(events: &[RawEvent], now: Now, cfg: &Config) -> Option<Selection> {
    let cands = prepare(events, now, cfg);
    select_from(&cands, now, cfg)
}

fn select_from(cands: &[Candidate], now: Now, cfg: &Config) -> Option<Selection> {
    let eligible: Vec<&Candidate> = cands.iter().filter(|c| is_eligible(c, cfg)).collect();
    let winner = eligible
        .iter()
        .copied()
        .min_by(|a, b| cmp_candidates(a, b, now.instant, cfg))?;

    let eligible_owned: Vec<Candidate> = eligible.iter().map(|c| (*c).clone()).collect();
    let mut sel = to_selection(winner, &eligible_owned, now.instant);
    sel.tier = tier(winner, now.instant, cfg);
    Some(sel)
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Round up, so a countdown never overstates how much time is left.
fn ceil_min(seconds: i64) -> i64 {
    (seconds + 59) / 60
}

fn hm(t: Hm) -> String {
    format!("{:02}:{:02}", t.0, t.1)
}

/// "now", "in 12 min", "14:00" or "tmr 09:00".
pub fn countdown(sel: &Selection, now: Now, cfg: &Config) -> String {
    if sel.all_day {
        return "all day".to_string();
    }
    if sel.phase == Phase::Running {
        return "now".to_string();
    }
    let d = sel.start.saturating_sub(now.instant);
    if d <= cfg.near_threshold {
        format!("in {} min", ceil_min(d))
    } else if sel.start < now.tomorrow_start {
        hm(sel.start_hm)
    } else {
        format!("tmr {}", hm(sel.start_hm))
    }
}

pub fn time_range(start_hm: Hm, end_hm: Hm, all_day: bool) -> String {
    if all_day {
        "all day".to_string()
    } else {
        format!("{} - {}", hm(start_hm), hm(end_hm))
    }
}

/// Truncates on `char` boundaries, backing off to the last word break near the
/// cut so that "Sprint Planning" becomes "Sprint..." rather than "Sprint Plann...".
pub fn truncate(text: &str, max: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if max == 0 {
        return String::new();
    }
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let keep: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    let cut = match keep.rfind(' ') {
        Some(i)
            if keep
                .chars()
                .count()
                .saturating_sub(keep[..i].chars().count())
                <= 6 =>
        {
            &keep[..i]
        }
        _ => &keep[..],
    };
    let mut out = cut.trim_end().to_string();
    out.push('\u{2026}');
    out
}

fn display_title(title: &str, max: usize) -> String {
    let t = title.trim();
    if t.is_empty() {
        "(no title)".to_string()
    } else {
        truncate(t, max)
    }
}

// ---------------------------------------------------------------------------
// State and menu model
// ---------------------------------------------------------------------------

fn resolve_state(
    cands: &[Candidate],
    now: Now,
    access: Access,
    loading: bool,
    cfg: &Config,
) -> State {
    // Permission always wins over any cached selection, so revoking access while
    // the app runs cannot leave a stale meeting on screen.
    match access {
        Access::NotDetermined => return State::NeedsPermission,
        Access::Denied | Access::WriteOnly => return State::Denied,
        Access::Restricted => return State::Restricted,
        Access::FullAccess => {}
    }
    if loading {
        return State::Loading;
    }
    match select_from(cands, now, cfg) {
        Some(sel) => State::Meeting(Box::new(sel)),
        None => State::NoUpcoming,
    }
}

/// Short string for a menu bar label. Not used by the dropdown, but it is the
/// same numbers, so it lives with them.
pub fn status_title(state: &State, now: Now, cfg: &Config) -> String {
    match state {
        State::Loading => "\u{2026}".to_string(),
        State::NeedsPermission | State::Denied | State::Restricted => "Cal !".to_string(),
        // Never empty: the button sizes to its content, so an empty title leaves
        // a blank sliver in the menu bar and the toggle looks like it did nothing.
        State::NoUpcoming => "No meetings".to_string(),
        State::Meeting(sel) => {
            let short = match countdown(sel, now, cfg).as_str() {
                "now" => "now".to_string(),
                other => other.replace("in ", "").replace(" min", "m"),
            };
            format!(
                "{} \u{00b7} {}",
                short,
                display_title(&sel.title, cfg.status_title_max_chars)
            )
        }
    }
}

fn header_for(state: &State, now: Now, cfg: &Config) -> Header {
    match state {
        State::Loading => Header {
            primary: "Loading calendars\u{2026}".to_string(),
            secondary: None,
            tertiary: None,
            tooltip: None,
            action: HeaderAction::None,
        },
        State::NeedsPermission => Header {
            primary: "Calendar access needed".to_string(),
            secondary: Some("Grant Calendar Access\u{2026}".to_string()),
            tertiary: None,
            tooltip: Some("Barback needs read access to show your next meeting.".to_string()),
            action: HeaderAction::RequestAccess,
        },
        State::Denied => Header {
            primary: "Calendar access denied".to_string(),
            secondary: Some("Open Privacy Settings\u{2026}".to_string()),
            tertiary: None,
            tooltip: Some("Enable Barback under Privacy & Security > Calendars.".to_string()),
            action: HeaderAction::OpenSettings,
        },
        State::Restricted => Header {
            primary: "Calendar access restricted".to_string(),
            secondary: Some("Blocked by a device policy".to_string()),
            tertiary: None,
            tooltip: None,
            action: HeaderAction::None,
        },
        State::NoUpcoming => Header {
            primary: "No upcoming meetings".to_string(),
            secondary: None,
            tertiary: None,
            tooltip: Some("Nothing scheduled between now and the end of tomorrow.".to_string()),
            action: HeaderAction::None,
        },
        State::Meeting(sel) => {
            let mut secondary = format!(
                "{} \u{00b7} {}",
                countdown(sel, now, cfg),
                time_range(sel.start_hm, sel.end_hm, sel.all_day)
            );
            if sel.conflicts > 0 {
                secondary.push_str(&format!(" \u{00b7} {} overlapping", sel.conflicts));
            }
            let mut tertiary = sel.calendar.clone();
            if sel.merged_count > 1 {
                tertiary.push_str(&format!(" (+{} more)", sel.merged_count - 1));
            }
            Header {
                primary: display_title(&sel.title, cfg.menu_title_max_chars),
                secondary: Some(secondary),
                tertiary: if tertiary.is_empty() {
                    None
                } else {
                    Some(tertiary)
                },
                tooltip: Some(sel.title.clone()),
                action: HeaderAction::None,
            }
        }
    }
}

fn row_for(c: &Candidate, selected_id: Option<&str>, all: &[&Candidate], cfg: &Config) -> MenuRow {
    let conflict = !c.all_day
        && all
            .iter()
            .any(|o| o.id != c.id && !o.all_day && c.overlaps(o));
    let mut detail = time_range(c.start_hm, c.end_hm, c.all_day);
    if !c.calendar.is_empty() {
        detail.push_str(&format!(" \u{00b7} {}", c.calendar));
    }
    if conflict {
        detail.push_str(" \u{00b7} overlaps");
    }
    MenuRow {
        event_id: c.id.clone(),
        title: display_title(&c.title, cfg.menu_title_max_chars),
        detail,
        tooltip: c.title.clone(),
        is_selected: selected_id == Some(c.id.as_str()),
        conflict,
        // Only an actual unanswered invitation is "not accepted". An event you
        // created for yourself has no attendee row at all and ranks the same,
        // but calling it not accepted would be nonsense. Some servers list the
        // organizer's own row as pending, hence the second guard.
        dimmed: !c.is_organizer
            && matches!(
                c.self_status,
                Some(SelfStatus::NeedsAction) | Some(SelfStatus::Tentative)
            ),
    }
}

/// Everything the dropdown needs, in one pass.
pub fn build_menu(
    events: &[RawEvent],
    now: Now,
    access: Access,
    loading: bool,
    cfg: &Config,
) -> MenuModel {
    let cands = prepare(events, now, cfg);
    let state = resolve_state(&cands, now, access, loading, cfg);
    let header = header_for(&state, now, cfg);

    let selected_id = match &state {
        State::Meeting(sel) => Some(sel.event_id.clone()),
        _ => None,
    };

    let show_events = matches!(state, State::Meeting(_) | State::NoUpcoming);
    if !show_events {
        return MenuModel {
            state,
            header,
            all_day: Vec::new(),
            today: Vec::new(),
            tomorrow: Vec::new(),
            today_truncated: 0,
            tomorrow_truncated: 0,
        };
    }

    // The dropdown shows everything that survived the hard filters, all-day
    // events included: the selection hides them, the list must not.
    let visible: Vec<&Candidate> = cands
        .iter()
        .filter(|c| {
            !c.excluded
                && (c.availability != Availability::Free || cfg.include_free)
                && c.availability != Availability::Unavailable
        })
        .collect();

    let timed: Vec<&Candidate> = visible.iter().copied().filter(|c| !c.all_day).collect();

    let mut all_day = Vec::new();
    let mut today = Vec::new();
    let mut tomorrow = Vec::new();
    let mut today_truncated = 0usize;
    let mut tomorrow_truncated = 0usize;

    for c in &visible {
        let row = row_for(c, selected_id.as_deref(), &timed, cfg);
        if c.all_day {
            all_day.push(row);
        } else if c.start < now.tomorrow_start {
            if today.len() < cfg.menu_max_rows {
                today.push(row);
            } else {
                today_truncated += 1;
            }
        } else if tomorrow.len() < cfg.menu_max_rows {
            tomorrow.push(row);
        } else {
            tomorrow_truncated += 1;
        }
    }

    MenuModel {
        state,
        header,
        all_day,
        today,
        tomorrow,
        today_truncated,
        tomorrow_truncated,
    }
}

/// The next instant at which any rendered string would change: a phase flip, a
/// tier flip, a countdown minute, or a meeting disappearing.
///
/// Clamped to five minutes so the caller always has a heartbeat, and `None` when
/// there is nothing to watch at all.
pub fn next_change_at(events: &[RawEvent], now: Now, cfg: &Config) -> Option<Instant> {
    let cands = prepare(events, now, cfg);
    let eligible: Vec<&Candidate> = cands.iter().filter(|c| is_eligible(c, cfg)).collect();
    if eligible.is_empty() {
        return None;
    }

    let mut marks: Vec<Instant> = Vec::new();
    for c in &eligible {
        marks.push(c.start);
        marks.push(c.eff_end);
        marks.push(c.start.saturating_sub(cfg.join_lead));
        marks.push(c.start.saturating_add(cfg.late_grace));
        marks.push(c.start.saturating_sub(cfg.near_threshold));
    }
    marks.push(now.tomorrow_start);
    marks.push(now.day_after_start);

    // The countdown of the currently selected meeting ticks on its own schedule.
    if let Some(sel) = select_from(&cands, now, cfg)
        && sel.phase == Phase::Upcoming
    {
        let d = sel.start.saturating_sub(now.instant);
        if d <= cfg.near_threshold {
            let m = ceil_min(d);
            marks.push(sel.start.saturating_sub(60 * (m - 1).max(0)));
        }
    }

    let next = marks
        .into_iter()
        .filter(|t| *t > now.instant)
        .min()
        .unwrap_or(Instant::MAX);

    Some(next.min(now.instant.saturating_add(300)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3600;
    const M: i64 = 60;

    /// Day D starts at epoch 0 in these tests. The pure layer never converts an
    /// instant to a wall clock, so this is only a readability choice.
    fn now_at(secs: i64) -> Now {
        Now {
            instant: secs,
            today_start: 0,
            tomorrow_start: 24 * H,
            day_after_start: 48 * H,
        }
    }

    fn ev(id: &str, start: i64, end: i64) -> RawEvent {
        RawEvent {
            id: id.to_string(),
            external_id: None,
            title: id.to_string(),
            calendar: "Work".to_string(),
            calendar_rank: 0,
            start,
            end,
            start_hm: (
                ((start.rem_euclid(24 * H)) / H) as u8,
                ((start.rem_euclid(H)) / M) as u8,
            ),
            end_hm: (
                ((end.rem_euclid(24 * H)) / H) as u8,
                ((end.rem_euclid(H)) / M) as u8,
            ),
            all_day: false,
            status: EventStatus::Confirmed,
            availability: Availability::Busy,
            self_status: Some(SelfStatus::Accepted),
            is_organizer: false,
            attendee_count: 4,
        }
    }

    fn pick(events: &[RawEvent], now: i64) -> Option<String> {
        select_nearest(events, now_at(now), &Config::default()).map(|s| s.event_id)
    }

    fn label(events: &[RawEvent], now: i64) -> String {
        let cfg = Config::default();
        let n = now_at(now);
        match select_nearest(events, n, &cfg) {
            Some(sel) => countdown(&sel, n, &cfg),
            None => "-".to_string(),
        }
    }

    #[test]
    fn empty_calendar_selects_nothing() {
        assert_eq!(pick(&[], 10 * H), None);
    }

    #[test]
    fn far_event_shows_clock_time_and_near_event_counts_down() {
        let far = [ev("A", 14 * H, 15 * H)];
        assert_eq!(label(&far, 10 * H), "14:00");
        let near = [ev("A", 10 * H + 30 * M, 11 * H)];
        assert_eq!(label(&near, 10 * H), "in 30 min");
    }

    #[test]
    fn countdown_rounds_up_and_never_reaches_zero() {
        let e = |s: i64| [ev("A", s, s + H)];
        assert_eq!(label(&e(10 * H + 90), 10 * H), "in 2 min");
        assert_eq!(label(&e(10 * H + 60), 10 * H), "in 1 min");
        assert_eq!(label(&e(10 * H + 61), 10 * H), "in 2 min");
        assert_eq!(label(&e(10 * H + 1), 10 * H), "in 1 min");
    }

    #[test]
    fn near_threshold_boundary_switches_to_clock_time() {
        assert_eq!(label(&[ev("A", 11 * H, 12 * H)], 10 * H), "in 60 min");
        assert_eq!(label(&[ev("A", 11 * H + 1, 12 * H)], 10 * H), "11:00");
    }

    #[test]
    fn running_meeting_reads_now() {
        assert_eq!(label(&[ev("A", 10 * H, 11 * H)], 10 * H), "now");
        assert_eq!(label(&[ev("A", 10 * H, 11 * H)], 10 * H + 30 * M), "now");
    }

    #[test]
    fn back_to_back_hands_over_before_the_boundary_and_flips_at_it() {
        let events = [ev("A", 10 * H, 11 * H), ev("B", 11 * H, 12 * H)];
        // Outside the join window the meeting you are sitting in still wins.
        assert_eq!(pick(&events, 10 * H + 40 * M).as_deref(), Some("A"));
        // Inside it the next transition is the only actionable information.
        assert_eq!(pick(&events, 11 * H - 1).as_deref(), Some("B"));
        // At the exact instant A is over and B is running: half-open intervals
        // mean there is no moment where both or neither are selected.
        assert_eq!(pick(&events, 11 * H).as_deref(), Some("B"));
        assert_eq!(label(&events, 11 * H), "now");
    }

    #[test]
    fn just_started_beats_imminent_but_settled_does_not() {
        // Two minutes into A, you are still arriving: A wins.
        let events = [
            ev("A", 10 * H, 11 * H),
            ev("B", 10 * H + 3 * M, 10 * H + 33 * M),
        ];
        assert_eq!(pick(&events, 10 * H + 2 * M).as_deref(), Some("A"));

        // Ten minutes in, the only useful information is the next transition.
        let events = [
            ev("A", 10 * H, 11 * H),
            ev("B", 10 * H + 15 * M, 10 * H + 45 * M),
        ];
        assert_eq!(pick(&events, 10 * H + 10 * M).as_deref(), Some("B"));
    }

    #[test]
    fn day_long_block_never_hides_a_real_meeting() {
        let events = [
            ev("Offsite", 9 * H, 17 * H),
            ev("Standup", 10 * H, 10 * H + 15 * M),
        ];
        assert_eq!(pick(&events, 9 * H + 58 * M).as_deref(), Some("Standup"));
        assert_eq!(pick(&events, 10 * H + 7 * M).as_deref(), Some("Standup"));
        // With nothing else in the day the block is still better than nothing.
        assert_eq!(pick(&events[..1], 12 * H).as_deref(), Some("Offsite"));
    }

    #[test]
    fn double_booking_prefers_the_shorter_meeting() {
        let events = [
            ev("Short", 10 * H, 10 * H + 30 * M),
            ev("Long", 10 * H, 11 * H),
        ];
        assert_eq!(pick(&events, 9 * H + 50 * M).as_deref(), Some("Short"));
    }

    #[test]
    fn double_booking_prefers_an_accepted_invitation() {
        let mut tentative = ev("Tentative", 10 * H, 11 * H);
        tentative.self_status = Some(SelfStatus::Tentative);
        let accepted = ev("Accepted", 10 * H, 11 * H);
        let events = [tentative, accepted];
        assert_eq!(pick(&events, 9 * H + 50 * M).as_deref(), Some("Accepted"));
    }

    #[test]
    fn declined_and_delegated_and_cancelled_are_dropped() {
        for status in [SelfStatus::Declined, SelfStatus::Delegated] {
            let mut a = ev("A", 10 * H, 10 * H + 30 * M);
            a.self_status = Some(status);
            let events = [a, ev("B", 12 * H, 13 * H)];
            assert_eq!(pick(&events, 9 * H + 50 * M).as_deref(), Some("B"));
        }
        let mut a = ev("A", 10 * H, 10 * H + 30 * M);
        a.status = EventStatus::Canceled;
        let events = [a, ev("B", 12 * H, 13 * H)];
        assert_eq!(pick(&events, 9 * H + 50 * M).as_deref(), Some("B"));
    }

    #[test]
    fn free_and_unavailable_time_is_not_a_meeting() {
        let mut free = ev("Free", 10 * H, 11 * H);
        free.availability = Availability::Free;
        let events = [free.clone(), ev("B", 13 * H, 14 * H)];
        assert_eq!(pick(&events, 9 * H).as_deref(), Some("B"));

        let cfg = Config {
            include_free: true,
            ..Config::default()
        };
        assert_eq!(
            select_nearest(&events, now_at(9 * H), &cfg).map(|s| s.event_id),
            Some("Free".to_string())
        );

        let mut ooo = ev("OOO", 10 * H, 11 * H);
        ooo.availability = Availability::Unavailable;
        let events = [ooo, ev("B", 13 * H, 14 * H)];
        assert_eq!(pick(&events, 9 * H).as_deref(), Some("B"));
    }

    #[test]
    fn unsupported_availability_is_not_a_filter() {
        let mut a = ev("A", 9 * H, 18 * H);
        a.availability = Availability::NotSupported;
        assert_eq!(pick(&[a], 9 * H).as_deref(), Some("A"));
    }

    #[test]
    fn all_day_events_never_win_but_stay_in_the_menu() {
        let mut offsite = ev("Offsite", 0, 24 * H);
        offsite.all_day = true;
        let events = [offsite.clone(), ev("B", 15 * H, 16 * H)];
        assert_eq!(pick(&events, 9 * H).as_deref(), Some("B"));
        assert_eq!(pick(&[offsite.clone()], 9 * H), None);

        let menu = build_menu(
            &events,
            now_at(9 * H),
            Access::FullAccess,
            false,
            &Config::default(),
        );
        assert_eq!(menu.all_day.len(), 1);
        assert_eq!(menu.today.len(), 1);

        let cfg = Config {
            include_all_day: true,
            ..Config::default()
        };
        assert_eq!(
            select_nearest(&[offsite], now_at(9 * H), &cfg).map(|s| s.event_id),
            Some("Offsite".to_string())
        );
    }

    #[test]
    fn zero_duration_events_stay_visible_for_five_minutes() {
        let e = [ev("A", 14 * H, 14 * H)];
        assert_eq!(pick(&e, 14 * H).as_deref(), Some("A"));
        assert_eq!(pick(&e, 14 * H + 5 * M - 1).as_deref(), Some("A"));
        assert_eq!(pick(&e, 14 * H + 5 * M), None);
    }

    #[test]
    fn malformed_end_before_start_does_not_panic_or_go_negative() {
        let e = [ev("A", 14 * H, 13 * H)];
        assert_eq!(pick(&e, 14 * H).as_deref(), Some("A"));
        assert_eq!(pick(&e, 14 * H + 5 * M), None);
    }

    #[test]
    fn extreme_instants_do_not_panic() {
        let mut a = ev("A", i64::MIN, i64::MAX);
        a.start_hm = (0, 0);
        a.end_hm = (0, 0);
        let mut b = ev("B", i64::MAX, i64::MIN);
        b.start_hm = (0, 0);
        b.end_hm = (0, 0);
        let _ = select_nearest(&[a, b], now_at(10 * H), &Config::default());
    }

    #[test]
    fn the_same_invitation_in_two_accounts_is_one_meeting() {
        let mut a = ev("A", 10 * H, 10 * H + 30 * M);
        a.external_id = Some("uid-1".to_string());
        let mut b = ev("B", 10 * H, 10 * H + 30 * M);
        b.external_id = Some("uid-1".to_string());
        b.calendar_rank = 1;

        let sel = select_nearest(&[a, b], now_at(9 * H + 50 * M), &Config::default()).unwrap();
        assert_eq!(sel.event_id, "A");
        assert_eq!(sel.merged_count, 2);
        assert_eq!(
            sel.conflicts, 0,
            "a merged duplicate must not conflict with itself"
        );
    }

    #[test]
    fn titles_match_fuzzily_only_when_a_uid_is_missing() {
        let mut a = ev("A", 10 * H, 10 * H + 30 * M);
        a.title = "Team Sync".to_string();
        let mut b = ev("B", 10 * H + 45, 10 * H + 30 * M + 45);
        b.title = "  team   SYNC ".to_string();
        let sel =
            select_nearest(&[a.clone(), b], now_at(9 * H + 50 * M), &Config::default()).unwrap();
        assert_eq!(sel.merged_count, 2);

        // 61 seconds apart is outside the tolerance, so these stay separate.
        let mut c = ev("C", 10 * H + 61, 10 * H + 30 * M + 61);
        c.title = "Team Sync".to_string();
        let sel = select_nearest(&[a, c], now_at(9 * H + 50 * M), &Config::default()).unwrap();
        assert_eq!(sel.merged_count, 1);
    }

    #[test]
    fn recurring_occurrences_share_a_uid_but_are_not_duplicates() {
        let mut a = ev("A", 10 * H, 10 * H + 30 * M);
        a.external_id = Some("uid-3".to_string());
        let mut b = ev("B", 11 * H, 11 * H + 30 * M);
        b.external_id = Some("uid-3".to_string());
        let sel = select_nearest(&[a, b], now_at(9 * H + 50 * M), &Config::default()).unwrap();
        assert_eq!(sel.event_id, "A");
        assert_eq!(sel.merged_count, 1);
    }

    #[test]
    fn a_uid_less_event_cannot_bridge_two_different_meetings() {
        // Two teams hold their own standup a minute apart, and a third,
        // UID-less copy sits between them. Merging transitively would make one
        // of the real standups disappear from the menu entirely.
        let mut loose = ev("A-loose", 10 * H, 10 * H + 15 * M);
        loose.title = "Standup".to_string();
        let mut team_a = ev("B-team-a", 10 * H + 45, 10 * H + 15 * M + 45);
        team_a.title = "Standup".to_string();
        team_a.external_id = Some("uid-team-a".to_string());
        let mut team_b = ev("C-team-b", 10 * H + 60, 10 * H + 15 * M + 60);
        team_b.title = "Standup".to_string();
        team_b.external_id = Some("uid-team-b".to_string());

        let mut events = vec![loose, team_a, team_b];
        let n = now_at(9 * H + 50 * M);
        let cfg = Config::default();
        for _ in 0..3 {
            let menu = build_menu(&events, n, Access::FullAccess, false, &cfg);
            assert_eq!(
                menu.today.len(),
                2,
                "neither team's standup may vanish, rows: {:?}",
                menu.today.iter().map(|r| &r.title).collect::<Vec<_>>()
            );
            events.rotate_left(1);
        }
    }

    #[test]
    fn an_event_you_made_for_yourself_is_not_marked_unanswered() {
        let mut lunch = ev("Lunch", 13 * H, 14 * H);
        lunch.self_status = None;
        lunch.attendee_count = 0;
        let mut invite = ev("Review", 15 * H, 16 * H);
        invite.self_status = Some(SelfStatus::NeedsAction);

        let menu = build_menu(
            &[lunch, invite],
            now_at(12 * H),
            Access::FullAccess,
            false,
            &Config::default(),
        );
        let dimmed: Vec<(&str, bool)> = menu
            .today
            .iter()
            .map(|r| (r.title.as_str(), r.dimmed))
            .collect();
        assert_eq!(dimmed, vec![("Lunch", false), ("Review", true)]);
    }

    #[test]
    fn a_padded_zero_length_event_does_not_invent_a_conflict() {
        // The five minute floor keeps a zero length event visible; it must not
        // make it overlap the meeting that starts right after it.
        let events = [
            ev("Ping", 10 * H, 10 * H),
            ev("Call", 10 * H + 2 * M, 11 * H),
        ];
        let menu = build_menu(
            &events,
            now_at(9 * H + 55 * M),
            Access::FullAccess,
            false,
            &Config::default(),
        );
        assert!(menu.today.iter().all(|r| !r.conflict));
    }

    #[test]
    fn declining_one_copy_declines_the_whole_meeting() {
        let mut a = ev("A", 10 * H, 10 * H + 30 * M);
        a.external_id = Some("uid-2".to_string());
        let mut b = ev("B", 10 * H, 10 * H + 30 * M);
        b.external_id = Some("uid-2".to_string());
        b.self_status = Some(SelfStatus::Declined);
        let events = [a, b, ev("C", 12 * H, 13 * H)];
        assert_eq!(pick(&events, 9 * H + 50 * M).as_deref(), Some("C"));
    }

    #[test]
    fn solo_blocks_count_but_lose_a_tie() {
        let mut solo = ev("Solo", 10 * H, 10 * H + 30 * M);
        solo.attendee_count = 0;
        let real = ev("Real", 10 * H, 10 * H + 30 * M);
        assert_eq!(
            pick(&[solo.clone(), real], 9 * H + 50 * M).as_deref(),
            Some("Real")
        );
        assert_eq!(pick(&[solo], 9 * H + 50 * M).as_deref(), Some("Solo"));
    }

    #[test]
    fn organizing_a_meeting_counts_as_accepting_it() {
        let mut mine = ev("Mine", 10 * H, 10 * H + 30 * M);
        mine.self_status = None;
        mine.is_organizer = true;
        let mut theirs = ev("Theirs", 10 * H, 10 * H + 30 * M);
        theirs.self_status = Some(SelfStatus::Tentative);
        assert_eq!(
            pick(&[mine, theirs], 9 * H + 50 * M).as_deref(),
            Some("Mine")
        );
    }

    #[test]
    fn tomorrow_is_reachable_and_the_day_after_is_not() {
        let events = [ev("A", 24 * H + 9 * H, 24 * H + 10 * H)];
        assert_eq!(label(&events, 23 * H + 50 * M), "tmr 09:00");

        let far = [ev("A", 72 * H, 73 * H)];
        assert_eq!(pick(&far, 23 * H + 50 * M), None);
    }

    #[test]
    fn a_countdown_across_midnight_stays_a_countdown() {
        let events = [ev("A", 24 * H + 2 * M, 24 * H + 32 * M)];
        assert_eq!(label(&events, 24 * H - 30), "in 3 min");
    }

    #[test]
    fn a_twenty_five_hour_day_does_not_break_the_horizon() {
        // Fall back: the local day is 25 hours long, so "tomorrow" is not now + 86400.
        let now = Now {
            instant: H + 30 * M,
            today_start: 0,
            tomorrow_start: 25 * H,
            day_after_start: 49 * H,
        };
        let cfg = Config::default();
        // 02:30 on the second pass, i.e. two real hours away, still today.
        let e = [ev("A", 3 * H + 30 * M, 4 * H)];
        let sel = select_nearest(&e, now, &cfg).unwrap();
        assert_eq!(countdown(&sel, now, &cfg), "03:30");

        // An event 24 hours out is still "today" on a 25 hour day.
        let e = [ev("B", 24 * H + 30 * M, 24 * H + 60 * M)];
        let sel = select_nearest(&e, now, &cfg).unwrap();
        assert!(!countdown(&sel, now, &cfg).starts_with("tmr"));
    }

    #[test]
    fn overlaps_are_counted_and_surfaced() {
        let events = [
            ev("A", 10 * H, 11 * H),
            ev("B", 10 * H + 30 * M, 11 * H + 30 * M),
        ];
        let sel = select_nearest(&events, now_at(9 * H + 50 * M), &Config::default()).unwrap();
        assert_eq!(sel.event_id, "A");
        assert_eq!(sel.conflicts, 1);

        let menu = build_menu(
            &events,
            now_at(9 * H + 50 * M),
            Access::FullAccess,
            false,
            &Config::default(),
        );
        assert!(menu.today.iter().all(|r| r.conflict));
    }

    #[test]
    fn selection_does_not_depend_on_input_order() {
        let mut events = vec![
            ev("A", 10 * H, 11 * H),
            ev("B", 10 * H, 10 * H + 30 * M),
            ev("C", 9 * H, 18 * H),
            ev("D", 10 * H + 45 * M, 11 * H),
        ];
        let expected = pick(&events, 9 * H + 55 * M);
        for _ in 0..12 {
            events.rotate_left(1);
            assert_eq!(pick(&events, 9 * H + 55 * M), expected);
            events.reverse();
            assert_eq!(pick(&events, 9 * H + 55 * M), expected);
        }
    }

    #[test]
    fn the_countdown_never_goes_backwards() {
        let events = [ev("A", 10 * H, 11 * H)];
        let cfg = Config::default();
        let mut previous = i64::MAX;
        let mut t = 9 * H;
        while t < 10 * H {
            let n = now_at(t);
            let sel = select_nearest(&events, n, &cfg).unwrap();
            if sel.phase == Phase::Upcoming {
                let d = sel.start - n.instant;
                if d <= cfg.near_threshold {
                    let m = ceil_min(d);
                    assert!(m >= 1, "a countdown must never render zero");
                    assert!(m <= previous, "the countdown jumped up at t={t}");
                    previous = m;
                }
            }
            t += 7;
        }
    }

    #[test]
    fn permission_states_override_any_selection() {
        let events = [ev("A", 10 * H, 11 * H)];
        let cfg = Config::default();
        let n = now_at(9 * H + 50 * M);
        for (access, expected) in [
            (Access::NotDetermined, State::NeedsPermission),
            (Access::Denied, State::Denied),
            (Access::WriteOnly, State::Denied),
            (Access::Restricted, State::Restricted),
        ] {
            let menu = build_menu(&events, n, access, false, &cfg);
            assert_eq!(menu.state, expected);
            assert!(
                menu.today.is_empty(),
                "no meeting may leak into a denied menu"
            );
        }
        let menu = build_menu(&events, n, Access::FullAccess, true, &cfg);
        assert_eq!(menu.state, State::Loading);
    }

    #[test]
    fn an_empty_day_is_distinguishable_from_a_permission_problem() {
        let cfg = Config::default();
        let menu = build_menu(&[], now_at(10 * H), Access::FullAccess, false, &cfg);
        assert_eq!(menu.state, State::NoUpcoming);
        assert_eq!(menu.header.primary, "No upcoming meetings");
    }

    #[test]
    fn titles_are_collapsed_truncated_and_never_split_a_char() {
        assert_eq!(
            truncate("Sprint  Planning\n Meeting", 40),
            "Sprint Planning Meeting"
        );
        assert_eq!(truncate("Sprint Planning Meeting", 12), "Sprint\u{2026}");
        assert_eq!(truncate("Retrospective", 6), "Retro\u{2026}");
        assert_eq!(
            truncate("\u{4f1a}\u{8b70}\u{4f1a}\u{8b70}\u{4f1a}\u{8b70}", 3),
            "\u{4f1a}\u{8b70}\u{2026}"
        );
        assert_eq!(display_title("   ", 20), "(no title)");
    }

    #[test]
    fn the_next_change_is_bounded_and_in_the_future() {
        let cfg = Config::default();
        let events = [ev("A", 10 * H, 11 * H)];
        let n = now_at(9 * H);
        let t = next_change_at(&events, n, &cfg).unwrap();
        assert!(t > n.instant && t <= n.instant + 300);

        // One minute out, the next change is the minute tick, not five minutes away.
        let n = now_at(10 * H - 90);
        let t = next_change_at(&events, n, &cfg).unwrap();
        assert_eq!(t, 10 * H - 60);

        assert_eq!(next_change_at(&[], now_at(10 * H), &cfg), None);
    }
}
