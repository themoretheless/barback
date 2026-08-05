use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Weekday};

use crate::event::Event;

/// All occurrences of `events` on `date`, sorted by start time.
pub fn occurrences_on(events: &[Event], date: NaiveDate) -> Vec<(NaiveDateTime, &Event)> {
    let mut out: Vec<_> = events
        .iter()
        .filter_map(|e| e.occurrence_on(date).map(|t| (t, e)))
        .collect();
    out.sort_by_key(|(t, _)| *t);
    out
}

pub fn day(events: &[Event], date: NaiveDate) {
    println!("{} ({})", date, date.weekday());
    let occ = occurrences_on(events, date);
    if occ.is_empty() {
        println!("  (пусто)");
        return;
    }
    for (start, e) in occ {
        let end = start + Duration::minutes(e.duration_min);
        println!(
            "  [{:>3}] {}-{}  {}",
            e.id,
            start.format("%H:%M"),
            end.format("%H:%M"),
            e.title
        );
    }
}

pub fn week(events: &[Event], date: NaiveDate) {
    let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
    for i in 0..7 {
        day(events, monday + Duration::days(i));
    }
}

pub fn month(events: &[Event], date: NaiveDate) {
    let first = date.with_day(1).unwrap();
    println!("     {}", first.format("%B %Y"));
    println!("Пн Вт Ср Чт Пт Сб Вс");
    let lead = first.weekday().num_days_from_monday();
    print!("{}", "   ".repeat(lead as usize));
    let mut d = first;
    loop {
        let busy = events.iter().any(|e| e.occurrence_on(d).is_some());
        let mark = if busy { '*' } else { ' ' };
        print!("{:>2}{}", d.day(), mark);
        if d.weekday() == Weekday::Sun {
            println!();
        }
        match d.succ_opt() {
            Some(next) if next.month() == d.month() => d = next,
            _ => break,
        }
    }
    println!();
    println!("* — день с событиями");
}

pub fn agenda(events: &[Event], from: NaiveDate, days: i64) {
    for i in 0..days {
        let date = from + Duration::days(i);
        let occ = occurrences_on(events, date);
        if occ.is_empty() {
            continue;
        }
        day(events, date);
    }
}
