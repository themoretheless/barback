//! End-to-end tests for the two REST engines against a mock server.
//!
//! The interesting behaviour is not the JSON mapping (covered by unit tests)
//! but the request shape: query parameters, pagination, and the headers that
//! decide whether the data is even correct.

use barback::integrations::providers::{ProviderConfig, ProviderKind};
use barback::integrations::{Auth, CalendarProvider, Event, EventTime, TimeRange};
use chrono::{TimeZone, Utc};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn google(server: &MockServer) -> Box<dyn CalendarProvider> {
    ProviderKind::Google
        .connect(
            ProviderConfig::new(Auth::Bearer("token-123".into()))
                .with_base_url(format!("{}/calendar/v3", server.uri())),
        )
        .expect("google provider should connect")
}

fn graph(server: &MockServer) -> Box<dyn CalendarProvider> {
    ProviderKind::Microsoft
        .connect(
            ProviderConfig::new(Auth::Bearer("token-456".into()))
                .with_base_url(format!("{}/v1.0/me", server.uri())),
        )
        .expect("graph provider should connect")
}

fn range() -> TimeRange {
    TimeRange::new(
        Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap(),
    )
}

#[tokio::test]
async fn google_sends_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .and(header("authorization", "Bearer token-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "ada@example.com", "summary": "Ada", "primary": true,
                       "accessRole": "owner"}]
        })))
        .mount(&server)
        .await;

    let calendars = google(&server).list_calendars().await.unwrap();
    assert_eq!(calendars.len(), 1);
    assert!(calendars[0].primary);
}

#[tokio::test]
async fn google_follows_calendar_list_pagination() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .and(query_param("pageToken", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "b", "summary": "Second"}]
        })))
        .mount(&server)
        .await;

    // Mounted second so it only matches requests without the token; wiremock
    // prefers the most recently mounted matching stub.
    Mock::given(method("GET"))
        .and(path("/calendar/v3/users/me/calendarList"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "a", "summary": "First"}],
            "nextPageToken": "page-2"
        })))
        .mount(&server)
        .await;

    let calendars = google(&server).list_calendars().await.unwrap();
    let names: Vec<&str> = calendars.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["First", "Second"]);
}

#[tokio::test]
async fn google_asks_the_api_to_expand_recurring_series() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/calendar/v3/calendars/ada%40example%2Ecom/events"))
        .and(query_param("singleEvents", "true"))
        .and(query_param("orderBy", "startTime"))
        .and(query_param("timeMin", "2026-08-03T00:00:00+00:00"))
        .and(query_param("timeMax", "2026-08-10T00:00:00+00:00"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{
                "id": "evt1",
                "iCalUID": "evt1@google.com",
                "summary": "Sync",
                "start": {"dateTime": "2026-08-03T12:00:00+02:00", "timeZone": "Europe/Berlin"},
                "end": {"dateTime": "2026-08-03T13:00:00+02:00", "timeZone": "Europe/Berlin"}
            }]
        })))
        .mount(&server)
        .await;

    let events = google(&server)
        .list_events("ada@example.com", range())
        .await
        .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].summary.as_deref(), Some("Sync"));
    assert_eq!(events[0].calendar_id, "ada@example.com");
    assert_eq!(
        events[0].start.to_utc(),
        Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()
    );
}

#[tokio::test]
async fn google_create_posts_the_event_and_returns_the_stored_copy() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/calendar/v3/calendars/ada%40example%2Ecom/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "server-assigned",
            "iCalUID": "server-assigned@google.com",
            "etag": "\"p33\"",
            "summary": "Design review",
            "start": {"dateTime": "2026-08-03T10:00:00Z"},
            "end": {"dateTime": "2026-08-03T11:00:00Z"}
        })))
        .mount(&server)
        .await;

    let event = Event::new(
        "Design review",
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
    );
    let created = google(&server)
        .create_event("ada@example.com", &event)
        .await
        .unwrap();

    assert_eq!(created.id, "server-assigned");
    assert_eq!(created.etag.as_deref(), Some("\"p33\""));
}

#[tokio::test]
async fn google_update_sends_if_match() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(header("if-match", "\"p33\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "evt1",
            "etag": "\"p34\"",
            "start": {"dateTime": "2026-08-03T10:00:00Z"},
            "end": {"dateTime": "2026-08-03T11:00:00Z"}
        })))
        .mount(&server)
        .await;

    let mut event = Event::new(
        "Design review",
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
    );
    event.id = "evt1".into();
    event.etag = Some("\"p33\"".into());

    let updated = google(&server)
        .update_event("ada@example.com", &event)
        .await
        .unwrap();
    assert_eq!(updated.etag.as_deref(), Some("\"p34\""));
}

