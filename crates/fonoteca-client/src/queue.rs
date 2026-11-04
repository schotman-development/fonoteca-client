//! The songs queued to play, in the order they play, and while shuffled the
//! order they were queued in. Each change says where every song went, so
//! the player carries on with what it has.
//!
//! The movements of a work queued whole are one unit: they stay together and
//! in order through shuffling, playing next and dragging, and the sheet lists
//! them as one row that opens to its movements.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use opensubsonic::Child;

use crate::browse;

/// Where each song of the queue before a change sits after it, if it
/// stayed.
pub type Moved = Vec<Option<usize>>;

#[derive(Default)]
pub struct Queue {
    entries: Vec<Entry>,
    /// The entries in the order they were queued in, while shuffled.
    unshuffled: Option<Vec<u64>>,
    /// The works whose rows the sheet lists open.
    expanded: HashSet<u64>,
    next_key: u64,
}

/// A song in the queue; the same song queued twice is two entries.
struct Entry {
    key: u64,
    /// Shared by the movements of a work queued whole.
    work: Option<u64>,
    song: Child,
}

/// Songs that play and move together: a work's movements, or one song.
pub struct Unit {
    pub songs: Range<usize>,
    /// The work's key, when the unit holds more than one movement.
    pub work: Option<u64>,
}

/// How a row of the sheet's list stands for the songs it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Song,
    Work {
        key: u64,
        expanded: bool,
    },
    /// One movement under its work's open row.
    Movement,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub songs: Range<usize>,
    pub shape: Shape,
}

impl Queue {
    /// The queue of `songs` queued before, as the server keeps them: a list
    /// of songs that has lost which of them were works, so each whole work
    /// is a unit again, as a page makes it.
    pub fn new(songs: Vec<Child>) -> Self {
        Queue::from_page(songs, 0)
    }

    /// The queue of a page's `songs`, played from the `start`th.
    pub fn from_page(songs: Vec<Child>, start: usize) -> Self {
        let mut queue = Queue::default();
        queue.entries = queue.page_entries(songs, start);
        queue
    }

    fn fresh_key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    fn entry(&mut self, song: Child, work: Option<u64>) -> Entry {
        Entry {
            key: self.fresh_key(),
            work,
            song,
        }
    }

    /// Entries for songs queued together. Where a page holds all of a work's
    /// movements in a row, they are a unit; a play from the middle of a work
    /// (`start` past its first movement) leaves its movements as songs.
    fn page_entries(&mut self, songs: Vec<Child>, start: usize) -> Vec<Entry> {
        let lengths: Vec<usize> = browse::whole_works(&songs)
            .iter()
            .map(|run| run.len())
            .collect();
        let mut songs = songs.into_iter();
        let mut first = 0;
        let mut entries = Vec::new();
        for length in lengths {
            let started_inside = first < start && start < first + length;
            let work = (length > 1 && !started_inside).then(|| self.fresh_key());
            entries.extend(
                songs
                    .by_ref()
                    .take(length)
                    .map(|song| self.entry(song, work)),
            );
            first += length;
        }
        entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Child> {
        self.entries.get(index).map(|entry| &entry.song)
    }

    pub fn songs(&self) -> impl Iterator<Item = &Child> {
        self.entries.iter().map(|entry| &entry.song)
    }

    pub fn shuffled(&self) -> bool {
        self.unshuffled.is_some()
    }

    /// The unit holding the `at`th song.
    pub fn unit_at(&self, at: usize) -> Unit {
        let work = self.entries.get(at).and_then(|entry| entry.work);
        let joined = |other: usize| work.is_some() && self.entries[other].work == work;
        let mut start = at;
        while start > 0 && joined(start - 1) {
            start -= 1;
        }
        let mut end = at + 1;
        while end < self.entries.len() && joined(end) {
            end += 1;
        }
        Unit {
            songs: start..end,
            work: work.filter(|_| end - start > 1),
        }
    }

    /// The rows the sheet lists from the unit holding the `shown`th song, or
    /// the first, on, until `limit` rows are listed: each work is one row,
    /// followed by its movements while it is open.
    pub fn rows(&self, shown: Option<usize>, limit: usize) -> Vec<Row> {
        let first = shown.filter(|&shown| shown < self.len());
        let mut at = first.map_or(0, |shown| self.unit_at(shown).songs.start);
        let mut rows = Vec::new();
        while at < self.len() && rows.len() < limit {
            let unit = self.unit_at(at);
            at = unit.songs.end;
            let Some(key) = unit.work else {
                rows.push(Row {
                    songs: unit.songs,
                    shape: Shape::Song,
                });
                continue;
            };
            let expanded = self.expanded.contains(&key);
            rows.push(Row {
                songs: unit.songs.clone(),
                shape: Shape::Work { key, expanded },
            });
            if expanded {
                rows.extend(unit.songs.map(|movement| Row {
                    songs: movement..movement + 1,
                    shape: Shape::Movement,
                }));
            }
        }
        rows
    }

    /// The rows the sheet lists above the unit holding the `shown`th song:
    /// the units before it in the queue's order (the shuffled one, while
    /// shuffled), each one row with a work closed. At most `limit` of them,
    /// the nearest, and how many songs come before those.
    pub fn previous_rows(&self, shown: Option<usize>, limit: usize) -> (Vec<Row>, usize) {
        let mut before = shown
            .filter(|&shown| shown < self.len())
            .map_or(0, |shown| self.unit_at(shown).songs.start);
        let mut rows = Vec::new();
        while before > 0 && rows.len() < limit {
            let unit = self.unit_at(before - 1);
            before = unit.songs.start;
            let shape = unit.work.map_or(Shape::Song, |key| Shape::Work {
                key,
                expanded: false,
            });
            rows.push(Row {
                songs: unit.songs,
                shape,
            });
        }
        rows.reverse();
        (rows, before)
    }

    /// Opens the row of the work `key`, or closes it.
    pub fn toggle_work(&mut self, key: u64) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
    }

