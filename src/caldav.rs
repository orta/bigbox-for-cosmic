// SPDX-License-Identifier: MPL-2.0

//! CalDAV client, used to mirror booked classes into a calendar.
//!
//! This started life as a JMAP client, because JMAP is Fastmail's own JSON API
//! and needs no XML parser. That turned out not to be usable: Fastmail's
//! [developer docs][dev] say calendars are reachable "via CalDAV… we will be
//! opening up JMAP access as well, as soon as the specification is finalized",
//! [JMAP for Calendars][draft] is still a draft rather than an RFC, and a real
//! token against a real account confirmed it — the session advertises no
//! calendar capability at all. So CalDAV it is, which has the consolation of
//! being portable to iCloud, Nextcloud and anything else that speaks it.
//!
//! Things worth knowing before changing this:
//!
//! * **Discovery is three hops.** `current-user-principal` on a well-known
//!   path, then `calendar-home-set` on the principal, then the calendars
//!   themselves. Guessing the URL from the username works on Fastmail today
//!   and is exactly the kind of thing that breaks silently later.
//! * **The app owns the resource path.** Events live at `{calendar}/bigbox-{id}.ics`,
//!   so a `PUT` is create-or-update with no read first, and the file name alone
//!   says whether an event is this app's to touch.
//! * **Times go out as UTC.** Unlike JSCalendar, iCalendar has no way to pair a
//!   local time with a zone without also emitting a `VTIMEZONE` with the zone's
//!   full DST rules. Converting to UTC via the club's IANA zone is the
//!   unambiguous option, and the one that can't be an hour out.
//!
//! [dev]: https://www.fastmail.com/dev/
//! [draft]: https://jmap.io/spec/calendars-draft/

use crate::calendar::Event;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use quick_xml::Reader;
use quick_xml::events::Event as XmlEvent;
use std::collections::HashMap;

/// Fastmail's CalDAV endpoint. Discovery starts from here.
pub const FASTMAIL_BASE: &str = "https://caldav.fastmail.com";

/// Where a member creates the password this client needs.
pub const APP_PASSWORD_URL: &str = "https://app.fastmail.com/settings/security/apps";

/// The zone used when a club doesn't report one. The club's naive timestamps
/// have to be anchored to something, and defaulting to UTC would be an hour out
/// through British Summer Time.
const FALLBACK_TZ: Tz = chrono_tz::Europe::London;

// --- Errors ---

#[derive(Debug, Clone)]
pub enum CalDavError {
    /// The username / app password pair was rejected.
    InvalidCredentials,
    /// Authentication worked but no calendar collection could be found.
    NoCalendars,
    Other(String),
}

impl std::fmt::Display for CalDavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalDavError::InvalidCredentials => write!(
                f,
                "Fastmail rejected that username or password. CalDAV needs an app \
                 password, not your normal login."
            ),
            CalDavError::NoCalendars => {
                write!(f, "No calendars could be found on that account")
            }
            CalDavError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CalDavError {}

impl From<reqwest::Error> for CalDavError {
    fn from(e: reqwest::Error) -> Self {
        CalDavError::Other(e.to_string())
    }
}

// --- Calendars ---

#[derive(Debug, Clone)]
pub struct Calendar {
    /// Absolute path on the server, e.g. `/dav/calendars/user/you/abc123/`.
    pub href: String,
    pub name: String,
}

impl Calendar {
    pub fn display_name(&self) -> &str {
        &self.name
    }
}

/// What one sync actually changed, for the confirmation line in the dialog.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    pub created: usize,
    pub updated: usize,
    pub removed: usize,
}

impl SyncOutcome {
    pub fn is_empty(&self) -> bool {
        self.created == 0 && self.updated == 0 && self.removed == 0
    }
}

// --- Client ---

pub struct Client {
    http: reqwest::Client,
    base: String,
    username: String,
    password: String,
}

