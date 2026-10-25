// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fetching tile covers, a few at a time.
//!
//! **The ceiling is the fix, and the reason is measurable.** Home builds every
//! tile of every shelf at once, so a cold Home asked Apple's image host for
//! nearly two hundred covers in one burst; 25 came back `Connection reset by
//! peer` and the tiles stayed blank. A grid hides the same burst because
//! recycling rebinds the tile and asks again — Home's tiles are built once and
//! never rebind, so one failure was permanent.
//!
//! So a request is queued, at most [`TILE_ART_PARALLEL`] run at a time, and a
//! failure goes back on the queue a couple of times before it is given up on
//! with a warning. Both halves matter: the ceiling stops the flood, and the
//! retry covers the ones that still fall over.

use relm4::ComponentSender;

use super::{AppModel, CommandMsg, TILE_ART, TILE_ART_PARALLEL};
use crate::components::artwork;

impl AppModel {
    /// Start as many queued covers as there is room for.
    ///
    /// Called whenever a slot frees: a delivery, a failure, or a new request on
    /// an idle pump. Doing nothing when the queue is empty is the common case,
    /// which is why this is cheap enough to call from every path.
    pub(super) fn pump_tile_art(&mut self, sender: &ComponentSender<Self>) {
        while self.tile_art_in_flight < TILE_ART_PARALLEL {
            let Some((key, art, attempt)) = self.tile_art_queue.pop_front() else {
                break;
            };
            self.tile_art_in_flight += 1;
            sender.oneshot_command(async move {
                // Fetch only if it is missing — `fetch` short-circuits on the
                // disk cache — but decode either way, off the GTK thread. That
                // is the whole point of #27: the tile is handed pixels, not a
                // filename. Either half failing is cosmetic but never silent;
                // `load_tile` says which.
                let (path, cover) = artwork::load_tile(art.clone(), TILE_ART, &key).await;
                CommandMsg::TileArt {
                    key,
                    art,
                    attempt,
                    path,
                    cover,
                }
            });
        }
    }
}