    /// The works whose rows are open, by the ids of their first songs, which
    /// outlast the keys.
    pub fn expanded_works(&self) -> Vec<String> {
        self.work_starts()
            .filter(|(_, key)| self.expanded.contains(key))
            .map(|(song, _)| song.id.clone())
            .collect()
    }

    /// Opens the rows of the works whose first songs are `first_songs`.
    pub fn expand_works(&mut self, first_songs: &[String]) {
        let keys: Vec<u64> = self
            .work_starts()
            .filter(|(song, _)| first_songs.contains(&song.id))
            .map(|(_, key)| key)
            .collect();
        self.expanded.extend(keys);
    }

    /// The first song of each work queued whole, and the work's key.
    fn work_starts(&self) -> impl Iterator<Item = (&Child, u64)> {
        self.entries.iter().enumerate().filter_map(|(at, entry)| {
            let key = entry.work?;
            let continues_its_work = at > 0 && self.entries[at - 1].work == Some(key);
            (!continues_its_work).then_some((&entry.song, key))
        })
    }

    /// Puts `songs` in before the `at`th song, or at the end; shuffled by
    /// `shuffle_seed`, if it is given. A work is never split by them: they
    /// go in after the unit `at` falls in.
    pub fn insert(&mut self, at: usize, songs: Vec<Child>, shuffle_seed: Option<u64>) -> Moved {
        self.change(|queue| {
            let mut added = queue.page_entries(songs, 0);
            if let Some(seed) = shuffle_seed {
                shuffle_units(&mut added, None, seed);
            }
            let at = queue.outside_works(at.min(queue.len()));
            let keys: Vec<u64> = added.iter().map(|entry| entry.key).collect();
            // Shuffled, songs played next come after the one before them in
            // the order they were queued in too; the rest at its end.
            let before = at.checked_sub(1).map(|before| queue.entries[before].key);
            if let Some(order) = &mut queue.unshuffled {
                let place = before
                    .filter(|_| at < queue.entries.len())
                    .and_then(|before| order.iter().position(|&key| key == before))
                    .map_or(order.len(), |place| place + 1);
                order.splice(place..place, keys);
            }
            queue.entries.splice(at..at, added);
        })
    }