/// Hand-written for the same reason as [`crate::api::Client`]'s: a derived
/// `Debug` would print the password in full, and this client travels inside the
/// messages the GUI routes and formats.
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: FASTMAIL_BASE.to_string(),
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    /// Sends a WebDAV request and returns the body, mapping the two status
    /// codes that mean something specific.
    async fn dav(
        &self,
        method: &str,
        url: &str,
        depth: Option<&str>,
        body: Option<String>,
    ) -> Result<String, CalDavError> {
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| CalDavError::Other(e.to_string()))?;

        let mut request = self
            .http
            .request(method, url)
            .basic_auth(&self.username, Some(&self.password));

        if let Some(depth) = depth {
            request = request.header("Depth", depth);
        }
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(body);
        }

        let response = request.send().await.map_err(CalDavError::from)?;
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(CalDavError::InvalidCredentials);
        }
        if !status.is_success() && status != reqwest::StatusCode::MULTI_STATUS {
            return Err(CalDavError::Other(format!("Server returned {status}")));
        }

        response.text().await.map_err(CalDavError::from)
    }

    fn url_for(&self, href: &str) -> String {
        if href.starts_with("http://") || href.starts_with("https://") {
            href.to_string()
        } else {
            format!("{}{href}", self.base)
        }
    }

    /// Every calendar on the account that can hold events.
    ///
    /// Discovery rather than a guessed URL: the principal path and the calendar
    /// home are both the server's to decide, and Fastmail's happen to be
    /// predictable only until they aren't.
    pub async fn calendars(&self) -> Result<Vec<Calendar>, CalDavError> {
        let principal = self.discover_principal().await?;
        let home = self.discover_home(&principal).await?;

        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
    <c:supported-calendar-component-set/>
    <d:current-user-privilege-set/>
  </d:prop>
</d:propfind>"#;

        let xml = self
            .dav(
                "PROPFIND",
                &self.url_for(&home),
                Some("1"),
                Some(body.to_string()),
            )
            .await?;

        let calendars: Vec<Calendar> = parse_multistatus(&xml)
            .into_iter()
            .filter(|response| response.is_calendar && response.holds_events() && response.writable)
            .map(|response| Calendar {
                name: response.display_name(),
                href: response.href,
            })
            .collect();

        if calendars.is_empty() {
            return Err(CalDavError::NoCalendars);
        }
        Ok(calendars)
    }

    async fn discover_principal(&self) -> Result<String, CalDavError> {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop><d:current-user-principal/></d:prop>
</d:propfind>"#;

        let xml = self
            .dav(
                "PROPFIND",
                &format!("{}/dav/principals/", self.base),
                Some("0"),
                Some(body.to_string()),
            )
            .await?;

        parse_multistatus(&xml)
            .into_iter()
            .find_map(|response| response.current_user_principal)
            .ok_or_else(|| CalDavError::Other("Could not find the account principal".to_string()))
    }

    async fn discover_home(&self, principal: &str) -> Result<String, CalDavError> {
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><c:calendar-home-set/></d:prop>
</d:propfind>"#;

        let xml = self
            .dav(
                "PROPFIND",
                &self.url_for(principal),
                Some("0"),
                Some(body.to_string()),
            )
            .await?;

        parse_multistatus(&xml)
            .into_iter()
            .find_map(|response| response.calendar_home)
            .ok_or_else(|| CalDavError::Other("Could not find the calendar home".to_string()))
    }

    /// Makes the calendar match `desired`, from `after` onwards.
    ///
    /// Only events this app wrote are considered, and only from `after` —
    /// classes that have finished fall outside the window on both sides at
    /// once, so they're never touched and stay as a record of what was
    /// attended. Nothing the member created themselves is ever looked at.
    pub async fn sync(
        &self,
        calendar_href: &str,
        desired: &[Event],
        after: DateTime<Utc>,
    ) -> Result<SyncOutcome, CalDavError> {
        let existing = self.existing_resources(calendar_href, after).await?;

        let mut outcome = SyncOutcome::default();

        for event in desired {
            let name = event.resource_name();
            let url = self.url_for(&join(calendar_href, &name));

            // A PUT is create-or-update, so an event that moved studio or time
            // is corrected in place without reading it back first.
            self.put_event(&url, event).await?;

            if existing.contains_key(&name) {
                outcome.updated += 1;
            } else {
                outcome.created += 1;
            }
        }

        let wanted: std::collections::HashSet<String> =
            desired.iter().map(Event::resource_name).collect();

        for (name, href) in &existing {
            if wanted.contains(name) {
                continue;
            }
            self.dav("DELETE", &self.url_for(href), None, None).await?;
            outcome.removed += 1;
        }

        Ok(outcome)
    }

    async fn put_event(&self, url: &str, event: &Event) -> Result<(), CalDavError> {
        let response = self
            .http
            .put(url)
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "text/calendar; charset=utf-8")
            .body(to_icalendar(event))
            .send()
            .await
            .map_err(CalDavError::from)?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(CalDavError::InvalidCredentials);
        }
        if !status.is_success() {
            return Err(CalDavError::Other(format!(
                "Could not write \"{}\" to the calendar: {status}",
                event.title
            )));
        }
        Ok(())
    }

    /// Resource name to href, for this app's own unfinished events.
    ///
    /// A `time-range` with only a start matches events that overlap it — i.e.
    /// ones that haven't ended — which is the same cutoff the desired set uses.
    async fn existing_resources(
        &self,
        calendar_href: &str,
        after: DateTime<Utc>,
    ) -> Result<HashMap<String, String>, CalDavError> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/></d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VEVENT">
        <c:time-range start="{}"/>
      </c:comp-filter>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>"#,
            after.format("%Y%m%dT%H%M%SZ")
        );

        let xml = self
            .dav(
                "REPORT",
                &self.url_for(calendar_href),
                Some("1"),
                Some(body),
            )
            .await?;

        Ok(parse_multistatus(&xml)
            .into_iter()
            .filter_map(|response| {
                let name = file_name(&response.href)?;
                Event::is_ours(&name).then_some((name, response.href))
            })
            .collect())
    }
}

