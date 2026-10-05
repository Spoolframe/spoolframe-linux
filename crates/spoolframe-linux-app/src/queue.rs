//! What plays next (P1-23), in two layers as Spotify has them:
//!
//! - **The context**: the album or list playback was started from, with
//!   shuffle and repeat.
//! - **The queue**: items added by hand ("Play next", "Add to queue"). They
//!   play after the current item and before the context goes on, in their
//!   own order, once; shuffle and repeat never touch them.
//!
//! Previous goes back through what actually played, queued items included.
//! No GStreamer here: [`crate::playback::Playback`] asks this what to play.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use spoolframe_types::{Album, Item};

/// What happens at the end of the context, or of the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    Off,
    /// The context again from the top.
    All,
    /// The same item again.
    One,
}

/// A hand-added item, with the album it is from for its cover and name.
/// The id stays with it as the queue is reordered, so a view can name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub id: u64,
    pub item: Item,
    pub album: Option<Album>,
}

/// What plays: the context's item at the cursor, or one taken from the
/// queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Now {
    Context,
    Queued(Box<Queued>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    /// The context's items, in their own (album) order.
    context: Vec<Item>,
    /// The album the context is, if it is one.
    album: Option<Album>,
    /// The context's play order, as positions in `context`.
    order: Vec<usize>,
    /// Where the context is in `order`: the context item playing, or the
    /// last one played while a queued item plays. `None` before any.
    at: Option<usize>,
    queued: VecDeque<Queued>,
    now: Option<Now>,
    shuffle: bool,
    repeat: Repeat,
    next_id: u64,
    /// What played before, with the context's cursor at the time, for
    /// Previous. Not saved: a restored queue starts its history afresh.
    #[serde(skip)]
    history: Vec<(Now, Option<usize>)>,
}

impl Queue {
    /// Plays `context` (of `album`) from `start`, with `queued` waiting,
    /// as carried over from the playback before. An empty context plays
    /// the queue.
    pub fn new(
        context: Vec<Item>,
        start: usize,
        album: Option<Album>,
        queued: Vec<Queued>,
    ) -> Self {
        let mut queue = Self {
            context: Vec::new(),
            album: None,
            order: Vec::new(),
            at: None,
            next_id: queued.iter().map(|q| q.id).max().unwrap_or(0),
            queued: queued.into(),
            now: None,
            shuffle: false,
            repeat: Repeat::Off,
            history: Vec::new(),
        };
        queue.set_context(context, start, album);
        queue
    }

    /// Replaces the context, playing it from `start`, and keeps the queue,
    /// as Spotify does. History of the old context is let go.
    pub fn set_context(&mut self, context: Vec<Item>, start: usize, album: Option<Album>) {
        self.order = (0..context.len()).collect();
        self.context = context;
        self.album = album;
        self.history.clear();
        if start < self.context.len() {
            self.at = Some(start);
            self.now = Some(Now::Context);
        } else {
            self.at = None;
            self.now = None;
            self.take_queued();
        }
    }

    /// Adds `items` to the end of the context: what follows it, such as
    /// a show's next episode when the episode was started on its own.
    pub fn extend_context(&mut self, items: Vec<Item>) {
        for item in items {
            self.order.push(self.context.len());
            self.context.push(item);
        }
    }

    pub fn current(&self) -> Option<&Item> {
        match self.now.as_ref()? {
            Now::Context => self.context.get(*self.order.get(self.at?)?),
            Now::Queued(queued) => Some(&queued.item),
        }
    }

    /// The album of what plays: the context's, or the queued item's own.
    pub fn current_album(&self) -> Option<&Album> {
        match self.now.as_ref()? {
            Now::Context => self.album.as_ref(),
            Now::Queued(queued) => queued.album.as_ref(),
        }
    }

    /// The context's album, for "Next from: <album>".
    pub fn album(&self) -> Option<&Album> {
        self.album.as_ref()
    }

