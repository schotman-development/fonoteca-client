slint::include_modules!();

mod browse;
mod store;

use std::cell::RefCell;
use std::rc::Rc;

use opensubsonic::{Auth, Client};
use serde::{Deserialize, Serialize};

/// What the onboarding sets up, kept by [`store`].
#[derive(Serialize, Deserialize)]
pub struct Account {
    server: String,
    login: Login,
    listenbrainz: Option<ListenBrainz>,
}

#[derive(Clone, Serialize, Deserialize)]
enum Login {
    ApiKey(String),
    Password { username: String, password: String },
}

#[derive(Serialize, Deserialize)]
struct ListenBrainz {
    token: String,
    user: String,
}

impl Login {
    fn auth(&self) -> Auth {
        match self.clone() {
            Login::ApiKey(key) => Auth::ApiKey(key),
            Login::Password { username, password } => Auth::Password { username, password },
        }
    }
}

/// Whether a Subsonic server answers at `server`, and if it offers API keys.
async fn find_server(server: &str) -> Result<bool, String> {
    let client = Client::new(server, Auth::None).map_err(|e| e.to_string())?;
    match client.get_open_subsonic_extensions().await {
        Ok(extensions) => return Ok(extensions.iter().any(|e| e.name == "apiKeyAuthentication")),
        Err(opensubsonic::Error::Api(_)) => return Ok(false),
        Err(_) => {}
    }
    // Servers without OpenSubsonic may not know that call. Any Subsonic answer
    // to a ping, even a refusal, shows one is there.
    match client.ping().await {
        Ok(_) | Err(opensubsonic::Error::Api(_)) => Ok(false),
        Err(e) => Err(format!("No music server answered at this address: {e}")),
    }
}

/// Signs in and returns the server's name and version, and the client to
/// browse with.
async fn sign_in(server: &str, login: &Login) -> Result<(String, Client), String> {
    let client = Client::new(server, login.auth()).map_err(|e| e.to_string())?;
    // Switches to form POST where the server allows it, before the ping sends
    // credentials. Servers without OpenSubsonic refuse it, which is fine.
    let _ = client.get_open_subsonic_extensions().await;
    let info = client.ping().await.map_err(|e| e.to_string())?;
    let name = match (info.r#type, info.server_version) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name,
        _ => format!("Subsonic server, API {}", info.version),
    };
    Ok((name, client))
}

/// Opens the library with the saved account, or asks for a server when
/// there is none. Runs at start, and again on "Try again" after the server
/// didn't answer.
fn resume(ui: &AppWindow, browser: &browse::Shared) {
    ui.set_offline("".into());
    let (weak, browser) = (ui.as_weak(), browser.clone());
    slint::spawn_local(async move {
        let account = store::load().await;
        let Some(ui) = weak.upgrade() else { return };
        let Some(account) = account else {
            ui.set_step(Step::Server);
            return;
        };
        show(&ui, &account);
        ui.set_loading(true);
        let signed_in = sign_in(&account.server, &account.login).await;
        // Signed out while this was under way, and maybe in again since.
        if ui.get_step() != Step::Done || browse::started(&browser) {
            return;
        }
        ui.set_loading(false);
        match signed_in {
            Ok((name, client)) => {
                ui.set_server_name(name.into());
                browse::start(&ui, &browser, client, listenbrainz_client(&account));
            }
            Err(e) => ui.set_offline(format!("Your server didn't answer: {e}").into()),
        }
    })
    .expect("the event loop is running");
}

/// The ListenBrainz client for loves, when the account has a token.
fn listenbrainz_client(account: &Account) -> Option<listenbrainz::Client> {
    let token = account.listenbrainz.as_ref()?.token.clone();
    listenbrainz::Client::new(token).ok()
}

async fn connect_listenbrainz(token: String) -> Result<ListenBrainz, String> {
    let client = listenbrainz::Client::new(token.clone()).map_err(|e| e.to_string())?;
    match client.validate_token().await.map_err(|e| e.to_string())? {
        Some(user) => Ok(ListenBrainz { token, user }),
        None => Err("ListenBrainz doesn't accept this token.".into()),
    }
}

/// Runs `task` while the screen shows it is busy, then hands its result to
/// `done`, or shows its error.
fn run<T: 'static>(
    ui: &AppWindow,
    task: impl Future<Output = Result<T, String>> + 'static,
    done: impl FnOnce(&AppWindow, T) + 'static,
) {
    ui.set_busy(true);
    ui.set_problem("".into());
    let weak = ui.as_weak();
    slint::spawn_local(async move {
        let result = task.await;
        let Some(ui) = weak.upgrade() else { return };
        ui.set_busy(false);
        match result {
            Ok(value) => done(&ui, value),
            Err(e) => ui.set_problem(e.into()),
        }
    })
    .expect("the event loop is running");
}

/// Shows the done screen for `account`, and clears the secrets from the
/// screens before it.
fn show(ui: &AppWindow, account: &Account) {
    ui.set_address(account.server.as_str().into());
    ui.set_signed_in(
        match &account.login {
            Login::ApiKey(_) => "Signed in with an API key".into(),
            Login::Password { username, .. } => format!("Signed in as {username}"),
        }
        .into(),
    );
    let user = account
        .listenbrainz
        .as_ref()
        .map_or("", |l| l.user.as_str());
    ui.set_listenbrainz_user(user.into());
    ui.set_api_key("".into());
    ui.set_password("".into());
    ui.set_listenbrainz_token("".into());
    ui.set_problem("".into());
    ui.set_step(Step::Done);
}

