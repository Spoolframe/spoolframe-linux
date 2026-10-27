//! Sorting and filtering the fake server's libraries (SDK 0.16), by the
//! rules the protocol states: a sort orders by its key, then by sort
//! title A to Z, then by id, and an item without a value for the key comes
//! last both ways; a filter's parts combine with AND and the values of one
//! part with OR, and an item without a value for a part it names is not
//! kept. What each kind of library offers is what the server's own (Plex)
//! libraries offer: never `size`, as Plex does not sort by it; a movie
//! library every other key, part and range; a TV library no duration,
//! resolution or range, as a show has no file of its own; music its title,
//! when it was added and its genres, with no watch state.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use spoolframe_types::browse::sort_title;
use spoolframe_types::{
    CalendarDate, Counted, DynamicRange, FilterPart, ItemFilter, ItemFilterValues, ItemSort,
    LibraryKind, MediaId, Resolution, SortKey, WatchedState,
};

use crate::backend::BackendError;

/// What the fake knows of an item to sort and filter by, beyond what its
/// [`spoolframe_types::Item`] carries: what Plex's listing has of it.
#[derive(Debug, Clone, Default)]
pub(super) struct Facts {
    /// The day it first came out: a show's first airing.
    pub released: Option<CalendarDate>,
    /// Unix seconds.
    pub added_at: Option<i64>,
    /// Tenths of the 0 to 10 ratings.
    pub rating: Option<u16>,
    pub audience_rating: Option<u16>,
    /// A movie's or an artist's; a show's are on its item.
    pub genres: Vec<String>,
    pub resolution: Option<Resolution>,
    pub dynamic_range: Option<DynamicRange>,
}

/// One top-level item as a sort and a filter read it: its item's own
/// fields, its [`Facts`], and the session user's watch state of it.
#[derive(Debug, Clone)]
pub(super) struct Browsed {
    pub media: MediaId,
    /// Its id as text, the last tiebreaker.
    pub id: String,
    /// Its sort title, case aside.
    pub sort_title: String,
    pub year: Option<u16>,
    pub content_rating: Option<String>,
    pub genres: Vec<String>,
    pub duration_ms: Option<u64>,
    pub watched: Option<WatchedState>,
    /// Unix seconds.
    pub last_played: Option<i64>,
    pub facts: Facts,
}

impl Browsed {
    pub fn new(media: MediaId, title: &str, facts: Facts) -> Self {
        Self {
            id: media.to_string(),
            media,
            sort_title: sort_title(title).to_lowercase(),
            year: None,
            content_rating: None,
            genres: facts.genres.clone(),
            duration_ms: None,
            watched: None,
            last_played: None,
            facts,
        }
    }
}

/// What a library of `kind` sorts and filters by, with no values yet.
fn offers(kind: LibraryKind) -> ItemFilterValues {
    use FilterPart::*;
    use SortKey::*;
    let (sorts, filters, ranges) = match kind {
        LibraryKind::Movies => (
            vec![
                Title,
                Released,
                Added,
                LastPlayed,
                Rating,
                AudienceRating,
                Duration,
            ],
            vec![
                Watched,
                Genres,
                Years,
                ContentRatings,
                Resolutions,
                DynamicRanges,
            ],
            vec![
                DynamicRange::Sdr,
                DynamicRange::Hdr,
                DynamicRange::DolbyVision,
            ],
        ),
        LibraryKind::Shows => (
            vec![Title, Released, Added, LastPlayed, Rating, AudienceRating],
            vec![Watched, Genres, Years, ContentRatings],
            Vec::new(),
        ),
        LibraryKind::Music => (vec![Title, Added], vec![Genres], Vec::new()),
    };
    ItemFilterValues {
        dynamic_ranges: ranges
            .into_iter()
            .map(|value| Counted { value, count: None })
            .collect(),
        sorts,
        filters,
        ..ItemFilterValues::default()
    }
}

/// `sort` and `filter` checked as the server checks them: a value this
/// build does not know, or a key, part or range the library does not
/// offer, is `InvalidInput`.
pub(super) fn check(
    kind: LibraryKind,
    sort: Option<&ItemSort>,
    filter: Option<&ItemFilter>,
) -> Result<(), BackendError> {
    let invalid = |why: String| BackendError::InvalidInput(why);
    if let Some(sort) = sort {
        sort.check().map_err(|e| invalid(e.to_string()))?;
    }
    if let Some(filter) = filter {
        filter.check().map_err(|e| invalid(e.to_string()))?;
    }
    match offers(kind).refuses(sort, filter) {
        Some(why) => Err(invalid(why)),
        None => Ok(()),
    }
}

