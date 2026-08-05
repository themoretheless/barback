//! The RFC 5545 content-line layer: folding, parameters, and the component
//! tree. Nothing here knows what a VEVENT means.

use crate::integrations::error::{CalendarError, Result};

/// Maximum octets per line before folding, per RFC 5545 section 3.1.
const FOLD_LIMIT: usize = 75;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prop {
    /// Upper-cased property name, e.g. `DTSTART`.
    pub name: String,
    /// Upper-cased parameter names with their raw values.
    pub params: Vec<(String, String)>,
    /// Raw property value, still escaped.
    pub value: String,
}

impl Prop {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Prop {
            name: name.into().to_ascii_uppercase(),
            params: Vec::new(),
            value: value.into(),
        }
    }

    pub fn with_param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params
            .push((name.into().to_ascii_uppercase(), value.into()));
        self
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Property value with RFC 5545 TEXT escaping removed.
    pub fn text(&self) -> String {
        unescape_text(&self.value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// Upper-cased component name, e.g. `VEVENT`.
    pub name: String,
    pub props: Vec<Prop>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn new(name: impl Into<String>) -> Self {
        Component {
            name: name.into().to_ascii_uppercase(),
            props: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn prop(&self, name: &str) -> Option<&Prop> {
        self.props
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn prop_text(&self, name: &str) -> Option<String> {
        self.prop(name).map(|p| p.text()).filter(|s| !s.is_empty())
    }

    pub fn props_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> + 'a {
        self.props
            .iter()
            .filter(move |p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Component> + 'a {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn push_prop(&mut self, prop: Prop) {
        self.props.push(prop);
    }

    /// Depth-first search for the first component with the given name,
    /// including `self`.
    pub fn find(&self, name: &str) -> Option<&Component> {
        if self.name.eq_ignore_ascii_case(name) {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }
}

/// Joins folded continuation lines back into logical lines.
///
/// A continuation is any line beginning with a space or tab; the leading
/// whitespace octet is removed and the remainder appended to the previous line.
/// Handles CRLF, LF and lone CR line endings, since servers are inconsistent.
pub fn unfold(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in input.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            continue;
        }
        let is_continuation = line.starts_with(' ') || line.starts_with('\t');
        if is_continuation && let Some(last) = out.last_mut() {
            last.push_str(&line[1..]);
            continue;
        }
        // A continuation with nothing to continue: keep it rather than
        // silently dropping content.
        out.push(line.to_string());
    }
    out
}

/// Parses one already-unfolded content line.
pub fn parse_content_line(line: &str) -> Result<Prop> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    // Property name runs until ';' (parameters) or ':' (value).
    let name_start = i;
    while i < chars.len() && chars[i] != ';' && chars[i] != ':' {
        i += 1;
    }
    if i >= chars.len() {
        return Err(CalendarError::parse(
            "iCalendar content line",
            format!("missing ':' in {line:?}"),
        ));
    }
    let name: String = chars[name_start..i]
        .iter()
        .collect::<String>()
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(CalendarError::parse(
            "iCalendar content line",
            format!("empty property name in {line:?}"),
        ));
    }

    let mut params: Vec<(String, String)> = Vec::new();
    while chars[i] == ';' {
        i += 1; // consume ';'

        let pname_start = i;
        while i < chars.len() && chars[i] != '=' && chars[i] != ';' && chars[i] != ':' {
            i += 1;
        }
        let pname: String = chars[pname_start..i].iter().collect();

        let mut pvalue = String::new();
        if i < chars.len() && chars[i] == '=' {
            i += 1; // consume '='
            // A parameter may carry several comma-separated values; they are
            // rejoined with ',' because no caller here needs them split.
            loop {
                if i < chars.len() && chars[i] == '"' {
                    i += 1; // consume opening quote
                    while i < chars.len() && chars[i] != '"' {
                        pvalue.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        i += 1; // consume closing quote
                    }
                } else {
                    while i < chars.len() && chars[i] != ',' && chars[i] != ';' && chars[i] != ':' {
                        pvalue.push(chars[i]);
                        i += 1;
                    }
                }
                if i < chars.len() && chars[i] == ',' {
                    pvalue.push(',');
                    i += 1;
                    continue;
                }
                break;
            }
        }

        params.push((pname.to_ascii_uppercase(), pvalue));

        if i >= chars.len() {
            return Err(CalendarError::parse(
                "iCalendar content line",
                format!("parameters not terminated by ':' in {line:?}"),
            ));
        }
    }

    // chars[i] is ':' here.
    let value: String = chars[i + 1..].iter().collect();

    Ok(Prop {
        name: name.to_ascii_uppercase(),
        params,
        value,
    })
}

/// Parses a full iCalendar stream into its top-level components.
///
/// Unbalanced `END` lines and properties appearing outside any component are
/// rejected rather than guessed at, since both indicate a truncated response.
pub fn parse(input: &str) -> Result<Vec<Component>> {
    let mut roots: Vec<Component> = Vec::new();
    let mut stack: Vec<Component> = Vec::new();

    for line in unfold(input) {
        let prop = parse_content_line(&line)?;

        if prop.name == "BEGIN" {
            stack.push(Component::new(prop.value.trim()));
            continue;
        }

        if prop.name == "END" {
            let finished = stack.pop().ok_or_else(|| {
                CalendarError::parse("iCalendar", format!("unmatched END:{}", prop.value))
            })?;
            let expected = prop.value.trim().to_ascii_uppercase();
            if finished.name != expected {
                return Err(CalendarError::parse(
                    "iCalendar",
                    format!("END:{} closes BEGIN:{}", expected, finished.name),
                ));
            }
            match stack.last_mut() {
                Some(parent) => parent.children.push(finished),
                None => roots.push(finished),
            }
            continue;
        }

        match stack.last_mut() {
            Some(current) => current.props.push(prop),
            None => {
                return Err(CalendarError::parse(
                    "iCalendar",
                    format!("property {} outside any component", prop.name),
                ));
            }
        }
    }

    if let Some(open) = stack.last() {
        return Err(CalendarError::parse(
            "iCalendar",
            format!("unterminated component BEGIN:{}", open.name),
        ));
    }

    Ok(roots)
}

/// Serializes a component tree back to a CRLF-delimited iCalendar stream.
pub fn write(component: &Component) -> String {
    let mut out = String::new();
    write_component(component, &mut out);
    out
}

fn write_component(component: &Component, out: &mut String) {
    fold_into(&format!("BEGIN:{}", component.name), out);
    for prop in &component.props {
        let mut line = prop.name.clone();
        for (k, v) in &prop.params {
            line.push(';');
            line.push_str(k);
            line.push('=');
            if needs_quoting(v) {
                line.push('"');
                line.push_str(&v.replace('"', ""));
                line.push('"');
            } else {
                line.push_str(v);
            }
        }
        line.push(':');
        line.push_str(&prop.value);
        fold_into(&line, out);
    }
    for child in &component.children {
        write_component(child, out);
    }
    fold_into(&format!("END:{}", component.name), out);
}

fn needs_quoting(value: &str) -> bool {
    value.contains([':', ';', ','])
}

/// Appends a logical line, folded to `FOLD_LIMIT` octets, never splitting a
/// multi-byte character.
fn fold_into(line: &str, out: &mut String) {
    let mut used = 0usize;
    let mut first = true;
    for ch in line.chars() {
        let len = ch.len_utf8();
        // Continuation lines start with a space, which counts toward the limit.
        let limit = if first { FOLD_LIMIT } else { FOLD_LIMIT - 1 };
        if used + len > limit {
            out.push_str("\r\n ");
            used = 0;
            first = false;
        }
        out.push(ch);
        used += len;
    }
    out.push_str("\r\n");
}

/// Removes RFC 5545 TEXT escaping.
pub fn unescape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(';') => out.push(';'),
            Some(',') => out.push(','),
            // Unknown escape: keep the escaped character, drop the backslash.
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Applies RFC 5545 TEXT escaping.
pub fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfolds_continuation_lines() {
        let input = "DESCRIPTION:a long\r\n  description\r\nSUMMARY:hi\r\n";
        let lines = unfold(input);
        assert_eq!(lines, vec!["DESCRIPTION:a long description", "SUMMARY:hi"]);
    }

    #[test]
    fn unfolds_bare_lf_and_tabs() {
        let input = "DESCRIPTION:one\n\ttwo\n";
        assert_eq!(unfold(input), vec!["DESCRIPTION:onetwo"]);
    }

    #[test]
    fn parses_parameters() {
        let p = parse_content_line("DTSTART;TZID=Europe/Berlin;VALUE=DATE-TIME:20260803T120000")
            .unwrap();
        assert_eq!(p.name, "DTSTART");
        assert_eq!(p.param("TZID"), Some("Europe/Berlin"));
        assert_eq!(p.param("tzid"), Some("Europe/Berlin"));
        assert_eq!(p.param("VALUE"), Some("DATE-TIME"));
        assert_eq!(p.value, "20260803T120000");
    }

    #[test]
    fn parses_quoted_parameter_containing_delimiters() {
        let p =
            parse_content_line("ATTENDEE;CN=\"Doe, John; Jr\":mailto:john@example.com").unwrap();
        assert_eq!(p.param("CN"), Some("Doe, John; Jr"));
        assert_eq!(p.value, "mailto:john@example.com");
    }

    #[test]
    fn value_may_contain_colons() {
        let p = parse_content_line("URL:https://example.com/a:b").unwrap();
        assert_eq!(p.value, "https://example.com/a:b");
    }

    #[test]
    fn parameter_without_value_is_tolerated() {
        let p = parse_content_line("KEY;FLAG:value").unwrap();
        assert_eq!(p.param("FLAG"), Some(""));
        assert_eq!(p.value, "value");
    }

    #[test]
    fn line_without_colon_is_rejected() {
        assert!(parse_content_line("BROKEN").is_err());
    }

    #[test]
    fn parses_nested_components() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\n\
                     BEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let roots = parse(input).unwrap();
        assert_eq!(roots.len(), 1);
        let cal = &roots[0];
        assert_eq!(cal.name, "VCALENDAR");
        assert_eq!(cal.prop_text("VERSION").as_deref(), Some("2.0"));
        let event = &cal.children[0];
        assert_eq!(event.name, "VEVENT");
        assert_eq!(event.children[0].name, "VALARM");
    }

    #[test]
    fn mismatched_end_is_rejected() {
        let input = "BEGIN:VCALENDAR\r\nEND:VEVENT\r\n";
        assert!(parse(input).is_err());
    }

    #[test]
    fn unterminated_component_is_rejected() {
        let input = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n";
        assert!(parse(input).is_err());
    }

    #[test]
    fn property_outside_component_is_rejected() {
        assert!(parse("VERSION:2.0\r\n").is_err());
    }

    #[test]
    fn text_escaping_round_trips() {
        let original = "line one\nsemi; comma, back\\slash";
        assert_eq!(unescape_text(&escape_text(original)), original);
    }

    #[test]
    fn folds_long_lines_at_75_octets() {
        let mut c = Component::new("VEVENT");
        c.push_prop(Prop::new("DESCRIPTION", "x".repeat(200)));
        let out = write(&c);
        for line in out.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.len() <= 75, "line too long: {} octets", line.len());
        }
        // Unfolding must give the original property back.
        let reparsed = parse(&format!("BEGIN:VCALENDAR\r\n{}END:VCALENDAR\r\n", out)).unwrap();
        let event = reparsed[0].find("VEVENT").unwrap();
        assert_eq!(event.prop_text("DESCRIPTION").unwrap(), "x".repeat(200));
    }

    #[test]
    fn folding_never_splits_multibyte_characters() {
        let mut c = Component::new("VEVENT");
        c.push_prop(Prop::new("SUMMARY", "ы".repeat(120)));
        let out = write(&c);
        // Round-tripping proves no character was cut in half.
        let reparsed = parse(&format!("BEGIN:VCALENDAR\r\n{}END:VCALENDAR\r\n", out)).unwrap();
        let event = reparsed[0].find("VEVENT").unwrap();
        assert_eq!(event.prop_text("SUMMARY").unwrap(), "ы".repeat(120));
    }

    #[test]
    fn writer_quotes_parameters_with_delimiters() {
        let mut c = Component::new("VEVENT");
        c.push_prop(Prop::new("ATTENDEE", "mailto:a@b.c").with_param("CN", "Doe, John"));
        let out = write(&c);
        assert!(out.contains("CN=\"Doe, John\""));
        let reparsed = parse(&format!("BEGIN:VCALENDAR\r\n{}END:VCALENDAR\r\n", out)).unwrap();
        let attendee = reparsed[0]
            .find("VEVENT")
            .unwrap()
            .prop("ATTENDEE")
            .unwrap();
        assert_eq!(attendee.param("CN"), Some("Doe, John"));
    }
}
