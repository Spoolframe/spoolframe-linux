//! What a library page's Sort menu and Filters pane say and ask, apart
//! from their widgets: the user's choice for a library, the request it
//! makes, the labels, the chips of an active filter, the badge and the
//! count, the toast on a choice that came back, and what a refused
//! request falls back to.
//!
//! The library says what it can be sorted and filtered by
//! ([`ItemFilterValues`]); what it does not say, or says in a value this
//! build does not know (`Unknown`), is never offered.

use spoolframe_linux_core::backend::{
    BackendError, Counted, DynamicRange, FilterPart, ItemFilter, ItemFilterValues, ItemSort,
    Resolution, Session, SortKey, WatchedState,
};
use spoolframe_types::{Item, LibraryId, LibraryKind};

/// Said when the server refuses a sort or filter the library listed: it
/// changed since, and the page shows it all again.
pub const FELL_BACK: &str = "This library's sort and filters changed; showing everything again";

/// What the user chose for one library: its order and what it keeps.
/// Saved per profile ([`crate::saved_choices`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Choice {
    pub sort: ItemSort,
    pub filter: ItemFilter,
}

impl Default for Choice {
    fn default() -> Self {
        Self {
            sort: ItemSort::DEFAULT,
            filter: ItemFilter::default(),
        }
    }
}

/// A sorted or filtered request, in `items_sorted`'s terms: the default
/// order and an empty filter are left out, as the server's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub sort: Option<ItemSort>,
    pub filter: Option<ItemFilter>,
}

impl Choice {
    /// The request it makes; `None` for the library as `items` gives it,
    /// which keeps its pages cached.
    pub fn request(&self) -> Option<Request> {
        let sort = (self.sort != ItemSort::DEFAULT).then_some(self.sort);
        let filter = (!self.filter.is_empty()).then(|| self.filter.clone());
        (sort.is_some() || filter.is_some()).then_some(Request { sort, filter })
    }

    /// Whether it keeps fewer than every item; a sort alone does not.
    pub fn filters(&self) -> bool {
        !self.filter.is_empty()
    }

    /// Whether it keeps items with `value`.
    pub fn has(&self, value: &Value) -> bool {
        let named =
            |names: &[String], name: &str| names.iter().any(|n| n.eq_ignore_ascii_case(name));
        match value {
            Value::Watched(state) => self.filter.watched.contains(state),
            Value::Genre(name) => named(&self.filter.genres, name),
            Value::ContentRating(name) => named(&self.filter.content_ratings, name),
            Value::Resolution(r) => self.filter.resolutions.contains(r),
            Value::DynamicRange(r) => self.filter.dynamic_ranges.contains(r),
        }
    }

    /// Keeps items with `value` too, or no longer.
    pub fn set(&mut self, value: &Value, on: bool) {
        fn toggle<T: PartialEq + Clone>(
            list: &mut Vec<T>,
            value: &T,
            on: bool,
            same: impl Fn(&T, &T) -> bool,
        ) {
            list.retain(|v| !same(v, value));
            if on {
                list.push(value.clone());
            }
        }
        let names = |a: &String, b: &String| a.eq_ignore_ascii_case(b);
        let filter = &mut self.filter;
        match value {
            Value::Watched(s) => toggle(&mut filter.watched, s, on, PartialEq::eq),
            Value::Genre(name) => toggle(&mut filter.genres, name, on, names),
            Value::ContentRating(name) => toggle(&mut filter.content_ratings, name, on, names),
            Value::Resolution(r) => toggle(&mut filter.resolutions, r, on, PartialEq::eq),
            Value::DynamicRange(r) => toggle(&mut filter.dynamic_ranges, r, on, PartialEq::eq),
        }
    }

    /// From `from` on, or from any year. A To before it moves up to it, so
    /// the range is never back to front.
    pub fn set_year_from(&mut self, from: Option<u16>) {
        self.filter.year_from = from;
        if let (Some(from), Some(to)) = (from, self.filter.year_to)
            && to < from
        {
            self.filter.year_to = Some(from);
        }
    }

    /// Up to `to`, or to any year. A From after it moves back to it.
    pub fn set_year_to(&mut self, to: Option<u16>) {
        self.filter.year_to = to;
        if let (Some(from), Some(to)) = (self.filter.year_from, to)
            && from > to
        {
            self.filter.year_from = Some(to);
        }
    }

    /// Without `part`: its Reset, and its chip's remove button. The other
    /// parts and the sort stay.
    pub fn reset(&mut self, part: FilterPart) {
        let filter = &mut self.filter;
        match part {
            FilterPart::Watched => filter.watched.clear(),
            FilterPart::Genres => filter.genres.clear(),
            FilterPart::Years => {
                filter.year_from = None;
                filter.year_to = None;
            }
            FilterPart::ContentRatings => filter.content_ratings.clear(),
            FilterPart::Resolutions => filter.resolutions.clear(),
            FilterPart::DynamicRanges => filter.dynamic_ranges.clear(),
            FilterPart::Unknown => {}
        }
    }
}

