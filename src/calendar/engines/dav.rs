//! WebDAV `multistatus` parsing.
//!
//! Deliberately namespace-agnostic: it matches on local element names only.
//! Servers disagree constantly about prefixes (`D:`, `d:`, `dav:`, none at
//! all) and about which namespace `calendar-color` lives in, and none of that
//! ambiguity is worth modelling.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::Event as XmlEvent;

use crate::calendar::error::{CalendarError, Result};

/// One property inside a `<prop>` block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DavProp {
    /// All descendant text, concatenated. For `<href>`-wrapped properties such
    /// as `calendar-home-set` this is exactly the href.
    pub text: String,
    /// Local names of every descendant element, in document order. This is how
    /// `resourcetype` and `current-user-privilege-set` are inspected.
    pub children: Vec<String>,
}

impl DavProp {
    pub fn has_child(&self, name: &str) -> bool {
        self.children.iter().any(|c| c == name)
    }
}

/// One `<response>` element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DavResponse {
    pub href: String,
    /// Lower-cased local property names.
    pub props: HashMap<String, DavProp>,
}

impl DavResponse {
    pub fn prop(&self, name: &str) -> Option<&DavProp> {
        self.props.get(name)
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        self.props
            .get(name)
            .map(|p| p.text.trim())
            .filter(|t| !t.is_empty())
    }
}

/// Parses a `<multistatus>` document into its responses.
pub fn parse_multistatus(xml: &str) -> Result<Vec<DavResponse>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut responses: Vec<DavResponse> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut current: Option<DavResponse> = None;
    let mut prop_depth: Option<usize> = None;
    let mut current_prop: Option<(String, DavProp)> = None;
    let mut in_response_href = false;

    loop {
        match reader.read_event()? {
            XmlEvent::Start(e) => {
                let name = local_name(e.name().as_ref());
                let comp_name = comp_name_attribute(&name, &e);
                on_start(
                    name,
                    comp_name,
                    &mut stack,
                    &mut current,
                    &mut prop_depth,
                    &mut current_prop,
                    &mut in_response_href,
                );
            }
            XmlEvent::Empty(e) => {
                let name = local_name(e.name().as_ref());
                let comp_name = comp_name_attribute(&name, &e);
                on_start(
                    name.clone(),
                    comp_name,
                    &mut stack,
                    &mut current,
                    &mut prop_depth,
                    &mut current_prop,
                    &mut in_response_href,
                );
                on_end(
                    &name,
                    &mut stack,
                    &mut responses,
                    &mut current,
                    &mut prop_depth,
                    &mut current_prop,
                    &mut in_response_href,
                );
            }
            XmlEvent::End(e) => {
                let name = local_name(e.name().as_ref());
                on_end(
                    &name,
                    &mut stack,
                    &mut responses,
                    &mut current,
                    &mut prop_depth,
                    &mut current_prop,
                    &mut in_response_href,
                );
            }
            XmlEvent::Text(t) => {
                let text = t
                    .xml10_content()
                    .map_err(|e| CalendarError::parse("XML text node", e.to_string()))?
                    .into_owned();
                append_text(&text, &mut current_prop, &mut current, in_response_href);
            }
            XmlEvent::CData(c) => {
                // calendar-data is sometimes wrapped in CDATA to avoid escaping
                // the ICS payload.
                let text = c
                    .decode()
                    .map_err(|e| CalendarError::parse("XML CDATA", e.to_string()))?
                    .into_owned();
                append_text(&text, &mut current_prop, &mut current, in_response_href);
            }
            XmlEvent::Eof => {
                // quick-xml tolerates elements left open at EOF; a truncated
                // multistatus is a real failure mode and must not look like an
                // empty result set.
                if let Some(open) = stack.last() {
                    return Err(CalendarError::parse(
                        "WebDAV multistatus",
                        format!("document ended inside <{open}>"),
                    ));
                }
                break;
            }
            _ => {}
        }
    }

    Ok(responses)
}

