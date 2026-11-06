// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! History: what was heard, by day, newest first.
//!
//! The daemon keeps the plays and says when there is a new one. This client
//! reads them and does its own grouping, the same split as the library: which
//! day a play falls on is a question about this screen's clock and its words.

use std::collections::HashSet;

use relm4::ComponentSender;
use relm4::gtk::prelude::*;
use relm4::gtk::{self, gio, glib};

use slipmat_core::ipc::{PlayMode, Request};
use slipmat_core::plays::{Play, Store};

use super::{AppModel, CommandMsg};
use crate::components::history_row::{HistoryItem, Row};

/// How far back History reads. A bound until it pages by day, which a
/// ListenBrainz import will need.
const SHOWN: usize = 1_000;

impl AppModel {
    /// Read the plays again, off the GTK thread.
    pub(super) fn load_history(&self, sender: &ComponentSender<Self>) {
        sender.oneshot_command(async {
            let read = tokio::task::spawn_blocking(read_plays).await;
            CommandMsg::History(match read {
                Ok(read) => read.map_err(|err| format!("{err:#}")),
                Err(err) => Err(err.to_string()),
            })
        });
    }

    pub(super) fn fill_history(&mut self, read: Result<Vec<Play>, String>) {
        match read {
            Ok(plays) => {
                self.plays = plays;
                self.rebuild_history();
            }
            Err(err) => tracing::warn!("could not read the play history: {err}"),
        }
    }

    /// Bring the list up to date.
    ///
    /// A splice rather than a rebuild, because a rebuild loses the scroll and
    /// this runs every time a song ends. New plays land at the top, so what
    /// changes is a few rows there and everything below them stays.
    pub(super) fn rebuild_history(&mut self) {
        let Ok(now) = glib::DateTime::now_local() else {
            return;
        };
        let favourites: HashSet<&str> = self
            .all_tracks
            .iter()
            .filter(|track| track.favorite)
            .filter_map(|track| track.catalog_id.as_deref())
            .collect();
        let rows = rows(
            &self.plays,
            &self.library_query,
            &now,
            Clock::of_desktop(),
            &favourites,
        );

        let kept = common_tail(&self.history_rows, &rows);
        let stale = self.history_rows.len() - kept;
        let fresh = rows.len() - kept;
        if stale == 0 && fresh == 0 {
            return;
        }
        let at_the_top = self
            .history
            .view
            .vadjustment()
            .is_none_or(|scroll| scroll.value() <= 0.0);
        crate::components::drop_focus(&self.history.view);
        for _ in 0..stale {
            self.history.remove(0);
        }
        for (at, row) in rows[..fresh].iter().enumerate() {
            let item = HistoryItem::new(
                row.clone(),
                self.history_art_widgets.clone(),
                self.tile_art_request.clone(),
                self.open_history_link.clone(),
                self.row_overrides.clone(),
            );
            self.history.insert(at as u32, item);
        }
        // A list keeps its eye on the row it was showing, so left alone a new
        // play would land just above the window of someone watching for it.
        if at_the_top && !rows.is_empty() {
            self.history
                .view
                .scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
        self.history_rows = rows;
    }

    /// Replay the day a row belongs to, from that row.
    pub(super) fn play_history_from(&mut self, at: u32) {
        let Some((ids, index)) = day_from(&self.history_rows, at as usize) else {
            self.toast("This song can’t be streamed");
            return;
        };
        tracing::info!(rows = ids.len(), index, "replaying a day");
        self.ask(Request::Play {
            ids,
            index,
            start: PlayMode::Clicked,
        });
    }
}

fn read_plays() -> anyhow::Result<Vec<Play>> {
    match Store::open_existing()? {
        Some(store) => store.recent(SHOWN),
        None => Ok(Vec::new()),
    }
}

/// How the desktop writes a time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clock {
    TwentyFourHour,
    TwelveHour,
}

