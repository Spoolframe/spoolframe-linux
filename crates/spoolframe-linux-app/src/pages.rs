//! The pages a card opens, laid out as in the UI mockups.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use spoolframe_linux_core::backend::{ArtworkSize, ChosenTracks, Selection, TrackReason};
use spoolframe_linux_core::format;
use spoolframe_types::{
    Album, Artist, Artwork, ArtworkKind, CalendarDate, CastMember, Item, MediaId, MediaTracks,
    Person, PersonRole, Season, Show,
};

use crate::views::{self, Cards, Enqueue, Facts};

const POSTER_LARGE: (i32, i32) = (220, 330);
/// An episode's still in its row.
const THUMB: (i32, i32) = (112, 63);
const SQUARE: (i32, i32) = (300, 300);
const BACKDROP_HEIGHT: i32 = 240;
/// How far a page's poster reaches up into the backdrop.
const POSTER_OVER_BACKDROP: i32 = 150;
/// A cast member's round portrait.
const PORTRAIT: i32 = 88;
/// How many of the cast an item's page shows.
const CAST_SHOWN: usize = 6;

/// Starts playback of the page's item, from a position or the start, with
/// the tracks picked on the page, or with the server's choice (`None`).
/// Plays from a position, with the tracks and the quality (P6-06) picked.
pub type Play =
    Rc<dyn Fn(Option<Duration>, Option<Selection>, spoolframe_client_sdk::PlaybackQuality)>;

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
    toolbar.add_top_bar(
        &adw::HeaderBar::builder()
            .css_classes(["item-header"])
            .build(),
    );
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
    next: Option<&Item>,
    cards: &Cards,
) -> gtk::Widget {
    let facts = Facts::of(item);
    let (title, summary, cast) = match item {
        Item::Movie(m) => (m.title.clone(), m.summary.clone(), m.cast.as_slice()),
        Item::Episode(e) => (e.title.clone(), e.summary.clone(), e.cast.as_slice()),
        _ => (facts.title.clone(), None, &[][..]),
    };

    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(28)
        .margin_start(48 + POSTER_LARGE.0 + 40)
        .margin_end(48)
        .margin_bottom(32)
        .build();
    // An episode's place: the show and the season as links, then which
    // episode; a movie's context, if any, as a line.
    match item {
        Item::Episode(e) => info.append(&breadcrumb(e, context, &cards.open)),
        _ => {
            if let Some(context) = context {
                info.append(&label(context, &["dim-label", "heading"]));
            }
        }
    }
    let title = label(&title, &["page-title"]);
    // A long title wraps; unwrapped it made the page wider than the window.
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    info.append(&title);

    // An episode says when it aired, or its year where the date is not
    // known; a movie's year is in its facts already.
    let aired = match item {
        Item::Episode(e) => e
            .air_date
            .map(air_date)
            .or_else(|| e.year.map(|y| y.to_string())),
        _ => None,
    };
    let meta: Vec<String> = [
        facts.detail.clone(),
        aired,
        facts.length.map(format::length),
    ]
    .into_iter()
    .flatten()
    .collect();
    // The facts as separate spans, as designed, not joined by dots.
    if !meta.is_empty() {
        let row = gtk::Box::builder()
            .spacing(14)
            .css_classes(["item-meta"])
            .build();
        for fact in &meta {
            row.append(&label(fact, &[]));
        }
        if let Some(rating) = content_rating(item) {
            row.append(&rating_badge(rating));
        }
        info.append(&row);
    }
    if let Some(summary) = summary {
        let text = label(&summary, &["item-summary"]);
        text.set_wrap(true);
        text.set_max_width_chars(70);
        info.append(&text);
    }
    let series = matches!(item, Item::Episode(_));
    let pickers = tracks.map(|t| TrackPickers::new(t, chosen, series, &actions.forget));
    let buttons = playback_buttons(&facts, &actions, pickers.clone());
    if let Item::Episode(e) = item {
        let menu = views::menu_popover(vec![views::go_to_entries(e, &cards.open)]);
        buttons.append(&views::menu_button(&menu, "More"));
    }
    info.append(&buttons);
    if let Some(pickers) = &pickers {
        info.append(&pickers.column);
    }
    if let Some(cast) = cast_row(cast, cards) {
        info.append(&cast);
    }
    if let Some(next) = next {
        info.append(&next_card(next, cards));
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

/// The years a show ran, from its first year and the day its latest
/// episode aired: "2022" when that was the same year, "2022–" (still
/// running) when it aired in the twelve months up to `today`, otherwise
/// "2022–2025". With no first year, the latest episode's year alone.
pub(crate) fn show_years(
    year: Option<u16>,
    last_aired: Option<CalendarDate>,
    today: CalendarDate,
) -> Option<String> {
    let Some(last) = last_aired else {
        return year.map(|y| y.to_string());
    };
    let Some(first) = year else {
        return Some(last.year().to_string());
    };
    if last.year() <= first {
        return Some(first.to_string());
    }
    // A year back from today, compared field by field: there is no
    // 29 February to land on in most years.
    let year_ago = (today.year() - 1, today.month(), today.day());
    if (last.year(), last.month(), last.day()) >= year_ago {
        Some(format!("{first}\u{2013}"))
    } else {
        Some(format!("{first}\u{2013}{}", last.year()))
    }
}

/// Today's date here, for how recently a show aired.
fn today() -> Option<CalendarDate> {
    let now = gtk::glib::DateTime::now_local().ok()?;
    CalendarDate::new(
        u16::try_from(now.year()).ok()?,
        u8::try_from(now.month()).ok()?,
        u8::try_from(now.day_of_month()).ok()?,
    )
}

/// An air date as the app writes dates: "31 Jan 2025".
pub(crate) fn air_date(date: CalendarDate) -> String {
    gtk::glib::DateTime::from_utc(
        i32::from(date.year()),
        i32::from(date.month()),
        i32::from(date.day()),
        12,
        0,
        0.0,
    )
    .and_then(|d| d.format("%-d %b %Y"))
    .map(|s| s.to_string())
    .unwrap_or_else(|_| date.to_string())
}

/// The cast an item's page shows: its first actors, in billing order.
/// Directors are left out.
pub(crate) fn billed(cast: &[CastMember]) -> impl Iterator<Item = &CastMember> {
    cast.iter()
        .filter(|m| m.role == PersonRole::Actor)
        .take(CAST_SHOWN)
}

/// The part a cast member plays, for the line under their name; none
/// where the catalog does not say.
pub(crate) fn character(member: &CastMember) -> Option<&str> {
    member
        .character
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
}

/// "Cast": the first actors, each a round portrait with their name and
/// part under it, opening their items. None when the item has no actors.
fn cast_row(cast: &[CastMember], cards: &Cards) -> Option<gtk::Box> {
    let people: Vec<gtk::Widget> = billed(cast).map(|m| cast_member(m, cards)).collect();
    // In a scrolled row, so a long cast or a narrow window never widens
    // the page.
    (!people.is_empty()).then(|| views::row("Cast", people))
}

fn cast_member(member: &CastMember, cards: &Cards) -> gtk::Widget {
    let portrait = portrait(member, cards);
    portrait.set_halign(gtk::Align::Center);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    body.append(&portrait);
    let name = views::caption_label(&member.name, &["heading"]);
    name.set_xalign(0.5);
    name.set_margin_top(6);
    body.append(&name);
    let mut tooltip = member.name.clone();
    if let Some(part) = character(member) {
        let line = views::caption_label(part, &["dim-label", "caption"]);
        line.set_xalign(0.5);
        body.append(&line);
        tooltip = format!("{tooltip} as {part}");
    }
    let button = gtk::Button::builder()
        .child(&body)
        .width_request(PORTRAIT + 24)
        .valign(gtk::Align::Start)
        .tooltip_text(tooltip)
        .css_classes(["card-button", "flat", "lift", "cast-member"])
        .build();
    if let Some(open_person) = cards.open_person.clone() {
        let person = Person {
            id: member.id.clone(),
            name: member.name.clone(),
            role: member.role,
            count: None,
        };
        button.connect_clicked(move |_| open_person(person.clone()));
    }
    button.upcast()
}

/// A round portrait: the photo once it loads, the person's initials on a
/// flat fill until then, or for good when there is none.
fn portrait(member: &CastMember, cards: &Cards) -> gtk::Overlay {
    let fill = gtk::Box::builder()
        .width_request(PORTRAIT)
        .height_request(PORTRAIT)
        .css_classes(["tile", "cast-fill"])
        .build();
    // "tile-title": the photo hides it once it is painted.
    let initials = gtk::Label::builder()
        .label(views::initials(&member.name))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["tile-title", "cast-initials"])
        .build();
    let tile = gtk::Overlay::builder()
        .child(&fill)
        .css_classes(["circle"])
        .overflow(gtk::Overflow::Hidden)
        .build();
    tile.add_overlay(&initials);
    if let Some(photo) = &member.photo {
        let artwork = [Artwork {
            id: photo.clone(),
            kind: ArtworkKind::Thumb,
        }];
        cards.art(&tile, &artwork, &[ArtworkKind::Thumb], ArtworkSize::Small);
    }
    tile
}

/// "Severance › Season 2 › Episode 4": the show and season open their
/// page, the show's on that season's tab.
fn breadcrumb(
    episode: &spoolframe_types::Episode,
    show: Option<&str>,
    open: &views::Open,
) -> gtk::Box {
    let row = gtk::Box::builder().spacing(6).build();
    let link = |text: &str, item: Item| {
        let button = gtk::Button::builder()
            .label(text)
            .css_classes(["flat", "text-link", "crumb"])
            .build();
        let open = open.clone();
        button.connect_clicked(move |_| open(item.clone()));
        button
    };
    let show_title = episode
        .show_title
        .clone()
        .or_else(|| show.map(str::to_owned))
        .unwrap_or_else(|| "Show".to_owned());
    let mut show_item = views::show_of(episode);
    if let Item::Show(s) = &mut show_item {
        s.title = show_title.clone();
    }
    row.append(&link(&show_title, show_item));
    row.append(&label("›", &["dim-label"]));
    row.append(&link(
        &views::season_name(episode.season),
        views::season_of(episode),
    ));
    row.append(&label("›", &["dim-label"]));
    row.append(&label(
        &format!("Episode {}", episode.episode),
        &["dim-label", "crumb"],
    ));
    row
}

/// "Next episode": its still, "5. Title", and a chevron; it opens it.
fn next_card(next: &Item, cards: &Cards) -> gtk::Button {
    let thumb = views::tile(next.id(), "", "thumb", THUMB);
    if let Item::Episode(e) = next {
        cards.art(
            &thumb,
            &e.artwork,
            &[ArtworkKind::Thumb, ArtworkKind::Background],
            ArtworkSize::Small,
        );
    }
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    names.append(&label("Next episode", &["dim-label", "caption"]));
    let title = match next {
        Item::Episode(e) => format!("{}. {}", e.episode, e.title),
        other => Facts::of(other).title,
    };
    names.append(&label(&title, &["heading"]));
    let row = gtk::Box::builder().spacing(14).build();
    row.append(&thumb);
    row.append(&names);
    row.append(&gtk::Image::from_icon_name(crate::icons::RIGHT));
    let button = gtk::Button::builder()
        .child(&row)
        .halign(gtk::Align::Start)
        .css_classes(["next-card"])
        .build();
    let (open, next) = (cards.open.clone(), next.clone());
    button.connect_clicked(move |_| open(next.clone()));
    button
}

/// Audio and subtitle pickers, read when Play is pressed.
#[derive(Clone)]
struct TrackPickers {
    column: gtk::Box,
    audio: Option<gtk::DropDown>,
    subtitles: Option<gtk::DropDown>,
    /// Auto, a bitrate or Original (P6-06): for this playback, so not
    /// remembered with the tracks.
    quality: gtk::DropDown,
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
        let qualities: Vec<String> = crate::quality::choices()
            .into_iter()
            .map(|q| crate::quality::label(q, None))
            .collect();
        let quality = picker(&row, "Quality", &qualities, 0);

        // A movie says what its file is, beside the pickers, as designed.
        if !series && let Some(file) = format::file(tracks.container, tracks.video.as_ref()) {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
            column.append(&label("FILE", &["picker-label"]));
            let text = label(&file, &["file-box"]);
            text.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.set_max_width_chars(1);
            text.set_tooltip_text(Some(&file));
            column.append(&text);
            row.append(&column);
        }
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
            quality,
            chosen: start,
            remember,
        }
    }

    /// The quality picked for this playback.
    fn quality(&self) -> spoolframe_client_sdk::PlaybackQuality {
        crate::quality::choices()
            .get(self.quality.selected() as usize)
            .copied()
            .unwrap_or_default()
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
        .css_classes(["picker"])
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
    column.append(&label(&title.to_uppercase(), &["picker-label"]));
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
                .css_classes(["item-progress"])
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
            return play(from, None, Default::default());
        };
        if pickers.remember.is_active() {
            remember(pickers.selection());
        }
        play(from, pickers.request(), pickers.quality());
    });
    button
}

