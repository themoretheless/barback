use async_trait::async_trait;

use super::error::Result;
use super::model::{Calendar, Capabilities, Event, TimeRange};

/// The single interface every calendar service is exposed through.
///
/// Identifiers are provider-native strings and are never interpreted by
/// callers: Google uses opaque ids, Graph uses base64-ish ids, CalDAV uses
/// URLs. Pass back whatever `list_calendars` and `list_events` returned.
///
/// Recurring events are returned expanded into instances where the provider
/// can expand them server-side, and as the master event with its `recurrence`
/// lines intact where it cannot. [`Capabilities`] tells you which.
#[async_trait]
pub trait CalendarProvider: Send + Sync {
    /// Stable short name, e.g. `"google"`. Used in error messages.
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// Every calendar the authenticated account can see.
    async fn list_calendars(&self) -> Result<Vec<Calendar>>;

    /// Events overlapping `range`, in the given calendar.
    async fn list_events(&self, calendar_id: &str, range: TimeRange) -> Result<Vec<Event>>;

    async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Event>;

    /// Creates an event and returns it as the server stored it, with `id` and
    /// `etag` populated.
    async fn create_event(&self, calendar_id: &str, event: &Event) -> Result<Event>;

    /// Updates an existing event. When `event.etag` is set and the provider
    /// supports ETags, the write is rejected with [`CalendarError::Conflict`]
    /// if the event changed server-side in the meantime.
    ///
    /// [`CalendarError::Conflict`]: super::error::CalendarError::Conflict
    async fn update_event(&self, calendar_id: &str, event: &Event) -> Result<Event>;

    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<()>;

    /// Convenience: the account's primary calendar, or the first one.
    async fn primary_calendar(&self) -> Result<Option<Calendar>> {
        let calendars = self.list_calendars().await?;
        Ok(calendars
            .iter()
            .find(|c| c.primary)
            .or_else(|| calendars.first())
            .cloned())
    }
}

/// Boxed provider, since the concrete type is chosen at runtime from config.
pub type BoxedProvider = Box<dyn CalendarProvider>;