// --- iCalendar ---

/// Renders one event as a complete `VCALENDAR`.
pub fn to_icalendar(event: &Event) -> String {
    let zone: Tz = event.time_zone.parse().unwrap_or(FALLBACK_TZ);

    // A local time can be ambiguous (the hour that repeats when the clocks go
    // back) or non-existent (the hour that's skipped when they go forward).
    // Neither should lose the event, so pick the earlier reading.
    let start = zone
        .from_local_datetime(&event.start)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&event.start));
    let end = start + chrono::Duration::minutes(event.minutes());

    let status = if event.tentative {
        "TENTATIVE"
    } else {
        "CONFIRMED"
    };
    // A waiting-list place shouldn't make the member look busy to anyone
    // checking their availability.
    let transparency = if event.tentative {
        "TRANSPARENT"
    } else {
        "OPAQUE"
    };

    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//orta//bigbox-for-cosmic//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}", escape(&event.uid)),
        format!("DTSTAMP:{}", Utc::now().format("%Y%m%dT%H%M%SZ")),
        format!("DTSTART:{}", start.format("%Y%m%dT%H%M%SZ")),
        format!("DTEND:{}", end.format("%Y%m%dT%H%M%SZ")),
        format!("SUMMARY:{}", escape(&event.title)),
        format!("STATUS:{status}"),
        format!("TRANSP:{transparency}"),
    ];

    if !event.description.is_empty() {
        lines.push(format!("DESCRIPTION:{}", escape(&event.description)));
    }
    if let Some(location) = &event.location {
        lines.push(format!("LOCATION:{}", escape(location)));
    }

    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());

    let folded: Vec<String> = lines.iter().map(|line| fold(line)).collect();
    format!("{}\r\n", folded.join("\r\n"))
}

/// Escapes the characters iCalendar gives meaning to. Order matters —
/// backslashes first, or the escapes introduced below would be escaped again.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}

/// Folds a content line to 75 octets, per RFC 5545.
///
/// The split has to fall on a character boundary, not just a byte one, or a
/// folded line can cut a multi-byte character in half — which matters here
/// because class locations carry an em dash.
fn fold(line: &str) -> String {
    const LIMIT: usize = 75;
    if line.len() <= LIMIT {
        return line.to_string();
    }

    let mut out = String::with_capacity(line.len() + line.len() / LIMIT * 3);
    let mut used = 0;
    // The first line gets the full width; continuations lose one octet to the
    // leading space that marks them.
    let mut budget = LIMIT;

    for character in line.chars() {
        let width = character.len_utf8();
        if used + width > budget {
            out.push_str("\r\n ");
            used = 0;
            budget = LIMIT - 1;
        }
        out.push(character);
        used += width;
    }
    out
}

// --- WebDAV XML ---

/// One `<response>` from a multistatus body, flattened to what this app reads.
#[derive(Debug, Default)]
struct DavResponse {
    href: String,
    displayname: Option<String>,
    is_calendar: bool,
    current_user_principal: Option<String>,
    calendar_home: Option<String>,
    /// Which `VCOMPONENT`s the collection accepts, when it says.
    components: Vec<String>,
    /// Assumed true unless the server returns a privilege set that withholds
    /// writing — a read-only subscription like a holiday feed.
    writable: bool,
}

