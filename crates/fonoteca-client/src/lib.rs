slint::include_modules!();

#[cfg(target_os = "android")]
mod bit_perfect;
mod browse;
#[cfg(not(target_family = "wasm"))]
mod eq_graph;
#[cfg(not(target_family = "wasm"))]
mod headphones;
#[cfg(target_os = "android")]
mod media;
#[cfg(target_os = "android")]
mod network;
#[cfg(not(target_family = "wasm"))]
mod opra;
#[cfg(not(target_family = "wasm"))]
mod play;
mod prefs;
#[cfg(not(target_family = "wasm"))]
mod quality;
#[cfg(not(target_family = "wasm"))]
mod queue;
#[cfg(not(target_family = "wasm"))]
mod queue_drag;
#[cfg(not(target_family = "wasm"))]
mod sound;
mod store;
#[cfg(not(target_family = "wasm"))]
mod waveform;
mod zone;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use opensubsonic::{Auth, Client};
use serde::{Deserialize, Serialize};

/// What the onboarding sets up, kept by [`store`].
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    /// Kept in an entry of its own, so that it outlives the login. Earlier
    /// versions kept it here, and it is only read from there, to move it.
    #[serde(default, skip_serializing)]
    server: String,
    login: Login,
    /// Where earlier versions kept the token. It is only read, to move it
    /// to its own entry, and never written back.
    #[serde(default, rename = "listenbrainz", skip_serializing)]
    legacy_listenbrainz: Option<ListenBrainz>,
}

#[derive(Clone, Serialize, Deserialize)]
enum Login {
    ApiKey(String),
    Password { username: String, password: String },
}

/// The user's ListenBrainz token, kept by [`store`] apart from the account.
#[derive(Clone, Serialize, Deserialize)]
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
        let mut account = store::load().await;
        let migrated = match account.as_mut() {
            Some(account) => split_legacy_account(account).await,
            None => Ok(()),
        };
        let listenbrainz = store::load_listenbrainz().await;
        let Some(ui) = weak.upgrade() else { return };
        ui.set_listenbrainz_user(listenbrainz.map_or("".into(), |l| l.user.into()));
        if let Err(e) = migrated {
            ui.set_saved_note(
                format!("Your server and ListenBrainz token are still kept with the login: {e}")
                    .into(),
            );
        }
        let Some(account) = account else {
            ask_for_login(&ui, store::load_server().await);
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
                browse::start(&ui, &browser, client);
            }
            Err(e) => ui.set_offline(format!("Your server didn't answer: {e}").into()),
        }
    })
    .expect("the event loop is running");
}

/// Changes the password of the account signed in at `server`, as
/// `username`, or with an API key when that is empty. When it signs in with
/// that password, also saves the new one where the account is saved, and
/// returns a client signed in with it, since the one in use goes on sending
/// the old one. Returns the word for the account page.
async fn change_own_password(
    client: Arc<Client>,
    server: String,
    username: String,
    password: String,
) -> Result<(Option<Client>, String), String> {
    let name = match username.is_empty() {
        true => {
            let info = client.token_info().await;
            info.map_err(|e| browse::refusal(&e))?.username
        }
        false => username.clone(),
    };
    client
        .change_password(&name, &password)
        .await
        .map_err(|e| browse::refusal(&e))?;
    if username.is_empty() {
        return Ok((None, "Password changed.".into()));
    }
    let login = Login::Password { username, password };
    let (_, fresh) = sign_in(&server, &login).await?;
    let note = match store::load().await {
        Some(mut account) => {
            account.login = login;
            match store::save(account).await {
                Ok(()) => format!("Password changed, and saved in {}.", store::PLACE),
                Err(e) => {
                    format!("Password changed, but not saved, so the next start asks for it: {e}")
                }
            }
        }
        None => "Password changed, but not saved, so the next start asks for it.".into(),
    };
    Ok((Some(fresh), note))
}

/// With the server of a saved login, goes on to its sign-in step, which
/// takes the login only; without one, asks for a server.
fn ask_for_login(ui: &AppWindow, server: Option<String>) {
    let Some(server) = server else {
        ui.set_step(Step::Server);
        return;
    };
    ui.set_address(server.into());
    ui.set_step(Step::Server);
    ui.invoke_find_server();
}

/// Moves what earlier versions kept in the account's entry, the server and
/// the ListenBrainz token, to entries of their own, unless the token's is
/// already there, and saves the account without them. The token goes
/// first, so that a failure in between leaves it in both places and the
/// next start tries again.
async fn split_legacy_account(account: &mut Account) -> Result<(), String> {
    let legacy_token = account.legacy_listenbrainz.take();
    let server_is_apart = store::load_server().await.is_some();
    if legacy_token.is_none() && server_is_apart {
        return Ok(());
    }
    if let Some(legacy) = legacy_token
        && store::load_listenbrainz().await.is_none()
    {
        store::save_listenbrainz(legacy).await?;
    }
    store::save(account.clone()).await
}

/// Asks ListenBrainz whether it accepts `token` and, when it does, keeps it.
async fn connect_listenbrainz(token: String) -> Result<ListenBrainz, String> {
    let client = listenbrainz::Client::new(token.clone()).map_err(|e| e.to_string())?;
    let Some(user) = client.validate_token().await.map_err(|e| e.to_string())? else {
        return Err("ListenBrainz doesn't accept this token.".into());
    };
    let listenbrainz = ListenBrainz { token, user };
    store::save_listenbrainz(listenbrainz.clone())
        .await
        .map_err(|e| {
            format!(
                "ListenBrainz accepts this token, but it couldn't be saved in {}: {e}",
                store::PLACE
            )
        })?;
    Ok(listenbrainz)
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

/// Stops the library and clears what the screens know of the account.
fn signed_out(ui: &AppWindow, browser: &browse::Shared) {
    browse::stop(browser);
    ui.set_offline("".into());
    ui.set_server_name("".into());
    ui.set_username("".into());
    ui.set_saved_note("".into());
}

/// Shows the done screen for `account`, and clears the secrets from the
/// screens before it.
fn show(ui: &AppWindow, account: &Account) {
    ui.set_address(account.server.as_str().into());
    let (signed_in, username) = match &account.login {
        Login::ApiKey(_) => ("Signed in with an API key".into(), ""),
        Login::Password { username, .. } => (format!("Signed in as {username}"), username.as_str()),
    };
    ui.set_signed_in(signed_in.into());
    // The home page's welcome names them.
    ui.set_username(username.into());
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
    browse::start(ui, browser, client);
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

/// Runs the app until its window closes.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    // reqwest needs a tokio runtime natively; in the browser it uses fetch.
    #[cfg(not(target_family = "wasm"))]
    let runtime = tokio::runtime::Runtime::new()?;
    #[cfg(not(target_family = "wasm"))]
    let _runtime = runtime.enter();

    start()?.run()?;
    Ok(())
}

/// The Android app's entry point, which android-activity calls on its own
/// thread once the activity starts.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    // reqwest checks certificates with Android's own verifier, which calls
    // into Java and so needs the VM and the activity.
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, app.activity_as_ptr().cast()) };
        rustls_platform_verifier::android::init_with_env(env, activity)
    })
    .expect("Android's certificate verifier");
    vm.attach_current_thread(|env| {
        let activity =
            unsafe { jni::objects::JObject::from_raw(env, app.activity_as_ptr().cast()) };
        media::init(env, &activity)
    })
    .expect("Android's media controls");
    // The time zone tells the radio page the country for its local shelf.
    let zone = vm.attach_current_thread(|env| -> jni::errors::Result<String> {
        let zone = env
            .call_static_method(
                jni::jni_str!("java/util/TimeZone"),
                jni::jni_str!("getDefault"),
                jni::jni_sig!("()Ljava/util/TimeZone;"),
                &[],
            )?
            .l()?;
        let id = env
            .call_method(
                &zone,
                jni::jni_str!("getID"),
                jni::jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let id = env.cast_local::<jni::objects::JString>(id)?;
        Ok(id.mutf8_chars(env)?.to_string())
    });
    if let Ok(zone) = zone {
        let _ = zone::ANDROID.set(zone);
    }
    if let Some(folder) = app.internal_data_path() {
        let _ = prefs::FOLDER.set(folder);
    }
    slint::android::init(app).expect("Slint's Android backend");
    main().expect("the app");
}