/// Whether `filter` keeps `item`.
pub(super) fn keeps(filter: &ItemFilter, item: &Browsed) -> bool {
    let any_text = |asked: &[String], has: &[&str]| {
        asked.is_empty()
            || asked
                .iter()
                .any(|a| has.iter().any(|h| a.eq_ignore_ascii_case(h)))
    };
    let genres: Vec<&str> = item.genres.iter().map(String::as_str).collect();
    let ratings: Vec<&str> = item.content_rating.as_deref().into_iter().collect();
    let watched = filter.watched.is_empty()
        || item
            .watched
            .is_some_and(|state| filter.watched.contains(&state));
    let years = match (filter.year_from, filter.year_to) {
        (None, None) => true,
        (from, to) => item.year.is_some_and(|year| {
            from.is_none_or(|from| year >= from) && to.is_none_or(|to| year <= to)
        }),
    };
    let resolution = filter.resolutions.is_empty()
        || item
            .facts
            .resolution
            .is_some_and(|r| filter.resolutions.contains(&r));
    let range = filter.dynamic_ranges.is_empty()
        || item.facts.dynamic_range.is_some_and(|has| {
            filter
                .dynamic_ranges
                .iter()
                .any(|&asked| matches(asked, has))
        });
    watched
        && any_text(&filter.genres, &genres)
        && years
        && any_text(&filter.content_ratings, &ratings)
        && resolution
        && range
}

/// Whether an item of range `has` is kept by a filter for `asked`: HDR
/// keeps Dolby Vision too.
fn matches(asked: DynamicRange, has: DynamicRange) -> bool {
    asked == has || (asked == DynamicRange::Hdr && has == DynamicRange::DolbyVision)
}

/// `item`'s value for `key`, as a number to compare; `None` without one.
fn value(key: SortKey, item: &Browsed) -> Option<i64> {
    let day = |d: CalendarDate| {
        i64::from(d.year()) * 10_000 + i64::from(d.month()) * 100 + i64::from(d.day())
    };
    match key {
        SortKey::Released => item.facts.released.map(day),
        SortKey::Added => item.facts.added_at,
        SortKey::LastPlayed => item.last_played,
        SortKey::Rating => item.facts.rating.map(i64::from),
        SortKey::AudienceRating => item.facts.audience_rating.map(i64::from),
        SortKey::Duration => item.duration_ms.and_then(|ms| i64::try_from(ms).ok()),
        SortKey::Title | SortKey::Size | SortKey::Unknown => None,
    }
}

/// Orders `items` by `sort`: its key, missing values last both ways, then
/// sort title A to Z, then id.
pub(super) fn order(sort: ItemSort, items: &mut [Browsed]) {
    let direct = |o: Ordering| if sort.descending { o.reverse() } else { o };
    items.sort_by(|a, b| {
        let by_key = match sort.key {
            SortKey::Title => direct(a.sort_title.cmp(&b.sort_title)),
            key => match (value(key, a), value(key, b)) {
                (Some(x), Some(y)) => direct(x.cmp(&y)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        };
        by_key
            .then_with(|| a.sort_title.cmp(&b.sort_title))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// What a library of `kind` with `items` has to filter by, each value
/// counted: only the parts it offers, each list in the protocol's order.
pub(super) fn values(kind: LibraryKind, items: &[Browsed]) -> ItemFilterValues {
    let mut values = offers(kind);
    let filters = values.filters.clone();
    let offered = |part| filters.contains(&part);
    if offered(FilterPart::Genres) {
        values.genres = by_name(items.iter().flat_map(|i| i.genres.iter()));
    }
    if offered(FilterPart::ContentRatings) {
        values.content_ratings = by_name(items.iter().filter_map(|i| i.content_rating.as_ref()));
    }
    if offered(FilterPart::Years) {
        values.years = counted(items.iter().filter_map(|i| i.year));
        values.years.reverse();
    }
    if offered(FilterPart::Resolutions) {
        // Resolution orders largest first.
        values.resolutions = counted(items.iter().filter_map(|i| i.facts.resolution));
    }
    for range in &mut values.dynamic_ranges {
        let count = items
            .iter()
            .filter(|i| {
                i.facts
                    .dynamic_range
                    .is_some_and(|has| matches(range.value, has))
            })
            .count();
        range.count = Some(count as u32);
    }
    values
}

/// Each value and how many times it comes, in its order.
fn counted<T: Ord>(values: impl Iterator<Item = T>) -> Vec<Counted<T>> {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(value, count)| Counted {
            value,
            count: Some(count),
        })
        .collect()
}

/// Names counted case aside, by name, each as its first item spells it.
fn by_name<'a>(names: impl Iterator<Item = &'a String>) -> Vec<Counted<String>> {
    let mut counts: BTreeMap<String, (String, u32)> = BTreeMap::new();
    for name in names {
        counts
            .entry(name.to_lowercase())
            .or_insert_with(|| (name.clone(), 0))
            .1 += 1;
    }
    counts
        .into_values()
        .map(|(value, count)| Counted {
            value,
            count: Some(count),
        })
        .collect()
}
