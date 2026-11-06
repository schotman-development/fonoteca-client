//! The player where there is no sound, as in the browser: it fetches and
//! plays nothing, but goes through its queue as the real one does when told
//! to, so the app's bar and now playing page show what would play. Nothing
//! moves by itself: a track stays where it was started or sought to until
//! another is asked for.

use std::cell::{Cell, RefCell};

use crate::{Leveling, RESTART_AFTER, Repeat, Status, Track};

#[derive(Default)]
pub struct Player {
    status: RefCell<Status>,
    /// How many tracks the queue has.
    length: Cell<usize>,
    repeat: Cell<Repeat>,
}

impl Player {
    /// A player with nothing queued.
    pub fn silent() -> Self {
        Self::default()
    }

    /// Plays `queue` from `seconds` into its `start`th track.
    pub fn play(&self, queue: Vec<Track>, start: usize, seconds: f64) {
        self.length.set(queue.len());
        let mut status = self.status.borrow_mut();
        *status = Status {
            index: Some(start),
            position: seconds,
            requeues: status.requeues,
            ..Status::default()
        };
    }

    /// Puts `queue` in place of the one playing, where `moved` gives each
    /// track of the one before its place in `queue`, or none if it was taken
    /// out. What plays goes on; taken out, the first after it that stayed
    /// plays in its place.
    pub fn requeue(&self, queue: Vec<Track>, moved: Vec<Option<usize>>) {
        self.length.set(queue.len());
        let place = |index: usize| moved.get(index).copied().flatten();
        let playing = {
            let mut status = self.status.borrow_mut();
            status.requeues += 1;
            status.index
        };
        let Some(playing) = playing else { return };
        match place(playing) {
            Some(index) => self.status.borrow_mut().index = Some(index),
            None => self.go_to((playing..moved.len()).find_map(place)),
        }
    }

    /// Plays the queue's `index`th track from its start.
    pub fn jump(&self, index: usize) {
        self.go_to(Some(index));
        self.set_paused(false);
    }

    pub fn next(&self) {
        let Some(index) = self.status.borrow().index else {
            return;
        };
        let next = if index + 1 < self.length.get() {
            Some(index + 1)
        } else {
            (self.repeat.get() == Repeat::All).then_some(0)
        };
        self.go_to(next);
    }

    /// Goes back to the track before, or to the start of this one when it is
    /// the first or further in than a moment, as the real player does.
    pub fn previous(&self) {
        let (index, position) = {
            let status = self.status.borrow();
            let Some(index) = status.index else { return };
            (index, status.position)
        };
        if position > RESTART_AFTER || index == 0 {
            self.go_to(Some(index));
        } else {
            self.go_to(Some(index - 1));
        }
    }

    pub fn seek(&self, seconds: f64) {
        self.status.borrow_mut().position = seconds.max(0.0);
    }

    pub fn set_paused(&self, paused: bool) {
        self.status.borrow_mut().paused = paused;
    }

    /// Nothing is heard, so there is nothing to level.
    pub fn set_leveling(&self, _leveling: Leveling) {}

    pub fn set_repeat(&self, repeat: Repeat) {
        self.repeat.set(repeat);
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// Plays the `index`th track from its start, or ends the queue at none.
    fn go_to(&self, index: Option<usize>) {
        let mut status = self.status.borrow_mut();
        status.index = index;
        status.position = 0.0;
        status.ended = index.is_none();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(length: usize) -> Vec<Track> {
        (0..length)
            .map(|n| Track {
                id: n.to_string(),
                gain: Default::default(),
            })
            .collect()
    }

    fn at(player: &Player) -> (Option<usize>, f64) {
        let status = player.status();
        (status.index, status.position)
    }

    #[test]
    fn a_queue_goes_on_only_when_told() {
        let player = Player::silent();
        player.play(queue(3), 1, 20.0);
        player.set_paused(true);
        player.set_leveling(Leveling::Album);
        assert_eq!(at(&player), (Some(1), 20.0));
        player.seek(50.0);
        player.next();
        assert_eq!(at(&player), (Some(2), 0.0));
        assert!(player.status().paused);
        player.seek(50.0);
        assert_eq!(at(&player), (Some(2), 50.0));
        player.jump(0);
        assert_eq!(at(&player), (Some(0), 0.0));
        player.seek(-5.0);
        assert_eq!(at(&player), (Some(0), 0.0));
        assert!(!player.status().paused);
    }

    #[test]
    fn the_end_of_the_queue_ends_it_unless_it_repeats() {
        let player = Player::silent();
        player.play(queue(2), 1, 0.0);
        player.next();
        assert!(player.status().ended);
        player.play(queue(2), 1, 0.0);
        player.set_repeat(Repeat::All);
        player.next();
        assert_eq!(at(&player), (Some(0), 0.0));
        assert!(!player.status().ended);
    }

    #[test]
    fn previous_starts_a_track_played_a_while_again() {
        let player = Player::silent();
        player.play(queue(2), 1, RESTART_AFTER + 1.0);
        player.previous();
        assert_eq!(at(&player), (Some(1), 0.0));
        player.previous();
        assert_eq!(at(&player), (Some(0), 0.0));
        player.previous();
        assert_eq!(at(&player), (Some(0), 0.0), "the first track starts again");
    }

    #[test]
    fn a_track_moved_plays_on_and_one_taken_out_gives_way_to_the_next_that_stayed() {
        let player = Player::silent();
        player.play(queue(4), 1, 30.0);
        player.requeue(queue(4), vec![Some(1), Some(3), Some(0), Some(2)]);
        assert_eq!(at(&player), (Some(3), 30.0));
        player.requeue(queue(2), vec![Some(0), Some(1), None, None]);
        assert_eq!(at(&player), (None, 0.0));
        assert!(player.status().ended);
        player.play(queue(4), 1, 30.0);
        player.requeue(queue(2), vec![Some(0), None, None, Some(1)]);
        assert_eq!(at(&player), (Some(1), 0.0));
        assert_eq!(player.status().requeues, 3);
    }
}