/// The parts of a filter in the order the pane, the chips and the badge
/// take them.
const PARTS: [FilterPart; 6] = [
    FilterPart::Watched,
    FilterPart::Genres,
    FilterPart::Years,
    FilterPart::ContentRatings,
    FilterPart::Resolutions,
    FilterPart::DynamicRanges,
];

/// Whether `filter` keeps items by `part`: what shows its Reset.
pub fn in_use(filter: &ItemFilter, part: FilterPart) -> bool {
    match part {
        FilterPart::Watched => !filter.watched.is_empty(),
        FilterPart::Genres => !filter.genres.is_empty(),
        FilterPart::Years => filter.year_from.is_some() || filter.year_to.is_some(),
        FilterPart::ContentRatings => !filter.content_ratings.is_empty(),
        FilterPart::Resolutions => !filter.resolutions.is_empty(),
        FilterPart::DynamicRanges => !filter.dynamic_ranges.is_empty(),
        FilterPart::Unknown => false,
    }
}

/// The Filters button's badge: how many parts are in use, not how many
/// values ("Sci-Fi or Thriller" is one).
pub fn parts_in_use(filter: &ItemFilter) -> usize {
    PARTS.iter().filter(|&&part| in_use(filter, part)).count()
}

/// The badge's name for a screen reader: "5 filters in use".
pub fn badge_label(parts: usize) -> String {
    match parts {
        1 => "1 filter in use".to_owned(),
        n => format!("{n} filters in use"),
    }
}

/// What to show for a choice in a library that lists `values`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The library as `items` gives it: nothing chosen, or nothing known of
    /// what the library offers (its values did not load, so it shows no
    /// controls to undo a choice with).
    Plain,
    Sorted(Request),
    /// The library no longer offers it: back to everything.
    Refused(String),
}

pub fn plan(choice: &Choice, values: Option<&ItemFilterValues>) -> Plan {
    let (Some(values), Some(request)) = (values, choice.request()) else {
        return Plan::Plain;
    };
    match values.refuses(request.sort.as_ref(), request.filter.as_ref()) {
        Some(why) => Plan::Refused(why),
        None => Plan::Sorted(request),
    }
}

/// A page of `library`, plainly or as `request` orders and keeps it.
pub async fn page(
    session: &dyn Session,
    library: &LibraryId,
    request: Option<&Request>,
    offset: u32,
    limit: u32,
) -> Result<(Vec<Item>, u32), BackendError> {
    match request {
        None => session.items(library, offset, limit).await,
        Some(request) => {
            session
                .items_sorted(
                    library,
                    offset,
                    limit,
                    request.sort,
                    request.filter.as_ref(),
                )
                .await
        }
    }
}

/// A sorted or filtered library's first page, as the page shows it.
#[derive(Debug)]
pub enum First {
    Items(Vec<Item>, u32),
    /// The filter keeps nothing: the empty state, not an empty grid.
    Nothing,
    /// The server refused it although the library listed it: it changed.
    Refused,
    Failed(BackendError),
}

/// Fetches the first page of `library` as `request` asks.
pub async fn first(
    session: &dyn Session,
    library: &LibraryId,
    request: &Request,
    limit: u32,
) -> First {
    match page(session, library, Some(request), 0, limit).await {
        Ok((_, 0)) if request.filter.is_some() => First::Nothing,
        Ok((items, total)) => First::Items(items, total),
        Err(e) => answer_to(e),
    }
}

/// What a sorted request's error means: refused, or failed like any call.
pub fn answer_to(error: BackendError) -> First {
    match error {
        BackendError::InvalidInput(_) => First::Refused,
        e => First::Failed(e),
    }
}

/// What a library's items are called, one and many: "37 of 412 films".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Noun {
    pub one: &'static str,
    pub many: &'static str,
}

impl Noun {
    /// The word for `n` of them.
    pub fn of(self, n: u32) -> &'static str {
        if n == 1 { self.one } else { self.many }
    }
}

/// What a library of `kind` lists. A music library lists its artists (the
/// boards say albums; the server's top-level items are artists).
pub fn noun(kind: LibraryKind) -> Noun {
    match kind {
        LibraryKind::Movies => Noun {
            one: "film",
            many: "films",
        },
        LibraryKind::Shows => Noun {
            one: "show",
            many: "shows",
        },
        LibraryKind::Music => Noun {
            one: "artist",
            many: "artists",
        },
    }
}

/// A sort key's name; `None` for one this build does not know.
pub fn key_label(key: SortKey) -> Option<&'static str> {
    Some(match key {
        SortKey::Title => "Title",
        SortKey::Released => "Released",
        SortKey::Added => "Added",
        SortKey::LastPlayed => "Last played",
        SortKey::Rating => "Rating",
        SortKey::AudienceRating => "Audience rating",
        SortKey::Duration => "Duration",
        SortKey::Size => "Size",
        SortKey::Unknown => return None,
    })
}

