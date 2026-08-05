//! End-to-end CalDAV tests against a mock server.
//!
//! These cover what the unit tests cannot: that the right method, headers and
//! body go out, that discovery chains PROPFIND correctly, and that ETags are
//! carried through a create/update/delete cycle.

use barback::calendar::providers::{ProviderConfig, ProviderKind};
use barback::calendar::{Auth, CalendarProvider, Event, EventTime, TimeRange};
use chrono::{TimeZone, Utc};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn multistatus(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(207)
        .set_body_string(body)
        .insert_header("content-type", "application/xml; charset=utf-8")
}

fn connect(server: &MockServer) -> Box<dyn CalendarProvider> {
    ProviderKind::Nextcloud
        .connect(
            ProviderConfig::new(Auth::basic("ada", "app-password"))
                .with_base_url(format!("{}/remote.php/dav/", server.uri())),
        )
        .expect("provider should connect")
}

const PRINCIPAL_RESPONSE: &str = r#"<multistatus xmlns="DAV:">
  <response>
    <href>/remote.php/dav/</href>
    <propstat><prop>
      <current-user-principal><href>/remote.php/dav/principals/users/ada/</href></current-user-principal>
    </prop><status>HTTP/1.1 200 OK</status></propstat>
  </response>
</multistatus>"#;

const HOME_SET_RESPONSE: &str = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <response>
    <href>/remote.php/dav/principals/users/ada/</href>
    <propstat><prop>
      <C:calendar-home-set><href>/remote.php/dav/calendars/ada/</href></C:calendar-home-set>
    </prop><status>HTTP/1.1 200 OK</status></propstat>
  </response>
</multistatus>"#;

const CALENDAR_LIST_RESPONSE: &str = r##"<multistatus xmlns="DAV:"
    xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:I="http://apple.com/ns/ical/">
  <response>
    <href>/remote.php/dav/calendars/ada/</href>
    <propstat><prop><resourcetype><collection/></resourcetype></prop></propstat>
  </response>
  <response>
    <href>/remote.php/dav/calendars/ada/work/</href>
    <propstat><prop>
      <displayname>Work</displayname>
      <resourcetype><collection/><C:calendar/></resourcetype>
      <I:calendar-color>#FF5733</I:calendar-color>
      <C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set>
      <current-user-privilege-set>
        <privilege><read/></privilege><privilege><write-content/></privilege>
      </current-user-privilege-set>
    </prop><status>HTTP/1.1 200 OK</status></propstat>
  </response>
  <response>
    <href>/remote.php/dav/calendars/ada/tasks/</href>
    <propstat><prop>
      <displayname>Tasks</displayname>
      <resourcetype><collection/><C:calendar/></resourcetype>
      <C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set>
    </prop><status>HTTP/1.1 200 OK</status></propstat>
  </response>
</multistatus>"##;

/// Wires up discovery so individual tests only have to mock what they exercise.
async fn mock_discovery(server: &MockServer) {
    Mock::given(method("PROPFIND"))
        .and(path("/remote.php/dav/"))
        .and(body_string_contains("current-user-principal"))
        .respond_with(multistatus(PRINCIPAL_RESPONSE))
        .mount(server)
        .await;

    Mock::given(method("PROPFIND"))
        .and(path("/remote.php/dav/principals/users/ada/"))
        .respond_with(multistatus(HOME_SET_RESPONSE))
        .mount(server)
        .await;

    Mock::given(method("PROPFIND"))
        .and(path("/remote.php/dav/calendars/ada/"))
        .respond_with(multistatus(CALENDAR_LIST_RESPONSE))
        .mount(server)
        .await;
}

#[tokio::test]
async fn discovery_walks_principal_then_home_set() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;

    let calendars = connect(&server).list_calendars().await.unwrap();

    // The plain collection and the VTODO-only calendar are both filtered out.
    assert_eq!(calendars.len(), 1);
    assert_eq!(calendars[0].name, "Work");
    assert_eq!(
        calendars[0].id,
        format!("{}/remote.php/dav/calendars/ada/work/", server.uri())
    );
    assert_eq!(calendars[0].color.as_deref(), Some("#FF5733"));
    assert!(!calendars[0].read_only);
}

