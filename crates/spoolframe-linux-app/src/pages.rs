//! The pages a card opens, laid out as in the UI mockups.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use spoolframe_linux_core::backend::{ArtworkSize, ChosenTracks, Selection, TrackReason};
use spoolframe_linux_core::format;
use spoolframe_types::{Album, Artist, ArtworkKind, Item, MediaTracks, Season, Show};

use crate::views::{self, Cards, Enqueue, Facts};

const POSTER_LARGE: (i32, i32) = (220, 330);
const SQUARE: (i32, i32) = (300, 300);
const BACKDROP_HEIGHT: i32 = 240;
/// How far a page's poster reaches up into the backdrop.
const POSTER_OVER_BACKDROP: i32 = 150;

/// Starts playback of the page's item, from a position or the start, with
/// the tracks picked on the page, or with the server's choice (`None`).
pub type Play = Rc<dyn Fn(Option<Duration>, Option<Selection>)>;

/// Plays a list of tracks from one of them, as the context; shuffled
/// (the queue's own shuffle, so the toggle shows it) when the flag says.
pub type PlayTracks = Rc<dyn Fn(Vec<Item>, usize, bool)>;

/// Puts tracks of the page's album in the play queue (P1-23).
pub type EnqueueTracks = Rc<dyn Fn(Vec<Item>, Enqueue)>;

/// What the details page of a movie or an episode can do.
pub struct PlayableActions {
    pub play: Play,
    /// Marks the item watched (`true`) or unwatched.
    pub set_watched: Rc<dyn Fn(bool)>,
    /// Remembers tracks for the movie, or for an episode's whole series.
    pub remember: Rc<dyn Fn(Selection)>,
    /// Forgets them: back to the user's preferences.
    pub forget: Rc<dyn Fn()>,
}

/// A page with its own header bar, which shows the Back button.
pub fn page(title: &str, body: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .child(body)
            .vexpand(true)
            .build(),
    ));
    adw::NavigationPage::builder()
        .title(title)
        .child(&toolbar)
        .build()
}