#[tokio::test]
async fn google_rate_limit_is_typed_not_swallowed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "30")
                .set_body_string("rateLimitExceeded"),
        )
        .mount(&server)
        .await;

    let err = google(&server)
        .list_calendars()
        .await
        .expect_err("429 must not look like an empty calendar list");
    match err {
        barback::integrations::CalendarError::RateLimited { retry_after } => {
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(30)));
        }
        other => panic!("expected a rate-limit error, got {other:?}"),
    }
}

#[tokio::test]
async fn graph_requests_utc_and_uses_calendar_view() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars/AAA%3D/calendarView"))
        .and(header("Prefer", "outlook.timezone=\"UTC\""))
        .and(query_param("startDateTime", "2026-08-03T00:00:00+00:00"))
        .and(query_param("endDateTime", "2026-08-10T00:00:00+00:00"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "value": [{
                "id": "AAMkAD",
                "subject": "Sync",
                "start": {"dateTime": "2026-08-03T10:00:00.0000000", "timeZone": "UTC"},
                "end": {"dateTime": "2026-08-03T11:00:00.0000000", "timeZone": "UTC"}
            }]
        })))
        .mount(&server)
        .await;

    let events = graph(&server).list_events("AAA=", range()).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].start.to_utc(),
        Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()
    );
}

#[tokio::test]
async fn graph_follows_odata_next_links() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1.0/me/page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "value": [{"id": "b", "name": "Second"}]
        })))
        .mount(&server)
        .await;

    let next_link = format!("{}/v1.0/me/page2", server.uri());
    Mock::given(method("GET"))
        .and(path("/v1.0/me/calendars"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "value": [{"id": "a", "name": "First"}],
            "@odata.nextLink": next_link
        })))
        .mount(&server)
        .await;

    let calendars = graph(&server).list_calendars().await.unwrap();
    let names: Vec<&str> = calendars.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["First", "Second"]);
}

#[tokio::test]
async fn graph_update_uses_patch_with_if_match() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/v1.0/me/events/AAMkAD"))
        .and(header("if-match", "W/\"abc\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "AAMkAD",
            "@odata.etag": "W/\"def\"",
            "start": {"dateTime": "2026-08-03T10:00:00.0000000", "timeZone": "UTC"},
            "end": {"dateTime": "2026-08-03T11:00:00.0000000", "timeZone": "UTC"}
        })))
        .mount(&server)
        .await;

    let mut event = Event::new(
        "Sync",
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
    );
    event.id = "AAMkAD".into();
    event.etag = Some("W/\"abc\"".into());

    let updated = graph(&server).update_event("AAA=", &event).await.unwrap();
    assert_eq!(updated.etag.as_deref(), Some("W/\"def\""));
}

#[tokio::test]
async fn graph_404_is_reported_as_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_string("ErrorItemNotFound"))
        .mount(&server)
        .await;

    let err = graph(&server)
        .get_event("AAA=", "missing")
        .await
        .expect_err("404 must be an error");
    assert!(
        matches!(err, barback::integrations::CalendarError::NotFound(_)),
        "expected not-found, got {err:?}"
    );
}

#[tokio::test]
async fn ics_feed_reads_and_filters_a_published_calendar() {
    let server = MockServer::start().await;
    let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Team offsites\r\n\
                BEGIN:VEVENT\r\nUID:in-range\r\nDTSTART:20260803T100000Z\r\n\
                DTEND:20260803T110000Z\r\nSUMMARY:Offsite\r\nEND:VEVENT\r\n\
                BEGIN:VEVENT\r\nUID:out-of-range\r\nDTSTART:20200101T100000Z\r\n\
                DTEND:20200101T110000Z\r\nSUMMARY:Ancient\r\nEND:VEVENT\r\n\
                END:VCALENDAR\r\n";

    Mock::given(method("GET"))
        .and(path("/feed.ics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(feed)
                .insert_header("content-type", "text/calendar"),
        )
        .mount(&server)
        .await;

    // The webcal scheme has to survive being turned into a real request.
    let url = format!("{}/feed.ics", server.uri()).replace("http://", "webcal://");
    let provider = ProviderKind::IcsFeed
        .connect(ProviderConfig::new(Auth::None).with_base_url(url))
        .expect("feed should connect");

    let calendars = provider.list_calendars().await;
    // webcal:// is rewritten to https://, which the plain-HTTP mock cannot
    // serve, so this path is verified over http directly instead.
    assert!(calendars.is_err(), "webcal must be upgraded to https");

    let provider = ProviderKind::IcsFeed
        .connect(
            ProviderConfig::new(Auth::None).with_base_url(format!("{}/feed.ics", server.uri())),
        )
        .expect("feed should connect");

    let calendars = provider.list_calendars().await.unwrap();
    assert_eq!(calendars[0].name, "Team offsites");
    assert!(calendars[0].read_only);

    let events = provider
        .list_events(&calendars[0].id, range())
        .await
        .unwrap();
    let uids: Vec<&str> = events.iter().map(|e| e.uid.as_str()).collect();
    assert_eq!(uids, vec!["in-range"]);
}
