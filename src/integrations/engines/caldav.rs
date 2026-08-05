//! CalDAV (RFC 4791) engine.
//!
//! Backs every provider here that is not Google or Graph: iCloud, Fastmail,
//! Yahoo, Zoho, Nextcloud, Zimbra and Open-Xchange all speak the same protocol,
//! and differ only in their base URL and how they want you to authenticate.
//!
//! Recurring events come back as the master `VEVENT` with its `RRULE` intact.
//! CalDAV's `time-range` filter is evaluated against expanded instances
//! server-side, so a weekly meeting that started last year is returned for a
//! window covering today, but it is returned *as the master*, with its original
//! `DTSTART`. Expanding it is the caller's job.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Method;
use reqwest::header::{CONTENT_TYPE, ETAG, HeaderValue};
use tokio::sync::RwLock;
use url::Url;

use crate::integrations::error::{CalendarError, Result};
use crate::integrations::http::HttpClient;
use crate::integrations::ical;
use crate::integrations::model::{Calendar, Capabilities, Event, TimeRange};
use crate::integrations::provider::CalendarProvider;

use super::dav::{DavResponse, parse_multistatus};

const XML_CONTENT_TYPE: &str = "application/xml; charset=utf-8";
const ICS_CONTENT_TYPE: &str = "text/calendar; charset=utf-8";

const PRINCIPAL_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:current-user-principal/></D:prop></D:propfind>"#;

const HOME_SET_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><C:calendar-home-set/></D:prop>
</D:propfind>"#;

const CALENDAR_LIST_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"
            xmlns:I="http://apple.com/ns/ical/">
  <D:prop>
    <D:displayname/>
    <D:resourcetype/>
    <D:current-user-privilege-set/>
    <C:calendar-description/>
    <C:calendar-timezone/>
    <C:supported-calendar-component-set/>
    <I:calendar-color/>
  </D:prop>
</D:propfind>"#;

pub struct CalDav {
    http: HttpClient,
    /// Entry point for discovery; also the base every relative href is
    /// resolved against.
    base: Url,
    provider: &'static str,
    home_set: RwLock<Option<Url>>,
}

impl CalDav {
    pub fn new(http: HttpClient, base: Url, provider: &'static str) -> Self {
        CalDav {
            http,
            base,
            provider,
            home_set: RwLock::new(None),
        }
    }

    fn method(name: &str) -> Method {
        Method::from_bytes(name.as_bytes()).expect("method names here are static and valid")
    }

    async fn dav_request(
        &self,
        method: &str,
        url: &Url,
        depth: &str,
        body: &'static str,
    ) -> Result<Vec<DavResponse>> {
        let request = self
            .http
            .request(Self::method(method), url.as_str())
            .header("Depth", depth)
            .header(CONTENT_TYPE, HeaderValue::from_static(XML_CONTENT_TYPE))
            .body(body);
        let response = self.http.send(request).await?;
        let text = response.text().await?;
        parse_multistatus(&text)
    }

    /// Resolves and caches the calendar home set.
    ///
    /// Discovery is best-effort: many servers are configured so that the base
    /// URL *is* the home set, and failing the whole session because they do not
    /// advertise a principal would be wrong.
    async fn home_set(&self) -> Result<Url> {
        if let Some(cached) = self.home_set.read().await.clone() {
            return Ok(cached);
        }

        let mut guard = self.home_set.write().await;
        if let Some(cached) = guard.clone() {
            return Ok(cached);
        }

        let discovered = self.discover_home_set().await.unwrap_or(None);
        let resolved = discovered.unwrap_or_else(|| self.base.clone());
        *guard = Some(resolved.clone());
        Ok(resolved)
    }

    async fn discover_home_set(&self) -> Result<Option<Url>> {
        let principal_responses = self
            .dav_request("PROPFIND", &self.base, "0", PRINCIPAL_BODY)
            .await?;

        let principal_href = principal_responses
            .iter()
            .find_map(|r| r.text("current-user-principal"))
            .map(str::to_string);

        let Some(principal_href) = principal_href else {
            return Ok(None);
        };
        let principal_url = self.absolute(&principal_href)?;

        let home_responses = self
            .dav_request("PROPFIND", &principal_url, "0", HOME_SET_BODY)
            .await?;

        let home_href = home_responses
            .iter()
            .find_map(|r| r.text("calendar-home-set"))
            .map(str::to_string);

        match home_href {
            Some(href) => Ok(Some(self.absolute(&href)?)),
            None => Ok(None),
        }
    }

