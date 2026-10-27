//! What a library page's Sort and Filter menus say and ask, apart from
//! their widgets: the user's choice for a library, the request it makes,
//! the labels, the chips of an active filter and its count, and what a
//! refused request falls back to.
//!
//! The library says what it can be sorted and filtered by
//! ([`ItemFilterValues`]); what it does not say, or says in a value this
//! build does not know (`Unknown`), is never offered.

use spoolframe_linux_core::backend::{
    BackendError, Counted, DynamicRange, FilterPart, ItemFilter, ItemFilterValues, ItemSort,
    Resolution, Session, SortKey, WatchedState,
};
use spoolframe_types::{Item, LibraryId};

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

    /// Without what `chip` stands for.
    pub fn remove(&mut self, chip: &Chip) {
        match chip {
            Chip::Value(value) => self.set(value, false),
            Chip::Years => {
                self.filter.year_from = None;
                self.filter.year_to = None;
            }
        }
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

/// A sort key's name; `None` for one this build does not know.
pub fn key_label(key: SortKey) -> Option<&'static str> {
    Some(match key {
        SortKey::Title => "Title",
        SortKey::Released => "Release date",
        SortKey::Added => "Date added",
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
        SortKey::Released | SortKey::Added | SortKey::LastPlayed => {
            ["Oldest first", "Newest first"]
        }
        SortKey::Rating | SortKey::AudienceRating => ["Lowest first", "Highest first"],
        SortKey::Duration => ["Shortest first", "Longest first"],
        SortKey::Size => ["Smallest first", "Largest first"],
        SortKey::Unknown => ["Ascending", "Descending"],
    }
}

/// The Sort button's label: "Date added, newest first".
pub fn sort_label(sort: ItemSort) -> String {
    let key = key_label(sort.key).unwrap_or("Title");
    let direction = directions(sort.key)[usize::from(sort.descending)];
    // "A to Z" keeps its capital; "Newest first" reads on in lower case.
    let direction = match sort.key {
        SortKey::Title => direction.to_owned(),
        _ => {
            let mut chars = direction.chars();
            chars
                .next()
                .map(|c| c.to_lowercase().chain(chars).collect())
                .unwrap_or_default()
        }
    };
    format!("{key}, {direction}")
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
            Value::DynamicRange(DynamicRange::Hdr) => "HDR",
            Value::DynamicRange(DynamicRange::DolbyVision) => "Dolby Vision",
            Value::Watched(WatchedState::Unknown)
            | Value::Resolution(Resolution::Unknown)
            | Value::DynamicRange(DynamicRange::Unknown) => return None,
            Value::Genre(name) | Value::ContentRating(name) => return Some(name.clone()),
        };
        Some(name.to_owned())
    }
}

/// A check row's title: the value, and how many items have it when the
/// server says ("Drama · 42").
pub fn counted_label(label: &str, count: Option<u32>) -> String {
    match count {
        Some(count) => format!("{label} · {count}"),
        None => label.to_owned(),
    }
}

/// One check row of the Filter menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub value: Value,
    pub label: String,
}

/// What a section of the Filter menu holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Checks(Vec<Check>),
    /// The years to pick a From and a To of, oldest first.
    Years(Vec<u16>),
}

/// One section of the Filter menu: a part of the filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub part: FilterPart,
    pub title: &'static str,
    pub body: Body,
}

