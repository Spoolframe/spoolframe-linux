//! The pages a card opens, laid out as in the UI mockups.

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use spoolframe_linux_core::backend::{ArtworkSize, ChosenTracks, Selection, TrackReason};
use spoolframe_linux_core::format;
use spoolframe_types::{Album, Artist, ArtworkKind, Item, MediaTracks, Season, Show};

use crate::views::{self, Cards, Facts};

const POSTER_LARGE: (i32, i32) = (220, 330);
const SQUARE: (i32, i32) = (300, 300);
const BACKDROP_HEIGHT: i32 = 240;

/// Starts playback of the page's item, from a position or the start, with
/// the tracks picked on the page, or with the server's choice (`None`).
pub type Play = Rc<dyn Fn(Option<Duration>, Option<Selection>)>;

/// Plays a list of tracks from one of them, as a queue.
pub type PlayTracks = Rc<dyn Fn(Vec<Item>, usize)>;

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
    info.append(&label(&title, &["title-1"]));

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
    poster.set_margin_top(BACKDROP_HEIGHT - POSTER_LARGE.1 / 2);
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
        let audio = (tracks.audio.len() > 1).then(|| {
            let names: Vec<String> = tracks.audio.iter().map(format::audio_track).collect();
            let selected = start
                .and_then(|s| s.audio)
                .map(|i| i as usize)
                .or_else(|| tracks.audio.iter().position(|t| t.default))
                .unwrap_or(0);
            picker(&row, "Audio", &names, selected)
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
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&label(title, &["caption-heading", "dim-label"]));
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
        &["pill", "suggested-action"],
        position,
        actions,
        &pickers,
    ));
    if position.is_some() {
        buttons.append(&play_button(
            "Play from start",
            &["pill"],
            None,
            actions,
            &pickers,
        ));
    }
    let watched = facts.playback.is_some_and(|p| p.watched);
    let toggle = gtk::ToggleButton::builder()
        .icon_name("object-select-symbolic")
        .active(watched)
        .valign(gtk::Align::Center)
        .css_classes(["circular"])
        .build();
    let tooltip = |watched| {
        if watched {
            "Watched"
        } else {
            "Mark as watched"
        }
    };
    toggle.set_tooltip_text(Some(tooltip(watched)));
    let set_watched = actions.set_watched.clone();
    toggle.connect_toggled(move |toggle| {
        toggle.set_tooltip_text(Some(tooltip(toggle.is_active())));
        set_watched(toggle.is_active());
    });
    buttons.append(&toggle);
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

fn play_button(
    label: &str,
    classes: &[&str],
    from: Option<Duration>,
    actions: &PlayableActions,
    pickers: &Option<TrackPickers>,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .css_classes(classes)
        .build();
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
            row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
        }
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let (episode, open) = (episode.clone(), cards.open.clone());
        row.connect_activated(move |_| open(episode.clone()));
        list.append(&row);
    }
    titled(title, &list)
}

/// An artist: their albums as a grid.
pub fn artist(artist: &Artist, albums: &[Item], cards: &Cards) -> gtk::Widget {
    let body = content();
    body.append(&label(&artist.name, &["title-1"]));
    body.append(&views::grid(
        "Albums",
        albums.iter().map(|a| views::poster_card(a, cards)),
    ));
    body.upcast()
}

/// An album: the cover and facts on the left, the tracks on the right.
/// Play and Shuffle queue the whole album; a track plays the album from it.
pub fn album(
    album: &Album,
    artist: &str,
    tracks: &[Item],
    play: PlayTracks,
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
        .css_classes(["pill", "suggested-action"])
        .build();
    {
        let (play, tracks) = (play.clone(), tracks.to_vec());
        play_all.connect_clicked(move |_| play(tracks.clone(), 0));
    }
    let shuffle = gtk::Button::builder()
        .label("Shuffle")
        .css_classes(["pill"])
        .build();
    {
        let (play, tracks) = (play.clone(), tracks.to_vec());
        shuffle.connect_clicked(move |_| play(shuffled(&tracks), 0));
    }
    buttons.append(&play_all);
    buttons.append(&shuffle);
    side.append(&buttons);
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
        let (play, tracks) = (play.clone(), tracks.to_vec());
        row.connect_activated(move |_| play(tracks.clone(), index));
        list.append(&row);
    }
    body.append(&list);
    body.upcast()
}

/// The tracks in a random order.
fn shuffled(tracks: &[Item]) -> Vec<Item> {
    let mut tracks = tracks.to_vec();
    // Fisher–Yates with GLib's random numbers; no new dependency for this.
    for i in (1..tracks.len()).rev() {
        let j = gtk::glib::random_int_range(0, i as i32 + 1) as usize;
        tracks.swap(i, j);
    }
    tracks
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