/// The show page's seasons with their episodes, changed as they are marked.
type Seasons = Rc<std::cell::RefCell<Vec<(Season, Vec<Item>)>>>;

/// Draws a season again; set once the page has made its drawing.
type Redraw = Rc<std::cell::RefCell<Option<Rc<dyn Fn(usize)>>>>;

/// What the show page does: play an episode (from a position, or the
/// start), and mark an item watched or not.
pub struct ShowActions {
    pub play: Rc<dyn Fn(Item, Option<Duration>)>,
    pub set_watched: Rc<dyn Fn(MediaId, bool)>,
}

/// The episode to watch next: the first one part-way through, else the
/// first not watched.
pub(crate) fn up_next(seasons: &[(Season, Vec<Item>)]) -> Option<&Item> {
    let episodes = || seasons.iter().flat_map(|(_, e)| e.iter());
    let watched = |item: &Item| matches!(item, Item::Episode(e) if e.playback.watched);
    episodes()
        .find(|e| !watched(e) && Facts::of(e).position().is_some())
        .or_else(|| episodes().find(|e| !watched(e)))
}

/// "S2 · E4".
pub(crate) fn episode_code(item: &Item) -> String {
    match item {
        Item::Episode(e) => format!("S{} · E{}", e.season, e.episode),
        _ => String::new(),
    }
}

