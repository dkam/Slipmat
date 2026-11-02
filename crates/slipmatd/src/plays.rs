// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Turning what the sidecar reports into plays for the history.
//!
//! The sidecar says which item is current and, a few times a second, where it
//! has got to. A play is one current item, from the moment it first moved until
//! something else became current. How much was heard is the sum of the small
//! forward steps between ticks, so a seek is not listening and a pause adds
//! nothing — and a queue restored on launch, paused, is not a play until it
//! moves.
//!
//! Every play is recorded, skips included. Whether one *counts* is a question
//! for whoever reads the history, so the rule can change without the rows.

use slipmat_core::player::protocol::{Event as PlayerEvent, Item};
use slipmat_core::plays::Play;

use crate::serve::Daemon;

/// A forward step longer than this between two ticks is a seek. Ticks arrive
/// about twice a second.
const STEP_MS: u64 = 2_000;

/// The play in progress, if anything has been current since the last one ended.
#[derive(Default)]
pub struct Listening {
    current: Option<Current>,
}

struct Current {
    /// Which queue entry this is. A song queued twice is two entries.
    key: String,
    item: Item,
    /// Began by going round rather than by becoming current.
    again: bool,
    started_at: Option<i64>,
    position_ms: u64,
    listened_ms: u64,
}

impl Listening {
    /// Fold one sidecar event in, and return the play it ended, if any.
    pub fn observe(&mut self, event: &PlayerEvent, now: i64) -> Option<Play> {
        match event {
            PlayerEvent::NowPlaying { item, .. } => self.now_playing(item.as_ref()),
            PlayerEvent::Position { position_ms, .. } => self.tick(*position_ms, now),
            _ => None,
        }
    }

    /// End the play in progress, because the player is going away.
    pub fn finish(&mut self) -> Option<Play> {
        self.current.take().and_then(Current::into_play)
    }

    fn now_playing(&mut self, item: Option<&Item>) -> Option<Play> {
        let key = item.map(key);
        if key.is_some() && key == self.current.as_ref().map(|c| c.key.clone()) {
            return None;
        }
        let ended = self
            .current
            .take()
            .filter(|c| !c.was_the_boundary())
            .and_then(Current::into_play);
        self.current = item.map(Current::new);
        ended
    }

    fn tick(&mut self, position_ms: u64, now: i64) -> Option<Play> {
        let current = self.current.as_ref()?;
        let ended = if current.went_round(position_ms) {
            let again = Current {
                again: true,
                ..Current::new(&current.item)
            };
            self.current.replace(again).and_then(Current::into_play)
        } else {
            None
        };
        if let Some(current) = &mut self.current {
            current.advance(position_ms, now);
        }
        ended
    }
}

/// Which queue entry an item is. Older sidecars send no occurrence id, and the
/// song is the best there is then.
fn key(item: &Item) -> String {
    if item.occurrence_id.is_empty() {
        item.catalog_id
            .clone()
            .or_else(|| item.id.clone())
            .unwrap_or_default()
    } else {
        item.occurrence_id.clone()
    }
}

impl Current {
    fn new(item: &Item) -> Self {
        Self {
            key: key(item),
            item: item.clone(),
            again: false,
            started_at: None,
            position_ms: 0,
            listened_ms: 0,
        }
    }

    fn advance(&mut self, position_ms: u64, now: i64) {
        let step = position_ms.saturating_sub(self.position_ms);
        if (1..=STEP_MS).contains(&step) {
            self.listened_ms += step;
            self.started_at.get_or_insert(now);
        }
        self.position_ms = position_ms;
    }

    /// Back at the start from the end, on the same queue entry: repeat-one,
    /// or the end of the song as MusicKit reports it (see `was_the_boundary`).
    fn went_round(&self, position_ms: u64) -> bool {
        self.listened_ms > 0
            && self.item.duration_ms > 0
            && position_ms < STEP_MS
            && self.position_ms + STEP_MS >= self.item.duration_ms
    }

    /// MusicKit starts the next song's clock a moment before it says the song
    /// changed. A replay that another song replaced within one step was that
    /// moment, not a replay.
    fn was_the_boundary(&self) -> bool {
        self.again && self.listened_ms <= STEP_MS
    }

    fn into_play(self) -> Option<Play> {
        Some(Play {
            played_at: self.started_at?,
            catalog_id: self.item.catalog_id,
            title: self.item.title,
            artist: self.item.artist,
            album: self.item.album,
            duration_ms: self.item.duration_ms,
            listened_ms: self.listened_ms,
        })
    }
}

/// Fold a sidecar event into the play in progress, and keep any play it ended.
pub fn observe(daemon: &Daemon, event: &PlayerEvent) {
    let ended = daemon.listening.borrow_mut().observe(event, unix_now());
    if let Some(play) = ended {
        keep(daemon, &play);
    }
}