/// The Filter menu's sections: each part the library offers, in its
/// order, with the values it lists. A part with nothing to pick is left
/// out.
pub fn sections(values: &ItemFilterValues) -> Vec<Section> {
    fn checks<T: Clone>(list: &[Counted<T>], value: impl Fn(T) -> Value) -> Body {
        Body::Checks(
            list.iter()
                .filter_map(|c| {
                    let value = value(c.value.clone());
                    let label = counted_label(&value.label()?, c.count);
                    Some(Check { value, label })
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
                        Check { value, label }
                    })
                    .collect(),
                ),
            ),
            FilterPart::Genres => ("Genres", checks(&values.genres, Value::Genre)),
            FilterPart::Years => {
                let mut years: Vec<u16> = values.years.iter().map(|c| c.value).collect();
                years.sort_unstable();
                years.dedup();
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

/// What a chip under the header stands for, and takes off when removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chip {
    Value(Value),
    /// The year range, as one chip.
    Years,
}

/// The chips of `filter`, each with its text, in the filter's own order.
pub fn chips(filter: &ItemFilter) -> Vec<(Chip, String)> {
    let mut chips = Vec::new();
    let mut add = |value: Value| {
        if let Some(label) = value.label() {
            chips.push((Chip::Value(value), label));
        }
    };
    filter.watched.iter().for_each(|s| add(Value::Watched(*s)));
    filter
        .genres
        .iter()
        .for_each(|g| add(Value::Genre(g.clone())));
    let years = years_text(filter.year_from, filter.year_to);
    filter
        .content_ratings
        .iter()
        .for_each(|r| add(Value::ContentRating(r.clone())));
    filter
        .resolutions
        .iter()
        .for_each(|r| add(Value::Resolution(*r)));
    filter
        .dynamic_ranges
        .iter()
        .for_each(|r| add(Value::DynamicRange(*r)));
    if let Some(years) = years {
        // After the genres, as the parts go.
        let at = filter.watched.len() + filter.genres.len();
        chips.insert(at.min(chips.len()), (Chip::Years, years));
    }
    chips
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

/// How many a filter keeps of the library: "37 of 412".
pub fn count_text(kept: u32, all: u32) -> String {
    format!("{kept} of {all}")
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
        assert_eq!(sort_label(ItemSort::DEFAULT), "Title, A to Z");
    }

    #[test]
    fn the_sort_label_says_the_key_and_its_direction() {
        let sort = ItemSort {
            key: SortKey::Added,
            descending: true,
        };
        assert_eq!(sort_label(sort), "Date added, newest first");
    }

    #[test]
    fn each_key_words_its_directions() {
        let worded: Vec<[&str; 2]> = SortKey::ALL.into_iter().map(directions).collect();
        assert_eq!(
            worded,
            vec![
                ["A to Z", "Z to A"],
                ["Oldest first", "Newest first"],
                ["Oldest first", "Newest first"],
                ["Oldest first", "Newest first"],
                ["Lowest first", "Highest first"],
                ["Lowest first", "Highest first"],
                ["Shortest first", "Longest first"],
                ["Smallest first", "Largest first"],
            ]
        );
    }

    #[test]
    fn the_sort_menu_lists_the_librarys_keys_in_its_order_without_unknown_ones() {
        let values = ItemFilterValues {
            sorts: vec![SortKey::Added, SortKey::Unknown, SortKey::Title],
            ..ItemFilterValues::default()
        };
        assert_eq!(sort_keys(&values), vec![SortKey::Added, SortKey::Title]);
    }

    #[test]
    fn a_count_follows_its_value() {
        assert_eq!(counted_label("Drama", Some(42)), "Drama · 42");
    }

    #[test]
    fn a_value_without_a_count_is_its_name_alone() {
        assert_eq!(counted_label("Drama", None), "Drama");
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
                label: "4K · 2".into()
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
    fn years_are_offered_oldest_first() {
        let values = ItemFilterValues {
            years: [2016, 1995, 1979]
                .map(|value| Counted { value, count: None })
                .to_vec(),
            filters: vec![FilterPart::Years],
            ..ItemFilterValues::default()
        };
        assert_eq!(
            sections(&values)[0].body,
            Body::Years(vec![1979, 1995, 2016])
        );
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

    #[test]
    fn chips_name_each_value_with_the_years_as_one() {
        let mut choice = genre("Drama");
        choice.set(&Value::Watched(WatchedState::Unwatched), true);
        choice.set(&Value::Resolution(Resolution::Uhd), true);
        choice.set_year_from(Some(1990));
        choice.set_year_to(Some(1999));
        let texts: Vec<String> = chips(&choice.filter).into_iter().map(|(_, t)| t).collect();
        assert_eq!(texts, vec!["Unwatched", "Drama", "1990–1999", "4K"]);
    }

    #[test]
    fn removing_the_year_chip_clears_both_ends() {
        let mut choice = Choice::default();
        choice.set_year_from(Some(1990));
        choice.set_year_to(Some(1999));
        choice.remove(&Chip::Years);
        assert!(choice.filter.is_empty());
    }

    #[test]
    fn the_count_says_how_many_of_all() {
        assert_eq!(count_text(37, 412), "37 of 412");
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
        assert_eq!(count_text(kept, all), "4 of 8");
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
