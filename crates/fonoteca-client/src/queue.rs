//! The songs queued to play, in the order they play, and while shuffled the
//! order they were queued in. Each change says where every song went, so
//! the player carries on with what it has.

use std::collections::HashMap;

use opensubsonic::Child;

/// Where each song of the queue before a change sits after it, if it
/// stayed.
pub type Moved = Vec<Option<usize>>;

#[derive(Default)]
pub struct Queue {
    entries: Vec<Entry>,
    /// The entries in the order they were queued in, while shuffled.
    unshuffled: Option<Vec<u64>>,
    next_key: u64,
}

/// A song in the queue; the same song queued twice is two entries.
struct Entry {
    key: u64,
    song: Child,
}

impl Queue {
    pub fn new(songs: Vec<Child>) -> Self {
        let mut queue = Queue::default();
        queue.entries = songs.into_iter().map(|song| queue.entry(song)).collect();
        queue
    }

    fn entry(&mut self, song: Child) -> Entry {
        self.next_key += 1;
        Entry {
            key: self.next_key,
            song,
        }
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

    /// Puts `songs` in before the `at`th song, or at the end.
    pub fn insert(&mut self, at: usize, songs: Vec<Child>) -> Moved {
        let at = at.min(self.len());
        self.change(|queue| {
            let added: Vec<Entry> = songs.into_iter().map(|song| queue.entry(song)).collect();
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

    pub fn remove(&mut self, at: usize) -> Moved {
        self.change(|queue| {
            if at < queue.entries.len() {
                let key = queue.entries.remove(at).key;
                if let Some(order) = &mut queue.unshuffled {
                    order.retain(|&kept| kept != key);
                }
            }
        })
    }

    /// Moves the `from`th song to be the `to`th.
    pub fn move_to(&mut self, from: usize, to: usize) -> Moved {
        self.change(|queue| {
            if from < queue.entries.len() {
                let entry = queue.entries.remove(from);
                queue.entries.insert(to.min(queue.entries.len()), entry);
            }
        })
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

    /// Puts the `current`th song first and the others after it in an order
    /// `seed` picks, keeping the order they were queued in.
    pub fn shuffle(&mut self, current: Option<usize>, seed: u64) -> Moved {
        self.change(|queue| {
            if queue.unshuffled.is_none() {
                queue.unshuffled = Some(queue.entries.iter().map(|entry| entry.key).collect());
            }
            let first = current
                .filter(|&current| current < queue.entries.len())
                .map(|current| queue.entries.remove(current));
            let mut random = Random(seed);
            // Fisher and Yates's: each order equally likely.
            for at in (1..queue.entries.len()).rev() {
                queue.entries.swap(at, random.below(at + 1));
            }
            queue.entries.splice(0..0, first);
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

/// The `count` songs from the `start`th, as a work's row queues its
/// movements.
pub fn run_from(songs: Vec<Child>, start: usize, count: usize) -> Vec<Child> {
    songs.into_iter().skip(start).take(count).collect()
}

/// `songs` in an order `seed` picks.
fn shuffled(songs: Vec<Child>, seed: u64) -> Vec<Child> {
    let mut queue = Queue::new(songs);
    queue.shuffle(None, seed);
    queue.songs().cloned().collect()
}

/// What to do with the songs of a page for one of its menu's words: "next",
/// "queue", "shuffle-now", "shuffle-next" or "shuffle-last".
pub enum Plan {
    /// Play them in a random order, the queue's shuffle on.
    PlayShuffled(Vec<Child>),
    /// Put them in after the song playing, or at the end.
    Enqueue(Vec<Child>, bool),
}

/// An empty queue has no order to put shuffled songs into: they play as
/// "shuffle-now" does, so that turning the shuffle off restores their order.
pub fn plan(how: &str, songs: Vec<Child>, queue_is_empty: bool, seed: u64) -> Plan {
    match how {
        "shuffle-now" => Plan::PlayShuffled(songs),
        "shuffle-next" | "shuffle-last" if queue_is_empty => Plan::PlayShuffled(songs),
        "shuffle-next" | "shuffle-last" => {
            Plan::Enqueue(shuffled(songs, seed), how == "shuffle-next")
        }
        _ => Plan::Enqueue(songs, how == "next"),
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
    fn each_change_says_where_the_songs_went() {
        let mut q = queue(&["a", "b", "c", "d"]);
        assert_eq!(
            q.insert(2, vec![song("a")]),
            [Some(0), Some(1), Some(3), Some(4)]
        );
        assert_eq!(ids(&q), ["a", "b", "a", "c", "d"]);
        assert_eq!(q.remove(1), [Some(0), None, Some(1), Some(2), Some(3)]);
        assert_eq!(q.move_to(3, 1), [Some(0), Some(2), Some(3), Some(1)]);
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
    fn shuffling_next_or_last_into_an_empty_queue_plays_them_as_shuffle_now_does() {
        let songs: Vec<Child> = ["a", "b", "c", "d", "e", "f"].map(song).to_vec();
        for how in ["shuffle-now", "shuffle-next", "shuffle-last"] {
            match plan(how, songs.clone(), true, 9) {
                // In their own order: the queue shuffles them, so the shuffle is on.
                Plan::PlayShuffled(played) => assert_eq!(ids_of(&played), ids_of(&songs), "{how}"),
                Plan::Enqueue(..) => panic!("{how} queued into an empty queue"),
            }
        }
    }

    #[test]
    fn shuffling_next_or_last_into_a_queue_puts_them_in_at_random() {
        let songs: Vec<Child> = ["a", "b", "c", "d", "e", "f"].map(song).to_vec();
        for (how, next) in [("shuffle-next", true), ("shuffle-last", false)] {
            let Plan::Enqueue(added, put_next) = plan(how, songs.clone(), false, 9) else {
                panic!("{how} didn't queue into a queue");
            };
            assert_eq!(put_next, next);
            assert_ne!(ids_of(&added), ids_of(&songs), "{how}");
            let mut sorted = ids_of(&added);
            sorted.sort();
            assert_eq!(sorted, ids_of(&songs));
        }
        let Plan::Enqueue(added, next) = plan("queue", songs.clone(), false, 9) else {
            panic!("queue didn't queue");
        };
        assert_eq!((ids_of(&added), next), (ids_of(&songs), false));
        let Plan::Enqueue(_, next) = plan("next", songs, false, 9) else {
            panic!("next didn't queue");
        };
        assert!(next);
    }

    #[test]
    fn shuffled_songs_keep_every_song_in_an_order_the_seed_picks() {
        let songs: Vec<Child> = ["a", "b", "c", "d", "e", "f", "g", "h"].map(song).to_vec();
        let ids = |songs: &[Child]| songs.iter().map(|song| song.id.clone()).collect::<Vec<_>>();
        let mixed = shuffled(songs.clone(), 3);
        assert_eq!(ids(&mixed), ids(&shuffled(songs.clone(), 3)));
        assert_ne!(ids(&mixed), ids(&songs));
        let mut sorted = ids(&mixed);
        sorted.sort();
        assert_eq!(sorted, ids(&songs));
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
        q.insert(1, vec![song("x")]);
        q.insert(q.len(), vec![song("y")]);
        let at = q.songs().position(|song| song.id == "e").unwrap();
        q.remove(at);
        q.unshuffle();
        assert_eq!(ids(&q), ["a", "b", "c", "x", "d", "f", "y"]);
        assert!(!q.shuffled());
    }
}