#[allow(clippy::too_many_arguments)]
fn on_start(
    name: String,
    comp_name: Option<String>,
    stack: &mut Vec<String>,
    current: &mut Option<DavResponse>,
    prop_depth: &mut Option<usize>,
    current_prop: &mut Option<(String, DavProp)>,
    in_response_href: &mut bool,
) {
    stack.push(name.clone());
    let depth = stack.len();

    if name == "response" && current.is_none() {
        *current = Some(DavResponse::default());
        return;
    }

    if let Some(pd) = *prop_depth {
        if depth == pd + 1 {
            *current_prop = Some((name, DavProp::default()));
        } else if depth > pd + 1
            && let Some((_, prop)) = current_prop.as_mut()
        {
            prop.children.push(name);
            // <comp name="VEVENT"/> carries its payload in an attribute.
            if let Some(comp) = comp_name {
                prop.children.push(comp);
            }
        }
        return;
    }

    if name == "prop" && current.is_some() {
        *prop_depth = Some(depth);
        return;
    }

    // The response's own href, as opposed to one nested inside a property.
    if name == "href" && current.is_some() {
        *in_response_href = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn on_end(
    name: &str,
    stack: &mut Vec<String>,
    responses: &mut Vec<DavResponse>,
    current: &mut Option<DavResponse>,
    prop_depth: &mut Option<usize>,
    current_prop: &mut Option<(String, DavProp)>,
    in_response_href: &mut bool,
) {
    let depth = stack.len();

    if let Some(pd) = *prop_depth {
        if depth == pd + 1 {
            if let (Some((prop_name, prop)), Some(response)) =
                (current_prop.take(), current.as_mut())
            {
                // Several propstat blocks may report the same property, one
                // with a 200 and one with a 404. The populated one wins.
                let entry = response.props.entry(prop_name).or_default();
                if !prop.text.trim().is_empty() || !prop.children.is_empty() {
                    *entry = prop;
                }
            }
        } else if depth == pd && name == "prop" {
            *prop_depth = None;
        }
    }

    if name == "href" {
        *in_response_href = false;
    }

    if name == "response"
        && prop_depth.is_none()
        && let Some(response) = current.take()
    {
        responses.push(response);
    }

    stack.pop();
}

fn append_text(
    text: &str,
    current_prop: &mut Option<(String, DavProp)>,
    current: &mut Option<DavResponse>,
    in_response_href: bool,
) {
    if let Some((_, prop)) = current_prop.as_mut() {
        prop.text.push_str(text);
        return;
    }
    if in_response_href && let Some(response) = current.as_mut() {
        response.href.push_str(text);
    }
}

fn local_name(qualified: &[u8]) -> String {
    let name = String::from_utf8_lossy(qualified);
    match name.rsplit(':').next() {
        Some(local) => local.to_ascii_lowercase(),
        None => name.to_ascii_lowercase(),
    }
}

fn comp_name_attribute(element: &str, start: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    if element != "comp" {
        return None;
    }
    for attr in start.attributes().flatten() {
        if local_name(attr.key.as_ref()) == "name" {
            return Some(String::from_utf8_lossy(attr.value.as_ref()).into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME_SET: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/principals/users/ada/</D:href>
    <D:propstat>
      <D:prop>
        <C:calendar-home-set><D:href>/calendars/ada/</D:href></C:calendar-home-set>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

    const CALENDARS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"
             xmlns:I="http://apple.com/ns/ical/">
  <response>
    <href>/calendars/ada/work/</href>
    <propstat>
      <prop>
        <displayname>Work</displayname>
        <resourcetype><collection/><C:calendar/></resourcetype>
        <I:calendar-color>#FF5733</I:calendar-color>
        <C:supported-calendar-component-set>
          <C:comp name="VEVENT"/>
          <C:comp name="VTODO"/>
        </C:supported-calendar-component-set>
        <current-user-privilege-set>
          <privilege><read/></privilege>
          <privilege><write-content/></privilege>
        </current-user-privilege-set>
      </prop>
      <status>HTTP/1.1 200 OK</status>
    </propstat>
    <propstat>
      <prop><calendar-description/></prop>
      <status>HTTP/1.1 404 Not Found</status>
    </propstat>
  </response>
  <response>
    <href>/calendars/ada/</href>
    <propstat>
      <prop>
        <displayname>Home collection</displayname>
        <resourcetype><collection/></resourcetype>
      </prop>
      <status>HTTP/1.1 200 OK</status>
    </propstat>
  </response>
</multistatus>"#;

    #[test]
    fn reads_href_nested_in_a_property() {
        let responses = parse_multistatus(HOME_SET).unwrap();
        assert_eq!(responses.len(), 1);
        // The response's own href must not be confused with the one inside
        // calendar-home-set.
        assert_eq!(responses[0].href, "/principals/users/ada/");
        assert_eq!(
            responses[0].text("calendar-home-set"),
            Some("/calendars/ada/")
        );
    }

    #[test]
    fn distinguishes_calendars_from_plain_collections() {
        let responses = parse_multistatus(CALENDARS).unwrap();
        assert_eq!(responses.len(), 2);

        let work = &responses[0];
        assert_eq!(work.href, "/calendars/ada/work/");
        assert_eq!(work.text("displayname"), Some("Work"));
        assert!(work.prop("resourcetype").unwrap().has_child("calendar"));

        let home = &responses[1];
        assert!(!home.prop("resourcetype").unwrap().has_child("calendar"));
    }

    #[test]
    fn ignores_namespace_prefixes() {
        let responses = parse_multistatus(CALENDARS).unwrap();
        // calendar-color lives in the Apple namespace, displayname in DAV:.
        assert_eq!(responses[0].text("calendar-color"), Some("#FF5733"));
    }

    #[test]
    fn captures_comp_name_attributes() {
        let responses = parse_multistatus(CALENDARS).unwrap();
        let comps = responses[0]
            .prop("supported-calendar-component-set")
            .unwrap();
        assert!(comps.has_child("VEVENT"));
        assert!(comps.has_child("VTODO"));
    }

    #[test]
    fn collects_privileges() {
        let responses = parse_multistatus(CALENDARS).unwrap();
        let privileges = responses[0].prop("current-user-privilege-set").unwrap();
        assert!(privileges.has_child("write-content"));
    }

    #[test]
    fn empty_404_propstat_does_not_clobber_a_found_property() {
        let xml = r#"<multistatus xmlns="DAV:">
          <response>
            <href>/c/</href>
            <propstat><prop><displayname>Real</displayname></prop>
              <status>HTTP/1.1 200 OK</status></propstat>
            <propstat><prop><displayname/></prop>
              <status>HTTP/1.1 404 Not Found</status></propstat>
          </response>
        </multistatus>"#;
        let responses = parse_multistatus(xml).unwrap();
        assert_eq!(responses[0].text("displayname"), Some("Real"));
    }

    #[test]
    fn reads_calendar_data_and_etag() {
        let xml = "<multistatus xmlns=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\
          <response><href>/c/e.ics</href><propstat><prop>\
          <getetag>\"abc\"</getetag>\
          <C:calendar-data>BEGIN:VCALENDAR&#13;\nEND:VCALENDAR</C:calendar-data>\
          </prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>";
        let responses = parse_multistatus(xml).unwrap();
        assert_eq!(responses[0].text("getetag"), Some("\"abc\""));
        assert!(
            responses[0]
                .text("calendar-data")
                .unwrap()
                .starts_with("BEGIN:VCALENDAR")
        );
    }

    #[test]
    fn handles_cdata_wrapped_calendar_data() {
        let xml = "<multistatus xmlns=\"DAV:\"><response><href>/c/e.ics</href>\
          <propstat><prop><calendar-data><![CDATA[BEGIN:VCALENDAR]]></calendar-data></prop>\
          </propstat></response></multistatus>";
        let responses = parse_multistatus(xml).unwrap();
        assert_eq!(responses[0].text("calendar-data"), Some("BEGIN:VCALENDAR"));
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(parse_multistatus("<multistatus><response>").is_err());
    }

    #[test]
    fn empty_multistatus_yields_no_responses() {
        let xml = r#"<multistatus xmlns="DAV:"></multistatus>"#;
        assert!(parse_multistatus(xml).unwrap().is_empty());
    }
}