    /// Records whether the song `id` is loved, wherever it is queued.
    pub fn set_loved(&mut self, id: &str, loved: bool) {
        for entry in self.entries.iter_mut().filter(|entry| entry.song.id == id) {
            entry.song.starred = loved.then(String::new);
        }
    }

    /// Takes out the songs in `songs`.
    pub fn remove(&mut self, songs: Range<usize>) -> Moved {
        self.change(|queue| {
            let removed: HashSet<u64> = queue
                .take_entries(songs)
                .into_iter()
                .map(|entry| entry.key)
                .collect();
            if let Some(order) = &mut queue.unshuffled {
                order.retain(|key| !removed.contains(key));
            }
        })
    }

    /// Moves the songs in `from` to start at the `to`th place, or the end of
    /// the work that place falls in.
    pub fn move_to(&mut self, from: Range<usize>, to: usize) -> Moved {
        self.change(|queue| {
            let moved = queue.take_entries(from);
            let at = queue.outside_works(to.min(queue.len()));
            queue.entries.splice(at..at, moved);
        })
    }

    /// Moves the songs in `songs`, played before the `current`th or queued
    /// after it, to play right after it, or after the work it is in.
    pub fn move_after(&mut self, songs: Range<usize>, current: usize) -> Moved {
        // Songs from before it leave a gap behind them.
        let gap = if songs.start < current {
            songs.len()
        } else {
            0
        };
        self.move_to(songs, current + 1 - gap)
    }

    /// Moves the songs in `from` onto the ones in `onto`: after them when
    /// they come from before, else before them.
    pub fn move_onto(&mut self, from: Range<usize>, onto: Range<usize>) -> Moved {
        let to = if onto.start > from.start {
            onto.end - from.len()
        } else {
            onto.start
        };
        self.move_to(from, to)
    }

    fn take_entries(&mut self, songs: Range<usize>) -> Vec<Entry> {
        let end = songs.end.min(self.len());
        let start = songs.start.min(end);
        self.entries.drain(start..end).collect()
    }

    /// `at`, or the end of the work it falls inside of.
    fn outside_works(&self, at: usize) -> usize {
        match self.entries.get(at) {
            Some(entry)
                if at > 0 && entry.work.is_some() && entry.work == self.entries[at - 1].work =>
            {
                self.unit_at(at).songs.end
            }
            _ => at,
        }
    }

    /// Takes out every song but the `current`th.
    pub fn keep_only(&mut self, current: usize) -> Moved {
        self.change(|queue| {
            if current < queue.entries.len() {
                let kept = queue.entries.swap_remove(current);
                queue.entries = vec![kept];
            }
            if let Some(order) = &mut queue.unshuffled {
                *order = queue.entries.iter().map(|entry| entry.key).collect();
            }
        })
    }

    /// Puts the unit of the `current`th song first and the others after it
    /// in an order `seed` picks, keeping the order they were queued in.
    pub fn shuffle(&mut self, current: Option<usize>, seed: u64) -> Moved {
        self.change(|queue| {
            if queue.unshuffled.is_none() {
                queue.unshuffled = Some(queue.entries.iter().map(|entry| entry.key).collect());
            }
            shuffle_units(&mut queue.entries, current, seed);
        })
    }

    /// Puts the songs back in the order they were queued in.
    pub fn unshuffle(&mut self) -> Moved {
        self.change(|queue| {
            let Some(order) = queue.unshuffled.take() else {
                return;
            };
            let place: HashMap<u64, usize> = order
                .iter()
                .enumerate()
                .map(|(at, &key)| (key, at))
                .collect();
            queue
                .entries
                .sort_by_key(|entry| place.get(&entry.key).copied());
        })
    }

    /// Makes `change` and says where each song went.
    fn change(&mut self, change: impl FnOnce(&mut Self)) -> Moved {
        let before: Vec<u64> = self.entries.iter().map(|entry| entry.key).collect();
        change(self);
        let after: HashMap<u64, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(at, entry)| (entry.key, at))
            .collect();
        before.iter().map(|key| after.get(key).copied()).collect()
    }
}

