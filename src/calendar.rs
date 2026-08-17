// SPDX-License-Identifier: MPL-2.0

//! Turns the member's bookings into the set of events their calendar should
//! hold.
//!
//! This is deliberately a pure function of the bookings and the directory: it
//! describes the *desired* state and knows nothing about what's already on the
//! calendar or how it gets there. [`crate::fastmail::Client::sync`] does the
//! diffing.

use crate::api::{Attendee, ClassEvent, Directory};
use crate::categories;
use chrono::NaiveDateTime;

/// Marks an event as this app's, so a sync only ever touches what it wrote.
/// Anything else in the chosen calendar is left strictly alone.
const UID_PREFIX: &str = "bigbox-";
const UID_SUFFIX: &str = "@bigbox-for-cosmic";

/// One class, as it should appear on the calendar.
///
/// Deliberately transport-neutral: this is the app's desired state, and it's
/// [`crate::caldav`] that knows how to render it as iCalendar and reconcile it
/// against a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Derived from the class IRI, so the same class produces the same event
    /// on every sync rather than a duplicate.
    pub uid: String,
    pub title: String,
    pub description: String,
    pub location: Option<String>,
    /// Wall-clock local time in [`Event::time_zone`], carried through from the
    /// API without conversion.
    pub start: NaiveDateTime,
    pub duration_minutes: i64,
    /// A waiting-list place rather than a seat. Written as tentative so
    /// calendar apps render it unconfirmed, and as free so it doesn't block
    /// the slot out.
    pub tentative: bool,
    /// IANA zone the club's naive timestamps are expressed in.
    pub time_zone: String,
}

impl Event {
    /// Namespaces a class IRI into a calendar UID.
    pub fn uid_for(class_id: &str) -> String {
        format!("{UID_PREFIX}{}{UID_SUFFIX}", tail_of(class_id))
    }

    /// The file this event occupies in a CalDAV collection.
    ///
    /// Kept to plain ASCII rather than reusing the UID, whose `@` would be
    /// percent-encoded by some servers and not others — which would make
    /// matching a returned href back to an event unreliable.
    pub fn resource_name(&self) -> String {
        let id = self
            .uid
            .strip_suffix(UID_SUFFIX)
            .unwrap_or(&self.uid);
        format!("{id}.ics")
    }

    /// Whether a resource name seen on the server was written by this app.
    pub fn is_ours(resource_name: &str) -> bool {
        resource_name.starts_with(UID_PREFIX) && resource_name.ends_with(".ics")
    }

    /// A class with no usable end time still deserves to land on the calendar,
    /// so this falls back to an hour rather than dropping it.
    pub fn minutes(&self) -> i64 {
        if self.duration_minutes > 0 {
            self.duration_minutes
        } else {
            60
        }
    }
}

/// IRIs are `/bigbox/class_events/4471`; the trailing id alone is unique and
/// keeps the UID readable.
fn tail_of(iri: &str) -> &str {
    iri.rsplit('/').next().unwrap_or(iri)
}

/// The club's timestamps are naive wall-clock times, so an event is only
/// unambiguous once paired with the club's zone. Clubs do report one, but a
/// missing value would otherwise silently become UTC — an hour out for half the
/// year — so it falls back to where the club actually is.
const FALLBACK_TIMEZONE: &str = "Europe/London";

/// Every class the member holds a place on that hasn't finished yet, as
/// calendar events.
///
/// Waiting-list places are included and marked [`Event::tentative`]; the club
/// auto-promotes, so a queued class is a genuine "this might be happening"
/// rather than noise. Cancelled bookings and cancelled classes are left out.
///
/// The cutoff is the class's *end*, not its start, and that matters: the sync
/// deletes anything the server still holds in this window but this list
/// doesn't name, and the server's own filter is likewise "hasn't ended yet".
/// Cutting at the start time instead would leave a class that's under way
/// visible to the server but absent here, and it would be deleted from the
/// calendar halfway through. Classes that have genuinely finished fall out of
/// both sides at once, so they're never touched again and stay as a record.
pub fn desired_events(
    bookings: &[Attendee],
    directory: &Directory,
    now: NaiveDateTime,
) -> Vec<Event> {
    let mut events: Vec<Event> = bookings
        .iter()
        .filter(|attendee| attendee.is_active())
        .filter_map(|attendee| {
            let class = attendee.class_event.as_deref()?;
            let start = class.start()?;
            // A class with no end time is judged on its start instead.
            let end = class.end().unwrap_or(start);
            if class.is_cancelled() || end <= now {
                return None;
            }
            Some(event_for(class, directory, start, attendee.is_queued()))
        })
        .collect();

    // Sorted so an unchanged set of bookings produces an identical list every
    // time, which is what lets the app skip a redundant round trip.
    events.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.uid.cmp(&b.uid)));
    events
}