impl DavResponse {
    fn display_name(&self) -> String {
        match self.displayname.as_deref() {
            Some(name) if !name.trim().is_empty() => name.to_string(),
            _ => file_name(&self.href).unwrap_or_else(|| "Calendar".to_string()),
        }
    }

    /// A calendar that only takes to-dos or journals is no use for classes.
    /// Servers that don't advertise the set at all are given the benefit of the
    /// doubt.
    fn holds_events(&self) -> bool {
        self.components.is_empty() || self.components.iter().any(|c| c == "VEVENT")
    }
}

/// Extracts the `<response>` elements from a WebDAV multistatus body.
///
/// Namespace prefixes vary between servers (`d:`, `D:`, none at all), so
/// everything is matched on local name.
fn parse_multistatus(xml: &str) -> Vec<DavResponse> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut responses = Vec::new();
    let mut current: Option<DavResponse> = None;
    let mut stack: Vec<String> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(XmlEvent::Start(element)) => {
                let name = local_name(element.name().as_ref());
                if name == "response" {
                    current = Some(DavResponse {
                        writable: true,
                        ..Default::default()
                    });
                }
                if let Some(response) = current.as_mut() {
                    note_element(response, &name, &stack, &element, true);
                }
                stack.push(name);
            }

            // Self-closing elements carry meaning here: `<d:collection/>`,
            // `<c:calendar/>`, `<d:write/>` and `<c:comp name="VEVENT"/>` all
            // arrive this way.
            Ok(XmlEvent::Empty(element)) => {
                let name = local_name(element.name().as_ref());
                if let Some(response) = current.as_mut() {
                    note_element(response, &name, &stack, &element, false);
                }
            }

            Ok(XmlEvent::Text(text)) => {
                if let Some(response) = current.as_mut()
                    && let Ok(raw) = text.decode()
                {
                    note_text(response, &stack, &raw);
                }
            }

            // Fastmail wraps every `displayname` in a CDATA section, which
            // quick-xml reports as its own event rather than as text. Handling
            // only `Text` reads those names as absent, and every calendar comes
            // out named after the UUID in its URL.
            Ok(XmlEvent::CData(data)) => {
                if let Some(response) = current.as_mut()
                    && let Ok(raw) = data.decode()
                {
                    note_text(response, &stack, &raw);
                }
            }

            Ok(XmlEvent::End(element)) => {
                let name = local_name(element.name().as_ref());
                stack.pop();
                if name == "response"
                    && let Some(response) = current.take()
                {
                    responses.push(response);
                }
            }

            Ok(XmlEvent::Eof) => break,
            // A malformed body yields whatever was parsed rather than an error
            // — the caller's own "found nothing" path is a better message than
            // an XML diagnostic would be.
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    responses
}

/// Files a run of character data against whichever element is open.
///
/// Shared by the text and CDATA arms: which of the two a server uses is its own
/// business, and the meaning is identical.
fn note_text(response: &mut DavResponse, stack: &[String], raw: &str) {
    let value = raw.trim();
    if value.is_empty() {
        return;
    }

    // An href means different things by where it sits: the response's own, the
    // principal's, or the calendar home's.
    match stack.last().map(String::as_str) {
        Some("href") => match stack.iter().rev().nth(1).map(String::as_str) {
            Some("current-user-principal") => {
                response.current_user_principal = Some(value.to_string());
            }
            Some("calendar-home-set") => response.calendar_home = Some(value.to_string()),
            _ => {
                if response.href.is_empty() {
                    response.href = value.to_string();
                }
            }
        },
        Some("displayname") => response.displayname = Some(value.to_string()),
        _ => {}
    }
}

