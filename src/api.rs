// SPDX-License-Identifier: MPL-2.0

//! Client for the Resamania member API that backs BigBox Leisure Club.
//!
//! The web app at <https://member.uk.resamania.com> is a React front end over an
//! API Platform (JSON-LD / Hydra) backend. Everything here mirrors what that app
//! does: an OAuth2 *password* grant against `/{client_token}/oauth/v2/token`,
//! then bearer-authenticated reads of `class_events` for the planning grid.
//!
//! Two things are worth knowing before reading further:
//!
//! * **Entities reference each other by IRI**, not by id — a `ClassEvent` names
//!   its activity as `"/bigbox/activities/2300"`. Turning those into display
//!   names needs the lookup tables in [`Directory`].
//! * **Timestamps are naive**, i.e. `"2026-08-14T06:30:00"` with no offset. They
//!   are wall-clock times in the club's own timezone (see [`Club::timezone`]),
//!   so they are parsed as [`NaiveDateTime`] and left un-converted.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::Mutex;

const API_BASE: &str = "https://api.uk.resamania.com";

/// Resamania is multi-tenant and every path is prefixed with the tenant's
/// "client token". BigBox Leisure Club is `bigbox`.
const CLIENT_TOKEN: &str = "bigbox";

/// OAuth2 client credentials. These are not secrets in any meaningful sense —
/// they are compiled into the public JavaScript bundle as the fallback values
/// for `REACT_APP_OAUTH_CLIENTID` / `REACT_APP_OAUTH_CLIENTSECRET`, and identify
/// the *application*, not the user.
const OAUTH_CLIENT_ID: &str = "33_v1q53htcy2imed6hm975i2hg37v3agspda7xmwo18oeueoh0qvd";
const OAUTH_CLIENT_SECRET: &str = "m4br8ty78lxlpxusbkxm35iwypb2zfcdn25429b0gpp185xj5qx";

/// Sent with every grant. The server validates it against the registered client
/// even for the password grant, where nothing is ever redirected.
const REDIRECT_URI: &str = "https://member.uk.resamania.com/";

/// Refresh this long before the token actually expires, so a request that is
/// about to be sent doesn't race the expiry.
const REFRESH_MARGIN: Duration = Duration::seconds(60);

/// Stop following `hydra:next` after this many pages. The planning extends
/// months into the future, so an unbounded date range would otherwise page
/// forever; this is a backstop for callers who pass one anyway.
const MAX_PAGES: u32 = 100;

// --- Errors ---

#[derive(Debug, Clone)]
pub enum ApiError {
    /// The email/password pair was rejected.
    InvalidCredentials,
    /// The access token expired and could not be refreshed — the user has to
    /// log in again.
    SessionExpired,
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::InvalidCredentials => write!(f, "Incorrect email or password"),
            ApiError::SessionExpired => write!(f, "Session expired"),
            ApiError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Other(e.to_string())
    }
}

// --- Session ---

#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

/// The live half of a session: the tokens, which rotate on every refresh.
#[derive(Debug, Clone)]
struct Tokens {
    access: String,
    refresh: String,
    expires_at: DateTime<Utc>,
}

impl Tokens {
    fn from_response(resp: TokenResponse) -> Self {
        Self {
            access: resp.access_token,
            refresh: resp.refresh_token,
            expires_at: Utc::now() + Duration::seconds(resp.expires_in),
        }
    }

    fn is_fresh(&self) -> bool {
        Utc::now() + REFRESH_MARGIN < self.expires_at
    }
}

/// The subset of the access token's JWT claims that the client needs. The web
/// app does the same thing — the token is the only place `targetId` appears.
#[derive(Debug, Clone, Deserialize)]
struct Claims {
    /// IRI of the logged-in *user*, e.g. `/bigbox/contact_users/2288503`.
    #[serde(rename = "targetId")]
    target_id: String,
    email: String,
}

/// Who the session belongs to. Booking requires denormalised copies of several
/// of these fields (see [`Client::book`]), which is why the client resolves the
/// full contact record up front rather than lazily.
#[derive(Debug, Clone)]
pub struct Profile {
    /// IRI, e.g. `/bigbox/contacts/2327536`.
    pub contact_id: String,
    /// IRI, e.g. `/bigbox/contact_users/2288503`.
    pub contact_user_id: String,
    /// IRI of the member's home club, e.g. `/bigbox/clubs/270`.
    pub club_id: String,
    /// Human-facing membership number, e.g. `C2327536`.
    pub number: String,
    pub given_name: String,
    pub family_name: String,
    pub email: String,
    /// Naive timestamp string, echoed back verbatim when booking.
    pub created_at: String,
}