/// Puts the unit of the `first`th entry first and the other units after it
/// in an order `seed` picks, each in its own order.
fn shuffle_units(entries: &mut Vec<Entry>, first: Option<usize>, seed: u64) {
    let mut units: Vec<Vec<Entry>> = Vec::new();
    for entry in std::mem::take(entries) {
        match units.last_mut() {
            Some(unit) if entry.work.is_some() && unit[0].work == entry.work => unit.push(entry),
            _ => units.push(vec![entry]),
        }
    }
    let first = first
        .and_then(|first| {
            let mut end = 0;
            units.iter().position(|unit| {
                end += unit.len();
                first < end
            })
        })
        .map(|at| units.remove(at));
    let mut random = Random(seed);
    // Fisher and Yates's: each order equally likely.
    for at in (1..units.len()).rev() {
        units.swap(at, random.below(at + 1));
    }
    entries.extend(first.into_iter().chain(units).flatten());
}

/// The `count` songs from the `start`th, as a work's row queues its
/// movements.
pub fn run_from(songs: Vec<Child>, start: usize, count: usize) -> Vec<Child> {
    songs.into_iter().skip(start).take(count).collect()
}

/// What to do with the songs of a page for one of its menu's words: "play",
/// "next", "queue", "shuffle-now", "shuffle-next" or "shuffle-last".
pub enum Plan {
    /// Play them in their order, in place of what played.
    Play(Vec<Child>),
    /// Play them in a random order, the queue's shuffle on.
    PlayShuffled(Vec<Child>),
    /// Put them in after the song playing, or at the end; in the order a
    /// seed picks, if it is given.
    Enqueue {
        songs: Vec<Child>,
        next: bool,
        shuffle_seed: Option<u64>,
    },
}

/// An empty queue has no order to put shuffled songs into: they play as
/// "shuffle-now" does, so that turning the shuffle off restores their order.
pub fn plan(how: &str, songs: Vec<Child>, queue_is_empty: bool, seed: u64) -> Plan {
    match how {
        "play" => Plan::Play(songs),
        "shuffle-now" => Plan::PlayShuffled(songs),
        "shuffle-next" | "shuffle-last" if queue_is_empty => Plan::PlayShuffled(songs),
        "shuffle-next" | "shuffle-last" => Plan::Enqueue {
            songs,
            next: how == "shuffle-next",
            shuffle_seed: Some(seed),
        },
        _ => Plan::Enqueue {
            songs,
            next: how == "next",
            shuffle_seed: None,
        },
    }
}