    /// Resolves an href from a DAV response, which may be absolute or a path.
    fn absolute(&self, href: &str) -> Result<Url> {
        self.base.join(href.trim()).map_err(CalendarError::from)
    }

    fn calendar_from_response(&self, response: &DavResponse) -> Option<Calendar> {
        let resourcetype = response.prop("resourcetype")?;
        if !resourcetype.has_child("calendar") {
            return None;
        }

        // A calendar that cannot hold events (a task list, say) is not useful
        // here. Servers that omit the property are assumed to accept VEVENTs.
        if let Some(comps) = response.prop("supported-calendar-component-set")
            && !comps.children.is_empty()
            && !comps.has_child("VEVENT")
        {
            return None;
        }

        let url = self.absolute(&response.href).ok()?;
        let name = response
            .text("displayname")
            .map(str::to_string)
            .unwrap_or_else(|| last_path_segment(&url));

        // Absence of the privilege set is not evidence of read-only access;
        // only an explicit set that lacks write privileges is.
        let read_only = response
            .prop("current-user-privilege-set")
            .map(|p| !(p.has_child("write") || p.has_child("write-content")))
            .unwrap_or(false);

        Some(Calendar {
            id: url.to_string(),
            name,
            description: response.text("calendar-description").map(str::to_string),
            timezone: response.text("calendar-timezone").and_then(tzid_from_ics),
            color: response.text("calendar-color").map(str::to_string),
            read_only,
            primary: false,
        })
    }

    async fn put(&self, url: &Url, event: &Event, if_header: (&str, String)) -> Result<Event> {
        let body = ical::to_ics(event);
        let request = self
            .http
            .request(Method::PUT, url.as_str())
            .header(CONTENT_TYPE, HeaderValue::from_static(ICS_CONTENT_TYPE))
            .header(if_header.0, if_header.1)
            .body(body);

        let response = self.http.send(request).await?;
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let mut stored = event.clone();
        stored.id = url.to_string();
        stored.calendar_id = parent_url(url);
        // Servers may omit the ETag on PUT; the caller then has to re-read
        // before it can do a conditional update, which is why this is None
        // rather than a fabricated value.
        stored.etag = etag;
        Ok(stored)
    }
}