impl Clock {
    /// GNOME's own setting. Looked up first, because asking for a schema that
    /// is not installed aborts the process.
    fn of_desktop() -> Self {
        const SCHEMA: &str = "org.gnome.desktop.interface";
        let twelve = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(SCHEMA, true))
            .filter(|schema| schema.has_key("clock-format"))
            .is_some_and(|_| gio::Settings::new(SCHEMA).string("clock-format") == "12h");
        if twelve {
            Self::TwelveHour
        } else {
            Self::TwentyFourHour
        }
    }

    fn format(self) -> &'static str {
        match self {
            Self::TwentyFourHour => "%H:%M",
            Self::TwelveHour => "%-l:%M %p",
        }
    }
}

/// Every row History shows, for plays newest first: each day's heading, then
/// its plays. Skips are left out, of the rows and of the day's totals.
fn rows(
    plays: &[Play],
    query: &str,
    now: &glib::DateTime,
    clock: Clock,
    favourites: &HashSet<&str>,
) -> Vec<Row> {
    let needle = query.trim().to_lowercase();
    let zone = now.timezone();
    let heard = plays
        .iter()
        .filter(|play| !play.skipped() && matches(play, &needle))
        .filter_map(|play| {
            let when = glib::DateTime::from_unix_utc(play.played_at)
                .and_then(|utc| utc.to_timezone(&zone))
                .ok()?;
            Some((when, play))
        })
        .collect::<Vec<_>>();

    let mut rows = Vec::with_capacity(heard.len());
    for day in heard.chunk_by(|(a, _), (b, _)| same_day(a, b)) {
        let (first, _) = &day[0];
        rows.push(Row::Day {
            heading: heading(first, now),
            summary: summary(day.iter().map(|(_, play)| *play)),
        });
        rows.extend(day.iter().map(|(when, play)| {
            Row::Play {
                time: when
                    .format(clock.format())
                    .map(|time| time.trim().to_owned())
                    .unwrap_or_default(),
                play: (*play).clone(),
                favorite: play
                    .catalog_id
                    .as_deref()
                    .is_some_and(|id| favourites.contains(id)),
            }
        }));
    }
    rows
}

fn matches(play: &Play, needle: &str) -> bool {
    needle.is_empty()
        || [&play.title, &play.artist, &play.album]
            .iter()
            .any(|field| field.to_lowercase().contains(needle))
}

fn same_day(a: &glib::DateTime, b: &glib::DateTime) -> bool {
    (a.year(), a.day_of_year()) == (b.year(), b.day_of_year())
}

/// "Today", "Yesterday", then "Sunday 26 October", with the year once it is
/// not this one.
fn heading(day: &glib::DateTime, now: &glib::DateTime) -> String {
    if same_day(day, now) {
        return "Today".into();
    }
    if now
        .add_days(-1)
        .is_ok_and(|yesterday| same_day(day, &yesterday))
    {
        return "Yesterday".into();
    }
    let format = if day.year() == now.year() {
        "%A %-d %B"
    } else {
        "%A %-d %B %Y"
    };
    day.format(format).map(Into::into).unwrap_or_default()
}

/// "23 plays · 1 h 32 min".
fn summary<'a>(plays: impl Iterator<Item = &'a Play>) -> String {
    let (count, listened_ms) = plays.fold((0usize, 0u64), |(count, ms), play| {
        (count + 1, ms + play.listened_ms)
    });
    let noun = if count == 1 { "play" } else { "plays" };
    format!("{count} {noun} · {}", total(listened_ms))
}

