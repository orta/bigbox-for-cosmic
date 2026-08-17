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
}

impl Config {
    pub fn has_credentials(&self) -> bool {
        !self.email.is_empty() && !self.password.is_empty()
    }
}
