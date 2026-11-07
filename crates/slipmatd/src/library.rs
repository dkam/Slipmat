// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the daemon knows about the library, and how a client asks for a slice
//! of it.

use slipmat_core::entry::Entry;
use slipmat_core::ipc::View;
use slipmat_core::library_cache;
use slipmat_core::music::types::{Album, Artist, Playlist, Track};
use slipmat_core::sort::{self, SortBy};
use std::collections::HashSet;

#[derive(Debug, Default, PartialEq)]
pub struct Library {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub playlists: Vec<Playlist>,
}

impl Library {
    /// What was on disk last time. An empty library is not an error — it means
    /// the first browse waits on Apple instead of answering instantly.
    pub fn from_cache() -> Self {
        let cached = library_cache::load();
        Self {
            tracks: cached.songs,
            albums: cached.albums,
            artists: cached.artists,
            playlists: cached.playlists,
        }
    }

    /// One window of one section.
    ///
    /// Returns the total *before* the window, so a client can size a scrollbar
    /// without asking for 535 rows it will not draw.
    pub fn browse(&self, ask: &Ask, dead: &HashSet<String>) -> (Vec<Entry>, usize) {
        let Ask {
            view,
            query,
            offset,
            limit,
            sort,
            reverse,
        } = *ask;
        let needle = query.trim().to_lowercase();
        let all: Vec<Entry> = match view {
            View::Songs => self
                .tracks
                .iter()
                .filter(|t| matches_track(t, &needle))
                .cloned()
                .map(Entry::Song)
                .collect(),
            View::Albums => self
                .albums
                .iter()
                .filter(|a| matches(&needle, &[&a.name, &a.artist]))
                .cloned()
                .map(Entry::Album)
                .collect(),
            View::Artists => self
                .artists
                .iter()
                .filter(|a| matches(&needle, &[&a.name]))
                .cloned()
                .map(Entry::Artist)
                .collect(),
            View::Playlists => self
                .playlists
                .iter()
                .filter(|p| matches(&needle, &[&p.name]))
                .cloned()
                .map(Entry::Playlist)
                .collect(),
        };

        // **Sorted before the window is taken.** Ordering one page is not
        // ordering the library: a client asking for the first forty rows of an
        // unsorted list would get forty arbitrary ones, neatly arranged.
        let mut all = all;
        sort_entries(&mut all, sort.valid_for(view));
        if reverse != sort.descends_by_default() {
            all.reverse();
        }
        sort::unplayable_last(&mut all, dead);

        let total = all.len();
        let window = all
            .into_iter()
            .skip(offset)
            // Zero means "the rest": a client that wants everything should not
            // have to guess a number bigger than the library.
            .take(if limit == 0 { usize::MAX } else { limit })
            .collect();
        (window, total)
    }
}

/// What a client asked `browse` for: [`Request::Browse`](slipmat_core::ipc::Request),
/// less the parts the daemon answers with.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    pub view: View,
    pub query: &'a str,
    pub offset: usize,
    pub limit: usize,
    pub sort: SortBy,
    pub reverse: bool,
}

/// Order a list of one kind of thing.
///
/// Each arm knows what it is holding, so the comparators take the real type
/// rather than an `Entry` that has to be unwrapped at every comparison.
fn sort_entries(entries: &mut [Entry], by: SortBy) {
    match entries.first() {
        Some(Entry::Song(_)) => entries.sort_by(|a, b| match (a, b) {
            (Entry::Song(a), Entry::Song(b)) => by.compare(a, b),
            _ => std::cmp::Ordering::Equal,
        }),
        Some(Entry::Album(_)) => entries.sort_by(|a, b| match (a, b) {
            (Entry::Album(a), Entry::Album(b)) => by.compare_album(a, b),
            _ => std::cmp::Ordering::Equal,
        }),
        Some(Entry::Artist(_)) => entries.sort_by(|a, b| match (a, b) {
            (Entry::Artist(a), Entry::Artist(b)) => SortBy::compare_artist(a, b),
            _ => std::cmp::Ordering::Equal,
        }),
        Some(Entry::Playlist(_)) => entries.sort_by(|a, b| match (a, b) {
            (Entry::Playlist(a), Entry::Playlist(b)) => by.compare_playlist(a, b),
            _ => std::cmp::Ordering::Equal,
        }),
        None => {}
    }
}

fn matches(needle: &str, fields: &[&str]) -> bool {
    needle.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(needle))
}

fn matches_track(track: &Track, needle: &str) -> bool {
    matches(needle, &[&track.title, &track.artist, &track.album])
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::music::types::TrackId;

    fn track(title: &str, artist: &str) -> Track {
        Track {
            date_added: String::new(),
            year: String::new(),
            favorite: false,
            in_library: true,
            library_id: None,
            id: TrackId(title.into()),
            catalog_id: Some(title.into()),
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration_ms: 0,
            track_number: 0,
            artwork: None,
        }
    }

    fn songs(query: &str, offset: usize, limit: usize) -> Ask<'_> {
        Ask {
            view: View::Songs,
            query,
            offset,
            limit,
            sort: SortBy::Title,
            reverse: false,
        }
    }

    fn library() -> Library {
        Library {
            tracks: vec![
                track("Wind Shear", "Pierce Fulton"),
                track("A Moment Apart", "ODESZA"),
                track("Colors", "Beck"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_window_reports_the_total_before_it_was_windowed() {
        // A client draws a screenful and a scrollbar. The scrollbar needs the
        // number it did not receive.
        let (rows, total) = library().browse(&songs("", 1, 1), &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(total, 3);
    }

    #[test]
    fn a_zero_limit_means_the_rest_rather_than_nothing() {
        // The alternative is every client guessing a number larger than the
        // library, which is the kind of thing that works until someone has
        // 20,000 songs.
        let (rows, _) = library().browse(&songs("", 0, 0), &HashSet::new());
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn the_query_reaches_past_the_title() {
        // Searching an artist you can spell but whose track titles you cannot
        // is the ordinary case, not the clever one.
        let (rows, total) = library().browse(&songs("odesza", 0, 0), &HashSet::new());
        assert_eq!(total, 1);
        assert!(matches!(&rows[0], Entry::Song(t) if t.title == "A Moment Apart"));
    }

    #[test]
    fn an_offset_past_the_end_is_empty_rather_than_a_panic() {
        let (rows, total) = library().browse(&songs("", 99, 10), &HashSet::new());
        assert!(rows.is_empty());
        assert_eq!(total, 3, "the total still describes the library");
    }

    #[test]
    fn unplayable_songs_stay_last_when_the_list_is_reversed() {
        // And before the window is taken, so a client's first page is the
        // playable songs, not a page of rows it cannot play.
        let dead = HashSet::from(["Colors".to_owned()]);
        let ask = Ask {
            reverse: true,
            ..songs("", 0, 0)
        };
        let (rows, _) = library().browse(&ask, &dead);
        let titles: Vec<&str> = rows.iter().map(Entry::title).collect();
        assert_eq!(titles, ["Wind Shear", "A Moment Apart", "Colors"]);
    }
}