/// To the nearest minute, and never nothing: a play in History was heard.
fn total(ms: u64) -> String {
    let minutes = ((ms + 30_000) / 60_000).max(1);
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// How many rows at the end of `old` are the same as the end of `new`.
fn common_tail(old: &[Row], new: &[Row]) -> usize {
    old.iter()
        .rev()
        .zip(new.iter().rev())
        .take_while(|(a, b)| a == b)
        .count()
}

/// The day `at` falls in as a queue, and where in it `at` is.
///
/// Oldest first, because that is the order it was heard in, whatever order the
/// screen reads in. All of the day rather than the rest of it, as any list
/// plays: going back from the clicked song reaches what came before it.
fn day_from(rows: &[Row], at: usize) -> Option<(Vec<String>, usize)> {
    let is_day = |row: &Row| matches!(row, Row::Day { .. });
    let start = rows
        .get(..at)?
        .iter()
        .rposition(is_day)
        .map_or(0, |i| i + 1);
    let end = rows[at..]
        .iter()
        .position(is_day)
        .map_or(rows.len(), |i| at + i);

    let day: Vec<(usize, &String)> = (start..end)
        .rev()
        .filter_map(|row| match &rows[row] {
            Row::Play { play, .. } => play.catalog_id.as_ref().map(|id| (row, id)),
            Row::Day { .. } => None,
        })
        .collect();
    let index = day.iter().position(|(row, _)| *row == at)?;
    Some((day.into_iter().map(|(_, id)| id.clone()).collect(), index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::plays::Source;

    fn at(day: i32, hour: i32, minute: i32) -> i64 {
        glib::DateTime::from_utc(2026, 11, day, hour, minute, 0.0)
            .unwrap()
            .to_unix()
    }

    fn now() -> glib::DateTime {
        glib::DateTime::from_utc(2026, 11, 7, 21, 30, 0.0).unwrap()
    }

    fn play(title: &str, played_at: i64) -> Play {
        Play {
            played_at,
            catalog_id: Some(format!("id-{title}")),
            title: title.into(),
            artist: "The Avalanches".into(),
            album: "Since I Left You".into(),
            duration_ms: 240_000,
            listened_ms: 240_000,
            source: Source::Here,
            artwork: None,
        }
    }

    fn shown(plays: &[Play], query: &str) -> Vec<Row> {
        rows(plays, query, &now(), Clock::TwentyFourHour, &HashSet::new())
    }

    /// Each row as one short string, so a whole screen fits in an assertion.
    fn outline(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Day { heading, summary } => format!("{heading} / {summary}"),
                Row::Play { time, play, .. } => format!("{time} {}", play.title),
            })
            .collect()
    }

    #[test]
    fn plays_fall_under_the_day_they_were_heard() {
        let plays = [
            play("Frontier Psychiatrist", at(7, 21, 14)),
            play("Since I Left You", at(7, 20, 58)),
            play("Teardrop", at(6, 23, 2)),
            play("Let It Happen", at(1, 9, 5)),
        ];
        assert_eq!(
            outline(&shown(&plays, "")),
            [
                "Today / 2 plays · 8 min",
                "21:14 Frontier Psychiatrist",
                "20:58 Since I Left You",
                "Yesterday / 1 play · 4 min",
                "23:02 Teardrop",
                "Sunday 1 November / 1 play · 4 min",
                "09:05 Let It Happen",
            ]
        );
    }

    #[test]
    fn a_day_from_another_year_says_which() {
        let last_year = glib::DateTime::from_utc(2025, 10, 26, 18, 0, 0.0)
            .unwrap()
            .to_unix();
        let Row::Day { heading, .. } = &shown(&[play("Teardrop", last_year)], "")[0] else {
            panic!("a day comes first");
        };
        assert_eq!(heading, "Sunday 26 October 2025");
    }

    #[test]
    fn a_twelve_hour_desktop_reads_a_twelve_hour_clock() {
        let rows = rows(
            &[play("Teardrop", at(7, 15, 4))],
            "",
            &now(),
            Clock::TwelveHour,
            &HashSet::new(),
        );
        assert!(matches!(&rows[1], Row::Play { time, .. } if time == "3:04 PM"));
    }

    #[test]
    fn a_skip_is_kept_out_of_the_list_and_out_of_the_day_it_fell_on() {
        let skipped = Play {
            listened_ms: 29_000,
            ..play("Skipped", at(7, 21, 0))
        };
        assert_eq!(
            outline(&shown(&[skipped.clone(), play("Heard", at(7, 20, 0))], "")),
            ["Today / 1 play · 4 min", "20:00 Heard"]
        );
        assert!(shown(&[skipped], "").is_empty(), "no heading over nothing");
    }

    #[test]
    fn the_search_box_reaches_title_artist_and_album() {
        let massive = Play {
            artist: "Massive Attack".into(),
            album: "Mezzanine".into(),
            ..play("Teardrop", at(7, 20, 0))
        };
        let plays = [massive, play("Radio", at(7, 19, 0))];
        for query in ["teardrop", " MASSIVE ", "mezzanine"] {
            assert_eq!(
                outline(&shown(&plays, query)),
                ["Today / 1 play · 4 min", "20:00 Teardrop"],
                "{query}"
            );
        }
    }

    #[test]
    fn a_favourite_is_starred_where_the_library_says_so() {
        let favourites = HashSet::from(["id-Teardrop"]);
        let plays = [play("Teardrop", at(7, 20, 0)), play("Radio", at(7, 19, 0))];
        let rows = rows(&plays, "", &now(), Clock::TwentyFourHour, &favourites);
        let starred: Vec<bool> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Play { favorite, .. } => Some(*favorite),
                Row::Day { .. } => None,
            })
            .collect();
        assert_eq!(starred, [true, false]);
    }

    #[test]
    fn a_days_total_reads_in_hours_and_minutes() {
        assert_eq!(total(45_000), "1 min");
        assert_eq!(total(48 * 60_000), "48 min");
        assert_eq!(total(60 * 60_000), "1 h");
        assert_eq!(total(92 * 60_000), "1 h 32 min");
    }

    #[test]
    fn replaying_runs_in_the_order_it_was_heard_and_stays_in_the_day() {
        let plays = [
            play("Third", at(7, 21, 0)),
            play("Second", at(7, 20, 0)),
            play("First", at(7, 19, 0)),
            play("Yesterday", at(6, 19, 0)),
        ];
        let rows = shown(&plays, "");
        // Row 0 is the heading, so "Second" is row 2.
        assert_eq!(
            day_from(&rows, 2),
            Some((
                vec!["id-First".into(), "id-Second".into(), "id-Third".into()],
                1
            ))
        );
    }

    #[test]
    fn a_song_with_no_catalog_id_leaves_the_queue_but_not_the_list() {
        let upload = Play {
            catalog_id: None,
            ..play("Upload", at(7, 20, 0))
        };
        let plays = [
            play("Later", at(7, 21, 0)),
            upload,
            play("Earlier", at(7, 19, 0)),
        ];
        let rows = shown(&plays, "");
        assert_eq!(
            day_from(&rows, 2),
            None,
            "the upload itself cannot start one"
        );
        assert_eq!(
            day_from(&rows, 1),
            Some((vec!["id-Earlier".into(), "id-Later".into()], 1))
        );
    }

    #[test]
    fn a_heading_is_not_a_place_to_start() {
        let rows = shown(&[play("Teardrop", at(7, 20, 0))], "");
        assert_eq!(day_from(&rows, 0), None);
        assert_eq!(day_from(&rows, 99), None);
    }

    #[test]
    fn a_new_play_touches_only_the_top_of_the_list() {
        let before = shown(
            &[
                play("Second", at(7, 20, 0)),
                play("Yesterday", at(6, 19, 0)),
            ],
            "",
        );
        let after = shown(
            &[
                play("Third", at(7, 21, 0)),
                play("Second", at(7, 20, 0)),
                play("Yesterday", at(6, 19, 0)),
            ],
            "",
        );
        // Today's heading changes with its total; "Second" and all of
        // yesterday stay where they are.
        assert_eq!(common_tail(&before, &after), 3);
    }
}