impl Profile {
    pub fn full_name(&self) -> String {
        format!("{} {}", self.given_name, self.family_name)
            .trim()
            .to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ContactUser {
    #[serde(rename = "contactId")]
    contact_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Contact {
    #[serde(rename = "@id")]
    id: String,
    #[serde(default)]
    number: Option<String>,
    #[serde(rename = "givenName", default)]
    given_name: Option<String>,
    #[serde(rename = "familyName", default)]
    family_name: Option<String>,
    #[serde(rename = "clubId", default)]
    club_id: Option<String>,
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
}

// --- Client ---

pub struct Client {
    http: reqwest::Client,
    /// Behind a mutex because refreshing mutates the tokens from behind the
    /// `&self` of an ordinary request.
    tokens: Mutex<Tokens>,
    profile: Profile,
}

/// Hand-written so the tokens never reach a log line — a derived `Debug` would
/// print the bearer token in full. GUI frameworks tend to `Debug`-format the
/// messages they route, and the client travels in one.
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("email", &self.profile.email)
            .field("contact_id", &self.profile.contact_id)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Logs in with a member's email and password and resolves their profile.
    pub async fn login(email: &str, password: &str) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent("bigbox-for-cosmic")
            .build()?;

        let tokens = Self::request_token(
            &http,
            &[
                ("grant_type", "password"),
                ("username", email),
                ("password", password),
            ],
        )
        .await?;

        let claims = decode_claims(&tokens.access)?;

        // `targetId` is a contact_user; the contact it points at is the record
        // that owns bookings, so both are needed.
        let client = Self {
            http,
            tokens: Mutex::new(tokens),
            // Placeholder — replaced below once the contact is fetched. Only
            // `contact_user_id` is read before then.
            profile: Profile {
                contact_id: String::new(),
                contact_user_id: claims.target_id.clone(),
                club_id: String::new(),
                number: String::new(),
                given_name: String::new(),
                family_name: String::new(),
                email: claims.email.clone(),
                created_at: String::new(),
            },
        };

        let user: ContactUser = client.get(&claims.target_id, &[]).await?;
        let contact: Contact = client.get(&user.contact_id, &[]).await?;

        Ok(Self {
            profile: Profile {
                contact_id: contact.id,
                contact_user_id: claims.target_id,
                club_id: contact.club_id.unwrap_or_default(),
                number: contact.number.unwrap_or_default(),
                given_name: contact.given_name.unwrap_or_default(),
                family_name: contact.family_name.unwrap_or_default(),
                email: claims.email,
                created_at: contact.created_at.unwrap_or_default(),
            },
            ..client
        })
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    async fn request_token(
        http: &reqwest::Client,
        grant: &[(&str, &str)],
    ) -> Result<Tokens, ApiError> {
        let mut form = vec![
            ("client_id", OAUTH_CLIENT_ID),
            ("client_secret", OAUTH_CLIENT_SECRET),
            ("redirect_uri", REDIRECT_URI),
        ];
        form.extend_from_slice(grant);

        let resp = http
            .post(format!("{API_BASE}/{CLIENT_TOKEN}/oauth/v2/token"))
            .form(&form)
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(ApiError::InvalidCredentials);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Other(format!(
                "Token request failed ({status}): {body}"
            )));
        }

        Ok(Tokens::from_response(resp.json().await?))
    }

    /// Returns a usable access token, refreshing first if it is at or near
    /// expiry.
    async fn access_token(&self) -> Result<String, ApiError> {
        let mut tokens = self.tokens.lock().await;
        if tokens.is_fresh() {
            return Ok(tokens.access.clone());
        }
        *tokens = Self::refresh(&self.http, &tokens.refresh).await?;
        Ok(tokens.access.clone())
    }

    /// Refreshes unconditionally. Used when a request comes back 401 despite a
    /// token that looked fresh — e.g. after the machine slept, or if the server
    /// revoked it.
    async fn force_refresh(&self) -> Result<String, ApiError> {
        let mut tokens = self.tokens.lock().await;
        *tokens = Self::refresh(&self.http, &tokens.refresh).await?;
        Ok(tokens.access.clone())
    }