/// A show, as on the LinuxShow board: its facts and Resume, then one
/// season at a time under tabs, opening on `open` or the season of the
/// episode up next.
pub fn show(
    show: &Show,
    seasons: &[(Season, Vec<Item>)],
    open: Option<&MediaId>,
    actions: ShowActions,
    cards: &Cards,
) -> gtk::Widget {
    // The seasons as this page knows them. Marking something watched
    // changes them here at once, as well as on the server, and the season
    // showing is drawn again: the page does not reload.
    let model: Seasons = Rc::new(std::cell::RefCell::new(seasons.to_vec()));
    let current = Rc::new(Cell::new(0usize));
    let redraw: Redraw = Rc::default();
    let actions = {
        let outer = actions;
        let (model, current, redraw) = (model.clone(), current.clone(), redraw.clone());
        let set_watched = outer.set_watched.clone();
        Rc::new(ShowActions {
            play: outer.play,
            set_watched: Rc::new(move |id: MediaId, watched| {
                set_watched(id.clone(), watched);
                mark_in(&mut model.borrow_mut(), &id, watched);
                let again = redraw.borrow().clone();
                if let Some(again) = again {
                    again(current.get());
                }
            }),
        })
    };
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
        .spacing(18)
        .valign(gtk::Align::End)
        .build();
    let title = label(&show.title, &["page-title"]);
    title.set_wrap(true);
    facts.append(&title);
    let meta = gtk::Box::builder()
        .spacing(14)
        .css_classes(["item-meta"])
        .build();
    let aired = today().and_then(|today| show_years(show.year, show.last_aired, today));
    if let Some(years) = aired.or_else(|| show.year.map(|y| y.to_string())) {
        meta.append(&label(&years, &[]));
    }
    let real_seasons = seasons.iter().filter(|(s, _)| s.season > 0).count();
    meta.append(&label(&format::seasons(real_seasons as u32), &[]));
    if !show.genres.is_empty() {
        let genres: Vec<&str> = show.genres.iter().take(2).map(String::as_str).collect();
        meta.append(&label(&genres.join(", "), &[]));
    }
    if let Some(rating) = &show.content_rating {
        meta.append(&rating_badge(rating));
    }
    facts.append(&meta);
    if let Some(summary) = &show.summary {
        let text = label(summary, &["item-summary"]);
        text.set_wrap(true);
        text.set_max_width_chars(70);
        facts.append(&text);
    }
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let next = up_next(seasons).or_else(|| seasons.iter().flat_map(|(_, e)| e.first()).next());
    if let Some(next) = next {
        let position = Facts::of(next).position();
        let word = if position.is_some() { "Resume" } else { "Play" };
        let play = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(crate::icons::PLAY)
                    .label(format!("{word} {}", episode_code(next)))
                    .build(),
            )
            .css_classes(["action", "suggested-action"])
            .build();
        let (actions, next) = (actions.clone(), next.clone());
        play.connect_clicked(move |_| (actions.play)(next.clone(), position));
        buttons.append(&play);
    }
    let all_watched = up_next(seasons).is_none();
    let mark = gtk::Button::builder()
        .icon_name(crate::icons::WATCHED)
        .tooltip_text(if all_watched {
            "Mark show as unwatched"
        } else {
            "Mark show as watched"
        })
        .css_classes(["action", "outlined", "square-action"])
        .build();
    {
        let (actions, id) = (actions.clone(), show.id.clone());
        mark.connect_clicked(move |_| (actions.set_watched)(id.clone(), !all_watched));
    }
    buttons.append(&mark);
    facts.append(&buttons);
    header.append(&facts);
    body.append(&header);
    if let Some(cast) = cast_row(&show.cast, cards) {
        body.append(&cast);
    }

    // The tabs, one per season, and the picked season under them.
    let picked = open
        .and_then(|id| seasons.iter().position(|(s, _)| &s.id == id))
        .or_else(|| {
            let next = up_next(seasons)?;
            seasons
                .iter()
                .position(|(_, e)| e.iter().any(|e| e.id() == next.id()))
        })
        .unwrap_or(0);
    let tabs = gtk::Box::builder()
        .css_classes(["linked", "season-tabs"])
        .build();
    let tab_scroller = gtk::ScrolledWindow::builder()
        .child(&tabs)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .hexpand(true)
        .build();
    let watched_count = label("", &["dim-label", "small-text"]);
    let mark_season = gtk::Button::builder()
        .css_classes(["row-button"])
        .valign(gtk::Align::Center)
        .build();
    // The tabs get the page's width and the row's ‹ ›, so it shows
    // there are more seasons than fit; how many are watched goes under.
    crate::views::wheel_scrolls_row(&tab_scroller);
    let (back, forward) = crate::views::scroll_buttons(&tab_scroller);
    let tab_line = gtk::Box::builder().spacing(6).build();
    tab_line.append(&tab_scroller);
    tab_line.append(&back);
    tab_line.append(&forward);
    let season_line = gtk::Box::builder().spacing(12).build();
    watched_count.set_hexpand(true);
    season_line.append(&watched_count);
    season_line.append(&mark_season);
    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    bar.append(&tab_line);
    bar.append(&season_line);
    body.append(&bar);
    let list_slot = adw::Bin::new();
    body.append(&list_slot);

    let open_season: Rc<dyn Fn(usize)> = {
        let (model, current, actions, cards_open, art) = (
            model.clone(),
            current.clone(),
            actions.clone(),
            cards.open.clone(),
            cards.art.clone(),
        );
        let (watched_count, mark_season, list_slot) = (
            watched_count.clone(),
            mark_season.clone(),
            list_slot.clone(),
        );
        let mark_handler: Rc<std::cell::RefCell<Option<gtk::glib::SignalHandlerId>>> =
            Rc::default();
        Rc::new(move |index| {
            current.set(index);
            let seasons = model.borrow();
            let Some((season, episodes)) = seasons.get(index) else {
                return;
            };
            let total = episodes.len();
            let seen = episodes
                .iter()
                .filter(|e| matches!(e, Item::Episode(e) if e.playback.watched))
                .count();
            watched_count.set_label(&if seen == total {
                format!("All {total} watched")
            } else {
                format!("{seen} of {total} watched")
            });
            let all = seen == total;
            mark_season.set_label(if all {
                "Mark season as unwatched"
            } else {
                "Mark season as watched"
            });
            if let Some(old) = mark_handler.borrow_mut().take() {
                mark_season.disconnect(old);
            }
            let (actions_, id) = (actions.clone(), season.id.clone());
            mark_handler.replace(Some(
                mark_season.connect_clicked(move |_| (actions_.set_watched)(id.clone(), !all)),
            ));
            list_slot.set_child(Some(&episode_list(
                episodes,
                &actions,
                &cards_open,
                art.as_ref(),
            )));
        })
    };
    redraw.replace(Some(open_season.clone()));
    let mut first: Option<gtk::ToggleButton> = None;
    for (index, (season, _)) in seasons.iter().enumerate() {
        let tab = gtk::ToggleButton::builder()
            .label(&season.title)
            .css_classes(["season-tab"])
            .build();
        match &first {
            Some(first) => tab.set_group(Some(first)),
            None => first = Some(tab.clone()),
        }
        tab.set_active(index == picked);
        let open_season = open_season.clone();
        tab.connect_toggled(move |tab| {
            if tab.is_active() {
                open_season(index);
            }
        });
        tabs.append(&tab);
    }
    open_season(picked);
    body.upcast()
}