fn note_element(
    response: &mut DavResponse,
    name: &str,
    stack: &[String],
    element: &quick_xml::events::BytesStart<'_>,
    has_children: bool,
) {
    let inside = |ancestor: &str| stack.iter().any(|parent| parent == ancestor);

    match name {
        "calendar" if inside("resourcetype") => response.is_calendar = true,
        "comp" => {
            if let Some(value) = attribute(element, "name") {
                response.components.push(value);
            }
        }
        // A privilege set is only authoritative when it actually contains
        // privileges. A server answers a PROPFIND across several propstats —
        // what it has under a 200, what it doesn't under a 404 — and echoes the
        // unanswered properties back as *empty* elements. Reading one of those
        // echoes as "no write access" hides a perfectly writable calendar.
        "current-user-privilege-set" if has_children => response.writable = false,
        // Privileges arrive as empty elements nested inside `<privilege>`.
        "write" | "write-content" | "all" if inside("current-user-privilege-set") => {
            response.writable = true;
        }
        _ => {}
    }
}

fn attribute(element: &quick_xml::events::BytesStart<'_>, wanted: &str) -> Option<String> {
    element.attributes().flatten().find_map(|attribute| {
        (local_name(attribute.key.as_ref()) == wanted)
            .then(|| String::from_utf8_lossy(&attribute.value).to_string())
    })
}

/// Strips any namespace prefix and lowercases, so `D:href`, `d:href` and `href`
/// all compare equal.
fn local_name(raw: &[u8]) -> String {
    let name = String::from_utf8_lossy(raw);
    name.rsplit(':').next().unwrap_or(&name).to_lowercase()
}