/// The two directions of `key`, ascending first, worded for it.
pub fn directions(key: SortKey) -> [&'static str; 2] {
    match key {
        SortKey::Title => ["A to Z", "Z to A"],
        SortKey::Released | SortKey::Added => ["Oldest", "Newest"],
        SortKey::LastPlayed => ["Longest ago", "Recently"],
        SortKey::Rating | SortKey::AudienceRating => ["Lowest", "Highest"],
        SortKey::Duration => ["Shortest", "Longest"],
        SortKey::Size => ["Smallest", "Largest"],
        SortKey::Unknown => ["Ascending", "Descending"],
    }
}

/// The Sort menu's two direction rows, top first, as whether each is
/// descending: A to Z leads for a title, the newest or largest for the
/// rest.
pub fn menu_directions(key: SortKey) -> [bool; 2] {
    match key {
        SortKey::Title | SortKey::Unknown => [false, true],
        _ => [true, false],
    }
}

/// The Sort button's label: "Released · Newest".
pub fn sort_label(sort: ItemSort) -> String {
    let key = key_label(sort.key).unwrap_or("Title");
    let direction = directions(sort.key)[usize::from(sort.descending)];
    format!("{key} · {direction}")
}

/// The line under the Sort menu: where items without the key's value go,
/// or, for a key every item has, what the library cannot be sorted by.
/// `offered` is the menu's keys.
pub fn sort_note(key: SortKey, noun: Noun, offered: &[SortKey]) -> Option<String> {
    let many = capitalized(noun.many);
    let missing = match key {
        SortKey::Released => Some("without a release date"),
        SortKey::LastPlayed => Some("never played"),
        SortKey::Rating | SortKey::AudienceRating => Some("without a rating"),
        _ => None,
    };
    if let Some(missing) = missing {
        return Some(format!("{many} {missing} come last, either way."));
    }
    let lacks = |keys: &[SortKey]| !keys.iter().any(|k| offered.contains(k));
    let absent: Vec<&str> = [
        (lacks(&[SortKey::Duration]), "duration"),
        (lacks(&[SortKey::Size]), "size"),
        (
            lacks(&[SortKey::Rating, SortKey::AudienceRating]),
            "ratings",
        ),
    ]
    .into_iter()
    .filter_map(|(absent, name)| absent.then_some(name))
    .collect();
    let (last, rest) = absent.split_last()?;
    let names = match rest {
        [] => (*last).to_owned(),
        rest => format!("{} or {last}", rest.join(", ")),
    };
    Some(format!("This library has no {names} to sort by."))
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The keys the Sort menu lists: the library's, in its order.
pub fn sort_keys(values: &ItemFilterValues) -> Vec<SortKey> {
    let mut keys: Vec<SortKey> = Vec::new();
    for key in &values.sorts {
        if key_label(*key).is_some() && !keys.contains(key) {
            keys.push(*key);
        }
    }
    keys
}

/// A value a filter keeps items by, as a check row and a chip name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Watched(WatchedState),
    Genre(String),
    ContentRating(String),
    Resolution(Resolution),
    DynamicRange(DynamicRange),
}

impl Value {
    /// Its name; `None` for one this build does not know.
    pub fn label(&self) -> Option<String> {
        let name = match self {
            Value::Watched(WatchedState::Unwatched) => "Unwatched",
            Value::Watched(WatchedState::InProgress) => "In progress",
            Value::Watched(WatchedState::Watched) => "Watched",
            Value::Resolution(Resolution::Uhd) => "4K",
            Value::Resolution(Resolution::FullHd) => "1080p",
            Value::Resolution(Resolution::Hd) => "720p",
            Value::Resolution(Resolution::Sd) => "SD",
            Value::DynamicRange(DynamicRange::Sdr) => "SDR",
            // The server keeps Dolby Vision under HDR too.
            Value::DynamicRange(DynamicRange::Hdr) => "HDR (includes Dolby Vision)",
            Value::DynamicRange(DynamicRange::DolbyVision) => "Dolby Vision",
            Value::Watched(WatchedState::Unknown)
            | Value::Resolution(Resolution::Unknown)
            | Value::DynamicRange(DynamicRange::Unknown) => return None,
            Value::Genre(name) | Value::ContentRating(name) => return Some(name.clone()),
        };
        Some(name.to_owned())
    }
}

/// One check row of the Filters pane (or, for Watched, one toggle).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub value: Value,
    pub label: String,
    /// How many items have it; `None` when the server did not say, and
    /// the row shows no count.
    pub count: Option<u32>,
}

/// What a section of the Filters pane holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Checks(Vec<Check>),
    /// The years to pick a From and a To of, newest first, as the
    /// dropdowns list them, each with its count when the server says.
    Years(Vec<Counted<u16>>),
}

/// One section of the Filters pane: a part of the filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub part: FilterPart,
    pub title: &'static str,
    pub body: Body,
}

/// How many check rows a section shows before its "Show all".
pub const SHOWN_CHECKS: usize = 6;

/// The button under a long section: "Show all 23 genres".
pub fn show_all_label(part: FilterPart, n: usize) -> String {
    let what = match part {
        FilterPart::Genres => "genres",
        FilterPart::ContentRatings => "content ratings",
        FilterPart::Resolutions => "resolutions",
        FilterPart::DynamicRanges => "dynamic ranges",
        FilterPart::Watched | FilterPart::Years | FilterPart::Unknown => "values",
    };
    format!("Show all {n} {what}")
}

