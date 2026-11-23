//! Star ratings of songs, albums and artists, in whole stars from one to
//! five. A rating shows at once, wherever its item is on screen, and goes
//! back if the server doesn't take it. A server that answers "not
//! authorized" (the account lacks the comment role) takes no ratings at all,
//! so the controls go away.

use super::*;

/// The Subsonic error for an account that may not do what it asked.
const NOT_AUTHORIZED: i64 = 50;
const MOST_STARS: i32 = 5;

/// `rating` as the stars the pages show, none when it has none.
pub(crate) fn stars_of(rating: Option<i64>) -> i32 {
    rating.map_or(0, |rating| rating.clamp(0, MOST_STARS.into()) as i32)
}

/// The rating a press on star `pressed` leaves an item rated `current`
/// stars: that star, or none when it already is the rating, which is how a
/// rating is taken back.
fn after_press(current: i32, pressed: i32) -> u8 {
    let pressed = pressed.clamp(1, MOST_STARS);
    if pressed == current { 0 } else { pressed as u8 }
}

fn is_not_authorized(error: &opensubsonic::Error) -> bool {
    matches!(error, opensubsonic::Error::Api(api) if api.code == NOT_AUTHORIZED)
}

/// Connects the pages' stars: a track of an album or playlist, a row of a
/// table, and the album or artist on screen.
pub(super) fn connect(ui: &AppWindow, browser: &Shared) {
    ui.on_rate_track({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, pressed| rate_track(&weak.unwrap(), &browser, row as usize, pressed)
    });
    ui.on_rate_line({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |row, pressed| rate_line(&weak.unwrap(), &browser, row as usize, pressed)
    });
    ui.on_rate_header({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |pressed| rate_header(&weak.unwrap(), &browser, pressed)
    });
}

fn rate_track(ui: &AppWindow, browser: &Shared, row: usize, pressed: i32) {
    let song = browser.borrow().songs.get(row).cloned();
    if let Some(song) = song {
        rate(ui, browser, song.id, stars_of(song.user_rating), pressed);
    }
}

fn rate_line(ui: &AppWindow, browser: &Shared, row: usize, pressed: i32) {
    let song = {
        let b = browser.borrow();
        let (songs, order) = listed(&b, ui.get_page());
        order.get(row).and_then(|&song| songs.get(song)).cloned()
    };
    if let Some(song) = song {
        rate(ui, browser, song.id, stars_of(song.user_rating), pressed);
    }
}

fn rate_header(ui: &AppWindow, browser: &Shared, pressed: i32) {
    let id = browser.borrow().header_rated.clone();
    if let Some(id) = id {
        rate(ui, browser, id, ui.get_header_rating(), pressed);
    }
}

/// Makes the header of the album or artist `id` show its rating, and rate it.
pub(super) fn show_header(ui: &AppWindow, browser: &Shared, id: &str, rating: Option<i64>) {
    browser.borrow_mut().header_rated = Some(id.to_owned());
    ui.set_header_rating(stars_of(rating));
}

/// Rates the song, album or artist `id`, rated `current` stars, by a press
/// on star `pressed`, shown at once and put back when the server doesn't
/// take it.
pub(crate) fn rate(ui: &AppWindow, browser: &Shared, id: String, current: i32, pressed: i32) {
    let (client, visit) = {
        let b = browser.borrow();
        (b.client.clone(), b.visit)
    };
    let Some(client) = client else { return };
    let stars = after_press(current, pressed);
    show(ui, browser, &id, stars.into());
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let Err(error) = client.set_rating(&id, stars).await else {
            return;
        };
        let Some(ui) = weak.upgrade() else { return };
        show(&ui, &browser, &id, current);
        if is_not_authorized(&error) {
            ui.set_may_rate(false);
        } else if browser.borrow().visit == visit {
            ui.set_page_problem(format!("Your server didn't take that: {error}").into());
        }
    })
    .expect("the event loop is running");
}

/// Shows the item `id` as rated `stars` wherever it is: in what the pages
/// hold, on the rows and header on screen, and for the queue and the sheet.
fn show(ui: &AppWindow, browser: &Shared, id: &str, stars: i32) {
    let mut b = browser.borrow_mut();
    keep(&mut b, id, stars);
    show_in_rows(&mut b, ui.get_page(), id, stars);
    if b.header_rated.as_deref() == Some(id) {
        ui.set_header_rating(stars);
    }
    drop(b);
    ui.invoke_song_rated(id.into(), stars);
}

/// Keeps the rating with every list the pages hold the item in.
fn keep(b: &mut Browser, id: &str, stars: i32) {
    let rating = (stars > 0).then_some(i64::from(stars));
    let songs = (b.song_list.iter_mut().chain(&mut b.entries)).chain(&mut b.songs);
    for song in songs.filter(|song| song.id == id) {
        song.user_rating = rating;
    }
    for album in (b.library.iter_mut().chain(b.album.as_mut())).filter(|album| album.id == id) {
        album.user_rating = rating;
    }
    for artist in b.artists.iter_mut().filter(|artist| artist.id == id) {
        artist.user_rating = rating;
    }
}

/// Shows the song's rating on its rows of the album or playlist and of the
/// table `page` shows, the Rating column among them.
fn show_in_rows(b: &mut Browser, page: Page, id: &str, stars: i32) {
    for row in (0..b.songs.len()).filter(|&row| b.songs[row].id == id) {
        if let Some(mut track) = b.tracks.row_data(row) {
            track.rating = stars;
            b.tracks.set_row_data(row, track);
        }
    }
    if !is_song_table(page) {
        return;
    }
    let rated_column = COLUMNS.iter().position(|column| column.title == RATING);
    let cell_of = song_table(b, page)
        .shown()
        .iter()
        .position(|&c| Some(c) == rated_column);
    let (songs, order) = listed(b, page);
    let rows: Vec<(usize, Vec<Name>)> = (order.iter().enumerate())
        .filter(|&(_, &song)| songs[song].id == id)
        .map(|(row, &song)| {
            (
                row,
                (COLUMNS[rated_column.unwrap_or_default()].cell)(&songs[song]),
            )
        })
        .collect();
    for (row, cell) in rows {
        let Some(mut line) = b.lines.row_data(row) else {
            continue;
        };
        line.rating = stars;
        if let Some(at) = cell_of {
            line.cells.set_row_data(at, names(cell));
        }
        b.lines.set_row_data(row, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_rates_and_a_press_on_the_rating_takes_it_back() {
        assert_eq!(after_press(0, 4), 4);
        assert_eq!(after_press(2, 5), 5, "another star changes it");
        assert_eq!(after_press(3, 3), 0, "the same star clears it");
    }

    #[test]
    fn a_press_outside_the_stars_is_one_of_them() {
        assert_eq!(after_press(0, 0), 1);
        assert_eq!(after_press(0, 9), 5);
        assert_eq!(after_press(5, 9), 0);
    }

    #[test]
    fn a_rating_outside_one_to_five_shows_as_the_nearest() {
        assert_eq!(stars_of(None), 0);
        assert_eq!(stars_of(Some(3)), 3);
        assert_eq!(stars_of(Some(-1)), 0);
        assert_eq!(stars_of(Some(9)), 5);
    }

    #[test]
    fn only_not_authorized_means_the_account_may_not_rate() {
        let api = |code| {
            opensubsonic::Error::Api(opensubsonic::ApiError {
                code,
                message: None,
                help_url: None,
            })
        };
        assert!(is_not_authorized(&api(50)));
        assert!(!is_not_authorized(&api(70)));
        assert!(!is_not_authorized(&opensubsonic::Error::Status(500)));
    }
}