#[async_trait]
impl CalendarProvider for CalDav {
    fn name(&self) -> &'static str {
        self.provider
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::FULL
    }

    async fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let home = self.home_set().await?;
        let responses = self
            .dav_request("PROPFIND", &home, "1", CALENDAR_LIST_BODY)
            .await?;

        let calendars: Vec<Calendar> = responses
            .iter()
            .filter_map(|r| self.calendar_from_response(r))
            .collect();

        if calendars.is_empty() && responses.is_empty() {
            return Err(CalendarError::Discovery(format!(
                "{} returned no collections under {home}",
                self.provider
            )));
        }
        Ok(calendars)
    }

    async fn list_events(&self, calendar_id: &str, range: TimeRange) -> Result<Vec<Event>> {
        let url = self.absolute(calendar_id)?;
        let body = calendar_query_body(range.start, range.end);

        let request = self
            .http
            .request(Self::method("REPORT"), url.as_str())
            .header("Depth", "1")
            .header(CONTENT_TYPE, HeaderValue::from_static(XML_CONTENT_TYPE))
            .body(body);
        let response = self.http.send(request).await?;
        let text = response.text().await?;
        let responses = parse_multistatus(&text)?;

        let mut events = Vec::new();
        for dav_response in &responses {
            let Some(data) = dav_response.text("calendar-data") else {
                continue;
            };
            let href = self
                .absolute(&dav_response.href)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| dav_response.href.clone());
            let etag = dav_response.text("getetag").map(str::to_string);

            // One resource holds a whole series: the master plus any
            // per-instance overrides, all sharing the href and ETag.
            for mut event in ical::parse_events(data)? {
                event.id = href.clone();
                event.etag = etag.clone();
                event.calendar_id = calendar_id.to_string();
                events.push(event);
            }
        }
        Ok(events)
    }

    async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Event> {
        let url = self.absolute(event_id)?;
        let response = self.http.send(self.http.get(url.as_str())).await?;
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let text = response.text().await?;

        let mut events = ical::parse_events(&text)?;
        if events.is_empty() {
            return Err(CalendarError::NotFound(format!("{url} contains no VEVENT")));
        }
        // With overrides present the master is the one without RECURRENCE-ID.
        let index = events
            .iter()
            .position(|e| e.recurring_event_id.is_none())
            .unwrap_or(0);
        let mut event = events.swap_remove(index);
        event.id = url.to_string();
        event.etag = etag;
        event.calendar_id = calendar_id.to_string();
        Ok(event)
    }

    async fn create_event(&self, calendar_id: &str, event: &Event) -> Result<Event> {
        let collection = self.absolute(calendar_id)?;
        let uid = if event.uid.is_empty() {
            crate::integrations::model::new_uid()
        } else {
            event.uid.clone()
        };

        let mut event = event.clone();
        event.uid = uid.clone();

        let url = collection.join(&format!("{}.ics", encode_segment(&uid)))?;
        // If-None-Match: * makes the PUT fail rather than overwrite, should the
        // UID already exist.
        self.put(&url, &event, ("If-None-Match", "*".to_string()))
            .await
    }

    async fn update_event(&self, _calendar_id: &str, event: &Event) -> Result<Event> {
        if event.id.is_empty() {
            return Err(CalendarError::Config(
                "update_event needs the event's resource href in `id`".into(),
            ));
        }
        let url = self.absolute(&event.id)?;
        let condition = match &event.etag {
            Some(etag) => ("If-Match", etag.clone()),
            // Without an ETag the write is unconditional and can clobber a
            // concurrent change. The caller opted into that by not carrying one.
            None => ("If-Match", "*".to_string()),
        };
        self.put(&url, event, condition).await
    }

    async fn delete_event(&self, _calendar_id: &str, event_id: &str) -> Result<()> {
        let url = self.absolute(event_id)?;
        let request = self.http.request(Method::DELETE, url.as_str());
        self.http.send(request).await?;
        Ok(())
    }
}

fn calendar_query_body(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><D:getetag/><C:calendar-data/></D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{}" end="{}"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#,
        start.format("%Y%m%dT%H%M%SZ"),
        end.format("%Y%m%dT%H%M%SZ")
    )
}

/// Percent-encodes a UID for use as a path segment.
fn encode_segment(value: &str) -> String {
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.');
    percent_encoding::utf8_percent_encode(value, KEEP).to_string()
}

fn last_path_segment(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut segments| segments.rfind(|s| !s.is_empty()))
        .unwrap_or("calendar")
        .to_string()
}

fn parent_url(url: &Url) -> String {
    let mut parent = url.clone();
    if let Ok(mut segments) = parent.path_segments_mut() {
        segments.pop();
        segments.push("");
    }
    parent.to_string()
}