fn event_for(
    class: &ClassEvent,
    directory: &Directory,
    start: NaiveDateTime,
    queued: bool,
) -> Event {
    let name = directory.activity_name(class).unwrap_or("Class");

    // The category is worth carrying over: "Mind & Body" says more in a week
    // view than "Reformer" does.
    let mut details = vec![categories::lookup(name).category.label().to_string()];
    if let Some(coach) = directory.coach_name(class) {
        details.push(coach);
    }
    if let Some(studio) = directory.studio_name(class) {
        details.push(studio.to_string());
    }

    let mut description = details.join(" · ");
    if queued {
        description.push_str("\n\nOn the waiting list — you'll be moved up automatically if a place frees up.");
    }

    Event {
        uid: Event::uid_for(&class.id),
        title: if queued {
            format!("{name} (waiting list)")
        } else {
            name.to_string()
        },
        description,
        location: location_for(class, directory),
        start,
        duration_minutes: class.duration_minutes().unwrap_or_default(),
        tentative: queued,
        time_zone: timezone_for(class, directory),
    }
}

/// Club name, narrowed to the studio when there is one — enough to navigate by
/// without repeating the club on every line.
fn location_for(class: &ClassEvent, directory: &Directory) -> Option<String> {
    let club = directory.club_name(class);
    let studio = directory.studio_name(class);

    match (club, studio) {
        (Some(club), Some(studio)) => Some(format!("{club} — {studio}")),
        (Some(club), None) => Some(club.to_string()),
        (None, Some(studio)) => Some(studio.to_string()),
        (None, None) => None,
    }
}