/// A movie or an episode: a backdrop, the poster overlapping it, and the
/// title, facts, summary, track pickers and playback buttons beside the
/// poster. `context` is what the item belongs to, the show's title for an
/// episode. The pickers start at the tracks the server would choose
/// (`chosen`, P1-20); without `tracks` there are no pickers, and the
/// server's choice plays.
pub fn playable(
    item: &Item,
    context: Option<&str>,
    tracks: Option<&MediaTracks>,
    chosen: Option<&ChosenTracks>,
    actions: PlayableActions,
    cards: &Cards,
) -> gtk::Widget {
    let facts = Facts::of(item);
    let (title, summary) = match item {
        Item::Movie(m) => (m.title.clone(), m.summary.clone()),
        Item::Episode(e) => (e.title.clone(), e.summary.clone()),
        _ => (facts.title.clone(), None),
    };

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(28)
        .margin_start(48 + POSTER_LARGE.0 + 40)
        .margin_end(48)
        .margin_bottom(32)
        .build();
    if let Some(context) = context {
        info.append(&label(context, &["dim-label", "heading"]));
    }
    let title = label(&title, &["page-title"]);
    // A long title wraps; unwrapped it made the page wider than the window.
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    info.append(&title);

    let meta: Vec<String> = [facts.detail.clone(), facts.length.map(format::length)]
        .into_iter()
        .flatten()
        .collect();
    if !meta.is_empty() {
        info.append(&label(&meta.join(" · "), &["dim-label"]));
    }
    if let Some(summary) = summary {
        let text = label(&summary, &["body"]);
        text.set_wrap(true);
        text.set_max_width_chars(70);
        info.append(&text);
    }
    let series = matches!(item, Item::Episode(_));
    let pickers = tracks.map(|t| TrackPickers::new(t, chosen, series, &actions.forget));
    info.append(&playback_buttons(&facts, &actions, pickers.clone()));
    if let Some(pickers) = &pickers {
        info.append(&pickers.column);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let backdrop = gtk::Overlay::builder()
        .child(
            &gtk::Box::builder()
                .height_request(BACKDROP_HEIGHT)
                .css_classes(["backdrop"])
                .build(),
        )
        .overflow(gtk::Overflow::Hidden)
        .build();
    cards.art(
        &backdrop,
        facts.artwork,
        &[ArtworkKind::Background],
        ArtworkSize::Large,
    );
    column.append(&backdrop);
    column.append(&info);

    // GTK has no negative margins; the poster overlaps the backdrop as an
    // overlay instead.
    let poster = views::tile(item.id(), &facts.title, "poster-large", POSTER_LARGE);
    cards.art(
        &poster,
        facts.artwork,
        &[ArtworkKind::Poster],
        ArtworkSize::Medium,
    );
    poster.set_halign(gtk::Align::Start);
    poster.set_valign(gtk::Align::Start);
    poster.set_margin_start(48);
    // Over the backdrop's lower edge, framed in the page's colour.
    poster.add_css_class("framed");
    poster.set_margin_top(BACKDROP_HEIGHT - POSTER_OVER_BACKDROP);
    let overlay = gtk::Overlay::builder().child(&column).build();
    overlay.add_overlay(&poster);
    overlay.upcast()
}

/// Audio and subtitle pickers, read when Play is pressed.
#[derive(Clone)]
struct TrackPickers {
    column: gtk::Box,
    audio: Option<gtk::DropDown>,
    subtitles: Option<gtk::DropDown>,
    /// What the server would play with no tracks asked for.
    chosen: Option<Selection>,
    remember: gtk::CheckButton,
}

impl TrackPickers {
    fn new(
        tracks: &MediaTracks,
        chosen: Option<&ChosenTracks>,
        series: bool,
        forget: &Rc<dyn Fn()>,
    ) -> Self {
        let start = chosen.map(|c| c.selection);
        let row = gtk::Box::builder().spacing(14).homogeneous(true).build();
        // Shown with a single track too, so it says what will play.
        let audio = (!tracks.audio.is_empty()).then(|| {
            let names: Vec<String> = tracks.audio.iter().map(format::audio_track).collect();
            let selected = start
                .and_then(|s| s.audio)
                .map(|i| i as usize)
                .or_else(|| tracks.audio.iter().position(|t| t.default))
                .unwrap_or(0);
            let dropdown = picker(&row, "Audio", &names, selected);
            dropdown.set_sensitive(names.len() > 1);
            dropdown
        });
        let subtitles = (!tracks.subtitles.is_empty()).then(|| {
            let names: Vec<String> = std::iter::once("Off".to_owned())
                .chain(tracks.subtitles.iter().map(format::subtitle_track))
                .collect();
            let selected = match start {
                Some(s) => s.subtitle.map_or(0, |i| i as usize + 1),
                None => tracks
                    .subtitles
                    .iter()
                    .position(|t| t.default)
                    .map_or(0, |i| i + 1),
            };
            picker(&row, "Subtitles", &names, selected)
        });

        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.append(&row);
        if let Some(chosen) = chosen {
            let why: Vec<String> = [
                format::track_reason(chosen.audio).map(|r| format!("audio from {r}")),
                format::track_reason(chosen.subtitle).map(|r| format!("subtitles: {r}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !why.is_empty() {
                let mut text = why.join(" · ");
                text[..1].make_ascii_uppercase();
                column.append(&label(&text, &["dim-label", "caption"]));
            }
        }
        let remember = gtk::CheckButton::builder()
            .label(if series {
                "Use these tracks for the whole series"
            } else {
                "Use these tracks for this movie"
            })
            .build();
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        footer.append(&remember);
        let overridden = chosen.is_some_and(|c| {
            c.audio == TrackReason::Override || c.subtitle == TrackReason::Override
        });
        if overridden {
            let reset = gtk::Button::builder()
                .label("Reset to my defaults")
                .css_classes(["flat"])
                .build();
            let forget = forget.clone();
            reset.connect_clicked(move |_| forget());
            footer.append(&reset);
        }
        column.append(&footer);
        Self {
            column,
            audio,
            subtitles,
            chosen: start,
            remember,
        }
    }

    /// What the pickers say now. Positions in the item's track lists; no
    /// subtitle for "Off".
    fn selection(&self) -> Selection {
        Selection {
            audio: self.audio.as_ref().map(|d| d.selected()),
            subtitle: self
                .subtitles
                .as_ref()
                .and_then(|d| d.selected().checked_sub(1)),
        }
    }

    /// The tracks to ask for: `None` while the pickers still show the
    /// server's own choice, so its reasons (and later changes to the
    /// preferences) stay in charge.
    fn request(&self) -> Option<Selection> {
        let picked = self.selection();
        let unchanged = self.chosen.is_some_and(|c| {
            (self.audio.is_none() || c.audio == picked.audio)
                && (self.subtitles.is_none() || c.subtitle == picked.subtitle)
        });
        (!unchanged).then_some(picked)
    }
}

fn picker(row: &gtk::Box, title: &str, names: &[String], selected: usize) -> gtk::DropDown {
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let dropdown = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&names))
        .selected(selected as u32)
        .build();
    // Long lists (dozens of subtitle languages) can be searched.
    if names.len() > crate::tracks::SEARCH_FROM {
        dropdown.set_expression(Some(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<gtk::Expression>,
            "string",
        )));
        dropdown.set_enable_search(true);
        dropdown.set_search_match_mode(gtk::StringFilterMatchMode::Substring);
    }
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&label(
        &title.to_uppercase(),
        &["caption-heading", "dim-label"],
    ));
    column.append(&dropdown);
    row.append(&column);
    dropdown
}

