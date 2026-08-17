// SPDX-License-Identifier: MPL-2.0

//! Prints the class planning for the logged-in member's club.
//!
//! ```sh
//! export BIGBOX_EMAIL=you@example.com BIGBOX_PASSWORD=…
//! cargo run --example planning          # today
//! cargo run --example planning -- 7     # the next 7 days
//! ```

use bigbox_for_cosmic::api::Client;
use chrono::{Duration, Local};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let email = std::env::var("BIGBOX_EMAIL").map_err(|_| "set BIGBOX_EMAIL")?;
    let password = std::env::var("BIGBOX_PASSWORD").map_err(|_| "set BIGBOX_PASSWORD")?;

    let days: i64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(1);

    let client = Client::login(&email, &password).await?;
    let profile = client.profile();
    println!("Signed in as {} ({})", profile.full_name(), profile.number);

    let from = Local::now().date_naive();
    let to = from + Duration::days(days);

    let (directory, events) = tokio::try_join!(
        client.directory(),
        client.planning(&profile.club_id, from, to),
    )?;

    println!(
        "\n{} classes from {from} to {to} at {}\n",
        events.len(),
        directory
            .clubs
            .get(&profile.club_id)
            .and_then(|c| c.name.as_deref())
            .unwrap_or("your club"),
    );

    let mut current_day = None;
    for event in &events {
        let entry = directory.resolve(event, &profile.contact_id);

        let day = entry.start.map(|s| s.date());
        if day != current_day {
            if let Some(date) = day {
                println!("── {} ──", date.format("%A %-d %B"));
            }
            current_day = day;
        }

        println!(
            "  {:<13} {:<26} {:<20} {:<18} {:<12} {}",
            entry.time_range(),
            entry.name,
            entry.coach.as_deref().unwrap_or("—"),
            entry.studio.as_deref().unwrap_or("—"),
            entry.places_text(),
            if entry.booked { "✓ booked" } else { "" },
        );
    }

    let booked: Vec<_> = events
        .iter()
        .filter(|e| e.is_booked_by(&profile.contact_id))
        .collect();
    if !booked.is_empty() {
        println!("\nYou are booked onto {} of these.", booked.len());
    }

    Ok(())
}