/// Keep the play in progress: the player is going away and will not report
/// what it ended on.
pub fn finish(daemon: &Daemon) {
    let ended = daemon.listening.borrow_mut().finish();
    if let Some(play) = ended {
        keep(daemon, &play);
    }
}

fn keep(daemon: &Daemon, play: &Play) {
    let Some(history) = &daemon.history else {
        return;
    };
    match history.record(play) {
        Ok(()) => tracing::debug!(
            title = %play.title,
            listened_ms = play.listened_ms,
            "play recorded"
        ),
        Err(err) => tracing::warn!("could not record a play: {err:#}"),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipmat_core::player::protocol::Queue;

    const SONG_MS: u64 = 200_000;

    fn item(occurrence: &str, title: &str) -> Item {
        Item {
            occurrence_id: occurrence.into(),
            catalog_id: Some(format!("id-{title}")),
            title: title.into(),
            duration_ms: SONG_MS,
            ..Default::default()
        }
    }

    fn now_playing(listening: &mut Listening, item: &Item) -> Option<Play> {
        let event = PlayerEvent::NowPlaying {
            item: Some(item.clone()),
            queue: Queue::default(),
        };
        listening.observe(&event, 0)
    }

    /// Ticks at the given positions, one second apart from `from`.
    fn ticks(listening: &mut Listening, from: i64, positions: &[u64]) -> Option<Play> {
        let mut ended = None;
        for (n, &position_ms) in positions.iter().enumerate() {
            let event = PlayerEvent::Position {
                position_ms,
                duration_ms: SONG_MS,
            };
            ended = ended.or(listening.observe(&event, from + n as i64));
        }
        ended
    }

    #[test]
    fn a_song_heard_until_the_next_one_is_a_play_from_when_it_started() {
        let mut listening = Listening::default();
        now_playing(&mut listening, &item("run:1", "Awake"));
        ticks(&mut listening, 1_000, &[500, 1_000, 1_500, 2_000]);
        let play = now_playing(&mut listening, &item("run:2", "Montana")).expect("a play");
        assert_eq!(play.title, "Awake");
        assert_eq!(play.played_at, 1_000);
        assert_eq!(play.listened_ms, 2_000);
    }

    #[test]
    fn a_seek_is_not_listening_and_neither_is_a_pause() {
        let mut listening = Listening::default();
        now_playing(&mut listening, &item("run:1", "Awake"));
        // Half a second, a jump to 1:30, a pause there, then another second.
        ticks(
            &mut listening,
            0,
            &[500, 90_000, 90_000, 90_000, 90_500, 91_000],
        );
        let play = listening.finish().expect("a play");
        assert_eq!(play.listened_ms, 1_500);
    }

    #[test]
    fn a_song_that_never_moved_is_not_a_play() {
        // A restored queue is current and paused until somebody presses play,
        // and a song skipped straight past was never heard.
        let mut listening = Listening::default();
        now_playing(&mut listening, &item("run:1", "Awake"));
        assert_eq!(now_playing(&mut listening, &item("run:2", "Montana")), None);
        assert_eq!(listening.finish(), None);
    }

    #[test]
    fn the_same_entry_reported_again_is_still_one_play() {
        // A queue edit re-sends the current item; that is not a new play.
        let mut listening = Listening::default();
        let awake = item("run:1", "Awake");
        now_playing(&mut listening, &awake);
        ticks(&mut listening, 0, &[500, 1_000]);
        assert_eq!(now_playing(&mut listening, &awake), None);
        ticks(&mut listening, 2, &[1_500, 2_000]);
        assert_eq!(listening.finish().expect("a play").listened_ms, 2_000);
    }

    #[test]
    fn a_song_on_repeat_is_a_play_each_time_round() {
        // Repeat-one keeps the same queue entry, so only the position coming
        // back to the start from the end says it went round again.
        let mut listening = Listening::default();
        now_playing(&mut listening, &item("run:1", "Awake"));
        ticks(&mut listening, 0, &[SONG_MS - 1_000, SONG_MS - 500]);
        let first = ticks(&mut listening, 10, &[300, 800]).expect("the first time round");
        assert_eq!(first.listened_ms, 500);
        let second = listening.finish().expect("the second time round");
        assert_eq!((second.played_at, second.listened_ms), (10, 800));
    }

    #[test]
    fn a_song_running_into_the_next_is_one_play() {
        // The last half second, the next song's first second, then the change.
        let mut listening = Listening::default();
        now_playing(&mut listening, &item("run:1", "Awake"));
        ticks(&mut listening, 0, &[SONG_MS - 1_000, SONG_MS - 500]);
        let heard = ticks(&mut listening, 10, &[0, 1_000]).expect("a play");
        assert_eq!(heard.title, "Awake");
        assert_eq!(now_playing(&mut listening, &item("run:2", "Montana")), None);
    }
}