    async fn refresh(http: &reqwest::Client, refresh_token: &str) -> Result<Tokens, ApiError> {
        Self::request_token(
            http,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
        .await
        // A rejected refresh token means the session is genuinely over, which
        // is a different remedy than "your password was wrong".
        .map_err(|e| match e {
            ApiError::InvalidCredentials => ApiError::SessionExpired,
            other => other,
        })
    }

    /// Sends an authenticated request, retrying once against a freshly
    /// refreshed token if the first attempt is rejected.
    async fn send<F>(&self, build: F) -> Result<reqwest::Response, ApiError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        let token = self.access_token().await?;
        let resp = build(&self.http)
            .bearer_auth(&token)
            .header("accept", "application/ld+json")
            .send()
            .await?;

        let resp = if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            let token = self.force_refresh().await?;
            build(&self.http)
                .bearer_auth(&token)
                .header("accept", "application/ld+json")
                .send()
                .await?
        } else {
            resp
        };

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::SessionExpired);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Other(format!(
                "Request failed ({status}): {body}"
            )));
        }
        Ok(resp)
    }

    /// GETs `path`, which may be either an IRI (`/bigbox/contacts/1`) or a bare
    /// collection name (`contacts`).
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ApiError> {
        let url = absolute(path);
        let resp = self.send(|http| http.get(&url).query(query)).await?;
        resp.json()
            .await
            .map_err(|e| ApiError::Other(format!("Parse error for {path}: {e}")))
    }

    /// GETs every page of a Hydra collection, following `hydra:next`.
    async fn get_all<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<T>, ApiError> {
        let mut items = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut paged = query.to_vec();
            paged.push(("page", page.to_string()));

            let collection: Collection<T> = self.get(path, &paged).await?;
            items.extend(collection.members);

            if collection.view.and_then(|v| v.next).is_none() {
                break;
            }
        }
        Ok(items)
    }

    // --- Planning ---

    /// Fetches the class planning for a club over a date range.
    ///
    /// `from` is inclusive and `to` is exclusive, matching the API's
    /// `startedAt[after]` / `startedAt[before]` filters. Both bounds are
    /// required: the planning runs months ahead, so an open-ended range would
    /// page until [`MAX_PAGES`].
    pub async fn planning(
        &self,
        club_id: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<ClassEvent>, ApiError> {
        self.get_all(
            "class_events",
            &[
                ("club", club_id.to_string()),
                ("startedAt[after]", from.to_string()),
                ("startedAt[before]", to.to_string()),
                ("order[startedAt]", "asc".to_string()),
            ],
        )
        .await
    }

    /// Fetches the logged-in member's bookings, oldest class first.
    ///
    /// This returns *every* attendee record for all time, including past
    /// classes they attended and ones they cancelled — for a regular member
    /// that's dozens of rows over many pages. Prefer
    /// [`Client::bookings_from`] unless you genuinely want the history, and
    /// filter on [`Attendee::is_active`] to drop cancellations.
    ///
    /// Each carries its [`Attendee::class_event`] embedded, though with fewer
    /// fields than [`Client::planning`] returns — notably no remaining-places
    /// counts.
    pub async fn my_bookings(&self) -> Result<Vec<Attendee>, ApiError> {
        self.fetch_bookings(&[]).await
    }

    /// Fetches bookings for classes starting on or after `from`, oldest first.
    ///
    /// The date bound is applied by the server, so this pages through the
    /// member's next few bookings rather than their entire history. Note the
    /// filter is whole-day inclusive: passing today returns classes from
    /// earlier today too, so callers wanting strictly-future bookings should
    /// still compare against the current time.
    pub async fn bookings_from(&self, from: NaiveDate) -> Result<Vec<Attendee>, ApiError> {
        self.fetch_bookings(&[("classEvent.startedAt[after]", from.to_string())])
            .await
    }

    async fn fetch_bookings(&self, filters: &[(&str, String)]) -> Result<Vec<Attendee>, ApiError> {
        let mut query = vec![("contactId", self.profile.contact_id.clone())];
        query.extend(filters.iter().cloned());

        let mut bookings: Vec<Attendee> = self.get_all("attendees", &query).await?;

        // Sorted here rather than by the server: `order[classEvent.startedAt]`
        // is advertised in the collection's Hydra search mapping but returns a
        // 500, and the embedded class carries the time anyway.
        bookings.sort_by_key(|a| a.class_event.as_ref().and_then(|c| c.start()));
        Ok(bookings)
    }

    /// Fetches one class event by IRI or numeric id.
    pub async fn class_event(&self, id: &str) -> Result<ClassEvent, ApiError> {
        self.get(&collection_path("class_events", id), &[]).await
    }

    // --- Lookups ---

    /// Fetches the reference data needed to turn a [`ClassEvent`]'s IRIs into
    /// names. Each of these is a small, unpaginated collection.
    pub async fn directory(&self) -> Result<Directory, ApiError> {
        let (activities, coaches, studios, clubs) = tokio::try_join!(
            self.get_all::<Activity>("activities", &[]),
            self.get_all::<Coach>("coaches", &[]),
            self.get_all::<Studio>("studios", &[]),
            self.get_all::<Club>("clubs", &[]),
        )?;

        Ok(Directory {
            activities: by_iri(activities, |a| a.id.clone()),
            coaches: by_iri(coaches, |c| c.id.clone()),
            studios: by_iri(studios, |s| s.id.clone()),
            clubs: by_iri(clubs, |c| c.id.clone()),
        })
    }

    /// Fetches the clubs this member can see.
    pub async fn clubs(&self) -> Result<Vec<Club>, ApiError> {
        self.get_all("clubs", &[]).await
    }

    // --- Booking ---

    /// Books the logged-in member onto a class, returning the created attendee.
    ///
    /// This is also how you join a waiting list: the request is byte-identical
    /// either way and the server decides, so check [`Attendee::is_queued`] on
    /// the result rather than assuming a seat. Booking a class whose seats are
    /// gone but whose queue has room yields `state: "queued"`.
    ///
    /// The API wants the contact's details denormalised into the request body
    /// rather than looked up from `contactId`; these are copied from
    /// [`Profile`], which is why login resolves the full contact record.
    pub async fn book(&self, class_event: &str) -> Result<Attendee, ApiError> {
        let body = BookingRequest {
            contact_id: &self.profile.contact_id,
            contact_club_id: &self.profile.club_id,
            contact_number: &self.profile.number,
            contact_family_name: &self.profile.family_name,
            contact_given_name: &self.profile.given_name,
            contact_created_at: &self.profile.created_at,
            class_event: &collection_path("class_events", class_event),
        };

        let url = absolute("attendees");
        let resp = self.send(|http| http.post(&url).json(&body)).await?;
        resp.json()
            .await
            .map_err(|e| ApiError::Other(format!("Parse error for booking: {e}")))
    }

    /// Cancels a booking. `attendee` is the IRI or id from
    /// [`ClassEvent::my_attendee`].
    ///
    /// Bookings are a state machine and cancelling is a *transition* on the
    /// attendee, not a delete — the record survives with a `cancelled` state.
    pub async fn cancel(&self, attendee: &str) -> Result<(), ApiError> {
        let url = absolute(&format!(
            "{}/transitions",
            collection_path("attendees", attendee)
        ));
        self.send(|http| {
            http.post(&url).json(&Transition {
                transition: "cancel",
            })
        })
        .await?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct BookingRequest<'a> {
    #[serde(rename = "contactId")]
    contact_id: &'a str,
    #[serde(rename = "contactClubId")]
    contact_club_id: &'a str,
    #[serde(rename = "contactNumber")]
    contact_number: &'a str,
    #[serde(rename = "contactFamilyName")]
    contact_family_name: &'a str,
    #[serde(rename = "contactGivenName")]
    contact_given_name: &'a str,
    #[serde(rename = "contactCreatedAt")]
    contact_created_at: &'a str,
    #[serde(rename = "classEvent")]
    class_event: &'a str,
}

#[derive(Debug, Serialize)]
struct Transition<'a> {
    transition: &'a str,
}

