// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Home, kept between runs so the shelves are on screen before Apple answers.
//!
//! Rederivable by definition — Apple will send the same shelves again, and it
//! even says when it will change them (`nextUpdateDate`). So this is a **cache**
//! beside `library.json`, and never `$XDG_STATE_HOME`: losing it costs one slow
//! launch, and nobody chose its contents.
//!
//! **The date is the point.** Refetching Home on every launch ignores a hint
//! Apple volunteers, and the shelves would be identical anyway. [`Home::stale`]
//! is the whole decision: a shelf Apple has dated and whose date has passed is
//! worth a refetch, and the rest are not.
//!
//! **Only dated shelves count.** The stations shelf never carries one — it
//! changes when you listen — and treating an undated shelf as stale made every
//! launch refetch all three requests, since almost every account has a personal
//! station. A cache with no dates at all is the one case that always refetches.
//!
//! Read tolerantly and written best-effort, like the library: an unreadable
//! file is an empty Home, never an error.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::music::types::Shelf;

/// Bumped when a field changes *meaning* — see `library_cache::VERSION`.
const VERSION: u32 = 1;

#[derive(Debug, Default, Deserialize)]
pub struct Home {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    pub shelves: Vec<Shelf>,
}

impl Home {
    pub fn is_empty(&self) -> bool {
        self.shelves.is_empty()
    }

    /// Whether Apple should be asked again.
    ///
    /// One expired shelf refetches all of them: they arrive together, in one
    /// request, and a half-refreshed Home is a screen with two vintages on it.
    ///
    /// **An undated shelf is not an expired one.** The fallback stations shelf
    /// never carries a date, so counting it made the date meaningless — every
    /// launch refetched all three requests. Undated shelves ride along with the
    /// dated ones; a cache with no dates at all is the one case with nothing to
    /// go on, and always refetches.
    pub fn stale(&self, now: u64) -> bool {
        let mut dated = self
            .shelves
            .iter()
            .filter_map(|shelf| epoch_seconds(&shelf.next_update))
            .peekable();
        if dated.peek().is_none() {
            return true;
        }
        dated.any(|due| due <= now)
    }
}

#[derive(Serialize)]
struct Writing<'a> {
    version: u32,
    shelves: &'a [Shelf],
}

fn cache_file() -> Option<PathBuf> {
    Some(crate::paths::cache_dir()?.join("home.json"))
}

/// Now, in the only unit compared against Apple's dates.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub fn load() -> Home {
    let Some(path) = cache_file() else {
        return Home::default();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Home::default();
    };
    parse(&raw)
}

/// The half of `load` that is not I/O, so tests never touch a home directory.
fn parse(raw: &str) -> Home {
    match serde_json::from_str::<Home>(raw) {
        Ok(home) if home.version == VERSION => home,
        Ok(home) => {
            tracing::debug!(found = home.version, want = VERSION, "home cache is old");
            Home::default()
        }
        Err(err) => {
            tracing::debug!(?err, "home cache unreadable; ignoring");
            Home::default()
        }
    }
}

/// Returns whether it reached disk — the caller publishes only what it saved,
/// the way `commit_library` does, so a failed write cannot look like a refresh.
pub fn save(shelves: &[Shelf]) -> bool {
    let Some(path) = cache_file() else {
        return false;
    };
    let Ok(json) = serde_json::to_string(&Writing {
        version: VERSION,
        shelves,
    }) else {
        return false;
    };
    if let Err(err) = crate::artwork::write_atomically(&path, json.as_bytes()) {
        tracing::debug!(?err, "could not save the home cache");
        return false;
    }
    true
}

/// Forget it — the user signed out, and these shelves were theirs.
pub fn clear() {
    if let Some(path) = cache_file() {
        let _ = std::fs::remove_file(path);
    }
}

