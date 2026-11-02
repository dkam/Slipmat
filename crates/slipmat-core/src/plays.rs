// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Every song Slipmat has played, and how much of each.
//!
//! **State that accumulates.** Like the session it cannot be recomputed, so it
//! lives in `$XDG_STATE_HOME/slipmat/plays.sqlite`; unlike the session it only
//! grows. Signing out does not touch it: that loses the account, not what you
//! listened to.
//!
//! SQLite rather than a file of lines because plays will arrive late and from
//! more than one place — Apple's recently played list, a ListenBrainz import
//! that can run to 160,000 listens — and be merged, then counted for any week
//! or month across years. The daemon is the only writer; WAL lets a client read
//! while it writes.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

/// The schema this build reads and writes. A newer file is refused rather than
/// misread.
const VERSION: i64 = 1;

const SCHEMA: &str = "
    CREATE TABLE plays (
        id INTEGER PRIMARY KEY,
        played_at INTEGER NOT NULL,
        catalog_id TEXT,
        title TEXT NOT NULL,
        artist TEXT NOT NULL,
        album TEXT NOT NULL,
        duration_ms INTEGER NOT NULL,
        listened_ms INTEGER NOT NULL
    );
    CREATE INDEX plays_by_time ON plays (played_at);
";

/// One time a song was heard. The same song twice is two plays.
#[derive(Debug, Clone, PartialEq)]
pub struct Play {
    /// When listening started, in Unix seconds.
    pub played_at: i64,
    pub catalog_id: Option<String>,
    /// Kept as text, not looked up from the id: a song can leave the catalog,
    /// and its history should still say what it was.
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    /// How much of it actually played. Seeking past part of a song is not
    /// listening to it.
    pub listened_ms: u64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// The history on disk, created the first time it is needed.
    pub fn open() -> Result<Self> {
        let dir = crate::paths::state_dir().context("no home directory for the play history")?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Self::at(&dir.join("plays.sqlite"))
    }

    fn at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::prepare(conn)
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        // A client reading while the daemon writes waits rather than failing.
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update_and_check(None, "journal_mode", "wal", |row| row.get::<_, String>(0))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => conn.execute_batch(&format!(
                "BEGIN; {SCHEMA} PRAGMA user_version = {VERSION}; COMMIT;"
            ))?,
            VERSION => {}
            newer => anyhow::bail!(
                "the play history is version {newer}, and this Slipmat reads version {VERSION}"
            ),
        }
        Ok(Self { conn })
    }

    pub fn record(&self, play: &Play) -> Result<()> {
        self.conn.execute(
            "INSERT INTO plays
                (played_at, catalog_id, title, artist, album, duration_ms, listened_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                play.played_at,
                play.catalog_id,
                play.title,
                play.artist,
                play.album,
                play.duration_ms,
                play.listened_ms,
            ],
        )?;
        Ok(())
    }

    /// The latest plays, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<Play>> {
        let mut statement = self.conn.prepare(
            "SELECT played_at, catalog_id, title, artist, album, duration_ms, listened_ms
             FROM plays ORDER BY played_at DESC, id DESC LIMIT ?1",
        )?;
        let plays = statement
            .query_map([limit], |row| {
                Ok(Play {
                    played_at: row.get(0)?,
                    catalog_id: row.get(1)?,
                    title: row.get(2)?,
                    artist: row.get(3)?,
                    album: row.get(4)?,
                    duration_ms: row.get(5)?,
                    listened_ms: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(plays)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(title: &str, played_at: i64) -> Play {
        Play {
            played_at,
            catalog_id: Some(format!("id-{title}")),
            title: title.into(),
            artist: "Tycho".into(),
            album: "Awake".into(),
            duration_ms: 270_000,
            listened_ms: 270_000,
        }
    }

    fn titles(plays: &[Play]) -> Vec<(&str, i64)> {
        plays
            .iter()
            .map(|p| (p.title.as_str(), p.played_at))
            .collect()
    }

    #[test]
    fn plays_come_back_newest_first_and_a_song_twice_is_two() {
        let store = Store::in_memory().unwrap();
        store.record(&play("Awake", 100)).unwrap();
        store.record(&play("Montana", 200)).unwrap();
        store.record(&play("Awake", 300)).unwrap();
        assert_eq!(
            titles(&store.recent(10).unwrap()),
            [("Awake", 300), ("Montana", 200), ("Awake", 100)]
        );
        assert_eq!(titles(&store.recent(1).unwrap()), [("Awake", 300)]);
    }

    #[test]
    fn a_play_reads_back_exactly_as_it_was_written() {
        // An upload has no catalog id, and a skip is a play too.
        let store = Store::in_memory().unwrap();
        let skipped = Play {
            catalog_id: None,
            listened_ms: 4_500,
            ..play("L", 100)
        };
        store.record(&skipped).unwrap();
        assert_eq!(store.recent(10).unwrap(), [skipped]);
    }

    #[test]
    fn a_history_from_a_newer_slipmat_is_refused_rather_than_misread() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", VERSION + 1)
            .unwrap();
        assert!(Store::prepare(conn).is_err());
    }

    #[test]
    fn reopening_the_file_keeps_what_was_recorded() {
        let path =
            std::env::temp_dir().join(format!("slipmat-plays-{}.sqlite", std::process::id()));
        Store::at(&path)
            .unwrap()
            .record(&play("Awake", 100))
            .unwrap();
        let reopened = Store::at(&path).unwrap().recent(10).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        assert_eq!(titles(&reopened), [("Awake", 100)]);
    }
}