// --- Hydra envelope ---

#[derive(Debug, Deserialize)]
struct Collection<T> {
    #[serde(rename = "hydra:member", default = "Vec::new")]
    members: Vec<T>,
    #[serde(rename = "hydra:view", default = "Option::default")]
    view: Option<View>,
}

#[derive(Debug, Deserialize)]
struct View {
    #[serde(rename = "hydra:next")]
    next: Option<String>,
}

// --- Entities ---

#[derive(Debug, Clone, Deserialize)]
pub struct ClassEvent {
    #[serde(rename = "@id")]
    pub id: String,
    pub club: Option<String>,
    pub studio: Option<String>,
    pub activity: Option<String>,
    pub coach: Option<String>,
    /// Total capacity, including places not offered online.
    #[serde(rename = "attendingLimit")]
    pub attending_limit: Option<i64>,
    /// Cap on how many of those places online members may take.
    #[serde(rename = "onlineLimit")]
    pub online_limit: Option<i64>,
    #[serde(rename = "queueLimit")]
    pub queue_limit: Option<i64>,
    #[serde(rename = "attendeeRemaining")]
    pub attendee_remaining: Option<i64>,
    #[serde(rename = "onlineAttendeeRemaining")]
    pub online_attendee_remaining: Option<i64>,
    #[serde(rename = "queueRemaining")]
    pub queue_remaining: Option<i64>,
    #[serde(rename = "startedAt")]
    pub started_at: Option<String>,
    #[serde(rename = "endedAt")]
    pub ended_at: Option<String>,
    #[serde(rename = "coachAvailable", default)]
    pub coach_available: bool,
    pub summary: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "instructionsComment")]
    pub instructions_comment: Option<String>,
    #[serde(rename = "bookedAttendees", default)]
    pub booked_attendees: Vec<Attendee>,
    #[serde(rename = "queuedAttendees", default)]
    pub queued_attendees: Vec<Attendee>,
    #[serde(rename = "deletedAt")]
    pub deleted_at: Option<String>,
}

impl ClassEvent {
    /// Local start time. The API sends wall-clock times in the club's timezone
    /// with no offset, so this is deliberately not a `DateTime`.
    pub fn start(&self) -> Option<NaiveDateTime> {
        parse_naive(self.started_at.as_deref())
    }

    pub fn end(&self) -> Option<NaiveDateTime> {
        parse_naive(self.ended_at.as_deref())
    }

    pub fn duration_minutes(&self) -> Option<i64> {
        Some((self.end()? - self.start()?).num_minutes())
    }

    /// This member's attendee record on the class, whether they hold a seat or
    /// a waiting-list place. Note that a cancelled booking still appears in
    /// `booked_attendees`, so callers that only care about live bookings should
    /// also check [`Attendee::is_active`].
    pub fn my_attendee(&self, contact_id: &str) -> Option<&Attendee> {
        self.booked_attendees
            .iter()
            .chain(&self.queued_attendees)
            .find(|a| a.contact_id.as_deref() == Some(contact_id))
    }

    /// Whether this member holds an actual seat — *not* a waiting-list place.
    /// The two lists are separate, and conflating them would tell someone they
    /// have a spot when they're only queued for one.
    pub fn is_booked_by(&self, contact_id: &str) -> bool {
        Self::holds_place(&self.booked_attendees, contact_id)
    }