/// `"2026-07-27T04:00:00Z"` → seconds since the epoch.
///
/// Hand-rolled because the project's dependency list is pinned and a date
/// library would be a whole crate for one comparison. A trailing offset is
/// **ignored**, not honoured: this decides whether a refetch is worth making,
/// and being an hour out on a hint costs one request, where parsing it properly
/// would cost a crate.
fn epoch_seconds(iso: &str) -> Option<u64> {
    let date = iso.get(0..10)?;
    let time = iso.get(11..19)?;
    let mut parts = date.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut clock = time.split(':');
    let hour = clock.next()?.parse::<i64>().ok()?;
    let minute = clock.next()?.parse::<i64>().ok()?;
    let second = clock.next()?.parse::<i64>().ok()?;
    if clock.next().is_some() || !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    if !(0..60).contains(&second) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(seconds).ok()
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`, which is the shortest correct form of
/// this that exists.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::types::{Album, Artwork, Playlist, ShelfItem};

    fn shelf(title: &str, next_update: &str) -> Shelf {
        Shelf {
            title: title.into(),
            next_update: next_update.into(),
            items: vec![ShelfItem::Album(Album {
                id: "1440857781".into(),
                date_added: String::new(),
                name: title.into(),
                artist: "Radiohead".into(),
                artwork: Some(Artwork::new("https://x/{w}x{h}bb.jpg")),
                year: "2011".into(),
                track_count: 8,
                library: false,
            })],
        }
    }

    fn write(home: &Home) -> String {
        serde_json::to_string(&Writing {
            version: home.version,
            shelves: &home.shelves,
        })
        .unwrap()
    }

    #[test]
    fn a_shelf_survives_the_round_trip() {
        let saved = Home {
            version: VERSION,
            shelves: vec![shelf("Made For You", "2026-07-27T04:00:00Z")],
        };
        let back = parse(&write(&saved));
        assert_eq!(back.shelves, saved.shelves);
    }

    #[test]
    fn a_playlist_tile_survives_as_a_playlist() {
        // The tag is the whole encoding: a mis-serialised `ShelfItem` would
        // come back as the wrong kind of tile and open the wrong page.
        let raw = serde_json::to_string(&ShelfItem::Playlist(Playlist {
            id: "p.abc".into(),
            date_added: String::new(),
            last_modified: String::new(),
            name: "Game on".into(),
            curator: String::new(),
            description: String::new(),
            artwork: None,
            library: false,
        }))
        .unwrap();
        assert!(raw.contains("\"kind\":\"playlist\""));
        assert!(matches!(
            serde_json::from_str::<ShelfItem>(&raw),
            Ok(ShelfItem::Playlist(_))
        ));
    }

    #[test]
    fn a_cache_with_no_dates_at_all_is_worth_refetching() {
        // A cache of nothing but undated shelves has nothing to go on.
        assert!(
            Home {
                version: VERSION,
                shelves: vec![shelf("Stations", "")],
            }
            .stale(1_800_000_000)
        );
    }

    #[test]
    fn an_undated_shelf_rides_along_with_a_fresh_dated_one() {
        // The rule this pins: every account has a personal station, its shelf
        // has no date, and counting that as stale refetched all three requests
        // on every launch — which made `nextUpdateDate` decorative.
        let home = Home {
            version: VERSION,
            shelves: vec![
                shelf("Stations", ""),
                shelf("Made For You", "2030-01-01T00:00:00Z"),
            ],
        };
        assert!(!home.stale(now()));
    }

    #[test]
    fn a_dated_shelf_is_fresh_until_its_date() {
        let home = Home {
            version: VERSION,
            shelves: vec![shelf("Made For You", "2026-07-27T04:00:00Z")],
        };
        let due = epoch_seconds("2026-07-27T04:00:00Z").unwrap();
        assert!(!home.stale(due - 1));
        assert!(home.stale(due));
        assert!(home.stale(due + 86_400));
    }

    #[test]
    fn one_stale_shelf_refetches_them_all() {
        let home = Home {
            version: VERSION,
            shelves: vec![
                shelf("Stations", ""),
                shelf("Made For You", "2030-01-01T00:00:00Z"),
                shelf("Recently Played", "2020-01-01T00:00:00Z"),
            ],
        };
        assert!(home.stale(now()));
    }

    #[test]
    fn an_empty_home_is_stale_and_a_file_we_cannot_read_is_survivable() {
        assert!(Home::default().stale(now()));
        assert!(parse("").is_empty());
        assert!(parse("{").is_empty());
        assert!(parse(r#"{"version":1,"shelves":"not a list"}"#).is_empty());
    }

    #[test]
    fn an_older_format_is_ignored_rather_than_misread() {
        let raw = write(&Home {
            version: VERSION - 1,
            shelves: vec![shelf("Made For You", "2026-07-27T04:00:00Z")],
        });
        assert!(serde_json::from_str::<Home>(&raw).is_ok());
        assert!(parse(&raw).is_empty());
    }

    #[test]
    fn apple_dates_parse_to_the_second() {
        // 1970-01-01 is zero, a date Apple actually sends, and a leap day —
        // the three places a hand-rolled civil-date algorithm goes wrong.
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("2024-02-29T12:34:56Z"), Some(1_709_210_096));
        // Fractional seconds are truncated rather than rejected.
        assert_eq!(
            epoch_seconds("2026-07-27T04:00:00.123Z"),
            epoch_seconds("2026-07-27T04:00:00Z")
        );
        // Nothing, or something that is not a date at all.
        assert_eq!(epoch_seconds(""), None);
        assert_eq!(epoch_seconds("tomorrow"), None);
        assert_eq!(epoch_seconds("2026-13-01T00:00:00Z"), None);
        assert_eq!(epoch_seconds("2026-07-27T24:00:00Z"), None);
    }
}
