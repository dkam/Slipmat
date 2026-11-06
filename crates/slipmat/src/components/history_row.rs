// SPDX-FileCopyrightText: 2026 Miguel Rincon
// SPDX-License-Identifier: GPL-3.0-or-later

//! One row of History: a day's heading, or one time a song was heard.
//!
//! A recycled `gtk::ListView` row like `track_row`, with the same rule: `bind`
//! sets every property, because the widget was showing another row a moment
//! ago. Both faces live in one widget and `bind` shows one, as the queue's
//! disclosure does.
//!
//! The artist and album are links, because History is a way back to the music.
//! They are buttons rather than `<a>` in a label: a label bakes a link's colour
//! into its text, so no stylesheet can give one the accent on hover. Their
//! handler is connected once in `setup`, so it reads the play from a cell that
//! `bind` rewrites rather than capturing one.

use std::cell::RefCell;
use std::rc::Rc;

use relm4::gtk::prelude::*;
use relm4::typed_view::list::RelmListItem;
use relm4::{RelmWidgetExt, gtk, view};

use crate::components::cover::Cover;
use crate::components::grid_item::{ArtRegistry, ArtRequest, register, unregister};
use crate::components::{TrackOverrides, overridden};
use slipmat_core::plays::{Play, Source};

/// The time column, wide enough for `≈ 18:40` when plays with only a window
/// arrive.
const TIME_PX: i32 = 84;

/// `cover-row`: the artwork on a track row or a history row.
const COVER_PX: i32 = 40;

/// What a row shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Day {
        heading: String,
        /// "23 plays · 1 h 32 min".
        summary: String,
    },
    Play {
        /// Already in the clock format the desktop uses.
        time: String,
        play: Play,
        /// As the library had it when the rows were built.
        favorite: bool,
    },
}

/// Which page a link asks for: the album's, or the artist's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Album,
    Artist,
}

/// "Open this song's album or artist." The app turns it into a request.
pub type OpenLink = Rc<dyn Fn(String, Link)>;

pub struct HistoryItem {
    pub row: Row,
    art: ArtRegistry,
    request: ArtRequest,
    open: OpenLink,
    overrides: TrackOverrides,
}

impl HistoryItem {
    pub fn new(
        row: Row,
        art: ArtRegistry,
        request: ArtRequest,
        open: OpenLink,
        overrides: TrackOverrides,
    ) -> Self {
        Self {
            row,
            art,
            request,
            open,
            overrides,
        }
    }
}

pub struct HistoryItemWidgets {
    day: gtk::Box,
    heading: gtk::Label,
    summary: gtk::Label,
    play: gtk::Box,
    time: gtk::Label,
    cover: Cover,
    title: gtk::Label,
    artist: NameLink,
    separator: gtk::Label,
    album: NameLink,
    chip_label: gtk::Label,
    star: gtk::Image,
    /// The song the links open, and who opens it. Written by `bind`.
    linked: Rc<RefCell<Option<(String, OpenLink)>>>,
    /// Its own list item, so a heading can say it is not a thing to activate.
    item: gtk::ListItem,
}

impl RelmListItem for HistoryItem {
    type Root = gtk::Box;
    type Widgets = HistoryItemWidgets;