    /// Whether what plays was taken from the queue.
    pub fn current_is_queued(&self) -> bool {
        matches!(self.now, Some(Now::Queued(_)))
    }

    /// The hand-added items still to play, in order.
    pub fn queued(&self) -> impl Iterator<Item = &Queued> {
        self.queued.iter()
    }

    /// The context's items still to play, with their positions in the
    /// play order (for [`Queue::play_context_at`]). Repeat does not wrap
    /// the listing round.
    pub fn context_ahead(&self) -> Vec<(usize, &Item)> {
        let from = self.at.map_or(0, |at| at + 1);
        (from..self.order.len())
            .map(|pos| (pos, &self.context[self.order[pos]]))
            .collect()
    }

    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    /// "Play next": `items` (of `album`) to the front of the queue, in
    /// their order.
    pub fn play_next(&mut self, items: Vec<Item>, album: Option<Album>) {
        for item in items.into_iter().rev() {
            let queued = self.entry(item, album.clone());
            self.queued.push_front(queued);
        }
        if self.now.is_none() {
            self.take_queued();
        }
    }

    /// "Add to queue": `items` (of `album`) to the end of the queue.
    pub fn add(&mut self, items: Vec<Item>, album: Option<Album>) {
        for item in items {
            let queued = self.entry(item, album.clone());
            self.queued.push_back(queued);
        }
        if self.now.is_none() {
            self.take_queued();
        }
    }

    fn entry(&mut self, item: Item, album: Option<Album>) -> Queued {
        self.next_id += 1;
        Queued {
            id: self.next_id,
            item,
            album,
        }
    }

    /// Takes the queued item `id` out of the queue; false if it is not
    /// there.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.queued.len();
        self.queued.retain(|q| q.id != id);
        self.queued.len() != before
    }

    /// Moves the queued item `id` to `index` within the queue.
    pub fn move_to(&mut self, id: u64, index: usize) {
        let Some(from) = self.queued.iter().position(|q| q.id == id) else {
            return;
        };
        if let Some(queued) = self.queued.remove(from) {
            let index = index.min(self.queued.len());
            self.queued.insert(index, queued);
        }
    }

    pub fn clear(&mut self) {
        self.queued.clear();
    }

    /// The hand-added items, to carry into the next playback.
    pub fn carry(&self) -> Vec<Queued> {
        self.queued.iter().cloned().collect()
    }

    pub fn has_next(&self) -> bool {
        !self.queued.is_empty() || self.context_next().is_some()
    }

    /// Whether the context goes on after what plays now.
    pub fn context_has_next(&self) -> bool {
        self.context_next().is_some()
    }

    /// The first queued item if there is one, else the context's next.
    /// False, with nothing changed, at the end of both.
    pub fn next(&mut self) -> bool {
        if !self.queued.is_empty() {
            self.remember();
            self.take_queued();
            true
        } else {
            self.next_in_context()
        }
    }

    /// The context's next item, passing over the queue (video, which does
    /// not play the queue).
    pub fn next_in_context(&mut self) -> bool {
        let Some(next) = self.context_next() else {
            return false;
        };
        self.remember();
        self.at = Some(next);
        self.now = Some(Now::Context);
        true
    }

    fn context_next(&self) -> Option<usize> {
        let next = self.at.map_or(0, |at| at + 1);
        if next < self.order.len() {
            Some(next)
        } else if self.repeat == Repeat::All && !self.order.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    fn remember(&mut self) {
        if let Some(now) = self.now.take() {
            self.history.push((now, self.at));
        }
    }

    fn take_queued(&mut self) {
        if let Some(queued) = self.queued.pop_front() {
            self.now = Some(Now::Queued(Box::new(queued)));
        }
    }

    pub fn has_previous(&self) -> bool {
        !self.history.is_empty()
    }

    /// Back to what played before; false if nothing did. A queued item
    /// stepped back from returns to the front of the queue, so Next plays
    /// it again.
    pub fn previous(&mut self) -> bool {
        let Some((now, at)) = self.history.pop() else {
            return false;
        };
        if let Some(Now::Queued(queued)) = self.now.take() {
            self.queued.push_front(*queued);
        }
        self.now = Some(now);
        self.at = at;
        true
    }

    /// Plays the queued item `id`; it leaves the queue. False if it is not
    /// there.
    pub fn play_queued(&mut self, id: u64) -> bool {
        let Some(from) = self.queued.iter().position(|q| q.id == id) else {
            return false;
        };
        let Some(queued) = self.queued.remove(from) else {
            return false;
        };
        self.remember();
        self.now = Some(Now::Queued(Box::new(queued)));
        true
    }

    /// Jumps the context to `pos` in its play order, keeping the queue.
    pub fn play_context_at(&mut self, pos: usize) -> bool {
        if pos >= self.order.len() {
            return false;
        }
        self.remember();
        self.at = Some(pos);
        self.now = Some(Now::Context);
        true
    }

    /// Shuffles the context still to come, or puts it back in order,
    /// keeping its current item where it is. `random(n)` is a number below
    /// `n`. The queue is never shuffled.
    pub fn set_shuffle(&mut self, on: bool, random: impl FnMut(usize) -> usize) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        let current = self.at.and_then(|at| self.order.get(at).copied());
        if on {
            let keep = self.at.map_or(0, |at| at + 1);
            self.order = shuffled_from(&self.order, keep, random);
        } else {
            self.order = (0..self.context.len()).collect();
            self.at = current;
        }
        // History holds cursors into the old order.
        self.history.clear();
    }

    /// Off, then the whole context, then the one item, then off again.
    pub fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        };
    }
}