    /// Whether this member is on the waiting list.
    pub fn is_queued_by(&self, contact_id: &str) -> bool {
        Self::holds_place(&self.queued_attendees, contact_id)
    }

    fn holds_place(attendees: &[Attendee], contact_id: &str) -> bool {
        attendees
            .iter()
            .any(|a| a.contact_id.as_deref() == Some(contact_id) && a.is_active())
    }

    /// This member's 1-based place in the waiting list.
    ///
    /// The API exposes no explicit position, but it returns `queuedAttendees`
    /// in ascending attendee id — i.e. the order people joined — so the index
    /// is the position.
    pub fn queue_position(&self, contact_id: &str) -> Option<usize> {
        self.queued_attendees
            .iter()
            .position(|a| a.contact_id.as_deref() == Some(contact_id))
            .map(|index| index + 1)
    }

    /// Places still bookable online. Falls back to the overall remaining count
    /// for classes with no separate online cap.
    pub fn places_remaining(&self) -> Option<i64> {
        self.online_attendee_remaining.or(self.attendee_remaining)
    }

    pub fn is_full(&self) -> bool {
        self.places_remaining().is_some_and(|n| n <= 0)
    }

    /// True when the class is full but its waiting list still has room.
    pub fn has_queue_space(&self) -> bool {
        self.is_full() && self.queue_remaining.is_some_and(|n| n > 0)
    }

    pub fn is_cancelled(&self) -> bool {
        self.deleted_at.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Attendee {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "contactId")]
    pub contact_id: Option<String>,
    /// Workflow state, e.g. `booked`, `queued`, `canceled`. Absent on the
    /// summarised attendees embedded in a `ClassEvent` listing.
    pub state: Option<String>,
    /// The class this booking is for. Present when the attendee came from
    /// [`Client::my_bookings`] or [`Client::book`], absent when it came
    /// embedded in a `ClassEvent`. Boxed to keep the two types from nesting
    /// into an infinitely sized cycle.
    #[serde(rename = "classEvent")]
    pub class_event: Option<Box<ClassEvent>>,
    #[serde(default)]
    pub showed: bool,
    /// Set once the class is close enough that cancelling is no longer allowed.
    #[serde(rename = "cancelDelayOver", default)]
    pub cancel_delay_over: bool,
}

impl Attendee {
    /// Whether this booking still stands.
    ///
    /// Attendees embedded in a `ClassEvent` carry no `state`, but the API only
    /// lists live ones there, so a missing state means active. Note the API
    /// spells the cancelled state `canceled`; both spellings are accepted here
    /// because the web app's own bundle uses them interchangeably.
    pub fn is_active(&self) -> bool {
        !matches!(
            self.state.as_deref(),
            Some("canceled") | Some("cancelled") | Some("refused")
        )
    }

    /// Whether this is a waiting-list place rather than a seat.
    ///
    /// Only meaningful on attendees that carry a `state` — those from
    /// [`Client::book`] or [`Client::my_bookings`]. Ones embedded in a
    /// `ClassEvent` have no state, and there you can tell the two apart by
    /// which list they came from instead.
    pub fn is_queued(&self) -> bool {
        self.state.as_deref() == Some("queued")
    }

    /// Whether cancelling is still permitted — false once the club's cancellation
    /// deadline has passed.
    pub fn is_cancellable(&self) -> bool {
        self.is_active() && !self.cancel_delay_over
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    #[serde(rename = "@id")]
    pub id: String,
    pub name: Option<String>,
    /// Used by the web app to colour the planning grid, e.g. `#fcc400`.
    #[serde(rename = "colorHex")]
    pub color_hex: Option<String>,
    #[serde(rename = "isBookable", default)]
    pub is_bookable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Coach {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "givenName")]
    pub given_name: Option<String>,
    #[serde(rename = "familyName")]
    pub family_name: Option<String>,
    /// Preferred display name, when the coach has one.
    #[serde(rename = "alternateName")]
    pub alternate_name: Option<String>,
}