    fn setup(list_item: &gtk::ListItem) -> (Self::Root, Self::Widgets) {
        crate::components::count_widget("history row");

        view! {
            root = gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                #[name = "day"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_spacing: 3,
                    set_margin_top: 18,
                    set_margin_bottom: 6,
                    set_margin_start: 8,

                    #[name = "heading"]
                    gtk::Label {
                        set_xalign: 0.0,
                        add_css_class: "title-4",
                    },

                    #[name = "summary"]
                    gtk::Label {
                        set_xalign: 0.0,
                        add_css_class: "caption",
                        add_css_class: "dim-label",
                        add_css_class: "numeric",
                    },
                },

                #[name = "play"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 12,
                    set_margin_all: 6,

                    #[name = "time"]
                    gtk::Label {
                        set_width_request: TIME_PX,
                        set_xalign: 1.0,
                        set_valign: gtk::Align::Center,
                        add_css_class: "numeric",
                    },

                    #[name = "cover_slot"]
                    gtk::Box {
                        set_valign: gtk::Align::Center,
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_valign: gtk::Align::Center,
                        set_hexpand: true,
                        set_spacing: 3,

                        #[name = "title"]
                        gtk::Label {
                            set_xalign: 0.0,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                            set_use_markup: false,
                        },

                        #[name = "links"]
                        gtk::Box {
                            add_css_class: "caption",
                            add_css_class: "history-links",

                            #[name = "separator"]
                            gtk::Label {
                                set_label: " · ",
                            },
                        },
                    },

                    gtk::Box {
                        set_spacing: 6,
                        set_valign: gtk::Align::Center,
                        add_css_class: "source-chip",

                        // Only "Here" carries colour: it is the one source
                        // Slipmat vouches for itself.
                        gtk::Box {
                            set_valign: gtk::Align::Center,
                            add_css_class: "source-dot",
                        },

                        #[name = "chip_label"]
                        gtk::Label {
                            add_css_class: "caption-heading",
                        },
                    },

                    #[name = "star"]
                    gtk::Image {
                        set_icon_name: Some("starred-symbolic"),
                        set_valign: gtk::Align::Center,
                        add_css_class: "favorite-star",
                    },
                },
            }
        }

        let cover = Cover::new(COVER_PX);
        cover.attach_first(&cover_slot);

        let linked: Rc<RefCell<Option<(String, OpenLink)>>> = Rc::default();
        let artist = NameLink::new(Link::Artist, &linked);
        let album = NameLink::new(Link::Album, &linked);
        links.prepend(&artist.button);
        links.append(&album.button);

        (
            root,
            HistoryItemWidgets {
                day,
                heading,
                summary,
                play,
                time,
                cover,
                title,
                artist,
                separator,
                album,
                chip_label,
                star,
                linked,
                item: list_item.clone(),
            },
        )
    }

    fn bind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        let is_play = matches!(self.row, Row::Play { .. });
        widgets.day.set_visible(!is_play);
        widgets.play.set_visible(is_play);
        widgets.item.set_activatable(is_play);
        widgets.item.set_selectable(false);
        widgets.item.set_focusable(is_play);

        match &self.row {
            Row::Day { heading, summary } => {
                widgets.heading.set_label(heading);
                widgets.summary.set_label(summary);
                *widgets.linked.borrow_mut() = None;
            }
            Row::Play {
                time,
                play,
                favorite,
            } => {
                widgets.time.set_label(time);
                widgets.title.set_label(&play.title);
                let followable = play.catalog_id.is_some();
                widgets.artist.show(&play.artist, followable);
                widgets.album.show(&play.album, followable);
                widgets
                    .separator
                    .set_visible(!play.artist.is_empty() && !play.album.is_empty());
                *widgets.linked.borrow_mut() =
                    play.catalog_id.clone().map(|id| (id, self.open.clone()));

                let (label, tooltip) = chip(play.source);
                widgets.chip_label.set_label(label);
                widgets.chip_label.set_tooltip_text(Some(tooltip));

                let (favorite, _) = overridden(
                    &self.overrides,
                    play.catalog_id.as_deref(),
                    *favorite,
                    false,
                );
                widgets.star.set_visible(favorite);

                widgets.cover.square("audio-x-generic-symbolic");
                if let Some(art) = &play.artwork {
                    let key = art.cache_key();
                    (self.request)(key.clone(), art.clone());
                    register(&mut self.art.borrow_mut(), key, &widgets.cover);
                }
            }
        }
    }

    fn unbind(&mut self, widgets: &mut Self::Widgets, _root: &mut Self::Root) {
        if let Row::Play { play, .. } = &self.row
            && let Some(art) = &play.artwork
        {
            unregister(&mut self.art.borrow_mut(), &art.cache_key(), &widgets.cover);
        }
    }
}

/// A name in a row's subtitle that opens its page.
struct NameLink {
    button: gtk::Button,
    label: gtk::Label,
}

impl NameLink {
    fn new(link: Link, linked: &Rc<RefCell<Option<(String, OpenLink)>>>) -> Self {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let button = gtk::Button::builder()
            .child(&label)
            .focus_on_click(false)
            .css_classes(["history-link"])
            .build();
        let asked = linked.clone();
        button.connect_clicked(move |_| {
            if let Some((catalog_id, open)) = asked.borrow().clone() {
                open(catalog_id, link);
            }
        });
        Self { button, label }
    }

    /// A song with no catalog id has nowhere to go, so its names are only text.
    fn show(&self, name: &str, followable: bool) {
        self.label.set_label(name);
        self.button.set_visible(!name.is_empty());
        self.button.set_can_target(followable);
        self.button.set_focusable(followable);
    }
}

/// What the chip says, and what it means when asked.
fn chip(source: Source) -> (&'static str, &'static str) {
    match source {
        Source::Here => ("Here", "Played in Slipmat, at exactly this time"),
    }
}