#[tokio::test]
async fn discovery_result_is_cached_across_calls() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;
    let provider = connect(&server);

    provider.list_calendars().await.unwrap();
    provider.list_calendars().await.unwrap();

    let principal_requests = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r: &Request| r.url.path() == "/remote.php/dav/principals/users/ada/")
        .count();
    assert_eq!(principal_requests, 1, "home set should be resolved once");
}

#[tokio::test]
async fn discovery_falls_back_to_the_base_url() {
    let server = MockServer::start().await;
    // A server that answers PROPFIND but advertises no principal.
    Mock::given(method("PROPFIND"))
        .and(body_string_contains("current-user-principal"))
        .respond_with(multistatus(r#"<multistatus xmlns="DAV:"></multistatus>"#))
        .mount(&server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/remote.php/dav/"))
        .respond_with(multistatus(CALENDAR_LIST_RESPONSE))
        .mount(&server)
        .await;

    let calendars = connect(&server).list_calendars().await.unwrap();
    assert_eq!(calendars.len(), 1);
}

#[tokio::test]
async fn calendar_query_sends_a_time_range_report() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;

    let report_body = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
      <response>
        <href>/remote.php/dav/calendars/ada/work/sync.ics</href>
        <propstat><prop>
          <getetag>"etag-1"</getetag>
          <C:calendar-data>BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VEVENT
UID:sync-1
DTSTART:20260803T100000Z
DTEND:20260803T110000Z
SUMMARY:Weekly sync
END:VEVENT
END:VCALENDAR</C:calendar-data>
        </prop><status>HTTP/1.1 200 OK</status></propstat>
      </response>
    </multistatus>"#;

    Mock::given(method("REPORT"))
        .and(path("/remote.php/dav/calendars/ada/work/"))
        .and(header("Depth", "1"))
        .and(body_string_contains(
            r#"<C:time-range start="20260803T000000Z" end="20260810T000000Z"/>"#,
        ))
        .respond_with(multistatus(report_body))
        .mount(&server)
        .await;

    let provider = connect(&server);
    let calendar_id = format!("{}/remote.php/dav/calendars/ada/work/", server.uri());
    let range = TimeRange::new(
        Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 8, 10, 0, 0, 0).unwrap(),
    );

    let events = provider.list_events(&calendar_id, range).await.unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].uid, "sync-1");
    assert_eq!(events[0].summary.as_deref(), Some("Weekly sync"));
    assert_eq!(events[0].etag.as_deref(), Some("\"etag-1\""));
    assert_eq!(
        events[0].id,
        format!(
            "{}/remote.php/dav/calendars/ada/work/sync.ics",
            server.uri()
        )
    );
}

#[tokio::test]
async fn a_series_and_its_overrides_share_one_resource() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;

    let report_body = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
      <response>
        <href>/remote.php/dav/calendars/ada/work/standup.ics</href>
        <propstat><prop>
          <getetag>"etag-2"</getetag>
          <C:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:standup
DTSTART:20260803T090000Z
DTEND:20260803T091500Z
RRULE:FREQ=DAILY
SUMMARY:Standup
END:VEVENT
BEGIN:VEVENT
UID:standup
RECURRENCE-ID:20260805T090000Z
DTSTART:20260805T100000Z
DTEND:20260805T101500Z
SUMMARY:Standup (moved)
END:VEVENT
END:VCALENDAR</C:calendar-data>
        </prop></propstat>
      </response>
    </multistatus>"#;

    Mock::given(method("REPORT"))
        .respond_with(multistatus(report_body))
        .mount(&server)
        .await;

    let provider = connect(&server);
    let calendar_id = format!("{}/remote.php/dav/calendars/ada/work/", server.uri());
    let events = provider
        .list_events(&calendar_id, TimeRange::days_from_now(7))
        .await
        .unwrap();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].recurrence, vec!["RRULE:FREQ=DAILY"]);
    assert!(events[0].recurring_event_id.is_none());
    assert_eq!(
        events[1].recurring_event_id.as_deref(),
        Some("20260805T090000Z")
    );
    // Both instances live in the same resource, so they share href and ETag.
    assert_eq!(events[0].id, events[1].id);
    assert_eq!(events[0].etag, events[1].etag);
}

