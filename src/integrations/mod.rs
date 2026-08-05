//! Remote calendar service integrations.
//!
//! Ten providers sit on four engines: the Google Calendar API, Microsoft Graph,
//! CalDAV, and read-only iCalendar feeds. Pick one with
//! [`ProviderKind`](providers::ProviderKind), hand it credentials, and work
//! through the [`CalendarProvider`] trait.
//!
//! This is distinct from the binary's `calendar` module, which reads the local
//! macOS calendar store through EventKit. That one asks the system what the
//! user already has; this one talks to the services over the network.
//!
//! ```no_run
//! use barback::integrations::{Auth, CalendarProvider, TimeRange};
//! use barback::integrations::providers::{ProviderConfig, ProviderKind};
//!
//! # async fn example() -> barback::integrations::Result<()> {
//! let calendar = ProviderKind::AppleICloud
//!     .connect(ProviderConfig::new(Auth::basic("ada@icloud.com", "app-specific-password")))?;
//!
//! for entry in calendar.list_calendars().await? {
//!     let events = calendar
//!         .list_events(&entry.id, TimeRange::days_from_now(7))
//!         .await?;
//!     println!("{}: {} events", entry.name, events.len());
//! }
//! # Ok(())
//! # }
//! ```

pub mod auth;
pub mod engines;
pub mod error;
pub mod http;
pub mod ical;
pub mod model;
pub mod provider;
pub mod providers;

pub use auth::{Auth, OAuth2, OAuth2Config, TokenSet};
pub use error::{CalendarError, Result};
pub use model::{
    Attendee, Calendar, Capabilities, Event, EventStatus, EventTime, ParticipationStatus, Person,
    Reminder, ReminderMethod, TimeRange, Transparency,
};
pub use provider::{BoxedProvider, CalendarProvider};
pub use providers::{AuthKind, Engine, Preset, ProviderConfig, ProviderKind};
