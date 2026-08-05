mod event;
mod store;
mod view;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{Local, NaiveDate, NaiveDateTime};

use event::{Event, Recurrence};
use store::Store;

const USAGE: &str = "barback — CLI-календарь

Использование:
  barback add <ГГГГ-ММ-ДД> <ЧЧ:ММ> <минуты> <название> [none|daily|weekly|monthly]
  barback rm <id>
  barback day [ГГГГ-ММ-ДД]
  barback week [ГГГГ-ММ-ДД]
  barback month [ГГГГ-ММ-ДД]
  barback agenda [дней]

События хранятся в barback.json в текущей директории.";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("Ошибка: {msg}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut store = Store::load(PathBuf::from("barback.json"))
        .map_err(|e| format!("чтение barback.json: {e}"))?;
    let today = Local::now().date_naive();

    match args.first().map(String::as_str) {
        Some("add") => {
            if args.len() < 5 {
                return Err("add: нужно минимум 4 аргумента".into());
            }
            let start = NaiveDateTime::parse_from_str(
                &format!("{} {}", args[1], args[2]),
                "%Y-%m-%d %H:%M",
            )
            .map_err(|e| format!("дата/время: {e}"))?;
            let duration_min: i64 = args[3]
                .parse()
                .map_err(|_| "минуты: не число".to_string())?;
            if duration_min <= 0 {
                return Err("минуты: должно быть > 0".into());
            }
            let recurrence = match args.last().map(String::as_str) {
                Some(last) if Recurrence::parse(last).is_some() && args.len() > 5 => {
                    Recurrence::parse(last).unwrap()
                }
                _ => Recurrence::None,
            };
            let title_end = if recurrence != Recurrence::None {
                args.len() - 1
            } else {
                args.len()
            };
            let title = args[4..title_end].join(" ");
            if title.is_empty() {
                return Err("название пустое".into());
            }
            let ev = Event {
                id: store.next_id(),
                title,
                start,
                duration_min,
                recurrence,
            };
            for other in &store.events {
                if let Some(t) = other.occurrence_on(start.date()) {
                    let shifted = Event {
                        start: t,
                        ..other.clone()
                    };
                    if shifted.overlaps(&ev) {
                        println!(
                            "Внимание: пересекается с [{}] {} ({})",
                            other.id,
                            other.title,
                            t.format("%H:%M")
                        );
                    }
                }
            }
            println!("Добавлено [{}] {}", ev.id, ev.title);
            store.events.push(ev);
            store.save().map_err(|e| format!("сохранение: {e}"))?;
        }
        Some("rm") => {
            let id: u64 = args
                .get(1)
                .ok_or("rm: нужен id")?
                .parse()
                .map_err(|_| "id: не число".to_string())?;
            if store.remove(id) {
                store.save().map_err(|e| format!("сохранение: {e}"))?;
                println!("Удалено [{id}]");
            } else {
                return Err(format!("события с id {id} нет"));
            }
        }
        Some("day") => view::day(&store.events, arg_date(&args, 1, today)?),
        Some("week") => view::week(&store.events, arg_date(&args, 1, today)?),
        Some("month") => view::month(&store.events, arg_date(&args, 1, today)?),
        Some("agenda") => {
            let days: i64 = match args.get(1) {
                Some(s) => s.parse().map_err(|_| "дней: не число".to_string())?,
                None => 14,
            };
            view::agenda(&store.events, today, days);
        }
        _ => {
            println!("{USAGE}");
        }
    }
    Ok(())
}

fn arg_date(args: &[String], idx: usize, default: NaiveDate) -> Result<NaiveDate, String> {
    match args.get(idx) {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| format!("дата: {e}")),
        None => Ok(default),
    }
}