/// `order` with everything from `keep` on shuffled (Fisher–Yates);
/// `random(n)` is a number below `n`. What has played and what plays stay
/// where they are.
fn shuffled_from(
    order: &[usize],
    keep: usize,
    mut random: impl FnMut(usize) -> usize,
) -> Vec<usize> {
    let mut order = order.to_vec();
    let start = keep.min(order.len());
    let rest = &mut order[start..];
    for i in (1..rest.len()).rev() {
        rest.swap(i, random(i + 1));
    }
    order
}

#[cfg(test)]
mod tests {
    use spoolframe_types::{MediaId, MediaKind, Namespace, PlaybackState, Track};

    use super::*;

    fn id(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).unwrap()
    }

    fn song(name: &str) -> Item {
        Item::Track(Track {
            id: id(MediaKind::Track, name),
            album_id: id(MediaKind::Album, "1"),
            artist_id: id(MediaKind::Artist, "1"),
            title: name.into(),
            track: None,
            disc: None,
            duration: None,
            playback: PlaybackState::default(),
            codec: None,
            bit_depth: None,
            sample_rate: None,
            album_track_count: None,
            artist_name: None,
            album_title: None,
            artwork: Vec::new(),
        })
    }

    fn songs(names: &str) -> Vec<Item> {
        names.split(' ').map(song).collect()
    }

    fn album(title: &str) -> Album {
        Album {
            id: id(MediaKind::Album, title),
            artist_id: id(MediaKind::Artist, "1"),
            artist_name: None,
            title: title.into(),
            year: None,
            artwork: Vec::new(),
            track_count: None,
        }
    }

    /// `names` playing as a context from its first item, nothing queued.
    fn context(names: &str) -> Queue {
        Queue::new(songs(names), 0, None, Vec::new())
    }

    fn title(item: Option<&Item>) -> String {
        match item {
            Some(Item::Track(t)) => t.title.clone(),
            _ => "-".into(),
        }
    }

    /// The titles played by pressing Next until the end, the current one
    /// first.
    fn played(queue: &mut Queue) -> String {
        let mut out = vec![title(queue.current())];
        while queue.next() {
            out.push(title(queue.current()));
            assert!(out.len() < 50, "next never ends");
        }
        out.join(" ")
    }

    /// A shuffle that always swaps with the first: deterministic, and
    /// not the identity.
    fn first(_: usize) -> usize {
        0
    }

    #[test]
    fn the_context_plays_in_order() {
        assert_eq!(played(&mut context("a b c")), "a b c");
    }

    #[test]
    fn play_next_plays_after_the_current_item_then_the_context_goes_on() {
        let mut queue = context("a b c");
        queue.play_next(songs("x"), None);
        assert_eq!(played(&mut queue), "a x b c");
    }

    #[test]
    fn play_next_goes_before_what_was_added_to_the_queue() {
        let mut queue = context("a b");
        queue.add(songs("x"), None);
        queue.play_next(songs("y z"), None);
        assert_eq!(played(&mut queue), "a y z x b");
    }

    #[test]
    fn add_to_queue_goes_to_the_end_of_the_queue() {
        let mut queue = context("a b");
        queue.add(songs("x"), None);
        queue.add(songs("y"), None);
        assert_eq!(played(&mut queue), "a x y b");
    }

    #[test]
    fn previous_goes_back_through_what_played_queued_items_included() {
        let mut queue = context("a b c");
        queue.add(songs("x"), None);
        queue.next(); // x
        queue.next(); // b
        queue.previous();
        assert_eq!(title(queue.current()), "x");
    }

    #[test]
    fn previous_then_next_returns_to_the_context_where_it_was() {
        let mut queue = context("a b c");
        queue.add(songs("x"), None);
        queue.next(); // x
        queue.next(); // b
        queue.previous(); // x
        queue.previous(); // a
        assert_eq!(played(&mut queue), "a x b c");
    }

    #[test]
    fn previous_at_the_start_does_nothing() {
        let mut queue = Queue::new(songs("a b"), 1, None, Vec::new());
        assert!(!queue.previous());
    }

    #[test]
    fn shuffle_never_touches_queued_items() {
        let mut queue = context("a b c d");
        queue.add(songs("x y"), None);
        queue.set_shuffle(true, first);
        let rest = played(&mut queue);
        assert!(rest.starts_with("a x y "), "{rest}");
    }

    #[test]
    fn shuffle_keeps_the_current_item_and_shuffles_the_rest_of_the_context() {
        let mut queue = context("a b c d");
        queue.set_shuffle(true, first);
        assert_eq!(played(&mut queue), "a c d b");
    }

    #[test]
    fn shuffle_off_puts_the_context_back_in_order_from_the_current_item() {
        let mut queue = context("a b c d");
        queue.set_shuffle(true, first);
        queue.next(); // c
        queue.set_shuffle(false, first);
        assert_eq!(played(&mut queue), "c d");
    }

    #[test]
    fn repeat_all_wraps_the_context_but_queued_items_play_once() {
        let mut queue = context("a b");
        queue.add(songs("x"), None);
        queue.cycle_repeat();
        let order: Vec<_> = (0..5)
            .map(|_| {
                queue.next();
                title(queue.current())
            })
            .collect();
        assert_eq!(order.join(" "), "x b a b a");
    }

    #[test]
    fn a_shuffled_context_then_a_queued_album_in_order() {
        // The ticket's case: shuffle on for the first album, the second
        // added to the queue. Queued items come first, in order; then the
        // first album's rest, shuffled.
        let mut queue = context("a b c d");
        queue.set_shuffle(true, first);
        queue.add(songs("p q r"), Some(album("second")));
        assert_eq!(played(&mut queue), "a p q r c d b");
    }

    #[test]
    fn a_new_context_keeps_the_queue() {
        let mut queue = context("a b");
        queue.add(songs("x"), None);
        queue.set_context(songs("p q"), 0, None);
        assert_eq!(played(&mut queue), "p x q");
    }

    #[test]
    fn a_carried_queue_plays_after_the_new_contexts_first_item() {
        let mut old = context("a");
        old.add(songs("x y"), None);
        let mut queue = Queue::new(songs("p q"), 0, None, old.carry());
        assert_eq!(played(&mut queue), "p x y q");
    }

    #[test]
    fn items_added_after_a_carry_get_new_ids() {
        let mut old = context("a");
        old.add(songs("x"), None);
        let mut queue = Queue::new(songs("p"), 0, None, old.carry());
        queue.add(songs("y"), None);
        let ids: Vec<u64> = queue.queued().map(|q| q.id).collect();
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn an_empty_context_plays_the_queue() {
        let mut queue = Queue::new(Vec::new(), 0, None, Vec::new());
        queue.add(songs("x y"), None);
        assert_eq!(played(&mut queue), "x y");
    }

    #[test]
    fn an_extended_context_goes_on_into_what_was_added() {
        let mut queue = context("a");
        queue.extend_context(songs("b"));
        assert_eq!(played(&mut queue), "a b");
    }

    #[test]
    fn next_in_context_passes_over_the_queue() {
        let mut queue = context("a b");
        queue.add(songs("x"), None);
        queue.next_in_context();
        assert_eq!(title(queue.current()), "b");
    }

    #[test]
    fn a_queued_item_brings_its_own_album() {
        let mut queue = Queue::new(songs("a"), 0, Some(album("first")), Vec::new());
        queue.add(songs("x"), Some(album("second")));
        queue.next();
        assert_eq!(
            queue.current_album().map(|a| a.title.as_str()),
            Some("second")
        );
    }

    #[test]
    fn a_queued_item_can_be_moved() {
        let mut queue = context("a");
        queue.add(songs("x y z"), None);
        let z = queue.queued().last().unwrap().id;
        queue.move_to(z, 0);
        assert_eq!(played(&mut queue), "a z x y");
    }

    #[test]
    fn a_queued_item_can_be_removed() {
        let mut queue = context("a");
        queue.add(songs("x y"), None);
        let x = queue.queued().next().unwrap().id;
        queue.remove(x);
        assert_eq!(played(&mut queue), "a y");
    }

    #[test]
    fn clear_empties_the_queue_and_keeps_the_context() {
        let mut queue = context("a b");
        queue.add(songs("x y"), None);
        queue.clear();
        assert_eq!(played(&mut queue), "a b");
    }

    #[test]
    fn playing_a_queued_item_takes_it_out_of_the_queue() {
        let mut queue = context("a b");
        queue.add(songs("x y"), None);
        let y = queue.queued().last().unwrap().id;
        queue.play_queued(y);
        assert_eq!(played(&mut queue), "y x b");
    }

    #[test]
    fn playing_a_context_item_jumps_there_and_keeps_the_queue() {
        let mut queue = context("a b c d");
        queue.add(songs("x"), None);
        queue.play_context_at(2);
        assert_eq!(played(&mut queue), "c x d");
    }

    #[test]
    fn the_context_ahead_lists_what_the_context_plays_next() {
        let mut queue = context("a b c");
        queue.add(songs("x"), None);
        queue.next(); // x: the context is still at a
        let ahead: Vec<_> = queue
            .context_ahead()
            .into_iter()
            .map(|(pos, item)| format!("{pos}:{}", title(Some(item))))
            .collect();
        assert_eq!(ahead.join(" "), "1:b 2:c");
    }

    #[test]
    fn a_saved_queue_comes_back_the_same() {
        let mut queue = Queue::new(songs("a b c"), 1, None, Vec::new());
        queue.add(songs("x"), None);
        queue.set_shuffle(true, first);
        let saved = serde_json::to_string(&queue).unwrap();
        let mut restored: Queue = serde_json::from_str(&saved).unwrap();
        assert_eq!(played(&mut restored), played(&mut queue));
    }
}
