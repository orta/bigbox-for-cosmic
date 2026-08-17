// SPDX-License-Identifier: MPL-2.0

//! Walks Fastmail's CalDAV discovery and prints what comes back, raw.
//!
//! The client parses these responses into a calendar list, and when a server
//! phrases something differently than expected the symptom is a calendar with
//! the wrong name rather than an error. This prints both sides — the XML as
//! sent, and what the parser made of it — so the two can be compared.
//!
//! ```sh
//! export FASTMAIL_USER=you@fastmail.com FASTMAIL_PASSWORD=…   # an app password
//! cargo run --example calendars
//! ```

use bigbox_for_cosmic::caldav::{Client, FASTMAIL_BASE};

const PRINCIPAL_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:">
  <d:prop><d:current-user-principal/></d:prop>
</d:propfind>"#;

const HOME_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><c:calendar-home-set/></d:prop>
</d:propfind>"#;

const CALENDARS_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop>
    <d:displayname/>
    <d:resourcetype/>
    <c:supported-calendar-component-set/>
    <d:current-user-privilege-set/>
  </d:prop>
</d:propfind>"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let username = std::env::var("FASTMAIL_USER").map_err(|_| "set FASTMAIL_USER")?;
    let password = std::env::var("FASTMAIL_PASSWORD").map_err(|_| "set FASTMAIL_PASSWORD")?;

    let http = reqwest::Client::new();

    let propfind = |url: String, depth: &'static str, body: &'static str| {
        let http = http.clone();
        let (username, password) = (username.clone(), password.clone());
        async move {
            let response = http
                .request(reqwest::Method::from_bytes(b"PROPFIND")?, &url)
                .basic_auth(&username, Some(&password))
                .header("Depth", depth)
                .header("Content-Type", "application/xml; charset=utf-8")
                .body(body)
                .send()
                .await?;

            println!("\n=== PROPFIND {url} (Depth: {depth}) -> {} ===", response.status());
            let text = response.text().await?;
            println!("{text}");
            Ok::<String, Box<dyn std::error::Error>>(text)
        }
    };

    // Hop 1: who am I?
    let principal_xml = propfind(
        format!("{FASTMAIL_BASE}/dav/principals/"),
        "0",
        PRINCIPAL_BODY,
    )
    .await?;

    let Some(principal) = between(&principal_xml, "current-user-principal") else {
        return Err("no current-user-principal in the response above".into());
    };

    // Hop 2: where do my calendars live?
    let home_xml = propfind(absolute(&principal), "0", HOME_BODY).await?;

    let Some(home) = between(&home_xml, "calendar-home-set") else {
        return Err("no calendar-home-set in the response above".into());
    };

    // Hop 3: what's in there?
    propfind(absolute(&home), "1", CALENDARS_BODY).await?;

    // And what the client makes of all that.
    println!("\n=== parsed by src/caldav.rs ===");
    match Client::new(&username, &password).calendars().await {
        Ok(calendars) => {
            for calendar in &calendars {
                println!("{:?}  {}", calendar.name, calendar.href);
            }
            if calendars.is_empty() {
                println!("(none)");
            }
        }
        Err(error) => println!("failed: {error}"),
    }

    Ok(())
}

/// Pulls the first `<href>` nested inside the named element out of a
/// multistatus body. Crude on purpose — this is a diagnostic, and it must not
/// share the parser whose output is in question.
fn between(xml: &str, element: &str) -> Option<String> {
    let start = xml.find(element)?;
    let rest = &xml[start..];
    let open = rest.find("href")?;
    let after = &rest[open..];
    let value_start = after.find('>')? + 1;
    let value_end = after[value_start..].find('<')? + value_start;
    Some(after[value_start..value_end].trim().to_string())
}

fn absolute(href: &str) -> String {
    if href.starts_with("http") {
        href.to_string()
    } else {
        format!("{FASTMAIL_BASE}{href}")
    }
}