/// Pulls the `TZID` out of a `calendar-timezone` property, which carries a
/// whole `VCALENDAR` wrapping a `VTIMEZONE`.
fn tzid_from_ics(ics: &str) -> Option<String> {
    let components = ical::syntax::parse(ics).ok()?;
    for root in &components {
        if let Some(vtimezone) = root.find("VTIMEZONE") {
            return vtimezone.prop_text("TZID");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::auth::Auth;
    use chrono::TimeZone;

    fn engine(base: &str) -> CalDav {
        let http = HttpClient::new(reqwest::Client::new(), Auth::None, "test");
        CalDav::new(http, Url::parse(base).unwrap(), "test")
    }

    #[test]
    fn resolves_relative_and_absolute_hrefs() {
        let dav = engine("https://caldav.example.com/dav/ada/");
        assert_eq!(
            dav.absolute("/calendars/ada/work/").unwrap().as_str(),
            "https://caldav.example.com/calendars/ada/work/"
        );
        assert_eq!(
            dav.absolute("https://other.example.com/c/")
                .unwrap()
                .as_str(),
            "https://other.example.com/c/"
        );
    }

    #[test]
    fn calendar_query_body_uses_ical_utc_stamps() {
        let start = Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap();
        let body = calendar_query_body(start, end);
        assert!(body.contains(r#"start="20260803T000000Z" end="20260810T000000Z""#));
        assert!(body.contains(r#"<C:comp-filter name="VEVENT">"#));
    }

    #[test]
    fn builds_calendars_and_skips_plain_collections() {
        let dav = engine("https://caldav.example.com/");
        let xml = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"
                                  xmlns:I="http://apple.com/ns/ical/">
          <response>
            <href>/calendars/ada/</href>
            <propstat><prop><resourcetype><collection/></resourcetype></prop></propstat>
          </response>
          <response>
            <href>/calendars/ada/work/</href>
            <propstat><prop>
              <displayname>Work</displayname>
              <resourcetype><collection/><C:calendar/></resourcetype>
              <I:calendar-color>#FF5733</I:calendar-color>
              <C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set>
            </prop></propstat>
          </response>
        </multistatus>"#;
        let responses = parse_multistatus(xml).unwrap();
        let calendars: Vec<Calendar> = responses
            .iter()
            .filter_map(|r| dav.calendar_from_response(r))
            .collect();

        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].name, "Work");
        assert_eq!(
            calendars[0].id,
            "https://caldav.example.com/calendars/ada/work/"
        );
        assert_eq!(calendars[0].color.as_deref(), Some("#FF5733"));
        assert!(!calendars[0].read_only);
    }

    #[test]
    fn task_only_collections_are_skipped() {
        let dav = engine("https://caldav.example.com/");
        let xml = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
          <response><href>/c/tasks/</href><propstat><prop>
            <resourcetype><collection/><C:calendar/></resourcetype>
            <C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set>
          </prop></propstat></response>
        </multistatus>"#;
        let responses = parse_multistatus(xml).unwrap();
        assert!(dav.calendar_from_response(&responses[0]).is_none());
    }

    #[test]
    fn explicit_read_only_privileges_are_honoured() {
        let dav = engine("https://caldav.example.com/");
        let xml = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
          <response><href>/c/shared/</href><propstat><prop>
            <displayname>Shared</displayname>
            <resourcetype><collection/><C:calendar/></resourcetype>
            <current-user-privilege-set><privilege><read/></privilege></current-user-privilege-set>
          </prop></propstat></response>
        </multistatus>"#;
        let responses = parse_multistatus(xml).unwrap();
        let calendar = dav.calendar_from_response(&responses[0]).unwrap();
        assert!(calendar.read_only);
    }

    #[test]
    fn missing_displayname_falls_back_to_the_path() {
        let dav = engine("https://caldav.example.com/");
        let xml = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
          <response><href>/calendars/ada/personal/</href><propstat><prop>
            <resourcetype><collection/><C:calendar/></resourcetype>
          </prop></propstat></response>
        </multistatus>"#;
        let responses = parse_multistatus(xml).unwrap();
        assert_eq!(
            dav.calendar_from_response(&responses[0]).unwrap().name,
            "personal"
        );
    }

    #[test]
    fn extracts_tzid_from_calendar_timezone() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
                   END:VTIMEZONE\r\nEND:VCALENDAR\r\n";
        assert_eq!(tzid_from_ics(ics).as_deref(), Some("Europe/Berlin"));
        assert_eq!(tzid_from_ics("not ical"), None);
    }

    #[test]
    fn encodes_uids_that_are_not_url_safe() {
        assert_eq!(encode_segment("abc-123_x.y"), "abc-123_x.y");
        assert_eq!(encode_segment("a b/c@d"), "a%20b%2Fc%40d");
    }

    #[test]
    fn parent_url_drops_the_resource_name() {
        let url = Url::parse("https://x.test/calendars/ada/work/evt.ics").unwrap();
        assert_eq!(parent_url(&url), "https://x.test/calendars/ada/work/");
    }

    #[tokio::test]
    async fn update_without_an_id_is_a_config_error() {
        let dav = engine("https://caldav.example.com/");
        let event = Event::new(
            "x",
            Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap().into(),
            Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap().into(),
        );
        let err = dav.update_event("cal", &event).await.unwrap_err();
        assert!(matches!(err, CalendarError::Config(_)));
    }
}