fn playback_buttons(
    facts: &Facts,
    actions: &PlayableActions,
    pickers: Option<TrackPickers>,
) -> gtk::Box {
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let position = facts.position();
    let resume = match position {
        Some(position) => format!("Resume from {}", format::timestamp(position)),
        None => "Play".to_owned(),
    };
    buttons.append(&play_button(
        &resume,
        &["action", "suggested-action"],
        position,
        actions,
        &pickers,
    ));
    if position.is_some() {
        buttons.append(&play_button(
            "Play from start",
            &["action", "outlined"],
            None,
            actions,
            &pickers,
        ));
    }
    let watched = facts.playback.is_some_and(|p| p.watched);
    buttons.append(&watched_button(watched, &actions.set_watched));
    if let (Some(length), Some(position)) = (facts.length, facts.position()) {
        buttons.append(
            &gtk::ProgressBar::builder()
                .fraction(format::progress(length, position))
                .width_request(160)
                .valign(gtk::Align::Center)
                .margin_start(8)
                .build(),
        );
    }
    buttons
}

/// Watched or not, as an eye and a word. A watched item's button is filled
/// with the accent colour, so the state reads at a glance; a check mark on
/// a barely tinted circle did not.
fn watched_button(watched: bool, set_watched: &Rc<dyn Fn(bool)>) -> gtk::Button {
    let content = adw::ButtonContent::builder()
        .icon_name(crate::icons::WATCHED)
        .build();
    let button = gtk::Button::builder()
        .child(&content)
        .valign(gtk::Align::Center)
        .css_classes(["action", "outlined", "watched-toggle"])
        .build();
    let show = {
        let (button, content) = (button.downgrade(), content.downgrade());
        move |watched: bool| {
            let (Some(button), Some(content)) = (button.upgrade(), content.upgrade()) else {
                return;
            };
            content.set_label(if watched { "Watched" } else { "Mark watched" });
            button.set_tooltip_text(Some(if watched {
                "Watched; click to mark unwatched"
            } else {
                "Mark as watched"
            }));
            if watched {
                button.add_css_class("watched");
            } else {
                button.remove_css_class("watched");
            }
        }
    };
    show(watched);
    let state = Rc::new(Cell::new(watched));
    let set_watched = set_watched.clone();
    button.connect_clicked(move |_| {
        let watched = !state.get();
        state.set(watched);
        show(watched);
        set_watched(watched);
    });
    button
}