/// The last path segment of an href, ignoring a trailing slash.
fn file_name(href: &str) -> Option<String> {
    let trimmed = href.trim_end_matches('/');
    let name = trimmed.rsplit('/').next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Joins a collection href to a resource name without doubling the slash.
fn join(collection: &str, name: &str) -> String {
    format!("{}/{name}", collection.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn event() -> Event {
        Event {
            uid: Event::uid_for("/bigbox/class_events/4471"),
            title: "Body Pump".to_string(),
            description: "Strength & Weights".to_string(),
            location: Some("BigBox — Studio 1".to_string()),
            start: NaiveDate::from_ymd_opt(2026, 8, 20)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
            duration_minutes: 45,
            tentative: false,
            time_zone: "Europe/London".to_string(),
        }
    }

    /// August is BST, so 18:30 club-local is 17:30 UTC. Getting this wrong is
    /// the single most likely way for the whole feature to be quietly useless.
    #[test]
    fn local_times_convert_to_utc_across_dst() {
        let ics = to_icalendar(&event());
        assert!(ics.contains("DTSTART:20260820T173000Z"), "{ics}");
        assert!(ics.contains("DTEND:20260820T181500Z"), "{ics}");

        // January is GMT, so the same wall-clock time is its own UTC value.
        let mut winter = event();
        winter.start = NaiveDate::from_ymd_opt(2026, 1, 20)
            .unwrap()
            .and_hms_opt(18, 30, 0)
            .unwrap();
        let ics = to_icalendar(&winter);
        assert!(ics.contains("DTSTART:20260120T183000Z"), "{ics}");
    }

    #[test]
    fn waiting_list_places_are_tentative_and_transparent() {
        let mut event = event();
        event.tentative = true;
        let ics = to_icalendar(&event);
        assert!(ics.contains("STATUS:TENTATIVE"));
        assert!(ics.contains("TRANSP:TRANSPARENT"));
    }

    #[test]
    fn resource_names_avoid_characters_servers_encode_differently() {
        let name = event().resource_name();
        assert_eq!(name, "bigbox-4471.ics");
        assert!(!name.contains('@'), "an @ would be percent-encoded by some servers");
        assert!(Event::is_ours(&name));
        assert!(!Event::is_ours("someone-elses-event.ics"));
    }

    #[test]
    fn special_characters_are_escaped() {
        let mut event = event();
        event.description = "Legs; bums, and tums\nSecond line".to_string();
        let ics = to_icalendar(&event);
        assert!(ics.contains("Legs\\; bums\\, and tums\\nSecond line"), "{ics}");
    }

    /// A long line has to fold on a character boundary — the location carries
    /// an em dash, and splitting it mid-byte produces invalid UTF-8.
    #[test]
    fn long_lines_fold_without_splitting_characters() {
        let mut event = event();
        event.location = Some("BigBox Leisure Club — the very long studio name ————————— end".to_string());
        let ics = to_icalendar(&event);

        for line in ics.split("\r\n") {
            assert!(line.len() <= 75, "line too long: {line:?}");
        }
        // Folding is only a transport concern: unfolding must give it back.
        let unfolded = ics.replace("\r\n ", "");
        assert!(unfolded.contains("————————— end"), "{unfolded}");
    }

    #[test]
    fn finds_the_principal_and_home() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/principals/</D:href>
    <D:propstat><D:prop>
      <D:current-user-principal><D:href>/dav/principals/user/you@example.com/</D:href></D:current-user-principal>
    </D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;
        let parsed = parse_multistatus(xml);
        assert_eq!(
            parsed[0].current_user_principal.as_deref(),
            Some("/dav/principals/user/you@example.com/")
        );
        // The response's own href must not be mistaken for the principal's.
        assert_eq!(parsed[0].href, "/dav/principals/");
    }

    #[test]
    fn picks_out_writable_event_calendars() {
        let xml = r#"<?xml version="1.0"?>
<multistatus xmlns="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <response>
    <href>/dav/calendars/user/you/personal/</href>
    <propstat><prop>
      <displayname>Personal</displayname>
      <resourcetype><collection/><c:calendar/></resourcetype>
      <c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set>
      <current-user-privilege-set><privilege><write/></privilege></current-user-privilege-set>
    </prop></propstat>
  </response>
  <response>
    <href>/dav/calendars/user/you/tasks/</href>
    <propstat><prop>
      <displayname>Tasks</displayname>
      <resourcetype><collection/><c:calendar/></resourcetype>
      <c:supported-calendar-component-set><c:comp name="VTODO"/></c:supported-calendar-component-set>
      <current-user-privilege-set><privilege><write/></privilege></current-user-privilege-set>
    </prop></propstat>
  </response>
  <response>
    <href>/dav/calendars/user/you/holidays/</href>
    <propstat><prop>
      <displayname>UK Holidays</displayname>
      <resourcetype><collection/><c:calendar/></resourcetype>
      <c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set>
      <current-user-privilege-set><privilege><read/></privilege></current-user-privilege-set>
    </prop></propstat>
  </response>
  <response>
    <href>/dav/calendars/user/you/</href>
    <propstat><prop>
      <resourcetype><collection/></resourcetype>
    </prop></propstat>
  </response>
</multistatus>"#;

        let usable: Vec<String> = parse_multistatus(xml)
            .into_iter()
            .filter(|r| r.is_calendar && r.holds_events() && r.writable)
            .map(|r| r.display_name())
            .collect();

        // Not the to-do list, not the read-only feed, not the plain collection.
        assert_eq!(usable, vec!["Personal".to_string()]);
    }

    #[test]
    fn only_this_apps_resources_are_matched() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response><D:href>/dav/calendars/user/you/personal/bigbox-4471.ics</D:href></D:response>
  <D:response><D:href>/dav/calendars/user/you/personal/dentist-appointment.ics</D:href></D:response>
</D:multistatus>"#;

        let ours: Vec<String> = parse_multistatus(xml)
            .into_iter()
            .filter_map(|r| {
                let name = file_name(&r.href)?;
                Event::is_ours(&name).then_some(name)
            })
            .collect();

        assert_eq!(ours, vec!["bigbox-4471.ics".to_string()]);
    }

    /// Servers split a PROPFIND across propstats: what they have under a 200,
    /// what they don't under a 404, with the missing ones echoed back as empty
    /// elements. Those echoes must not be read as answers.
    #[test]
    fn empty_property_echoes_in_a_404_propstat_are_ignored() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <response>
    <href>/dav/calendars/user/you/1a2b3c/</href>
    <propstat>
      <prop>
        <displayname>Puzzmo Team Calendar</displayname>
        <resourcetype><collection/><C:calendar/></resourcetype>
        <current-user-privilege-set><privilege><write/></privilege></current-user-privilege-set>
      </prop>
      <status>HTTP/1.1 200 OK</status>
    </propstat>
    <propstat>
      <prop>
        <C:supported-calendar-component-set/>
      </prop>
      <status>HTTP/1.1 404 Not Found</status>
    </propstat>
  </response>
</multistatus>"#;

        let parsed = parse_multistatus(xml);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].display_name(), "Puzzmo Team Calendar");
        assert!(parsed[0].is_calendar);
        assert!(
            parsed[0].writable,
            "an unanswered privilege set must not read as read-only"
        );
    }

    /// The same, but with the privilege set itself unanswered — the common
    /// shape when a server reports rights only on some collections.
    #[test]
    fn an_unanswered_privilege_set_does_not_hide_a_calendar() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <response>
    <href>/dav/calendars/user/you/1a2b3c/</href>
    <propstat>
      <prop>
        <displayname>Calendar</displayname>
        <resourcetype><collection/><C:calendar/></resourcetype>
      </prop>
      <status>HTTP/1.1 200 OK</status>
    </propstat>
    <propstat>
      <prop>
        <current-user-privilege-set/>
      </prop>
      <status>HTTP/1.1 404 Not Found</status>
    </propstat>
  </response>