impl Coach {
    pub fn display_name(&self) -> String {
        if let Some(name) = self.alternate_name.as_deref().filter(|n| !n.is_empty()) {
            return name.to_string();
        }
        format!(
            "{} {}",
            self.given_name.as_deref().unwrap_or(""),
            self.family_name.as_deref().unwrap_or("")
        )
        .trim()
        .to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Studio {
    #[serde(rename = "@id")]
    pub id: String,
    pub name: Option<String>,
    pub club: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Club {
    #[serde(rename = "@id")]
    pub id: String,
    pub name: Option<String>,
    /// IANA zone the club's naive timestamps are expressed in.
    pub timezone: Option<String>,
    pub locale: Option<String>,
}

/// The classes someone is actually going to, out of a set of attendee records.
///
/// Three things have to be excluded and it's easy to forget one:
/// [`Client::my_bookings`] returns a member's whole history, cancellations stay
/// in the list with a `canceled` state, and a waiting-list place isn't a
/// booking. Miss the date check in particular and a regular member looks like
/// they have dozens of upcoming classes when they have none.
pub fn upcoming_class_ids(
    attendees: impl IntoIterator<Item = Attendee>,
    now: NaiveDateTime,
) -> std::collections::HashSet<String> {
    attendees
        .into_iter()
        .filter(|attendee| attendee.is_active() && !attendee.is_queued())
        .filter_map(|attendee| attendee.class_event)
        .filter(|class| class.start().is_some_and(|start| start >= now))
        .map(|class| class.id)
        .collect()
}

// --- Directory ---

/// Reference data for resolving the IRIs on a [`ClassEvent`] into names.
#[derive(Debug, Clone, Default)]
pub struct Directory {
    pub activities: HashMap<String, Activity>,
    pub coaches: HashMap<String, Coach>,
    pub studios: HashMap<String, Studio>,
    pub clubs: HashMap<String, Club>,
}

impl Directory {
    pub fn activity_name(&self, event: &ClassEvent) -> Option<&str> {
        let activity = self.activities.get(event.activity.as_deref()?)?;
        activity.name.as_deref()
    }

    pub fn activity_color(&self, event: &ClassEvent) -> Option<&str> {
        let activity = self.activities.get(event.activity.as_deref()?)?;
        activity.color_hex.as_deref()
    }

    pub fn coach_name(&self, event: &ClassEvent) -> Option<String> {
        Some(self.coaches.get(event.coach.as_deref()?)?.display_name())
    }

    pub fn studio_name(&self, event: &ClassEvent) -> Option<&str> {
        let studio = self.studios.get(event.studio.as_deref()?)?;
        studio.name.as_deref()
    }

    pub fn club_name(&self, event: &ClassEvent) -> Option<&str> {
        let club = self.clubs.get(event.club.as_deref()?)?;
        club.name.as_deref()
    }

    /// Resolves an event into a flat, display-ready row.
    pub fn resolve(&self, event: &ClassEvent, contact_id: &str) -> PlanningEntry {
        PlanningEntry {
            name: self.activity_name(event).unwrap_or("Class").to_string(),
            color_hex: self.activity_color(event).map(str::to_string),
            coach: self.coach_name(event),
            studio: self.studio_name(event).map(str::to_string),
            club: self.club_name(event).map(str::to_string),
            start: event.start(),
            duration_minutes: event.duration_minutes(),
            places_remaining: event.places_remaining(),
            capacity: event.attending_limit,
            is_full: event.is_full(),
            has_queue_space: event.has_queue_space(),
            is_cancelled: event.is_cancelled(),
            booked: event.is_booked_by(contact_id),
            queued: event.is_queued_by(contact_id),
            queue_position: event.queue_position(contact_id),
            attendee_id: event.my_attendee(contact_id).map(|a| a.id.clone()),
            id: event.id.clone(),
        }
    }
}

/// A [`ClassEvent`] with its references resolved — what a UI actually renders.
#[derive(Debug, Clone)]
pub struct PlanningEntry {
    pub id: String,
    pub name: String,
    pub color_hex: Option<String>,
    pub coach: Option<String>,
    pub studio: Option<String>,
    pub club: Option<String>,
    pub start: Option<NaiveDateTime>,
    pub duration_minutes: Option<i64>,
    pub places_remaining: Option<i64>,
    pub capacity: Option<i64>,
    pub is_full: bool,
    pub has_queue_space: bool,
    pub is_cancelled: bool,
    /// Whether the logged-in member holds an actual seat.
    pub booked: bool,
    /// Whether the member is on the waiting list rather than booked in.
    pub queued: bool,
    /// The member's 1-based place in the waiting list, when queued.
    pub queue_position: Option<usize>,
    /// The member's attendee IRI, to pass to [`Client::cancel`]. Set whether
    /// they hold a seat or a queue place — cancelling works the same for both.
    pub attendee_id: Option<String>,
}

impl PlanningEntry {
    pub fn time_range(&self) -> String {
        let Some(start) = self.start else {
            return String::new();
        };
        match self.duration_minutes {
            Some(mins) => format!(
                "{}–{}",
                start.format("%H:%M"),
                (start + Duration::minutes(mins)).format("%H:%M")
            ),
            None => start.format("%H:%M").to_string(),
        }
    }

    pub fn places_text(&self) -> String {
        match (self.places_remaining, self.capacity) {
            _ if self.is_cancelled => "Cancelled".to_string(),
            (Some(free), _) if free <= 0 => "Full".to_string(),
            (Some(free), Some(cap)) => format!("{free}/{cap} free"),
            (Some(free), None) => format!("{free} free"),
            _ => String::new(),
        }
    }
}

// --- Helpers ---

/// Turns an IRI or bare collection name into an absolute URL.
fn absolute(path: &str) -> String {
    if path.starts_with("http") {
        path.to_string()
    } else if let Some(iri) = path.strip_prefix('/') {
        format!("{API_BASE}/{iri}")
    } else {
        format!("{API_BASE}/{CLIENT_TOKEN}/{path}")
    }
}

/// Accepts either a full IRI or a bare id and returns an IRI, so callers can
/// pass whichever they have.
fn collection_path(collection: &str, id_or_iri: &str) -> String {
    if id_or_iri.starts_with('/') {
        id_or_iri.to_string()
    } else {
        format!("/{CLIENT_TOKEN}/{collection}/{id_or_iri}")
    }
}

fn by_iri<T, F: Fn(&T) -> String>(items: Vec<T>, key: F) -> HashMap<String, T> {
    items.into_iter().map(|item| (key(&item), item)).collect()
}

fn parse_naive(value: Option<&str>) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value?, "%Y-%m-%dT%H:%M:%S").ok()
}

/// Reads the claims out of a JWT without verifying it — the server is the one
/// that validates the signature; this only needs the payload's contents.
fn decode_claims(jwt: &str) -> Result<Claims, ApiError> {
    use base64::Engine as _;

    let payload = jwt
        .split('.')
        .nth(1)
        .ok_or_else(|| ApiError::Other("Malformed access token".to_string()))?;

    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| ApiError::Other(format!("Malformed access token: {e}")))?;

    serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::Other(format!("Unexpected access token claims: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_handles_iris_and_collection_names() {
        assert_eq!(
            absolute("class_events"),
            "https://api.uk.resamania.com/bigbox/class_events"
        );
        assert_eq!(
            absolute("/bigbox/contacts/1"),
            "https://api.uk.resamania.com/bigbox/contacts/1"
        );
    }

    #[test]
    fn collection_path_accepts_ids_and_iris() {
        assert_eq!(
            collection_path("class_events", "1722803"),
            "/bigbox/class_events/1722803"
        );
        assert_eq!(
            collection_path("attendees", "/bigbox/attendees/1"),
            "/bigbox/attendees/1"
        );
    }

    #[test]
    fn parses_naive_club_local_timestamps() {
        let event = ClassEvent {
            started_at: Some("2026-08-14T06:30:00".to_string()),
            ended_at: Some("2026-08-14T07:15:00".to_string()),
            ..sample_event()
        };
        assert_eq!(event.duration_minutes(), Some(45));
        assert_eq!(
            event.start().map(|s| s.format("%H:%M").to_string()),
            Some("06:30".to_string())
        );
    }

    #[test]
    fn online_limit_takes_precedence_over_overall_capacity() {
        let event = ClassEvent {
            attendee_remaining: Some(5),
            online_attendee_remaining: Some(0),
            queue_remaining: Some(3),
            ..sample_event()
        };
        assert!(event.is_full());
        assert!(event.has_queue_space());
    }

    /// The API spells it `canceled`; getting this wrong would show a cancelled
    /// class as still booked.
    #[test]
    fn cancelled_bookings_do_not_count_as_booked() {
        for state in ["canceled", "cancelled", "refused"] {
            let mine = "/bigbox/contacts/2327536";
            let event = ClassEvent {
                booked_attendees: vec![Attendee {
                    state: Some(state.to_string()),
                    ..sample_attendee(mine)
                }],
                ..sample_event()
            };
            assert!(!event.is_booked_by(mine), "{state} should not be booked");
            // Still discoverable, so a caller can tell "never booked" from
            // "booked then cancelled".
            assert!(event.my_attendee(mine).is_some());
        }
    }

    #[test]
    fn embedded_attendees_without_state_are_active() {
        let mine = "/bigbox/contacts/2327536";
        let event = ClassEvent {
            booked_attendees: vec![sample_attendee(mine)],
            ..sample_event()
        };
        assert!(event.is_booked_by(mine));
    }

    #[test]
    fn passing_the_cancellation_deadline_blocks_cancelling() {
        let attendee = Attendee {
            state: Some("booked".to_string()),
            cancel_delay_over: true,
            ..sample_attendee("/bigbox/contacts/1")
        };
        assert!(attendee.is_active());
        assert!(!attendee.is_cancellable());
    }

    /// A `ClassEvent` listing embeds bare attendees, while `/attendees` embeds
    /// the class inside the attendee — one type has to deserialize both.
    #[test]
    fn attendees_parse_with_and_without_an_embedded_class() {
        let embedded_in_event: Attendee = serde_json::from_str(
            r#"{"@id":"/bigbox/attendees/6617591","contactId":"/bigbox/contacts/1463180",
                "showed":true,"cancelDelayOver":false}"#,
        )
        .unwrap();
        assert!(embedded_in_event.class_event.is_none());
        assert!(embedded_in_event.is_active());

        let from_bookings: Attendee = serde_json::from_str(
            r#"{"@id":"/bigbox/attendees/6681488","state":"canceled",
                "contactId":"/bigbox/contacts/2327536","cancelDelayOver":false,
                "classEvent":{"@id":"/bigbox/class_events/1722803",
                "activity":"/bigbox/activities/2818","startedAt":"2026-08-15T09:00:00",
                "endedAt":"2026-08-15T10:00:00"}}"#,
        )
        .unwrap();
        assert!(!from_bookings.is_active());
        let class = from_bookings.class_event.expect("class embedded");
        assert_eq!(class.duration_minutes(), Some(60));
    }

    /// A waiting-list place is not a seat. Reporting "booked" for a queued
    /// member would tell them they have a spot they haven't got.
    #[test]
    fn queued_members_are_not_counted_as_booked() {
        let mine = "/bigbox/contacts/2327536";
        let event = ClassEvent {
            queued_attendees: vec![sample_attendee(mine)],
            ..sample_event()
        };

        assert!(event.is_queued_by(mine));
        assert!(!event.is_booked_by(mine));
        // Still reachable, because cancelling a queue place uses the same call.
        assert_eq!(
            event.my_attendee(mine).map(|a| a.id.as_str()),
            Some("/bigbox/attendees/1")
        );
    }

    #[test]
    fn queue_position_counts_from_one_in_join_order() {
        let mine = "/bigbox/contacts/3";
        let event = ClassEvent {
            queued_attendees: vec![
                sample_attendee("/bigbox/contacts/1"),
                sample_attendee("/bigbox/contacts/2"),
                sample_attendee(mine),
            ],
            ..sample_event()
        };

        assert_eq!(event.queue_position(mine), Some(3));
        assert_eq!(event.queue_position("/bigbox/contacts/1"), Some(1));
        assert_eq!(event.queue_position("/bigbox/contacts/99"), None);
    }

    #[test]
    fn a_full_class_with_queue_room_is_joinable() {
        let event = ClassEvent {
            attendee_remaining: Some(0),
            queue_remaining: Some(9),
            ..sample_event()
        };
        assert!(event.is_full());
        assert!(event.has_queue_space());

        let queue_full = ClassEvent {
            attendee_remaining: Some(0),
            queue_remaining: Some(0),
            ..sample_event()
        };
        assert!(queue_full.is_full());
        assert!(!queue_full.has_queue_space());
    }

    #[test]
    fn attendee_state_distinguishes_a_queue_place_from_a_seat() {
        let queued = Attendee {
            state: Some("queued".to_string()),
            ..sample_attendee("/bigbox/contacts/1")
        };
        assert!(queued.is_queued());
        assert!(queued.is_active());

        let booked = Attendee {
            state: Some("booked".to_string()),
            ..sample_attendee("/bigbox/contacts/1")
        };
        assert!(!booked.is_queued());
        assert!(booked.is_active());
    }

    /// The regression: a member's booking history came back as "44 upcoming
    /// bookings" because only cancellations were being filtered out, never the
    /// date.
    #[test]
    fn a_history_of_attended_classes_is_not_upcoming() {
        let now =
            NaiveDateTime::parse_from_str("2026-08-14T22:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();

        let history: Vec<Attendee> = (0..44)
            .map(|i| {
                booking(
                    &format!("/bigbox/class_events/{i}"),
                    "2026-01-05T09:00:00",
                    "booked",
                )
            })
            .collect();

        assert!(
            upcoming_class_ids(history, now).is_empty(),
            "past classes must not count as upcoming"
        );
    }

    #[test]
    fn upcoming_keeps_only_future_uncancelled_seats() {
        let now =
            NaiveDateTime::parse_from_str("2026-08-14T22:00:00", "%Y-%m-%dT%H:%M:%S").unwrap();

        let attendees = vec![
            booking("/bigbox/class_events/1", "2026-08-20T09:00:00", "booked"),
            // Earlier today: the server's date filter is whole-day, so this
            // one arrives from the API and has to be dropped here.
            booking("/bigbox/class_events/2", "2026-08-14T09:00:00", "booked"),
            booking("/bigbox/class_events/3", "2026-08-20T10:00:00", "canceled"),
            // On a waiting list — not going yet.
            booking("/bigbox/class_events/4", "2026-08-20T11:00:00", "queued"),
        ];

        let upcoming = upcoming_class_ids(attendees, now);
        assert_eq!(upcoming.len(), 1);
        assert!(upcoming.contains("/bigbox/class_events/1"));
    }

    fn booking(class_id: &str, start: &str, state: &str) -> Attendee {
        Attendee {
            state: Some(state.to_string()),
            class_event: Some(Box::new(ClassEvent {
                id: class_id.to_string(),
                started_at: Some(start.to_string()),
                ..sample_event()
            })),
            ..sample_attendee("/bigbox/contacts/1")
        }
    }

    fn sample_attendee(contact_id: &str) -> Attendee {
        Attendee {
            id: "/bigbox/attendees/1".to_string(),
            contact_id: Some(contact_id.to_string()),
            state: None,
            class_event: None,
            showed: false,
            cancel_delay_over: false,
        }
    }

    fn sample_event() -> ClassEvent {
        ClassEvent {
            id: "/bigbox/class_events/1".to_string(),
            club: None,
            studio: None,
            activity: None,
            coach: None,
            attending_limit: None,
            online_limit: None,
            queue_limit: None,
            attendee_remaining: None,
            online_attendee_remaining: None,
            queue_remaining: None,
            started_at: None,
            ended_at: None,
            coach_available: true,
            summary: None,
            description: None,
            instructions_comment: None,
            booked_attendees: Vec::new(),
            queued_attendees: Vec::new(),
            deleted_at: None,
        }
    }
}
