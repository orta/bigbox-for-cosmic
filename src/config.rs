// SPDX-License-Identifier: MPL-2.0

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};

/// A friend's account, so their bookings can be shown alongside yours.
///
/// These are full logins, not a sharing feature the club offers — BigBox has no
/// concept of following another member, so seeing what a friend is going to
/// means signing in as them with credentials they've given you.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Friend {
    /// What to call them in the UI. Falls back to the email if left blank.
    pub name: String,
    pub email: String,
    pub password: String,
}

impl Friend {
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

/// Where booked classes get mirrored to, over CalDAV.
///
/// Stored in plain text like everything else here. An *app password* is the
/// right credential to put in this file rather than the account's real one:
/// it's revocable on its own, and it's what Fastmail requires for CalDAV in any
/// case.
#[derive(Debug, Default, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct CalendarSync {
    /// Fastmail address the app password belongs to.
    pub username: String,
    /// App password, not the account login.
    pub password: String,
    /// Path of the chosen calendar collection on the server.
    pub calendar_href: String,
    /// Kept alongside the href so the settings dialog can name the chosen
    /// calendar before the server has been re-contacted on launch.
    pub calendar_name: String,
    /// Off by default, and left off until a calendar has actually been picked.
    pub enabled: bool,
}

impl CalendarSync {
    pub fn has_credentials(&self) -> bool {
        !self.username.is_empty() && !self.password.is_empty()
    }

    /// Whether there's enough here to attempt a sync.
    pub fn is_active(&self) -> bool {
        self.enabled && self.has_credentials() && !self.calendar_href.is_empty()
    }
}

#[derive(Debug, Default, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Member's login email, kept so the app can sign back in on launch.
    pub email: String,
    /// Stored in plain text in the app's config directory, the same way the
    /// sibling gymgroup app keeps its PIN. Persisting a refresh token instead
    /// wouldn't help — Resamania's expire quickly and rotate on every use.
    pub password: String,
    /// Friends whose bookings show up on the planning grid.
    pub friends: Vec<Friend>,
    /// Fastmail calendar mirroring. Each field of a config entry is its own
    /// file on disk, so this being absent from an older config is not an error
    /// — it just reads back as the default.
    pub calendar: CalendarSync,
}

impl Config {
    pub fn has_credentials(&self) -> bool {
        !self.email.is_empty() && !self.password.is_empty()
    }
}