</multistatus>"#;

        let usable: Vec<String> = parse_multistatus(xml)
            .into_iter()
            .filter(|r| r.is_calendar && r.holds_events() && r.writable)
            .map(|r| r.display_name())
            .collect();

        assert_eq!(usable, vec!["Calendar".to_string()]);
    }

    /// Trimmed from a real Fastmail response. Two things here are not guesses
    /// and were both got wrong first time: names arrive as CDATA rather than
    /// text, and the account carries several collections that look calendar-ish
    /// but must not be offered.
    #[test]
    fn reads_a_real_fastmail_calendar_home() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:CY="http://cyrusimap.org/ns/">
  <d:response>
    <d:href>/dav/calendars/user/you/</d:href>
    <d:propstat><d:prop>
      <d:displayname><![CDATA[Your Name]]></d:displayname>
      <d:resourcetype><d:collection/></d:resourcetype>
      <d:current-user-privilege-set><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
    </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
    <d:propstat><d:prop>
      <c:supported-calendar-component-set/>
    </d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/calendars/user/you/500d9339-2886-44a1-91ba-f1c3933f15c0/</d:href>
    <d:propstat><d:prop>
      <d:displayname><![CDATA[Orta Main Calendar]]></d:displayname>
      <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
      <c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set>
      <d:current-user-privilege-set>
        <d:privilege><d:all/></d:privilege>
        <d:privilege><d:read/></d:privilege>
        <d:privilege><d:write/></d:privilege>
        <d:privilege><CY:admin/></d:privilege>
      </d:current-user-privilege-set>
    </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/calendars/user/you/326494EF-4EB7-44B6-ACE9-52A72FAFBA83/</d:href>
    <d:propstat><d:prop>
      <d:displayname><![CDATA[DEFAULT_TASK_CALENDAR_NAME]]></d:displayname>
      <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
      <c:supported-calendar-component-set><c:comp name="VTODO"/></c:supported-calendar-component-set>
      <d:current-user-privilege-set><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
    </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/calendars/user/you/Inbox/</d:href>
    <d:propstat><d:prop>
      <d:displayname><![CDATA[Inbox]]></d:displayname>
      <d:resourcetype><d:collection/><c:schedule-inbox/></d:resourcetype>
      <c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set>
      <d:current-user-privilege-set><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
    </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
  <d:response>
    <d:href>/dav/calendars/user/you/e55d6bc5-c32e-49ed-b515-4a7a8cce02e7/</d:href>
    <d:propstat><d:prop>
      <d:displayname><![CDATA[Contacts]]></d:displayname>
      <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
      <c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set>
      <d:current-user-privilege-set>
        <d:privilege><d:read/></d:privilege>
        <d:privilege><CY:write-properties-resource/></d:privilege>
        <d:privilege><CY:admin/></d:privilege>
      </d:current-user-privilege-set>
    </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  </d:response>
</d:multistatus>"#;

        let offered: Vec<String> = parse_multistatus(xml)
            .into_iter()
            .filter(|r| r.is_calendar && r.holds_events() && r.writable)
            .map(|r| r.display_name())
            .collect();

        // The real name, not the UUID from the href.
        assert_eq!(offered, vec!["Orta Main Calendar".to_string()]);

        // And nothing else: not the home collection, not the to-do list, not
        // the scheduling inbox, and not a calendar with no write privilege.
        assert!(!offered.iter().any(|name| name.contains("TASK")));
        assert!(!offered.contains(&"Inbox".to_string()));
        assert!(!offered.contains(&"Contacts".to_string()));
    }

    #[test]
    fn hrefs_join_without_doubling_slashes() {
        assert_eq!(join("/dav/cal/", "a.ics"), "/dav/cal/a.ics");
        assert_eq!(join("/dav/cal", "a.ics"), "/dav/cal/a.ics");
    }
}
