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
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OpenFlags, params};

use crate::music::types::Artwork;

/// The schema this build reads and writes. A newer file is refused rather than
/// misread.
const VERSION: i64 = 2;

const SCHEMA: &str = "
    CREATE TABLE plays (
        id INTEGER PRIMARY KEY,
        played_at INTEGER NOT NULL,
        catalog_id TEXT,
        title TEXT NOT NULL,
        artist TEXT NOT NULL,
        album TEXT NOT NULL,
        duration_ms INTEGER NOT NULL,
        listened_ms INTEGER NOT NULL,
        source TEXT NOT NULL DEFAULT 'here',
        artwork TEXT
    );
    CREATE INDEX plays_by_time ON plays (played_at);
";

/// Version 1 knew only Slipmat's own plays, so every one of them was heard here.
const FROM_1: &str = "
    ALTER TABLE plays ADD COLUMN source TEXT NOT NULL DEFAULT 'here';
    ALTER TABLE plays ADD COLUMN artwork TEXT;
";

/// Heard for less than this, a play is a skip. Kept, because a skip says
/// something, and left out of History unless asked for.
pub const SKIP_MS: u64 = 30_000;

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
    pub source: Source,
    /// Kept with the play for the same reason as the title.
    pub artwork: Option<Artwork>,
}

impl Play {
    pub fn skipped(&self) -> bool {
        self.listened_ms < SKIP_MS
    }
}

/// Where a play came from, which is also how far its time can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Slipmat played it, so the time is exact.
    Here,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Here => "here",
        }
    }
}

impl ToSql for Source {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl FromSql for Source {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value.as_str()? {
            "here" => Ok(Self::Here),
            _ => Err(FromSqlError::InvalidType),
        }
    }
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

    /// The history as a client reads it, or `None` if nothing has played yet.
    ///
    /// Never created or migrated from here: the daemon is the only writer.
    pub fn open_existing() -> Result<Option<Self>> {
        match crate::paths::state_dir() {
            Some(dir) => Self::existing(&dir.join("plays.sqlite")),
            None => Ok(None),
        }
    }

    fn existing(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        // Writable without being created: a reader of a WAL file may need to
        // make its shared-memory index.
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(1))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        anyhow::ensure!(
            version == VERSION,
            "the play history is version {version}, and this Slipmat reads version {VERSION}"
        );
        Ok(Some(Self { conn }))
    }

    fn at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::prepare(conn)
    }

    /// A history that lasts as long as this process, for tests here and in the
    /// daemon.
    pub fn in_memory() -> Result<Self> {
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
            1 => conn.execute_batch(&format!(
                "BEGIN; {FROM_1} PRAGMA user_version = {VERSION}; COMMIT;"
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
                (played_at, catalog_id, title, artist, album, duration_ms, listened_ms,
                 source, artwork)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                play.played_at,
                play.catalog_id,
                play.title,
                play.artist,
                play.album,
                play.duration_ms,
                play.listened_ms,
                play.source,
                play.artwork.as_ref().map(Artwork::template),
            ],
        )?;
        Ok(())
    }

    /// The latest plays, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<Play>> {
        let mut statement = self.conn.prepare(
            "SELECT played_at, catalog_id, title, artist, album, duration_ms, listened_ms,
                    source, artwork
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
                    source: row.get(7)?,
                    artwork: row.get::<_, Option<String>>(8)?.map(Artwork::new),
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
            source: Source::Here,
            artwork: Some(Artwork::new("https://example.com/{w}x{h}bb.jpg")),
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
        // An upload has no catalog id or artwork, and a skip is a play too.
        let store = Store::in_memory().unwrap();
        let skipped = Play {
            catalog_id: None,
            listened_ms: 4_500,
            artwork: None,
            ..play("L", 100)
        };
        store.record(&skipped).unwrap();
        assert_eq!(store.recent(10).unwrap(), [skipped]);
    }

    #[test]
    fn a_first_version_history_keeps_its_plays_and_says_they_were_here() {
        // What 0.12 builds of the daemon wrote: no source and no artwork.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE plays (
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
            INSERT INTO plays
                (played_at, catalog_id, title, artist, album, duration_ms, listened_ms)
                VALUES (100, 'id-Awake', 'Awake', 'Tycho', 'Awake', 270000, 270000);
            PRAGMA user_version = 1;",
        )
        .unwrap();
        let store = Store::prepare(conn).unwrap();
        assert_eq!(
            store.recent(10).unwrap(),
            [Play {
                artwork: None,
                ..play("Awake", 100)
            }]
        );
        store.record(&play("Montana", 200)).unwrap();
        assert_eq!(
            titles(&store.recent(10).unwrap()),
            [("Montana", 200), ("Awake", 100)]
        );
    }

    #[test]
    fn under_thirty_seconds_is_a_skip() {
        let heard = |listened_ms| Play {
            listened_ms,
            ..play("Awake", 100)
        };
        assert!(heard(29_999).skipped());
        assert!(!heard(30_000).skipped());
    }

    #[test]
    fn a_client_finds_no_history_where_nothing_has_played() {
        let missing =
            std::env::temp_dir().join(format!("slipmat-none-{}.sqlite", std::process::id()));
        assert!(Store::existing(&missing).unwrap().is_none());
        assert!(
            !missing.exists(),
            "a client must not create the daemon's file"
        );
    }

    #[test]
    fn a_client_refuses_a_history_the_daemon_has_not_brought_up_to_date() {
        // The daemon is the only writer, so a client never migrates.
        let path = std::env::temp_dir().join(format!("slipmat-old-{}.sqlite", std::process::id()));
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 1)
            .unwrap();
        let opened = Store::existing(&path);
        let _ = std::fs::remove_file(&path);
        assert!(opened.is_err());
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
