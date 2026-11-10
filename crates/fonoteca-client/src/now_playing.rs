//! What plays, as the system's media controls are told it, and what they ask
//! back: the keys of a keyboard or headset, Control Center's buttons and its
//! slider. Nothing here touches the system, so the rules are tested alone;
//! `desktop_media` is the system's side.

use std::path::PathBuf;

/// What the media controls show of the song or station playing.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// In seconds; 0 for a live stream.
    pub duration: f64,
    /// A picture on this device, never a server's address: that carries the
    /// account's credentials.
    pub cover: Option<PathBuf>,
    pub live: bool,
}

/// What a control asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Seek(f64),
}

/// What the bar's own controls do for a [`Command`], as the Android buttons
/// do: it is those the app has callbacks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    TogglePause,
    Next,
    Previous,
    Seek(f64),
}

/// A headset's play while playing, or pause while paused, changes nothing.
pub fn action(command: Command, paused: bool) -> Option<Action> {
    match command {
        Command::Play if paused => Some(Action::TogglePause),
        Command::Pause if !paused => Some(Action::TogglePause),
        Command::Play | Command::Pause => None,
        Command::Toggle => Some(Action::TogglePause),
        Command::Next => Some(Action::Next),
        Command::Previous => Some(Action::Previous),
        Command::Seek(seconds) => Some(Action::Seek(seconds)),
    }
}

/// The system's side of the media controls.
pub trait NowPlaying {
    /// Shows `track`, `position` seconds in, as playing or paused.
    fn show(&self, track: &Track, playing: bool, position: f64);
    /// Moves what is shown to playing or paused, `position` seconds in.
    fn set_state(&self, playing: bool, position: f64);
    /// Shows nothing and gives up the keys.
    fn clear(&self);
}

/// Tells a [`NowPlaying`] what plays, only once something has played, and
/// keeps a picture that comes late from replacing a newer song's.
#[derive(Default)]
pub struct Announcer<N> {
    target: N,
    /// The id of the track shown.
    shown: Option<String>,
}

impl<N: NowPlaying> Announcer<N> {
    /// Tells that the track `id` is at `position`, playing or paused. A queue
    /// restored paused is not announced: until something plays, the keys
    /// are not the app's. Returns whether this is a track not shown before,
    /// whose cover is to be fetched.
    pub fn announce(&mut self, id: &str, track: &Track, playing: bool, position: f64) -> bool {
        if self.shows(id) {
            self.target.set_state(playing, position);
            return false;
        }
        if !playing && self.shown.is_none() {
            return false;
        }
        self.target.show(track, playing, position);
        self.shown = Some(id.to_owned());
        true
    }

    /// Whether the track `id` is the one shown.
    pub fn shows(&self, id: &str) -> bool {
        self.shown.as_deref() == Some(id)
    }

    /// Shows `track` again with its cover, unless the track `id` is no longer
    /// the one shown.
    pub fn cover_ready(&mut self, id: &str, track: &Track, playing: bool, position: f64) {
        if self.shows(id) {
            self.target.show(track, playing, position);
        }
    }

    /// Stops announcing, until something plays again.
    pub fn clear(&mut self) {
        if self.shown.take().is_some() {
            self.target.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    enum Told {
        Show(String, bool, f64),
        State(bool, f64),
        Clear,
    }

    #[derive(Clone, Default)]
    struct Recording(Rc<RefCell<Vec<Told>>>);

    impl NowPlaying for Recording {
        fn show(&self, track: &Track, playing: bool, position: f64) {
            let title = format!("{}|{:?}", track.title, track.cover);
            self.0
                .borrow_mut()
                .push(Told::Show(title, playing, position));
        }
        fn set_state(&self, playing: bool, position: f64) {
            self.0.borrow_mut().push(Told::State(playing, position));
        }
        fn clear(&self) {
            self.0.borrow_mut().push(Told::Clear);
        }
    }

    fn track(title: &str) -> Track {
        Track {
            title: title.into(),
            artist: String::new(),
            album: String::new(),
            duration: 200.0,
            cover: None,
            live: false,
        }
    }

    fn announcer() -> (Announcer<Recording>, Recording) {
        let told = Recording::default();
        let announcer = Announcer {
            target: told.clone(),
            shown: None,
        };
        (announcer, told)
    }

    #[test]
    fn a_play_while_playing_and_a_pause_while_paused_change_nothing() {
        assert_eq!(action(Command::Play, false), None);
        assert_eq!(action(Command::Pause, true), None);
    }

    #[test]
    fn a_play_while_paused_and_a_pause_while_playing_toggle() {
        assert_eq!(action(Command::Play, true), Some(Action::TogglePause));
        assert_eq!(action(Command::Pause, false), Some(Action::TogglePause));
    }

    #[test]
    fn the_other_commands_pass_as_they_are() {
        for paused in [false, true] {
            assert_eq!(action(Command::Toggle, paused), Some(Action::TogglePause));
            assert_eq!(action(Command::Next, paused), Some(Action::Next));
            assert_eq!(action(Command::Previous, paused), Some(Action::Previous));
            assert_eq!(action(Command::Seek(9.5), paused), Some(Action::Seek(9.5)));
        }
    }

    #[test]
    fn a_queue_restored_paused_is_not_announced() {
        let (mut announcer, told) = announcer();
        assert!(!announcer.announce("a", &track("A"), false, 30.0));
        assert!(told.0.borrow().is_empty());
    }

    #[test]
    fn a_new_track_is_shown_and_asks_for_its_cover_once() {
        let (mut announcer, told) = announcer();
        assert!(announcer.announce("a", &track("A"), true, 0.0));
        assert!(!announcer.announce("a", &track("A"), true, 15.0));
        assert_eq!(
            *told.0.borrow(),
            [
                Told::Show("A|None".into(), true, 0.0),
                Told::State(true, 15.0)
            ]
        );
    }

    #[test]
    fn a_pause_after_playing_is_announced() {
        let (mut announcer, told) = announcer();
        announcer.announce("a", &track("A"), true, 0.0);
        announcer.announce("a", &track("A"), false, 4.0);
        assert_eq!(told.0.borrow()[1], Told::State(false, 4.0));
    }

    #[test]
    fn a_cover_for_a_track_no_longer_shown_is_dropped() {
        let (mut announcer, told) = announcer();
        announcer.announce("a", &track("A"), true, 0.0);
        announcer.announce("b", &track("B"), true, 0.0);
        let mut late = track("A");
        late.cover = Some("a.img".into());
        announcer.cover_ready("a", &late, true, 3.0);
        assert_eq!(told.0.borrow().len(), 2);
        assert!(!announcer.shows("a"));
        let mut current = track("B");
        current.cover = Some("b.img".into());
        announcer.cover_ready("b", &current, true, 3.0);
        assert_eq!(
            told.0.borrow()[2],
            Told::Show("B|Some(\"b.img\")".into(), true, 3.0)
        );
    }

    #[test]
    fn clearing_forgets_what_played_so_a_paused_restore_stays_quiet() {
        let (mut announcer, told) = announcer();
        announcer.announce("a", &track("A"), true, 0.0);
        announcer.clear();
        announcer.clear();
        assert!(!announcer.announce("a", &track("A"), false, 0.0));
        let cover = track("A");
        announcer.cover_ready("a", &cover, true, 0.0);
        assert_eq!(
            *told.0.borrow(),
            [Told::Show("A|None".into(), true, 0.0), Told::Clear]
        );
    }
}