/// Builds the window, wires up its screens and loads the saved account in the
/// background, so a store waiting on an unlock prompt doesn't hold it back.
fn start() -> Result<AppWindow, slint::PlatformError> {
    let ui = AppWindow::new()?;
    // Every press there is a finger's, the first one too.
    #[cfg(target_os = "android")]
    ui.global::<Input>().set_finger(true);
    let pending: Rc<RefCell<Option<(Account, Client)>>> = Rc::default();
    let browser = browse::Shared::default();
    browse::wire(&ui, &browser);
    #[cfg(not(target_family = "wasm"))]
    play::wire(&ui, &browser);
    #[cfg(target_os = "android")]
    media::wire(&ui);

    // On a phone-sized screen everything is drawn a quarter larger than the
    // browser's pixel ratio. This runs whenever the width or the scale
    // changes, since the backend sets its own scale at startup and on zoom
    // without laying the window out again.
    #[cfg(target_family = "wasm")]
    ui.on_resized({
        let weak = ui.as_weak();
        move || {
            use slint::platform::WindowEvent;
            let ui = weak.unwrap();
            let window = ui.window();
            let Some(page) = web_sys::window() else {
                return;
            };
            let page_width = page.inner_width().ok().and_then(|width| width.as_f64());
            let ratio = page.device_pixel_ratio() as f32;
            let scale = if page_width.is_some_and(|width| width < 600.0) {
                ratio * 1.25
            } else {
                ratio
            };
            if scale != window.scale_factor() {
                window.dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                });
            }
            window.dispatch_event(WindowEvent::Resized {
                size: window.size().to_logical(scale),
            });
        }
    });

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
        let browser = browser.clone();
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
            let (pending, browser) = (pending.clone(), browser.clone());
            let task = async move {
                let (name, client) = sign_in(&server, &login).await?;
                let account = Account {
                    server,
                    login,
                    legacy_listenbrainz: None,
                };
                Ok((name, account, client))
            };
            run(&ui, task, move |ui, (name, account, client)| {
                ui.set_server_name(name.into());
                // The token outlives the account it was entered for.
                if ui.get_listenbrainz_user() != "" {
                    finish(ui, &browser, account, client);
                    return;
                }
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
                ui.set_listenbrainz_user(listenbrainz.user.into());
                ui.set_listenbrainz_token("".into());
                if let Some((account, client)) = pending.take() {
                    finish(ui, &browser, account, client);
                }
            });
        }
    });

    ui.on_remove_listenbrainz({
        let weak = ui.as_weak();
        move || {
            let ui = weak.unwrap();
            // A disabled button still takes a screen reader's press.
            if ui.get_busy() {
                return;
            }
            run(&ui, store::forget_listenbrainz(), |ui, ()| {
                ui.set_listenbrainz_user("".into());
                ui.set_listenbrainz_token("".into());
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
                signed_out(ui, &browser);
                ask_for_login(ui, Some(ui.get_address().into()));
            });
        }
    });

    ui.on_change_server({
        let weak = ui.as_weak();
        let browser = browser.clone();
        move || {
            let ui = weak.unwrap();
            // A disabled button still takes a screen reader's press.
            if ui.get_busy() {
                return;
            }
            let browser = browser.clone();
            let task = async {
                store::forget().await?;
                store::forget_server().await
            };
            run(&ui, task, move |ui, ()| {
                signed_out(ui, &browser);
                ui.set_address("".into());
                ui.set_step(Step::Server);
            });
        }
    });

    ui.on_change_own_password({
        let (weak, browser) = (ui.as_weak(), browser.clone());
        move |password| {
            let ui = weak.unwrap();
            let Some(client) = browse::client(&browser) else {
                return;
            };
            let browser = browser.clone();
            let (server, username) = (ui.get_address().into(), ui.get_username().into());
            let task = change_own_password(client, server, username, password.into());
            run(&ui, task, move |ui, (fresh, note)| {
                if let Some(fresh) = fresh {
                    browse::swap(&browser, fresh);
                }
                ui.set_saved_note(note.into());
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
    use std::collections::BTreeSet;
    use std::io::{BufRead, BufReader, Write};
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementRoot};
    use slint::Model;
    use slint::platform::{PointerEventButton, WindowEvent};

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

    /// The playlist of the classical album's songs.
    const WORKS: &str = r#""id":"p5","name":"Symphony","songCount":7,"duration":3000,"created":"2024-01-01T00:00:00Z","changed":"2024-02-01T00:00:00Z""#;
    /// A classical album: two movements of a symphony by one orchestra, the
    /// second with its conductor too, an overture whose composer has no artist
    /// id, two encores that belong to no work, and two more movements of the
    /// symphony after them, one of unknown length.
    const CLASSICAL: &str = r#""id":"a2","name":"Symphonies","songCount":4,"duration":1629,"created":"2021-01-01T00:00:00Z","song":[{"id":"c1","isDir":false,"title":"Symphony No. 5: I. Allegro con brio","track":1,"duration":441,"artist":"Wiener Philharmoniker","artistId":"r7","works":[{"name":"Symphony No. 5","musicBrainzId":"w5","composed":{"from":{"year":1804},"to":{"year":1808}}}],"movements":[{"name":"I. Allegro con brio","number":1,"count":4}],"contributors":[{"role":"composer","artist":{"id":"r9","name":"Ludwig van Beethoven"}}],"displayComposer":"Ludwig van Beethoven","recorded":{"from":{"year":1974,"month":3,"day":2},"to":{"year":1974,"month":3,"day":2},"places":["Musikverein"]}},{"id":"c2","isDir":false,"title":"Symphony No. 5: II. Andante con moto","track":2,"duration":588,"artists":[{"id":"r7","name":"Wiener Philharmoniker"},{"id":"r8","name":"Carlos Kleiber"}],"works":[{"name":"Symphony No. 5","musicBrainzId":"w5","composed":{"from":{"year":1804},"to":{"year":1808}}}],"contributors":[{"role":"composer","artist":{"id":"r9","name":"Ludwig van Beethoven"}}],"recorded":{"from":{"year":1974,"month":4},"to":{"year":1974,"month":4},"places":["Musikverein"]}},{"id":"c3","isDir":false,"title":"Egmont Overture","track":3,"duration":500,"artist":"Wiener Philharmoniker","artistId":"r7","works":[{"name":"Egmont Overture","composed":{"from":{"year":1809},"to":{"year":1810}}}],"contributors":[{"role":"composer","artist":{"id":"","name":"Ludwig van Beethoven"}}],"recorded":{"from":{"year":1975},"to":{"year":1975},"places":["Sofiensaal"]}},{"id":"c4","isDir":false,"title":"Encore","track":4,"duration":100,"recorded":{"from":{"year":1975},"to":{"year":1975},"places":["Sofiensaal"]}},{"id":"c6","isDir":false,"title":"Bows","track":5,"duration":30},{"id":"c7","isDir":false,"title":"Symphony No. 5: III. Scherzo","track":6,"works":[{"name":"Symphony No. 5","musicBrainzId":"w5"}],"displayComposer":"Ludwig van Beethoven"},{"id":"c5","isDir":false,"title":"Symphony No. 5: IV. Allegro","track":7,"duration":660,"works":[{"name":"Symphony No. 5","musicBrainzId":"w5"}],"displayComposer":"Ludwig van Beethoven"}]"#;

    /// A composition with its movements and its recordings on two albums, the
    /// second album's between the first's.
    const SYMPHONY: &str = r#""id":"w5","name":"Symphony No. 5","composers":[{"id":"r9","name":"Ludwig van Beethoven"}],"composed":{"from":{"year":1804},"to":{"year":1808}},"albumCount":2,"coverArt":"al-a2","movement":[{"name":"I. Allegro con brio","number":1},{"name":"II. Andante con moto","number":2},{"name":"III. Scherzo","number":3},{"name":"IV. Allegro","number":4}],"song":[{"id":"c1","isDir":false,"title":"Symphony No. 5: I. Allegro con brio","album":"Symphonies","albumId":"a2","year":1974,"duration":441,"artist":"Wiener Philharmoniker","artistId":"r7","recorded":{"from":{"year":1974,"month":3,"day":2},"to":{"year":1974,"month":3,"day":2},"places":["Musikverein"]}},{"id":"k1","isDir":false,"title":"Symphony No. 5","album":"Live in Munich","albumId":"a3","year":1982,"duration":1900,"artists":[{"id":"r8","name":"Carlos Kleiber"},{"id":"r12","name":"Bayerisches Staatsorchester"}]},{"id":"c2","isDir":false,"title":"Symphony No. 5: II. Andante con moto","album":"Symphonies","albumId":"a2","year":1974,"duration":588,"artists":[{"id":"r7","name":"Wiener Philharmoniker"},{"id":"r8","name":"Carlos Kleiber"}],"recorded":{"from":{"year":1974,"month":4},"to":{"year":1974,"month":4},"places":["Musikverein"]}}]"#;

    /// The first song of the empty search, with more of its fields filled.
    const HARBOUR_LIGHTS: &str = r#"{"id":"t1","isDir":false,"title":"Harbour Lights","album":"Night Harbour","albumId":"a1","artists":[{"id":"r1","name":"Mara Lind"}],"year":2021,"duration":252,"starred":"2024-01-01T00:00:00Z","coverArt":"al-a1","genres":[{"name":"Folk"},{"name":"Pop"}],"contributors":[{"role":"performer","subRole":"piano","artist":{"id":"r3","name":"Anna Bach"}}],"replayGain":{"trackGain":-6.2},"size":4404019,"samplingRate":44100}"#;

    /// The user signed in, an admin while `ADMIN` is set.
    fn joe() -> String {
        format!(
            r#"{{"username":"joe","scrobblingEnabled":true,"adminRole":{},"settingsRole":true,"downloadRole":true,"uploadRole":false,"playlistRole":true,"coverArtRole":false,"commentRole":false,"podcastRole":false,"streamRole":true,"jukeboxRole":false,"shareRole":false,"videoConversionRole":false,"folder":[1]}}"#,
            ADMIN.load(Relaxed)
        )
    }

    /// The ids the server was asked to star.
    static STARRED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The songs of the playlist `WORKS`: the classical album's four
    /// movements of Symphony No. 5 together, its overture, then its second
    /// and third movements alone, numbered as Fonoteca numbers them.
    fn works_playlist() -> String {
        let album: serde_json::Value = serde_json::from_str(&format!("{{{CLASSICAL}}}")).unwrap();
        let song = |id: &str, number: Option<i64>| {
            let mut song = album["song"]
                .as_array()
                .unwrap()
                .iter()
                .find(|song| song["id"] == id)
                .unwrap()
                .clone();
            if let Some(number) = number {
                song["movements"] =
                    serde_json::json!([{"name": "m", "number": number, "count": 4}]);
            }
            song
        };
        let songs = [
            song("c1", Some(1)),
            song("c2", Some(2)),
            song("c7", Some(3)),
            song("c5", Some(4)),
            song("c3", None),
            song("c2", Some(2)),
            song("c7", Some(3)),
        ];
        serde_json::to_string(&songs).unwrap()
    }

    /// The playlist changes the server was asked for, each its method and
    /// its arguments but the credentials.
    static PLAYLIST_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the Road Trip playlist is "Moved", with a new id, as a
    /// server that names playlists by their files has it after a rename.
    static MOVED: AtomicBool = AtomicBool::new(false);
    /// While set, the flow test's server answers every request with an error.
    static DOWN: AtomicBool = AtomicBool::new(false);
    /// How many covers the server was asked for, and the first ones' ids.
    static COVERS: AtomicUsize = AtomicUsize::new(0);
    static FIRST_COVERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// How many times the album list was given, and whether it refuses all
    /// but its first part.
    static LISTS: AtomicUsize = AtomicUsize::new(0);
    static LATER_DOWN: AtomicBool = AtomicBool::new(false);
    /// How many hearts were refused: every unstar, and the symphony's star.
    static REFUSED: AtomicUsize = AtomicUsize::new(0);
    /// While set, the server takes only an artist's name for their top
    /// songs, not their id, as Fonoteca does.
    static NAMES_ONLY: AtomicBool = AtomicBool::new(false);
    /// While set, the server has a saved play queue, one song of the
    /// classical album, and no motion covers; and how many it was asked for.
    static QUEUED: AtomicBool = AtomicBool::new(false);
    static NO_MOTION: AtomicBool = AtomicBool::new(false);
    /// While set, the classical album's motion cover takes a while to come.
    static SLOW_MOTION: AtomicBool = AtomicBool::new(false);
    static MOTIONS_ASKED: AtomicUsize = AtomicUsize::new(0);
    static NO_WAVEFORM: AtomicBool = AtomicBool::new(false);
    /// While set, the server converts a song from part way in.
    static TRANSCODE_OFFSET: AtomicBool = AtomicBool::new(false);
    /// While set, the server's genres are Fonoteca's first version, with no
    /// descriptions.
    static OLD_GENRES: AtomicBool = AtomicBool::new(false);
    static SLOW_WAVEFORM: AtomicBool = AtomicBool::new(false);
    static WAVEFORMS_ASKED: AtomicUsize = AtomicUsize::new(0);
    /// How many scans were asked for; the server refuses the first. Whether
    /// the one it took is still going, and how often that was asked.
    static SCANS: AtomicUsize = AtomicUsize::new(0);
    static SCANNING: AtomicBool = AtomicBool::new(false);
    static SCAN_ASKS: AtomicUsize = AtomicUsize::new(0);
    /// The user changes the server was asked for, as `PLAYLIST_WRITES` has
    /// them; whether joe is an admin.
    static USER_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The request lines of the songs streamed.
    static STREAMS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static ADMIN: AtomicBool = AtomicBool::new(true);
    /// The radio station changes, the same way.
    static RADIO_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the server has playlist folders and Fonoteca's tags; the
    /// changes to those and to where playlists are, the same way.
    static ORGANIZED: AtomicBool = AtomicBool::new(false);
    static ORGANIZE_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the server refuses to give the tags.
    static TAGS_DOWN: AtomicBool = AtomicBool::new(false);

    /// A request's method and its arguments but the credentials.
    fn write_of(request: &str) -> String {
        let target = request.split_whitespace().nth(1).unwrap_or_default();
        let (method, query) = target.split_once('?').unwrap_or((target, ""));
        let credentials = ["u=", "t=", "s=", "p=", "v=", "c=", "f="];
        let asked: Vec<&str> = query
            .split('&')
            .filter(|arg| !credentials.iter().any(|c| arg.starts_with(c)))
            .collect();
        format!(
            "{} {}",
            method.trim_start_matches("/rest/"),
            asked.join("&")
        )
    }

    /// The radio directory: two stations with the most votes, 65 of jazz
    /// with the server's Jazz first, one of rock, none of other genres, one
    /// of the Netherlands, one of Dutch jazz, two named News, the countries'
    /// names, three languages, one without a code, and a picture of each
    /// station.
    fn radio_directory(request: &str) -> Vec<u8> {
        let target = request.split_whitespace().nth(1).unwrap_or_default();
        let host = request
            .lines()
            .find_map(|line| {
                line.to_lowercase()
                    .strip_prefix("host: ")
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let number = |name: &str| {
            target
                .split(['?', '&'])
                .find_map(|param| param.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
                .unwrap_or(0)
        };
        let station = |n: usize, name: &str, country: &str| {
            format!(
                r#"{{"stationuuid":"s{n}","name":"{name}","url":"https://s{n}.example.org/listen.m3u","url_resolved":"https://s{n}.example.org/listen","homepage":"https://s{n}.example.org/","favicon":"http://{}/logo{n}.png","countrycode":"{country}","votes":{n}}}"#,
                host.trim()
            )
        };
        let list = |stations: Vec<String>| json(&format!("[{}]", stations.join(",")));
        if target.starts_with("/json/countries") {
            json(
                r#"[{"name":"The Netherlands","iso_3166_1":"NL","stationcount":3},{"name":"Germany","iso_3166_1":"DE","stationcount":1}]"#,
            )
        } else if target.starts_with("/json/stations/topvote?") {
            list(vec![station(1, "Wave", "NL"), station(2, "Talk", "DE")])
        } else if target.starts_with("/json/stations/bytagexact/jazz?") {
            let first = number("offset");
            let jazz = (first..65.min(first + number("limit"))).map(|n| match n {
                0 => station(100, "Jazz", "").replace(
                    "https://s100.example.org/listen\"",
                    "https://stream.example.org/jazz\"",
                ),
                n => station(100 + n, &format!("Jazz {n}"), "NL"),
            });
            list(jazz.collect())
        } else if target.starts_with("/json/stations/bytagexact/rock?") {
            list(vec![station(3, "Rock", "DE")])
        } else if target.starts_with("/json/stations/bycountrycodeexact/NL?") {
            list(vec![station(4, "Radio NL", "NL")])
        } else if target.starts_with("/json/stations/search?")
            && target.contains("tag=jazz&")
            && target.contains("language=dutch&")
        {
            list(vec![station(200, "Dutch Jazz", "NL")])
        } else if target.starts_with("/json/stations/search?") && target.contains("name=News&") {
            // Another News first, whose picture doesn't come, then the
            // server's.
            let other = station(5, "News", "GB").replace("/logo5.png", "/missing.png");
            let same = station(6, "News", "NL").replace(
                "https://s6.example.org/listen\"",
                "http://news.example.org/live.mp3\"",
            );
            list(vec![other, same])
        } else if target.starts_with("/json/languages?") {
            json(
                r#"[{"name":"english","iso_639":"en","stationcount":9},{"name":"dutch colombo","iso_639":null,"stationcount":1},{"name":"dutch","iso_639":"nl","stationcount":5}]"#,
            )
        } else if target.starts_with("/json/stations/") {
            list(Vec::new())
        } else if target.starts_with("/logo") {
            reply("200 OK", "image/png", PNG)
        } else {
            not_found(request)
        }
    }

    fn flaky(request: &str) -> Vec<u8> {
        if DOWN.load(Relaxed) {
            reply("503 Service Unavailable", "text/plain", b"down")
        } else {
            subsonic(request)
        }
    }

    /// An OpenSubsonic server without API keys that knows one user, joe, and
    /// 603 albums, more than the client asks for at once. The first has songs, two on two discs; unstarring them is
    /// refused. The second is classical, with a motion cover, as is a4. The last three
    /// are by or about Bach, and each order puts them differently. It knows
    /// 122 artists, in three indexes, two playlists, one of them empty and
    /// read-only, and 230 songs for the empty search, more than the client
    /// asks for at once. It takes every playlist change but a rename to
    /// "Taken".
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
            let by_id = if NAMES_ONLY.load(Relaxed) {
                ""
            } else {
                r#",{"name":"topSongsByArtistId","versions":[1]}"#
            };
            let organized = if ORGANIZED.load(Relaxed) {
                r#",{"name":"playlistFolders","versions":[1]},{"name":"fonotecaTags","versions":[1]}"#
            } else {
                ""
            };
            let motion = if NO_MOTION.load(Relaxed) {
                ""
            } else {
                r#"{"name":"fonotecaAlbumMotion","versions":[1]},"#
            };
            let waveform = if NO_WAVEFORM.load(Relaxed) {
                ""
            } else {
                r#"{"name":"fonotecaWaveform","versions":[1]},"#
            };
            let genre_versions = if OLD_GENRES.load(Relaxed) { "1" } else { "1,2" };
            let offset = if TRANSCODE_OFFSET.load(Relaxed) {
                r#",{"name":"transcodeOffset","versions":[1]}"#
            } else {
                ""
            };
            ok(&format!(
                r#""openSubsonicExtensions":[{{"name":"songLyrics","versions":[1]}},{motion}{waveform}{{"name":"fonotecaCompositions","versions":[1]}},{{"name":"fonotecaGenres","versions":[{genre_versions}]}},{{"name":"fonotecaArtistBanner","versions":[1]}}{by_id}{organized}{offset}]"#
            ))
        } else if !["u=joe&", "u=eve&"]
            .iter()
            .any(|user| request.contains(user))
        {
            json(WRONG_PASSWORD)
        } else if request.contains("/rest/getPlaylistFolders?") {
            ok(
                r#""playlistFolders":{"playlistFolder":[{"id":"f1","name":"Trips","sortOrder":1},{"id":"f2","name":"Old","parentId":"f1"}]}"#,
            )
        } else if [
            "createPlaylistFolder",
            "updatePlaylistFolder",
            "deletePlaylistFolder",
            "movePlaylist",
            "createTag",
            "updateTag",
            "deleteTag",
        ]
        .iter()
        .any(|change| request.contains(&format!("/rest/{change}?")))
        {
            let write = write_of(request);
            ORGANIZE_WRITES.lock().unwrap().push(write.clone());
            if write.starts_with("deletePlaylistFolder") {
                json(
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"The folder isn't empty"}}}"#,
                )
            } else if write.starts_with("createPlaylistFolder") {
                ok(r#""playlistFolder":{"id":"f3","name":"Live","parentId":"f1"}"#)
            } else if write.starts_with("createTag") {
                ok(r#""tag":{"id":"t9","name":"Live","songCount":0}"#)
            } else {
                json(OK)
            }
        } else if request.contains("/rest/getTags?") && TAGS_DOWN.load(Relaxed) {
            reply("503 Service Unavailable", "text/plain", b"down")
        } else if request.contains("/rest/getTags?") {
            ok(
                r#""tags":{"tag":[{"id":"t1","name":"Mood","songCount":2},{"id":"t2","name":"Calm","parentId":"t1","songCount":1},{"id":"t3","name":"Instrument","songCount":0}]}"#,
            )
        } else if request.contains("/rest/getTag?") {
            ok(
                r#""tag":{"id":"t1","name":"Mood","songCount":2,"song":[{"id":"s1","isDir":false,"title":"Harbour Lights","duration":252,"tagIds":["t1"]},{"id":"s9","isDir":false,"title":"Fugue","duration":218,"tagIds":["t2"]}]}"#,
            )
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
            MOTIONS_ASKED.fetch_add(1, Relaxed);
            if id == Some("a2") || id == Some("a4") {
                if SLOW_MOTION.load(Relaxed) {
                    std::thread::sleep(Duration::from_millis(400));
                }
                reply("200 OK", "image/webp", MOTION)
            } else {
                json(
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":70,"message":"Not found"}}}"#,
                )
            }
        } else if request.contains("/rest/getWaveform") {
            WAVEFORMS_ASKED.fetch_add(1, Relaxed);
            if SLOW_WAVEFORM.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(400));
            }
            if id == Some("w1") {
                ok(r#""waveform":{"level":[0.1,0.4,0.2,0]}"#)
            } else {
                json(
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":70,"message":"No such waveform."}}}"#,
                )
            }
        } else if request.contains("/rest/getAlbumInfo2") {
            ok(
                r#""albumInfo":{"notes":"Recorded at sea &amp; on land.<br/><a href=\"https://example.org\">Read more</a>"}"#,
            )
        } else if request.contains("/rest/getArtistInfo2") {
            ok(
                r#""artistInfo2":{"biography":"<p>Mara Lind sings.</p>","similarArtist":[{"id":"r2","name":"Ove Strand"},{"id":"","name":"Nobody Here"}]}"#,
            )
        } else if request.contains("/rest/getTopSongs")
            && if NAMES_ONLY.load(Relaxed) {
                id.is_none() && request.contains("artist=Mara")
            } else {
                id == Some("r1")
            }
        {
            ok(
                r#""topSongs":{"song":[{"id":"s2","isDir":false,"title":"Rope and Anchor","album":"Night Harbour","albumId":"a1","duration":218,"artists":[{"id":"r1","name":"Mara Lind"}]},{"id":"s1","isDir":false,"title":"Harbour Lights","album":"Night Harbour","albumId":"a1","duration":252,"starred":"2024-01-01T00:00:00Z"}]}"#,
            )
        } else if request.contains("/rest/getAlbum") && id == Some("a2") {
            ok(&format!(r#""album":{{{CLASSICAL}}}"#))
        } else if request.contains("/rest/getAlbum") {
            let songs = r#"[{"id":"s1","isDir":false,"title":"Harbour Lights","track":1,"discNumber":1,"duration":252,"starred":"2024-01-01T00:00:00Z","artists":[{"id":"r1","name":"Mara Lind"}]},{"id":"s2","isDir":false,"title":"Rope and Anchor","track":1,"discNumber":2,"duration":218,"artist":"Mara Lind","artistId":"r1"}]"#;
            ok(&format!(r#""album":{{{ALBUM},"song":{songs}}}"#))
        } else if request.contains("/rest/getSong") && id == Some("t1") {
            ok(&format!(r#""song":{HARBOUR_LIGHTS}"#))
        } else if request.contains("/rest/getIndexes") {
            ok(
                r#""indexes":{"ignoredArticles":"The","lastModified":1,"index":[{"name":"M","artist":[{"id":"d1","name":"Mara Lind"}]},{"name":"S","artist":[{"id":"d3","name":"Singles","starred":"2024-01-01T00:00:00Z"}]}],"child":[{"id":"t9","isDir":false,"title":"Loose Song","duration":30}]}"#,
            )
        } else if request.contains("/rest/getMusicDirectory") && id == Some("d1") {
            ok(
                r#""directory":{"id":"d1","parent":"root","name":"Mara Lind","child":[{"id":"d2","parent":"d1","isDir":true,"title":"Night Harbour","coverArt":"al-a1"},{"id":"t1","parent":"d1","isDir":false,"title":"Harbour Lights","duration":252}]}"#,
            )
        } else if request.contains("/rest/getArtists") {
            let others: Vec<String> = (1..=120)
                .map(|n| {
                    format!(
                        r#"{{"id":"o{n}","name":"Other {n}","coverArt":"ar-o{n}","albumCount":1}}"#
                    )
                })
                .collect();
            ok(&format!(
                r#""artists":{{"ignoredArticles":"The","index":[{{"name":"A","artist":[{{"id":"r3","name":"Anna Bach","albumCount":2}}]}},{{"name":"M","artist":[{{"id":"r1","name":"Mara Lind","coverArt":"ar-r1","bannerArt":"ba-r1","albumCount":1}}]}},{{"name":"O","artist":[{}]}}]}}"#,
                others.join(",")
            ))
        } else if request.contains("/rest/getArtist") {
            ok(&format!(
                r#""artist":{{"id":"r1","name":"Mara Lind","coverArt":"ar-r1","album":[{{{ALBUM}}}]}}"#
            ))
        } else if request.contains("/rest/search3") {
            let number = |name: &str| param(name).and_then(|n| n.parse::<usize>().ok());
            let song = |n: usize| {
                if n == 1 {
                    HARBOUR_LIGHTS.to_owned()
                } else if n == 2 {
                    // Also on the Road Trip playlist.
                    r#"{"id":"s9","isDir":false,"title":"Fugue","duration":218,"artist":"Anna Bach"}"#.to_owned()
                } else {
                    format!(r#"{{"id":"t{n}","isDir":false,"title":"Song {n}","duration":60}}"#)
                }
            };
            let songs: Vec<String> = (1..=230)
                .map(song)
                .skip(number("songOffset").unwrap_or(0))
                .take(number("songCount").unwrap_or(20))
                .collect();
            if param("query") != Some("") || number("albumCount") != Some(0) {
                return json(OK);
            }
            ok(&format!(
                r#""searchResult3":{{"song":[{}]}}"#,
                songs.join(",")
            ))
        } else if request.contains("/rest/getPlaylists") {
            let road_trip = if MOVED.load(Relaxed) {
                ROAD_TRIP.replace(
                    r#""id":"p1","name":"Road Trip""#,
                    r#""id":"p4","name":"Moved""#,
                )
            } else {
                ROAD_TRIP.to_owned()
            };
            let road_trip = if ORGANIZED.load(Relaxed) {
                road_trip.replacen(r#""name":"#, r#""playlistFolderId":"f1","name":"#, 1)
            } else {
                road_trip
            };
            ok(&format!(
                r#""playlists":{{"playlist":[{{{road_trip}}},{{"id":"p2","name":"Empty","songCount":0,"duration":0,"created":"2024-01-01T00:00:00Z","changed":"2024-01-01T00:00:00Z","readonly":true}}]}}"#
            ))
        } else if request.contains("/rest/getPlaylist") && id == Some("p2") {
            ok(
                r#""playlist":{"id":"p2","name":"Empty","songCount":0,"duration":0,"created":"2024-01-01T00:00:00Z","changed":"2024-01-01T00:00:00Z","readonly":true}"#,
            )
        } else if request.contains("/rest/getUser") && param("username") == Some("joe") {
            ok(&format!(r#""user":{}"#, joe()))
        } else if request.contains("/rest/getGenres") {
            ok(
                r#""genres":{"genre":[{"value":"Folk","songCount":2,"albumCount":1},{"value":"Jazz","songCount":3,"albumCount":2,"description":"Jazz is music of improvisation.","descriptionSource":"Wikipedia","descriptionUrl":"https://en.wikipedia.org/wiki/Jazz"},{"value":"hip-hop","songCount":1,"albumCount":1,"parents":["Jazz"]}]}"#,
            )
        } else if request.contains("/rest/getInternetRadioStations") {
            ok(
                r#""internetRadioStations":{"internetRadioStation":[{"id":"r1","name":"Jazz","streamUrl":"https://stream.example.org/jazz","homePageUrl":"https://radio.example.org/","coverArt":"ir-r1"},{"id":"r2","name":"News","streamUrl":"http://news.example.org/live.mp3"}]}"#,
            )
        } else if ["create", "update", "delete"]
            .iter()
            .any(|change| request.contains(&format!("/rest/{change}InternetRadioStation")))
        {
            RADIO_WRITES.lock().unwrap().push(write_of(request));
            json(OK)
        } else if request.contains("/rest/stream?") {
            STREAMS
                .lock()
                .unwrap()
                .push(request.lines().next().unwrap_or_default().to_owned());
            reply("200 OK", "audio/mpeg", b"abc")
        } else if request.contains("/rest/changePassword") {
            USER_WRITES.lock().unwrap().push(write_of(request));
            json(OK)
        } else if ["create", "update", "delete"]
            .iter()
            .any(|change| request.contains(&format!("/rest/{change}Playlist")))
        {
            let write = write_of(request);
            PLAYLIST_WRITES.lock().unwrap().push(write.clone());
            if write.contains("name=Moved") {
                MOVED.store(true, Relaxed);
            }
            if write.contains("name=Taken") {
                json(
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":0,"message":"That name is taken"}}}"#,
                )
            } else if write.starts_with("createPlaylist") {
                ok(
                    r#""playlist":{"id":"p3","name":"Mix","songCount":0,"duration":0,"created":"2024-03-01T00:00:00Z","changed":"2024-03-01T00:00:00Z"}"#,
                )
            } else {
                json(OK)
            }
        } else if request.contains("/rest/getPlaylist") && id == Some("p5") {
            ok(&format!(
                r#""playlist":{{{WORKS},"entry":{}}}"#,
                works_playlist()
            ))
        } else if request.contains("/rest/getPlaylist") {
            let songs = r#"[{"id":"s1","isDir":false,"title":"Harbour Lights","album":"Night Harbour","albumId":"a1","duration":252,"starred":"2024-01-01T00:00:00Z","artists":[{"id":"r1","name":"Mara Lind"}]},{"id":"s9","isDir":false,"title":"Fugue","duration":218,"artist":"Anna Bach"}]"#;
            ok(&format!(r#""playlist":{{{ROAD_TRIP},"entry":{songs}}}"#))
        } else if request.contains("/rest/getComposers") {
            ok(
                r#""composers":{"index":[{"name":"B","composer":[{"id":"r9","name":"Ludwig van Beethoven","sortName":"Beethoven, Ludwig van","coverArt":"ar-r9","compositionCount":2}]},{"name":"M","composer":[{"id":"r10","name":"Wolfgang Amadeus Mozart","compositionCount":1}]}]}"#,
            )
        } else if request.contains("/rest/getCompositions") && id == Some("w5") {
            ok(&format!(
                r#""compositions":{{"composition":[{{{SYMPHONY}}}]}}"#
            ))
        } else if request.contains("/rest/getCompositions") {
            let egmont = r#"{"id":"w9","name":"Egmont Overture","composers":[{"id":"r9","name":"Ludwig van Beethoven"}],"composed":{"from":{"year":1809},"to":{"year":1810}},"albumCount":1}"#;
            let requiem = r#"{"id":"w7","name":"Requiem","composers":[{"id":"r10","name":"Wolfgang Amadeus Mozart"},{"id":"r11","name":"Franz Xaver Süssmayr"}],"albumCount":3,"form":"Requiem","period":"Classical","starred":"2026-10-19T08:00:00Z","key":"D minor","movementCount":14,"duration":3300,"lyricists":[{"id":"r12","name":"Thomas of Celano"},{"id":"","name":"Liturgy"}],"languages":["lat","deu"]}"#;
            let symphony = r#"{"id":"w5","name":"Symphony No. 5","composers":[{"id":"r9","name":"Ludwig van Beethoven"}],"composed":{"from":{"year":1804},"to":{"year":1808}},"albumCount":2,"movementCount":4}"#;
            let pieces = if param("composerId") == Some("r9") {
                [egmont, symphony].join(",")
            } else {
                [egmont, requiem, symphony].join(",")
            };
            ok(&format!(r#""compositions":{{"composition":[{pieces}]}}"#))
        } else if request.contains("/rest/getScanStatus") {
            SCAN_ASKS.fetch_add(1, Relaxed);
            let scanning = SCANNING.load(Relaxed);
            let count = match (scanning, SCANS.load(Relaxed)) {
                (true, _) => 3,
                (false, 0 | 1) => 146,
                (false, _) => 150,
            };
            ok(&format!(
                r#""scanStatus":{{"scanning":{scanning},"count":{count}}}"#
            ))
        } else if request.contains("/rest/startScan") && SCANS.fetch_add(1, Relaxed) == 0 {
            json(
                r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":50,"message":"Not allowed"}}}"#,
            )
        } else if request.contains("/rest/startScan") {
            SCANNING.store(true, Relaxed);
            ok(r#""scanStatus":{"scanning":true,"count":3}"#)
        } else if request.contains("/rest/getPlayQueue") && QUEUED.load(Relaxed) {
            ok(
                r#""playQueue":{"current":"c1","position":0,"username":"joe","changed":"2024-01-01T00:00:00Z","changedBy":"test","entry":[{"id":"c1","isDir":false,"title":"Symphony No. 5: I. Allegro con brio","album":"Symphonies","albumId":"a2","coverArt":"al-a2","duration":441}]}"#,
            )
        } else if request.contains("/rest/getCoverArt") {
            if COVERS.fetch_add(1, Relaxed) < 6 {
                FIRST_COVERS.lock().unwrap().extend(id.map(str::to_owned));
            }
            reply("200 OK", "image/png", PNG)
        } else if request.contains("/rest/unstar")
            || (request.contains("/rest/star") && id == Some("w5"))
        {
            REFUSED.fetch_add(1, Relaxed);
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

    /// What the playing art is: its motion's colour, as far as encoding
    /// lets them be, the still cover, or nothing.
    fn art_looks(ui: &AppWindow) -> &'static str {
        let art = ui.get_playing_art();
        match art.size().width {
            0 => "blank",
            1 => "still",
            _ => match art.to_rgba8().map(|picture| picture.as_slice()[0]) {
                Some(pixel) if pixel.r > pixel.b => "red",
                _ => "blue",
            },
        }
    }

    /// How the playing art looked over `time`.
    async fn art_over(ui: &AppWindow, time: Duration) -> BTreeSet<&'static str> {
        let seen = std::cell::RefCell::new(BTreeSet::new());
        let until = Instant::now() + time;
        wait_for("the time to pass", || {
            seen.borrow_mut().insert(art_looks(ui));
            Instant::now() >= until
        })
        .await;
        seen.into_inner()
    }

    /// Lets the app run for `time`.
    async fn idle(time: Duration) {
        let until = Instant::now() + time;
        wait_for("the time to pass", || Instant::now() >= until).await;
    }

    /// Holds the left button, or a finger with `Input.touch` on, at
    /// `position` for `ms`, moved `drift` to the right.
    async fn hold(ui: &AppWindow, position: slint::LogicalPosition, drift: f32, ms: u64) {
        let (window, button) = (ui.window(), PointerEventButton::Left);
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        let position = slint::LogicalPosition::new(position.x + drift, position.y);
        window.dispatch_event(WindowEvent::PointerMoved { position });
        idle(Duration::from_millis(ms)).await;
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    /// Drags the left button, or a finger with `Input.finger` on, from
    /// `from` along `path`: each step `ms` after the one before, to `dx`,
    /// `dy` from where it went down. Lets go `rest` ms after the last. A
    /// finger lifted leaves the window, as on a phone.
    async fn drag(
        ui: &AppWindow,
        from: slint::LogicalPosition,
        path: &[(u64, f32, f32)],
        rest: u64,
    ) {
        let (window, button) = (ui.window(), PointerEventButton::Left);
        window.dispatch_event(WindowEvent::PointerPressed {
            position: from,
            button,
        });
        let mut position = from;
        for &(ms, dx, dy) in path {
            idle(Duration::from_millis(ms)).await;
            position = slint::LogicalPosition::new(from.x + dx, from.y + dy);
            window.dispatch_event(WindowEvent::PointerMoved { position });
        }
        idle(Duration::from_millis(rest)).await;
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
        if ui.global::<Input>().get_finger() {
            window.dispatch_event(WindowEvent::PointerExited);
        }
    }

    /// Waits for `value` to stop changing, and gives where it stopped.
    async fn settled(value: impl Fn() -> f32) -> f32 {
        let mut last = value();
        for _ in 0..50 {
            idle(Duration::from_millis(200)).await;
            let now = value();
            if now == last {
                return now;
            }
            last = now;
        }
        panic!("still moving at {last}");
    }

    /// Waits for a glide just let go 248px short of an edge at 2000px/s,
    /// which glides 647px, to reach it, and checks it eased into it, as a
    /// shelf into its last tile, rather than stopping dead on it after a
    /// tenth of a second; in 248/647 of the 925ms, so it left the finger as
    /// fast.
    async fn eases_into(edge: &str, reached: impl Fn() -> bool) {
        let let_go = Instant::now();
        wait_for(edge, reached).await;
        let eased = let_go.elapsed();
        assert!(
            eased > Duration::from_millis(250),
            "stopped dead on {edge} after {eased:?}"
        );
        assert!(
            eased < Duration::from_millis(600),
            "eased into {edge} over {eased:?}"
        );
    }

    fn middle(element: &ElementHandle) -> slint::LogicalPosition {
        let (at, size) = (element.absolute_position(), element.size());
        slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0)
    }

    fn button(ui: &AppWindow, label: &str) -> ElementHandle {
        find(ui, label).unwrap_or_else(|| panic!("no {label} button"))
    }

    /// The button with `label`, unless it is out of sight.
    fn find(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
        let label = label.to_owned();
        ui.root_element()
            .query_descendants()
            .match_accessible_role(AccessibleRole::Button)
            .match_predicate(move |button| button.accessible_label().is_some_and(|l| l == label))
            .find_first()
    }

    /// Whether a drop-down with `label` is in sight.
    fn find_combo(ui: &AppWindow, label: &str) -> bool {
        let label = label.to_owned();
        ui.root_element()
            .query_descendants()
            .match_accessible_role(AccessibleRole::Combobox)
            .match_predicate(move |combo| combo.accessible_label().is_some_and(|l| l == label))
            .find_first()
            .is_some()
    }

    fn click(ui: &AppWindow, label: &str) {
        button(ui, label).invoke_accessible_default_action();
    }

    /// The sidebar entry that is lit, or nothing.
    fn lit(ui: &AppWindow) -> String {
        (usize::try_from(ui.get_section()).ok())
            .and_then(|at| ui.get_sections().row_data(at))
            .map(Into::into)
            .unwrap_or_default()
    }

    /// Presses and lets go of `key`, for what has the keys.
    fn press(ui: &AppWindow, key: impl Into<slint::SharedString>) {
        let text = key.into();
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
    }

    /// The field with `label`, which holds `text` once its old text is
    /// deleted and `text` typed.
    async fn type_into(ui: &AppWindow, label: &str, text: &str) {
        let label = label.to_owned();
        ui.root_element()
            .query_descendants()
            .match_predicate(move |field| field.accessible_label().is_some_and(|l| l == label))
            .find_first()
            .expect("no such field")
            .single_click(PointerEventButton::Left)
            .await;
        press(ui, slint::platform::Key::End);
        for _ in 0..12 {
            press(ui, slint::platform::Key::Backspace);
        }
        for key in text.chars() {
            press(ui, key);
        }
    }

    /// Presses at `from` on the graph, once the page has held the press, and
    /// moves to `to` in steps, without letting go.
    async fn drag_to(ui: &AppWindow, from: slint::LogicalPosition, to: slint::LogicalPosition) {
        let window = ui.window();
        window.dispatch_event(WindowEvent::PointerPressed {
            position: from,
            button: PointerEventButton::Left,
        });
        idle(Duration::from_millis(150)).await;
        for step in 1..=5 {
            let fraction = step as f32 / 5.0;
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(
                    from.x + (to.x - from.x) * fraction,
                    from.y + (to.y - from.y) * fraction,
                ),
            });
        }
    }

    /// Clicks the first heart that is `loved`, or the first that isn't.
    fn click_heart(ui: &AppWindow, loved: bool) {
        ui.root_element()
            .query_descendants()
            .match_accessible_role(AccessibleRole::Button)
            .match_predicate(move |heart| {
                heart
                    .accessible_label()
                    .is_some_and(|l| l.starts_with("Love "))
                    && heart.accessible_checked() == Some(loved)
            })
            .find_first()
            .expect("no heart")
            .invoke_accessible_default_action();
    }

    /// The only test that may start Slint or touch the store: both are
    /// global to the process.
    #[test]
    fn onboarding_browsing_and_sign_out() {
        i_slint_backend_testing::init_integration_test_with_system_time();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _runtime = runtime.enter();
        let server = serve(flaky);
        *browse::DIRECTORY_ROOT.lock().unwrap() = format!("{}/json/", serve(radio_directory));
        *zone::TESTS.lock().unwrap() = "Europe/Amsterdam".into();

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

            // On a small phone, or above an open keyboard, the form scrolls
            // to its last button, wrapped lines and all.
            ui.window().set_size(slint::LogicalSize::new(360.0, 340.0));
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(180.0, 170.0),
                    delta_x: 0.0,
                    delta_y: -2000.0,
                });
            let back = button(&ui, "Back");
            let bottom = back.absolute_position().y + back.size().height;
            assert!(bottom <= 340.0, "Back ends at {bottom}");
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));

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
            assert_eq!(ui.get_page(), Page::Home);
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
            assert!(store::load_listenbrainz().await.is_none());

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
            // The filter keeps the keys while the page shows its matches.
            filter("");
            ui.root_element()
                .query_descendants()
                .match_predicate(|field| {
                    field.accessible_label().is_some_and(|l| l == "Filter albums")
                })
                .find_first()
                .expect("no album filter")
                .single_click(PointerEventButton::Left)
                .await;
            let visit = ui.get_visit();
            press(&ui, "b");
            wait_for("the matches", || ui.get_visit() != visit).await;
            idle(Duration::from_millis(50)).await;
            press(&ui, "a");
            assert_eq!(ui.get_album_filter(), "ba");
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
            ui.invoke_select_tile(2, false);
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
            // So does Escape.
            ui.invoke_select_tile(1, false);
            press(&ui, slint::platform::Key::Escape);
            assert_eq!(ui.get_selected_tiles(), 0);
            assert!(selected(&ui).is_empty());
            button(&ui, "Night Harbour")
                .single_click(PointerEventButton::Right)
                .await;
            assert_eq!(selected(&ui), [0]);
            button(&ui, "Mara Lind")
                .single_click(PointerEventButton::Right)
                .await;
            assert!(selected(&ui).is_empty());
            // On a phone a finger held on a tile, or on its name, does the
            // same, and the tile doesn't open.
            ui.global::<Input>().set_touch(true);
            hold(&ui, middle(&button(&ui, "Night Harbour")), 0.0, 800).await;
            assert_eq!(selected(&ui), [0]);
            hold(&ui, middle(&button(&ui, "Mara Lind")), 0.0, 800).await;
            assert!(selected(&ui).is_empty());
            assert_eq!(ui.get_page(), Page::Albums);
            ui.global::<Input>().set_touch(false);
            ui.invoke_select_tile(1, false);
            ui.invoke_clear_selection();
            assert!(selected(&ui).is_empty());
            assert_eq!(ui.get_selected_tiles(), 0);
            // With Shift, every tile from the one last picked to it, either
            // way; with none picked on the page, just the one.
            ui.invoke_select_tile(3, true);
            assert_eq!(selected(&ui), [3]);
            ui.invoke_select_tile(1, false);
            assert_eq!(selected(&ui), [1, 3]);
            ui.invoke_select_tile(4, true);
            assert_eq!(selected(&ui), [1, 2, 3, 4]);
            ui.invoke_select_tile(0, true);
            assert_eq!(selected(&ui), [0, 1, 2, 3, 4]);
            assert_eq!(ui.get_selected_tiles(), 5);
            ui.invoke_clear_selection();
            // A click with Shift held is one.
            let shift = |pressed: bool| {
                let text = slint::platform::Key::Shift.into();
                ui.window().dispatch_event(if pressed {
                    WindowEvent::KeyPressed { text }
                } else {
                    WindowEvent::KeyReleased { text }
                });
            };
            ui.invoke_select_tile(0, false);
            let third = ui.get_tiles().row_data(2).unwrap().title;
            shift(true);
            button(&ui, &third)
                .single_click(PointerEventButton::Left)
                .await;
            shift(false);
            assert_eq!(selected(&ui), [0, 1, 2]);
            button(&ui, &third)
                .single_click(PointerEventButton::Left)
                .await;
            assert_eq!(selected(&ui), [0, 1]);
            ui.invoke_clear_selection();
            // A page shown again has nothing picked on it to start a range.
            ui.invoke_select_tile(1, false);
            ui.invoke_open("artists".into());
            ui.invoke_open("albums".into());
            ui.invoke_select_tile(3, true);
            assert_eq!(selected(&ui), [1, 3]);
            ui.invoke_clear_selection();
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
            ui.invoke_select_tile(1, false);
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
            click(&ui, "Settings");
            assert_eq!(ui.get_page(), Page::Settings);
            assert_eq!(ui.get_selected_tiles(), 1);
            for (section, choices) in [
                (
                    "Playback",
                    [
                        "Volume leveling",
                        "Streaming format",
                        "Streaming bitrate",
                    ]
                    .as_slice(),
                ),
                ("Sound processing", &["Headroom", "Crossfeed", "Upsampling"]),
            ] {
                click(&ui, section);
                assert_eq!(ui.get_page(), Page::Setting);
                for choice in choices {
                    assert!(find_combo(&ui, choice), "no {choice} choice in {section}");
                }
                assert!(
                    !find_combo(&ui, "Sound device") && find(&ui, "Sound device").is_none(),
                    "the sound device is chosen on now playing, not in {section}"
                );
                ui.invoke_back();
                assert_eq!(ui.get_page(), Page::Settings);
            }
            // The streaming quality picked is what the next song is fetched in;
            // a device without mobile data has only the one choice.
            click(&ui, "Playback");
            assert!(!find_combo(&ui, "Streaming on mobile data format"));
            let streamed_with = |format: i32, bitrate: i32, seconds: f64| {
                ui.invoke_choose_stream_quality(false, format, bitrate);
                let begins_at = play::testing::fetch_song("tr-1", seconds);
                let request = STREAMS.lock().unwrap().last().cloned();
                (request.unwrap_or_default(), begins_at)
            };
            let (request, _) = streamed_with(2, 5, 0.0);
            assert!(
                request.contains("format=opus") && request.contains("maxBitRate=96"),
                "{request}"
            );
            // Without transcodeOffset the whole conversion comes, from its
            // start, for the player to seek in.
            let (request, begins_at) = streamed_with(2, 5, 61.5);
            assert!(!request.contains("timeOffset"), "{request}");
            assert_eq!(begins_at, None);
            let (request, _) = streamed_with(0, 5, 0.0);
            assert!(request.contains("format=raw"), "{request}");
            assert!(!request.contains("maxBitRate"), "{request}");
            // What is saved shows in the settings at the start, held to the
            // entries there are.
            let mut saved = prefs::load();
            saved.wifi_quality = prefs::StreamQuality {
                format: 9,
                bitrate: 40,
            };
            saved.mobile_quality = prefs::StreamQuality {
                format: 1,
                bitrate: 2,
            };
            prefs::save(&saved);
            play::testing::show_saved_stream_quality(&ui);
            assert_eq!((ui.get_wifi_format(), ui.get_wifi_bitrate()), (3, 6));
            assert_eq!((ui.get_mobile_format(), ui.get_mobile_bitrate()), (1, 2));
            ui.invoke_back();
            // The ListenBrainz token has its own entry. An earlier version's
            // moves out of the account's, but never over one already there.
            let account = store::load().await.expect("saved");
            let kept_by_account = |token: &str| {
                let mut json = serde_json::to_value(&account).unwrap();
                json["listenbrainz"] = serde_json::json!({ "token": token, "user": "old" });
                json["server"] = account.server.clone().into();
                serde_json::from_value::<Account>(json).unwrap()
            };
            let token = |listenbrainz: Option<ListenBrainz>| listenbrainz.map(|l| l.token);
            let mut old = kept_by_account("old token");
            split_legacy_account(&mut old).await.unwrap();
            assert_eq!(token(store::load_listenbrainz().await), Some("old token".into()));
            let migrated = serde_json::to_value(store::load().await.unwrap()).unwrap();
            assert!(migrated.get("listenbrainz").is_none(), "the account lost it");
            assert!(migrated.get("server").is_none(), "and its server");
            assert_eq!(store::load_server().await, Some(account.server.clone()));
            assert_eq!(store::load().await.unwrap().server, account.server);
            let mut other = kept_by_account("other token");
            split_legacy_account(&mut other).await.unwrap();
            assert_eq!(token(store::load_listenbrainz().await), Some("old token".into()));
            // One that kept no token has its server moved all the same.
            store::forget_server().await.unwrap();
            store::forget_listenbrainz().await.unwrap();
            let mut json = serde_json::to_value(&account).unwrap();
            json["server"] = account.server.clone().into();
            let mut tokenless = serde_json::from_value::<Account>(json).unwrap();
            split_legacy_account(&mut tokenless).await.unwrap();
            assert_eq!(store::load_server().await, Some(account.server.clone()));
            assert!(store::load_listenbrainz().await.is_none());

            // Removing it deletes the entry and leaves the account signed in.
            ui.set_listenbrainz_user("old".into());
            click(&ui, "ListenBrainz");
            assert_eq!(ui.get_page(), Page::Setting);
            assert!(find(&ui, "Replace token").is_some());
            click(&ui, "Remove token");
            wait_for("the token removed", || ui.get_listenbrainz_user() == "").await;
            assert!(store::load_listenbrainz().await.is_none());
            assert!(store::load().await.is_some());
            assert_eq!(ui.get_step(), Step::Done);
            assert!(find(&ui, "Set token").is_some());
            ui.invoke_back();
            // The EQ's graph: a click adds a band, a point dragged changes
            // its numbers as it goes and is saved once let go, a typed
            // number moves the point, and the picked band can be removed
            // from under the graph.
            click(&ui, "Sound processing");
            ui.set_eq_on(true);
            ui.invoke_toggle_eq(true);
            // Tall, so the band rows are in sight too.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 1500.0));
            idle(Duration::from_millis(100)).await;
            let graph = || {
                let graph = ui
                    .root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Groupbox)
                    .match_predicate(|graph| graph.accessible_label().is_some_and(|l| l == "EQ graph"))
                    .find_first()
                    .expect("no EQ graph");
                let (corner, size) = (graph.absolute_position(), graph.size());
                // The plot is inset 14px, where a place is a fraction of it.
                move |x: f32, y: f32| {
                    slint::LogicalPosition::new(
                        corner.x + 14.0 + x * (size.width - 28.0),
                        corner.y + 14.0 + y * (size.height - 28.0),
                    )
                }
            };
            let saved_band = || prefs::load().eq.bands.first().cloned();
            let (window, left) = (ui.window(), PointerEventButton::Left);
            let at = graph();
            // The page holds a press back a moment, to see if it scrolls.
            let held = Duration::from_millis(150);
            window.dispatch_event(WindowEvent::PointerPressed { position: at(0.5, 0.25), button: left });
            idle(held).await;
            window.dispatch_event(WindowEvent::PointerReleased { position: at(0.5, 0.25), button: left });
            let line = |row: usize| ui.get_eq_bands().row_data(row).unwrap();
            assert_eq!(ui.get_eq_bands().row_count(), 1, "a click adds a band");
            assert_eq!((line(0).frequency, line(0).gain), ("632".into(), "7.5".into()));
            assert_eq!(ui.get_eq_selected(), 0);
            assert_eq!(saved_band().unwrap().gain, 7.5);
            // A click that drifts is a drag of nothing, not a band.
            window.dispatch_event(WindowEvent::PointerPressed { position: at(0.2, 0.8), button: left });
            idle(held).await;
            window.dispatch_event(WindowEvent::PointerReleased { position: at(0.25, 0.8), button: left });
            assert_eq!(ui.get_eq_bands().row_count(), 1);

            let point = || button(&ui, "Band 1 point");
            let center = |point: &ElementHandle| {
                let (corner, size) = (point.absolute_position(), point.size());
                slint::LogicalPosition::new(corner.x + size.width / 2.0, corner.y + size.height / 2.0)
            };
            let from = center(&point());
            let target = at(0.8, 0.75);
            drag_to(&ui, from, target).await;
            let shown = ui.get_eq_points().row_data(0).unwrap();
            assert!(
                (shown.x - 0.8).abs() < 0.01 && (shown.y - 0.75).abs() < 0.01,
                "the point follows the drag: {shown:?}"
            );
            let frequency: f64 = line(0).frequency.parse().unwrap();
            assert!((frequency - 5024.0).abs() < 8.0, "{frequency} Hz");
            assert_eq!(line(0).gain, "-7.5", "the numbers follow the drag");
            assert_eq!(saved_band().unwrap().frequency, 632.0, "saved once let go");
            window.dispatch_event(WindowEvent::PointerReleased { position: target, button: left });
            let saved = saved_band().unwrap();
            assert_eq!((saved.frequency, saved.gain), (frequency, -7.5));

            // A band typed in is still the band the graph moves, and a band
            // moved on the graph is still what the row's other fields keep.
            let field_text = |label: &str| {
                let label = label.to_owned();
                ui.root_element()
                    .query_descendants()
                    .match_predicate(move |field| field.accessible_label().is_some_and(|l| l == label))
                    .find_first()
                    .expect("no such field")
                    .accessible_value()
                    .unwrap()
                    .to_string()
            };
            let row = |ui: &AppWindow| {
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::ListItem)
                    .match_predicate(|row| row.accessible_label().is_some_and(|l| l == "Band 1"))
                    .find_first()
                    .expect("no Band 1 row")
            };
            ui.set_eq_selected(-1);
            assert_eq!(row(&ui).accessible_checked(), Some(false));
            type_into(&ui, "Band 1 frequency in Hz", "2500").await;
            assert_eq!(ui.get_eq_selected(), 0, "typing in a row picks its band");
            assert_eq!(row(&ui).accessible_checked(), Some(true), "the row of the picked band is lit");
            assert_eq!(saved_band().unwrap().frequency, 2500.0);
            // One move, to where the row's last value was, which the typed
            // field is not: only the graph's push puts it right.
            ui.invoke_move_band(0, 0.8, 0.75);
            ui.invoke_release_band();
            // The fields take the change on the event loop's next turn.
            idle(Duration::from_millis(50)).await;
            // Back where the row's last value was, which a typed field is not.
            assert_eq!(saved_band().unwrap().frequency, frequency);
            assert_eq!(field_text("Band 1 frequency in Hz"), line(0).frequency.as_str(), "the typed field shows the drag");
            assert_eq!(field_text("Band 1 gain in dB"), "-7.5");
            type_into(&ui, "Band 1 gain in dB", "3").await;
            let kept = saved_band().unwrap();
            assert_eq!((kept.frequency, kept.gain), (frequency, 3.0), "the drag is kept by the next edit");
            assert_eq!(line(0).frequency.as_str(), field_text("Band 1 frequency in Hz"));

            // Typing 1 kHz and 0 dB puts the point there.
            let typed = EqBandLine { filter: 0, frequency: "1000".into(), gain: "0".into(), q: "1".into() };
            ui.invoke_edit_band(0, typed);
            let moved = center(&point());
            let expected = at(0.5663, 0.5);
            assert!(
                (moved.x - expected.x).abs() < 2.0 && (moved.y - expected.y).abs() < 2.0,
                "{moved:?} against {expected:?}"
            );
            // A pass has no gain, so it moves sideways only.
            ui.invoke_add_band_at(0.3, 0.5);
            assert_eq!(ui.get_eq_selected(), 1);
            ui.invoke_edit_band(1, EqBandLine { filter: 4, frequency: "100".into(), gain: "6".into(), q: "0.71".into() });
            assert_eq!(ui.get_eq_points().row_data(1).unwrap().y, 0.5);
            ui.invoke_move_band(1, 0.1, 0.0);
            ui.invoke_release_band();
            let pass = prefs::load().eq.bands[1].clone();
            assert_eq!((pass.frequency, pass.gain), (40.0, 6.0));

            click(&ui, "Remove band 2 from the graph");
            assert_eq!(ui.get_eq_bands().row_count(), 1);
            let left_over = prefs::load().eq.bands[0].clone();
            assert_eq!((left_over.filter, left_over.frequency), (0, 1_000.0), "the picked band went");
            assert_eq!(ui.get_eq_selected(), -1);
            assert_eq!(prefs::load().eq.bands.len(), 1);
            // A band that goes from before the picked one leaves it picked.
            ui.invoke_add_band_at(0.2, 0.5);
            ui.invoke_add_band_at(0.7, 0.5);
            let before = prefs::load().eq.bands;
            assert_eq!(ui.get_eq_selected(), 2);
            ui.invoke_remove_band(1);
            let after = prefs::load().eq.bands;
            assert_eq!(after, [before[0].clone(), before[2].clone()]);
            assert_eq!(ui.get_eq_selected(), 1, "the picked band is still the one picked");
            ui.invoke_remove_band(1);
            assert_eq!((ui.get_eq_selected(), prefs::load().eq.bands.len()), (-1, 1));
            // While a point is dragged the player is handed the EQ now and
            // then, not at each move, and the saved one once it is let go.
            let heard: std::rc::Rc<std::cell::RefCell<Vec<playback::Dsp>>> = Default::default();
            let other = AppWindow::new().unwrap();
            sound::wire(&other, {
                let heard = heard.clone();
                move |dsp| heard.borrow_mut().push(dsp)
            });
            let band_heard = |at: usize| heard.borrow()[at].eq.clone().unwrap().bands[0].frequency;
            let started = Instant::now();
            for _ in 0..5 {
                other.invoke_move_band(0, 0.3, 0.4);
            }
            idle(Duration::from_millis(200)).await;
            assert!(heard.borrow().is_empty(), "not at each move");
            wait_for("the player to follow", || !heard.borrow().is_empty()).await;
            assert!(started.elapsed() >= Duration::from_millis(400), "{:?}", started.elapsed());
            assert_eq!((heard.borrow().len(), band_heard(0)), (1, 159.0));
            assert_eq!(prefs::load().eq.bands[0].frequency, 1_000.0, "saved once let go");
            for _ in 0..5 {
                other.invoke_move_band(0, 0.35, 0.4);
            }
            idle(Duration::from_millis(300)).await;
            assert_eq!(heard.borrow().len(), 1, "not at each move");
            // A drag that goes on is followed while it does, not only once
            // it stops.
            for _ in 0..8 {
                other.invoke_move_band(0, 0.35, 0.4);
                idle(Duration::from_millis(100)).await;
            }
            let followed = heard.borrow().len();
            assert!((2..=3).contains(&followed), "{followed} times during the drag");
            other.invoke_release_band();
            wait_for("the saved EQ", || heard.borrow().len() == followed + 1).await;
            assert_eq!(band_heard(followed), 224.0);
            assert_eq!(prefs::load().eq.bands[0].frequency, 224.0);
            // A finger dragging a point, or the graph's empty part, with some
            // drift up or down moves the point and leaves the page where it
            // is; the next finger elsewhere scrolls it.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            idle(Duration::from_millis(100)).await;
            ui.global::<Input>().set_finger(true);
            let frame = || {
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Groupbox)
                    .match_predicate(|graph| graph.accessible_label().is_some_and(|l| l == "EQ graph"))
                    .find_first()
                    .expect("no EQ graph")
            };
            let page_at = frame().absolute_position().y;
            let (x_before, bands) = (ui.get_eq_points().row_data(0).unwrap().x, ui.get_eq_bands().row_count());
            let path = [(0, 4.0, -3.0), (16, 12.0, -14.0), (16, 40.0, -30.0), (16, 80.0, -45.0)];
            drag(&ui, middle(&button(&ui, "Band 1 point")), &path, 0).await;
            assert!(ui.get_eq_points().row_data(0).unwrap().x > x_before, "the point follows the finger");
            assert_eq!(frame().absolute_position().y, page_at, "the page stays put");
            let empty = frame().absolute_position();
            let corner = slint::LogicalPosition::new(empty.x + 20.0, empty.y + frame().size().height - 20.0);
            drag(&ui, corner, &path, 0).await;
            assert_eq!(frame().absolute_position().y, page_at, "the page stays put");
            assert_eq!(ui.get_eq_bands().row_count(), bands, "a drag adds no band");
            drag(&ui, slint::LogicalPosition::new(900.0, 400.0), &[(0, 0.0, -20.0), (0, 0.0, -40.0)], 100).await;
            assert!(frame().absolute_position().y < page_at, "the page scrolls elsewhere");
            ui.global::<Input>().set_finger(false);
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Settings);
            // A headphone picked from OPRA's presets loads into the EQ: the
            // page goes back to it, with the bands on the graph and in rows.
            click(&ui, "Sound processing");
            click(&ui, "Headphone presets");
            assert_eq!(ui.get_shown_id(), "headphones");
            wait_for("the headphone presets", || ui.get_headphone_status().starts_with("Type a brand")).await;
            assert_eq!(ui.get_headphone_results().row_count(), 0, "nothing is listed before typing");
            ui.invoke_search_headphones("sennheiser hd 650".into());
            assert!(ui.get_headphone_results().row_count() > 0);
            ui.invoke_pick_headphone(0);
            assert!(ui.get_headphone_presets().row_count() > 0);
            assert!(find(&ui, "Load into EQ").is_some());
            let catalog = opra::bundled().unwrap();
            let expected = catalog.product(catalog.search("sennheiser hd 650")[0]).presets[0]
                .equalizer()
                .unwrap();
            ui.invoke_load_headphone(0);
            assert_eq!((ui.get_page(), ui.get_shown_id()), (Page::Setting, "sound".into()));
            assert_eq!(prefs::load().eq, expected);
            assert!(prefs::load().eq_on);
            assert!(!expected.bands.is_empty());
            assert_eq!(ui.get_eq_bands().row_count(), expected.bands.len(), "its bands in rows");
            assert_eq!(ui.get_eq_points().row_count(), expected.bands.len(), "its points on the graph");
            assert_ne!(ui.get_eq_curve(), "");
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Settings);
            click(&ui, "Library");
            assert_eq!(ui.get_page(), Page::Setting);
            // The server's scan: where it stands, one the server refuses,
            // and one it takes, followed while the page is up until done.
            wait_for("the scan status", || {
                ui.get_scan_status() == "146 items scanned."
            })
            .await;
            assert!(!ui.get_scanning());
            click(&ui, "Scan library");
            wait_for("the refusal", || ui.get_page_problem() != "").await;
            assert_eq!(
                ui.get_page_problem(),
                "Your server didn't take that: server error 50: Not allowed"
            );
            assert!(!ui.get_scanning());
            click(&ui, "Scan library");
            assert_eq!(ui.get_page_problem(), "");
            wait_for("the scan going", || {
                ui.get_scan_status() == "Scanning, 3 items so far…"
            })
            .await;
            assert!(ui.get_scanning());
            let asked = SCAN_ASKS.load(Relaxed);
            wait_for("a look at it", || SCAN_ASKS.load(Relaxed) > asked).await;
            let quiet = || async {
                idle(Duration::from_millis(300)).await;
                let asked = SCAN_ASKS.load(Relaxed);
                idle(Duration::from_millis(2500)).await;
                SCAN_ASKS.load(Relaxed) == asked
            };
            ui.invoke_back();
            assert!(quiet().await, "the scan is followed off its page");
            ui.invoke_forward();
            assert_eq!(ui.get_page(), Page::Setting);
            let asked = SCAN_ASKS.load(Relaxed);
            wait_for("a look on its page again", || {
                SCAN_ASKS.load(Relaxed) > asked + 1
            })
            .await;
            SCANNING.store(false, Relaxed);
            wait_for("the scan done", || {
                ui.get_scan_status() == "150 items scanned."
            })
            .await;
            assert!(!ui.get_scanning());
            assert!(quiet().await, "the scan is followed once done");
            // This page asks who is signed in, for what they may do.
            wait_for("the roles", || ui.get_roles() != "").await;
            assert_eq!(ui.get_roles(), "Admin · Settings · Stream · Download");
            assert!(ui.get_may_administer());
            // Your own password: the server's, then the saved one, which
            // the library goes on with.
            let writes = || USER_WRITES.lock().unwrap().clone();
            USER_WRITES.lock().unwrap().clear();
            ui.invoke_change_own_password("n3w pw".into());
            wait_for("the password changed", || {
                ui.get_saved_note() == "Password changed, and saved in your system's secret store."
            })
            .await;
            assert_eq!(writes(), ["changePassword username=joe&password=n3w+pw"]);
            let saved = store::load().await.expect("saved");
            assert!(
                matches!(saved.login, Login::Password { ref password, .. } if password == "n3w pw")
            );
            // Where the account couldn't be saved, it changes all the same.
            store::forget().await.unwrap();
            ui.invoke_change_own_password("n3w pw 2".into());
            wait_for("the unsaved password changed", || {
                ui.get_saved_note()
                    == "Password changed, but not saved, so the next start asks for it."
            })
            .await;
            assert_eq!(writes()[1], "changePassword username=joe&password=n3w+pw+2");
            assert!(store::load().await.is_none());
            store::save(saved).await.unwrap();
            ui.set_notice("".into());
            // The radio stations, with their home pages and where they
            // stream from; an admin adds, changes and deletes them.
            click(&ui, "Radio");
            assert_eq!(ui.get_page(), Page::Radio);
            wait_for("the stations", || ui.get_station_rows().row_count() == 2).await;
            let station = |row: usize| {
                let station = ui.get_station_rows().row_data(row).unwrap();
                [station.id, station.name, station.homepage, station.details].map(String::from)
            };
            assert_eq!(
                station(0),
                [
                    "r1",
                    "Jazz",
                    "https://radio.example.org/",
                    "radio.example.org · stream.example.org/jazz"
                ]
            );
            assert_eq!(station(1), ["r2", "News", "", "news.example.org/live.mp3"]);
            assert!(find(&ui, "Add station").is_some());
            // A dialog leaves the page behind it out of the keys' reach, and
            // Escape lets it go.
            click(&ui, "Add station");
            assert!(find(&ui, "Cancel").is_some());
            assert!(ui.global::<Input>().get_modal());
            assert_eq!(button(&ui, "Add station").accessible_enabled(), Some(false));
            press(&ui, slint::platform::Key::Escape);
            assert!(find(&ui, "Cancel").is_none());
            assert!(!ui.global::<Input>().get_modal());
            // The menus of the elements of a type, of those drawn.
            let menus = |of: &str| -> usize {
                let more =
                    |b: &ElementHandle| b.accessible_label().is_some_and(|l| l.starts_with("More"));
                let elements = ui.root_element().query_descendants().match_type_name(of).find_all();
                (elements.iter())
                    .map(|element| element.query_descendants().match_predicate(more).find_all().len())
                    .sum()
            };
            let radio_writes = || RADIO_WRITES.lock().unwrap().clone();
            ui.invoke_create_station(" Folk ".into(), "https://f.example.org/a".into(), " ".into());
            wait_for("the station added", || ui.get_notice() == "Added Folk.").await;
            ui.invoke_update_station(
                "r2".into(),
                "News".into(),
                "https://news.example.org/live.mp3".into(),
                "https://news.example.org".into(),
            );
            wait_for("the station saved", || ui.get_notice() == "Saved News.").await;
            ui.invoke_delete_station("r1".into(), "Jazz".into());
            wait_for("the station deleted", || ui.get_notice() == "Deleted Jazz.").await;
            assert_eq!(
                radio_writes(),
                [
                    "createInternetRadioStation name=Folk&streamUrl=https%3A%2F%2Ff.example.org%2Fa",
                    "updateInternetRadioStation id=r2&name=News&streamUrl=https%3A%2F%2Fnews.example.org%2Flive.mp3&homepageUrl=https%3A%2F%2Fnews.example.org",
                    "deleteInternetRadioStation id=r1"
                ]
            );
            assert_eq!(ui.get_page_problem(), "");
            // The directory's genres, each a shelf once it comes: Top
            // worldwide, the device's country, then the genres by the
            // library's albums, so Jazz before Rock, and none for a genre
            // without stations.
            wait_for("the shelves", || ui.get_station_shelves().row_count() == 4).await;
            let shelf = |row: usize| ui.get_station_shelves().row_data(row).unwrap();
            let titles: Vec<String> = (0..4).map(|row| shelf(row).title.into()).collect();
            assert_eq!(titles, ["Top worldwide", "Top in the Netherlands", "Jazz", "Rock"]);
            assert_eq!(
                [shelf(0).link, shelf(1).link, shelf(2).link].map(String::from),
                ["stations:", "stations:@NL", "stations:jazz"]
            );
            assert_eq!(shelf(2).tiles.row_count(), 12);
            let tile = shelf(0).tiles.row_data(0).unwrap();
            assert_eq!((tile.title.as_str(), tile.link.as_str()), ("Wave", ""));
            assert_eq!(tile.names.row_data(0).unwrap().label, "Netherlands");
            // Pictures come for the tiles in view.
            ui.invoke_station_shelf_in_view(0, 0, 0);
            let cover = |tiles: slint::ModelRc<AlbumTile>, row| tiles.row_data(row).unwrap().cover.size();
            wait_for("a station's picture", || cover(shelf(0).tiles, 0).width > 0).await;
            assert_eq!(cover(shelf(0).tiles, 1).width, 0);
            assert_eq!(ui.get_saved_stations().row_count(), 2);
            assert_eq!(menus("ListRow"), 0);
            assert!(menus("CoverTile") > 0);
            // An admin saves a station on the server, unless it has it.
            ui.invoke_save_station(0, 0);
            wait_for("the station saved", || ui.get_notice() == "Saved Wave.").await;
            assert_eq!(
                radio_writes().last().unwrap(),
                "createInternetRadioStation name=Wave&streamUrl=https%3A%2F%2Fs1.example.org%2Flisten&homepageUrl=https%3A%2F%2Fs1.example.org%2F"
            );
            ui.invoke_save_station(4, 0);
            assert_eq!(ui.get_notice(), "Jazz is saved already.");
            // The languages picked go first in each genre, the order kept on
            // the device.
            let choices = || {
                let choices = ui.get_language_choices();
                (choices.iter())
                    .map(|choice| (choice.title.to_string(), choice.shown))
                    .collect::<Vec<_>>()
            };
            assert_eq!(choices(), [("Dutch".into(), false), ("English".into(), false)]);
            ui.invoke_toggle_language(0);
            assert_eq!(choices(), [("Dutch".into(), true), ("English".into(), false)]);
            assert_eq!(crate::prefs::load().radio_languages, ["dutch"]);
            let first_jazz = || shelf(2).tiles.row_data(0).unwrap().title;
            wait_for("Dutch jazz first", || first_jazz() == "Dutch Jazz").await;
            assert_eq!(ui.get_station_shelves().row_count(), 4);
            assert_eq!(shelf(2).tiles.row_count(), 12);
            // A genre's page has all of it, a page at a time.
            ui.invoke_open("stations:jazz".into());
            assert_eq!(ui.get_page(), Page::Stations);
            assert_eq!(lit(&ui), "Radio");
            assert_eq!(ui.get_header_title(), "Jazz");
            wait_for("a page of jazz", || ui.get_tiles().row_count() == 61).await;
            assert_eq!(ui.get_tiles().row_data(0).unwrap().title, "Dutch Jazz");
            // Drawing the grid asks for the pictures of the tiles in view.
            ui.root_element().query_descendants().find_all();
            let cover = || ui.get_tiles().row_data(0).unwrap().cover.size().width;
            wait_for("a jazz station's picture", || cover() > 0).await;
            assert!(ui.get_more_tiles());
            ui.invoke_show_more();
            wait_for("the rest of the jazz", || ui.get_tiles().row_count() == 66).await;
            assert!(!ui.get_more_tiles());
            ui.invoke_save_station(-1, 65);
            wait_for("a jazz station saved", || ui.get_notice() == "Saved Jazz 64.").await;
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Radio);
            // The home page has the saved stations, with the server's
            // pictures, else the directory's.
            click(&ui, "Home");
            wait_for("the saved stations", || ui.get_saved_stations().row_count() == 2).await;
            let saved = |row| ui.get_saved_stations().row_data(row).unwrap();
            assert_eq!(saved(0).title, "Jazz");
            assert_eq!(saved(0).names.row_data(0).unwrap().label, "radio.example.org");
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Radio);
            // Waited for off the home page, which would otherwise have the
            // artists loaded before the artists page below fails. News is
            // found by name and told from another News by its stream.
            wait_for("Jazz's picture", || saved(0).cover.size().width > 0).await;
            wait_for("News's picture", || saved(1).cover.size().width > 0).await;
            // The device keeps the directory's picture, not the server's.
            let kept = crate::prefs::station_logos();
            assert_eq!(kept.len(), 1);
            assert_eq!(kept["http://news.example.org/live.mp3"].bytes().unwrap(), PNG);
            ui.set_notice("".into());
            // Anyone may see them, but only admins change them.
            // The settings page asks the server anew what they may do; nor
            // may they have the server scan.
            ADMIN.store(false, Relaxed);
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Setting);
            wait_for("joe no admin", || !ui.get_may_administer()).await;
            assert_eq!(ui.get_roles(), "Settings · Stream · Download");
            assert!(find(&ui, "Scan library").is_none());
            ui.invoke_forward();
            assert!(find(&ui, "Add station").is_none());
            wait_for("the stations", || ui.get_station_rows().row_count() == 2).await;
            assert_eq!((menus("ListRow"), menus("CoverTile")), (0, 0));
            ADMIN.store(true, Relaxed);
            ui.invoke_back();
            wait_for("joe an admin once more", || ui.get_may_administer()).await;
            assert!(find(&ui, "Scan library").is_some());
            ui.set_notice("".into());
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Settings);
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
            ui.invoke_select_tile(1, false);
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
            // What the server says about her, her top songs by her id, and
            // the artists like her, a link where they have a page.
            wait_for("her top songs", || ui.get_tracks().row_count() == 2).await;
            let top = ui.get_tracks().row_data(0).unwrap();
            assert_eq!(top.title, "Rope and Anchor");
            assert_eq!(top.duration, "3:38");
            assert!(ui.get_tracks().row_data(1).unwrap().loved);
            wait_for("her biography", || ui.get_about() != "").await;
            assert_eq!(ui.get_about(), "Mara Lind sings.");
            assert_eq!(ui.get_about_short(), "Mara Lind sings.");
            let similar: Vec<(String, String)> = ui
                .get_similar()
                .iter()
                .map(|name| (name.label.into(), name.link.into()))
                .collect();
            assert_eq!(
                similar,
                [
                    ("Ove Strand".to_owned(), "artist:r2".to_owned()),
                    ("Nobody Here".to_owned(), String::new())
                ]
            );
            click(&ui, "Albums");
            assert_eq!(ui.get_tiles().row_count(), 100);

            // The genres page has the server's genres, those most albums
            // carry first, each with a picture of the artist known for it
            // best, a banner, in the genres' tint. A genre's page has the
            // library's albums carrying it and their artists, all of each
            // behind See all. With Fonoteca's genres, those under none, and on
            // a genre's page those under it, which link back, and its
            // description. Its popular stations come from the radio directory.
            // What a genre has nothing of is left out.
            click(&ui, "Genres");
            assert_eq!(ui.get_page(), Page::Genres);
            wait_for("the genres", || ui.get_genre_tiles().row_count() == 2).await;
            let genre = |row| ui.get_genre_tiles().row_data(row).unwrap();
            assert_eq!(
                (genre(0).title.as_str(), genre(0).link.as_str()),
                ("Jazz", "genre:Jazz")
            );
            assert_eq!(genre(0).names.row_data(0).unwrap().label, "2 albums");
            assert_eq!(genre(1).title, "Folk");
            wait_for("Folk's picture", || genre(1).cover.size().width > 0).await;
            let pixel = genre(1).cover.to_rgba8().unwrap().as_slice()[0];
            assert!(pixel.b > pixel.r);
            assert_eq!(genre(0).cover.size().width, 0, "no artist known for Jazz has a banner");
            ui.invoke_open("genre:Jazz".into());
            wait_for("Jazz's subgenres", || ui.get_genre_tiles().row_count() == 1).await;
            assert_eq!(genre(0).link, "genre:hip-hop");
            assert_eq!(ui.get_about(), "Jazz is music of improvisation.");
            assert_eq!(
                (ui.get_about_source().as_str(), ui.get_about_url().as_str()),
                ("Wikipedia", "https://en.wikipedia.org/wiki/Jazz")
            );
            wait_for("Jazz's stations", || ui.get_genre_radio().tiles.row_count() == 12).await;
            let stations = ui.get_genre_radio();
            assert_eq!(stations.link, "stations:jazz");
            assert_eq!(stations.tiles.row_data(0).unwrap().title, "Dutch Jazz");
            assert_eq!(ui.get_genre_banner().size().width, 0);
            ui.invoke_open("genre:hip-hop".into());
            let parents = || -> Vec<String> {
                ui.get_header_names().iter().map(|name| name.link.into()).collect()
            };
            wait_for("its parent", || parents() == ["genre:Jazz"]).await;
            ui.invoke_back();
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Genres);
            let tile = |row| ui.get_tiles().row_data(row).unwrap();
            // Jazz's stations coming after Folk's page opened are not Folk's.
            ui.invoke_open("genre:Jazz".into());
            ui.invoke_open("genre:Folk".into());
            assert_eq!(ui.get_page(), Page::Genre);
            assert_eq!(ui.get_header_title(), "Folk");
            assert_eq!(ui.get_genre_radio().tiles.row_count(), 0);
            wait_for("Folk's banner", || ui.get_genre_banner().size().width > 0).await;
            assert_eq!(ui.get_about(), "");
            assert_eq!(ui.get_about_source(), "");
            wait_for("Folk's albums", || ui.get_tiles().row_count() == 1).await;
            assert_eq!(tile(0).link, "album:a1");
            wait_for("Folk's artists", || ui.get_genre_artists().row_count() == 1).await;
            let artist = ui.get_genre_artists().row_data(0).unwrap();
            assert_eq!(
                (artist.link.as_str(), artist.names.row_data(0).unwrap().label.as_str()),
                ("artist:r1", "1 album")
            );
            wait_for("the whole library", || ui.get_header_facts() == "1 album · 1 artist").await;
            let later = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < later {
                YieldNow(false).await;
            }
            assert_eq!(ui.get_genre_radio().tiles.row_count(), 0);
            assert_eq!(ui.get_genre_tiles().row_count(), 0);
            let section = || -> String {
                let at = usize::try_from(ui.get_section()).unwrap();
                ui.get_sections().row_data(at).unwrap().into()
            };
            assert_eq!(section(), "Genres");
            ui.invoke_open("albums:Folk".into());
            assert_eq!((ui.get_page(), ui.get_header_title().as_str()), (Page::Albums, "Folk"));
            assert_eq!(section(), "Albums");
            wait_for("all Folk's albums", || ui.get_tiles().row_count() == 1).await;
            assert_eq!(tile(0).link, "album:a1");
            ui.invoke_back();
            ui.invoke_open("artists:Folk".into());
            assert_eq!((ui.get_page(), ui.get_header_title().as_str()), (Page::Artists, "Folk"));
            assert_eq!(section(), "Artists");
            wait_for("all Folk's artists", || ui.get_tiles().row_count() == 1).await;
            assert_eq!(tile(0).link, "artist:r1");
            ui.invoke_back();
            ui.invoke_back();
            ui.invoke_back();
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Albums);

            // The playlists were asked for on sign-in, while the server was
            // down. Their page asks again, and so does Try again.
            assert_eq!(ui.get_sections().row_count(), 10);
            DOWN.store(true, Relaxed);
            click(&ui, "Playlists");
            wait_for("the failed playlists", || ui.get_page_problem() != "").await;
            assert_eq!(ui.get_page(), Page::Playlists);
            assert_eq!(ui.get_section(), 9);
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
                    "Home",
                    "Genres",
                    "Radio",
                    "Albums",
                    "Artists",
                    "Tracks",
                    "Composers",
                    "Compositions",
                    "Folders",
                    "Playlists",
                    "Road Trip",
                    "Empty"
                ]
            );
            assert_eq!(
                strings(ui.get_section_groups()),
                [
                    "Browse",
                    "Browse",
                    "Browse",
                    "Library",
                    "Library",
                    "Library",
                    "Library",
                    "Library",
                    "Library",
                    "Playlists",
                    "Playlists",
                    "Playlists"
                ]
            );
            // Each with its icon in front, but for the playlists.
            let icons = ui.global::<Icons>();
            let pictures = strings(ui.get_section_icons());
            assert_eq!(pictures.len(), 12);
            assert_eq!(pictures[0], icons.get_home().as_str());
            assert_eq!(pictures[1], icons.get_genre().as_str());
            assert_eq!(pictures[2], icons.get_radio().as_str());
            let library = [
                icons.get_album(),
                icons.get_artist(),
                icons.get_note(),
                icons.get_composer(),
                icons.get_composition(),
                icons.get_folder(),
            ];
            assert_eq!(pictures[3..9], library.map(String::from));
            assert_eq!(pictures[9..12], ["", "", ""]);
            ui.invoke_show_section(10);
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Playlist);
            assert_eq!(ui.get_section(), 10);
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
            // Songs go in the playlists the server doesn't call read-only.
            let targets: Vec<(String, String)> = ui
                .get_playlist_targets()
                .iter()
                .map(|name| (name.label.into(), name.link.into()))
                .collect();
            assert_eq!(targets, [("Road Trip".to_owned(), "p1".to_owned())]);
            assert!(ui.get_playlist_editable());
            // A track taken off leaves the page at once, and the playlist is
            // asked for again.
            let writes = || PLAYLIST_WRITES.lock().unwrap().clone();
            PLAYLIST_WRITES.lock().unwrap().clear();
            ui.invoke_remove_from_playlist(0);
            assert_eq!(ui.get_tracks().row_count(), 1);
            assert_eq!(ui.get_tracks().row_data(0).unwrap().title, "Fugue");
            wait_for("the track taken off", || {
                writes() == ["updatePlaylist playlistId=p1&songIndexToRemove=0"]
            })
            .await;
            wait_for("the playlist again", || ui.get_tracks().row_count() == 2).await;
            // A new name shows at once; one the server refuses goes back.
            ui.invoke_rename_playlist(" Drive ".into());
            assert_eq!(ui.get_header_title(), "Drive");
            wait_for("the rename", || {
                writes().last().map(String::as_str)
                    == Some("updatePlaylist playlistId=p1&name=Drive")
            })
            .await;
            ui.invoke_rename_playlist("Taken".into());
            wait_for("the refused rename", || ui.get_page_problem() != "").await;
            assert!(ui.get_page_problem().contains("That name is taken"));
            wait_for("the old name", || ui.get_header_title() == "Road Trip").await;
            ui.set_page_problem("".into());
            // Where a new name is a new id, the page moves to it, and a
            // change made there goes to the new one.
            ui.invoke_rename_playlist("Moved".into());
            wait_for("the page moved", || {
                let at = ui.get_section();
                at >= 0 && ui.get_sections().row_data(at as usize).as_deref() == Some("Moved")
            })
            .await;
            ui.invoke_remove_from_playlist(0);
            wait_for("the track taken off the moved one", || {
                writes().last().map(String::as_str)
                    == Some("updatePlaylist playlistId=p4&songIndexToRemove=0")
            })
            .await;
            MOVED.store(false, Relaxed);
            ui.invoke_open("playlist:p1".into());
            wait_for("Road Trip again", || {
                ui.get_tracks().row_count() == 2
                    && ui
                        .get_playlist_targets()
                        .iter()
                        .any(|name| name.link == "p1")
            })
            .await;
            // A song of the playlist on screen added to it.
            ui.invoke_add_track_to_playlist(1, "p1".into());
            wait_for("the song added", || {
                writes().last().map(String::as_str)
                    == Some("updatePlaylist playlistId=p1&songIdToAdd=s9")
            })
            .await;
            wait_for("the notice", || ui.get_notice() != "").await;
            assert_eq!(ui.get_notice(), "Added 1 track to Road Trip.");
            ui.set_notice("".into());
            // Movements of a whole work are its row and their rows; the
            // second and third of four, and the overture, stay songs.
            ui.invoke_open("playlist:p5".into());
            wait_for("the playlist of works", || ui.get_tracks().row_count() == 7).await;
            let rows: Vec<(String, i32, bool)> = ui
                .get_tracks()
                .iter()
                .map(|track| (track.work.to_string(), track.work_tracks, track.part))
                .collect();
            let row = |work: &str, tracks: i32, part: bool| (work.to_owned(), tracks, part);
            assert_eq!(
                rows,
                [
                    row("Symphony No. 5", 4, true),
                    row("", 0, true),
                    row("", 0, true),
                    row("", 0, true),
                    row("", 0, false),
                    row("", 0, false),
                    row("", 0, false)
                ]
            );
            // A song inside the work leaves the playlist by its own place.
            PLAYLIST_WRITES.lock().unwrap().clear();
            ui.invoke_remove_from_playlist(2);
            wait_for("the movement removed", || {
                writes() == ["updatePlaylist playlistId=p5&songIndexToRemove=2"]
            })
            .await;
            // A selected album goes in a playlist song by song, and the
            // selection is let go.
            ui.invoke_open("albums".into());
            wait_for("the albums", || ui.get_tiles().row_count() == 100).await;
            PLAYLIST_WRITES.lock().unwrap().clear();
            ui.invoke_select_tile(0, false);
            ui.invoke_add_selection_to_playlist("p1".into());
            assert_eq!(ui.get_selected_tiles(), 0);
            wait_for("the album added", || {
                writes() == ["updatePlaylist playlistId=p1&songIdToAdd=s1&songIdToAdd=s2"]
            })
            .await;
            wait_for("the notice", || ui.get_notice() != "").await;
            assert_eq!(ui.get_notice(), "Added 2 tracks to Road Trip.");
            ui.set_notice("".into());
            // A read-only playlist offers no changes.
            ui.invoke_open("playlist:p2".into());
            wait_for("the empty playlist", || ui.get_header_title() == "Empty").await;
            assert!(!ui.get_playlist_editable());
            ui.invoke_remove_from_playlist(0);
            // A new playlist needs a name, and opens.
            let sent = writes().len();
            ui.invoke_create_playlist("  ".into());
            ui.invoke_create_playlist("Mix".into());
            wait_for("the new playlist", || {
                ui.get_page() == Page::Playlist && ui.get_tracks().row_count() == 2
            })
            .await;
            assert_eq!(writes()[sent..], ["createPlaylist name=Mix"]);
            // Deleting the one on screen leads to the playlists.
            ui.invoke_delete_playlist();
            wait_for("the playlists", || ui.get_page() == Page::Playlists).await;
            assert_eq!(
                writes().last().map(String::as_str),
                Some("deletePlaylist id=p3")
            );
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            click(&ui, "Account");
            assert_eq!(ui.get_section(), -1);
            // Shown again at once, while they are asked for again.
            click(&ui, "Playlists");
            assert_eq!(ui.get_tiles().row_count(), 2);
            assert!(!ui.get_loading());
            click(&ui, "Albums");
            assert_eq!(ui.get_tiles().row_count(), 100);

            click(&ui, "Night Harbour");
            wait_for("the album", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Album);
            // Its notes as plain text, the first paragraph until opened.
            wait_for("the notes", || ui.get_about() != "").await;
            assert_eq!(ui.get_about(), "Recorded at sea & on land.\nRead more");
            assert_eq!(ui.get_about_short(), "Recorded at sea & on land.");
            let at = usize::try_from(ui.get_section()).unwrap();
            assert_eq!(ui.get_sections().row_data(at).unwrap(), "Albums", "an album is among the albums");
            assert_eq!(ui.get_header_title(), "Night Harbour");
            assert_eq!(ui.get_header_facts(), "2021 · Folk · 2 tracks, 8 minutes");
            let tracks = ui.get_tracks();
            assert_eq!(tracks.row_data(0).unwrap().title, "Harbour Lights");
            assert_eq!(tracks.row_data(1).unwrap().title, "Rope and Anchor");
            assert_eq!(tracks.row_data(0).unwrap().duration, "4:12");
            assert!(tracks.row_data(0).unwrap().loved);
            assert!(!tracks.row_data(1).unwrap().loved);
            assert!(ui.get_can_go_back());

            click_heart(&ui, false);
            assert!(ui.get_tracks().row_data(1).unwrap().loved);
            wait_for("the star", || *STARRED.lock().unwrap() == ["s2"]).await;
            assert_eq!(ui.get_page_problem(), "");
            click_heart(&ui, true);
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
            // A finger pulling the page down asks for the library anew, and
            // the page stays; one swiping in from an edge goes back or
            // forward. A mouse does neither.
            let page = ui
                .root_element()
                .query_descendants()
                .match_accessible_role(AccessibleRole::Main)
                .find_first()
                .unwrap();
            let (at, size) = (page.absolute_position(), page.size());
            let y = at.y + size.height / 2.0;
            let left = slint::LogicalPosition::new(at.x + 4.0, y);
            let right = slint::LogicalPosition::new(at.x + size.width - 4.0, y);
            // In the page's margin, past the edge but beside the tiles, which
            // a mouse press would open.
            let margin = slint::LogicalPosition::new(at.x + 22.0, y);
            let pull = |from: slint::LogicalPosition, by: f32| {
                let (window, button) = (ui.window(), PointerEventButton::Left);
                let to = slint::LogicalPosition::new(from.x, from.y + by);
                window.dispatch_event(WindowEvent::PointerPressed { position: from, button });
                window.dispatch_event(WindowEvent::PointerMoved { position: to });
                window.dispatch_event(WindowEvent::PointerReleased { position: to, button });
            };
            let scroll = |by: f32| {
                ui.window().dispatch_event(WindowEvent::PointerScrolled {
                    position: middle(&page),
                    delta_x: 0.0,
                    delta_y: by,
                });
            };
            ui.global::<Input>().set_finger(false);
            pull(left, 100.0);
            idle(Duration::from_millis(200)).await;
            assert_eq!(LISTS.load(Relaxed), lists);
            ui.global::<Input>().set_finger(true);
            pull(middle(&page), 50.0);
            idle(Duration::from_millis(200)).await;
            assert_eq!(LISTS.load(Relaxed), lists, "not far enough");
            scroll(-200.0);
            pull(margin, 100.0);
            idle(Duration::from_millis(200)).await;
            assert_eq!(LISTS.load(Relaxed), lists, "away from the top");
            pull(margin, 300.0);
            idle(Duration::from_millis(200)).await;
            assert_eq!(LISTS.load(Relaxed), lists, "reaching the top on the way");
            scroll(100000.0);
            pull(middle(&page), 100.0);
            wait_for("the library anew", || {
                LISTS.load(Relaxed) == lists + 2 && ui.get_tiles().row_count() == 100
            })
            .await;
            assert_eq!(ui.get_page(), Page::Albums);
            assert!(ui.get_can_go_back() && !ui.get_can_go_forward());
            // A finger's drag scrolls the page from its first touch on, the
            // page following the finger, and glides on when let go while
            // moving, as far as Android's lists do, but not once it rested.
            // Slint holds each press 100ms to see where it goes: these drags
            // go within it.
            let heading = page
                .query_descendants()
                .match_accessible_role(AccessibleRole::Text)
                .match_predicate(|text| text.accessible_label().is_some_and(|l| l == "Albums"))
                .find_first()
                .unwrap();
            let top = heading.absolute_position().y;
            let scrolled = || top - heading.absolute_position().y;
            let from = middle(&page);
            drag(&ui, from, &[(0, 0.0, -20.0), (0, 0.0, -70.0), (0, 0.0, -120.0)], 100).await;
            let followed = settled(scrolled).await;
            assert!((followed - 120.0).abs() < 0.01, "followed to {followed}");
            // One more sideways than up or down leaves a page no wider than
            // the window.
            drag(&ui, margin, &[(0, 12.0, -10.0), (0, 30.0, -20.0)], 100).await;
            assert!((scrolled() - followed).abs() < 0.01, "the page stays put");
            // At 2000px/s, 32px in a frame, the shortest a speed is measured
            // over, Android glides 647px; Slint's Flickable glided 1000px.
            drag(&ui, from, &[(0, 0.0, -20.0), (0, 0.0, -52.0)], 0).await;
            let glided = settled(scrolled).await - 172.0;
            assert!((glided - 647.4).abs() < 0.5, "glided {glided}px");
            // A finger landing stops a glide, and taps nothing, as on Android.
            let let_go = scrolled() + 52.0;
            drag(&ui, from, &[(0, 0.0, -20.0), (0, 0.0, -52.0)], 0).await;
            idle(Duration::from_millis(100)).await;
            drag(&ui, from, &[], 0).await;
            let stopped = settled(scrolled).await - let_go;
            assert!(stopped > 0.0 && stopped < glided, "stopped after {stopped}px");
            assert_eq!(ui.get_page(), Page::Albums, "the tap opened nothing");
            // A key or the wheel moving the page meanwhile takes over from a
            // glide.
            let take_over = async |by: &dyn Fn()| {
                drag(&ui, from, &[(0, 0.0, -20.0), (0, 0.0, -52.0)], 0).await;
                idle(Duration::from_millis(50)).await;
                by();
                let moved = scrolled();
                assert_eq!(settled(scrolled).await, moved, "the glide went on");
            };
            take_over(&|| press(&ui, slint::platform::Key::PageUp)).await;
            take_over(&|| scroll(40.0)).await;
            // Past 8000px/s, Android's top speed, it glides no further: at
            // 12500px/s 7186px, where Slint's Flickable glided 39062px.
            let let_go = scrolled() + 220.0;
            drag(&ui, from, &[(0, 0.0, -20.0), (0, 0.0, -220.0)], 0).await;
            let glided = settled(scrolled).await - let_go;
            assert!((glided - 7186.4).abs() < 0.5, "glided {glided}px");
            // A list more may come under it glides on into, as far as ever:
            // short of the bottom it had when let go, it takes in the next
            // hundred as it nears it.
            assert!(ui.get_more_tiles());
            let let_go = scrolled() + 220.0;
            drag(&ui, from, &[(0, 0.0, -20.0), (0, 0.0, -220.0)], 0).await;
            let glided = settled(scrolled).await - let_go;
            assert!((glided - 7186.4).abs() < 0.5, "glided {glided}px on");
            // Once nothing more comes under it, it eases into the bottom, as
            // into the top.
            wait_for("the last albums", || {
                scroll(-100000.0);
                !ui.get_more_tiles()
            })
            .await;
            scroll(-100000.0);
            let bottom = scrolled();
            scroll(300.0);
            drag(&ui, from, &[(0, 0.0, -20.0), (0, 0.0, -52.0)], 0).await;
            eases_into("the bottom", || (scrolled() - bottom).abs() < 0.01).await;
            scroll(100000.0);
            scroll(-300.0);
            assert_eq!(scrolled(), 300.0);
            drag(&ui, from, &[(0, 0.0, 20.0), (0, 0.0, 52.0)], 0).await;
            eases_into("the top", || scrolled() == 0.0).await;
            hold(&ui, left, 50.0, 0).await;
            assert_eq!(ui.get_page(), Page::Albums, "not far enough");
            hold(&ui, left, 100.0, 0).await;
            wait_for("the album by a swipe", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Album);
            hold(&ui, right, -50.0, 0).await;
            assert_eq!(ui.get_page(), Page::Album, "not far enough");
            hold(&ui, right, -100.0, 0).await;
            assert_eq!(ui.get_page(), Page::Albums);
            ui.global::<Input>().set_finger(false);
            hold(&ui, left, 100.0, 0).await;
            assert_eq!(ui.get_page(), Page::Albums, "a mouse doesn't swipe");
            // Now playing is a sheet over the page: it comes and goes
            // with the page, its place and its history as they were.
            {
                let (can_play, playing) = (ui.get_can_play(), ui.get_playing());
                ui.set_can_play(true);
                ui.set_playing(true);
                ui.set_playing_title("Northern Lights".into());
                let aurora = Name {
                    label: "Aurora".into(),
                    link: "albums".into(),
                };
                ui.set_playing_album(aurora);
                scroll(-40.0);
                let filter_at = || {
                    ui.root_element()
                        .query_descendants()
                        .match_predicate(|e| e.accessible_label().is_some_and(|l| l == "Filter albums"))
                        .find_first()
                        .map(|filter| filter.absolute_position())
                };
                let before = (filter_at(), ui.get_arrival(), ui.get_can_go_forward());
                assert!(before.0.is_some(), "the filter is in sight");
                // PageDown scrolls the page when it has the keys, and the
                // page goes back to where it was.
                let page_has_the_keys = || {
                    press(&ui, slint::platform::Key::PageDown);
                    let scrolled = filter_at() != before.0;
                    press(&ui, slint::platform::Key::PageUp);
                    assert_eq!(filter_at(), before.0);
                    scrolled
                };
                assert!(page_has_the_keys(), "the page has the keys");
                assert!(find(&ui, "Back").is_some());
                click(&ui, "Now playing");
                assert!(ui.get_now_playing_open());
                idle(Duration::from_millis(400)).await;
                assert!(
                    find(&ui, "Back").is_none() && filter_at().is_none(),
                    "the top bar and the page are out of reach under it"
                );
                // Tab and Shift+Tab never stop on what the sheet covers: the
                // keys would scroll the page, which sits under it.
                for key in [slint::platform::Key::Tab, slint::platform::Key::Backtab] {
                    for _ in 0..120 {
                        press(&ui, key);
                        press(&ui, slint::platform::Key::PageDown);
                    }
                }
                click(&ui, "Close now playing");
                idle(Duration::from_millis(400)).await;
                assert_eq!(filter_at(), before.0, "Tab left the page alone");
                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;
                // Whatever hands the keys back to the page, as a closed menu
                // does, leaves them with the sheet while it is over the page.
                let input = ui.global::<Input>();
                input.set_refocus(input.get_refocus() + 1);
                press(&ui, slint::platform::Key::Back);
                assert!(!ui.get_now_playing_open(), "Back closes it");
                assert!(page_has_the_keys(), "closing hands the page the keys");
                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;
                press(&ui, slint::platform::Key::Escape);
                assert!(!ui.get_now_playing_open(), "Escape closes it");
                assert_eq!(ui.get_page(), Page::Albums);
                idle(Duration::from_millis(400)).await;
                assert_eq!(
                    (filter_at(), ui.get_arrival(), ui.get_can_go_forward()),
                    before
                );
                assert!(page_has_the_keys(), "closing hands the page the keys");
                // The sidebar's entry for the page shown closes it too.
                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;
                click(&ui, &lit(&ui));
                assert!(!ui.get_now_playing_open(), "the sidebar closes it");
                idle(Duration::from_millis(400)).await;
                assert!(page_has_the_keys(), "the sidebar hands the page the keys");
                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;
                // The sound device is chosen from the sheet, wide, narrow and
                // on a phone: the one in use is marked, the choice is kept as
                // the settings once kept it, and opening the menu lists the
                // devices again.
                let dac = "alsa:hw:CARD=DAC,DEV=0";
                let merged_away = "alsa:plughw:CARD=DAC,DEV=0";
                let fake_devices = || {
                    play::testing::show_devices(
                        &ui,
                        vec![playback::OutputDevice {
                            id: dac.into(),
                            name: "USB DAC".into(),
                            detail: "hw:CARD=DAC,DEV=0".into(),
                            title: "USB DAC (hw:CARD=DAC,DEV=0)".into(),
                            same_as: vec![merged_away.into()],
                        }],
                    );
                };
                let icons = ui.global::<Icons>();
                let marked = || -> Vec<bool> {
                    ui.get_output_devices()
                        .iter()
                        .map(|entry| entry.icon == icons.get_check())
                        .collect()
                };
                let listed = || -> Vec<String> {
                    ui.get_output_devices().iter().map(|entry| entry.label.into()).collect()
                };
                for (width, height) in [(1100.0, 760.0), (700.0, 760.0), (400.0, 760.0)] {
                    ui.window().set_size(slint::LogicalSize::new(width, height));
                    idle(Duration::from_millis(400)).await;
                    fake_devices();
                    assert_eq!(marked(), [true, false]);
                    assert_eq!(ui.get_output_name(), "System default", "{width}px");
                    if width == 1100.0 {
                        let picker = button(&ui, "Sound device");
                        let (position, size) = (picker.absolute_position(), picker.size());
                        // The sheet ends above the player bar, and what plays
                        // fills its first screen down to the stage's padding.
                        let (player_bar, stage_padding) = (80.0, 24.0);
                        let bottom = position.y + size.height;
                        assert!(
                            (bottom - (height - player_bar - stage_padding)).abs() <= 1.0,
                            "at the bottom of the sheet, not at {bottom}"
                        );
                        // Button and name are centred together in what the
                        // docked sidebar leaves, so the button sits left of
                        // its middle by half the name and the gap.
                        let (sidebar, name_and_gap) = (240.0, 240.0);
                        let middle = sidebar + (width - sidebar) / 2.0;
                        let centre = position.x + size.width / 2.0;
                        assert!(
                            (middle - name_and_gap..=middle).contains(&centre),
                            "centred, not at {centre}"
                        );
                    }
                    click(&ui, "Sound device");
                    wait_for("the devices listed again", || !listed().contains(&"USB DAC".to_owned())).await;
                    fake_devices();
                    click(&ui, "USB DAC, hw:CARD=DAC,DEV=0");
                    assert_eq!(crate::prefs::load().output_device.as_deref(), Some(dac), "{width}px");
                    assert_eq!(marked(), [false, true], "{width}px");
                    assert_eq!(ui.get_output_name(), "USB DAC (hw:CARD=DAC,DEV=0)", "{width}px");
                    // Listed again at start-up or on opening, the one saved is the one marked.
                    fake_devices();
                    assert_eq!(marked(), [false, true], "{width}px");
                    click(&ui, "Sound device");
                    click(&ui, "System default");
                    assert_eq!(crate::prefs::load().output_device, None, "{width}px");
                    assert_eq!(marked().first(), Some(&true), "{width}px");
                    wait_for("the devices listed again", || !listed().contains(&"USB DAC".to_owned())).await;
                }
                // A choice saved as a route that was folded into another is
                // that other.
                let mut saved = crate::prefs::load();
                saved.output_device = Some(merged_away.into());
                crate::prefs::save(&saved);
                fake_devices();
                assert_eq!(marked(), [false, true]);
                assert_eq!(ui.get_output_name(), "USB DAC (hw:CARD=DAC,DEV=0)");
                saved.output_device = None;
                crate::prefs::save(&saved);
                ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
                idle(Duration::from_millis(400)).await;
                click(&ui, "Aurora");
                assert!(!ui.get_now_playing_open(), "a link out closes it");
                assert_eq!(ui.get_page(), Page::Albums);
                assert_eq!(ui.get_arrival(), before.1, "a link to this page is no step");
                idle(Duration::from_millis(400)).await;
                // A menu closing hands the keys back while the sheet is gone,
                // and the page, not the hidden sheet, takes them.
                input.set_refocus(input.get_refocus() + 1);
                assert!(page_has_the_keys(), "the page takes the keys back");

                // Escape closes it from wherever the keys sit: the sheet's close
                // button and the bar's controls, which Shift+Tab reaches from
                // the sheet's scroll in turn, before it comes to the covered
                // page and the sidebar.
                for presses in 0..=9 {
                    click(&ui, "Now playing");
                    idle(Duration::from_millis(300)).await;
                    for _ in 0..presses {
                        press(&ui, slint::platform::Key::Backtab);
                    }
                    press(&ui, slint::platform::Key::Escape);
                    assert!(
                        !ui.get_now_playing_open(),
                        "Escape closes it after {presses} presses of Shift+Tab"
                    );
                }
                idle(Duration::from_millis(400)).await;

                // The bar opens it wherever it is clicked but on its controls,
                // and a second click closes it. The artist line is text.
                let bar = button(&ui, "Now playing");
                let (bar_at, bar_size) = (bar.absolute_position(), bar.size());
                assert_eq!(bar_size.height, 80.0);
                assert_eq!(bar_at.y + bar_size.height, 760.0, "the bar ends the window");
                assert_eq!(bar_at.x + bar_size.width, 1100.0, "the bar spans the window");
                assert!(bar_size.width > 600.0);
                ui.set_playing_names(slint::ModelRc::new(slint::VecModel::from(vec![Name {
                    label: "Ada Lovelace".into(),
                    link: "artists".into(),
                }])));
                // A press on a bar says whether a finger made it.
                let finger = input.get_finger();
                let tap = |position: slint::LogicalPosition| hold(&ui, position, 0.0, 0);
                let at_bar = |x: f32, y: f32| slint::LogicalPosition::new(bar_at.x + x, bar_at.y + y);
                // Empty space below the title, beyond the 48px a smaller area
                // would cover, and the artist line.
                let artist = button(&ui, "Ada Lovelace");
                assert_eq!(artist.accessible_enabled(), Some(false), "the artist line is no link");
                for position in [at_bar(bar_size.width / 2.0, 72.0), middle(&artist)] {
                    tap(position).await;
                    assert!(ui.get_now_playing_open(), "a click on the bar opens it");
                    idle(Duration::from_millis(400)).await;
                    tap(at_bar(bar_size.width / 2.0, 72.0)).await;
                    assert!(!ui.get_now_playing_open(), "a second click closes it");
                    idle(Duration::from_millis(400)).await;
                }
                input.set_finger(finger);

                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;

                // What plays fills the first screen: the queue starts below it.
                let queue_heading = || {
                    ui.root_element()
                        .query_descendants()
                        .match_predicate(|e| e.accessible_label().is_some_and(|l| l == "Queue"))
                        .find_first()
                };
                let queue_lines = ui.get_queue_lines();
                ui.set_queue_lines(slint::ModelRc::new(slint::VecModel::from(vec![
                    QueueLine::default(),
                    QueueLine::default(),
                ])));
                press(&ui, slint::platform::Key::Home);
                assert!(queue_heading().is_none(), "the queue starts below the first screen");
                press(&ui, slint::platform::Key::PageDown);
                assert!(queue_heading().is_some(), "the queue follows when scrolled to");
                ui.set_queue_lines(queue_lines);

                // The seek bar has a row of its own: no title, artist, album
                // or time runs under or over it, whatever the width.
                let long = |what: &str| format!("{what} that keeps going and going until it has to wrap");
                let long_names = [long("An artist"), long("Another artist"), long("A third artist")];
                ui.set_playing_title(long("A song title").into());
                ui.set_playing_names(slint::ModelRc::new(slint::VecModel::from(
                    long_names
                        .iter()
                        .map(|label| Name {
                            label: label.into(),
                            link: "artists".into(),
                        })
                        .collect::<Vec<_>>(),
                )));
                ui.set_playing_album(Name {
                    label: long("An album").into(),
                    link: "albums".into(),
                });
                ui.set_playing_position(65.0);
                ui.set_playing_duration(271.0);
                let labels = [
                    long("A song title"),
                    long_names[0].clone(),
                    long_names[1].clone(),
                    long_names[2].clone(),
                    long("An album"),
                    "Position".to_owned(),
                    "1:05".to_owned(),
                    "4:31".to_owned(),
                ];
                for (width, height) in [(412.0, 915.0), (600.0, 760.0), (1100.0, 760.0)] {
                    ui.window().set_size(slint::LogicalSize::new(width, height));
                    idle(Duration::from_millis(400)).await;
                    let boxes: Vec<_> = labels
                        .iter()
                        .map(|label| {
                            let element = ui
                                .root_element()
                                .query_descendants()
                                .match_predicate({
                                    let label = label.clone();
                                    move |e| e.accessible_label().is_some_and(|l| l == label)
                                })
                                .find_first()
                                .unwrap_or_else(|| panic!("no {label} at {width}"));
                            (label, element.absolute_position(), element.size())
                        })
                        .collect();
                    for (i, (one, at, size)) in boxes.iter().enumerate() {
                        for (other, other_at, other_size) in &boxes[i + 1..] {
                            let apart = at.x + size.width <= other_at.x
                                || other_at.x + other_size.width <= at.x
                                || at.y + size.height <= other_at.y
                                || other_at.y + other_size.height <= at.y;
                            assert!(apart, "at {width}x{height}, {one} ({at:?} {size:?}) overlaps {other} ({other_at:?} {other_size:?})");
                        }
                    }
                }
                // The queue starts under the controls, however far the words
                // reach down the sheet.
                let queued = ui.get_queue_lines();
                ui.set_queue_lines(slint::ModelRc::new(slint::VecModel::from(vec![
                    QueueLine::default(),
                    QueueLine::default(),
                ])));
                ui.window().set_size(slint::LogicalSize::new(412.0, 1000.0));
                idle(Duration::from_millis(400)).await;
                press(&ui, slint::platform::Key::End);
                idle(Duration::from_millis(400)).await;
                let next = button(&ui, "Next");
                let queue = ui
                    .root_element()
                    .query_descendants()
                    .match_predicate(|e| e.accessible_label().is_some_and(|l| l == "Queue"))
                    .find_first()
                    .expect("no Queue heading");
                assert!(
                    queue.absolute_position().y >= next.absolute_position().y + next.size().height,
                    "the queue starts under the controls"
                );
                ui.set_queue_lines(queued);
                ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
                idle(Duration::from_millis(400)).await;

                // The sheet closes when the page changes under it, and when
                // playback ends.
                let visit = ui.get_visit();
                ui.invoke_back();
                wait_for("the page before to close it", || !ui.get_now_playing_open()).await;
                let back = ui.get_visit();
                assert_ne!(back, visit);
                ui.invoke_forward();
                wait_for("the page again", || ui.get_visit() != back).await;
                assert_eq!(ui.get_page(), Page::Albums);
                idle(Duration::from_millis(400)).await;
                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;
                assert!(ui.get_now_playing_open());
                ui.set_playing(false);
                wait_for("the end of playback to close it", || !ui.get_now_playing_open()).await;
                ui.set_playing(true);
                ui.set_playing_names(slint::ModelRc::default());
                scroll(100000.0);
                ui.set_playing(playing);
                ui.set_can_play(can_play);
                idle(Duration::from_millis(100)).await;
            }
            // Android's back in a filter hands the keys back to the page,
            // and the next one goes back a page.
            let filter = ui
                .root_element()
                .query_descendants()
                .match_predicate(|e| e.accessible_label().is_some_and(|l| l == "Filter albums"))
                .find_first()
                .unwrap();
            {
                let (window, button) = (ui.window(), PointerEventButton::Left);
                let position = middle(&filter);
                window.dispatch_event(WindowEvent::PointerPressed { position, button });
                window.dispatch_event(WindowEvent::PointerReleased { position, button });
            }
            press(&ui, slint::platform::Key::Back);
            assert_eq!(ui.get_page(), Page::Albums);
            press(&ui, slint::platform::Key::Back);
            assert_eq!(ui.get_page(), Page::Album);
            click(&ui, "Forward");
            assert_eq!(ui.get_page(), Page::Albums);
            click(&ui, "Back");
            wait_for("the album before them", || ui.get_tracks().row_count() == 2).await;
            hold(&ui, right, -100.0, 0).await;
            assert_eq!(ui.get_page(), Page::Album, "a mouse doesn't swipe");
            // Android's back button goes back a page.
            press(&ui, slint::platform::Key::Back);
            assert_eq!(ui.get_page(), Page::Albums);
            click(&ui, "Forward");
            assert_eq!(ui.get_page(), Page::Album);
            wait_for("the album once again", || ui.get_tracks().row_count() == 2).await;

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
            // The work's row queues the movements it heads, and no other row does.
            let work_tracks: Vec<i32> = tracks.iter().map(|track| track.work_tracks).collect();
            assert_eq!(work_tracks, [2, 0, 0, 0, 0, 2, 0]);
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
            // On a phone the rows wrap, and the page still scrolls to the end
            // of the last one.
            let window = ui.window();
            window.set_size(slint::LogicalSize::new(360.0, 640.0));
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(180.0, 400.0),
                delta_x: 0.0,
                delta_y: -100000.0,
            });
            let last = button(&ui, "IV. Allegro");
            let bottom = last.absolute_position().y + last.size().height;
            assert!(
                (600.0..=640.0).contains(&bottom),
                "the last row ends at {bottom}"
            );
            window.set_size(slint::LogicalSize::new(1100.0, 760.0));
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

            // A motion cover that comes after its album's page was left
            // doesn't take the page shown now.
            SLOW_MOTION.store(true, Relaxed);
            ui.invoke_open("album:a2".into());
            wait_for("the classical album", || ui.get_header_title() == "Symphonies").await;
            ui.invoke_open("album:a1".into());
            wait_for("the album left for", || ui.get_header_title() == "Night Harbour").await;
            idle(Duration::from_millis(1000)).await;
            assert_ne!(ui.get_header_image().size().width, 2);
            SLOW_MOTION.store(false, Relaxed);

            // A composer's page opened before the composers have arrived asks
            // for them too, for the picture.
            ui.invoke_open("composer:r9".into());
            wait_for("his picture", || ui.get_header_image().size().width > 0).await;
            // With Fonoteca's compositions, composers are round tiles and
            // filter like the artists.
            click(&ui, "Composers");
            wait_for("the composers", || ui.get_tiles().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Composers);
            assert_eq!(ui.get_section(), 6);
            let tile = ui.get_tiles().row_data(0).unwrap();
            assert_eq!(
                (tile.title.as_str(), tile.link.as_str()),
                ("Ludwig van Beethoven", "composer:r9")
            );
            assert_eq!(tile.names.row_data(0).unwrap().label, "2 compositions");
            ui.set_composer_filter("moz".into());
            ui.invoke_filter_composers();
            wait_for("one composer", || ui.get_tiles().row_count() == 1).await;
            assert_eq!(ui.get_tiles().row_data(0).unwrap().link, "composer:r10");
            ui.set_composer_filter("".into());
            ui.invoke_filter_composers();
            wait_for("both composers", || ui.get_tiles().row_count() == 2).await;
            // A composer's page lists their pieces, without their own name on
            // each.
            click(&ui, "Ludwig van Beethoven");
            wait_for("his pieces", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Composer);
            assert_eq!(lit(&ui), "Composers");
            assert_eq!(ui.get_header_title(), "Ludwig van Beethoven");
            assert_eq!(ui.get_header_facts(), "2 compositions");
            let rows = |ui: &AppWindow| -> Vec<(String, String, String, String, Vec<String>)> {
                ui.get_tracks()
                    .iter()
                    .map(|track| {
                        let names = track.names.iter().map(|name| name.link.into()).collect();
                        (
                            track.title.into(),
                            track.years.into(),
                            track.duration.into(),
                            track.link.into(),
                            names,
                        )
                    })
                    .collect()
            };
            let row = |title: &str, years: &str, end: &str, link: &str, names: &[&str]| {
                let names = names.iter().map(|name| name.to_string()).collect();
                (title.into(), years.into(), end.into(), link.into(), names)
            };
            assert_eq!(
                rows(&ui),
                [
                    row(
                        "Egmont Overture",
                        "1809 – 1810",
                        "1 album",
                        "composition:w9",
                        &[]
                    ),
                    row(
                        "Symphony No. 5",
                        "1804 – 1808",
                        "2 albums",
                        "composition:w5",
                        &[]
                    ),
                ]
            );
            // The compositions page is a table like the tracks page's, with
            // the composers linked, found by name or composer.
            click(&ui, "Compositions");
            wait_for("the compositions", || ui.get_track_lines().row_count() == 3).await;
            assert_eq!(ui.get_page(), Page::Compositions);
            let headings: Vec<String> = ui
                .get_track_columns()
                .iter()
                .map(|column| column.title.into())
                .collect();
            assert_eq!(
                headings,
                [
                    "Cover",
                    "Composition",
                    "Composer",
                    "Date",
                    "Period",
                    "Form",
                    "Performances",
                    "Loved"
                ]
            );
            let piece = |ui: &AppWindow, row: usize| {
                let line = ui.get_track_lines().row_data(row).unwrap();
                let cells: Vec<Vec<(String, String)>> = line
                    .cells
                    .iter()
                    .map(|names| {
                        names
                            .iter()
                            .map(|name| (name.label.into(), name.link.into()))
                            .collect()
                    })
                    .collect();
                cells
            };
            let plain = |label: &str| vec![(label.to_owned(), String::new())];
            let composers = vec![
                (
                    "Wolfgang Amadeus Mozart".to_owned(),
                    "composer:r10".to_owned(),
                ),
                ("Franz Xaver Süssmayr".to_owned(), "composer:r11".to_owned()),
            ];
            let requiem = vec![("Requiem".to_owned(), "composition:w7".to_owned())];
            assert_eq!(
                piece(&ui, 1),
                [
                    vec![],
                    requiem,
                    composers,
                    vec![],
                    plain("Classical"),
                    plain("Requiem"),
                    plain("3"),
                    vec![]
                ]
            );
            assert!(ui.get_track_lines().row_data(1).unwrap().loved);
            assert_eq!(piece(&ui, 2)[3], plain("1804 – 1808"));
            // By the year begun, either way with the undated last.
            let order = |ui: &AppWindow| -> Vec<String> {
                ui.get_track_lines()
                    .iter()
                    .map(|line| {
                        line.cells
                            .row_data(1)
                            .unwrap()
                            .row_data(0)
                            .unwrap()
                            .link
                            .into()
                    })
                    .collect()
            };
            // Loved ones last, by the hearts the server sent.
            ui.invoke_sort_tracks(7);
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w5", "composition:w7"]
            );
            ui.invoke_sort_tracks(3);
            assert_eq!(
                order(&ui),
                ["composition:w5", "composition:w9", "composition:w7"]
            );
            ui.invoke_sort_tracks(3);
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w5", "composition:w7"]
            );
            // The columns hidden at first hold the rest of what the server
            // knows of a piece.
            let more = ["Key", "Movements", "Duration", "Lyricists", "Languages"];
            let choose = |ui: &AppWindow, title: &str| {
                let at = ui
                    .get_track_choices()
                    .iter()
                    .position(|choice| choice.title == title)
                    .unwrap();
                ui.invoke_toggle_track_column(at as i32);
            };
            for title in more {
                choose(&ui, title);
            }
            let heading = |ui: &AppWindow, title: &str| {
                ui.get_track_columns()
                    .iter()
                    .position(|column| column.title == title)
                    .unwrap()
            };
            let cells = piece(&ui, 2);
            let cell = |title: &str| cells[heading(&ui, title)].clone();
            assert_eq!(cell("Key"), plain("D minor"));
            assert_eq!(cell("Movements"), plain("14"));
            assert_eq!(cell("Duration"), plain("55:00"));
            assert_eq!(
                cell("Lyricists"),
                [
                    ("Thomas of Celano".to_owned(), "artist:r12".to_owned()),
                    ("Liturgy".to_owned(), String::new())
                ]
            );
            assert_eq!(cell("Languages"), plain("Latin, German"));
            ui.invoke_sort_tracks(heading(&ui, "Movements") as i32);
            assert_eq!(
                order(&ui),
                ["composition:w5", "composition:w7", "composition:w9"]
            );
            ui.invoke_sort_tracks(heading(&ui, "Duration") as i32);
            assert_eq!(
                order(&ui),
                ["composition:w7", "composition:w9", "composition:w5"]
            );
            for title in more {
                choose(&ui, title);
            }
            ui.invoke_sort_tracks(3);
            ui.invoke_sort_tracks(3);
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w5", "composition:w7"]
            );
            let saved = crate::prefs::load();
            assert_eq!(saved.composition_sort.as_deref(), Some("Date"));
            assert!(saved.composition_descending);
            // Its columns are picked apart from the tracks table's, and the
            // composition's name takes the room left over.
            let mbid = ui
                .get_track_choices()
                .iter()
                .position(|choice| choice.title == "MusicBrainz ID")
                .unwrap();
            ui.invoke_toggle_track_column(mbid as i32);
            let last = ui.get_track_columns().iter().last().unwrap();
            assert_eq!(last.title, "MusicBrainz ID");
            let line = ui.get_track_lines().row_data(0).unwrap();
            assert_eq!(line.cells.row_count(), 9);
            assert!(ui.get_track_columns().row_data(1).unwrap().stretch);
            let saved = crate::prefs::load().composition_columns.unwrap();
            assert!(saved.contains(&"MusicBrainz ID".to_owned()));
            // Its rows are the tracks table's: the heart loves the piece on
            // the server, a right-click selects it, the menu goes to its
            // composers.
            assert_eq!(line.artists.row_data(0).unwrap().link, "composer:r9");
            ui.invoke_toggle_line_love(0);
            assert!(ui.get_track_lines().row_data(0).unwrap().loved);
            wait_for("the piece's star", || {
                STARRED.lock().unwrap().contains(&"w9".to_owned())
            })
            .await;
            ui.invoke_select_line(1, false);
            assert!(ui.get_track_lines().row_data(1).unwrap().selected);
            assert_eq!(ui.get_selected_tiles(), 1);
            // It stays selected in another order.
            ui.invoke_sort_tracks(3);
            assert!(ui.get_track_lines().row_data(0).unwrap().selected);
            ui.invoke_sort_tracks(3);
            ui.invoke_clear_selection();
            assert!(!ui.get_track_lines().row_data(1).unwrap().selected);
            ui.set_composition_filter("beethoven SYMPH".into());
            ui.invoke_filter_compositions();
            wait_for("one composition", || ui.get_track_lines().row_count() == 1).await;
            // Its page has a row for each album that holds it, which opens
            // the album.
            click(&ui, "Symphony No. 5");
            wait_for("the symphony", || {
                ui.get_page() == Page::Composition && ui.get_tracks().row_count() == 2
            })
            .await;
            assert_eq!(ui.get_header_title(), "Symphony No. 5");
            assert_eq!(lit(&ui), "Compositions");
            assert_eq!(
                ui.get_header_facts(),
                "1804 – 1808 · 4 movements · 2 performances"
            );
            let header: Vec<String> = ui
                .get_header_names()
                .iter()
                .map(|n| n.link.into())
                .collect();
            assert_eq!(header, ["composer:r9"]);
            assert_eq!(
                rows(&ui),
                [
                    row("Symphonies", "1974", "17:09", "album:a2", &["artist:r7"]),
                    row(
                        "Live in Munich",
                        "1982",
                        "31:40",
                        "album:a3",
                        &["artist:r8", "artist:r12"]
                    ),
                ]
            );
            let performed: Vec<String> =
                ui.get_tracks().iter().map(|t| t.performed.into()).collect();
            assert_eq!(
                performed,
                ["Performed 2 Mar 1974 – Apr 1974, Musikverein", ""]
            );
            click(&ui, "Live in Munich");
            wait_for("its album", || ui.get_page() == Page::Album).await;
            click(&ui, "Back");
            wait_for("the symphony again", || ui.get_tracks().row_count() == 2).await;
            click(&ui, "Ludwig van Beethoven");
            wait_for("his page", || ui.get_page() == Page::Composer).await;
            // The filter is kept for the session.
            click(&ui, "Compositions");
            assert_eq!(ui.get_track_lines().row_count(), 1);
            ui.set_composition_filter("".into());
            ui.invoke_filter_compositions();
            wait_for("all compositions", || ui.get_track_lines().row_count() == 3).await;

            // The tracks page lists every song in the columns picked, a page
            // at a time.
            click(&ui, "Tracks");
            wait_for("the tracks", || ui.get_track_lines().row_count() == 100).await;
            assert_eq!(ui.get_page(), Page::Tracks);
            assert!(ui.get_more_tiles());
            let titles = |ui: &AppWindow| -> Vec<String> {
                ui.get_track_columns()
                    .iter()
                    .map(|column| column.title.into())
                    .collect()
            };
            assert_eq!(
                titles(&ui),
                ["Cover", "Title", "Artists", "Album", "Duration", "Loved"]
            );
            let cell = |ui: &AppWindow, row: usize, column: usize| -> Vec<(String, String)> {
                let line = ui.get_track_lines().row_data(row).unwrap();
                let names = line.cells.row_data(column).unwrap();
                names
                    .iter()
                    .map(|name| (name.label.into(), name.link.into()))
                    .collect()
            };
            let one = |label: &str, link: &str| vec![(label.to_owned(), link.to_owned())];
            assert_eq!(cell(&ui, 0, 1), one("Harbour Lights", "song:t1"));
            assert_eq!(cell(&ui, 0, 2), one("Mara Lind", "artist:r1"));
            assert_eq!(cell(&ui, 0, 3), one("Night Harbour", "album:a1"));
            assert_eq!(cell(&ui, 0, 4), one("4:12", ""));
            assert!(cell(&ui, 1, 3).is_empty());
            // A title opens its song's page: what the server's getSong says
            // of it, under the names the columns have.
            click(&ui, "Harbour Lights");
            wait_for("the song", || ui.get_details().row_count() > 0).await;
            assert_eq!(ui.get_page(), Page::Track);
            assert_eq!(lit(&ui), "Tracks");
            assert_eq!(ui.get_header_title(), "Harbour Lights");
            assert_eq!(ui.get_header_facts(), "2021 · 4:12");
            let header: Vec<String> = ui
                .get_header_names()
                .iter()
                .map(|n| n.link.into())
                .collect();
            assert_eq!(header, ["artist:r1"]);
            let details: Vec<(String, Vec<(String, String)>)> = ui
                .get_details()
                .iter()
                .map(|detail| {
                    let names = detail
                        .names
                        .iter()
                        .map(|name| (name.label.into(), name.link.into()))
                        .collect();
                    (detail.label.into(), names)
                })
                .collect();
            let detail = |label: &str, names: &[(&str, &str)]| {
                let names = names
                    .iter()
                    .map(|(label, link)| (label.to_string(), link.to_string()))
                    .collect();
                (label.to_owned(), names)
            };
            assert_eq!(
                details,
                [
                    detail("Artists", &[("Mara Lind", "artist:r1")]),
                    detail("Album", &[("Night Harbour", "album:a1")]),
                    detail(
                        "Contributors",
                        &[("Anna Bach (performer, piano)", "artist:r3")]
                    ),
                    detail("Year", &[("2021", "")]),
                    detail("Genres", &[("Folk, Pop", "")]),
                    detail("Duration", &[("4:12", "")]),
                    detail("Starred", &[("2024-01-01", "")]),
                    detail("Track gain", &[("-6.20 dB", "")]),
                    detail("Sample rate", &[("44.1 kHz", "")]),
                    detail("Size", &[("4.2 MB", "")]),
                ]
            );
            click(&ui, "Night Harbour");
            wait_for("its album", || ui.get_page() == Page::Album).await;
            click(&ui, "Back");
            wait_for("the song again", || ui.get_details().row_count() == 10).await;
            click(&ui, "Back");
            wait_for("the tracks once more", || {
                ui.get_track_lines().row_count() == 100
            })
            .await;
            // The rows in view get their album's cover, songs without one
            // none.
            ui.root_element().query_descendants().find_all();
            let cover = |ui: &AppWindow, row: usize| {
                ui.get_track_lines()
                    .row_data(row)
                    .unwrap()
                    .cover
                    .size()
                    .width
            };
            wait_for("a row's cover", || cover(&ui, 0) > 0).await;
            assert_eq!(cover(&ui, 1), 0);
            let width = ui.get_track_table_width();
            ui.invoke_show_more();
            assert_eq!(ui.get_track_lines().row_count(), 200);
            ui.invoke_show_more();
            assert_eq!(ui.get_track_lines().row_count(), 230);
            wait_for("the empty part after the last", || !ui.get_more_tiles()).await;
            assert_eq!(ui.get_track_lines().row_count(), 230);
            // The filter finds songs by title, artist or album.
            ui.set_track_filter("mara night".into());
            ui.invoke_filter_tracks();
            wait_for("one track", || ui.get_track_lines().row_count() == 1).await;
            assert_eq!(cell(&ui, 0, 1), one("Harbour Lights", "song:t1"));
            ui.set_track_filter("".into());
            ui.invoke_filter_tracks();
            wait_for("the tracks again", || {
                ui.get_track_lines().row_count() == 100
            })
            .await;
            // A song loved on another page is loved here too.
            let loved =
                |ui: &AppWindow, row: usize| ui.get_track_lines().row_data(row).unwrap().loved;
            assert!(loved(&ui, 0));
            assert!(!loved(&ui, 1));
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            click_heart(&ui, false);
            wait_for("the star", || {
                STARRED.lock().unwrap().contains(&"s9".to_owned())
            })
            .await;
            click(&ui, "Back");
            assert!(loved(&ui, 1));
            // A cover fetched before is there at once.
            assert!(cover(&ui, 0) > 0);
            // The heart stars the song.
            click_heart(&ui, false);
            assert!(loved(&ui, 2));
            wait_for("the star", || {
                STARRED.lock().unwrap().contains(&"t3".to_owned())
            })
            .await;
            // A column for every field of a song; the rows follow the choice.
            let choice = |ui: &AppWindow, title: &str| {
                let choices = ui.get_track_choices();
                let at = choices.iter().position(|c| c.title == title);
                at.unwrap_or_else(|| panic!("no {title} column")) as i32
            };
            for title in [
                "Genres",
                "Contributors",
                "Track gain",
                "Size",
                "Sample rate",
                "Year",
            ] {
                ui.invoke_toggle_track_column(choice(&ui, title));
            }
            ui.invoke_toggle_track_column(choice(&ui, "Artists"));
            // Saved on this device at once.
            let columns = crate::prefs::load().track_columns.unwrap();
            assert!(columns.contains(&"Year".to_owned()));
            assert!(!columns.contains(&"Artists".to_owned()));
            assert_eq!(
                titles(&ui),
                [
                    "Cover",
                    "Title",
                    "Album",
                    "Contributors",
                    "Year",
                    "Genres",
                    "Duration",
                    "Loved",
                    "Track gain",
                    "Sample rate",
                    "Size"
                ]
            );
            assert!(ui.get_track_table_width() > width);
            // A finger's drag more sideways than up or down pans a table
            // wider than the page, as far as its edges.
            let page = ui
                .root_element()
                .query_descendants()
                .match_accessible_role(AccessibleRole::Main)
                .find_first()
                .unwrap();
            let first_title = cell(&ui, 0, 1)[0].0.clone();
            let title_x = || button(&ui, &first_title).absolute_position().x;
            let unpanned = title_x();
            ui.global::<Input>().set_finger(true);
            drag(&ui, middle(&page), &[(0, -20.0, 4.0), (0, -60.0, 6.0)], 100).await;
            let panned = unpanned - title_x();
            assert!((panned - 60.0).abs() < 0.01, "panned {panned}px");
            // Flung sideways, it eases into its edge, though more may come
            // under it. Only the columns in sight are there.
            assert!(ui.get_more_tiles());
            let size_x = || find(&ui, "Sort by Size").map(|size| size.absolute_position().x);
            drag(&ui, middle(&page), &[(0, -20.0, 0.0), (0, -2000.0, 0.0)], 100).await;
            let edge = size_x().unwrap();
            drag(&ui, middle(&page), &[(0, 20.0, 0.0), (0, 300.0, 0.0)], 100).await;
            drag(&ui, middle(&page), &[(0, -20.0, 0.0), (0, -52.0, 0.0)], 0).await;
            let at_edge = || size_x().is_some_and(|x| (x - edge).abs() < 0.01);
            eases_into("the right edge", at_edge).await;
            drag(&ui, middle(&page), &[(0, 20.0, 0.0), (0, 2000.0, 0.0)], 100).await;
            assert!((title_x() - unpanned).abs() < 0.01, "back at its left edge");
            ui.global::<Input>().set_finger(false);
            assert_eq!(ui.get_track_lines().row_count(), 100);
            assert_eq!(cell(&ui, 0, 2), one("Night Harbour", "album:a1"));
            assert_eq!(
                cell(&ui, 0, 3),
                one("Anna Bach (performer, piano)", "artist:r3")
            );
            assert_eq!(cell(&ui, 0, 4), one("2021", ""));
            assert_eq!(cell(&ui, 0, 5), one("Folk, Pop", ""));
            assert_eq!(cell(&ui, 0, 8), one("-6.20 dB", ""));
            assert_eq!(cell(&ui, 0, 9), one("44.1 kHz", ""));
            assert_eq!(cell(&ui, 0, 10), one("4.2 MB", ""));
            let choices = ui.get_track_choices();
            assert!(choices.iter().any(|c| c.title == "Genres" && c.shown));
            assert!(choices.iter().any(|c| c.title == "Artists" && !c.shown));
            // The heart's row keeps it when the columns change.
            assert!(loved(&ui, 2));
            // A click on a heading sorts by it, a second turns that around;
            // songs without the value go last either way.
            let first = |ui: &AppWindow| -> Vec<String> {
                (0..3).map(|row| cell(ui, row, 1)[0].0.clone()).collect()
            };
            let sorted = |ui: &AppWindow| -> Vec<(String, bool)> {
                ui.get_track_columns()
                    .iter()
                    .filter(|column| column.sorted)
                    .map(|column| (column.title.into(), column.descending))
                    .collect()
            };
            click(&ui, "Sort by Title");
            assert_eq!(first(&ui), ["Fugue", "Harbour Lights", "Song 10"]);
            assert_eq!(sorted(&ui), [("Title".to_owned(), false)]);
            assert_eq!(ui.get_track_lines().row_count(), 100);
            click(&ui, "Sort by Title");
            assert_eq!(first(&ui), ["Song 99", "Song 98", "Song 97"]);
            assert_eq!(sorted(&ui), [("Title".to_owned(), true)]);
            let column = |ui: &AppWindow, title: &str| -> i32 {
                titles(ui).iter().position(|t| t == title).unwrap() as i32
            };
            ui.invoke_sort_tracks(column(&ui, "Duration"));
            assert_eq!(first(&ui), ["Song 3", "Song 4", "Song 5"]);
            ui.invoke_sort_tracks(column(&ui, "Duration"));
            assert_eq!(first(&ui), ["Harbour Lights", "Fugue", "Song 3"]);
            ui.invoke_sort_tracks(column(&ui, "Year"));
            assert_eq!(first(&ui), ["Harbour Lights", "Fugue", "Song 3"]);
            ui.invoke_sort_tracks(column(&ui, "Year"));
            assert_eq!(first(&ui), ["Harbour Lights", "Fugue", "Song 3"]);
            assert_eq!(sorted(&ui), [("Year".to_owned(), true)]);
            // The heart follows its song through the sort.
            ui.invoke_sort_tracks(column(&ui, "Loved"));
            assert_eq!(first(&ui), ["Song 4", "Song 5", "Song 6"]);
            ui.invoke_sort_tracks(column(&ui, "Loved"));
            assert_eq!(first(&ui), ["Harbour Lights", "Fugue", "Song 3"]);
            assert!(loved(&ui, 2));
            ui.invoke_toggle_line_love(2);
            wait_for("the refused unstar", || ui.get_page_problem() != "").await;
            assert!(loved(&ui, 2));
            ui.set_page_problem("".into());
            // Refused after the page has changed, the heart goes back on its
            // song, not on the row in its place on the compositions page.
            assert!(loved(&ui, 1));
            let unstars = REFUSED.load(Relaxed);
            ui.invoke_toggle_line_love(1);
            click(&ui, "Compositions");
            let hearts = |ui: &AppWindow| -> Vec<bool> {
                ui.get_track_lines().iter().map(|line| line.loved).collect()
            };
            assert_eq!(hearts(&ui), [true, false, true]);
            wait_for("the song's refused unstar", || {
                REFUSED.load(Relaxed) > unstars
            })
            .await;
            // A piece's refused heart goes back too; its answer comes after
            // the song's, so that one has been handled by then.
            ui.invoke_toggle_line_love(2);
            assert_eq!(hearts(&ui), [true, false, false]);
            wait_for("the piece's refused unstar", || ui.get_page_problem() != "").await;
            assert_eq!(hearts(&ui), [true, false, true]);
            ui.set_page_problem("".into());
            click(&ui, "Tracks");
            assert!(loved(&ui, 1));
            // The same the other way: the symphony's refused heart doesn't
            // land on the loved song in its place on the tracks page...
            click(&ui, "Compositions");
            let refused = REFUSED.load(Relaxed);
            ui.invoke_toggle_line_love(1);
            click(&ui, "Tracks");
            wait_for("the symphony's refused star", || {
                REFUSED.load(Relaxed) > refused
            })
            .await;
            ui.invoke_toggle_line_love(0);
            wait_for("the song's refused unstar", || ui.get_page_problem() != "").await;
            ui.set_page_problem("".into());
            assert!(loved(&ui, 0) && loved(&ui, 1));
            // ...nor a playlist song's on the unloved symphony.
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            let fugue = ui
                .get_tracks()
                .iter()
                .position(|song| song.title == "Fugue")
                .unwrap() as i32;
            let refused = REFUSED.load(Relaxed);
            ui.invoke_toggle_love(fugue);
            ui.invoke_toggle_love(fugue);
            click(&ui, "Compositions");
            wait_for("the song's refused unstar", || {
                REFUSED.load(Relaxed) > refused
            })
            .await;
            ui.invoke_toggle_line_love(0);
            wait_for("the piece's refused unstar", || ui.get_page_problem() != "").await;
            ui.set_page_problem("".into());
            assert_eq!(hearts(&ui), [true, false, true]);
            click(&ui, "Tracks");
            assert!(loved(&ui, 1));
            ui.invoke_sort_tracks(column(&ui, "Year"));
            // A right-click selects a row, with the cards' selection, and it
            // stays selected through sorting. Its menu goes to its album and
            // artists.
            let selected =
                |ui: &AppWindow, row: usize| ui.get_track_lines().row_data(row).unwrap().selected;
            let before = ui.get_selected_tiles();
            ui.invoke_select_line(0, false);
            assert!(selected(&ui, 0));
            assert_eq!(ui.get_selected_tiles(), before + 1);
            ui.invoke_sort_tracks(column(&ui, "Title"));
            assert!(!selected(&ui, 0) && selected(&ui, 1));
            ui.invoke_sort_tracks(column(&ui, "Year"));
            assert!(selected(&ui, 0));
            let line = ui.get_track_lines().row_data(0).unwrap();
            assert_eq!(line.album.link, "album:a1");
            let artists: Vec<String> = line.artists.iter().map(|a| a.link.into()).collect();
            assert_eq!(artists, ["artist:r1"]);
            assert_eq!(ui.get_track_lines().row_data(1).unwrap().album.link, "");
            ui.invoke_clear_selection();
            assert!(!selected(&ui, 0));
            assert_eq!(ui.get_selected_tiles(), 0);
            // With Shift, every row from the one last picked to it, in the
            // order shown.
            ui.invoke_select_line(1, false);
            ui.invoke_select_line(3, true);
            let rows: Vec<bool> = (0..5).map(|row| selected(&ui, row)).collect();
            assert_eq!(rows, [false, true, true, true, false]);
            assert_eq!(ui.get_selected_tiles(), 3);
            ui.invoke_clear_selection();
            // So does a click on a row with Shift held, beside its links.
            let title = column(&ui, "Title") as usize;
            let beside = |row: usize| {
                let line = ui.get_track_lines().row_data(row).unwrap();
                let name = line.cells.row_data(title).unwrap().row_data(0).unwrap();
                let link = button(&ui, &name.label);
                let at = link.absolute_position();
                slint::LogicalPosition::new(at.x - 4.0, at.y + link.size().height / 2.0)
            };
            ui.invoke_select_line(1, false);
            let window = ui.window();
            let shift = slint::platform::Key::Shift;
            window.dispatch_event(WindowEvent::KeyPressed { text: shift.into() });
            let (position, left) = (beside(3), PointerEventButton::Left);
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: left,
            });
            window.dispatch_event(WindowEvent::KeyReleased { text: shift.into() });
            let rows: Vec<bool> = (0..5).map(|row| selected(&ui, row)).collect();
            assert_eq!(rows, [false, true, true, true, false]);
            ui.invoke_clear_selection();
            // A finger held on a row picks it; a mouse button held, a finger
            // that moves, or one lifted sooner, doesn't.
            hold(&ui, beside(2), 0.0, 800).await;
            assert!(!selected(&ui, 2));
            ui.global::<Input>().set_touch(true);
            hold(&ui, beside(2), 20.0, 800).await;
            assert!(!selected(&ui, 2));
            hold(&ui, beside(2), 0.0, 300).await;
            idle(Duration::from_millis(500)).await;
            assert!(!selected(&ui, 2));
            hold(&ui, beside(2), 0.0, 800).await;
            let rows: Vec<bool> = (0..5).map(|row| selected(&ui, row)).collect();
            assert_eq!(rows, [false, false, true, false, false]);
            // The next tap on it is a tap again.
            hold(&ui, beside(2), 0.0, 0).await;
            assert!(!selected(&ui, 2));
            ui.global::<Input>().set_touch(false);
            ui.invoke_clear_selection();
            // Kept for the session, columns, order and all.
            click(&ui, "Albums");
            click(&ui, "Tracks");
            assert_eq!(ui.get_track_lines().row_count(), 100);
            assert!(titles(&ui).contains(&"Year".to_owned()));
            assert_eq!(sorted(&ui), [("Year".to_owned(), false)]);
            assert_eq!(first(&ui), ["Harbour Lights", "Fugue", "Song 3"]);
            // The order is saved on this device too.
            ui.invoke_sort_tracks(column(&ui, "Year"));
            let saved = crate::prefs::load();
            assert_eq!(saved.track_sort.as_deref(), Some("Year"));
            assert!(saved.track_descending);
            sort(1, true);

            // The folders page lists the music folders' top level in a table
            // like the tracks page's, folders first, then the files beside
            // them. A folder's title opens it, a song's its page.
            click(&ui, "Folders");
            wait_for("the folders", || ui.get_track_lines().row_count() == 3).await;
            assert_eq!(ui.get_page(), Page::Folders);
            assert_eq!(ui.get_header_title(), "Folders");
            assert_eq!(ui.get_section(), 8);
            let entry = |ui: &AppWindow, row: usize| -> (String, String) {
                let title = titles(ui).iter().position(|t| t == "Title").unwrap();
                let line = ui.get_track_lines().row_data(row).unwrap();
                let name = line.cells.row_data(title).unwrap().row_data(0).unwrap();
                (name.label.into(), name.link.into())
            };
            let named = |label: &str, link: &str| (label.to_owned(), link.to_owned());
            assert_eq!(entry(&ui, 0), named("Mara Lind", "folder:d1"));
            assert_eq!(entry(&ui, 1), named("Singles", "folder:d3"));
            assert_eq!(entry(&ui, 2), named("Loose Song", "song:t9"));
            let line = |ui: &AppWindow, row: usize| ui.get_track_lines().row_data(row).unwrap();
            assert!(line(&ui, 0).folder && !line(&ui, 2).folder);
            assert!(line(&ui, 1).loved && !line(&ui, 0).loved);
            // Its columns and filter are its own.
            assert_eq!(
                titles(&ui),
                ["Cover", "Title", "Artists", "Album", "Duration", "Loved"]
            );
            ui.set_folder_filter("sing".into());
            ui.invoke_filter_folders();
            wait_for("one folder", || ui.get_track_lines().row_count() == 1).await;
            assert_eq!(entry(&ui, 0), named("Singles", "folder:d3"));
            assert_eq!(ui.get_track_filter(), "");
            ui.set_folder_filter("".into());
            ui.invoke_filter_folders();
            wait_for("the three again", || ui.get_track_lines().row_count() == 3).await;
            click(&ui, "Mara Lind");
            wait_for("her folder", || ui.get_header_title() == "Mara Lind").await;
            assert_eq!(ui.get_section(), 8, "a folder is among the folders");
            assert_eq!(entry(&ui, 0), named("Night Harbour", "folder:d2"));
            assert_eq!(entry(&ui, 1), named("Harbour Lights", "song:t1"));
            // A folder shows its cover, as an album does.
            ui.root_element().query_descendants().find_all();
            wait_for("the folder's cover", || line(&ui, 0).cover.size().width > 0).await;
            // The heart loves a song, a right-click selects a folder.
            ui.invoke_toggle_line_love(1);
            assert!(line(&ui, 1).loved);
            wait_for("the song's star", || {
                STARRED.lock().unwrap().contains(&"t1".to_owned())
            })
            .await;
            ui.invoke_select_line(0, false);
            assert!(line(&ui, 0).selected);
            ui.invoke_clear_selection();
            // A song row goes in a playlist; of a selection, only its songs.
            ui.invoke_add_line_to_playlist(1, "p1".into());
            wait_for("the song added", || {
                PLAYLIST_WRITES.lock().unwrap().last().map(String::as_str)
                    == Some("updatePlaylist playlistId=p1&songIdToAdd=t1")
            })
            .await;
            // Its notice comes before the next one, which may come at once.
            wait_for("its notice", || ui.get_notice() == "Added 1 track to Road Trip.").await;
            PLAYLIST_WRITES.lock().unwrap().clear();
            ui.invoke_select_line(0, false);
            ui.invoke_add_selection_to_playlist("p1".into());
            wait_for("nothing added", || {
                ui.get_notice() == "Nothing selected goes in a playlist."
            })
            .await;
            assert!(PLAYLIST_WRITES.lock().unwrap().is_empty());
            // It sorts by a heading, kept apart from the tracks table's.
            ui.invoke_sort_tracks(1);
            assert_eq!(entry(&ui, 0), named("Harbour Lights", "song:t1"));
            let saved = crate::prefs::load();
            assert_eq!(saved.folder_sort.as_deref(), Some("Title"));
            assert_eq!(saved.track_sort.as_deref(), Some("Year"));
            click(&ui, "Harbour Lights");
            wait_for("the song", || ui.get_details().row_count() > 0).await;
            assert_eq!(ui.get_page(), Page::Track);
            click(&ui, "Back");
            wait_for("her folder again", || ui.get_track_lines().row_count() == 2).await;
            // The song loved here is still loved, and a column hidden here
            // leaves its rows at once and stays hidden.
            assert!(line(&ui, 0).loved);
            let album = ui
                .get_track_choices()
                .iter()
                .position(|choice| choice.title == "Album")
                .unwrap();
            ui.invoke_toggle_track_column(album as i32);
            assert!(!titles(&ui).contains(&"Album".to_owned()));
            assert_eq!(line(&ui, 0).cells.row_count(), titles(&ui).len());
            let saved = crate::prefs::load().folder_columns.unwrap();
            assert!(!saved.contains(&"Album".to_owned()));
            ui.invoke_toggle_track_column(album as i32);
            click(&ui, "Back");
            wait_for("the top level again", || {
                ui.get_header_title() == "Folders" && ui.get_track_lines().row_count() == 3
            })
            .await;
            click(&ui, "Tracks");
            wait_for("the tracks", || ui.get_track_lines().row_count() == 100).await;

            // A second start finds the saved account, with its server down.
            DOWN.store(true, Relaxed);
            let again = start().unwrap();
            wait_for("the offline note", || again.get_offline() != "").await;
            assert_eq!(again.get_step(), Step::Done);
            DOWN.store(false, Relaxed);
            again
                .window()
                .set_size(slint::LogicalSize::new(1100.0, 760.0));
            click(&again, "Try again");
            // It opens on the home page: a welcome, the library's counts and
            // its newest albums.
            let stats = |ui: &AppWindow| -> Vec<(String, String)> {
                let stats = ui.get_stats();
                stats
                    .iter()
                    .map(|stat| (stat.label.into(), stat.value.into()))
                    .collect()
            };
            wait_for("the home page", || {
                again.get_tiles().row_count() == 24
                    && stats(&again).iter().all(|(_, value)| value != "…")
            })
            .await;
            assert_eq!(again.get_offline(), "");
            assert_eq!(again.get_page(), Page::Home);
            assert_eq!(again.get_username(), "joe");
            assert_eq!(
                stats(&again),
                [
                    ("Artists", "122"),
                    ("Albums", "603"),
                    ("Tracks", "2"),
                    ("Composers", "2"),
                    ("Compositions", "3"),
                ]
                .map(|(label, value)| (label.to_owned(), value.to_owned()))
            );
            // Each opens its page.
            let links: Vec<String> = again.get_stats().iter().map(|s| s.link.into()).collect();
            assert_eq!(
                links,
                ["artists", "albums", "tracks", "composers", "compositions"]
            );
            let newest: Vec<String> = again.get_tiles().iter().map(|t| t.link.into()).collect();
            assert_eq!(newest[..4], ids(&["b2", "b3", "b1", "a1"]));
            // The shelf comes to rest with a tile at its left edge: Next and
            // Previous move a width, three tiles here, a drag on the nearest
            // tile, a fling on further, and a sideways scroll on to the next
            // tile its way.
            let x = |title: &str| find(&again, title).map_or(f32::NAN, |b| b.absolute_position().x);
            let tile =
                |row: usize| -> String { again.get_tiles().row_data(row).unwrap().title.into() };
            let (left, pitch) = (x(&tile(0)), x(&tile(1)) - x(&tile(0)));
            // Whether the `row`th tile is at the left edge.
            let rests = |row: usize| move || (x(&tile(row)) - left).abs() < 0.5;
            // Where the first tile is, drawn or not.
            let shelf_at = || {
                (0..again.get_tiles().row_count())
                    .find_map(|row| {
                        let at = x(&tile(row));
                        at.is_finite().then_some(at - row as f32 * pitch)
                    })
                    .unwrap_or(f32::NAN)
            };
            click(&again, "Next in Recently added");
            wait_for("the next three", rests(3)).await;
            let y = {
                let tile = button(&again, "Night Harbour");
                tile.absolute_position().y + tile.size().height / 2.0
            };
            let at = |x: f32| slint::LogicalPosition::new(x, y);
            let window = again.window();
            // Drags from the second tile `by` tiles sideways, slowly, and
            // stops before letting go.
            let slide = async |by: f32| {
                let path: Vec<_> =
                    (1..=6).map(|step| (40, by * pitch * step as f32 / 6.0, 0.0)).collect();
                drag(&again, at(left + pitch), &path, 60).await;
            };
            slide(-1.2).await;
            assert!(!again.global::<Input>().get_sliding());
            wait_for("the nearest tile on", rests(4)).await;
            assert_eq!(again.get_page(), Page::Home, "a drag opens nothing");
            slide(0.3).await;
            settled(shelf_at).await;
            assert!(rests(4)(), "a short drag goes back");
            // At 15 tiles a second, a fling glides on past the tile it was
            // let go nearest to.
            let flick = [
                (0, -0.12 * pitch, 0.0),
                (16, -0.36 * pitch, 0.0),
                (16, -0.6 * pitch, 0.0),
            ];
            drag(&again, at(left + pitch), &flick, 0).await;
            let resting = (left - settled(shelf_at).await) / pitch;
            assert!(
                resting > 5.99 && (resting - resting.round()).abs() < 0.01,
                "came to rest on {resting}"
            );
            while !rests(0)() {
                click(&again, "Previous in Recently added");
                settled(shelf_at).await;
            }
            // A drag past its start goes nowhere, and opens nothing.
            slide(0.3).await;
            settled(shelf_at).await;
            assert!(rests(0)());
            assert_eq!(again.get_page(), Page::Home);
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: at(left + pitch),
                delta_x: -0.3 * pitch,
                delta_y: 0.0,
            });
            wait_for("one tile on", rests(1)).await;
            // Scrolling down over it scrolls the page, not the shelf.
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: at(left + pitch),
                delta_x: 0.0,
                delta_y: -40.0,
            });
            let later = Instant::now() + Duration::from_millis(400);
            wait_for("a pause", || Instant::now() > later).await;
            assert!(rests(1)(), "the shelf stays put");
            // A finger's drag mostly sideways moves only the shelf; one
            // mostly up or down only the page. Its first move finds the
            // shelf, and the page's drag slides the shelf from the next on,
            // also once the page has let the press through after 100ms.
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: at(left + pitch),
                delta_x: 0.0,
                delta_y: 100000.0,
            });
            again.global::<Input>().set_touch(true);
            again.global::<Input>().set_finger(true);
            let tile_y = || find(&again, &tile(2)).map_or(f32::NAN, |b| b.absolute_position().y);
            let page_at = tile_y();
            // Once the shelf has the finger, the page doesn't take it even
            // when it goes on up.
            let sideways = [
                (0, -12.0, 3.0),
                (120, -0.6 * pitch, 8.0),
                (40, -1.2 * pitch, 10.0),
                (20, -1.2 * pitch, -1.2 * pitch - 40.0),
            ];
            drag(&again, at(left + pitch), &sideways, 60).await;
            wait_for("the shelf one tile on", rests(2)).await;
            assert_eq!(tile_y(), page_at, "the page stays put");
            assert_eq!(again.get_page(), Page::Home);
            drag(&again, at(left + pitch), &[(0, 6.0, -30.0), (0, 10.0, -50.0)], 60).await;
            assert!((page_at - tile_y() - 50.0).abs() < 0.01, "the page follows the finger");
            assert!(rests(2)(), "the shelf stays put");
            // A quick flick, let go within the 100ms the page holds a finger
            // back, slides the shelf on rather than tapping the tile under
            // the finger. The shelf follows it as soon as its first move has
            // found the shelf.
            let start = shelf_at();
            let shelf_y = middle(&button(&again, &tile(2))).y;
            let along = |dx: f32| slint::LogicalPosition::new(left + pitch + dx, shelf_y);
            let finger = PointerEventButton::Left;
            window.dispatch_event(WindowEvent::PointerPressed { position: along(0.0), button: finger });
            window.dispatch_event(WindowEvent::PointerMoved { position: along(-4.0) });
            window.dispatch_event(WindowEvent::PointerMoved { position: along(-30.0) });
            assert!((start - shelf_at() - 30.0).abs() < 0.01, "the shelf follows the finger");
            window.dispatch_event(WindowEvent::PointerReleased { position: along(-30.0), button: finger });
            window.dispatch_event(WindowEvent::PointerExited);
            let resting = (left - settled(shelf_at).await) / pitch;
            assert!(resting > 2.5, "came to rest on {resting}");
            assert_eq!(again.get_page(), Page::Home, "the flick opened nothing");
            // Enter and Space on a tile reached with Tab, and a screen
            // reader's action, still open it.
            let back_home = async || {
                wait_for("its album", || again.get_page() == Page::Album).await;
                again.invoke_back();
                wait_for("the shelf", || find(&again, "Next in Recently added").is_some()).await;
            };
            // Tab reaches only the tiles in view. The Menu key, which selects
            // a tile and does nothing to what else Tab stops on, tells when
            // a tile has the keys.
            let opens = async |key: slint::platform::Key| {
                let reached = (0..100).any(|_| {
                    press(&again, slint::platform::Key::Tab);
                    press(&again, slint::platform::Key::Menu);
                    again.get_selected_tiles() > 0
                });
                assert!(reached, "Tab reached no tile");
                press(&again, slint::platform::Key::Escape);
                assert_eq!(again.get_selected_tiles(), 0);
                press(&again, key);
                back_home().await;
            };
            opens(slint::platform::Key::Return).await;
            opens(slint::platform::Key::Space).await;
            let shown = (0..again.get_tiles().row_count())
                .map(tile)
                .find(|title| find(&again, title).is_some())
                .unwrap();
            click(&again, &shown);
            back_home().await;
            // A finger on the seek bar is the bar's: on the player bar, and
            // on the now playing sheet, whose page leaves it the finger even
            // when it goes more up than sideways.
            let (can_play, playing) = (again.get_can_play(), again.get_playing());
            let seeks = Rc::new(RefCell::new(Vec::new()));
            again.on_seek({
                let seeks = seeks.clone();
                move |seconds| seeks.borrow_mut().push(seconds)
            });
            again.set_can_play(true);
            again.set_playing(true);
            again.set_playing_duration(200.0);
            let seek_bar = || {
                again
                    .root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Slider)
                    .match_predicate(|bar| bar.accessible_label().is_some_and(|l| l == "Position"))
                    .find_first()
                    .unwrap()
            };
            // Scrubs it with a finger drifting up as it goes right; gives
            // where the bar was, and where it seeks to.
            let scrub = async || {
                let bar = seek_bar();
                let (from, at, width) = (middle(&bar), bar.absolute_position(), bar.size().width);
                let path = [(0, 4.0, -3.0), (16, 12.0, -14.0), (16, 40.0, -30.0), (16, 80.0, -45.0)];
                drag(&again, from, &path, 0).await;
                (at.y, ((from.x + 80.0 - at.x) / width).clamp(0.0, 1.0) * 200.0)
            };
            let (_, to) = scrub().await;
            assert!(seeks.borrow().last().is_some_and(|seek| (seek - to).abs() < 0.01), "the bar's");
            let queue_lines = again.get_queue_lines();
            again.set_queue_lines(slint::ModelRc::new(slint::VecModel::from(vec![
                QueueLine::default(),
                QueueLine::default(),
            ])));
            again.window().set_size(slint::LogicalSize::new(360.0, 640.0));
            click(&again, "Now playing");
            idle(Duration::from_millis(400)).await;
            let (y, to) = scrub().await;
            assert_eq!(seeks.borrow().len(), 2);
            assert!(seeks.borrow().last().is_some_and(|seek| (seek - to).abs() < 0.01), "the sheet's");
            assert_eq!(seek_bar().absolute_position().y, y, "the sheet stays put");
            // The bar's claim ends with its finger: the next one scrolls it.
            let above = slint::LogicalPosition::new(180.0, y - 120.0);
            drag(&again, above, &[(0, 0.0, -20.0), (0, 0.0, -40.0)], 100).await;
            assert!(seek_bar().absolute_position().y < y, "the sheet scrolls");
            press(&again, slint::platform::Key::Escape);
            wait_for("the sheet to close", || !again.get_now_playing_open()).await;
            again.set_queue_lines(queue_lines);
            again.set_playing(playing);
            again.set_can_play(can_play);
            again
                .window()
                .set_size(slint::LogicalSize::new(1100.0, 760.0));
            again.global::<Input>().set_touch(false);
            again.global::<Input>().set_finger(false);
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: at(left + pitch),
                delta_x: 0.0,
                delta_y: 100000.0,
            });
            // Try again asks only for what's missing, and keeps the shelf.
            again.invoke_show_more();
            assert_eq!(again.get_tiles().row_count(), 24);
            // Its tiles stand where the albums grid's do, which its count opens.
            click(&again, "603 albums");
            wait_for("the albums", || again.get_tiles().row_count() == 100).await;
            let tiles = again.get_tiles();
            let grid = |row: usize| x(tiles.row_data(row).unwrap().title.as_str());
            assert_eq!((grid(0), grid(1) - grid(0)), (left, pitch));
            // It picks up the preferences saved on this device.
            assert_eq!(again.get_album_sort(), 1);
            assert!(again.get_album_descending());
            click(&again, "Tracks");
            wait_for("the tracks", || again.get_track_lines().row_count() == 100).await;
            assert_eq!(titles(&again), titles(&ui));
            assert_eq!(sorted(&again), [("Year".to_owned(), true)]);
            click(&again, "Compositions");
            wait_for("the compositions", || {
                again.get_track_lines().row_count() == 3
            })
            .await;
            assert_eq!(titles(&again).last().unwrap(), "MusicBrainz ID");
            assert_eq!(sorted(&again), [("Date".to_owned(), true)]);
            drop(again);

            // A phone's top bar has buttons a finger can hit, the first
            // one's icon on the pages' 24px margin.
            ui.window().set_size(slint::LogicalSize::new(360.0, 640.0));
            let menu = button(&ui, "Menu");
            assert_eq!(menu.size(), slint::LogicalSize::new(48.0, 48.0));
            assert_eq!(menu.absolute_position().x, 12.0);
            // Android's back closes the drawer and stays on the page.
            let drawers = || {
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Navigation)
                    .find_all()
                    .len()
            };
            let page = ui.get_page();
            click(&ui, "Menu");
            assert_eq!(drawers(), 1);
            press(&ui, slint::platform::Key::Back);
            assert_eq!(drawers(), 0);
            assert_eq!(ui.get_page(), page);
            // So does an upright tablet's; a laptop's window docks the sidebar.
            ui.window().set_size(slint::LogicalSize::new(800.0, 640.0));
            button(&ui, "Menu");
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            assert!(find(&ui, "Menu").is_none());

            let kept = ListenBrainz {
                token: "kept token".into(),
                user: "listener".into(),
            };
            store::save_listenbrainz(kept.clone()).await.unwrap();
            ui.set_listenbrainz_user("listener".into());
            click(&ui, "Account");
            click(&ui, "Sign out");
            // Signing out keeps the server, and asks for the login alone.
            wait_for("the sign-in step", || ui.get_step() == Step::SignIn).await;
            assert_eq!(ui.get_username(), "");
            assert_eq!(ui.get_address(), server.as_str());
            assert!(store::load().await.is_none());
            assert_eq!(store::load_server().await, Some(server.clone()));
            store::forget().await.expect("forgetting twice is fine");
            // It keeps the token too, and so does signing in as someone
            // else, which doesn't ask for one.
            assert_eq!(token(store::load_listenbrainz().await), Some(kept.token.clone()));
            ui.set_username("eve".into());
            ui.set_password("pw".into());
            click(&ui, "Sign in");
            wait_for("the done step", || ui.get_step() == Step::Done).await;
            wait_for("the account saved", || !ui.get_busy()).await;
            assert_eq!(ui.get_listenbrainz_user(), "listener");
            assert_eq!(token(store::load_listenbrainz().await), Some(kept.token));
            let signed_in_as = store::load().await.expect("saved").login;
            assert!(matches!(signed_in_as, Login::Password { ref username, .. } if username == "eve"));
            // Only changing the server forgets it, and the token stays.
            click(&ui, "Account");
            click(&ui, "Change server");
            wait_for("the server step", || ui.get_step() == Step::Server).await;
            assert_eq!(ui.get_address(), "");
            assert!(store::load().await.is_none());
            assert!(store::load_server().await.is_none());
            assert_eq!(token(store::load_listenbrainz().await), Some("kept token".into()));
            store::forget_listenbrainz().await.unwrap();
            ui.set_listenbrainz_user("".into());

            // A server that takes only an artist's name for their top songs,
            // as Fonoteca does, is asked by name.
            NAMES_ONLY.store(true, Relaxed);
            TRANSCODE_OFFSET.store(true, Relaxed);
            ORGANIZED.store(true, Relaxed);
            TAGS_DOWN.store(true, Relaxed);
            QUEUED.store(true, Relaxed);
            let asked = MOTIONS_ASKED.load(Relaxed);
            ui.set_address(server.as_str().into());
            click(&ui, "Continue");
            wait_for("the sign-in step", || ui.get_step() == Step::SignIn).await;
            ui.set_username("joe".into());
            ui.set_password("pw".into());
            click(&ui, "Sign in");
            wait_for("the ListenBrainz step", || {
                ui.get_step() == Step::Listenbrainz
            })
            .await;
            click(&ui, "Skip");
            ui.invoke_open("artist:r1".into());
            wait_for("her top songs by name", || ui.get_tracks().row_count() == 2).await;
            wait_for("the account saved", || !ui.get_busy()).await;

            // Fonoteca converts a song from part way in, so a conversion
            // played from there is asked for from there; the original still
            // comes whole.
            let streamed_from = |format: i32, seconds: f64| {
                ui.invoke_choose_stream_quality(false, format, 5);
                let begins_at = play::testing::fetch_song("tr-1", seconds);
                let request = STREAMS.lock().unwrap().last().cloned();
                (request.unwrap_or_default(), begins_at)
            };
            let (request, begins_at) = streamed_from(2, 61.5);
            assert!(request.contains("timeOffset=61.5"), "{request}");
            assert_eq!(begins_at, Some(61.5));
            let (request, begins_at) = streamed_from(2, 0.0);
            assert!(!request.contains("timeOffset"), "{request}");
            assert_eq!(begins_at, Some(0.0), "a conversion from its start begins anywhere too");
            let (request, begins_at) = streamed_from(0, 61.5);
            assert!(!request.contains("timeOffset"), "{request}");
            assert_eq!(begins_at, None);

            // The album of the saved queue's song has its motion cover come
            // to the now playing sheet, whose still cover stood in until then.
            wait_for("the playing album's motion cover", || {
                ui.get_playing_art().size().width == 2
            })
            .await;
            assert_eq!(MOTIONS_ASKED.load(Relaxed), asked + 1);
            let moving = || BTreeSet::from(["blue", "red"]);
            let held = |frame: &'static str| BTreeSet::from([frame]);

            // It moves only while the sheet is open and the song plays, and is
            // held on its frame otherwise, never blanked.
            let frame = art_looks(&ui);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, held(frame));
            ui.set_playing_paused(false);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, held(frame));
            ui.set_playing_paused(true);
            click(&ui, "Now playing");
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, held(frame));
            ui.set_playing_paused(false);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, moving());
            ui.set_playing_paused(true);
            idle(Duration::from_millis(50)).await;
            let frame = art_looks(&ui);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, held(frame));
            ui.set_playing_paused(false);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, moving());
            click(&ui, "Close now playing");
            idle(Duration::from_millis(50)).await;
            let frame = art_looks(&ui);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, held(frame));
            click(&ui, "Now playing");
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, moving());

            // The next song of the album goes on with it: no new request, no
            // blank, and its still cover doesn't take a frame's place.
            let song = |id: &str, album: &str| -> opensubsonic::Child {
                serde_json::from_str(&format!(
                    r#"{{"id":"{id}","isDir":false,"title":"{id}","albumId":"{album}","coverArt":"al-{album}"}}"#
                ))
                .unwrap()
            };
            let queue = || vec![song("c1", "a2"), song("c2", "a2"), song("t1", "a1"), song("c3", "a2")];
            let asked = MOTIONS_ASKED.load(Relaxed);
            play::testing::show_song(&ui, queue(), 1);
            assert_eq!(ui.get_playing_art().size().width, 2, "not blanked");
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, moving());
            assert_eq!(MOTIONS_ASKED.load(Relaxed), asked);

            // Another album's motion cover moves too, as the sheet stays open
            // and the song plays.
            play::testing::show_song(&ui, vec![song("d1", "a4")], 0);
            wait_for("the other album's motion", || {
                ["red", "blue"].contains(&art_looks(&ui))
            })
            .await;
            assert_eq!(MOTIONS_ASKED.load(Relaxed), asked + 1);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, moving());
            let asked = MOTIONS_ASKED.load(Relaxed);

            // An album without one shows its still cover, and the last
            // album's frames stop.
            play::testing::show_song(&ui, queue(), 2);
            wait_for("the still cover", || art_looks(&ui) == "still").await;
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, held("still"));
            assert_eq!(MOTIONS_ASKED.load(Relaxed), asked + 1);

            // Frames that come late for an album left behind don't show.
            SLOW_MOTION.store(true, Relaxed);
            play::testing::show_song(&ui, queue(), 3);
            play::testing::show_song(&ui, queue(), 2);
            let seen = art_over(&ui, Duration::from_millis(1200)).await;
            assert!(seen.is_subset(&BTreeSet::from(["blank", "still"])), "{seen:?}");
            assert_eq!(art_looks(&ui), "still");
            SLOW_MOTION.store(false, Relaxed);

            // With the queue gone, the same album asks again and plays.
            play::testing::show_song(&ui, queue(), 0);
            wait_for("its motion again", || ["red", "blue"].contains(&art_looks(&ui))).await;
            play::testing::clear(&ui);
            assert_eq!(play::testing::motion_album(), None);
            let asked = MOTIONS_ASKED.load(Relaxed);
            play::testing::show_song(&ui, queue(), 0);
            wait_for("its motion once more", || MOTIONS_ASKED.load(Relaxed) > asked).await;
            ui.set_playing(true);
            click(&ui, "Now playing");
            ui.set_playing_paused(false);
            wait_for("its frames", || ["red", "blue"].contains(&art_looks(&ui))).await;
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, moving());

            // A work played whole is one row of the queue that opens to its
            // movements; played from the middle, its movements are songs.
            let movement = |id: &str, work: &str| -> opensubsonic::Child {
                serde_json::from_str(&format!(
                    r#"{{"id":"{id}","isDir":false,"title":"{work}: {id}","albumId":"a2","works":[{{"name":"{work}"}}],"movements":[{{"name":"{id}"}}]}}"#
                ))
                .unwrap()
            };
            let pieces = || {
                vec![
                    movement("I", "Sonata"),
                    movement("II", "Sonata"),
                    movement("III", "Sonata"),
                    song("solo", "a2"),
                    movement("one", "Suite"),
                    movement("two", "Suite"),
                ]
            };
            let rows = || -> Vec<(String, bool, bool)> {
                ui.get_queue_lines()
                    .iter()
                    .map(|line| (line.title.to_string(), line.work, line.part))
                    .collect()
            };
            let row = |title: &str, work: bool, part: bool| (title.to_owned(), work, part);
            play::testing::play_page(&ui, pieces(), 0);
            assert_eq!(
                rows(),
                [
                    row("Sonata", true, false),
                    row("solo", false, false),
                    row("Suite", true, false)
                ]
            );
            ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(550.0, 400.0),
                delta_x: 0.0,
                delta_y: -100000.0,
            });
            idle(Duration::from_millis(100)).await;
            click(&ui, "Show the movements of Sonata");
            assert_eq!(
                rows(),
                [
                    row("Sonata", true, false),
                    row("I", false, true),
                    row("II", false, true),
                    row("III", false, true),
                    row("solo", false, false),
                    row("Suite", true, false)
                ]
            );
            let sonata = ui.get_queue_lines().row_data(0).unwrap();
            assert!(sonata.current && sonata.expanded, "the work holds the song playing");
            assert_eq!(sonata.details.as_str(), "Playing I (1 of 3)");
            ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(550.0, 400.0),
                delta_x: 0.0,
                delta_y: -100000.0,
            });
            idle(Duration::from_millis(100)).await;
            click(&ui, "Hide the movements of Sonata");
            assert_eq!(rows().len(), 3);
            // From its second movement, the Sonata is songs; the Suite stays whole.
            play::testing::play_page(&ui, pieces(), 1);
            assert_eq!(
                rows(),
                [
                    row("Sonata: II", false, false),
                    row("Sonata: III", false, false),
                    row("solo", false, false),
                    row("Suite", true, false)
                ]
            );

            // Two movements of a work that has four are songs.
            let counted = |id: &str, number: i64| -> opensubsonic::Child {
                serde_json::from_str(&format!(
                    r#"{{"id":"{id}","isDir":false,"title":"{id}","albumId":"a2","works":[{{"name":"Sonata"}}],"movements":[{{"name":"{id}","number":{number},"count":4}}]}}"#
                ))
                .unwrap()
            };
            play::testing::play_page(&ui, vec![counted("II", 2), counted("III", 3)], 0);
            assert_eq!(rows(), [row("II", false, false), row("III", false, false)]);

            // A row of the queue moves by its grip, dragged by a mouse or a
            // finger, and the song playing plays on.
            let singles = || (0..5).map(|n| song(&format!("q{n}"), "a2")).collect::<Vec<_>>();
            let titles = || rows().into_iter().map(|line| line.0).collect::<Vec<_>>();
            let to_the_end = async |x: f32| {
                ui.window().dispatch_event(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(x, 300.0),
                    delta_x: 0.0,
                    delta_y: -100000.0,
                });
                idle(Duration::from_millis(600)).await;
            };
            let row_in_sight = |title: &str| -> Option<ElementHandle> {
                let wanted = title.to_owned();
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::ListItem)
                    .match_predicate(move |row| row.accessible_label().is_some_and(|l| l == wanted))
                    .find_first()
            };
            let queue_row = |title: &str| -> ElementHandle {
                row_in_sight(title).unwrap_or_else(|| panic!("no row {title}"))
            };
            let grip_of = |title: &str| {
                let row = queue_row(title);
                let (at, size) = (row.absolute_position(), row.size());
                slint::LogicalPosition::new(at.x + size.width - 12.0, at.y + size.height / 2.0)
            };
            let row_height = 56.0;
            play::testing::play_page(&ui, singles(), 0);
            to_the_end(550.0).await;
            assert_eq!(queue_row("q1").size().height, row_height);
            // The row follows the pointer as a copy, the row itself dimmed in
            // its place, while the pointer is down.
            let (window, left) = (ui.window(), PointerEventButton::Left);
            let from = grip_of("q1");
            window.dispatch_event(WindowEvent::PointerPressed { position: from, button: left });
            let down_two = slint::LogicalPosition::new(from.x, from.y + 2.0 * row_height + 20.0);
            window.dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(from.x, from.y + 6.0) });
            window.dispatch_event(WindowEvent::PointerMoved { position: down_two });
            let copies = |title: &str| {
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::ListItem)
                    .match_predicate({
                        let title = title.to_owned();
                        move |row| row.accessible_label().is_some_and(|l| l == title)
                    })
                    .find_all()
            };
            let held = copies("q1");
            assert_eq!(held.len(), 2, "the copy is drawn with the row");
            let copy_y = held.iter().map(|row| row.absolute_position().y).fold(f32::MIN, f32::max);
            assert!((copy_y - (queue_row("q0").absolute_position().y + 3.0 * row_height + 20.0)).abs() < 0.5, "it follows the pointer");
            assert_eq!(titles(), ["q0", "q1", "q2", "q3", "q4"], "nothing moves before it is let go");
            window.dispatch_event(WindowEvent::PointerReleased { position: down_two, button: left });
            assert_eq!(titles(), ["q0", "q2", "q3", "q1", "q4"], "two places down");
            assert!(ui.get_queue_lines().row_data(0).unwrap().current, "the song playing is the first still");
            let (ids, shown) = play::testing::queued_ids();
            assert_eq!((ids, shown), (["q0", "q2", "q3", "q1", "q4"].map(String::from).to_vec(), Some(0)));
            // A mouse dragging nowhere in particular moves nothing, nor does
            // one let go on the row it started from.
            drag(&ui, grip_of("q2"), &[(0, 0.0, 4.0), (20, 0.0, 12.0)], 20).await;
            assert_eq!(titles(), ["q0", "q2", "q3", "q1", "q4"]);
            // The next song follows the new order when one moves up past it.
            drag(&ui, grip_of("q4"), &[(0, 0.0, -6.0), (20, 0.0, -40.0), (20, 0.0, -3.0 * row_height - 20.0)], 20).await;
            assert_eq!(titles(), ["q0", "q4", "q2", "q3", "q1"]);
            assert_eq!(play::testing::queued_ids().0[..2].to_vec(), ["q0", "q4"], "q4 plays after q0");
            // Not above the song playing, and not by its own grip.
            drag(&ui, grip_of("q1"), &[(0, 0.0, -6.0), (20, 0.0, -400.0)], 20).await;
            assert_eq!(titles(), ["q0", "q1", "q4", "q2", "q3"], "the top slot is below the one playing");

            // A work moves as one unit, collapsed.
            play::testing::play_page(&ui, pieces(), 0);
            to_the_end(550.0).await;
            drag(&ui, grip_of("Suite"), &[(0, 0.0, -6.0), (20, 0.0, -row_height - 16.0)], 20).await;
            assert_eq!(titles(), ["Sonata", "Suite", "solo"]);
            assert_eq!(play::testing::queued_ids().0, ["I", "II", "III", "one", "two", "solo"]);
            drag(&ui, grip_of("Sonata"), &[(0, 0.0, 6.0), (20, 0.0, 60.0), (20, 0.0, 130.0)], 20).await;
            assert_eq!(titles(), ["Sonata", "Suite", "solo"], "the work playing has no grip to move by");

            // A finger on a grip drags the row; one anywhere else on the row
            // scrolls the page, as the page's drag takes it.
            ui.window().set_size(slint::LogicalSize::new(360.0, 640.0));
            idle(Duration::from_millis(400)).await;
            let input = ui.global::<Input>();
            input.set_touch(true);
            input.set_finger(true);
            play::testing::play_page(&ui, singles(), 0);
            to_the_end(180.0).await;
            let path_down = |rows_down: f32| [(0, 0.0, 4.0), (120, 0.0, 14.0), (16, 0.0, 40.0), (16, 0.0, rows_down * row_height + 20.0)];
            let page_at = queue_row("q0").absolute_position().y;
            drag(&ui, grip_of("q2"), &path_down(1.0), 40).await;
            assert_eq!(titles(), ["q0", "q1", "q3", "q2", "q4"], "a finger drags the row");
            assert_eq!(queue_row("q0").absolute_position().y, page_at, "the page stays put");
            let middle_of = |title: &str| middle(&queue_row(title));
            let (title_x, title_y) = (middle_of("q1").x - 40.0, middle_of("q1").y);
            drag(&ui, slint::LogicalPosition::new(title_x, title_y), &[(0, 0.0, 6.0), (16, 0.0, 30.0), (16, 0.0, 60.0)], 100).await;
            assert_eq!(titles(), ["q0", "q1", "q3", "q2", "q4"], "a swipe on a row moves nothing");
            assert!(queue_row("q0").absolute_position().y > page_at + 30.0, "it scrolls the page");
            input.set_touch(false);
            input.set_finger(false);
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            idle(Duration::from_millis(400)).await;

            // Alt and an arrow key move the row that has the keys by one unit,
            // and it keeps the keys, so the next press moves it on.
            let with_alt = |key: slint::platform::Key| {
                let alt = || slint::platform::Key::Alt.into();
                ui.window().dispatch_event(WindowEvent::KeyPressed { text: alt() });
                press(&ui, key);
                ui.window().dispatch_event(WindowEvent::KeyReleased { text: alt() });
            };
            play::testing::play_page(&ui, singles(), 0);
            to_the_end(550.0).await;
            let before = titles();
            // A tap on q2's cover, which is no link or button, gives it the keys.
            let row = queue_row("q2");
            let (at, size) = (row.absolute_position(), row.size());
            hold(&ui, slint::LogicalPosition::new(at.x + 28.0, at.y + size.height / 2.0), 0.0, 0).await;
            assert_eq!(titles(), before, "a tap on a row moves nothing");
            with_alt(slint::platform::Key::DownArrow);
            assert_eq!(titles(), ["q0", "q1", "q3", "q2", "q4"], "down one unit");
            press(&ui, slint::platform::Key::DownArrow);
            assert_eq!(titles(), ["q0", "q1", "q3", "q2", "q4"], "the arrow key alone moves nothing");
            with_alt(slint::platform::Key::DownArrow);
            assert_eq!(titles(), ["q0", "q1", "q3", "q4", "q2"], "the row kept the keys");
            with_alt(slint::platform::Key::DownArrow);
            assert_eq!(titles(), ["q0", "q1", "q3", "q4", "q2"], "and no further than the end");
            for _ in 0..4 {
                with_alt(slint::platform::Key::UpArrow);
            }
            assert_eq!(titles(), ["q0", "q2", "q1", "q3", "q4"], "up, and not above the song playing");

            // The pointer near the bottom of the sheet scrolls it on, so a row
            // reaches rows that were far below the view.
            let long: Vec<_> = (0..40).map(|n| song(&format!("q{n}"), "a2")).collect();
            play::testing::play_page(&ui, long, 0);
            for _ in 0..40 {
                if row_in_sight("q1").is_some_and(|row| row.absolute_position().y < 500.0) {
                    break;
                }
                ui.window().dispatch_event(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(550.0, 300.0),
                    delta_x: 0.0,
                    delta_y: -150.0,
                });
                idle(Duration::from_millis(50)).await;
            }
            let from = grip_of("q1");
            let near_bottom = slint::LogicalPosition::new(from.x, 752.0);
            window.dispatch_event(WindowEvent::PointerPressed { position: from, button: left });
            window.dispatch_event(WindowEvent::PointerMoved { position: slint::LogicalPosition::new(from.x, from.y + 6.0) });
            window.dispatch_event(WindowEvent::PointerMoved { position: near_bottom });
            idle(Duration::from_millis(1500)).await;
            window.dispatch_event(WindowEvent::PointerReleased { position: near_bottom, button: left });
            let landed = titles().iter().position(|title| title == "q1").unwrap();
            assert!(landed > 20, "q1 landed at {landed}, past what the view showed");

            // An open work and its movements have no grip, and nothing lands
            // between its movements.
            let mut lead_in = vec![song("lead", "a2")];
            lead_in.extend(pieces());
            play::testing::play_page(&ui, lead_in, 0);
            to_the_end(550.0).await;
            click(&ui, "Show the movements of Sonata");
            to_the_end(550.0).await;
            let open = titles();
            assert_eq!(open, ["lead", "Sonata", "I", "II", "III", "solo", "Suite"]);
            // Far enough down that the work would land past the song, if a grip moved it.
            for title in ["Sonata", "I"] {
                drag(&ui, grip_of(title), &[(0, 0.0, 6.0), (20, 0.0, 4.6 * row_height)], 20).await;
                assert_eq!(titles(), open, "{title} has no grip");
            }
            drag(&ui, grip_of("solo"), &[(0, 0.0, -6.0), (20, 0.0, -3.3 * row_height)], 20).await;
            assert_eq!(titles(), ["lead", "solo", "Sonata", "I", "II", "III", "Suite"], "above the work, not among its movements");
            assert_eq!(play::testing::queued_ids().0, ["lead", "solo", "I", "II", "III", "one", "two"]);

            // So does the queue loaded anew, as on a sign in.
            let asked = MOTIONS_ASKED.load(Relaxed);
            play::testing::restore(&ui);
            wait_for("its motion after the sign in", || {
                MOTIONS_ASKED.load(Relaxed) > asked
            })
            .await;

            // A song with a waveform shows it as the seek bar, and one without
            // keeps the plain bar. The last song's waveform goes at once, and
            // one that comes late for a song left behind doesn't show.
            let asked = WAVEFORMS_ASKED.load(Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            wait_for("its waveform", || !ui.get_playing_waveform().is_empty()).await;
            play::testing::show_song(&ui, vec![song("w2", "a1")], 0);
            assert_eq!(ui.get_playing_waveform(), "", "the last song's is gone");
            wait_for("the waveform asked for", || {
                WAVEFORMS_ASKED.load(Relaxed) == asked + 2
            })
            .await;
            SLOW_WAVEFORM.store(true, Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            play::testing::show_song(&ui, vec![song("w2", "a1")], 0);
            idle(Duration::from_millis(1000)).await;
            assert_eq!(ui.get_playing_waveform(), "");
            SLOW_WAVEFORM.store(false, Relaxed);

            // Without the extension the still cover stays and none is asked for.
            let asked = MOTIONS_ASKED.load(Relaxed);
            let waveforms_asked = WAVEFORMS_ASKED.load(Relaxed);
            click(&ui, "Account");
            click(&ui, "Sign out");
            wait_for("the sign-in step", || ui.get_step() == Step::SignIn).await;
            NO_MOTION.store(true, Relaxed);
            NO_WAVEFORM.store(true, Relaxed);
            OLD_GENRES.store(true, Relaxed);
            ui.set_username("joe".into());
            ui.set_password("pw".into());
            click(&ui, "Sign in");
            wait_for("the ListenBrainz step", || {
                ui.get_step() == Step::Listenbrainz
            })
            .await;
            click(&ui, "Skip");
            wait_for("the still cover", || ui.get_playing_art().size().width == 1).await;
            let later = Instant::now() + Duration::from_millis(300);
            wait_for("a few frames' time", || Instant::now() > later).await;
            assert_eq!(ui.get_playing_art().size().width, 1);
            assert_eq!(MOTIONS_ASKED.load(Relaxed), asked);
            // Descriptions come from the second version of the genres only.
            ui.invoke_open("genre:Jazz".into());
            wait_for("Jazz's subgenres", || ui.get_genre_tiles().row_count() == 1).await;
            assert_eq!(ui.get_about(), "");
            ui.invoke_back();
            OLD_GENRES.store(false, Relaxed);
            NO_MOTION.store(false, Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_playing_waveform(), "");
            assert_eq!(WAVEFORMS_ASKED.load(Relaxed), waveforms_asked);
            NO_WAVEFORM.store(false, Relaxed);
            QUEUED.store(false, Relaxed);

            // On a server with playlist folders, the sidebar has every
            // playlist, the one in a folder too, and no folders.
            let texts = |model: slint::ModelRc<slint::SharedString>| -> Vec<String> {
                model.iter().map(Into::into).collect()
            };
            wait_for("the playlists", || {
                texts(ui.get_sections()).contains(&"Road Trip".to_owned())
            })
            .await;
            let sections = texts(ui.get_sections());
            let tracks = sections.iter().position(|s| s == "Tracks").unwrap();
            assert_eq!(sections[tracks + 1], "Tags");
            let playlists = sections.iter().rposition(|s| s == "Playlists").unwrap();
            assert_eq!(sections[playlists + 1..playlists + 3], ["Road Trip", "Empty"]);
            assert!(!sections.contains(&"Trips".to_owned()), "it is a folder");

            // A folder's page has the folders and playlists in it, and makes
            // new ones there.
            ui.invoke_open("playlists:f1".into());
            assert_eq!(ui.get_page(), Page::Playlists);
            assert_eq!(ui.get_header_title(), "Trips");
            wait_for("the folder's tiles", || ui.get_tiles().row_count() == 2).await;
            let tiles: Vec<_> = ui.get_tiles().iter().collect();
            assert_eq!(
                tiles
                    .iter()
                    .map(|t| (t.title.as_str(), t.link.as_str(), t.folder))
                    .collect::<Vec<_>>(),
                [
                    ("Old", "playlists:f2", true),
                    ("Road Trip", "playlist:p1", false)
                ]
            );
            // A folder, however deep, is under the playlists' entry; a
            // playlist in one has its own.
            assert_eq!(ui.get_section(), playlists as i32);
            for (link, entry) in [("playlists:f2", playlists), ("playlist:p1", playlists + 1)] {
                ui.invoke_open(link.into());
                assert_eq!(ui.get_section(), entry as i32, "{link}");
                ui.invoke_back();
            }
            assert_eq!(tiles[0].names.row_data(0).unwrap().label, "0 items");
            let writes = || ORGANIZE_WRITES.lock().unwrap().clone();
            ui.invoke_create_folder("Live".into());
            wait_for("the new folder", || !writes().is_empty()).await;
            assert_eq!(writes(), ["createPlaylistFolder name=Live&parentId=f1"]);
            // The server keeps a folder with something in it.
            ui.invoke_delete_folder();
            wait_for("the refusal", || ui.get_page_problem() != "").await;
            assert!(ui.get_page_problem().contains("The folder isn't empty"));
            assert_eq!(ui.get_page(), Page::Playlists);

            // A playlist moves into any folder, by its path.
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.invoke_open("playlist:p2".into());
            wait_for("the moves", || ui.get_move_targets().row_count() == 2).await;
            let targets: Vec<String> =
                ui.get_move_targets().iter().map(|t| t.label.into()).collect();
            assert_eq!(targets, ["Trips", "Trips / Old"]);
            ui.invoke_move_playlist("f2".into());
            wait_for("the move", || !writes().is_empty()).await;
            assert_eq!(writes(), ["movePlaylist playlistId=p2&playlistFolderId=f2"]);
            wait_for("the notice", || ui.get_notice() == "Moved to Trips / Old.").await;

            // On the playlists page a playlist's tile moves it, at once, as
            // a selection's playlists do, and one dropped onto a folder or
            // onto the playlists' heading, unless it is there already. The
            // server keeps the places it had.
            let tile_titles = |ui: &AppWindow| -> Vec<String> {
                ui.get_tiles().iter().map(|tile| tile.title.into()).collect()
            };
            let move_labels = |model: slint::ModelRc<Name>| -> Vec<String> {
                model.iter().map(|name| name.label.into()).collect()
            };
            ui.invoke_show_section(playlists as i32);
            wait_for("the top level", || tile_titles(&ui) == ["Trips", "Empty"]).await;
            assert_eq!(move_labels(ui.get_tile_moves()), ["Trips", "Trips / Old"]);
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.invoke_move_tile(1, "f1".into());
            assert_eq!(tile_titles(&ui), ["Trips"]);
            wait_for("the tile's move", || !writes().is_empty()).await;
            assert_eq!(writes(), ["movePlaylist playlistId=p2&playlistFolderId=f1"]);

            wait_for("the places kept", || tile_titles(&ui) == ["Trips", "Empty"]).await;
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.invoke_select_tile(0, false);
            assert_eq!(ui.get_selection_moves().row_count(), 0, "only a folder");
            ui.invoke_select_tile(1, false);
            assert_eq!(
                move_labels(ui.get_selection_moves()),
                ["Top level", "Trips", "Trips / Old"]
            );
            ui.invoke_move_selection("f2".into());
            assert_eq!(ui.get_selected_tiles(), 0);
            assert_eq!(tile_titles(&ui), ["Trips"]);
            wait_for("the selection's move", || !writes().is_empty()).await;
            assert_eq!(writes(), ["movePlaylist playlistId=p2&playlistFolderId=f2"]);

            wait_for("the places kept", || tile_titles(&ui) == ["Trips", "Empty"]).await;
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.global::<Drag>().set_link("playlist:p2".into());
            ui.invoke_drop_on_tile(0);
            assert_eq!(ui.global::<Drag>().get_link(), "");
            assert_eq!(tile_titles(&ui), ["Trips"]);
            wait_for("the drop", || !writes().is_empty()).await;
            assert_eq!(writes(), ["movePlaylist playlistId=p2&playlistFolderId=f1"]);
            wait_for("the places kept", || tile_titles(&ui) == ["Trips", "Empty"]).await;
            ORGANIZE_WRITES.lock().unwrap().clear();
            for dragged in ["playlist:p2", "playlist:p1"] {
                ui.global::<Drag>().set_link(dragged.into());
                ui.invoke_drop_on_section(playlists as i32);
            }
            wait_for("the drop on the heading", || !writes().is_empty()).await;
            assert_eq!(writes(), ["movePlaylist playlistId=p1&playlistFolderId="]);

            // The tags page has every tag under the one it is in. Those
            // refused at sign-in and on the page come with Try again.
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.invoke_show_section((tracks + 1) as i32);
            assert_eq!(ui.get_page(), Page::Tags);
            wait_for("the failed tags", || ui.get_page_problem() != "").await;
            TAGS_DOWN.store(false, Relaxed);
            click(&ui, "Try again");
            wait_for("the tags", || ui.get_tag_rows().row_count() == 3).await;
            assert_eq!(ui.get_page_problem(), "");
            let rows: Vec<_> = ui.get_tag_rows().iter().collect();
            assert_eq!(
                rows.iter()
                    .map(|r| (r.title.as_str(), r.depth, r.details.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("Instrument", 0, "0 tracks"),
                    ("Mood", 0, "2 tracks"),
                    ("Calm", 1, "1 track")
                ]
            );
            let targets: Vec<String> = rows[2].targets.iter().map(|t| t.label.into()).collect();
            assert_eq!(targets, ["Top level", "Instrument"]);

            // A tag's page lists its tracks and those of the tags in it.
            ui.invoke_open("tag:t1".into());
            assert_eq!(ui.get_page(), Page::Tag);
            assert_eq!(lit(&ui), "Tags");
            wait_for("the tag's tracks", || ui.get_track_lines().row_count() == 2).await;
            assert_eq!(ui.get_header_title(), "Mood");
            assert_eq!(ui.get_header_names().row_data(0).unwrap().label, "Calm");
            let harbour = (0..2)
                .find(|&row| {
                    let line = ui.get_track_lines().row_data(row).unwrap();
                    line.cells.iter().any(|cell| cell.iter().any(|n| n.label == "Harbour Lights"))
                })
                .unwrap();

            // The tags dialog ticks the song's own tags; each tick puts one
            // on or takes it off at once.
            ui.invoke_tag_line(harbour as i32);
            assert!(!ui.get_tag_choices_loading());
            let ticks = || -> Vec<bool> { ui.get_tag_choices().iter().map(|c| c.ticked).collect() };
            assert_eq!(ticks(), [false, true, false]);
            ui.invoke_toggle_tag(0);
            assert_eq!(ticks(), [true, true, false]);
            wait_for("the tag put on", || !writes().is_empty()).await;
            ui.invoke_toggle_tag(1);
            assert_eq!(ticks(), [true, false, false]);
            wait_for("the tag taken off", || writes().len() == 2).await;
            ui.invoke_tag_with_new("Live".into());
            wait_for("the new tag", || writes().len() == 4).await;
            assert_eq!(
                writes(),
                [
                    "updateTag tagId=t3&songIdToAdd=s1",
                    "updateTag tagId=t1&songIdToRemove=s1",
                    "createTag name=Live",
                    "updateTag tagId=t9&songIdToAdd=s1"
                ]
            );
            // Shown again, a tag's page still knows which tag it is.
            ui.invoke_open("tags".into());
            ui.invoke_open("tag:t1".into());
            assert_eq!(ui.get_shown_id(), "t1");
            assert_eq!(ui.get_header_names().row_count(), 1);
            assert_eq!(ui.get_move_targets().row_count(), 1);
            assert!(ui.get_track_lines().row_count() == 2, "kept, not asked for again");
            // After a change to the tags elsewhere, it asks again.
            ui.invoke_open("tags".into());
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.invoke_move_tag("t2".into(), "".into());
            wait_for("the tag moved", || !writes().is_empty()).await;
            assert_eq!(writes(), ["updateTag tagId=t2&parentId="]);
            ui.invoke_open("tag:t1".into());
            assert_eq!(ui.get_track_lines().row_count(), 0, "asked for again");
            wait_for("the tag's tracks again", || ui.get_track_lines().row_count() == 2).await;
            click(&ui, "Account");
            click(&ui, "Sign out");
            wait_for("the sign-in step again", || ui.get_step() == Step::SignIn).await;

            slint::quit_event_loop().unwrap();
        })
        .unwrap();
        slint::run_event_loop_until_quit().unwrap();
    }
}