fn play_button(
    label: &str,
    classes: &[&str],
    from: Option<Duration>,
    actions: &PlayableActions,
    pickers: &Option<TrackPickers>,
) -> gtk::Button {
    let button = gtk::Button::builder().css_classes(classes).build();
    // The main action carries the play mark, as designed.
    if classes.contains(&"suggested-action") {
        button.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::PLAY)
                .label(label)
                .build(),
        ));
    } else {
        button.set_label(label);
    }
    let (play, remember, pickers) = (
        actions.play.clone(),
        actions.remember.clone(),
        pickers.clone(),
    );
    button.connect_clicked(move |_| {
        let Some(pickers) = &pickers else {
            return play(from, None);
        };
        if pickers.remember.is_active() {
            remember(pickers.selection());
        }
        play(from, pickers.request());
    });
    button
}

/// A show: its facts, then each season with its episodes.
pub fn show(show: &Show, seasons: &[(Season, Vec<Item>)], cards: &Cards) -> gtk::Widget {
    let body = content();
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 28);
    let poster = views::tile(&show.id, &show.title, "poster-large", POSTER_LARGE);
    cards.art(
        &poster,
        &show.artwork,
        &[ArtworkKind::Poster],
        ArtworkSize::Medium,
    );
    header.append(&poster);
    let facts = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .valign(gtk::Align::End)
        .build();
    facts.append(&label(&show.title, &["title-1"]));
    if let Some(year) = show.year {
        facts.append(&label(&year.to_string(), &["dim-label"]));
    }
    if let Some(summary) = &show.summary {
        let text = label(summary, &["body"]);
        text.set_wrap(true);
        text.set_max_width_chars(70);
        facts.append(&text);
    }
    header.append(&facts);
    body.append(&header);

    for (season, episodes) in seasons {
        body.append(&episode_list(&season.title, episodes, cards));
    }
    body.upcast()
}

/// A season on its own page, when one is opened directly.
pub fn season(season: &Season, episodes: &[Item], cards: &Cards) -> gtk::Widget {
    let body = content();
    body.append(&episode_list(&season.title, episodes, cards));
    body.upcast()
}

fn episode_list(title: &str, episodes: &[Item], cards: &Cards) -> gtk::Box {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for episode in episodes {
        let Item::Episode(e) = episode else { continue };
        let facts = Facts::of(episode);
        let status = match (facts.length, facts.position(), e.playback.watched) {
            (_, _, true) => "Watched".to_owned(),
            (Some(length), Some(position), _) => format::remaining(length, position),
            (Some(length), None, _) => format::length(length),
            (None, _, _) => String::new(),
        };
        let row = adw::ActionRow::builder()
            .title(format!("{}. {}", e.episode, e.title))
            .subtitle(status)
            .activatable(true)
            .build();
        if e.playback.watched {
            row.add_suffix(&gtk::Image::from_icon_name(crate::icons::CHECK));
        }
        row.add_suffix(&gtk::Image::from_icon_name(crate::icons::RIGHT));
        let (episode, open) = (episode.clone(), cards.open.clone());
        row.connect_activated(move |_| open(episode.clone()));
        list.append(&row);
    }
    titled(title, &list)
}

/// An artist: their albums as a grid, and a "⋯" to queue them all.
pub fn artist(artist: &Artist, albums: &[Item], cards: &Cards) -> gtk::Widget {
    let body = content();
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.append(&label(&artist.name, &["title-1"]));
    if let Some(enqueue) = &cards.enqueue {
        let (item, enqueue) = (Item::Artist(artist.clone()), enqueue.clone());
        let popover = views::menu_popover(vec![views::enqueue_entries(move |at| {
            enqueue(item.clone(), at)
        })]);
        heading.append(&views::menu_button(&popover, "More"));
    }
    body.append(&heading);
    body.append(&views::grid(
        "Albums",
        albums.iter().map(|a| views::poster_card(a, cards)),
    ));
    body.upcast()
}