/// Shows and saves the account, and opens the library. Sign out waits while
/// it saves, so it can't be overtaken by the save.
fn finish(ui: &AppWindow, browser: &browse::Shared, account: Account, client: Client) {
    show(ui, &account);
    browse::start(ui, browser, client, listenbrainz_client(&account));
    ui.set_saved_note("Saving…".into());
    run(
        ui,
        async move { Ok(store::save(account).await) },
        |ui, saved| {
            let note = match saved {
                Ok(()) => format!("Saved in {}.", store::PLACE),
                Err(e) => format!("Not saved, so the next start asks again: {e}"),
            };
            ui.set_saved_note(note.into());
        },
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // reqwest needs a tokio runtime natively; in the browser it uses fetch.
    #[cfg(not(target_family = "wasm"))]
    let runtime = tokio::runtime::Runtime::new()?;
    #[cfg(not(target_family = "wasm"))]
    let _runtime = runtime.enter();

    start()?.run()?;
    Ok(())
}

/// Builds the window, wires up its screens and loads the saved account in the
/// background, so a store waiting on an unlock prompt doesn't hold it back.
fn start() -> Result<AppWindow, slint::PlatformError> {
    let ui = AppWindow::new()?;
    let pending: Rc<RefCell<Option<(Account, Client)>>> = Rc::default();
    let browser = browse::Shared::default();
    browse::wire(&ui, &browser);

    ui.on_find_server({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            let server = ui.get_address().trim().to_owned();
            let unencrypted = server.to_ascii_lowercase().starts_with("http://");
            run(
                &ui,
                async move { find_server(&server).await },
                move |ui, api_key| {
                    ui.set_api_key_offered(api_key);
                    ui.set_login_method(if api_key { 0 } else { 1 });
                    ui.set_unencrypted(unencrypted);
                    ui.set_step(Step::SignIn);
                },
            );
        }
    });

    ui.on_sign_in({
        let weak = ui.as_weak();
        let pending = pending.clone();
        move || {
            let ui = weak.unwrap();
            let server = ui.get_address().trim().to_owned();
            let login = if ui.get_api_key_offered() && ui.get_login_method() == 0 {
                Login::ApiKey(ui.get_api_key().trim().into())
            } else {
                Login::Password {
                    username: ui.get_username().trim().into(),
                    password: ui.get_password().into(),
                }
            };
            let pending = pending.clone();
            let task = async move {
                let (name, client) = sign_in(&server, &login).await?;
                let account = Account {
                    server,
                    login,
                    listenbrainz: None,
                };
                Ok((name, account, client))
            };
            run(&ui, task, move |ui, (name, account, client)| {
                ui.set_server_name(name.into());
                *pending.borrow_mut() = Some((account, client));
                ui.set_step(Step::Listenbrainz);
            });
        }
    });

    ui.on_connect_listenbrainz({
        let weak = ui.as_weak();
        let pending = pending.clone();
        let browser = browser.clone();
        move || {
            let ui = weak.unwrap();
            let token = ui.get_listenbrainz_token().trim().to_owned();
            let (pending, browser) = (pending.clone(), browser.clone());
            run(&ui, connect_listenbrainz(token), move |ui, listenbrainz| {
                if let Some((mut account, client)) = pending.take() {
                    account.listenbrainz = Some(listenbrainz);
                    finish(ui, &browser, account, client);
                }
            });
        }
    });

    ui.on_skip_listenbrainz({
        let weak = ui.as_weak();
        let pending = pending.clone();
        let browser = browser.clone();
        move || {
            if let Some((account, client)) = pending.take() {
                finish(&weak.unwrap(), &browser, account, client);
            }
        }
    });

    ui.on_sign_out({
        let weak = ui.as_weak();
        let browser = browser.clone();
        move || {
            let ui = weak.unwrap();
            // A disabled button still takes a screen reader's press.
            if ui.get_busy() {
                return;
            }
            let browser = browser.clone();
            run(&ui, store::forget(), move |ui, ()| {
                browse::stop(&browser);
                ui.set_offline("".into());
                ui.set_server_name("".into());
                ui.set_username("".into());
                ui.set_saved_note("".into());
                ui.set_step(Step::Server);
            });
        }
    });

    ui.on_reconnect({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move || resume(&weak.unwrap(), &browser)
    });

    resume(&ui, &browser);

    Ok(ui)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementRoot};
    use slint::Model;
    use slint::platform::PointerEventButton;

    use super::*;

    /// A local HTTP server that answers every request with `answer(request)`.
    fn serve(answer: fn(&str) -> Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = BufReader::new(stream.unwrap());
                let mut request = String::new();
                while stream.read_line(&mut request).unwrap_or(0) > 2 {}
                let _ = stream.get_mut().write_all(&answer(&request));
            }
        });
        address
    }

    fn reply(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend_from_slice(body);
        reply
    }

    fn json(body: &str) -> Vec<u8> {
        reply("200 OK", "application/json", body.as_bytes())
    }

    fn not_found(_: &str) -> Vec<u8> {
        reply("404 Not Found", "text/html", b"<h1>Not found</h1>")
    }

    /// A PNG of one transparent pixel.
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 96, 0, 0, 0, 6, 0,
        2, 48, 129, 208, 47, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    const OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","serverVersion":"0.64.0","openSubsonic":true}}"#;
    const WRONG_PASSWORD: &str = r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#;

    /// The fields of the one album, without its braces.
    const ALBUM: &str = r#""id":"a1","name":"Night Harbour","artist":"Mara Lind","artistId":"r1","coverArt":"al-a1","songCount":2,"duration":470,"created":"2021-01-01T00:00:00Z","year":2021,"genre":"Folk""#;

    /// The fields of the playlist with songs, without its braces.
    const ROAD_TRIP: &str = r#""id":"p1","name":"Road Trip","comment":"For the car","owner":"joe","public":false,"songCount":2,"duration":470,"created":"2024-01-01T00:00:00Z","changed":"2024-02-01T00:00:00Z","coverArt":"pl-p1""#;

    /// A two-pixel-square animated WebP: red, then blue, 100 ms each.
    const MOTION: &[u8] = &[
        82, 73, 70, 70, 132, 0, 0, 0, 87, 69, 66, 80, 86, 80, 56, 88, 10, 0, 0, 0, 2, 0, 0, 0, 1,
        0, 0, 1, 0, 0, 65, 78, 73, 77, 6, 0, 0, 0, 255, 255, 255, 255, 0, 0, 65, 78, 77, 70, 40, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 100, 0, 0, 2, 86, 80, 56, 76, 15, 0, 0, 0, 47, 1,
        64, 0, 0, 7, 16, 237, 143, 254, 7, 34, 162, 255, 1, 0, 65, 78, 77, 70, 40, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 100, 0, 0, 0, 86, 80, 56, 76, 15, 0, 0, 0, 47, 1, 64, 0, 0,
        7, 16, 81, 255, 254, 7, 34, 162, 255, 1, 0,
    ];

    /// A classical album: two movements of a symphony by one orchestra, the
    /// second with its conductor too, an overture whose composer has no artist
    /// id, two encores that belong to no work, and two more movements of the
    /// symphony after them, one of unknown length.
    const CLASSICAL: &str = r#""id":"a2","name":"Symphonies","songCount":4,"duration":1629,"created":"2021-01-01T00:00:00Z","song":[{"id":"c1","isDir":false,"title":"Symphony No. 5: I. Allegro con brio","track":1,"duration":441,"artist":"Wiener Philharmoniker","artistId":"r7","works":[{"name":"Symphony No. 5","musicBrainzId":"w5","composed":{"from":{"year":1804},"to":{"year":1808}}}],"movements":[{"name":"I. Allegro con brio","number":1,"count":4}],"contributors":[{"role":"composer","artist":{"id":"r9","name":"Ludwig van Beethoven"}}],"displayComposer":"Ludwig van Beethoven","recorded":{"from":{"year":1974,"month":3,"day":2},"to":{"year":1974,"month":3,"day":2},"places":["Musikverein"]}},{"id":"c2","isDir":false,"title":"Symphony No. 5: II. Andante con moto","track":2,"duration":588,"artists":[{"id":"r7","name":"Wiener Philharmoniker"},{"id":"r8","name":"Carlos Kleiber"}],"works":[{"name":"Symphony No. 5","musicBrainzId":"w5","composed":{"from":{"year":1804},"to":{"year":1808}}}],"contributors":[{"role":"composer","artist":{"id":"r9","name":"Ludwig van Beethoven"}}],"recorded":{"from":{"year":1974,"month":4},"to":{"year":1974,"month":4},"places":["Musikverein"]}},{"id":"c3","isDir":false,"title":"Egmont Overture","track":3,"duration":500,"artist":"Wiener Philharmoniker","artistId":"r7","works":[{"name":"Egmont Overture","composed":{"from":{"year":1809},"to":{"year":1810}}}],"contributors":[{"role":"composer","artist":{"id":"","name":"Ludwig van Beethoven"}}],"recorded":{"from":{"year":1975},"to":{"year":1975},"places":["Sofiensaal"]}},{"id":"c4","isDir":false,"title":"Encore","track":4,"duration":100,"recorded":{"from":{"year":1975},"to":{"year":1975},"places":["Sofiensaal"]}},{"id":"c6","isDir":false,"title":"Bows","track":5,"duration":30},{"id":"c7","isDir":false,"title":"Symphony No. 5: III. Scherzo","track":6,"works":[{"name":"Symphony No. 5","musicBrainzId":"w5"}],"displayComposer":"Ludwig van Beethoven"},{"id":"c5","isDir":false,"title":"Symphony No. 5: IV. Allegro","track":7,"duration":660,"works":[{"name":"Symphony No. 5","musicBrainzId":"w5"}],"displayComposer":"Ludwig van Beethoven"}]"#;

    /// The ids the server was asked to star.
    static STARRED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the flow test's server answers every request with an error.
    static DOWN: AtomicBool = AtomicBool::new(false);
    /// How many covers the server was asked for, and the first ones' ids.
    static COVERS: AtomicUsize = AtomicUsize::new(0);
    static FIRST_COVERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// How many times the album list was given, and whether it refuses all
    /// but its first part.
    static LISTS: AtomicUsize = AtomicUsize::new(0);
    static LATER_DOWN: AtomicBool = AtomicBool::new(false);

    fn flaky(request: &str) -> Vec<u8> {
        if DOWN.load(Relaxed) {
            reply("503 Service Unavailable", "text/plain", b"down")
        } else {
            subsonic(request)
        }
    }

    /// An OpenSubsonic server without API keys that knows one user, joe, and
    /// 603 albums, more than the client asks for at once. The first has songs, two on two discs; unstarring them is
    /// refused. The second is classical, with a motion cover. The last three
    /// are by or about Bach, and each order puts them differently. It knows
    /// 122 artists, in three indexes, and two playlists, one of them empty.
    fn subsonic(request: &str) -> Vec<u8> {
        let ok = |body: &str| {
            json(&format!(
                r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1",{body}}}}}"#
            ))
        };
        let param = |name: &str| {
            request
                .split(['?', '&', ' '])
                .find_map(|param| param.strip_prefix(name)?.strip_prefix('='))
        };
        let id = param("id");
        if request.contains("getOpenSubsonicExtensions") {
            ok(
                r#""openSubsonicExtensions":[{"name":"songLyrics","versions":[1]},{"name":"fonotecaAlbumMotion","versions":[1]}]"#,
            )
        } else if !request.contains("u=joe&") {
            json(WRONG_PASSWORD)
        } else if request.contains("/rest/getAlbumList2") {
            let number = |name: &str| param(name).and_then(|n| n.parse::<usize>().ok());
            if LATER_DOWN.load(Relaxed) && number("offset").unwrap_or(0) > 0 {
                return reply("503 Service Unavailable", "text/plain", b"down");
            }
            LISTS.fetch_add(1, Relaxed);
            let album = |n: u32| {
                format!(
                    r#"{{"id":"a{n}","name":"Other {n}","coverArt":"al-a{n}","songCount":0,"duration":0,"created":"2020-01-01T00:00:00Z"}}"#
                )
            };
            let albums: Vec<String> = std::iter::once(format!("{{{ALBUM}}}"))
                .chain((2..=600).map(album))
                .chain([
                    r#"{"id":"b1","name":"Sonatas","artist":"Bach Ensemble","year":2010,"songCount":0,"duration":0,"created":"2022-01-01T00:00:00Z"}"#.to_owned(),
                    r#"{"id":"b3","name":"Variations on Bach","artist":"Anna Bach","year":0,"songCount":0,"duration":0,"created":"2023-01-01T00:00:00Z"}"#.to_owned(),
                    r#"{"id":"b2","name":"partitas","artist":"Anna Bach","year":2010,"songCount":0,"duration":0,"created":"2024-01-01T00:00:00Z"}"#.to_owned(),
                ])
                .skip(number("offset").unwrap_or(0))
                .take(number("size").unwrap_or(10))
                .collect();
            ok(&format!(
                r#""albumList2":{{"album":[{}]}}"#,
                albums.join(",")
            ))
        } else if request.contains("/rest/getAlbumMotion") {
            if id == Some("a2") {
                reply("200 OK", "image/webp", MOTION)
            } else {
                json(
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":70,"message":"Not found"}}}"#,
                )
            }
        } else if request.contains("/rest/getAlbum") && id == Some("a2") {
            ok(&format!(r#""album":{{{CLASSICAL}}}"#))
        } else if request.contains("/rest/getAlbum") {
            let songs = r#"[{"id":"s1","isDir":false,"title":"Harbour Lights","track":1,"discNumber":1,"duration":252,"starred":"2024-01-01T00:00:00Z","artists":[{"id":"r1","name":"Mara Lind"}]},{"id":"s2","isDir":false,"title":"Rope and Anchor","track":1,"discNumber":2,"duration":218,"artist":"Mara Lind","artistId":"r1"}]"#;
            ok(&format!(r#""album":{{{ALBUM},"song":{songs}}}"#))
        } else if request.contains("/rest/getArtists") {
            let others: Vec<String> = (1..=120)
                .map(|n| {
                    format!(
                        r#"{{"id":"o{n}","name":"Other {n}","coverArt":"ar-o{n}","albumCount":1}}"#
                    )
                })
                .collect();
            ok(&format!(
                r#""artists":{{"ignoredArticles":"The","index":[{{"name":"A","artist":[{{"id":"r3","name":"Anna Bach","albumCount":2}}]}},{{"name":"M","artist":[{{"id":"r1","name":"Mara Lind","coverArt":"ar-r1","albumCount":1}}]}},{{"name":"O","artist":[{}]}}]}}"#,
                others.join(",")
            ))
        } else if request.contains("/rest/getArtist") {
            ok(&format!(
                r#""artist":{{"id":"r1","name":"Mara Lind","coverArt":"ar-r1","album":[{{{ALBUM}}}]}}"#
            ))
        } else if request.contains("/rest/getPlaylists") {
            ok(&format!(
                r#""playlists":{{"playlist":[{{{ROAD_TRIP}}},{{"id":"p2","name":"Empty","songCount":0,"duration":0,"created":"2024-01-01T00:00:00Z","changed":"2024-01-01T00:00:00Z"}}]}}"#
            ))
        } else if request.contains("/rest/getPlaylist") {
            let songs = r#"[{"id":"s1","isDir":false,"title":"Harbour Lights","album":"Night Harbour","albumId":"a1","duration":252,"starred":"2024-01-01T00:00:00Z","artists":[{"id":"r1","name":"Mara Lind"}]},{"id":"s9","isDir":false,"title":"Fugue","duration":218,"artist":"Anna Bach"}]"#;
            ok(&format!(r#""playlist":{{{ROAD_TRIP},"entry":{songs}}}"#))
        } else if request.contains("/rest/getCoverArt") {
            if COVERS.fetch_add(1, Relaxed) < 6 {
                FIRST_COVERS.lock().unwrap().extend(id.map(str::to_owned));
            }
            reply("200 OK", "image/png", PNG)
        } else if request.contains("/rest/unstar") {
            json(
                r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":50,"message":"Not allowed"}}}"#,
            )
        } else if request.contains("/rest/star") {
            STARRED.lock().unwrap().extend(id.map(str::to_owned));
            json(OK)
        } else {
            json(OK)
        }
    }

    #[test]
    fn finds_servers_and_whether_they_take_api_keys() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let find = |answer: fn(&str) -> Vec<u8>| runtime.block_on(find_server(&serve(answer)));

        assert_eq!(
            find(|_| json(
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","openSubsonicExtensions":[{"name":"apiKeyAuthentication","versions":[1]}]}}"#
            )),
            Ok(true)
        );
        assert_eq!(find(subsonic), Ok(false));
        // A Subsonic server that refuses the call it doesn't know.
        assert_eq!(
            find(|_| json(
                r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"Unknown method"}}}"#
            )),
            Ok(false)
        );
        // One that answers it with a 404, but answers a ping.
        assert_eq!(
            find(|request| if request.contains("/rest/ping") {
                json(WRONG_PASSWORD)
            } else {
                not_found(request)
            }),
            Ok(false)
        );
        let none = find(not_found).unwrap_err();
        assert!(none.starts_with("No music server answered"), "{none}");
    }

    #[test]
    fn sign_in_names_the_server() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = serve(subsonic);
        let login = |username: &str| Login::Password {
            username: username.into(),
            password: "pw".into(),
        };
        let name = |username| {
            runtime
                .block_on(sign_in(&server, &login(username)))
                .map(|(name, _)| name)
        };
        assert_eq!(name("joe"), Ok("navidrome 0.64.0".to_owned()));
        let wrong = name("ann").unwrap_err();
        assert!(wrong.contains("40"), "{wrong}");
    }

    /// Ready on its second poll, so the event loop runs other tasks in between.
    struct YieldNow(bool);

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                return Poll::Ready(());
            }
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }

    async fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            YieldNow(false).await;
        }
    }

    fn button(ui: &AppWindow, label: &str) -> ElementHandle {
        let label = label.to_owned();
        let button = ui
            .root_element()
            .query_descendants()
            .match_accessible_role(AccessibleRole::Button)
            .match_predicate({
                let label = label.clone();
                move |button| button.accessible_label().is_some_and(|l| l == label)
            })
            .find_first();
        button.unwrap_or_else(|| panic!("no {label} button"))
    }

    fn click(ui: &AppWindow, label: &str) {
        button(ui, label).invoke_accessible_default_action();
    }

    /// The only test that may start Slint or touch the store: both are
    /// global to the process.
    #[test]
    fn onboarding_browsing_and_sign_out() {
        i_slint_backend_testing::init_integration_test_with_system_time();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _runtime = runtime.enter();
        let server = serve(flaky);

        slint::spawn_local(async move {
            let ui = start().unwrap();
            // Covers are fetched for the tiles in view, so the window needs a size.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            wait_for("the server step", || ui.get_step() == Step::Server).await;

            ui.set_address("http://127.0.0.1:1".into());
            click(&ui, "Continue");
            wait_for("the address check", || !ui.get_busy()).await;
            assert!(ui.get_problem().starts_with("No music server answered"));

            ui.set_address(server.as_str().into());
            click(&ui, "Continue");
            wait_for("the sign-in step", || ui.get_step() == Step::SignIn).await;
            assert!(ui.get_unencrypted());
            assert!(!ui.get_api_key_offered());

            ui.set_username("ann".into());
            ui.set_password("pw".into());
            click(&ui, "Sign in");
            wait_for("the refused sign-in", || !ui.get_busy()).await;
            assert!(ui.get_problem().contains("40"), "{}", ui.get_problem());

            ui.set_username("joe".into());
            click(&ui, "Sign in");
            wait_for("the ListenBrainz step", || {
                ui.get_step() == Step::Listenbrainz
            })
            .await;
            assert_eq!(ui.get_server_name(), "navidrome 0.64.0");

            // The library is asked for once signed in, and fails.
            DOWN.store(true, Relaxed);
            click(&ui, "Skip");
            assert_eq!(ui.get_step(), Step::Done);
            assert!(ui.get_busy(), "sign out waits while it saves");
            click(&ui, "Account");
            assert_eq!(ui.get_page(), Page::Account);
            click(&ui, "Sign out");
            wait_for("the save", || !ui.get_busy()).await;
            assert_eq!(
                ui.get_step(),
                Step::Done,
                "sign out during the save is ignored"
            );
            assert_eq!(ui.get_saved_note(), "Saved in your system's secret store.");
            assert_eq!(ui.get_password(), "");
            let saved = store::load().await.expect("saved");
            assert!(
                matches!(saved.login, Login::Password { ref username, .. } if username == "joe")
            );
            assert!(saved.listenbrainz.is_none());

            // Showing the albums asks again, and Try again once more.
            click(&ui, "Albums");
            wait_for("the failed library", || ui.get_page_problem() != "").await;
            assert_eq!(ui.get_tiles().row_count(), 0);
            assert!(ui.get_more_tiles());
            DOWN.store(false, Relaxed);
            LATER_DOWN.store(true, Relaxed);
            ui.invoke_show_more();
            assert_eq!(ui.get_page_problem(), "");
            wait_for("the albums", || ui.get_tiles().row_count() == 100).await;
            assert_eq!(ui.get_page(), Page::Albums);
            assert!(ui.get_more_tiles());
            let tile = ui.get_tiles().row_data(0).unwrap();
            assert_eq!(tile.link, "album:a1");
            assert_eq!(tile.names.row_data(0).unwrap().link, "artist:r1");
            // A window that draws creates the grid, which asks for the covers
            // in view; here a query over the elements creates it.
            ui.root_element().query_descendants().find_all();
            wait_for("a cover", || {
                ui.get_tiles().row_data(0).unwrap().cover.size().width > 0
            })
            .await;
            // Only the tiles on screen and a screen below it get theirs.
            let later = Instant::now() + Duration::from_millis(300);
            wait_for("the covers in view", || Instant::now() > later).await;
            let asked = COVERS.load(Relaxed);
            assert!((4..40).contains(&asked), "{asked} covers asked for");
            // Top first: the first tile's is among the first fetches.
            let first = FIRST_COVERS.lock().unwrap().clone();
            assert!(first.contains(&"al-a1".to_owned()), "{first:?}");
            let links = |ui: &AppWindow| -> Vec<String> {
                ui.get_tiles().iter().map(|tile| tile.link.into()).collect()
            };
            let ids = |ids: &[&str]| -> Vec<String> {
                ids.iter().map(|id| format!("album:{id}")).collect()
            };
            let sort = |sort: i32, descending: bool| {
                ui.set_album_sort(sort);
                ui.set_album_descending(descending);
                ui.invoke_sort_albums();
            };
            // The library's second part fails at first. Sorting meanwhile
            // sorts the first part, and the rest takes its place once it's in.
            wait_for("the failed second part", || ui.get_page_problem() != "").await;
            sort(2, true);
            wait_for("the second part failing again", || {
                ui.get_page_problem() != ""
            })
            .await;
            assert_eq!(links(&ui)[0], "album:a1");
            LATER_DOWN.store(false, Relaxed);
            ui.invoke_show_more();
            wait_for("the rest in order", || {
                links(&ui).starts_with(&ids(&["b2", "b3", "b1", "a1"]))
            })
            .await;
            sort(0, false);
            // What scrolling near the end asks for comes from the library.
            assert_eq!(LISTS.load(Relaxed), 2);
            ui.invoke_show_more();
            assert_eq!(ui.get_tiles().row_count(), 200);
            assert!(ui.get_more_tiles());

            // Sorting, reversing and filtering happen here, at once, without
            // asking the server again.
            let lists = LISTS.load(Relaxed);
            sort(2, true);
            assert_eq!(links(&ui)[..4], ids(&["b2", "b3", "b1", "a1"]));
            assert_eq!(ui.get_tiles().row_count(), 100);
            sort(0, true);
            assert_eq!(links(&ui)[..3], ids(&["b3", "b1", "b2"]));
            sort(3, false);
            assert_eq!(links(&ui), ids(&["b2", "b1", "a1"]));
            assert!(!ui.get_more_tiles());
            let filter = |typed: &str| {
                ui.set_album_filter(typed.into());
                ui.invoke_filter_albums();
            };
            filter("bach");
            for (order, descending, expected) in [
                (3, false, &["b2", "b1"][..]),
                (3, true, &["b1", "b2"]),
                (0, false, &["b2", "b1", "b3"]),
                (0, true, &["b3", "b1", "b2"]),
                (1, false, &["b2", "b3", "b1"]),
                (2, false, &["b1", "b3", "b2"]),
                (2, true, &["b2", "b3", "b1"]),
            ] {
                sort(order, descending);
                wait_for("the sorted matches", || links(&ui) == ids(expected)).await;
            }
            // Every word, at the start of a word of the name or the artist.
            filter("BACH part");
            wait_for("one match", || links(&ui) == ids(&["b2"])).await;
            filter("artitas");
            wait_for("no matches", || ui.get_tiles().row_count() == 0).await;
            assert!(!ui.get_loading());
            filter("");
            sort(0, false);
            wait_for("all albums", || ui.get_tiles().row_count() == 100).await;
            assert_eq!(links(&ui)[0], "album:a1");
            assert_eq!(LISTS.load(Relaxed), lists);

            // Once a tile is selected, a click selects another, or lets a
            // selected one go, instead of opening it.
            let selected = |ui: &AppWindow| -> Vec<usize> {
                let tiles = ui.get_tiles();
                (0..tiles.row_count())
                    .filter(|row| tiles.row_data(*row).unwrap().selected)
                    .collect()
            };
            ui.invoke_select_tile(2);
            click(&ui, "Night Harbour");
            assert_eq!(ui.get_page(), Page::Albums);
            assert_eq!(selected(&ui), [0, 2]);
            assert_eq!(ui.get_selected_tiles(), 2);
            click(&ui, "Night Harbour");
            assert_eq!(selected(&ui), [2]);
            assert_eq!(ui.get_selected_tiles(), 1);
            // A name on a tile selects the tile too, and so does a right-click.
            click(&ui, "Mara Lind");
            assert_eq!(ui.get_page(), Page::Albums);
            assert_eq!(selected(&ui), [0, 2]);
            click(&ui, "Clear selection");
            assert!(selected(&ui).is_empty());
            button(&ui, "Night Harbour")
                .single_click(PointerEventButton::Right)
                .await;
            assert_eq!(selected(&ui), [0]);
            button(&ui, "Mara Lind")
                .single_click(PointerEventButton::Right)
                .await;
            assert!(selected(&ui).is_empty());
            ui.invoke_select_tile(1);
            ui.invoke_clear_selection();
            assert!(selected(&ui).is_empty());
            assert_eq!(ui.get_selected_tiles(), 0);
            // The selection lasts through sorting, filtering and other pages.
            let picked = |ui: &AppWindow| -> Vec<String> {
                let links = links(ui);
                selected(ui)
                    .into_iter()
                    .map(|row| links[row].clone())
                    .collect()
            };
            filter("bach");
            wait_for("the Bach albums", || links(&ui) == ids(&["b2", "b1", "b3"])).await;
            ui.invoke_select_tile(1);
            sort(0, true);
            assert_eq!(links(&ui), ids(&["b3", "b1", "b2"]));
            assert_eq!(picked(&ui), ids(&["b1"]));
            filter("BACH part");
            wait_for("one match", || links(&ui) == ids(&["b2"])).await;
            assert!(picked(&ui).is_empty());
            assert_eq!(ui.get_selected_tiles(), 1);
            filter("bach");
            wait_for("the Bach albums again", || links(&ui).len() == 3).await;
            assert_eq!(picked(&ui), ids(&["b1"]));
            click(&ui, "Account");
            assert_eq!(ui.get_page(), Page::Account);
            assert_eq!(ui.get_selected_tiles(), 1);
            ui.invoke_back();
            wait_for("the albums page", || links(&ui).len() == 3).await;
            assert_eq!(picked(&ui), ids(&["b1"]));
            assert_eq!(ui.get_selected_tiles(), 1);
            ui.invoke_clear_selection();
            assert!(picked(&ui).is_empty());
            assert_eq!(ui.get_selected_tiles(), 0);
            filter("");
            sort(0, false);
            wait_for("all albums again", || ui.get_tiles().row_count() == 100).await;

            // The artists arrive at once, and Try again asks again when they
            // don't. They show a page at a time, in the server's order.
            DOWN.store(true, Relaxed);
            click(&ui, "Artists");
            wait_for("the failed artists", || ui.get_page_problem() != "").await;
            assert_eq!(ui.get_page(), Page::Artists);
            DOWN.store(false, Relaxed);
            ui.invoke_show_more();
            wait_for("the artists", || ui.get_tiles().row_count() == 100).await;
            assert!(ui.get_more_tiles());
            let tile = ui.get_tiles().row_data(0).unwrap();
            assert_eq!(
                (tile.title.as_str(), tile.link.as_str()),
                ("Anna Bach", "artist:r3")
            );
            assert_eq!(tile.names.row_data(0).unwrap().label, "2 albums");
            let tile = ui.get_tiles().row_data(1).unwrap();
            assert_eq!(tile.names.row_data(0).unwrap().label, "1 album");
            ui.invoke_show_more();
            assert_eq!(ui.get_tiles().row_count(), 122);
            assert!(!ui.get_more_tiles());
            // Their filter is their own.
            let filter_artists = |typed: &str| {
                ui.set_artist_filter(typed.into());
                ui.invoke_filter_artists();
            };
            filter_artists("LIND");
            wait_for("one artist", || links(&ui) == ["artist:r1"]).await;
            assert_eq!(ui.get_album_filter(), "");
            filter_artists("lindsay");
            wait_for("no artists", || ui.get_tiles().row_count() == 0).await;
            filter_artists("");
            wait_for("all artists", || ui.get_tiles().row_count() == 100).await;
            // They join the albums' selection, and are kept for the session.
            ui.invoke_select_tile(1);
            click(&ui, "Albums");
            assert_eq!(ui.get_selected_tiles(), 1);
            click(&ui, "Artists");
            assert_eq!(ui.get_tiles().row_count(), 100);
            assert!(ui.get_tiles().row_data(1).unwrap().selected);
            ui.invoke_clear_selection();
            click(&ui, "Mara Lind");
            wait_for("her page", || {
                ui.get_page() == Page::Artist && ui.get_tiles().row_count() == 1
            })
            .await;
            click(&ui, "Albums");
            assert_eq!(ui.get_tiles().row_count(), 100);

            // The playlists were asked for on sign-in, while the server was
            // down. Their page asks again, and so does Try again.
            assert_eq!(ui.get_sections().row_count(), 4);
            DOWN.store(true, Relaxed);
            click(&ui, "Playlists");
            wait_for("the failed playlists", || ui.get_page_problem() != "").await;
            assert_eq!(ui.get_page(), Page::Playlists);
            assert_eq!(ui.get_section(), 2);
            DOWN.store(false, Relaxed);
            click(&ui, "Try again");
            wait_for("the playlists", || ui.get_tiles().row_count() == 2).await;
            let tile = ui.get_tiles().row_data(0).unwrap();
            assert_eq!(
                (tile.title.as_str(), tile.link.as_str()),
                ("Road Trip", "playlist:p1")
            );
            assert_eq!(tile.names.row_data(0).unwrap().label, "2 tracks");
            // The sidebar lists them under their own heading.
            let strings = |model: slint::ModelRc<slint::SharedString>| -> Vec<String> {
                model.iter().map(Into::into).collect()
            };
            assert_eq!(
                strings(ui.get_sections()),
                [
                    "Albums",
                    "Artists",
                    "Playlists",
                    "Road Trip",
                    "Empty",
                    "Account"
                ]
            );
            assert_eq!(
                strings(ui.get_section_groups()),
                [
                    "Library",
                    "Library",
                    "Library",
                    "Playlists",
                    "Playlists",
                    ""
                ]
            );
            ui.invoke_show_section(3);
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Playlist);
            assert_eq!(ui.get_section(), 3);
            assert_eq!(ui.get_header_title(), "Road Trip");
            assert_eq!(ui.get_header_facts(), "For the car · 2 tracks, 8 minutes");
            // Each track with its artists, then its album.
            let tracks: Vec<Track> = ui.get_tracks().iter().collect();
            let links = |track: &Track| -> Vec<String> {
                track.names.iter().map(|name| name.link.into()).collect()
            };
            assert_eq!(tracks[0].title, "Harbour Lights");
            assert_eq!(links(&tracks[0]), ["artist:r1", "album:a1"]);
            assert_eq!(tracks[0].split, 1);
            assert!(tracks[0].loved);
            assert_eq!(links(&tracks[1]), [""]);
            assert!(!tracks[1].loved);
            click(&ui, "Account");
            assert_eq!(ui.get_section(), 5);
            // Shown again at once, while they are asked for again.
            click(&ui, "Playlists");
            assert_eq!(ui.get_tiles().row_count(), 2);
            assert!(!ui.get_loading());
            click(&ui, "Albums");
            assert_eq!(ui.get_tiles().row_count(), 100);

            click(&ui, "Night Harbour");
            wait_for("the album", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Album);
            assert_eq!(ui.get_section(), -1, "an album has no sidebar entry");
            assert_eq!(ui.get_header_title(), "Night Harbour");
            assert_eq!(ui.get_header_facts(), "2021 · Folk · 2 tracks, 8 minutes");
            let tracks = ui.get_tracks();
            assert_eq!(tracks.row_data(0).unwrap().title, "Harbour Lights");
            assert_eq!(tracks.row_data(1).unwrap().title, "Rope and Anchor");
            assert_eq!(tracks.row_data(0).unwrap().duration, "4:12");
            assert!(tracks.row_data(0).unwrap().loved);
            assert!(!tracks.row_data(1).unwrap().loved);
            assert!(ui.get_can_go_back());

            click(&ui, "Love");
            assert!(ui.get_tracks().row_data(1).unwrap().loved);
            wait_for("the star", || *STARRED.lock().unwrap() == ["s2"]).await;
            assert_eq!(ui.get_page_problem(), "");
            click(&ui, "Loved");
            wait_for("the refused unstar", || ui.get_page_problem() != "").await;
            assert!(ui.get_tracks().iter().all(|track| track.loved));

            click(&ui, "Mara Lind");
            wait_for("the artist", || {
                ui.get_page() == Page::Artist && ui.get_tiles().row_count() == 1
            })
            .await;
            assert_eq!(ui.get_header_title(), "Mara Lind");
            assert_eq!(
                ui.get_tiles()
                    .row_data(0)
                    .unwrap()
                    .names
                    .row_data(0)
                    .unwrap()
                    .label,
                "2021"
            );

            click(&ui, "Back");
            wait_for("the album again", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Album);
            assert!(ui.get_can_go_forward());
            click(&ui, "Forward");
            wait_for("the artist again", || ui.get_tiles().row_count() == 1).await;
            assert_eq!(ui.get_page(), Page::Artist);
            assert!(!ui.get_can_go_forward());
            ui.invoke_forward();
            assert_eq!(ui.get_page(), Page::Artist);

            // A page opened from back in the history drops the pages ahead.
            click(&ui, "Back");
            wait_for("the album once more", || ui.get_tracks().row_count() == 2).await;
            click(&ui, "Albums");
            assert_eq!(ui.get_tiles().row_count(), 100, "the library is kept");
            assert_eq!(LISTS.load(Relaxed), lists);
            assert!(!ui.get_can_go_forward());
            click(&ui, "Back");
            wait_for("the album before them", || ui.get_tracks().row_count() == 2).await;

            ui.invoke_open("album:a2".into());
            wait_for("the classical album", || ui.get_tracks().row_count() == 7).await;
            let tracks: Vec<Track> = ui.get_tracks().iter().collect();
            let links = |names: &slint::ModelRc<Name>| -> Vec<String> {
                names.iter().map(|name| name.link.to_string()).collect()
            };
            let work = |row: usize| {
                let track = &tracks[row];
                let names = links(&track.work_names);
                (
                    track.work.as_str(),
                    names,
                    track.work_split,
                    track.work_duration.as_str(),
                )
            };
            let row = |row: usize| {
                let track = &tracks[row];
                let names = links(&track.names);
                (
                    track.title.as_str(),
                    names,
                    track.split,
                    track.part,
                    track.alone,
                )
            };
            let to = |links: &[&str]| -> Vec<String> {
                links.iter().map(|link| link.to_string()).collect()
            };
            let none = || ("", to(&[]), 0, "");
            let performance = |row: usize| {
                let track = &tracks[row];
                (
                    track.work_years.as_str(),
                    track.work_performed.as_str(),
                    track.years.as_str(),
                    track.performed.as_str(),
                )
            };
            assert_eq!(
                work(0),
                (
                    "Symphony No. 5",
                    to(&["artist:r9", "artist:r7"]),
                    1,
                    "17:09"
                )
            );
            assert_eq!(row(0), ("I. Allegro con brio", to(&[]), 0, true, false));
            assert_eq!(
                performance(0),
                (
                    "1804 – 1808",
                    "Performed 2 Mar 1974 – Apr 1974, Musikverein",
                    "",
                    ""
                )
            );
            assert_eq!(performance(1), ("", "", "", ""));
            assert_eq!(work(1), none());
            assert_eq!(
                row(1),
                (
                    "II. Andante con moto",
                    to(&["artist:r7", "artist:r8"]),
                    0,
                    true,
                    false
                )
            );
            assert_eq!(work(2), none());
            assert_eq!(
                row(2),
                ("Egmont Overture", to(&["", "artist:r7"]), 1, false, true)
            );
            assert_eq!(
                performance(2),
                ("", "", "1809 – 1810", "Performed 1975, Sofiensaal")
            );
            assert_eq!(row(3), ("Encore", to(&[]), 0, false, false));
            assert_eq!(performance(3), ("", "", "", ""));
            assert_eq!(row(4), ("Bows", to(&[]), 0, false, false));
            assert_eq!(work(5), ("Symphony No. 5", to(&[""]), 1, ""));
            assert_eq!(performance(5), ("", "", "", ""));
            assert_eq!(row(5), ("III. Scherzo", to(&[]), 0, true, false));
            assert_eq!(work(6), none());
            assert_eq!(row(6), ("IV. Allegro", to(&[]), 0, true, false));
            // Red or blue, as far as encoding lets them be.
            let colour = || {
                let picture = ui.get_header_image().to_rgba8()?;
                let pixel = picture.as_slice()[0];
                Some(if pixel.r > pixel.b { "red" } else { "blue" })
            };
            wait_for("the motion cover", || colour() == Some("red")).await;
            wait_for("its next frame", || colour() == Some("blue")).await;
            wait_for("its first frame again", || colour() == Some("red")).await;
            // Leaving the album stops it.
            click(&ui, "Back");
            wait_for("the album before it", || ui.get_tracks().row_count() == 2).await;
            let later = Instant::now() + Duration::from_millis(300);
            wait_for("a few frames' time", || Instant::now() > later).await;
            assert_ne!(ui.get_header_image().size().width, 2);

            // A second start finds the saved account, with its server down.
            DOWN.store(true, Relaxed);
            let again = start().unwrap();
            wait_for("the offline note", || again.get_offline() != "").await;
            assert_eq!(again.get_step(), Step::Done);
            DOWN.store(false, Relaxed);
            click(&again, "Try again");
            wait_for("the albums after trying again", || {
                again.get_tiles().row_count() == 100
            })
            .await;
            assert_eq!(again.get_offline(), "");
            drop(again);

            click(&ui, "Account");
            click(&ui, "Sign out");
            wait_for("the server step", || ui.get_step() == Step::Server).await;
            assert_eq!(ui.get_username(), "");
            assert!(store::load().await.is_none());
            store::forget().await.expect("forgetting twice is fine");

            slint::quit_event_loop().unwrap();
        })
        .unwrap();
        slint::run_event_loop_until_quit().unwrap();
    }
}