/// SplitMix64: a well-spread run of numbers from one seed, enough to shuffle
/// songs.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) % bound as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str) -> Child {
        serde_json::from_value(serde_json::json!({ "id": id, "isDir": false, "title": id }))
            .unwrap()
    }

    fn queue(ids: &[&str]) -> Queue {
        Queue::new(ids.iter().map(|id| song(id)).collect())
    }

    fn ids(queue: &Queue) -> Vec<&str> {
        queue.songs().map(|song| song.id.as_str()).collect()
    }

    #[test]
    fn loving_a_song_marks_every_copy_of_it_in_the_queue() {
        let mut q = queue(&["a", "b", "a"]);
        let loved = |q: &Queue| -> Vec<bool> { q.songs().map(|s| s.starred.is_some()).collect() };
        q.set_loved("a", true);
        assert_eq!(loved(&q), [true, false, true]);
        q.set_loved("a", false);
        assert_eq!(loved(&q), [false, false, false]);
    }

    #[test]
    fn the_rows_above_the_one_playing_are_the_units_played_in_their_order() {
        let mut q = queue(&["a", "b", "c", "d", "e"]);
        let rows = |q: &Queue, shown, limit| {
            let (rows, hidden) = q.previous_rows(shown, limit);
            (
                rows.into_iter().map(|row| row.songs).collect::<Vec<_>>(),
                hidden,
            )
        };
        assert_eq!(rows(&q, Some(0), 10), (vec![], 0), "nothing played yet");
        assert_eq!(rows(&q, None, 10), (vec![], 0), "nothing playing");
        assert_eq!(rows(&q, Some(3), 10), (vec![0..1, 1..2, 2..3], 0));
        assert_eq!(rows(&q, Some(3), 2), (vec![1..2, 2..3], 1), "the nearest");
        // A work queued whole is one row, whichever of its movements plays.
        let work = |id: &str| -> Child {
            serde_json::from_value(serde_json::json!({
                "id": id, "isDir": false, "title": id,
                "works": [{ "name": "Suite" }], "movements": [{ "name": id }]
            }))
            .unwrap()
        };
        q.insert(2, vec![work("w1"), work("w2"), work("w3")], None);
        assert_eq!(ids(&q), ["a", "b", "w1", "w2", "w3", "c", "d", "e"]);
        let (before, _) = q.previous_rows(Some(5), 10);
        assert_eq!(before.len(), 3);
        assert_eq!(before[2].songs, 2..5);
        assert!(matches!(
            before[2].shape,
            Shape::Work {
                expanded: false,
                ..
            }
        ));
        assert_eq!(
            rows(&q, Some(3), 10).0,
            [0..1, 1..2],
            "inside the work, it is current"
        );
    }

    #[test]
    fn a_song_played_before_comes_round_again_right_after_the_one_playing() {
        let mut q = queue(&["a", "b", "c", "d", "e"]);
        q.move_after(0..1, 2);
        assert_eq!(ids(&q), ["b", "c", "a", "d", "e"], "from before it");
        q.move_after(4..5, 1);
        assert_eq!(
            ids(&q),
            ["b", "c", "e", "a", "d"],
            "from after it, as before"
        );
        q.move_after(0..2, 3);
        assert_eq!(
            ids(&q),
            ["e", "a", "b", "c", "d"],
            "two songs from before it"
        );
    }

    #[test]
    fn each_change_says_where_the_songs_went() {
        let mut q = queue(&["a", "b", "c", "d"]);
        assert_eq!(
            q.insert(2, vec![song("a")], None),
            [Some(0), Some(1), Some(3), Some(4)]
        );
        assert_eq!(ids(&q), ["a", "b", "a", "c", "d"]);
        assert_eq!(q.remove(1..2), [Some(0), None, Some(1), Some(2), Some(3)]);
        assert_eq!(q.move_to(3..4, 1), [Some(0), Some(2), Some(3), Some(1)]);
        assert_eq!(ids(&q), ["a", "d", "a", "c"]);
        assert_eq!(q.keep_only(2), [None, None, Some(0), None]);
        assert_eq!(ids(&q), ["a"]);
    }

    #[test]
    fn a_run_is_the_songs_from_a_row_and_stops_at_the_last() {
        let songs: Vec<Child> = ["a", "b", "c", "d", "e"].map(song).to_vec();
        let ids = |songs: Vec<Child>| songs.into_iter().map(|song| song.id).collect::<Vec<_>>();
        assert_eq!(ids(run_from(songs.clone(), 1, 3)), ["b", "c", "d"]);
        assert_eq!(ids(run_from(songs.clone(), 3, 4)), ["d", "e"]);
        assert_eq!(ids(run_from(songs.clone(), 2, 1)), ["c"]);
        assert!(run_from(songs, 5, 2).is_empty());
    }

    #[test]
    fn each_seed_shuffles_its_own_way() {
        let order = |seed| {
            let mut q = queue(&["a", "b", "c", "d", "e", "f", "g", "h"]);
            q.shuffle(None, seed);
            ids(&q).concat()
        };
        let orders: std::collections::HashSet<String> = (0..5).map(order).collect();
        assert!(orders.len() > 3, "{orders:?}");
    }

    fn ids_of(songs: &[Child]) -> Vec<String> {
        songs.iter().map(|song| song.id.clone()).collect()
    }

    #[test]
    fn playing_keeps_the_order_and_queueing_says_where() {
        let songs: Vec<Child> = ["a", "b", "c"].map(song).to_vec();
        let Plan::Play(played) = plan("play", songs.clone(), false, 1) else {
            panic!("play didn't play");
        };
        assert_eq!(ids_of(&played), ids_of(&songs));
    }

    #[test]
    fn shuffling_next_or_last_into_an_empty_queue_plays_them_as_shuffle_now_does() {
        let songs: Vec<Child> = ["a", "b", "c", "d", "e", "f"].map(song).to_vec();
        for how in ["shuffle-now", "shuffle-next", "shuffle-last"] {
            match plan(how, songs.clone(), true, 9) {
                // In their own order: the queue shuffles them, so the shuffle is on.
                Plan::PlayShuffled(played) => assert_eq!(ids_of(&played), ids_of(&songs), "{how}"),
                _ => panic!("{how} didn't play shuffled into an empty queue"),
            }
        }
    }

    #[test]
    fn shuffling_next_or_last_into_a_queue_puts_them_in_at_random() {
        let songs: Vec<Child> = ["a", "b", "c", "d", "e", "f"].map(song).to_vec();
        for (how, want_next) in [("shuffle-next", true), ("shuffle-last", false)] {
            let Plan::Enqueue {
                songs: added,
                next,
                shuffle_seed: Some(seed),
            } = plan(how, songs.clone(), false, 9)
            else {
                panic!("{how} didn't queue shuffled into a queue");
            };
            assert_eq!((ids_of(&added), next), (ids_of(&songs), want_next));
            let mut q = queue(&["x"]);
            q.insert(1, added, Some(seed));
            assert_ne!(ids(&q)[1..], ["a", "b", "c", "d", "e", "f"], "{how}");
            let mut sorted = ids(&q);
            sorted.sort();
            assert_eq!(sorted, ["a", "b", "c", "d", "e", "f", "x"]);
        }
        for (how, want_next) in [("queue", false), ("next", true)] {
            let Plan::Enqueue {
                songs: added,
                next,
                shuffle_seed: None,
            } = plan(how, songs.clone(), false, 9)
            else {
                panic!("{how} didn't queue in order");
            };
            assert_eq!((ids_of(&added), next), (ids_of(&songs), want_next));
        }
    }

    #[test]
    fn shuffled_the_song_playing_comes_first_and_unshuffled_the_order_returns() {
        let mut q = queue(&["a", "b", "c", "d", "e", "f"]);
        let moved = q.shuffle(Some(2), 7);
        assert_eq!(ids(&q)[0], "c");
        assert_eq!(moved[2], Some(0));
        let mut sorted = ids(&q);
        sorted.sort();
        assert_eq!(sorted, ["a", "b", "c", "d", "e", "f"]);
        assert_ne!(ids(&q), ["c", "a", "b", "d", "e", "f"], "seed 7 moves some");
        // Played next while shuffled, it comes after c in the queued order
        // too; added, at its end. One taken out stays out.
        q.insert(1, vec![song("x")], None);
        q.insert(q.len(), vec![song("y")], None);
        let at = q.songs().position(|song| song.id == "e").unwrap();
        q.remove(at..at + 1);
        q.unshuffle();
        assert_eq!(ids(&q), ["a", "b", "c", "x", "d", "f", "y"]);
        assert!(!q.shuffled());
    }

    /// A movement of `work` on album `album`.
    fn movement(id: &str, work: &str, album: &str) -> Child {
        serde_json::from_value(serde_json::json!({
            "id": id, "isDir": false, "title": id, "albumId": album,
            "works": [{ "name": work }],
        }))
        .unwrap()
    }

    /// A page of songs: pieces "p1" and "p2" of work "P" on album "x",
    /// "s1" to "s3" of work "S", and plain songs "a" and "b".
    fn page() -> Vec<Child> {
        vec![
            song("a"),
            movement("p1", "P", "x"),
            movement("p2", "P", "x"),
            song("b"),
            movement("s1", "S", "x"),
            movement("s2", "S", "x"),
            movement("s3", "S", "x"),
        ]
    }

    fn units(queue: &Queue) -> Vec<Range<usize>> {
        let mut at = 0;
        let mut units = Vec::new();
        while at < queue.len() {
            let unit = queue.unit_at(at);
            at = unit.songs.end;
            units.push(unit.songs);
        }
        units
    }

    #[test]
    fn a_run_of_one_works_movements_on_a_page_is_one_unit() {
        let q = Queue::from_page(page(), 0);
        assert_eq!(units(&q), [0..1, 1..3, 3..4, 4..7]);
        assert!(q.unit_at(2).work.is_some());
        assert_eq!(q.unit_at(0).work, None);
    }

    #[test]
    fn a_work_with_one_song_or_split_by_another_or_by_albums_is_not_joined() {
        let songs = vec![
            movement("p1", "P", "x"),
            song("a"),
            movement("p2", "P", "x"),
            movement("p3", "P", "y"),
            movement("p4", "P", "y"),
        ];
        let q = Queue::from_page(songs, 0);
        assert_eq!(units(&q), [0..1, 1..2, 2..3, 3..5]);
        assert!(q.unit_at(0).work.is_none());
        assert!(q.unit_at(3).work.is_some());
    }

    #[test]
    fn a_play_from_inside_a_work_leaves_its_movements_as_songs() {
        // From a movement after the first: all of that work's are songs.
        assert_eq!(
            units(&Queue::from_page(page(), 5)),
            [0..1, 1..3, 3..4, 4..5, 5..6, 6..7]
        );
        // From the first movement, or a song, the work stays whole.
        assert_eq!(
            units(&Queue::from_page(page(), 4)),
            [0..1, 1..3, 3..4, 4..7]
        );
        assert_eq!(
            units(&Queue::from_page(page(), 3)),
            [0..1, 1..3, 3..4, 4..7]
        );
    }

    #[test]
    fn a_shuffle_moves_a_work_as_one_unit_with_its_movements_in_order() {
        for seed in 0..40 {
            let mut q = Queue::from_page(page(), 0);
            q.shuffle(None, seed);
            let order = ids(&q).join(" ");
            assert!(order.contains("p1 p2"), "{order}");
            assert!(order.contains("s1 s2 s3"), "{order}");
            assert_eq!(q.len(), 7);
        }
        let orders: HashSet<String> = (0..40)
            .map(|seed| {
                let mut q = Queue::from_page(page(), 0);
                q.shuffle(None, seed);
                ids(&q).join(" ")
            })
            .collect();
        assert!(orders.len() > 5, "{orders:?}");
    }

    #[test]
    fn shuffled_the_unit_playing_comes_first_and_unshuffled_the_order_returns() {
        let mut q = Queue::from_page(page(), 0);
        let moved = q.shuffle(Some(5), 3);
        assert_eq!(ids(&q)[..3], ["s1", "s2", "s3"]);
        assert_eq!(moved[5], Some(1));
        q.unshuffle();
        assert_eq!(ids(&q), ["a", "p1", "p2", "b", "s1", "s2", "s3"]);
        assert_eq!(units(&q), [0..1, 1..3, 3..4, 4..7]);
    }

    #[test]
    fn songs_played_next_go_in_after_the_work_playing() {
        let mut q = Queue::from_page(page(), 0);
        // Song p1 plays, so the next place is inside its work.
        q.insert(2, vec![song("x")], None);
        assert_eq!(ids(&q), ["a", "p1", "p2", "x", "b", "s1", "s2", "s3"]);
        let moved = q.move_to(7..8, 2);
        assert_eq!(ids(&q), ["a", "p1", "p2", "s3", "x", "b", "s1", "s2"]);
        assert_eq!(moved[7], Some(3));
    }

    #[test]
    fn works_queued_shuffled_stay_whole_and_unshuffle_with_the_queue() {
        for seed in 0..40 {
            let mut q = queue(&["x", "y"]);
            q.shuffle(None, 1);
            q.insert(1, page(), Some(seed));
            let order = ids(&q).join(" ");
            assert!(
                order.contains("p1 p2") && order.contains("s1 s2 s3"),
                "{order}"
            );
        }
        let mut q = queue(&["x", "y"]);
        q.insert(2, page(), Some(4));
        assert_eq!(&ids(&q)[..2], ["x", "y"]);
    }

    #[test]
    fn the_split_buttons_shuffles_keep_works_whole() {
        for how in ["shuffle-next", "shuffle-last"] {
            let Plan::Enqueue {
                songs,
                next,
                shuffle_seed,
            } = plan(how, page(), false, 5)
            else {
                panic!("{how} didn't queue");
            };
            let mut q = queue(&["x", "y", "z"]);
            q.insert(if next { 1 } else { 3 }, songs, shuffle_seed);
            let order = ids(&q).join(" ");
            assert!(
                order.contains("p1 p2") && order.contains("s1 s2 s3"),
                "{how}: {order}"
            );
            assert_eq!(ids(&q)[..1], ["x"]);
        }
        let Plan::PlayShuffled(songs) = plan("shuffle-now", page(), false, 5) else {
            panic!("shuffle-now didn't play shuffled");
        };
        let mut q = Queue::from_page(songs, 0);
        q.shuffle(None, 5);
        assert!(ids(&q).join(" ").contains("s1 s2 s3"));
    }

    #[test]
    fn taking_out_a_movement_keeps_the_others_and_a_work_goes_as_one() {
        let mut q = Queue::from_page(page(), 0);
        assert_eq!(
            q.remove(5..6),
            [Some(0), Some(1), Some(2), Some(3), Some(4), None, Some(5)]
        );
        assert_eq!(units(&q), [0..1, 1..3, 3..4, 4..6]);
        q.remove(1..3);
        assert_eq!(ids(&q), ["a", "b", "s1", "s3"]);
        // One movement left is a song again.
        q.remove(3..4);
        assert_eq!(q.unit_at(2).work, None);
    }

    #[test]
    fn dropping_a_work_onto_another_row_puts_it_before_or_after_that_row_whole() {
        let mut q = Queue::from_page(page(), 0);
        q.move_onto(1..3, 4..7);
        assert_eq!(ids(&q), ["a", "b", "s1", "s2", "s3", "p1", "p2"]);
        q.move_onto(2..5, 0..1);
        assert_eq!(ids(&q), ["s1", "s2", "s3", "a", "b", "p1", "p2"]);
        q.move_onto(0..1, 3..4);
        assert_eq!(ids(&q), ["s2", "s3", "a", "s1", "b", "p1", "p2"]);
    }

    #[test]
    fn the_list_has_a_row_per_work_and_opens_it_to_its_movements() {
        let mut q = Queue::from_page(page(), 0);
        let row = |songs: Range<usize>, shape| Row { songs, shape };
        let key = |q: &Queue, at| q.unit_at(at).work.unwrap();
        let closed = q.rows(Some(1), 10);
        assert_eq!(
            closed,
            [
                row(
                    1..3,
                    Shape::Work {
                        key: key(&q, 1),
                        expanded: false
                    }
                ),
                row(3..4, Shape::Song),
                row(
                    4..7,
                    Shape::Work {
                        key: key(&q, 4),
                        expanded: false
                    }
                ),
            ]
        );
        // From a movement inside a work, the list starts at the work.
        assert_eq!(q.rows(Some(2), 10), closed);
        assert_eq!(q.rows(None, 10).len(), 4);
        q.toggle_work(key(&q, 4));
        let open = q.rows(Some(3), 10);
        assert_eq!(open.len(), 5);
        assert_eq!(
            open[1],
            row(
                4..7,
                Shape::Work {
                    key: key(&q, 4),
                    expanded: true
                }
            )
        );
        assert_eq!(
            open[2..],
            [
                row(4..5, Shape::Movement),
                row(5..6, Shape::Movement),
                row(6..7, Shape::Movement),
            ]
        );
        assert_eq!(q.rows(None, 2).len(), 2, "stops at the limit");
        q.toggle_work(key(&q, 4));
        assert_eq!(q.rows(Some(3), 10).len(), 2);
    }

    #[test]
    fn a_queue_loaded_anew_has_its_whole_works_open_as_they_were() {
        let mut q = Queue::from_page(page(), 0);
        q.toggle_work(q.unit_at(4).work.unwrap());
        let expanded = q.expanded_works();
        assert_eq!(expanded, ["s1"]);

        let mut loaded = Queue::new(page());
        assert_eq!(units(&loaded), [0..1, 1..3, 3..4, 4..7]);
        loaded.expand_works(&expanded);
        let rows = loaded.rows(None, 10);
        assert_eq!(rows.len(), 7);
        assert!(matches!(
            rows[1].shape,
            Shape::Work {
                expanded: false,
                ..
            }
        ));
        assert!(matches!(rows[3].shape, Shape::Work { expanded: true, .. }));
        assert_eq!(loaded.expanded_works(), ["s1"]);
    }
}