fn timezone_for(class: &ClassEvent, directory: &Directory) -> String {
    class
        .club
        .as_deref()
        .and_then(|iri| directory.clubs.get(iri))
        .and_then(|club| club.timezone.as_deref())
        .filter(|zone| !zone.trim().is_empty())
        .unwrap_or(FALLBACK_TIMEZONE)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Activity, Club};
    use chrono::NaiveDate;

    fn at(day: u32, hour: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 8, day)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
    }

    fn now() -> NaiveDateTime {
        at(17, 12)
    }

    fn directory() -> Directory {
        let mut directory = Directory::default();
        directory.activities.insert(
            "/bigbox/activities/1".to_string(),
            Activity {
                id: "/bigbox/activities/1".to_string(),
                name: Some("Body Pump".to_string()),
                color_hex: None,
                is_bookable: true,
            },
        );
        directory.clubs.insert(
            "/bigbox/clubs/270".to_string(),
            Club {
                id: "/bigbox/clubs/270".to_string(),
                name: Some("BigBox".to_string()),
                timezone: Some("Europe/London".to_string()),
                locale: None,
            },
        );
        directory
    }

    fn class(id: &str, start: NaiveDateTime, end: NaiveDateTime) -> ClassEvent {
        ClassEvent {
            id: id.to_string(),
            club: Some("/bigbox/clubs/270".to_string()),
            studio: None,
            activity: Some("/bigbox/activities/1".to_string()),
            coach: None,
            attending_limit: None,
            online_limit: None,
            queue_limit: None,
            attendee_remaining: None,
            online_attendee_remaining: None,
            queue_remaining: None,
            started_at: Some(start.format("%Y-%m-%dT%H:%M:%S").to_string()),
            ended_at: Some(end.format("%Y-%m-%dT%H:%M:%S").to_string()),
            coach_available: false,
            summary: None,
            description: None,
            instructions_comment: None,
            booked_attendees: Vec::new(),
            queued_attendees: Vec::new(),
            deleted_at: None,
        }
    }

    fn booking(id: &str, state: &str, class: ClassEvent) -> Attendee {
        Attendee {
            id: id.to_string(),
            contact_id: Some("/bigbox/contacts/1".to_string()),
            state: Some(state.to_string()),
            class_event: Some(Box::new(class)),
            showed: false,
            cancel_delay_over: false,
        }
    }

    #[test]
    fn upcoming_bookings_become_events() {
        let bookings = vec![booking(
            "/bigbox/attendees/1",
            "booked",
            class("/bigbox/class_events/4471", at(20, 18), at(20, 19)),
        )];

        let events = desired_events(&bookings, &directory(), now());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].title, "Body Pump");
        assert_eq!(events[0].start, at(20, 18));
        assert_eq!(events[0].duration_minutes, 60);
        assert_eq!(events[0].time_zone, "Europe/London");
        assert_eq!(events[0].location.as_deref(), Some("BigBox"));
        assert!(!events[0].tentative);
    }

    #[test]
    fn waiting_list_places_are_marked_tentative() {
        let bookings = vec![booking(
            "/bigbox/attendees/2",
            "queued",
            class("/bigbox/class_events/4472", at(21, 7), at(21, 8)),
        )];

        let events = desired_events(&bookings, &directory(), now());
        assert_eq!(events.len(), 1);
        assert!(events[0].tentative);
        assert!(events[0].title.contains("waiting list"));
    }

    /// `my_bookings` returns the member's whole history with cancellations left
    /// in place, so a sync that trusted it would resurrect classes they'd
    /// dropped.
    #[test]
    fn cancelled_and_past_bookings_are_excluded() {
        let mut cancelled_class = class("/bigbox/class_events/1", at(20, 18), at(20, 19));
        cancelled_class.deleted_at = Some("2026-08-16T00:00:00".to_string());

        let bookings = vec![
            // Cancelled by the member.
            booking(
                "/bigbox/attendees/1",
                "canceled",
                class("/bigbox/class_events/2", at(20, 18), at(20, 19)),
            ),
            // Cancelled by the club.
            booking("/bigbox/attendees/2", "booked", cancelled_class),
            // Already happened.
            booking(
                "/bigbox/attendees/3",
                "booked",
                class("/bigbox/class_events/3", at(16, 18), at(16, 19)),
            ),
        ];

        assert!(desired_events(&bookings, &directory(), now()).is_empty());
    }

    /// The sync removes anything the server holds in its window that this list
    /// doesn't name, so a class that's under way has to stay listed — cutting
    /// at the start time would delete it from the calendar mid-class.
    #[test]
    fn a_class_in_progress_is_still_wanted() {
        let bookings = vec![booking(
            "/bigbox/attendees/1",
            "booked",
            class("/bigbox/class_events/1", at(17, 11), at(17, 13)),
        )];

        let events = desired_events(&bookings, &directory(), now());
        assert_eq!(events.len(), 1, "a class running now should stay on the calendar");

        // One that finished an hour ago drops out of both sides at once.
        let finished = vec![booking(
            "/bigbox/attendees/2",
            "booked",
            class("/bigbox/class_events/2", at(17, 10), at(17, 11)),
        )];
        assert!(desired_events(&finished, &directory(), now()).is_empty());
    }

    #[test]
    fn events_are_ordered_by_start_so_repeat_syncs_match() {
        let bookings = vec![
            booking(
                "/bigbox/attendees/1",
                "booked",
                class("/bigbox/class_events/9", at(22, 9), at(22, 10)),
            ),
            booking(
                "/bigbox/attendees/2",
                "booked",
                class("/bigbox/class_events/8", at(20, 18), at(20, 19)),
            ),
        ];

        let events = desired_events(&bookings, &directory(), now());
        assert_eq!(events[0].start, at(20, 18));
        assert_eq!(events[1].start, at(22, 9));

        // Same input, same output — this is what lets a redundant sync be
        // skipped rather than sent.
        assert_eq!(events, desired_events(&bookings, &directory(), now()));
    }

    /// A club with no timezone must not silently become UTC.
    #[test]
    fn a_missing_club_timezone_falls_back_to_the_clubs_own() {
        let mut directory = directory();
        directory
            .clubs
            .get_mut("/bigbox/clubs/270")
            .unwrap()
            .timezone = None;

        let bookings = vec![booking(
            "/bigbox/attendees/1",
            "booked",
            class("/bigbox/class_events/1", at(20, 18), at(20, 19)),
        )];

        let events = desired_events(&bookings, &directory, now());
        assert_eq!(events[0].time_zone, "Europe/London");
    }
}
