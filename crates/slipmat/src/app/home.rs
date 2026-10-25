// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Home: Apple's shelves, drawn as rows of tiles.
//!
//! A shelf is a heading and a horizontally scrolling strip. The tiles are the
//! same ones the grids bind — [`TileView`] is shared, so a shelf tile and a
//! grid tile cannot drift apart — and what clicking one does depends on what it
//! is: an album or a playlist pushes the page it already has, and a station
//! **plays**, because a station has no track list to show until MusicKit
//! generates one.
//!
//! ## Why this one is not a `TypedGridView`
//!
//! The grids virtualise because a library is hundreds of tiles and every bind
//! costs a cover. Home is Apple's dozen-per-shelf, and the shelves are built
//! once per fetch behind the loading spinner, so the scroll position is not
//! worth a factory here. If a shelf ever grows past thirty tiles, this is the
//! paragraph to revisit — and `count_widget("shelf-tile")` is what would say so.
//!
//! ## The fingerprint
//!
//! Rebuilding is not free, and Home is fetched again whenever Apple says a
//! shelf has changed. `built_home` records what the widgets were built for, the
//! same discipline the three grids follow: an answer that says the same thing
//! must not rebuild, because the second rebuild is a hundred cover decodes for
//! no visible change.

use relm4::ComponentSender;
use relm4::gtk;
use relm4::gtk::prelude::*;

use super::{AppModel, AppMsg, PageKind, Timed, View};
use crate::components::grid_item::{Tile, TileView};
use slipmat_core::music::types::{Shelf, ShelfItem};

impl From<&ShelfItem> for Tile {
    fn from(item: &ShelfItem) -> Self {
        match item {
            ShelfItem::Album(album) => Self::Album(album.clone()),
            ShelfItem::Playlist(playlist) => Self::Playlist(playlist.clone()),
            ShelfItem::Station(station) => Self::Station(station.clone()),
        }
    }
}

fn item_id(item: &ShelfItem) -> &str {
    match item {
        ShelfItem::Album(album) => &album.id,
        ShelfItem::Playlist(playlist) => &playlist.id,
        ShelfItem::Station(station) => &station.id,
    }
}

impl AppModel {
    /// Open on what Apple said last time, before the daemon has answered.
    ///
    /// Home is the landing view, so this is the difference between a window
    /// that opens on shelves and one that opens on a spinner — the same trade
    /// the library cache made, for the same reason.
    pub(super) fn seed_home(&mut self, sender: &ComponentSender<Self>) {
        let cached = slipmat_core::home_cache::load();
        if cached.is_empty() {
            return;
        }
        tracing::info!(shelves = cached.shelves.len(), "opened on the cached home");
        self.home = cached.shelves;
        if self.view == View::Home {
            self.rebuild_home(sender);
        }
    }

    /// Take the shelves the daemon fetched.
    pub(super) fn fill_home(&mut self, shelves: Vec<Shelf>, sender: &ComponentSender<Self>) {
        self.loading_home = false;
        self.tried_home = true;
        // Copying and decoding a hundred covers to show what is already on
        // screen is the cost this comparison exists to avoid.
        if shelves == self.home {
            tracing::debug!("home came back unchanged");
            return;
        }
        self.home = shelves;
        self.built_home = None;
        if self.view == View::Home {
            self.rebuild_home(sender);
        }
    }

    /// Ask for Home, and say it is loading only when there is nothing to show.
    ///
    /// A reload over shelves that are already drawn keeps them up: taking the
    /// screen away to show a spinner is the behaviour the library sections
    /// deliberately dropped.
    pub(super) fn ask_home(&mut self) {
        if self.home.is_empty() {
            self.loading_home = true;
        }
        self.ask(slipmat_core::ipc::Request::Home);
    }

    /// Rebuild the shelves from `self.home`.
    pub(super) fn rebuild_home(&mut self, sender: &ComponentSender<Self>) {
        let fingerprint = self.home_fingerprint();
        if self.built_home.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        self.built_home = Some(fingerprint);

        let started = std::time::Instant::now();
        let _timed = Timed("home", started);

        // Rows are about to be discarded; none of their covers are ours now.
        // Per Home rather than shared with a grid, for the reason the grids each
        // have their own: clearing one list's registry must not unregister
        // another's live tiles.
        self.home_art_widgets.borrow_mut().clear();
        while let Some(child) = self.home_box.first_child() {
            self.home_box.remove(&child);
        }
        for (shelf, index) in self.home.iter().zip(0..) {
            let row = self.shelf_row(index, shelf, sender);
            self.home_box.append(&row);
        }
    }

    fn home_fingerprint(&self) -> String {
        let mut fingerprint = String::new();
        for shelf in &self.home {
            fingerprint.push_str(&shelf.title);
            fingerprint.push('\u{1}');
            for item in &shelf.items {
                fingerprint.push_str(item_id(item));
                fingerprint.push('\u{1}');
            }
        }
        fingerprint
    }

    /// One heading and its strip of tiles.
    fn shelf_row(
        &self,
        shelf_index: usize,
        shelf: &Shelf,
        sender: &ComponentSender<Self>,
    ) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let heading = gtk::Label::new(Some(&shelf.title));
        heading.set_xalign(0.0);
        heading.add_css_class("title-4");
        row.append(&heading);

        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        for (tile, item_index) in shelf.items.iter().map(Tile::from).zip(0..) {
            let view = TileView::new("shelf-tile");
            view.bind(&tile, &self.home_art_widgets, &self.tile_art_request);
            view.without_pin_menu();
            // **A button, not a click gesture.** Home is what the app opens on,
            // and a gesture on a plain box cannot be reached with the keyboard
            // at all — no focus ring, no Enter. The button carries the tile as
            // its child and is styled flat (`.tile-button`) so nothing about
            // the tile's look changes.
            let button = gtk::Button::new();
            button.set_has_frame(false);
            button.add_css_class("tile-button");
            button.set_child(Some(&view.root));
            // A click is a person, and the strip can be rebuilt under it — so
            // the handler carries the shelf and the position, and the reducer
            // resolves them against the shelves it holds *now*.
            let sender = sender.clone();
            button.connect_clicked(move |_| {
                sender.input(AppMsg::HomeTile {
                    shelf: shelf_index,
                    item: item_index,
                });
            });
            strip.append(&button);
        }

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            // Exactly as tall as one tile and its captions, so the shelf does
            // not reserve a viewport's worth of empty space under it.
            .propagate_natural_height(true)
            .child(&strip)
            .build();
        row.append(&scroller);
        row
    }

    /// What a Home tile does when it is clicked.
    pub(super) fn home_tile(&self, shelf: usize, item: usize, sender: &ComponentSender<Self>) {
        let Some(item) = self.home.get(shelf).and_then(|shelf| shelf.items.get(item)) else {
            // The shelves moved between the click and the handler.
            return;
        };
        match item {
            ShelfItem::Album(album) => sender.input(AppMsg::OpenPage(PageKind::album(album))),
            ShelfItem::Playlist(playlist) => {
                sender.input(AppMsg::OpenPage(PageKind::playlist(playlist)));
            }
            // No page: a station is not a list of tracks until MusicKit builds
            // one, so clicking it starts it.
            ShelfItem::Station(station) => {
                self.ask(slipmat_core::ipc::Request::PlayStation {
                    id: station.id.clone(),
                });
            }
        }
    }
}