/// Marks `id` watched or not in the page's copy: the whole show when it is
/// neither a season nor an episode of it, else that season or episode.
/// Watched or not, the episode's position is cleared, as the server does.
fn mark_in(seasons: &mut [(Season, Vec<Item>)], id: &MediaId, watched: bool) {
    let whole = !seasons
        .iter()
        .any(|(s, e)| &s.id == id || e.iter().any(|i| i.id() == id));
    for (season, episodes) in seasons.iter_mut() {
        let all = whole || &season.id == id;
        for item in episodes.iter_mut() {
            if let Item::Episode(e) = item
                && (all || &e.id == id)
            {
                e.playback.watched = watched;
                e.playback.position = None;
            }
        }
    }
}

/// A season's episodes: a thumbnail with its progress, "4. Title", how
/// far along, a check when watched; right-click to play or mark it.
fn episode_list(
    episodes: &[Item],
    actions: &Rc<ShowActions>,
    open: &views::Open,
    art: Option<&Rc<crate::artwork::Art>>,
) -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "episode-list"])
        .build();
    for episode in episodes {
        let Item::Episode(e) = episode else { continue };
        let facts = Facts::of(episode);
        let position = facts.position();
        let status = match (facts.length, position, e.playback.watched) {
            (_, _, true) => "Watched".to_owned(),
            (Some(length), Some(position), _) => format::remaining(length, position),
            (Some(length), None, _) => format::length(length),
            (None, _, _) => String::new(),
        };
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable(true)
            .build();
        row.set_title(&format!("{}. {}", e.episode, e.title));
        row.set_subtitle(&status);
        if position.is_some() && !e.playback.watched {
            row.add_css_class("in-progress");
        }
        let thumb = views::tile(&e.id, "", "thumb", THUMB);
        if let Some(art) = art {
            art.show(
                &thumb,
                &e.artwork,
                &[ArtworkKind::Thumb, ArtworkKind::Background],
                ArtworkSize::Small,
            );
        }
        if let (Some(length), Some(position)) = (facts.length, position) {
            thumb.add_overlay(
                &gtk::ProgressBar::builder()
                    .fraction(format::progress(length, position))
                    .valign(gtk::Align::End)
                    .css_classes(["tile-progress"])
                    .build(),
            );
        }
        thumb.set_valign(gtk::Align::Center);
        thumb.set_margin_top(6);
        thumb.set_margin_bottom(6);
        row.add_prefix(&thumb);
        if e.playback.watched {
            row.add_suffix(
                &gtk::Image::builder()
                    .icon_name(crate::icons::CHECK)
                    .css_classes(["accent"])
                    .build(),
            );
        }
        row.add_suffix(&gtk::Image::from_icon_name(crate::icons::RIGHT));
        // Right-click: play it, from where it was or the start, or mark it.
        let play = |label: String, from: Option<Duration>| {
            let (actions, item) = (actions.clone(), episode.clone());
            views::MenuEntry::new(label, crate::icons::PLAY, move || {
                (actions.play)(item.clone(), from)
            })
        };
        let mut first = vec![match position {
            Some(at) => play(format!("Resume from {}", format::timestamp(at)), Some(at)),
            None => play("Play".to_owned(), None),
        }];
        if position.is_some() {
            first.push(views::MenuEntry::new(
                "Play from beginning",
                crate::icons::BACK_10,
                {
                    let (actions, item) = (actions.clone(), episode.clone());
                    move || (actions.play)(item.clone(), None)
                },
            ));
        }
        let watched = e.playback.watched;
        let mark = {
            let (actions, id) = (actions.clone(), e.id.clone());
            views::MenuEntry::new(
                if watched {
                    "Mark as unwatched"
                } else {
                    "Mark as watched"
                },
                crate::icons::WATCHED,
                move || (actions.set_watched)(id.clone(), !watched),
            )
        };
        let menu = views::menu_popover(vec![first, vec![mark]]);
        views::context_menu(row.upcast_ref(), &menu);
        let (episode, open) = (episode.clone(), open.clone());
        row.connect_activated(move |_| open(episode.clone()));
        list.append(&row);
    }
    list
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
/// Play and Shuffle play the whole album; a track plays the album from
/// it. "⋯" on the album or a track puts it in the play queue.
pub fn album(
    album: &Album,
    artist: &str,
    tracks: &[Item],
    play: PlayTracks,
    enqueue: EnqueueTracks,
    playing: tokio::sync::watch::Receiver<Option<MediaId>>,
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
        .spacing(18)
        .width_request(SQUARE.0)
        .build();
    let cover = views::tile(&album.id, &album.title, "square", SQUARE);
    // A square, whatever the column does.
    cover.set_halign(gtk::Align::Start);
    cards.art(
        &cover,
        &album.artwork,
        &[ArtworkKind::Poster],
        ArtworkSize::Medium,
    );
    side.append(&cover);
    // The title, artist and facts close together.
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    // Wraps at the column's width: asking for its whole width on one line,
    // it widened the column and the cover stretched with it.
    let title = label(&album.title, &["album-title"]);
    title.set_wrap(true);
    title.set_max_width_chars(1);
    names.append(&title);
    let length: Duration = tracks
        .iter()
        .filter_map(|t| match t {
            Item::Track(t) => t.duration,
            _ => None,
        })
        .sum();
    // The album's own count: the tracks listed can be fewer.
    let count = album.track_count.unwrap_or(tracks.len() as u32);
    let sound = tracks.iter().find_map(|t| match t {
        Item::Track(t) => format::audio_format(t.codec, t.bit_depth, t.sample_rate),
        _ => None,
    });
    let meta: Vec<String> = [
        album.year.map(|y| y.to_string()),
        Some(match count {
            1 => "1 track".to_owned(),
            n => format!("{n} tracks"),
        }),
        Some(format::length(length)),
        sound,
    ]
    .into_iter()
    .flatten()
    .collect();
    // The artist is a link to their page.
    let artist_name = label(artist, &[]);
    artist_name.set_wrap(true);
    artist_name.set_max_width_chars(1);
    let artist_link = gtk::Button::builder()
        .child(&artist_name)
        .halign(gtk::Align::Start)
        .css_classes(["flat", "text-link", "album-artist"])
        .tooltip_text("Go to artist")
        .build();
    {
        let (open, item) = (
            cards.open.clone(),
            Item::Artist(Artist {
                id: album.artist_id.clone(),
                name: artist.to_owned(),
                summary: None,
                artwork: Vec::new(),
            }),
        );
        artist_link.connect_clicked(move |_| open(item.clone()));
    }
    names.append(&artist_link);
    let facts_line = label(&meta.join(" · "), &["dim-label", "small-text"]);
    facts_line.set_wrap(true);
    facts_line.set_max_width_chars(1);
    names.append(&facts_line);
    side.append(&names);
    let buttons = gtk::Box::builder().spacing(10).homogeneous(true).build();
    let play_all = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::PLAY)
                .label("Play")
                .build(),
        )
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
    buttons.set_hexpand(true);
    let actions = buttons;
    side.append(&actions);
    body.append(&side);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .css_classes(["track-list"])
        .build();
    // Each track's row and number, to mark the one playing.
    let mut marked: Vec<(MediaId, adw::ActionRow, gtk::Label, String)> = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        let Item::Track(t) = track else { continue };
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .activatable(true)
            .build();
        row.set_title(&t.title);
        let shown = t.track.map(|n| n.to_string()).unwrap_or_default();
        let number = label(&shown, &["dim-label", "numeric", "small-text"]);
        number.set_width_chars(2);
        row.add_prefix(&number);
        marked.push((t.id.clone(), row.clone(), number, shown));
        if let Some(length) = t.duration {
            row.add_suffix(&label(
                &format::timestamp(length),
                &["dim-label", "numeric", "small-text"],
            ));
        }
        // Right-click (or a long press) for the track's menu, as designed:
        // no button on every row.
        let menu = {
            let (enqueue, track) = (enqueue.clone(), track.clone());
            views::menu_popover(vec![
                views::enqueue_entries(move |at| enqueue(vec![track.clone()], at)),
                views::track_go_to_entries(t, &cards.open, false),
            ])
        };
        views::context_menu(row.upcast_ref(), &menu);
        let (play, tracks) = (play.clone(), tracks.to_vec());
        row.connect_activated(move |_| play(tracks.clone(), index, false));
        list.append(&row);
    }
    // The track playing is on the surface, its title in the accent and ▶
    // for its number, as it changes, for as long as the page is open.
    let weak = list.downgrade();
    let mut playing = playing;
    gtk::glib::spawn_future_local(async move {
        loop {
            if weak.upgrade().is_none() {
                break;
            }
            let now = playing.borrow_and_update().clone();
            for (id, row, number, shown) in &marked {
                if now.as_ref() == Some(id) {
                    row.add_css_class("playing");
                    number.set_label("▶");
                } else {
                    row.remove_css_class("playing");
                    number.set_label(shown);
                }
            }
            if playing.changed().await.is_err() {
                break;
            }
        }
    });
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