/// How many columns a section's `n` check rows take, as the boards lay
/// them: short names (ratings, resolutions) side by side, three across
/// from five of them; long ones (genres, ranges) one under another.
pub fn columns(part: FilterPart, n: usize) -> u32 {
    match part {
        FilterPart::ContentRatings | FilterPart::Resolutions if n > 4 => 3,
        FilterPart::ContentRatings | FilterPart::Resolutions => 2,
        _ => 1,
    }
}

/// The Filters pane's sections: each part the library offers, in its
/// order, with the values it lists. A part with nothing to pick is left
/// out.
pub fn sections(values: &ItemFilterValues) -> Vec<Section> {
    fn checks<T: Clone>(list: &[Counted<T>], value: impl Fn(T) -> Value) -> Body {
        Body::Checks(
            list.iter()
                .filter_map(|c| {
                    let value = value(c.value.clone());
                    let label = value.label()?;
                    Some(Check {
                        value,
                        label,
                        count: c.count,
                    })
                })
                .collect(),
        )
    }
    let mut sections: Vec<Section> = Vec::new();
    for &part in &values.filters {
        if sections.iter().any(|s| s.part == part) {
            continue;
        }
        let (title, body) = match part {
            FilterPart::Watched => (
                "Watched",
                Body::Checks(
                    [
                        WatchedState::Unwatched,
                        WatchedState::InProgress,
                        WatchedState::Watched,
                    ]
                    .into_iter()
                    .map(|state| {
                        let value = Value::Watched(state);
                        let label = value.label().unwrap_or_default();
                        Check {
                            value,
                            label,
                            count: None,
                        }
                    })
                    .collect(),
                ),
            ),
            FilterPart::Genres => ("Genres", checks(&values.genres, Value::Genre)),
            FilterPart::Years => {
                let mut years = values.years.clone();
                years.sort_by_key(|c| std::cmp::Reverse(c.value));
                years.dedup_by_key(|c| c.value);
                ("Years", Body::Years(years))
            }
            FilterPart::ContentRatings => (
                "Content rating",
                checks(&values.content_ratings, Value::ContentRating),
            ),
            FilterPart::Resolutions => {
                ("Resolution", checks(&values.resolutions, Value::Resolution))
            }
            FilterPart::DynamicRanges => (
                "Dynamic range",
                checks(&values.dynamic_ranges, Value::DynamicRange),
            ),
            FilterPart::Unknown => continue,
        };
        let empty = match &body {
            Body::Checks(checks) => checks.is_empty(),
            Body::Years(years) => years.is_empty(),
        };
        if !empty {
            sections.push(Section { part, title, body });
        }
    }
    sections
}

/// The chips over the grid: one per part in use, its values joined by
/// "or" ("Sci-Fi or Thriller"), as the pane orders the parts. Removing
/// one resets its part ([`Choice::reset`]). Values this build cannot name
/// are left out, and a part with none it can name has no chip.
pub fn chips(filter: &ItemFilter) -> Vec<(FilterPart, String)> {
    fn named(values: impl Iterator<Item = Value>) -> Option<String> {
        let names: Vec<String> = values.filter_map(|v| v.label()).collect();
        (!names.is_empty()).then(|| names.join(" or "))
    }
    PARTS
        .into_iter()
        .filter_map(|part| {
            let text = match part {
                FilterPart::Watched => named(filter.watched.iter().map(|s| Value::Watched(*s))),
                FilterPart::Genres => named(filter.genres.iter().cloned().map(Value::Genre)),
                FilterPart::Years => years_text(filter.year_from, filter.year_to),
                FilterPart::ContentRatings => named(
                    filter
                        .content_ratings
                        .iter()
                        .cloned()
                        .map(Value::ContentRating),
                ),
                FilterPart::Resolutions => {
                    named(filter.resolutions.iter().map(|r| Value::Resolution(*r)))
                }
                FilterPart::DynamicRanges => named(
                    filter
                        .dynamic_ranges
                        .iter()
                        .map(|r| Value::DynamicRange(*r)),
                ),
                FilterPart::Unknown => None,
            };
            Some((part, text?))
        })
        .collect()
}

/// The start of the chips row: the choice is saved for this library.
pub fn kept_for(library: &str) -> String {
    format!("Kept for {library}")
}

/// Told on opening a library whose saved sort and filters came back.
pub fn restored_text(library: &str) -> String {
    format!("Your sort and filters for {library} are back.")
}

/// Whether opening a library tells the user its choice came back: only
/// the first time it opens in the session (`first`), only for a choice
/// other than the default, and only when that choice is shown (`plan`),
/// not refused or left out for want of the library's values.
pub fn greets(choice: &Choice, plan: &Plan, first: bool) -> bool {
    first && *choice != Choice::default() && matches!(plan, Plan::Sorted(_))
}