#[tokio::test]
async fn create_puts_ics_with_if_none_match() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;

    Mock::given(method("PUT"))
        .and(header("If-None-Match", "*"))
        .and(header("content-type", "text/calendar; charset=utf-8"))
        .and(body_string_contains("SUMMARY:Design review"))
        .and(body_string_contains("DTSTART:20260803T100000Z"))
        .respond_with(ResponseTemplate::new(201).insert_header("ETag", "\"created-1\""))
        .mount(&server)
        .await;

    let provider = connect(&server);
    let calendar_id = format!("{}/remote.php/dav/calendars/ada/work/", server.uri());

    let mut event = Event::new(
        "Design review",
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
    );
    event.uid = "design-review-1".into();

    let created = provider.create_event(&calendar_id, &event).await.unwrap();

    assert_eq!(created.etag.as_deref(), Some("\"created-1\""));
    assert_eq!(
        created.id,
        format!(
            "{}/remote.php/dav/calendars/ada/work/design-review-1.ics",
            server.uri()
        )
    );
}

#[tokio::test]
async fn update_sends_if_match_with_the_etag() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;

    Mock::given(method("PUT"))
        .and(header("If-Match", "\"etag-1\""))
        .respond_with(ResponseTemplate::new(204).insert_header("ETag", "\"etag-2\""))
        .mount(&server)
        .await;

    let provider = connect(&server);
    let calendar_id = format!("{}/remote.php/dav/calendars/ada/work/", server.uri());

    let mut event = Event::new(
        "Design review",
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
    );
    event.id = format!(
        "{}/remote.php/dav/calendars/ada/work/sync.ics",
        server.uri()
    );
    event.etag = Some("\"etag-1\"".into());

    let updated = provider.update_event(&calendar_id, &event).await.unwrap();
    assert_eq!(updated.etag.as_deref(), Some("\"etag-2\""));
}

#[tokio::test]
async fn a_stale_etag_surfaces_as_a_conflict() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;

    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(412).set_body_string("etag mismatch"))
        .mount(&server)
        .await;

    let provider = connect(&server);
    let calendar_id = format!("{}/remote.php/dav/calendars/ada/work/", server.uri());

    let mut event = Event::new(
        "Design review",
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap()),
        EventTime::Utc(Utc.with_ymd_and_hms(2026, 8, 3, 11, 0, 0).unwrap()),
    );
    event.id = format!(
        "{}/remote.php/dav/calendars/ada/work/sync.ics",
        server.uri()
    );
    event.etag = Some("\"stale\"".into());

    let err = provider
        .update_event(&calendar_id, &event)
        .await
        .expect_err("a 412 must not look like success");
    assert!(
        matches!(err, barback::calendar::CalendarError::Conflict(_)),
        "expected a conflict, got {err:?}"
    );
}

#[tokio::test]
async fn delete_issues_a_delete_to_the_resource() {
    let server = MockServer::start().await;
    mock_discovery(&server).await;

    Mock::given(method("DELETE"))
        .and(path("/remote.php/dav/calendars/ada/work/sync.ics"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let provider = connect(&server);
    let calendar_id = format!("{}/remote.php/dav/calendars/ada/work/", server.uri());
    let event_id = format!(
        "{}/remote.php/dav/calendars/ada/work/sync.ics",
        server.uri()
    );

    provider
        .delete_event(&calendar_id, &event_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn basic_credentials_are_sent() {
    let server = MockServer::start().await;
    // "ada:app-password" base64-encoded.
    Mock::given(method("PROPFIND"))
        .and(header("authorization", "Basic YWRhOmFwcC1wYXNzd29yZA=="))
        .respond_with(multistatus(PRINCIPAL_RESPONSE))
        .mount(&server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/remote.php/dav/principals/users/ada/"))
        .respond_with(multistatus(HOME_SET_RESPONSE))
        .mount(&server)
        .await;
    Mock::given(method("PROPFIND"))
        .and(path("/remote.php/dav/calendars/ada/"))
        .respond_with(multistatus(CALENDAR_LIST_RESPONSE))
        .mount(&server)
        .await;

    assert!(connect(&server).list_calendars().await.is_ok());
}

#[tokio::test]
async fn a_401_is_reported_as_an_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("PROPFIND"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad password"))
        .mount(&server)
        .await;

    // Discovery swallows its own failures, so the error surfaces on the
    // calendar listing rather than being silently treated as "no principal".
    let err = connect(&server)
        .list_calendars()
        .await
        .expect_err("401 must not be silently ignored");
    assert!(
        matches!(err, barback::calendar::CalendarError::Auth(_)),
        "expected an auth error, got {err:?}"
    );
}