/// A movie's or an episode's rating, "R", "TV-MA".
fn content_rating(item: &Item) -> Option<&str> {
    match item {
        Item::Movie(m) => m.content_rating.as_deref(),
        Item::Episode(e) => e.content_rating.as_deref(),
        _ => None,
    }
}

/// A rating in a thin box, as the boards draw it.
fn rating_badge(rating: &str) -> gtk::Label {
    let badge = label(rating, &["rating-badge"]);
    badge.set_valign(gtk::Align::Center);
    badge
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(classes)
        .build()
}

#[cfg(test)]
mod tests {
    use spoolframe_types::{Episode, MediaKind, Namespace, PlaybackState};

    use super::*;

    fn id(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).unwrap()
    }

    fn season(n: u32) -> Season {
        Season {
            id: id(MediaKind::Season, &format!("s{n}")),
            show_id: id(MediaKind::Show, "show"),
            show_title: None,
            season: n,
            title: format!("Season {n}"),
            artwork: Vec::new(),
        }
    }

    fn episode(season: u32, n: u32, position: Option<u64>, watched: bool) -> Item {
        Item::Episode(Episode {
            id: id(MediaKind::Episode, &format!("s{season}e{n}")),
            show_id: id(MediaKind::Show, "show"),
            show_title: None,
            season_id: id(MediaKind::Season, &format!("s{season}")),
            season,
            episode: n,
            title: format!("Episode {n}"),
            summary: None,
            duration: Some(Duration::from_secs(3000)),
            artwork: Vec::new(),
            playback: PlaybackState {
                position: position.map(Duration::from_secs),
                watched,
            },
            content_rating: None,
            year: None,
            air_date: None,
            cast: Vec::new(),
        })
    }

    fn date(y: u16, m: u8, d: u8) -> CalendarDate {
        CalendarDate::new(y, m, d).unwrap()
    }

    fn member(name: &str, role: PersonRole, character: Option<&str>) -> CastMember {
        CastMember {
            id: spoolframe_types::PersonId::new(Namespace::Plex, name).unwrap(),
            name: name.into(),
            role,
            character: character.map(Into::into),
            photo: None,
        }
    }

    const TODAY: (u16, u8, u8) = (2026, 10, 14);

    fn years(year: Option<u16>, last_aired: Option<CalendarDate>) -> Option<String> {
        let (y, m, d) = TODAY;
        show_years(year, last_aired, date(y, m, d))
    }

    #[test]
    fn a_show_with_no_air_date_shows_its_first_year() {
        assert_eq!(years(Some(2022), None), Some("2022".into()));
    }

    #[test]
    fn a_show_last_aired_in_its_first_year_shows_that_year() {
        assert_eq!(
            years(Some(2022), Some(date(2022, 12, 1))),
            Some("2022".into())
        );
    }

    #[test]
    fn a_show_aired_in_the_last_twelve_months_is_an_open_range() {
        assert_eq!(
            years(Some(2022), Some(date(2025, 10, 14))),
            Some("2022\u{2013}".into())
        );
    }

    #[test]
    fn a_show_last_aired_over_a_year_ago_shows_both_years() {
        assert_eq!(
            years(Some(2022), Some(date(2025, 10, 13))),
            Some("2022\u{2013}2025".into())
        );
    }

    #[test]
    fn a_show_with_no_first_year_shows_when_it_last_aired() {
        assert_eq!(years(None, Some(date(2019, 3, 2))), Some("2019".into()));
    }

    #[test]
    fn an_air_date_reads_day_month_year() {
        assert_eq!(air_date(date(2025, 1, 31)), "31 Jan 2025");
    }

    #[test]
    fn the_page_shows_the_first_six_actors_in_billing_order() {
        let names = ["A", "B", "C", "D", "E", "F", "G"];
        let cast: Vec<CastMember> = names
            .iter()
            .map(|n| member(n, PersonRole::Actor, None))
            .collect();
        let shown: Vec<&str> = billed(&cast).map(|m| m.name.as_str()).collect();
        assert_eq!(shown, names[..6]);
    }

    #[test]
    fn directors_are_left_out_of_the_cast_row() {
        let cast = vec![
            member("Sigourney Weaver", PersonRole::Actor, Some("Ripley")),
            member("Ridley Scott", PersonRole::Director, None),
        ];
        let shown: Vec<&str> = billed(&cast).map(|m| m.name.as_str()).collect();
        assert_eq!(shown, ["Sigourney Weaver"]);
    }

    #[test]
    fn an_actor_shows_the_part_they_play() {
        let ripley = member("Sigourney Weaver", PersonRole::Actor, Some("Ellen Ripley"));
        assert_eq!(character(&ripley), Some("Ellen Ripley"));
    }

    #[test]
    fn a_part_that_is_blank_has_no_line() {
        let unnamed = member("Tom Skerritt", PersonRole::Actor, Some("  "));
        assert_eq!(character(&unnamed), None);
    }

    #[test]
    fn up_next_is_the_episode_part_way_through() {
        let seasons = vec![
            (
                season(1),
                vec![episode(1, 1, None, true), episode(1, 2, None, false)],
            ),
            (season(2), vec![episode(2, 1, Some(600), false)]),
        ];
        assert_eq!(up_next(&seasons).map(episode_code), Some("S2 · E1".into()));
    }

    #[test]
    fn without_one_part_way_up_next_is_the_first_not_watched() {
        let seasons = vec![(
            season(1),
            vec![episode(1, 1, None, true), episode(1, 2, None, false)],
        )];
        assert_eq!(up_next(&seasons).map(episode_code), Some("S1 · E2".into()));
    }

    #[test]
    fn marking_a_season_marks_its_episodes_only() {
        let mut seasons = vec![
            (season(1), vec![episode(1, 1, None, false)]),
            (
                season(2),
                vec![episode(2, 1, Some(60), false), episode(2, 2, None, false)],
            ),
        ];
        let id = seasons[1].0.id.clone();
        mark_in(&mut seasons, &id, true);
        let watched: Vec<bool> = seasons
            .iter()
            .flat_map(|(_, e)| e.iter())
            .map(|i| matches!(i, Item::Episode(e) if e.playback.watched))
            .collect();
        assert_eq!(watched, [false, true, true]);
    }

    #[test]
    fn marking_the_show_marks_every_episode() {
        let mut seasons = vec![
            (season(1), vec![episode(1, 1, None, false)]),
            (season(2), vec![episode(2, 1, None, false)]),
        ];
        mark_in(&mut seasons, &id(MediaKind::Show, "show"), true);
        assert_eq!(up_next(&seasons), None);
    }

    #[test]
    fn a_show_all_watched_has_nothing_up_next() {
        let seasons = vec![(season(1), vec![episode(1, 1, None, true)])];
        assert_eq!(up_next(&seasons), None);
    }
}
