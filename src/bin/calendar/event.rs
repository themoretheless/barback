use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Recurrence {
    None,
    Daily,
    Weekly,
    Monthly,
}

impl Recurrence {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub title: String,
    pub start: NaiveDateTime,
    pub duration_min: i64,
    pub recurrence: Recurrence,
}

impl Event {
    pub fn end(&self) -> NaiveDateTime {
        self.start + Duration::minutes(self.duration_min)
    }

    /// Returns the occurrence of this event on `date`, if any.
    pub fn occurrence_on(&self, date: NaiveDate) -> Option<NaiveDateTime> {
        let base = self.start.date();
        if date < base {
            return None;
        }
        let matches = match self.recurrence {
            Recurrence::None => date == base,
            Recurrence::Daily => true,
            Recurrence::Weekly => date.weekday() == base.weekday(),
            Recurrence::Monthly => date.day() == base.day(),
        };
        matches.then(|| NaiveDateTime::new(date, self.start.time()))
    }

    pub fn overlaps(&self, other: &Event) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}