/// An album: the cover and facts on the left, the tracks on the right.
/// Play and Shuffle play the whole album; a track plays the album from
/// it. "⋯" on the album or a track puts it in the play queue.
pub fn album(
    album: &Album,
    artist: &str,
    tracks: &[Item],
    play: PlayTracks,
    enqueue: EnqueueTracks,
    cards: &Cards,
) -> gtk::Widget {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(40)
        .margin_top(36)
        .margin_bottom(36)
        .margin_start(48)
        .margin_end(48)
        .build();

    let side = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .width_request(SQUARE.0)
        .build();
    let cover = views::tile(&album.id, &album.title, "square", SQUARE);
    cards.art(
        &cover,
        &album.artwork,
        &[ArtworkKind::Poster],
        ArtworkSize::Medium,
    );
    side.append(&cover);
    side.append(&label(&album.title, &["title-1"]));
    side.append(&label(artist, &["heading", "accent"]));
    let length: Duration = tracks
        .iter()
        .filter_map(|t| match t {
            Item::Track(t) => t.duration,
            _ => None,
        })
        .sum();
    let meta: Vec<String> = [
        album.year.map(|y| y.to_string()),
        Some(format!("{} tracks", tracks.len())),
        Some(format::length(length)),
    ]
    .into_iter()
    .flatten()
    .collect();
    side.append(&label(&meta.join(" · "), &["dim-label", "caption"]));
    let buttons = gtk::Box::builder().spacing(10).homogeneous(true).build();
    let play_all = gtk::Button::builder()
        .label("Play")
        .css_classes(["action", "suggested-action"])
        .build();
    {
        let (play, tracks) = (play.clone(), tracks.to_vec());
        play_all.connect_clicked(move |_| play(tracks.clone(), 0, false));
    }
    let shuffle = gtk::Button::builder()
        .label("Shuffle")
        .css_classes(["action", "outlined"])
        .build();
    {
        let (play, tracks) = (play.clone(), tracks.to_vec());
        // From a random track, the rest shuffled by the queue.
        shuffle.connect_clicked(move |_| {
            let start = gtk::glib::random_int_range(0, tracks.len().max(1) as i32) as usize;
            play(tracks.clone(), start, true)
        });
    }
    buttons.append(&play_all);
    buttons.append(&shuffle);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_hexpand(true);
    actions.append(&buttons);
    {
        let (enqueue, tracks) = (enqueue.clone(), tracks.to_vec());
        let popover = views::menu_popover(vec![views::enqueue_entries(move |at| {
            enqueue(tracks.clone(), at)
        })]);
        actions.append(&views::menu_button(&popover, "More"));
    }
    side.append(&actions);
    body.append(&side);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .css_classes(["boxed-list"])
        .build();
    for (index, track) in tracks.iter().enumerate() {
        let Item::Track(t) = track else { continue };
        let row = adw::ActionRow::builder()
            .title(&t.title)
            .activatable(true)
            .build();
        if let Some(number) = t.track {
            let number = label(&number.to_string(), &["dim-label", "numeric"]);
            number.set_width_chars(2);
            row.add_prefix(&number);
        }
        if let Some(length) = t.duration {
            row.add_suffix(&label(
                &format::timestamp(length),
                &["dim-label", "numeric"],
            ));
        }
        let menu = {
            let (enqueue, track) = (enqueue.clone(), track.clone());
            views::menu_popover(vec![views::enqueue_entries(move |at| {
                enqueue(vec![track.clone()], at)
            })])
        };
        row.add_suffix(&views::menu_button(&menu, "More"));
        let (play, tracks) = (play.clone(), tracks.to_vec());
        row.connect_activated(move |_| play(tracks.clone(), index, false));
        list.append(&row);
    }
    body.append(&list);
    body.upcast()
}

fn content() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(28)
        .margin_top(36)
        .margin_bottom(36)
        .margin_start(48)
        .margin_end(48)
        .build()
}

fn titled(title: &str, body: &impl IsA<gtk::Widget>) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
    section.append(&label(title, &["title-3"]));
    section.append(body);
    section
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(classes)
        .build()
}