/// The empty state's line, under "No items match these filters".
pub fn nothing_matches_text(noun: Noun, library: &str) -> String {
    format!(
        "Remove a filter above, or clear them all to see every {} in {library}.",
        noun.one
    )
}

/// The year chip's text: "1990–1999", "From 2010", "Until 1985", or one
/// year; `None` without a range.
pub fn years_text(from: Option<u16>, to: Option<u16>) -> Option<String> {
    match (from, to) {
        (Some(from), Some(to)) if from == to => Some(from.to_string()),
        (Some(from), Some(to)) => Some(format!("{from}–{to}")),
        (Some(from), None) => Some(format!("From {from}")),
        (None, Some(to)) => Some(format!("Until {to}")),
        (None, None) => None,
    }
}

/// How many a filter keeps of the library, the number apart so it can
/// stand out: ("37", " of 412 films"). Without a filter, the library's
/// own count: ("412", " films").
pub fn count_parts(kept: Option<u32>, all: u32, noun: Noun) -> (String, String) {
    match kept {
        Some(kept) => (kept.to_string(), format!(" of {all} {}", noun.of(all))),
        None => (all.to_string(), format!(" {}", noun.of(all))),
    }
}

/// [`count_parts`] as one line: "37 of 412 films".
pub fn count_text(kept: Option<u32>, all: u32, noun: Noun) -> String {
    let (number, rest) = count_parts(kept, all, noun);
    number + &rest
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::Client;
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::ready;
    use spoolframe_types::{Library, LibraryKind};

    use super::*;

    fn session() -> std::sync::Arc<dyn Session> {
        FakeClient::new(None)
            .session(&FakeClient::profile("andrey"))
            .unwrap()
    }

    fn library(session: &dyn Session, kind: LibraryKind) -> Library {
        ready(session.libraries())
            .unwrap()
            .into_iter()
            .find(|l| l.kind == kind)
            .unwrap()
    }

    fn titles(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                Item::Movie(m) => m.title.clone(),
                Item::Show(s) => s.title.clone(),
                Item::Artist(a) => a.name.clone(),
                other => other.id().to_string(),
            })
            .collect()
    }

    fn sorted(key: SortKey, descending: bool) -> Choice {
        Choice {
            sort: ItemSort { key, descending },
            ..Choice::default()
        }
    }

    fn genre(name: &str) -> Choice {
        let mut choice = Choice::default();
        choice.set(&Value::Genre(name.into()), true);
        choice
    }

    #[test]
    fn nothing_chosen_asks_the_plain_cached_listing() {
        assert_eq!(Choice::default().request(), None);
    }

    #[test]
    fn a_sort_alone_asks_for_that_order_and_no_filter() {
        assert_eq!(
            sorted(SortKey::Added, true).request(),
            Some(Request {
                sort: Some(ItemSort {
                    key: SortKey::Added,
                    descending: true
                }),
                filter: None
            })
        );
    }

    #[test]
    fn a_filter_in_the_default_order_leaves_the_sort_out() {
        assert_eq!(genre("Drama").request().unwrap().sort, None);
    }

    #[test]
    fn a_sort_alone_is_not_a_filter() {
        assert!(!sorted(SortKey::Rating, false).filters());
    }

    #[test]
    fn ticking_a_value_twice_keeps_it_once() {
        let mut choice = genre("Drama");
        choice.set(&Value::Genre("drama".into()), true);
        assert_eq!(choice.filter.genres.len(), 1);
    }

    #[test]
    fn unticking_a_value_takes_it_off_case_aside() {
        let mut choice = genre("Drama");
        choice.set(&Value::Genre("DRAMA".into()), false);
        assert!(choice.filter.is_empty());
    }

    #[test]
    fn the_default_sort_reads_title_a_to_z() {
        assert_eq!(sort_label(ItemSort::DEFAULT), "Title · A to Z");
    }

    #[test]
    fn the_sort_label_says_the_key_and_its_direction() {
        let sort = ItemSort {
            key: SortKey::Released,
            descending: true,
        };
        assert_eq!(sort_label(sort), "Released · Newest");
    }

    #[test]
    fn last_played_newest_first_reads_recently() {
        let sort = ItemSort {
            key: SortKey::LastPlayed,
            descending: true,
        };
        assert_eq!(sort_label(sort), "Last played · Recently");
    }

    #[test]
    fn each_key_words_its_directions() {
        let worded: Vec<[&str; 2]> = SortKey::ALL.into_iter().map(directions).collect();
        assert_eq!(
            worded,
            vec![
                ["A to Z", "Z to A"],
                ["Oldest", "Newest"],
                ["Oldest", "Newest"],
                ["Longest ago", "Recently"],
                ["Lowest", "Highest"],
                ["Lowest", "Highest"],
                ["Shortest", "Longest"],
                ["Smallest", "Largest"],
            ]
        );
    }

    #[test]
    fn the_sort_menu_lists_a_title_a_to_z_first() {
        assert_eq!(menu_directions(SortKey::Title), [false, true]);
    }

    #[test]
    fn the_sort_menu_lists_a_date_newest_first() {
        assert_eq!(menu_directions(SortKey::Released), [true, false]);
    }

    fn films() -> Noun {
        noun(LibraryKind::Movies)
    }

    #[test]
    fn a_release_date_sort_notes_where_undated_films_go() {
        assert_eq!(
            sort_note(SortKey::Released, films(), &SortKey::ALL).as_deref(),
            Some("Films without a release date come last, either way.")
        );
    }

    #[test]
    fn a_key_every_item_has_notes_what_the_library_cannot_sort_by() {
        let offered = [
            SortKey::Title,
            SortKey::Released,
            SortKey::Added,
            SortKey::LastPlayed,
            SortKey::Duration,
        ];
        assert_eq!(
            sort_note(SortKey::Duration, films(), &offered).as_deref(),
            Some("This library has no size or ratings to sort by.")
        );
    }

    #[test]
    fn a_library_with_every_key_has_no_note_by_title() {
        assert_eq!(sort_note(SortKey::Title, films(), &SortKey::ALL), None);
    }

    #[test]
    fn each_kind_names_what_it_lists() {
        let nouns: Vec<&str> = [LibraryKind::Movies, LibraryKind::Shows, LibraryKind::Music]
            .into_iter()
            .map(|kind| noun(kind).many)
            .collect();
        assert_eq!(nouns, vec!["films", "shows", "artists"]);
    }

    #[test]
    fn the_sort_menu_lists_the_librarys_keys_in_its_order_without_unknown_ones() {
        let values = ItemFilterValues {
            sorts: vec![SortKey::Added, SortKey::Unknown, SortKey::Title],
            ..ItemFilterValues::default()
        };
        assert_eq!(sort_keys(&values), vec![SortKey::Added, SortKey::Title]);
    }

    fn genres(counts: &[Option<u32>]) -> Body {
        let values = ItemFilterValues {
            genres: counts
                .iter()
                .enumerate()
                .map(|(n, &count)| Counted {
                    value: format!("Genre {n}"),
                    count,
                })
                .collect(),
            filters: vec![FilterPart::Genres],
            ..ItemFilterValues::default()
        };
        sections(&values).remove(0).body
    }

    #[test]
    fn a_value_keeps_the_count_the_server_sent() {
        let Body::Checks(checks) = genres(&[Some(42)]) else {
            panic!("no checks");
        };
        assert_eq!(checks[0].count, Some(42));
    }

    #[test]
    fn a_value_the_server_sent_no_count_for_shows_none() {
        let Body::Checks(checks) = genres(&[None]) else {
            panic!("no checks");
        };
        assert_eq!(
            (checks[0].label.as_str(), checks[0].count),
            ("Genre 0", None)
        );
    }

    #[test]
    fn hdr_says_it_includes_dolby_vision() {
        assert_eq!(
            Value::DynamicRange(DynamicRange::Hdr).label().as_deref(),
            Some("HDR (includes Dolby Vision)")
        );
    }

    #[test]
    fn a_long_section_offers_to_show_all_its_values() {
        assert_eq!(show_all_label(FilterPart::Genres, 23), "Show all 23 genres");
    }

    #[test]
    fn six_content_ratings_sit_three_across() {
        assert_eq!(columns(FilterPart::ContentRatings, 6), 3);
    }

    #[test]
    fn four_resolutions_sit_two_across() {
        assert_eq!(columns(FilterPart::Resolutions, 4), 2);
    }

    #[test]
    fn genres_sit_one_under_another() {
        assert_eq!(columns(FilterPart::Genres, 6), 1);
    }

    #[test]
    fn sections_follow_the_librarys_order_without_unknown_parts() {
        let values = ItemFilterValues {
            genres: vec![Counted {
                value: "Drama".into(),
                count: None,
            }],
            filters: vec![FilterPart::Genres, FilterPart::Unknown, FilterPart::Watched],
            ..ItemFilterValues::default()
        };
        let parts: Vec<FilterPart> = sections(&values).into_iter().map(|s| s.part).collect();
        assert_eq!(parts, vec![FilterPart::Genres, FilterPart::Watched]);
    }

    #[test]
    fn unknown_values_are_not_offered() {
        let values = ItemFilterValues {
            resolutions: vec![
                Counted {
                    value: Resolution::Unknown,
                    count: Some(3),
                },
                Counted {
                    value: Resolution::Uhd,
                    count: Some(2),
                },
            ],
            filters: vec![FilterPart::Resolutions],
            ..ItemFilterValues::default()
        };
        assert_eq!(
            sections(&values)[0].body,
            Body::Checks(vec![Check {
                value: Value::Resolution(Resolution::Uhd),
                label: "4K".into(),
                count: Some(2),
            }])
        );
    }

    #[test]
    fn a_part_with_nothing_to_pick_is_left_out() {
        let values = ItemFilterValues {
            filters: vec![FilterPart::Genres],
            ..ItemFilterValues::default()
        };
        assert!(sections(&values).is_empty());
    }

    #[test]
    fn years_are_offered_newest_first() {
        let values = ItemFilterValues {
            years: [1995, 2016, 1979]
                .map(|value| Counted { value, count: None })
                .to_vec(),
            filters: vec![FilterPart::Years],
            ..ItemFilterValues::default()
        };
        let Body::Years(years) = &sections(&values)[0].body else {
            panic!("no years");
        };
        let years: Vec<u16> = years.iter().map(|c| c.value).collect();
        assert_eq!(years, vec![2016, 1995, 1979]);
    }

    #[test]
    fn a_from_after_the_to_moves_the_to_up() {
        let mut choice = Choice::default();
        choice.set_year_to(Some(1990));
        choice.set_year_from(Some(2000));
        assert_eq!(
            (choice.filter.year_from, choice.filter.year_to),
            (Some(2000), Some(2000))
        );
    }

    #[test]
    fn a_to_before_the_from_moves_the_from_back() {
        let mut choice = Choice::default();
        choice.set_year_from(Some(2000));
        choice.set_year_to(Some(1990));
        assert_eq!(
            (choice.filter.year_from, choice.filter.year_to),
            (Some(1990), Some(1990))
        );
    }

    #[test]
    fn any_year_leaves_the_other_end_as_it_is() {
        let mut choice = Choice::default();
        choice.set_year_from(Some(1990));
        choice.set_year_to(Some(1999));
        choice.set_year_from(None);
        assert_eq!(
            (choice.filter.year_from, choice.filter.year_to),
            (None, Some(1999))
        );
    }

    #[test]
    fn a_decade_is_one_chip() {
        assert_eq!(
            years_text(Some(1990), Some(1999)).as_deref(),
            Some("1990–1999")
        );
    }

    #[test]
    fn a_range_open_at_the_end_reads_from() {
        assert_eq!(years_text(Some(2010), None).as_deref(), Some("From 2010"));
    }

    #[test]
    fn a_range_open_at_the_start_reads_until() {
        assert_eq!(years_text(None, Some(1985)).as_deref(), Some("Until 1985"));
    }

    /// Movies' board: Unwatched, Sci-Fi or Thriller, 1990–2016, 4K, HDR.
    fn boards_choice() -> Choice {
        let mut choice = Choice::default();
        choice.set(&Value::DynamicRange(DynamicRange::Hdr), true);
        choice.set(&Value::Resolution(Resolution::Uhd), true);
        choice.set(&Value::Genre("Sci-Fi".into()), true);
        choice.set(&Value::Genre("Thriller".into()), true);
        choice.set(&Value::Watched(WatchedState::Unwatched), true);
        choice.set_year_from(Some(1990));
        choice.set_year_to(Some(2016));
        choice
    }

    #[test]
    fn chips_name_each_part_its_values_joined_by_or() {
        let texts: Vec<String> = chips(&boards_choice().filter)
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(
            texts,
            vec![
                "Unwatched",
                "Sci-Fi or Thriller",
                "1990–2016",
                "4K",
                "HDR (includes Dolby Vision)"
            ]
        );
    }

    #[test]
    fn a_value_this_build_cannot_name_gets_no_chip() {
        let mut choice = Choice::default();
        choice.set(&Value::Watched(WatchedState::Unknown), true);
        assert!(chips(&choice.filter).is_empty());
    }

    #[test]
    fn the_badge_counts_parts_not_values() {
        assert_eq!(parts_in_use(&boards_choice().filter), 5);
    }

    #[test]
    fn no_filter_has_no_parts_in_use() {
        assert_eq!(parts_in_use(&ItemFilter::default()), 0);
    }

    #[test]
    fn one_part_in_use_is_named_so() {
        assert_eq!(badge_label(1), "1 filter in use");
    }

    #[test]
    fn a_year_from_alone_puts_the_years_in_use() {
        let mut choice = Choice::default();
        choice.set_year_from(Some(1990));
        assert!(in_use(&choice.filter, FilterPart::Years));
    }

    #[test]
    fn resetting_the_years_clears_both_ends() {
        let mut choice = Choice::default();
        choice.set_year_from(Some(1990));
        choice.set_year_to(Some(1999));
        choice.reset(FilterPart::Years);
        assert!(choice.filter.is_empty());
    }

    #[test]
    fn resetting_a_part_leaves_the_others_and_the_sort() {
        let mut choice = boards_choice();
        choice.sort = ItemSort {
            key: SortKey::Released,
            descending: true,
        };
        choice.reset(FilterPart::Genres);
        assert_eq!(
            (
                parts_in_use(&choice.filter),
                choice.filter.genres.len(),
                choice.sort.key
            ),
            (4, 0, SortKey::Released)
        );
    }

    #[test]
    fn the_count_says_how_many_of_all_films() {
        assert_eq!(count_text(Some(37), 412, films()), "37 of 412 films");
    }

    #[test]
    fn without_a_filter_the_count_is_the_librarys() {
        assert_eq!(count_text(None, 412, films()), "412 films");
    }

    #[test]
    fn one_film_is_one() {
        assert_eq!(count_text(None, 1, films()), "1 film");
    }

    #[test]
    fn the_count_sets_its_number_apart() {
        assert_eq!(
            count_parts(Some(37), 412, films()),
            ("37".to_owned(), " of 412 films".to_owned())
        );
    }

    #[test]
    fn the_chips_row_says_whose_choice_it_is() {
        assert_eq!(kept_for("Movies"), "Kept for Movies");
    }

    #[test]
    fn the_toast_says_the_choice_is_back() {
        assert_eq!(
            restored_text("Movies"),
            "Your sort and filters for Movies are back."
        );
    }

    #[test]
    fn the_empty_state_says_what_clearing_shows() {
        assert_eq!(
            nothing_matches_text(films(), "Movies"),
            "Remove a filter above, or clear them all to see every film in Movies."
        );
    }

    fn shown(choice: &Choice) -> Plan {
        Plan::Sorted(choice.request().unwrap())
    }

    #[test]
    fn a_restored_choice_greets_on_the_first_open() {
        let choice = genre("Drama");
        assert!(greets(&choice, &shown(&choice), true));
    }

    #[test]
    fn a_restored_choice_does_not_greet_again() {
        let choice = genre("Drama");
        assert!(!greets(&choice, &shown(&choice), false));
    }

    #[test]
    fn the_default_does_not_greet() {
        assert!(!greets(&Choice::default(), &Plan::Plain, true));
    }

    #[test]
    fn a_refused_choice_does_not_greet() {
        assert!(!greets(
            &genre("Drama"),
            &Plan::Refused("gone".into()),
            true
        ));
    }

    #[test]
    fn a_choice_shown_plainly_for_want_of_values_does_not_greet() {
        assert!(!greets(&genre("Drama"), &Plan::Plain, true));
    }

    #[test]
    fn without_the_librarys_values_the_choice_is_not_sent() {
        assert_eq!(plan(&genre("Drama"), None), Plan::Plain);
    }

    #[test]
    fn a_choice_the_library_no_longer_lists_is_refused_before_sending() {
        let values = ItemFilterValues {
            sorts: vec![SortKey::Title],
            ..ItemFilterValues::default()
        };
        assert!(matches!(
            plan(&sorted(SortKey::Duration, false), Some(&values)),
            Plan::Refused(_)
        ));
    }

    #[test]
    fn date_added_newest_first_orders_the_movies_so() {
        let session = session();
        let movies = library(session.as_ref(), LibraryKind::Movies);
        let request = sorted(SortKey::Added, true).request().unwrap();
        let First::Items(items, _) = ready(first(session.as_ref(), &movies.id, &request, 500))
        else {
            panic!("no items");
        };
        assert_eq!(
            titles(&items),
            vec![
                "Arrival",
                "Heat",
                "Blade Runner",
                "Moon",
                "Sunshine",
                "Contact",
                "Dune: Part Two",
                "Alien"
            ]
        );
    }

    #[test]
    fn a_genre_narrows_the_movies_and_the_count_says_of_how_many() {
        let session = session();
        let movies = library(session.as_ref(), LibraryKind::Movies);
        let (_, all) = ready(session.items(&movies.id, 0, 1)).unwrap();
        let request = genre("Drama").request().unwrap();
        let First::Items(_, kept) = ready(first(session.as_ref(), &movies.id, &request, 500))
        else {
            panic!("no items");
        };
        assert_eq!(count_text(Some(kept), all, films()), "4 of 8 films");
    }

    #[test]
    fn a_music_library_has_no_watched_section() {
        let session = session();
        let music = library(session.as_ref(), LibraryKind::Music);
        let values = ready(session.item_filter_values(&music.id)).unwrap();
        assert!(
            !sections(&values)
                .iter()
                .any(|s| s.part == FilterPart::Watched)
        );
    }

    #[test]
    fn a_filter_that_keeps_nothing_is_the_empty_state() {
        let session = session();
        let movies = library(session.as_ref(), LibraryKind::Movies);
        // Alien, the one horror film, is 1080p.
        let mut choice = genre("Horror");
        choice.set(&Value::Resolution(Resolution::Uhd), true);
        let request = choice.request().unwrap();
        assert!(matches!(
            ready(first(session.as_ref(), &movies.id, &request, 500)),
            First::Nothing
        ));
    }

    #[test]
    fn a_sort_the_server_refuses_after_all_falls_back() {
        let session = session();
        let shows = library(session.as_ref(), LibraryKind::Shows);
        // The movies' values, as if the TV library had listed duration
        // before it changed.
        let movies = library(session.as_ref(), LibraryKind::Movies);
        let stale = ready(session.item_filter_values(&movies.id)).unwrap();
        let Plan::Sorted(request) = plan(&sorted(SortKey::Duration, true), Some(&stale)) else {
            panic!("refused before sending");
        };
        assert!(matches!(
            ready(first(session.as_ref(), &shows.id, &request, 500)),
            First::Refused
        ));
    }

    #[test]
    fn an_error_other_than_a_refusal_is_a_failure() {
        assert!(matches!(
            answer_to(BackendError::Unreachable("down".into())),
            First::Failed(_)
        ));
    }
}
