slint::include_modules!();

mod active;
mod answers;
mod appearance;
#[cfg(any(target_os = "android", test))]
mod back;
#[cfg(target_os = "android")]
mod bit_perfect;
mod browse;
#[cfg(target_family = "wasm")]
mod browser_clock;
mod catalogue;
#[cfg(target_os = "macos")]
mod desktop_media;
#[cfg(target_os = "macos")]
mod macos_colors;
#[cfg(not(target_family = "wasm"))]
mod download_job;
#[cfg(not(target_family = "wasm"))]
mod downloads;
#[cfg(not(target_family = "wasm"))]
mod eq_graph;
#[cfg(target_family = "wasm")]
mod fonts;
#[cfg(not(target_family = "wasm"))]
mod headphones;
mod logo_ink;
mod loves;
mod lyrics;
#[cfg(target_os = "android")]
mod media;
#[cfg(target_os = "android")]
mod network;
#[cfg(not(target_family = "wasm"))]
mod network_outputs;
#[cfg(not(target_family = "wasm"))]
mod network_player;
#[cfg(any(target_os = "macos", test))]
mod now_playing;
mod offline;
#[cfg(not(target_family = "wasm"))]
mod opra;
mod play;
mod plays;
mod prefs;
#[cfg(not(target_family = "wasm"))]
mod quality;
mod queue;
mod queue_drag;
mod radio;
#[cfg(not(target_family = "wasm"))]
mod scrobble;
#[cfg(not(target_family = "wasm"))]
mod sound;
#[cfg(not(target_family = "wasm"))]
mod sound_presets;
mod store;
mod waveform;
mod zone;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use opensubsonic::{Auth, Client, SignIn};
use serde::{Deserialize, Serialize};

/// How a seek bar draws its waveform, which the workbench's need too.
pub use waveform::path as waveform_path;

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

/// How long to wait before asking a server that didn't answer again.
#[cfg(not(test))]
const RECONNECT_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(test)]
const RECONNECT_EVERY: std::time::Duration = std::time::Duration::from_millis(300);

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
/// didn't answer, which signs the library in again.
///
/// With the server not answering and music downloaded from it, opens the
/// downloads under the note that it didn't answer, and asks it again every
/// so often until it does.
fn resume(ui: &AppWindow, browser: &browse::Shared) {
    // The note stays up while the downloads are shown.
    if !browse::is_offline(browser) {
        ui.set_offline("".into());
    }
    let signing_in_again = browse::sign_in_failed(browser);
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
        match signing_in_again {
            true => go_back_online(&ui, &browser, &account).await,
            false => open_library(&ui, &browser, &account).await,
        }
    })
    .expect("the event loop is running");
}

/// Opens the library at once on what this device kept from before, while
/// it signs in, and has its pages ask the server for the rest once it has.
async fn open_library(ui: &AppWindow, browser: &browse::Shared, account: &Account) {
    let client = match Client::new(&account.server, account.login.auth()) {
        Ok(client) => client,
        Err(e) => return ui.set_offline(format!("Your server didn't answer: {e}").into()),
    };
    let sign_in_over = Arc::new(SignIn::default());
    browse::open_library(ui, browser, client.waiting_for(sign_in_over.clone()));
    let opened = browse::client(browser);
    let signed_in = sign_in(&account.server, &account.login).await;
    let signed_in = signed_in.map(|(name, client)| (name, Arc::new(client)));
    // Signed out while this was under way, and maybe in again since: then
    // the pages left behind ask the server nothing more.
    let still_open = goes_on_with(browser, opened);
    let asks_through = signed_in.as_ref().ok().filter(|_| still_open);
    sign_in_over.end(asks_through.map(|(_, client)| client.clone()));
    if !still_open {
        return;
    }
    match signed_in {
        Ok((name, _)) => {
            ui.set_server_name(name.into());
            browse::signed_in(ui, browser);
        }
        Err(e) => didnt_answer(ui, browser, account, &e),
    }
}

/// Signs in again after the server didn't answer, and goes on from the page
/// on screen once it does.
async fn go_back_online(ui: &AppWindow, browser: &browse::Shared, account: &Account) {
    let failed = browse::client(browser);
    let signed_in = sign_in(&account.server, &account.login).await;
    // Signed out while this was under way, and maybe in again since, or
    // signed in by another "Try again".
    if !goes_on_with(browser, failed) {
        return;
    }
    match signed_in {
        Ok((name, client)) => {
            ui.set_server_name(name.into());
            ui.set_offline("".into());
            browse::reconnect(ui, browser, client);
        }
        Err(e) => didnt_answer(ui, browser, account, &e),
    }
}

/// Whether the library still goes on with `client`: signing out drops it,
/// and signing in opens the library with another.
fn goes_on_with(browser: &browse::Shared, client: Option<Arc<Client>>) -> bool {
    (browse::client(browser).zip(client)).is_some_and(|(now, then)| Arc::ptr_eq(&now, &then))
}

/// Says the server didn't answer. With music downloaded from it, shows the
/// downloads and asks the server again every so often.
fn didnt_answer(ui: &AppWindow, browser: &browse::Shared, account: &Account, problem: &str) {
    ui.set_offline(format!("Your server didn't answer: {problem}").into());
    if !browse::downloads(browser).has_songs_from(&account.server) {
        return;
    }
    if !browse::is_offline(browser) {
        browse::go_offline(ui, browser);
    }
    ask_again_later(ui, browser);
}

/// Asks the server again after a while, if the app is still showing what is
/// kept on this device.
fn ask_again_later(ui: &AppWindow, browser: &browse::Shared) {
    let (weak, browser) = (ui.as_weak(), browser.clone());
    // One timer for the whole app, so each "Try again" restarts the wait
    // instead of starting another chain of asks.
    RECONNECT.with(|timer| {
        timer.start(slint::TimerMode::SingleShot, RECONNECT_EVERY, move || {
            if let Some(ui) = weak.upgrade()
                && browse::is_offline(&browser)
            {
                resume(&ui, &browser);
            }
        });
    });
}

thread_local! {
    /// Waits to ask a server that didn't answer again.
    static RECONNECT: slint::Timer = slint::Timer::default();
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

/// Deletes the downloads, which belong to the account and its server, before
/// either goes. When they can't be deleted it says so and stays signed in.
fn remove_downloads(ui: &AppWindow, browser: &browse::Shared) -> bool {
    match browse::downloads(browser).remove_all() {
        Ok(()) => true,
        Err(e) => {
            let note = format!("Not signed out: the downloads couldn't be removed: {e}");
            ui.set_saved_note(note.into());
            false
        }
    }
}

/// Forgets the pages' answers kept for the account before it goes. When they
/// can't be removed it says so and stays signed in.
fn forget_answers(ui: &AppWindow, browser: &browse::Shared) -> bool {
    match browse::forget_answers(browser) {
        Ok(()) => true,
        Err(e) => {
            let note =
                format!("Not signed out: the pages kept on this device couldn't be removed: {e}");
            ui.set_saved_note(note.into());
            false
        }
    }
}

/// Forgets the plays logged on this device for the account before it goes.
/// When they can't be removed it says so and stays signed in.
fn forget_plays(ui: &AppWindow, browser: &browse::Shared) -> bool {
    match browse::forget_plays(browser) {
        Ok(()) => true,
        Err(e) => {
            let note =
                format!("Not signed out: the plays logged on this device couldn't be removed: {e}");
            ui.set_saved_note(note.into());
            false
        }
    }
}

/// Forgets the plays waiting for the account's server before it goes. When
/// they can't be removed it says so and stays signed in.
fn forget_waiting_plays(ui: &AppWindow, browser: &browse::Shared, playing: &play::Shared) -> bool {
    match play::forget_waiting_plays(playing, browser) {
        Ok(()) => true,
        Err(e) => {
            let note = format!(
                "Not signed out: the plays waiting for the server couldn't be removed: {e}"
            );
            ui.set_saved_note(note.into());
            false
        }
    }
}

/// Removes all the account keeps on this device. When something can't be
/// removed it says so; whether all went.
fn forget_account(ui: &AppWindow, browser: &browse::Shared, playing: &play::Shared) -> bool {
    remove_downloads(ui, browser)
        && forget_answers(ui, browser)
        && forget_plays(ui, browser)
        && forget_waiting_plays(ui, browser, playing)
}

/// Tells the server of the plays waiting for it once more, saying so with
/// `shows` on the button pressed while they go, removes all the account
/// keeps on this device, and then `goes`. With no plays waiting it goes at
/// once. When something can't be removed it says so and stays signed in.
fn leave(
    ui: &AppWindow,
    browser: &browse::Shared,
    playing: &play::Shared,
    shows: fn(&AppWindow, slint::SharedString),
    goes: impl FnOnce(&AppWindow) + 'static,
) {
    let waiting = play::waiting_plays(playing, browser);
    if waiting == 0 {
        if forget_account(ui, browser, playing) {
            goes(ui);
        }
        return;
    }
    let sending = format!("Sending {}…", browse::count(waiting, "play", "plays"));
    shows(ui, sending.into());
    let last_send = play::send_waiting_plays(playing.clone(), browser.clone());
    let (browser, playing) = (browser.clone(), playing.clone());
    let task = async {
        last_send.await;
        Ok(())
    };
    run(ui, task, move |ui, ()| {
        shows(ui, slint::SharedString::new());
        if forget_account(ui, &browser, &playing) {
            goes(ui);
        }
    });
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

    #[cfg(target_family = "wasm")]
    browser_clock::install();

    let ui = start()?;
    // Desktop environments match the window to its .desktop file by this; it
    // needs the window system, so only once the window exists.
    #[cfg(target_os = "linux")]
    slint::set_xdg_app_id("io.github.schotman_development.fonoteca")?;
    // The window has to exist to be tagged, and run() would not return before it closes.
    #[cfg(target_os = "macos")]
    {
        ui.show()?;
        macos_colors::tag_windows_srgb();
    }
    ui.run()?;
    #[cfg(not(target_family = "wasm"))]
    ui.invoke_event_loop_ended();
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
        media::init(env, &activity)?;
        back::init(env, &activity)?;
        download_job::init(env, &activity)
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
    #[cfg(target_family = "wasm")]
    fonts::register();
    // Every press there is a finger's, the first one too.
    #[cfg(target_os = "android")]
    ui.global::<Input>().set_finger(true);
    let pending: Rc<RefCell<Option<(Account, Client)>>> = Rc::default();
    let browser = browse::Shared::default();
    browse::wire(&ui, &browser);
    let playing = play::wire(&ui, &browser);
    appearance::wire(&ui, &browser, &playing);
    waveform::wire(&ui);
    lyrics::wire(&ui);
    #[cfg(not(target_family = "wasm"))]
    downloads::wire(&ui, &browser);
    #[cfg(target_os = "android")]
    {
        media::wire(&ui);
        back::wire(&ui);
        download_job::wire(&ui);
    }

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
        let browser = browser.clone();
        move || {
            let ui = weak.unwrap();
            // A disabled button still takes a screen reader's press.
            if ui.get_busy() {
                return;
            }
            if let Err(e) = browse::forget_listenbrainz_answers(&browser) {
                let problem = format!(
                    "The token wasn't removed: what ListenBrainz answered couldn't be removed from this device: {e}"
                );
                return ui.set_problem(problem.into());
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
        let (browser, playing) = (browser.clone(), playing.clone());
        move || {
            let ui = weak.unwrap();
            // A disabled button still takes a screen reader's press.
            if ui.get_busy() {
                return;
            }
            let signing_out = browser.clone();
            let shows = AppWindow::set_sign_out_sending;
            leave(&ui, &browser, &playing, shows, move |ui| {
                run(ui, store::forget(), move |ui, ()| {
                    signed_out(ui, &signing_out);
                    ask_for_login(ui, Some(ui.get_address().into()));
                });
            });
        }
    });

    ui.on_change_server({
        let weak = ui.as_weak();
        let (browser, playing) = (browser.clone(), playing.clone());
        move || {
            let ui = weak.unwrap();
            // A disabled button still takes a screen reader's press.
            if ui.get_busy() {
                return;
            }
            let signing_out = browser.clone();
            let shows = AppWindow::set_change_server_sending;
            leave(&ui, &browser, &playing, shows, move |ui| {
                let task = async {
                    store::forget().await?;
                    store::forget_server().await
                };
                run(ui, task, move |ui, ()| {
                    signed_out(ui, &signing_out);
                    ui.set_address("".into());
                    ui.set_step(Step::Server);
                });
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

    /// A WAV file of four silent 16-bit samples, the size of a header and
    /// a little more.
    fn tiny_wav() -> Vec<u8> {
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&44u32.to_le_bytes());
        file.extend_from_slice(b"WAVEfmt ");
        file.extend_from_slice(&16u32.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&8000u32.to_le_bytes());
        file.extend_from_slice(&16000u32.to_le_bytes());
        file.extend_from_slice(&2u16.to_le_bytes());
        file.extend_from_slice(&16u16.to_le_bytes());
        file.extend_from_slice(b"data");
        file.extend_from_slice(&8u32.to_le_bytes());
        file.extend_from_slice(&[0; 8]);
        file
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

    /// The first album's fields as the server has them now.
    fn album_now() -> String {
        match ALBUM_RENAMED.load(Relaxed) {
            true => ALBUM.replace("Night Harbour", "Night Harbour (Remastered)"),
            false => ALBUM.to_owned(),
        }
    }

    /// The plays field of a composer or composition as the server has it
    /// now: `before` the first play is told, then `after`; and none from
    /// Fonoteca's first version of its compositions.
    fn composition_plays(before: u32, after: u32) -> String {
        if OLD_COMPOSITIONS.load(Relaxed) {
            return String::new();
        }
        let plays = match SCROBBLES.lock().unwrap().is_empty() {
            true => before,
            false => after,
        };
        format!(r#","playCount":{plays}"#)
    }

    /// Road Trip's fields as the server has them now.
    fn road_trip_now() -> String {
        if ROAD_TRIP_EDITED.load(Relaxed) {
            ROAD_TRIP.replace("2024-02-01T00:00:00Z", "2024-03-01T00:00:00Z")
        } else {
            ROAD_TRIP.to_owned()
        }
    }

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
    const SYMPHONY: &str = r#""id":"w5","name":"Symphony No. 5","composers":[{"id":"r9","name":"Ludwig van Beethoven"}],"composed":{"from":{"year":1804},"to":{"year":1808}},"albumCount":2,"coverArt":"al-a2","movement":[{"name":"I. Allegro con brio","number":1},{"name":"II. Andante con moto","number":2},{"name":"III. Scherzo","number":3},{"name":"IV. Allegro","number":4}],"song":[{"id":"c1","isDir":false,"title":"Symphony No. 5: I. Allegro con brio","album":"Symphonies","albumId":"a2","coverArt":"al-a2","year":1974,"duration":441,"artist":"Wiener Philharmoniker","artistId":"r7","recorded":{"from":{"year":1974,"month":3,"day":2},"to":{"year":1974,"month":3,"day":2},"places":["Musikverein"]}},{"id":"k1","isDir":false,"title":"Symphony No. 5","album":"Live in Munich","albumId":"a3","coverArt":"al-a3","year":1982,"duration":1900,"artists":[{"id":"r8","name":"Carlos Kleiber"},{"id":"r12","name":"Bayerisches Staatsorchester"}]},{"id":"c2","isDir":false,"title":"Symphony No. 5: II. Andante con moto","album":"Symphonies","albumId":"a2","year":1974,"duration":588,"artists":[{"id":"r7","name":"Wiener Philharmoniker"},{"id":"r8","name":"Carlos Kleiber"}],"recorded":{"from":{"year":1974,"month":4},"to":{"year":1974,"month":4},"places":["Musikverein"]}}]"#;

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
    /// Every call it turned down so, as `ASKED` has them.
    static ASKED_WHILE_DOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the flow test's server holds its answer to a ping, as one
    /// slow to sign in to does, and answers nothing else meanwhile.
    static PING_HELD: AtomicBool = AtomicBool::new(false);
    /// While set, it holds its answer to any other call so.
    static ANSWERS_HELD: AtomicBool = AtomicBool::new(false);
    /// While set, the server hangs up on every folder it is asked for, as
    /// one out of reach does.
    static FOLDERS_UNREACHABLE: AtomicBool = AtomicBool::new(false);
    /// How many covers the server was asked for, and the first ones' ids.
    static COVERS: AtomicUsize = AtomicUsize::new(0);
    static FIRST_COVERS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// How many times the album list was given, and whether it refuses all
    /// but its first part.
    static LISTS: AtomicUsize = AtomicUsize::new(0);
    static LATER_DOWN: AtomicBool = AtomicBool::new(false);
    /// Every call the server was asked, as `PLAYLIST_WRITES` has them.
    static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the first album and the first saved station were renamed
    /// elsewhere, and the album's notes rewritten.
    static ALBUM_RENAMED: AtomicBool = AtomicBool::new(false);
    /// While set, the album page's answer takes a while to come.
    static SLOW_ALBUM: AtomicBool = AtomicBool::new(false);
    /// While set, each part of the library takes a while to come.
    static SLOW_LISTS: AtomicBool = AtomicBool::new(false);
    /// How many hearts were refused: every unstar, and the symphony's star.
    static REFUSED: AtomicUsize = AtomicUsize::new(0);
    /// While set, the server takes only an artist's name for their top
    /// songs, not their id, as Fonoteca does.
    static NAMES_ONLY: AtomicBool = AtomicBool::new(false);
    /// While set, the server has a saved play queue, a song and a whole work
    /// of the classical album a minute into the work's second movement, and
    /// no motion covers; and how many it was asked for.
    static QUEUED: AtomicBool = AtomicBool::new(false);
    /// The play queues the server was asked to save, as `PLAYLIST_WRITES`
    /// has them; while set, it answers the next save as one restarting does,
    /// and every save as one that doesn't let the user.
    static SAVES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static SAVE_DOWN: AtomicBool = AtomicBool::new(false);
    static SAVE_REFUSED: AtomicBool = AtomicBool::new(false);
    static NO_MOTION: AtomicBool = AtomicBool::new(false);
    /// While set, the classical album's motion cover takes a while to come.
    static SLOW_MOTION: AtomicBool = AtomicBool::new(false);
    static MOTIONS_ASKED: AtomicUsize = AtomicUsize::new(0);
    static NO_WAVEFORM: AtomicBool = AtomicBool::new(false);
    /// While set, the server has no Fonoteca compositions.
    static NO_COMPOSITIONS: AtomicBool = AtomicBool::new(false);
    /// While set, the server's compositions are Fonoteca's first version,
    /// whose composers and compositions carry no plays.
    static OLD_COMPOSITIONS: AtomicBool = AtomicBool::new(false);
    /// While set, the server refuses to give the composers.
    static COMPOSERS_DOWN: AtomicBool = AtomicBool::new(false);
    static NO_BANNER: AtomicBool = AtomicBool::new(false);
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
    /// The request targets ListenBrainz's statistics were asked at.
    static STATISTICS_ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// The Authorization header of each of those requests, and whether each
    /// cover art archive request had one.
    static STATISTICS_AUTHORIZED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static ARCHIVE_AUTHORIZED: Mutex<Vec<bool>> = Mutex::new(Vec::new());
    /// The radio station changes, the same way.
    static RADIO_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the server has playlist folders and Fonoteca's tags; the
    /// changes to those and to where playlists are, the same way.
    static ORGANIZED: AtomicBool = AtomicBool::new(false);
    static ORGANIZE_WRITES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// While set, the server refuses to give the tags.
    static TAGS_DOWN: AtomicBool = AtomicBool::new(false);
    /// Whether the server takes its time over a song's download.
    static SLOW_DOWNLOAD: AtomicBool = AtomicBool::new(false);
    /// Whether Road Trip has been edited on the server since: changed later,
    /// and Rope and Anchor in place of Fugue.
    static ROAD_TRIP_EDITED: AtomicBool = AtomicBool::new(false);
    /// The albums deleted on the server since, by id.
    static GONE_ALBUMS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// Whether a playlist named Later Jams for joe was made elsewhere.
    static LATER_JAMS_ELSEWHERE: AtomicBool = AtomicBool::new(false);
    /// Whether the station Wave was saved elsewhere.
    static WAVE_ELSEWHERE: AtomicBool = AtomicBool::new(false);
    /// Whether the server converts what `stream` asks for, or sends the
    /// original (FLAC) as Fonoteca does.
    static STREAM_CONVERTS: AtomicBool = AtomicBool::new(false);
    /// The plays the server was told of, as `PLAYLIST_WRITES` has them, and
    /// how often it was asked for the albums played last: none until it
    /// was told of a play, then the first album and one by Anna Bach and
    /// Mara Lind. The songs it was told play now, whether it can't be
    /// reached for either, and how many plays it turned away so.
    static SCROBBLES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static RECENT_ASKS: AtomicUsize = AtomicUsize::new(0);
    static PLAYING_NOW: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static SCROBBLE_DOWN: AtomicBool = AtomicBool::new(false);
    static PLAYS_TURNED_AWAY: AtomicUsize = AtomicUsize::new(0);

    /// What names `username` on `server` on this device.
    fn account_of(server: &str, username: &str) -> String {
        let auth = Auth::Password {
            username: username.into(),
            password: String::new(),
        };
        Client::new(server, auth)
            .unwrap()
            .account(&prefs::answers_salt())
    }

    /// Where the pages' answers are kept for `username` on `server`.
    fn answers_kept_for(server: &str, username: &str) -> std::path::PathBuf {
        answers::file(&account_of(server, username)).unwrap()
    }

    /// Where what ListenBrainz answered is kept for `username` on `server`.
    fn listenbrainz_kept_for(server: &str, username: &str) -> std::path::PathBuf {
        answers::file(&browse::listenbrainz_account(&account_of(server, username))).unwrap()
    }

    /// Where the plays are logged for `username` on `server`.
    fn plays_kept_for(server: &str, username: &str) -> std::path::PathBuf {
        plays::file(&account_of(server, username)).unwrap()
    }

    /// How many times the server was asked for `method`.
    fn times_asked(method: &str) -> usize {
        let call = format!("{method} ");
        let asked = ASKED.lock().unwrap();
        asked
            .iter()
            .filter(|asked| asked.starts_with(&call))
            .count()
    }

    /// Whether the server was asked for `method` after the last `write`.
    fn asked_after(write: &str, method: &str) -> bool {
        let (write, method) = (format!("{write} "), format!("{method} "));
        let asked = ASKED.lock().unwrap();
        let last = |call: &str| asked.iter().rposition(|asked| asked.starts_with(call));
        last(&method) > last(&write)
    }

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

    /// What the flow test's ListenBrainz was asked, as each request's line
    /// and whether it carried the token "tok", and whether it is down.
    static LISTENBRAINZ_ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// When each of those was asked.
    static LISTENBRAINZ_AT: Mutex<Vec<Instant>> = Mutex::new(Vec::new());
    static LISTENBRAINZ_DOWN: AtomicBool = AtomicBool::new(false);
    /// Whether the stand-in answers for the playlist "later" after a pause, and
    /// whether it has answered, to show a tile's banner before the last's tracks.
    static LATER_SLOW: AtomicBool = AtomicBool::new(false);
    static LATER_ANSWERED: AtomicBool = AtomicBool::new(false);
    /// How long the stand-in takes to answer for the playlist "later" when it is slow.
    const LATER_PAUSE: Duration = Duration::from_millis(900);
    /// The playlists ListenBrainz says it changed since it made them.
    static PLAYLISTS_CHANGED: Mutex<Vec<&str>> = Mutex::new(Vec::new());
    /// While set, the server gives Mara Lind another banner, after a pause,
    /// and which banners it was asked for.
    static BANNER_CHANGED: AtomicBool = AtomicBool::new(false);
    static BANNERS_ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// How many searches for a song by its title the server was asked, and
    /// whether it refuses them.
    static SEARCHES: AtomicUsize = AtomicUsize::new(0);
    static SEARCH_DOWN: AtomicBool = AtomicBool::new(false);
    /// While set, the library has a song of "Not Here" too, and its search
    /// for Cher's "Familiar" takes a while.
    static NOT_HERE_ADDED: AtomicBool = AtomicBool::new(false);
    static CHER_SLOW: AtomicBool = AtomicBool::new(false);
    /// What the global search asked the server and the radio directory for,
    /// in turn.
    static GLOBAL_SEARCHES: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static STATION_SEARCHES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// The songs of the library the ListenBrainz playlists are matched
    /// against, by what a search for them starts with: two share a title, one
    /// of them by another artist; the third is accented in the library, not in
    /// ListenBrainz.
    const LISTENBRAINZ_LIBRARY: [(&str, &str); 5] = [
        (
            "familiar",
            r#"{"id":"lb9","isDir":false,"title":"Familiar","artist":"Someone Else","duration":200,"musicBrainzId":"00000000-0000-4000-8000-000000000009"}"#,
        ),
        (
            "familiar",
            r#"{"id":"lb1","isDir":false,"title":"Familiar","artist":"Nils Frahm","duration":210,"musicBrainzId":"31dad9e2-25fc-48aa-8c01-72c3729f53a1"}"#,
        ),
        (
            "cafe del mar",
            r#"{"id":"lb2","isDir":false,"title":"Café del Mar","artist":"ENERGY 52","duration":420}"#,
        ),
        (
            "fugue",
            r#"{"id":"s9","isDir":false,"title":"Fugue","duration":218,"artist":"Anna Bach"}"#,
        ),
        (
            "salt",
            r#"{"id":"lb5","isDir":false,"title":"Salt","duration":180,"artist":"Mara Lind"}"#,
        ),
    ];

    /// The songs of `LISTENBRAINZ_LIBRARY` a search finds.
    fn library_search(query: &str) -> String {
        let query = query.replace("%20", " ").replace('+', " ").to_lowercase();
        let mut found: Vec<&str> = LISTENBRAINZ_LIBRARY
            .iter()
            .filter(|(searched, _)| query.starts_with(searched))
            .map(|(_, song)| *song)
            .collect();
        if NOT_HERE_ADDED.load(Relaxed) && query.starts_with("not here") {
            found.push(
                r#"{"id":"lb6","isDir":false,"title":"Not Here","artist":"Nobody","duration":200}"#,
            );
        }
        if CHER_SLOW.load(Relaxed) && query.starts_with("familiar cher") {
            std::thread::sleep(Duration::from_millis(600));
        }
        format!(r#""searchResult3":{{"song":[{}]}}"#, found.join(","))
    }

    /// What the global search finds on the server: Mara Lind, her album and
    /// her song for "Mara", and for "harbour" after a while; for "slow"
    /// another album, later still, which a search made meanwhile must never
    /// show; nothing for anything else.
    fn global_search(query: &str) -> String {
        GLOBAL_SEARCHES.lock().unwrap().push(query.to_owned());
        let hers = format!(
            r#""artist":[{{"id":"r1","name":"Mara Lind","coverArt":"ar-r1","albumCount":1}}],"album":[{{{ALBUM}}}],"song":[{HARBOUR_LIGHTS}]"#
        );
        let (wait, found) = match query.to_lowercase().as_str() {
            "mara" => (0, hers),
            "harbour" => (300, hers),
            "slow" => (
                600,
                r#""album":[{"id":"a9","name":"Slow Boat","songCount":1,"duration":60,"created":"2021-01-01T00:00:00Z"}]"#.to_owned(),
            ),
            _ => (0, String::new()),
        };
        std::thread::sleep(Duration::from_millis(wait));
        format!(r#""searchResult3":{{{found}}}"#)
    }

    /// A stand-in for ListenBrainz's API, which knows joe: his three
    /// playlists, one of them the weekly exploration, and two of them.
    fn listenbrainz(request: &str) -> Vec<u8> {
        let line = request.lines().next().unwrap_or_default();
        let token = request.to_lowercase().contains("authorization: token tok");
        LISTENBRAINZ_ASKED
            .lock()
            .unwrap()
            .push(format!("{line} token={token}"));
        LISTENBRAINZ_AT.lock().unwrap().push(Instant::now());
        if LISTENBRAINZ_DOWN.load(Relaxed) {
            return reply("503 Service Unavailable", "text/plain", b"down");
        }
        let playlist = |title: &str, id: &str, patch: &str, tracks: &str| {
            // The weekly ones a week apart, the newest first.
            let date = match id {
                "later" => "2026-10-26T22:00:00+00:00",
                "trip" => "2026-11-02T22:00:00+00:00",
                _ => "2026-11-04T22:00:00+00:00",
            };
            let changed = match PLAYLISTS_CHANGED.lock().unwrap().contains(&id) {
                true => "2026-11-05T22:00:00+00:00",
                false => date,
            };
            format!(
                r#"{{"playlist":{{"title":"{title}","identifier":"https://listenbrainz.org/playlist/{id}","date":"{date}","annotation":"<p>Made for joe</p>","track":[{tracks}],"extension":{{"https://musicbrainz.org/doc/jspf#playlist":{{"last_modified_at":"{changed}","additional_metadata":{{"algorithm_metadata":{{"source_patch":"{patch}"}}}}}}}}}}}}"#
            )
        };
        let track = |title: &str, artist: &str, mbid: Option<&str>| {
            let identifier = mbid.map_or(String::new(), |mbid| {
                format!(r#""identifier":["https://musicbrainz.org/recording/{mbid}"],"#)
            });
            format!(
                r#"{{{identifier}"title":"{title}","creator":"{artist}","album":"Album","duration":200000}}"#
            )
        };
        let daily = [
            track(
                "Familiar",
                "Nils Frahm",
                Some("31dad9e2-25fc-48aa-8c01-72c3729f53a1"),
            ),
            track("Cafe del Mar", "Energy 52", None),
            track("Not Here", "Nobody", None),
            track("Familiar", "Cher", None),
        ]
        .join(",");
        let lists = [
            (
                "Daily Jams for joe, 2026-11-04 Wed",
                "daily",
                "daily-jams",
                daily.as_str(),
            ),
            (
                "Weekly Exploration for joe",
                "exploring",
                "weekly-exploration",
                &track("Lost", "Nobody", None),
            ),
            (
                "Road Trip",
                "trip",
                "weekly-jams",
                // Two credits: the second is the library's artist with a banner.
                r#"{"title":"Fugue","creator":"Anna Bach, Mara Lind","album":"Album","duration":200000,"extension":{"https://musicbrainz.org/doc/jspf#track":{"additional_metadata":{"artists":[{"artist_credit_name":"Anna Bach","join_phrase":", "},{"artist_credit_name":"Mara Lind","join_phrase":""}]}}}}"#,
            ),
            (
                "Later Jams for joe",
                "later",
                "weekly-jams",
                &track("Salt", "Mara Lind", None),
            ),
        ];
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let body = if target == "/1/user/joe/playlists/createdfor" {
            let all: Vec<String> = (lists.iter())
                .map(|(title, id, patch, _)| playlist(title, id, patch, ""))
                .collect();
            format!(
                r#"{{"count":4,"offset":0,"playlist_count":4,"playlists":[{}]}}"#,
                all.join(",")
            )
        } else if let Some(id) = target.strip_prefix("/1/playlist/") {
            match lists.iter().find(|list| list.1 == id) {
                Some((title, id, patch, tracks)) => {
                    if *id == "later" && LATER_SLOW.load(Relaxed) {
                        std::thread::sleep(LATER_PAUSE);
                        LATER_ANSWERED.store(true, Relaxed);
                    }
                    playlist(title, id, patch, tracks)
                }
                None => {
                    return reply(
                        "404 Not Found",
                        "application/json",
                        br#"{"error":"no such playlist"}"#,
                    );
                }
            }
        } else if let Some(query) = target.strip_prefix("/1/user/joe/listens?") {
            listens_of_joe(query)
        } else if target == "/1/user/joe/listen-count" {
            format!(r#"{{"payload":{{"count":{}}}}}"#, joe_listen_count())
        } else if target == "/1/feedback/recording-feedback" {
            r#"{"status":"ok"}"#.to_owned()
        } else if target.starts_with("/1/feedback/user/joe/get-feedback") {
            format!(
                r#"{{"count":1,"offset":0,"total_count":1,"feedback":[{{"user_id":"joe","recording_msid":null,"score":1,"recording_mbid":"{FAMILIAR_RECORDING}","track_metadata":{{"artist_name":"Nils Frahm","track_name":"Familiar"}}}}]}}"#
            )
        } else {
            return reply(
                "404 Not Found",
                "application/json",
                br#"{"error":"unknown"}"#,
            );
        };
        json(&body)
    }

    /// The recording ListenBrainz's stand-in has joe love.
    const FAMILIAR_RECORDING: &str = "31dad9e2-25fc-48aa-8c01-72c3729f53a1";

    /// How many listens joe has, and when he heard the last; they are ten
    /// minutes apart.
    const JOE_LISTENS: i64 = 150;
    const JOE_LISTENED_LAST: i64 = 1_767_225_600;
    const JOE_LISTENS_APART: i64 = 600;

    /// While set, joe has heard one listen more, ten minutes after the last;
    /// and ListenBrainz counts one listen of his more than it gives, as one
    /// that came late with an older time.
    static JOE_NEW_LISTEN: AtomicBool = AtomicBool::new(false);
    static JOE_LATE_LISTEN: AtomicBool = AtomicBool::new(false);

    /// When joe heard his `n`th listen, counting from the newest.
    fn joe_listened_at(n: i64) -> i64 {
        JOE_LISTENED_LAST - n * JOE_LISTENS_APART
    }

    /// When joe heard each of his listens, newest first.
    fn joe_listens() -> impl DoubleEndedIterator<Item = i64> {
        let newest =
            (JOE_NEW_LISTEN.load(Relaxed)).then_some(JOE_LISTENED_LAST + JOE_LISTENS_APART);
        newest
            .into_iter()
            .chain((0..JOE_LISTENS).map(joe_listened_at))
    }

    /// How many listens ListenBrainz counts for joe.
    fn joe_listen_count() -> usize {
        joe_listens().count() + usize::from(JOE_LATE_LISTEN.load(Relaxed))
    }

    /// A page of joe's listens, as ListenBrainz pages them: the newest, those
    /// before `max_ts` or those just after `min_ts`, newest first. They are
    /// of four tracks in turn: one the playlists above found by recording,
    /// one the library has by title and artist that they didn't, one the
    /// library lacks, and one ListenBrainz matched that they found by title
    /// and artist.
    fn listens_of_joe(query: &str) -> String {
        let number = |name: &str| {
            query.split('&').find_map(|param| {
                param
                    .strip_prefix(name)?
                    .strip_prefix('=')?
                    .parse::<i64>()
                    .ok()
            })
        };
        let all = joe_listens();
        let count = number("count").unwrap_or(25) as usize;
        let page: Vec<i64> = match (number("max_ts"), number("min_ts")) {
            (Some(before), _) => all.filter(|&at| at < before).take(count).collect(),
            (None, Some(after)) => {
                let mut newer: Vec<i64> = all.rev().filter(|&at| at > after).take(count).collect();
                newer.reverse();
                newer
            }
            (None, None) => all.take(count).collect(),
        };
        let listen = |at: i64| {
            let metadata = match (JOE_LISTENED_LAST - at) / JOE_LISTENS_APART % 4 {
                0 => format!(
                    r#""track_name":"Familiar","artist_name":"Nils Frahm","additional_info":{{"recording_mbid":"{FAMILIAR_RECORDING}","tracknumber":3}}"#
                ),
                1 => r#""track_name":"Salt","artist_name":"Mara Lind","additional_info":{"recording_mbid":"00000000-0000-4000-8000-000000000005","artist_names":["Mara Lind"]}"#.to_owned(),
                2 => r#""track_name":"Gone","artist_name":"Nobody""#.to_owned(),
                _ => r#""track_name":"Cafe del Mar","artist_name":"Energy 52","mbid_mapping":{"artists":[{"artist_credit_name":"Energy 52"}]}"#.to_owned(),
            };
            format!(r#"{{"listened_at":{at},"user_name":"joe","track_metadata":{{{metadata}}}}}"#)
        };
        let listens: Vec<String> = page.iter().copied().map(listen).collect();
        format!(
            r#"{{"payload":{{"count":{},"user_id":"joe","latest_listen_ts":{},"oldest_listen_ts":{},"listens":[{}]}}}}"#,
            listens.len(),
            joe_listens().next().unwrap(),
            joe_listened_at(JOE_LISTENS - 1),
            listens.join(",")
        )
    }

    /// The radio directory: two stations with the most votes, 65 of jazz
    /// with the server's Jazz first, one of rock, none of other genres, one
    /// of the Netherlands, one of Dutch jazz, two named News, the countries'
    /// names, three languages, one without a code, and a picture of each
    /// station.
    static LRCLIB_ASKED: AtomicUsize = AtomicUsize::new(0);

    /// LRCLIB with words for "Moonlit" only; its other songs are a 404.
    fn lyrics_service(request: &str) -> Vec<u8> {
        LRCLIB_ASKED.fetch_add(1, Relaxed);
        if request.contains("track_name=Moonlit") && request.contains("duration=200") {
            json(
                r#"{"id":1,"instrumental":false,"plainLyrics":"Moon on the water\nUnder the stars","syncedLyrics":"[ar:Mara Lind]\n[00:01.00] Moon on the water\n[00:05.00] Under the stars"}"#,
            )
        } else {
            reply(
                "404 Not Found",
                "application/json",
                br#"{"message":"Failed to find specified track","name":"TrackNotFound","statusCode":404}"#,
            )
        }
    }

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
        } else if let Some(name) = target
            .strip_prefix("/json/stations/search?name=")
            .filter(|_| !target.contains("nameExact"))
        {
            let name = name.split('&').next().unwrap_or_default();
            // Each search's station has a logo none before it had.
            let n = {
                let mut searches = STATION_SEARCHES.lock().unwrap();
                searches.push(name.to_owned());
                searches.len()
            };
            list(vec![station(300 + n, &format!("{name} FM"), "NL")])
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

    /// ListenBrainz's statistics of rob: this year, three albums of which two
    /// are in the library (one by its release group's ID, one by its names)
    /// and one is not, three artists, and no genres worked out; all time,
    /// which is slow, with one genre; nothing for any other range. The cover
    /// art archive is on the same server.
    fn listenbrainz_statistics(request: &str) -> Vec<u8> {
        let target = request.split_whitespace().nth(1).unwrap_or_default();
        let authorization = (request.lines()).find_map(|line| {
            line.to_lowercase()
                .strip_prefix("authorization: ")
                .map(str::to_owned)
        });
        if target.starts_with("/release/") {
            ARCHIVE_AUTHORIZED
                .lock()
                .unwrap()
                .push(authorization.is_some());
            return reply("200 OK", "image/png", PNG);
        }
        STATISTICS_AUTHORIZED.lock().unwrap().extend(authorization);
        STATISTICS_ASKED.lock().unwrap().push(target.to_owned());
        let range = target.split("range=").nth(1).unwrap_or_default();
        let range = range.split('&').next().unwrap_or_default();
        let kind = target.split('/').nth(5).unwrap_or_default();
        let kind = kind.split('?').next().unwrap_or_default();
        if range == "all_time" && kind == "release-groups" {
            std::thread::sleep(Duration::from_millis(400));
        }
        let none = || reply("204 No Content", "application/json", b"");
        match (range, kind) {
            ("this_year", "release-groups") => json(
                r#"{"payload":{"release_groups":[
                {"artist_name":"Bach Ensemble","release_group_mbid":"rg-sonatas","release_group_name":"Sonatas (Deluxe)","listen_count":18},
                {"artist_name":"mara lind","release_group_name":"night harbour","listen_count":12},
                {"artist_name":"Cell","release_group_name":"Away","caa_id":42,"caa_release_mbid":"7c45","listen_count":1}]}}"#,
            ),
            ("this_year", "artists") => json(
                r#"{"payload":{"artists":[
                {"artist_name":"MARA LIND","listen_count":30},
                {"artist_mbid":"MB-ANNA","artist_name":"A. Bach","listen_count":9},
                {"artist_name":"Röyksopp","listen_count":2}]}}"#,
            ),
            ("all_time", "release-groups") => json(
                r#"{"payload":{"release_groups":[
                {"artist_name":"Mara Lind","release_group_name":"Night Harbour","listen_count":480}]}}"#,
            ),
            ("all_time", "artists") => json(r#"{"payload":{"artists":[]}}"#),
            ("all_time", "genre-activity") => json(
                r#"{"payload":{"genre_activity":[
                {"genre":"folk","hour":1,"listen_count":20},
                {"genre":"folk","hour":9,"listen_count":10},
                {"genre":"shoegaze","hour":9,"listen_count":4},
                {"genre":"ambient","hour":9,"listen_count":3},
                {"genre":"dub","hour":9,"listen_count":2},
                {"genre":"drone","hour":9,"listen_count":1}]}}"#,
            ),
            _ => none(),
        }
    }

    fn flaky(request: &str) -> Vec<u8> {
        let held = match request.contains("/rest/ping") {
            true => &PING_HELD,
            false => &ANSWERS_HELD,
        };
        while held.load(Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
        if DOWN.load(Relaxed) {
            ASKED_WHILE_DOWN.lock().unwrap().push(write_of(request));
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
    /// asks for at once, as many as its scans count once done. It takes every
    /// playlist change but a rename to "Taken".
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
        ASKED.lock().unwrap().push(write_of(request));
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
            let composition_versions = if OLD_COMPOSITIONS.load(Relaxed) {
                "1"
            } else {
                "1,2"
            };
            let compositions = if NO_COMPOSITIONS.load(Relaxed) {
                String::new()
            } else {
                format!(r#"{{"name":"fonotecaCompositions","versions":[{composition_versions}]}},"#)
            };
            let banner = if NO_BANNER.load(Relaxed) {
                ""
            } else {
                r#"{"name":"fonotecaArtistBanner","versions":[1]},"#
            };
            let genre_versions = if OLD_GENRES.load(Relaxed) { "1" } else { "1,2" };
            let offset = if TRANSCODE_OFFSET.load(Relaxed) {
                r#",{"name":"transcodeOffset","versions":[1]}"#
            } else {
                ""
            };
            ok(&format!(
                r#""openSubsonicExtensions":[{{"name":"songLyrics","versions":[1]}},{motion}{waveform}{compositions}{{"name":"fonotecaGenres","versions":[{genre_versions}]}},{banner}{{"name":"fonotecaRadioCatalogue","versions":[1,2]}}{by_id}{organized}{offset}]"#
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
        } else if request.contains("/rest/getAlbumList2") && param("type") == Some("recent") {
            RECENT_ASKS.fetch_add(1, Relaxed);
            let played = if SCROBBLES.lock().unwrap().is_empty() {
                String::new()
            } else {
                format!(
                    r#"{{{ALBUM}}},{{"id":"a9","name":"Duets","artists":[{{"id":"r3","name":"Anna Bach"}},{{"id":"r1","name":"Mara Lind"}}],"coverArt":"al-a9","songCount":0,"duration":0,"created":"2020-01-01T00:00:00Z"}}"#
                )
            };
            ok(&format!(r#""albumList2":{{"album":[{played}]}}"#))
        } else if request.contains("/rest/getAlbumList2") {
            let number = |name: &str| param(name).and_then(|n| n.parse::<usize>().ok());
            if LATER_DOWN.load(Relaxed) && number("offset").unwrap_or(0) > 0 {
                return reply("503 Service Unavailable", "text/plain", b"down");
            }
            if SLOW_LISTS.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(300));
            }
            LISTS.fetch_add(1, Relaxed);
            let first = match SCROBBLES.lock().unwrap().is_empty() {
                true => format!("{{{}}}", album_now()),
                false => format!(r#"{{{},"playCount":5}}"#, album_now()),
            };
            let album = |n: u32| {
                format!(
                    r#"{{"id":"a{n}","name":"Other {n}","coverArt":"al-a{n}","songCount":0,"duration":0,"created":"2020-01-01T00:00:00Z"}}"#
                )
            };
            let albums: Vec<String> = std::iter::once(first)
                .chain((2..=600).map(album))
                .chain([
                    r#"{"id":"b1","name":"Sonatas","artist":"Bach Ensemble","musicBrainzId":"RG-SONATAS","year":2010,"songCount":0,"duration":0,"created":"2022-01-01T00:00:00Z"}"#.to_owned(),
                    r#"{"id":"b3","name":"Variations on Bach","artist":"Anna Bach","year":0,"songCount":0,"duration":0,"playCount":3,"created":"2023-01-01T00:00:00Z"}"#.to_owned(),
                    r#"{"id":"b2","name":"partitas","artist":"Anna Bach","year":2010,"songCount":0,"duration":0,"playCount":3,"created":"2024-01-01T00:00:00Z"}"#.to_owned(),
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
        } else if request.contains("/rest/getLyricsBySongId") {
            if id == Some("t1") {
                ok(
                    r#""lyricsList":{"structuredLyrics":[{"lang":"en","synced":false,"line":[{"value":"Lights on the water"},{"value":"And the boats come home"}]}]}"#,
                )
            } else {
                ok(r#""lyricsList":{"structuredLyrics":[]}"#)
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
            let notes = r#""albumInfo":{"notes":"Recorded at sea &amp; on land.<br/><a href=\"https://example.org\">Read more</a>"}"#;
            match ALBUM_RENAMED.load(Relaxed) {
                true => ok(&notes.replace("Recorded", "Remastered")),
                false => ok(notes),
            }
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
        } else if request.contains("/rest/download?") {
            if SLOW_DOWNLOAD.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(600));
            }
            reply("200 OK", "audio/wav", &tiny_wav())
        } else if request.contains("/rest/getAlbum?")
            && id.is_some_and(|id| GONE_ALBUMS.lock().unwrap().iter().any(|gone| gone == id))
        {
            json(
                r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":70,"message":"Album not found"}}}"#,
            )
        } else if request.contains("/rest/getAlbum") && id == Some("a2") {
            ok(&format!(r#""album":{{{CLASSICAL}}}"#))
        } else if request.contains("/rest/getAlbum") {
            let songs = r#"[{"id":"s1","isDir":false,"title":"Harbour Lights","track":1,"discNumber":1,"duration":252,"starred":"2024-01-01T00:00:00Z","artists":[{"id":"r1","name":"Mara Lind"}]},{"id":"s2","isDir":false,"title":"Rope and Anchor","track":1,"discNumber":2,"duration":218,"artist":"Mara Lind","artistId":"r1"}]"#;
            let songs = if STARRED.lock().unwrap().contains(&"s2".to_owned()) {
                songs.replace(
                    r#""title":"Rope and Anchor","#,
                    r#""title":"Rope and Anchor","starred":"2024-02-01T00:00:00Z","#,
                )
            } else {
                songs.to_owned()
            };
            if SLOW_ALBUM.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(400));
            }
            ok(&format!(r#""album":{{{},"song":{songs}}}"#, album_now()))
        } else if request.contains("/rest/getSong") && id == Some("t1") {
            ok(&format!(r#""song":{HARBOUR_LIGHTS}"#))
        } else if request.contains("/rest/getSong") && id == Some("t2") {
            ok(
                r#""song":{"id":"t2","parent":"d2","isDir":false,"title":"Rope and Anchor","duration":218}"#,
            )
        } else if request.contains("/rest/getIndexes") {
            ok(
                r#""indexes":{"ignoredArticles":"The","lastModified":1,"index":[{"name":"M","artist":[{"id":"d1","name":"Mara Lind"}]},{"name":"S","artist":[{"id":"d3","name":"Singles","starred":"2024-01-01T00:00:00Z"}]}],"child":[{"id":"t9","isDir":false,"title":"Loose Song","duration":30}]}"#,
            )
        } else if request.contains("/rest/getMusicDirectory") && FOLDERS_UNREACHABLE.load(Relaxed) {
            Vec::new()
        } else if request.contains("/rest/getMusicDirectory") && id == Some("d3") {
            ok(r#""directory":{"id":"d3","parent":"root","name":"Singles","child":[]}"#)
        } else if request.contains("/rest/getMusicDirectory") && id == Some("d2") {
            // The way back to her folder, as a link loop on disk makes.
            ok(
                r#""directory":{"id":"d2","parent":"d1","name":"Night Harbour","child":[{"id":"t2","parent":"d2","isDir":false,"title":"Rope and Anchor","duration":218,"coverArt":"al-a1"},{"id":"d1","parent":"d2","isDir":true,"title":"Mara Lind"}]}"#,
            )
        } else if request.contains("/rest/getMusicDirectory") && id == Some("d1") {
            ok(
                r#""directory":{"id":"d1","parent":"root","name":"Mara Lind","child":[{"id":"d2","parent":"d1","isDir":true,"title":"Night Harbour","coverArt":"al-a1"},{"id":"t1","parent":"d1","isDir":false,"title":"Harbour Lights","duration":252}]}"#,
            )
        } else if request.contains("/rest/getArtists") {
            let changed = BANNER_CHANGED.load(Relaxed);
            if changed {
                // The playlists made for the user have come by then.
                std::thread::sleep(LATER_PAUSE);
            }
            let banner = if changed { "ba-r1b" } else { "ba-r1" };
            let others: Vec<String> = (1..=120)
                .map(|n| {
                    format!(
                        r#"{{"id":"o{n}","name":"Other {n}","coverArt":"ar-o{n}","albumCount":1}}"#
                    )
                })
                .collect();
            ok(&format!(
                r#""artists":{{"ignoredArticles":"The","index":[{{"name":"A","artist":[{{"id":"r3","name":"Anna Bach","musicBrainzId":"mb-anna","albumCount":2}}]}},{{"name":"M","artist":[{{"id":"r1","name":"Mara Lind","coverArt":"ar-r1","bannerArt":"{banner}","albumCount":1}}]}},{{"name":"O","artist":[{}]}}]}}"#,
                others.join(",")
            ))
        } else if request.contains("/rest/getArtist?") && id == Some("o120") {
            // An album of their own, and a compilation they are on.
            ok(
                r#""artist":{"id":"o120","name":"Other 120","album":[{"id":"a120","name":"Other 120","artistId":"o120","songCount":2,"duration":470,"created":"2024-01-01T00:00:00Z"},{"id":"a2","name":"Classics","artistId":"o120","songCount":7,"duration":1,"created":"2024-01-01T00:00:00Z","isCompilation":true}]}"#,
            )
        } else if request.contains("/rest/getArtist") {
            ok(&format!(
                r#""artist":{{"id":"r1","name":"Mara Lind","coverArt":"ar-r1","bannerArt":"ba-r1","album":[{{{}}}]}}"#,
                album_now()
            ))
        } else if request.contains("/rest/search3") && param("artistCount").is_none() {
            ok(&global_search(param("query").unwrap_or_default()))
        } else if request.contains("/rest/search3") && param("query").is_some_and(|q| !q.is_empty())
        {
            if SEARCH_DOWN.load(Relaxed) {
                return reply("503 Service Unavailable", "text/plain", b"down");
            }
            SEARCHES.fetch_add(1, Relaxed);
            ok(&library_search(param("query").unwrap_or_default()))
        } else if request.contains("/rest/search3") {
            let number = |name: &str| param(name).and_then(|n| n.parse::<usize>().ok());
            let plays_told = SCROBBLES.lock().unwrap().len();
            let played = plays_told > 0;
            let song = |n: usize| {
                if n == 1 && played {
                    HARBOUR_LIGHTS.replace(r#""isDir":false"#, r#""isDir":false,"playCount":5"#)
                } else if n == 1 {
                    HARBOUR_LIGHTS.to_owned()
                } else if n == 2 {
                    // Also on the Road Trip playlist.
                    r#"{"id":"s9","isDir":false,"title":"Fugue","duration":218,"artist":"Anna Bach"}"#.to_owned()
                } else {
                    let plays = match n {
                        // The second play told is Song 5's.
                        5 if plays_told > 1 => r#","playCount":4"#,
                        5 => r#","playCount":3"#,
                        3 | 8 => r#","playCount":2"#,
                        _ => "",
                    };
                    format!(
                        r#"{{"id":"t{n}","isDir":false,"title":"Song {n}","duration":60{plays}}}"#
                    )
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
        } else if request.contains("/rest/getStarred2") {
            ok(r#""starred2":{"song":[]}"#)
        } else if request.contains("/rest/getPlaylists") {
            let road_trip = if MOVED.load(Relaxed) {
                road_trip_now().replace(
                    r#""id":"p1","name":"Road Trip""#,
                    r#""id":"p4","name":"Moved""#,
                )
            } else {
                road_trip_now()
            };
            let road_trip = if ORGANIZED.load(Relaxed) {
                road_trip.replacen(r#""name":"#, r#""playlistFolderId":"f1","name":"#, 1)
            } else {
                road_trip
            };
            let later_jams = match LATER_JAMS_ELSEWHERE.load(Relaxed) {
                true => {
                    r#",{"id":"p9","name":"Later Jams for joe","songCount":0,"duration":0,"created":"2024-01-01T00:00:00Z","changed":"2024-01-01T00:00:00Z"}"#
                }
                false => "",
            };
            ok(&format!(
                r#""playlists":{{"playlist":[{{{road_trip}}},{{"id":"p2","name":"Empty","songCount":0,"duration":0,"created":"2024-01-01T00:00:00Z","changed":"2024-01-01T00:00:00Z","readonly":true}}{later_jams}]}}"#
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
        } else if request.contains("/rest/getRadioCatalogue") {
            ok(
                r#""radioCatalogue":{"internetRadioStation":[{"id":"rc-canal.xspf","name":"Canal","streamUrl":"https://catalogue.example.org/canal","homePageUrl":"https://canal.example.org/","coverArt":"rc-canal.xspf","countryCode":"NL","languageCodes":["nl"],"genres":["jazz"]},{"id":"rc-jazz.xspf","name":"Jazz","streamUrl":"https://stream.example.org/jazz","coverArt":"rc-jazz.xspf","countryCode":"NL","languageCodes":["nl"],"genres":["jazz"]},{"id":"rc-berlin.xspf","name":"Berlin","streamUrl":"https://catalogue.example.org/berlin","coverArt":"rc-berlin.xspf","countryCode":"DE","languageCodes":["de"],"genres":["news"]}]}"#,
            )
        } else if request.contains("/rest/getInternetRadioStations") && ALBUM_RENAMED.load(Relaxed)
        {
            ok(
                r#""internetRadioStations":{"internetRadioStation":[{"id":"r1","name":"Jazz FM","streamUrl":"https://stream.example.org/jazz"}]}"#,
            )
        } else if request.contains("/rest/getInternetRadioStations") {
            let wave = match WAVE_ELSEWHERE.load(Relaxed) {
                true => r#",{"id":"r3","name":"Wave","streamUrl":"https://s1.example.org/listen"}"#,
                false => "",
            };
            ok(&format!(
                r#""internetRadioStations":{{"internetRadioStation":[{{"id":"r1","name":"Jazz","streamUrl":"https://stream.example.org/jazz","homePageUrl":"https://radio.example.org/","coverArt":"ir-r1"}},{{"id":"r2","name":"News","streamUrl":"http://news.example.org/live.mp3"}}{wave}]}}"#
            ))
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
            let kind = if STREAM_CONVERTS.load(Relaxed) {
                "audio/ogg"
            } else {
                "audio/flac"
            };
            reply("200 OK", kind, &tiny_wav())
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
            let second = if ROAD_TRIP_EDITED.load(Relaxed) {
                r#"{"id":"s2","isDir":false,"title":"Rope and Anchor","duration":218,"artist":"Mara Lind"}"#
            } else {
                r#"{"id":"s9","isDir":false,"title":"Fugue","duration":218,"artist":"Anna Bach"}"#
            };
            let songs = format!(
                r#"[{{"id":"s1","isDir":false,"title":"Harbour Lights","album":"Night Harbour","albumId":"a1","duration":252,"starred":"2024-01-01T00:00:00Z","artists":[{{"id":"r1","name":"Mara Lind"}}]}},{second}]"#
            );
            ok(&format!(
                r#""playlist":{{{},"entry":{songs}}}"#,
                road_trip_now()
            ))
        } else if request.contains("/rest/getComposers") && COMPOSERS_DOWN.load(Relaxed) {
            reply("503 Service Unavailable", "text/plain", b"down")
        } else if request.contains("/rest/getComposers") {
            let (beethoven, mozart) = (composition_plays(10, 22), composition_plays(12, 12));
            ok(&format!(
                r#""composers":{{"index":[{{"name":"B","composer":[{{"id":"r9","name":"Ludwig van Beethoven","sortName":"Beethoven, Ludwig van","coverArt":"ar-r9","compositionCount":2{beethoven}}}]}},{{"name":"M","composer":[{{"id":"r10","name":"Wolfgang Amadeus Mozart","compositionCount":1{mozart}}}]}}]}}"#,
            ))
        } else if request.contains("/rest/getCompositions") && id == Some("w5") {
            ok(&format!(
                r#""compositions":{{"composition":[{{{SYMPHONY}}}]}}"#
            ))
        } else if request.contains("/rest/getCompositions") {
            let egmont_plays = composition_plays(3, 15);
            let egmont = format!(
                r#"{{"id":"w9","name":"Egmont Overture","composers":[{{"id":"r9","name":"Ludwig van Beethoven"}}],"composed":{{"from":{{"year":1809}},"to":{{"year":1810}}}},"albumCount":1{egmont_plays}}}"#
            );
            let requiem_plays = composition_plays(12, 12);
            let requiem = format!(
                r#"{{"id":"w7","name":"Requiem","composers":[{{"id":"r10","name":"Wolfgang Amadeus Mozart"}},{{"id":"r11","name":"Franz Xaver Süssmayr"}}],"albumCount":3,"form":"Requiem","period":"Classical","starred":"2026-10-19T08:00:00Z","key":"D minor","movementCount":14,"duration":3300,"lyricists":[{{"id":"r12","name":"Thomas of Celano"}},{{"id":"","name":"Liturgy"}}],"languages":["lat","deu"]{requiem_plays}}}"#
            );
            let symphony_plays = composition_plays(7, 7);
            let symphony = format!(
                r#"{{"id":"w5","name":"Symphony No. 5","composers":[{{"id":"r9","name":"Ludwig van Beethoven"}}],"composed":{{"from":{{"year":1804}},"to":{{"year":1808}}}},"albumCount":2,"movementCount":4{symphony_plays}}}"#
            );
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
                (false, 0 | 1) => 226,
                (false, _) => 230,
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
        } else if request.contains("/rest/scrobble?") && SCROBBLE_DOWN.load(Relaxed) {
            if !write_of(request).ends_with("&submission=false") {
                PLAYS_TURNED_AWAY.fetch_add(1, Relaxed);
            }
            reply("503 Service Unavailable", "text/plain", b"down")
        } else if request.contains("/rest/scrobble?") {
            let told = write_of(request);
            match told.ends_with("&submission=false") {
                true => PLAYING_NOW.lock().unwrap().push(told),
                false => SCROBBLES.lock().unwrap().push(told),
            }
            json(OK)
        } else if request.contains("/rest/savePlayQueue") {
            SAVES.lock().unwrap().push(write_of(request));
            if SAVE_DOWN.swap(false, Relaxed) {
                reply("503 Service Unavailable", "text/plain", b"Restarting.")
            } else if SAVE_REFUSED.load(Relaxed) {
                json(
                    r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":50,"message":"Not allowed"}}}"#,
                )
            } else {
                json(OK)
            }
        } else if request.contains("/rest/getPlayQueue") && QUEUED.load(Relaxed) {
            let movement = |id: &str, name: &str, number: u8| {
                format!(
                    r#"{{"id":"{id}","isDir":false,"title":"Sonata: {name}","album":"Symphonies","albumId":"a2","coverArt":"al-a2","artistId":"r1","duration":300,"works":[{{"name":"Sonata"}}],"movements":[{{"name":"{name}","number":{number},"count":3}}]}}"#
                )
            };
            let entries = [
                r#"{"id":"c1","isDir":false,"title":"Symphony No. 5: I. Allegro con brio","album":"Symphonies","albumId":"a2","coverArt":"al-a2","duration":441}"#.to_owned(),
                movement("m1", "I", 1),
                movement("m2", "II", 2),
                movement("m3", "III", 3),
            ]
            .join(",");
            ok(&format!(
                r#""playQueue":{{"current":"m2","position":61500,"username":"joe","changed":"2024-01-01T00:00:00Z","changedBy":"test","entry":[{entries}]}}"#
            ))
        } else if request.contains("/rest/getCoverArt") {
            if let Some(banner) = id.filter(|id| id.starts_with("ba-")) {
                BANNERS_ASKED.lock().unwrap().push(banner.to_owned());
            }
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

    /// The banner behind a playlist made for the user at the top of its page.
    fn mix_banner(ui: &AppWindow) -> Option<ElementHandle> {
        ui.root_element()
            .query_descendants()
            .match_inherits("GenreBanner")
            .find_first()
    }

    /// Whether the page header shows a cover square.
    fn header_has_cover(ui: &AppWindow) -> bool {
        let header = ui
            .root_element()
            .query_descendants()
            .match_inherits("PageHeader")
            .find_first()
            .expect("the page header");
        header
            .query_descendants()
            .match_inherits("Artwork")
            .find_first()
            .is_some()
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

    /// How far down the window the text `label` is, while it is shown.
    fn text_top(ui: &AppWindow, label: &str) -> Option<f32> {
        let label = label.to_owned();
        ui.root_element()
            .query_descendants()
            .match_accessible_role(AccessibleRole::Text)
            .match_predicate(move |text| text.accessible_label().is_some_and(|l| l == label))
            .find_first()
            .map(|text| text.absolute_position().y)
    }

    /// A window tall enough for all of Home, whose queries skip what is
    /// outside the window.
    const ALL_OF_HOME: slint::LogicalSize = slint::LogicalSize::new(1100.0, 3000.0);

    /// A little longer than the stats' slides take to come to rest.
    const SLIDE_GLIDE: Duration = Duration::from_millis(400);

    /// A swipe for `drag`: its name, whether a finger makes it (else a mouse),
    /// its path and how long it rests before letting go.
    type Flick<'a> = (&'a str, bool, &'a [(u64, f32, f32)], u64);

    /// A scroll far enough up to reach the top of any page.
    const TOP_OF_PAGE: f32 = 100_000.0;

    /// Scrolls the page by `delta_y`, at once; up, to its top, when
    /// positive and large.
    fn scroll_page(ui: &AppWindow, delta_y: f32) {
        ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(600.0, 400.0),
            delta_x: 0.0,
            delta_y,
        });
    }

    /// Scrolls the page by `delta_y` at once, with the pointer `x` across the
    /// window.
    fn scroll_page_at(ui: &AppWindow, x: f32, delta_y: f32) {
        ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(x, 400.0),
            delta_x: 0.0,
            delta_y,
        });
    }

    /// Scrolls the page to its top and then down until the button with
    /// `label` is in view, or to its end, and says whether it came.
    async fn comes_into_view(ui: &AppWindow, label: &str) -> bool {
        scroll_page(ui, TOP_OF_PAGE);
        for _ in 0..40 {
            idle(Duration::from_millis(30)).await;
            if find(ui, label).is_some() {
                return true;
            }
            scroll_page(ui, -150.0);
        }
        false
    }

    /// Presses the page header's Download and says yes to the question what
    /// it takes.
    fn download_page(ui: &AppWindow) {
        click(ui, "Download");
        confirm_download(ui);
    }

    /// Presses the question's Download, the last button of that name, since
    /// the dialog is drawn over the page.
    fn confirm_download(ui: &AppWindow) {
        let ask = ui
            .root_element()
            .query_descendants()
            .match_accessible_role(AccessibleRole::Button)
            .match_predicate(|button| button.accessible_label().is_some_and(|l| l == "Download"))
            .find_all();
        ask.last()
            .expect("the question is up")
            .invoke_accessible_default_action();
    }

    fn click(ui: &AppWindow, label: &str) {
        button(ui, label).invoke_accessible_default_action();
    }

    /// Opens the page's Sort menu, moves its choice one entry along with
    /// `key`, as the arrow keys do, and closes it.
    async fn move_sort_choice(ui: &AppWindow, key: slint::platform::Key) {
        ui.root_element()
            .query_descendants()
            .match_accessible_role(AccessibleRole::Combobox)
            .match_predicate(|combo| combo.accessible_label().is_some_and(|l| l == "Sort by"))
            .find_first()
            .expect("no Sort menu")
            .single_click(PointerEventButton::Left)
            .await;
        press(ui, key);
        press(ui, slint::platform::Key::Return);
    }

    /// Plays what `pick` selects from the selection's menu: next, at the
    /// queue's end, then in place of the queue. Checks that each lets the
    /// selection go and puts `songs` in the queue there, in that order.
    async fn play_selection(ui: &AppWindow, pick: impl Fn(), songs: &[&str]) {
        let lone = |id: &str| -> opensubsonic::Child {
            serde_json::from_str(&format!(r#"{{"id":"{id}","isDir":false,"title":"{id}"}}"#))
                .unwrap()
        };
        let choose = |entry: &str| {
            pick();
            click(ui, "More for the selection");
            click(ui, entry);
            assert_eq!(ui.get_selected_tiles(), 0, "{entry} lets the selection go");
        };
        let queued = || play::testing::queued_ids().0;
        play::testing::clear(ui);
        play::testing::play_page(ui, vec![lone("x"), lone("y")], 0);
        choose("Play next");
        let next = [vec!["x"], songs.to_vec(), vec!["y"]].concat();
        wait_for("the selection next", || queued() == next).await;
        choose("Add to queue");
        let last = [next.clone(), songs.to_vec()].concat();
        wait_for("the selection at the end", || queued() == last).await;
        choose("Play now");
        wait_for("the selection alone", || queued() == songs).await;
        play::testing::clear(ui);
        ui.set_notice("".into());
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
        *lyrics::LRCLIB_ROOT.lock().unwrap() = format!("{}/api/", serve(lyrics_service));
        let statistics = serve(listenbrainz_statistics);
        *browse::LISTENBRAINZ_ROOTS.lock().unwrap() =
            (format!("{statistics}/1/"), format!("{statistics}/"));
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

            // Most played, chosen from the artists' Sort menu, starts with
            // the most played, is saved on this device, and asks for the
            // library again, which that order needs.
            click(&ui, "Artists");
            let refused = |call: &str| {
                let refused = ASKED_WHILE_DOWN.lock().unwrap();
                refused.iter().filter(|asked| asked.starts_with(call)).count()
            };
            wait_for("the failed artists", || refused("getArtists") > 0).await;
            let libraries_refused = refused("getAlbumList2");
            move_sort_choice(&ui, slint::platform::Key::DownArrow).await;
            assert_eq!((ui.get_artist_sort(), ui.get_artist_descending()), (1, true));
            let saved = crate::prefs::load();
            assert_eq!((saved.artist_sort, saved.artist_descending), (1, true));
            wait_for("the library asked for again", || {
                refused("getAlbumList2") > libraries_refused
            })
            .await;
            // Name, chosen again, goes A to Z.
            move_sort_choice(&ui, slint::platform::Key::UpArrow).await;
            assert_eq!((ui.get_artist_sort(), ui.get_artist_descending()), (0, false));
            let saved = crate::prefs::load();
            assert_eq!((saved.artist_sort, saved.artist_descending), (0, false));

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
                // Most played, ties by name either way, not in the server's
                // order, and Sonatas, which the server has no count of,
                // played 0 times.
                (4, true, &["b2", "b3", "b1"]),
                (4, false, &["b1", "b2", "b3"]),
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
            let saved_band = || prefs::load().sound.eq.bands.first().cloned();
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
            let pass = prefs::load().sound.eq.bands[1].clone();
            assert_eq!((pass.frequency, pass.gain), (40.0, 6.0));

            click(&ui, "Remove band 2 from the graph");
            assert_eq!(ui.get_eq_bands().row_count(), 1);
            let left_over = prefs::load().sound.eq.bands[0].clone();
            assert_eq!((left_over.filter, left_over.frequency), (0, 1_000.0), "the picked band went");
            assert_eq!(ui.get_eq_selected(), -1);
            assert_eq!(prefs::load().sound.eq.bands.len(), 1);
            // A band that goes from before the picked one leaves it picked.
            ui.invoke_add_band_at(0.2, 0.5);
            ui.invoke_add_band_at(0.7, 0.5);
            let before = prefs::load().sound.eq.bands;
            assert_eq!(ui.get_eq_selected(), 2);
            ui.invoke_remove_band(1);
            let after = prefs::load().sound.eq.bands;
            assert_eq!(after, [before[0].clone(), before[2].clone()]);
            assert_eq!(ui.get_eq_selected(), 1, "the picked band is still the one picked");
            ui.invoke_remove_band(1);
            assert_eq!((ui.get_eq_selected(), prefs::load().sound.eq.bands.len()), (-1, 1));
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
            assert_eq!(prefs::load().sound.eq.bands[0].frequency, 1_000.0, "saved once let go");
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
            assert_eq!(prefs::load().sound.eq.bands[0].frequency, 224.0);
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
            assert_eq!(prefs::load().sound.eq, expected);
            assert!(prefs::load().sound.eq_on);
            assert!(!expected.bands.is_empty());
            assert_eq!(ui.get_eq_bands().row_count(), expected.bands.len(), "its bands in rows");
            assert_eq!(ui.get_eq_points().row_count(), expected.bands.len(), "its points on the graph");
            assert_ne!(ui.get_eq_curve(), "");
            // Every setting is saved as a preset, which is linked to the sound
            // devices it is for: a device becoming the output takes its preset
            // up, one without plays with no processing, and what wasn't saved
            // goes either way. Listing the devices again changes nothing.
            let device = |id: &str, name: &str| {
                let pcm = id.trim_start_matches("alsa:");
                playback::OutputDevice {
                    id: id.into(),
                    key: id.into(),
                    name: name.into(),
                    detail: pcm.into(),
                    title: format!("{name} ({pcm})"),
                    kind: playback::OutputKind::Unknown,
                    same_as: Vec::new(),
                }
            };
            let (speakers, dac) = (device("alsa:hw:CARD=PCH,DEV=0", "Speakers"), device("alsa:hw:CARD=DAC,DEV=0", "USB DAC"));
            let list_devices = || {
                play::testing::show_devices(&ui, vec![speakers.clone(), dac.clone()], Some(speakers.id.clone()));
            };
            let no_processing = prefs::Sound::default();
            let headphone_sound = prefs::load().sound;
            scroll_page(&ui, TOP_OF_PAGE);
            idle(Duration::from_millis(100)).await;
            assert!(find_combo(&ui, "Sound preset"));
            assert!(ui.get_sound_unsaved(), "the headphone's EQ is in no preset yet");
            click(&ui, "Sound presets");
            click(&ui, "Save as new preset");
            type_into(&ui, "Name", "Headphones").await;
            click(&ui, "Save");
            assert_eq!(prefs::load().sound_preset.as_deref(), Some("Headphones"));
            assert_eq!((ui.get_sound_preset(), ui.get_sound_unsaved()), (1, false));
            let mut saved = prefs::load();
            saved.output_device = Some(dac.id.clone());
            prefs::save(&saved);
            list_devices();
            assert_eq!(prefs::load().sound, no_processing, "the DAC has no preset");
            assert_eq!((ui.get_sound_preset(), ui.get_eq_on()), (0, false));
            ui.invoke_choose_sound_preset(1);
            assert_eq!(prefs::load().sound, headphone_sound);
            click(&ui, "Sound presets");
            click(&ui, "Link to USB DAC (hw:CARD=DAC,DEV=0)");
            assert_eq!(ui.get_sound_links(), "Linked to USB DAC (hw:CARD=DAC,DEV=0).");
            ui.invoke_choose_crossfeed(2);
            assert!(ui.get_sound_unsaved());
            assert!(text_top(&ui, "Unsaved changes, lost when the sound device changes.").is_some());
            list_devices();
            assert_eq!(prefs::load().sound.crossfeed, 2, "the same device, listed again");
            ui.invoke_choose_output(0);
            assert_eq!(prefs::load().sound, no_processing, "the speakers have no preset");
            assert!(!ui.get_sound_unsaved());
            ui.invoke_choose_output(1);
            assert_eq!(prefs::load().sound, headphone_sound, "the DAC's preset, without what wasn't saved");
            assert_eq!(ui.get_sound_preset(), 1);
            // One preset for several devices, whose links follow a rename.
            click(&ui, "Sound presets");
            click(&ui, "Link a device");
            click(&ui, "Speakers (hw:CARD=PCH,DEV=0)");
            assert_eq!(ui.get_sound_links(), "Linked to USB DAC (hw:CARD=DAC,DEV=0) and Speakers (hw:CARD=PCH,DEV=0).");
            click(&ui, "Sound presets");
            click(&ui, "Rename preset");
            type_into(&ui, "Name", "Desk").await;
            click(&ui, "Rename");
            ui.invoke_choose_output(0);
            assert_eq!(prefs::load().sound_preset.as_deref(), Some("Desk"));
            assert_eq!(prefs::load().sound, headphone_sound);
            click(&ui, "Sound presets");
            click(&ui, "Unlink a device");
            click(&ui, "Speakers (hw:CARD=PCH,DEV=0)");
            assert_eq!(ui.get_sound_links(), "Linked to USB DAC (hw:CARD=DAC,DEV=0).");
            click(&ui, "Sound presets");
            click(&ui, "Delete preset");
            assert!(prefs::load().sound_presets.is_empty());
            assert!(prefs::load().device_presets.is_empty());
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Settings);
            click(&ui, "Library");
            assert_eq!(ui.get_page(), Page::Setting);
            // The server's scan: where it stands, one the server refuses,
            // and one it takes, followed while the page is up until done.
            wait_for("the scan status", || {
                ui.get_scan_status() == "226 items scanned."
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
                ui.get_scan_status() == "230 items scanned."
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
            assert_eq!((tile.title.as_str(), tile.link.as_str()), ("Wave", "station:shelf:0:0"));
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
            // One saved elsewhere since the saved stations were asked for
            // isn't saved twice: the server is asked.
            // Asked again after the save, which keeps the two.
            wait_for("the stations as they are", || {
                asked_after("createInternetRadioStation", "getInternetRadioStations")
            })
            .await;
            idle(Duration::from_millis(100)).await;
            let sent = radio_writes().len();
            WAVE_ELSEWHERE.store(true, Relaxed);
            ui.invoke_save_station(0, 0);
            wait_for("the station found", || ui.get_notice() == "Wave is saved already.").await;
            assert_eq!(radio_writes().len(), sent);
            wait_for("the stations anew", || ui.get_saved_stations().row_count() == 3).await;
            WAVE_ELSEWHERE.store(false, Relaxed);
            ui.invoke_save_station(4, 0);
            assert_eq!(ui.get_notice(), "Jazz is saved already.");
            // The curated catalogue is shelved by the device's country, then
            // genre, spoken ones last; a station the server has saved shows
            // as saved and is not saved twice.
            wait_for("the catalogue", || ui.get_catalogue_shelves().row_count() == 3).await;
            let curated = |row: usize| ui.get_catalogue_shelves().row_data(row).unwrap();
            let titles: Vec<String> = (0..3).map(|row| curated(row).title.into()).collect();
            assert_eq!(titles, ["Curated · in the Netherlands", "Curated · Jazz", "Curated · News"]);
            let saved_flags: Vec<bool> = curated(0).tiles.iter().map(|tile| tile.saved).collect();
            assert_eq!(saved_flags, [false, true]);
            ui.invoke_catalogue_shelf_in_view(0, 0, 1);
            wait_for("a catalogue logo", || curated(0).tiles.row_data(0).unwrap().cover.size().width > 0).await;
            ui.invoke_save_catalogue_station(0, 1);
            assert_eq!(ui.get_notice(), "Jazz is saved already.");
            ui.invoke_save_catalogue_station(0, 0);
            wait_for("a catalogue station saved", || ui.get_notice() == "Saved Canal.").await;
            assert_eq!(
                radio_writes().last().unwrap(),
                "createInternetRadioStation name=Canal&streamUrl=https%3A%2F%2Fcatalogue.example.org%2Fcanal&homepageUrl=https%3A%2F%2Fcanal.example.org%2F"
            );
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
            // A language picked has a shelf of the catalogue.
            let curated_titles = || -> Vec<String> {
                ui.get_catalogue_shelves().iter().map(|shelf| shelf.title.into()).collect()
            };
            assert_eq!(curated_titles()[1], "Curated · Dutch");
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
            wait_for("the artists counted", || {
                ui.get_stats().iter().any(|stat| stat.label == "Artists" && stat.value != "…")
            })
            .await;
            let saved = |row| ui.get_saved_stations().row_data(row).unwrap();
            assert_eq!(saved(0).title, "Jazz");
            assert_eq!(saved(0).names.row_data(0).unwrap().label, "radio.example.org");
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Radio);
            // News is found by name and told from another News by its stream.
            wait_for("Jazz's picture", || saved(0).cover.size().width > 0).await;
            wait_for("News's picture", || saved(1).cover.size().width > 0).await;
            // The device keeps the directory's picture, not the server's.
            let kept = crate::prefs::station_logos();
            assert_eq!(kept.len(), 1);
            assert_eq!(kept["http://news.example.org/live.mp3"].bytes().unwrap(), PNG);
            // A tile plays its station in place of a queue: live, with no
            // length or other tracks, and nothing of it is saved as the
            // server's queue or scrobbled.
            assert_eq!(saved(0).link, "station:saved:0");
            assert_eq!(ui.get_station_shelves().row_data(0).unwrap().tiles.row_data(0).unwrap().link, "station:shelf:0:0");
            let saves_before = SAVES.lock().unwrap().len();
            let looks_before = play::testing::scrobbler_calls().0;
            ui.invoke_play_station(saved(0).link);
            wait_for("the station on the bar", || ui.get_playing_live()).await;
            assert!(ui.get_playing());
            assert_eq!(ui.get_playing_title(), "Jazz");
            assert_eq!(ui.get_playing_names().row_data(0).unwrap().label, "Live radio");
            assert_eq!(button(&ui, "Live radio").accessible_enabled(), Some(false), "its chip opens no page");
            assert_eq!(ui.get_playing_duration(), 0.0);
            assert_eq!(ui.get_playing_album().link, "");
            assert_eq!(
                play::testing::tuned(),
                Some(("Jazz".to_owned(), "https://stream.example.org/jazz".to_owned()))
            );
            assert!(play::testing::queued_ids().0.is_empty());
            wait_for("the scrobbler to look", || play::testing::scrobbler_calls().0 > looks_before + 1).await;
            assert_eq!(SAVES.lock().unwrap().len(), saves_before, "no queue saved");
            // A directory station plays from its resolved stream.
            ui.invoke_play_station("station:shelf:0:0".into());
            wait_for("Wave", || ui.get_playing_title() == "Wave").await;
            assert_eq!(
                play::testing::tuned().map(|(_, stream)| stream),
                Some("https://s1.example.org/listen".to_owned())
            );
            // So does a station of the curated catalogue, by its stream.
            let curated_tile = curated(0).tiles.row_data(0).unwrap();
            assert_eq!(curated_tile.link, "station:catalogue:0:0");
            ui.invoke_play_station(curated_tile.link);
            wait_for("a catalogue station", || ui.get_playing_title() == curated_tile.title).await;
            assert!(play::testing::tuned().is_some_and(|(_, stream)| stream.starts_with("https://")));
            // A station has no words, so a song's go when one takes over.
            play::testing::show_song(&ui, vec![serde_json::from_str(HARBOUR_LIGHTS).unwrap()], 0);
            wait_for("the song's words", || ui.get_playing_lyrics() != "").await;
            ui.invoke_play_station(saved(0).link);
            wait_for("the station once more", || ui.get_playing_live()).await;
            assert_eq!(ui.get_playing_lyrics(), "", "a station has no words");
            // A song taken up after it puts the song back on the bar.
            let queued = opensubsonic::Child {
                id: "r1".into(),
                title: "Song".into(),
                ..Default::default()
            };
            play::testing::show_song(&ui, vec![queued], 0);
            assert!(!ui.get_playing_live());
            play::testing::clear(&ui);
            assert_eq!(play::testing::tuned(), None);
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

            // The artists arrive at once, kept since the home page asked for
            // them, with the server down too. They show a page at a time, in
            // the server's order.
            DOWN.store(true, Relaxed);
            click(&ui, "Artists");
            assert_eq!(ui.get_page(), Page::Artists);
            wait_for("the artists", || ui.get_tiles().row_count() == 100).await;
            assert_eq!(ui.get_page_problem(), "");
            DOWN.store(false, Relaxed);
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
            // The selection's menu plays the songs of an artist's own
            // albums, not those of a compilation they are on.
            filter_artists("Other 120");
            wait_for("Other 120", || links(&ui) == ["artist:o120"]).await;
            play_selection(&ui, || ui.invoke_select_tile(0, false), &["s1", "s2"]).await;
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

            // The top bar's Search opens the search with the keys in its
            // field. Typing asks the server and the radio directory once,
            // when it pauses, for the artists, albums and tracks found and
            // the directory's stations, which play.
            click(&ui, "Search");
            assert_eq!(ui.get_page(), Page::Search);
            assert_eq!(ui.get_bar_section(), -1, "the search lights no section");
            // The field is there once the page is drawn, after its changes.
            idle(Duration::from_millis(50)).await;
            ui.root_element().query_descendants().find_all();
            let asked = || GLOBAL_SEARCHES.lock().unwrap().clone();
            let asked_directory = || STATION_SEARCHES.lock().unwrap().clone();
            for key in "Mara".chars() {
                press(&ui, key.to_string());
            }
            assert_eq!(ui.get_search_query(), "Mara", "the field has the keys");
            idle(Duration::from_millis(100)).await;
            assert!(asked().is_empty(), "nothing asked before the pause");
            wait_for("Mara FM", || ui.get_found_radio().tiles.row_count() == 1).await;
            assert_eq!(asked(), ["Mara"]);
            assert_eq!(asked_directory(), ["Mara"]);
            assert_eq!(ui.get_page_problem(), "");
            let link = |tiles: slint::ModelRc<AlbumTile>| tiles.row_data(0).unwrap().link.to_string();
            assert_eq!(link(ui.get_found_artists()), "artist:r1");
            assert_eq!(link(ui.get_found_albums()), "album:a1");
            assert_eq!(ui.get_tracks().row_data(0).unwrap().title, "Harbour Lights");
            assert!(ui.get_search_answered());
            assert!(!ui.get_loading());
            let station = || ui.get_found_radio().tiles.row_data(0).unwrap();
            assert_eq!(station().title, "Mara FM");
            wait_for("Mara FM's logo", || station().cover.size().width > 0).await;
            ui.invoke_play_station(station().link);
            wait_for("Mara FM playing", || ui.get_playing_title() == "Mara FM").await;
            assert!(ui.get_playing_cover().size().width > 0, "its logo plays along");
            play::testing::clear(&ui);
            // Enter searches at once, without the pause; the saved stations
            // are found by name.
            for _ in 0..4 {
                press(&ui, slint::platform::Key::Backspace);
            }
            assert_eq!(ui.get_search_query(), "", "the field still has the keys");
            for key in "Jazz".chars() {
                press(&ui, key.to_string());
            }
            press(&ui, slint::platform::Key::Return);
            assert!(ui.get_loading(), "Enter asks at once");
            assert_eq!(link(ui.get_found_stations()), "station:saved:0");
            wait_for("Jazz FM", || ui.get_found_radio().tiles.row_data(0).unwrap().title == "Jazz FM").await;
            // In the row Mara FM had, it has its own logo.
            wait_for("Jazz FM's logo", || station().cover.size().width > 0).await;
            assert_eq!(ui.get_found_albums().row_count(), 0);
            idle(Duration::from_millis(400)).await;
            assert_eq!(asked(), ["Mara", "Jazz"], "asked once each");
            // On Fonoteca the composers and compositions are found by name.
            let search_for = |query: &str| {
                ui.set_search_query(query.into());
                ui.invoke_search_now();
            };
            search_for(" Ludwig ");
            wait_for("Beethoven", || ui.get_found_composers().row_count() == 1).await;
            assert_eq!(link(ui.get_found_composers()), "composer:r9");
            // The spaces around what is typed are left out.
            wait_for("Ludwig answered", || ui.get_search_answered()).await;
            assert_eq!(asked().last().map(String::as_str), Some("Ludwig"));
            assert_eq!(asked_directory().last().map(String::as_str), Some("Ludwig"));
            search_for("symphony");
            wait_for("Symphony No. 5", || ui.get_found_works().row_count() == 1).await;
            assert_eq!(ui.get_found_works().row_data(0).unwrap().link, "composition:w5");
            wait_for("all of it answered", || ui.get_search_answered()).await;
            // The answer to a search a later one took over from never shows.
            search_for("slow");
            wait_for("the slow search", || asked().last().is_some_and(|q| q == "slow")).await;
            search_for("harbour");
            wait_for("Harbour's album", || {
                let albums: Vec<String> = ui.get_found_albums().iter().map(|tile| tile.link.into()).collect();
                assert!(!albums.contains(&"album:a9".to_owned()), "the slow answer showed");
                albums == ["album:a1"]
            })
            .await;
            wait_for("Harbour answered", || ui.get_search_answered()).await;
            assert_eq!(asked_directory().last().map(String::as_str), Some("harbour"));
            assert!(!asked_directory().contains(&"slow".to_owned()));
            // An album found opens its page; back on the search, what it
            // found is there at once, and a track found plays as an album's.
            // The window is tall enough for the albums' shelf.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 1500.0));
            click(&ui, "Night Harbour");
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            assert_eq!(ui.get_page(), Page::Album);
            wait_for("its tracks", || ui.get_tracks().row_count() > 0).await;
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Search);
            assert_eq!(ui.get_search_query(), "harbour");
            assert_eq!(ui.get_found_albums().row_count(), 1);
            assert_eq!(ui.get_tracks().row_data(0).unwrap().title, "Harbour Lights");
            ui.invoke_play_tracks(0);
            wait_for("Harbour Lights playing", || ui.get_playing_title() == "Harbour Lights").await;
            assert_eq!(play::testing::queued_ids().0, ["t1"]);
            play::testing::clear(&ui);
            wait_for("searched again", || ui.get_search_answered()).await;
            // Escape closes the search, from the page as from its field.
            idle(Duration::from_millis(50)).await;
            ui.root_element().query_descendants().find_all();
            press(&ui, slint::platform::Key::Escape);
            assert_eq!(ui.get_page(), Page::Albums);
            // The field keeps the keys, a space too, which elsewhere scrolls
            // the page, also when it is drawn before the page takes in that
            // it arrived.
            ui.set_search_query("".into());
            click(&ui, "Search");
            ui.root_element().query_descendants().find_all();
            idle(Duration::from_millis(50)).await;
            for key in "a b".chars() {
                press(&ui, key.to_string());
            }
            assert_eq!(ui.get_search_query(), "a b", "the field kept the keys");
            press(&ui, slint::platform::Key::Escape);
            assert_eq!(ui.get_page(), Page::Albums, "Escape in the field");

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
            assert_eq!(ui.get_sections().row_count(), 11);
            DOWN.store(true, Relaxed);
            click(&ui, "Playlists");
            wait_for("the failed playlists", || ui.get_page_problem() != "").await;
            assert_eq!(ui.get_page(), Page::Playlists);
            assert_eq!(ui.get_section(), 10);
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
                    "History",
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
            assert_eq!(pictures.len(), 13);
            assert_eq!(pictures[0], icons.get_home().as_str());
            assert_eq!(pictures[1], icons.get_genre().as_str());
            assert_eq!(pictures[2], icons.get_radio().as_str());
            assert_eq!(pictures[3], icons.get_history().as_str());
            let library = [
                icons.get_album(),
                icons.get_artist(),
                icons.get_note(),
                icons.get_composer(),
                icons.get_composition(),
                icons.get_folder(),
            ];
            assert_eq!(pictures[4..10], library.map(String::from));
            assert_eq!(pictures[10..13], ["", "", ""]);
            // The search finds them by name.
            ui.set_search_query("road".into());
            ui.invoke_open("search".into());
            let found = ui.get_found_playlists().row_data(0).unwrap();
            assert_eq!((found.title.as_str(), found.link.as_str()), ("Road Trip", "playlist:p1"));
            ui.invoke_back();
            ui.invoke_show_section(11);
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Playlist);
            assert_eq!(ui.get_section(), 11);
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
            ui.invoke_select_track(0, false);
            ui.invoke_select_track(1, false);
            assert_eq!(ui.get_selected_tiles(), 2);
            let playlists_asked = times_asked("getPlaylist");
            ui.invoke_remove_from_playlist(0);
            assert_eq!(ui.get_selected_tiles(), 1, "the song taken off is no longer picked");
            assert_eq!(ui.get_tracks().row_count(), 1);
            assert_eq!(ui.get_tracks().row_data(0).unwrap().title, "Fugue");
            assert!(ui.get_tracks().row_data(0).unwrap().selected);
            ui.invoke_clear_selection();
            wait_for("the track taken off", || {
                writes() == ["updatePlaylist playlistId=p1&songIndexToRemove=0"]
            })
            .await;
            wait_for("the playlist again", || ui.get_tracks().row_count() == 2).await;
            assert!(times_asked("getPlaylist") > playlists_asked, "not the one kept from before");
            // Changed elsewhere within the hour, the playlist shows as kept,
            // and a track taken off there by its place is looked for in the
            // server's first: another song is there now, so none is taken off.
            ui.invoke_open("playlists".into());
            ROAD_TRIP_EDITED.store(true, Relaxed);
            let playlists_asked = times_asked("getPlaylist");
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist kept", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(times_asked("getPlaylist"), playlists_asked, "kept within the hour");
            assert_eq!(ui.get_tracks().row_data(1).unwrap().title, "Fugue");
            PLAYLIST_WRITES.lock().unwrap().clear();
            ui.invoke_remove_from_playlist(1);
            assert_eq!(ui.get_tracks().row_count(), 1);
            wait_for("the playlist as the server has it", || {
                (ui.get_tracks().row_data(1)).is_some_and(|track| track.title == "Rope and Anchor")
            })
            .await;
            assert!(writes().is_empty(), "nothing taken off");
            assert_eq!(
                ui.get_notice(),
                "The playlist changed elsewhere, so nothing was taken off."
            );
            ui.set_notice("".into());
            // Changed back elsewhere, its songs go in a playlist as the
            // server has them, not as kept.
            ROAD_TRIP_EDITED.store(false, Relaxed);
            ui.invoke_open("playlists".into());
            wait_for("the playlists", || ui.get_tiles().row_count() == 2).await;
            PLAYLIST_WRITES.lock().unwrap().clear();
            ui.invoke_select_tile(0, false);
            ui.invoke_add_selection_to_playlist("p1".into());
            wait_for("the playlist's songs added", || !writes().is_empty()).await;
            assert_eq!(writes(), ["updatePlaylist playlistId=p1&songIdToAdd=s1&songIdToAdd=s9"]);
            wait_for("the notice", || ui.get_notice() != "").await;
            ui.set_notice("".into());
            // The selection's menu plays a playlist's songs.
            play_selection(&ui, || ui.invoke_select_tile(0, false), &["s1", "s9"]).await;
            ui.invoke_open("playlist:p1".into());
            wait_for("Road Trip as the server has it", || {
                (ui.get_tracks().row_data(1)).is_some_and(|track| track.title == "Fugue")
            })
            .await;
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
            // The selection's menu plays the albums picked in the page's
            // order, whichever was picked first. Of the albums matching
            // "Other 2", only the first, a2, has songs of its own.
            let filter_albums = |typed: &str| {
                ui.set_album_filter(typed.into());
                ui.invoke_filter_albums();
            };
            let tile_link = |row: usize| ui.get_tiles().row_data(row).map(|tile| tile.link);
            filter_albums("Other 2");
            wait_for("a2 first", || tile_link(0).is_some_and(|link| link == "album:a2")).await;
            assert_eq!(tile_link(1).unwrap(), "album:a20");
            let pick_albums = || {
                ui.invoke_select_tile(1, false);
                ui.invoke_select_tile(0, false);
            };
            let albums = ["c1", "c2", "c3", "c4", "c6", "c7", "c5", "s1", "s2"];
            play_selection(&ui, pick_albums, &albums).await;
            // Albums deleted on the server since are left out, the album
            // after them plays all the same, and the notice counts them.
            let album_id = |row: usize| tile_link(row).unwrap().replace("album:", "");
            GONE_ALBUMS.lock().unwrap().extend([album_id(2), album_id(3)]);
            (2..5).for_each(|row| ui.invoke_select_tile(row, false));
            click(&ui, "More for the selection");
            click(&ui, "Play now");
            wait_for("the album after them", || play::testing::queued_ids().0 == ["s1", "s2"]).await;
            assert_eq!(
                ui.get_notice(),
                "The songs of 2 items couldn't be fetched: server error 70: Album not found"
            );
            GONE_ALBUMS.lock().unwrap().clear();
            play::testing::clear(&ui);
            ui.set_notice("".into());
            filter_albums("");
            wait_for("all albums", || ui.get_tiles().row_count() == 100).await;
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

            let albums = times_asked("getAlbum");
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

            let artists = times_asked("getArtist");
            click(&ui, "Back");
            wait_for("the album again", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Album);
            assert!(ui.get_can_go_forward());
            // The star outdated the album kept: it is asked for again, and
            // shows the song loved.
            assert_eq!(times_asked("getAlbum"), albums + 1);
            assert!(ui.get_tracks().row_data(1).unwrap().loved);
            click(&ui, "Forward");
            wait_for("the artist again", || ui.get_tiles().row_count() == 1).await;
            assert_eq!(ui.get_page(), Page::Artist);
            assert!(!ui.get_can_go_forward());
            ui.invoke_forward();
            assert_eq!(ui.get_page(), Page::Artist);

            // A page opened from back in the history drops the pages ahead.
            click(&ui, "Back");
            wait_for("the album once more", || ui.get_tracks().row_count() == 2).await;
            // Within the hour, pages seen before show what is kept, asking
            // nothing.
            assert_eq!(times_asked("getAlbum"), albums + 1);
            assert_eq!(times_asked("getArtist"), artists);
            // After it, they show it at once all the same, and what the
            // server says takes its place when it comes, never blank.
            let an_hour = opensubsonic::FRESH_FOR;
            answers::LATER.store(an_hour, Relaxed);
            ALBUM_RENAMED.store(true, Relaxed);
            SLOW_ALBUM.store(true, Relaxed);
            let only_tile = |title: &str| {
                let tiles = ui.get_tiles();
                tiles.row_count() == 1 && tiles.row_data(0).unwrap().title == title
            };
            click(&ui, "Forward");
            wait_for("the artist as it is now", || only_tile("Night Harbour (Remastered)")).await;
            click(&ui, "Back");
            wait_for("the aged album", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_header_title(), "Night Harbour");
            assert!(!ui.get_loading());
            wait_for("the album as it is now", || {
                ui.get_header_title() == "Night Harbour (Remastered)"
            })
            .await;
            wait_for("its notes as they are now", || ui.get_about().starts_with("Remastered")).await;
            assert_eq!(ui.get_tracks().row_count(), 2);
            assert_eq!(times_asked("getAlbum"), albums + 2);
            // What the server says once its page was left stays off the page
            // on screen then.
            answers::LATER.store(2 * an_hour, Relaxed);
            ALBUM_RENAMED.store(false, Relaxed);
            click(&ui, "Forward");
            wait_for("the artist as it was", || only_tile("Night Harbour")).await;
            click(&ui, "Back");
            wait_for("the album aged again", || {
                ui.get_header_title() == "Night Harbour (Remastered)"
            })
            .await;
            click(&ui, "Mara Lind");
            wait_for("the artist", || ui.get_page() == Page::Artist).await;
            idle(Duration::from_millis(700)).await;
            assert_eq!(ui.get_header_title(), "Mara Lind");
            assert!(only_tile("Night Harbour"));
            SLOW_ALBUM.store(false, Relaxed);
            answers::LATER.store(0, Relaxed);
            // Asked for anew, the home page's newest albums and saved
            // stations show what was kept at once too, and then what the
            // server says.
            click(&ui, "Home");
            let counted = |label: &str| {
                (ui.get_stats().iter()).any(|stat| stat.label == label && stat.value != "…")
            };
            let named = |title: &str| ui.get_tiles().iter().any(|tile| tile.title == title);
            let station = || ui.get_saved_stations().row_data(0).map(|row| row.title);
            ui.invoke_refresh();
            wait_for("the home page anew", || {
                counted("Albums") && named("Night Harbour") && station().is_some()
            })
            .await;
            ALBUM_RENAMED.store(true, Relaxed);
            ui.invoke_refresh();
            wait_for("the home page as it is now", || {
                named("Night Harbour (Remastered)") && station().as_deref() == Some("Jazz FM")
            })
            .await;
            ALBUM_RENAMED.store(false, Relaxed);
            ui.invoke_refresh();
            wait_for("the home page as it was", || {
                named("Night Harbour") && station().as_deref() == Some("Jazz")
            })
            .await;
            ui.invoke_back();
            ui.invoke_back();
            wait_for("the album, back past the artist", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_page(), Page::Album);
            let lists = LISTS.load(Relaxed);
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
            // What was kept shows at once, and gives way to what the server
            // says once that has all come.
            ALBUM_RENAMED.store(true, Relaxed);
            pull(middle(&page), 100.0);
            wait_for("the library anew", || {
                LISTS.load(Relaxed) == lists + 2 && ui.get_tiles().row_count() == 100
            })
            .await;
            wait_for("the library as it is now", || named("Night Harbour (Remastered)")).await;
            assert_eq!(ui.get_tiles().row_count(), 100);
            ALBUM_RENAMED.store(false, Relaxed);
            ui.invoke_refresh();
            wait_for("the library as it was", || named("Night Harbour")).await;
            assert_eq!(ui.get_page(), Page::Albums);
            // Asked again and saying nothing new, the server leaves the
            // albums shown as they are, not drawn anew.
            SLOW_LISTS.store(true, Relaxed);
            let lists = LISTS.load(Relaxed);
            ui.invoke_refresh();
            idle(Duration::from_millis(100)).await;
            let shown = ui.get_tiles();
            assert_eq!(shown.row_count(), 100);
            wait_for("the library asked again", || LISTS.load(Relaxed) == lists + 2).await;
            idle(Duration::from_millis(300)).await;
            assert!(ui.get_tiles() == shown, "drawn anew");
            SLOW_LISTS.store(false, Relaxed);
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
                // So does an Android back that nothing with the keys took.
                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;
                back::pressed(&ui, true);
                assert!(!ui.get_now_playing_open(), "back closes it first");
                assert_eq!(ui.get_page(), Page::Albums);
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
                // The sound device is chosen from a desktop's bar, and from the
                // page of a tablet and a phone: the one in use is marked, the
                // choice is kept as the settings once kept it, and opening the
                // menu lists the devices again. There is no system default to
                // choose: with none chosen, the system's own device is, by name.
                // Each device shows its kind's icon, and the button the one in
                // use's; a screen reader says the kind before the name.
                let speakers = "alsa:hw:CARD=PCH,DEV=0";
                let dac = "alsa:hw:CARD=DAC,DEV=0";
                let merged_away = "alsa:plughw:CARD=DAC,DEV=0";
                let fake_devices_beside = |system: Option<&str>| {
                    play::testing::show_devices(
                        &ui,
                        vec![
                            playback::OutputDevice {
                                id: speakers.into(),
                                key: speakers.into(),
                                name: "Speakers".into(),
                                detail: "hw:CARD=PCH,DEV=0".into(),
                                title: "Speakers (hw:CARD=PCH,DEV=0)".into(),
                                kind: playback::OutputKind::Speaker,
                                same_as: Vec::new(),
                            },
                            playback::OutputDevice {
                                id: dac.into(),
                                key: dac.into(),
                                name: "USB DAC".into(),
                                detail: "hw:CARD=DAC,DEV=0".into(),
                                title: "USB DAC (hw:CARD=DAC,DEV=0)".into(),
                                kind: playback::OutputKind::Usb,
                                same_as: vec![merged_away.into()],
                            },
                        ],
                        system.map(str::to_owned),
                    );
                };
                let fake_devices = || fake_devices_beside(Some(speakers));
                let choose_saved = |device: Option<&str>| {
                    let mut saved = crate::prefs::load();
                    saved.output_device = device.map(str::to_owned);
                    crate::prefs::save(&saved);
                };
                let icons = ui.global::<Icons>();
                let marked = || -> Vec<bool> { ui.get_output_devices().iter().map(|entry| entry.checked).collect() };
                let named_in_use = |name: &str| {
                    ui.root_element()
                        .query_descendants()
                        .match_accessible_role(AccessibleRole::Text)
                        .match_predicate({
                            let name = name.to_owned();
                            move |text| text.accessible_label().is_some_and(|label| label == name)
                        })
                        .find_first()
                        .is_some()
                };
                let listed = || -> Vec<String> {
                    ui.get_output_devices().iter().map(|entry| entry.label.into()).collect()
                };
                for (width, height) in [(1100.0, 760.0), (700.0, 760.0), (400.0, 760.0)] {
                    ui.window().set_size(slint::LogicalSize::new(width, height));
                    idle(Duration::from_millis(400)).await;
                    choose_saved(None);
                    fake_devices();
                    assert_eq!(listed(), ["Speakers", "USB DAC"], "{width}px");
                    assert_eq!(marked(), [true, false], "{width}px");
                    let kind_icons: Vec<_> = ui.get_output_devices().iter().map(|entry| entry.icon).collect();
                    assert_eq!(kind_icons, [icons.get_loudspeaker(), icons.get_usb()], "{width}px");
                    assert_eq!(ui.get_output_icon(), icons.get_loudspeaker(), "{width}px");
                    assert_eq!(ui.get_output_name(), "Speakers (hw:CARD=PCH,DEV=0)", "{width}px");
                    assert_eq!(crate::prefs::load().output_device.as_deref(), Some(speakers), "{width}px");
                    // The bar names the device only from 1400px, the page always.
                    let on_the_bar = width == 1100.0;
                    if on_the_bar {
                        let picker = button(&ui, "Sound device");
                        let (top, bottom) = (picker.absolute_position().y, picker.absolute_position().y + picker.size().height);
                        let player_bar = 80.0;
                        assert!(top >= height - player_bar && bottom <= height, "in the bar, not at {top}");
                    } else {
                        // Under the cover and the controls.
                        ui.window().dispatch_event(WindowEvent::PointerScrolled {
                            position: slint::LogicalPosition::new(width / 2.0, 300.0),
                            delta_x: 0.0,
                            delta_y: -100000.0,
                        });
                        idle(Duration::from_millis(100)).await;
                        assert!(named_in_use("Speaker: Speakers (hw:CARD=PCH,DEV=0)"), "{width}px");
                    }
                    click(&ui, "Sound device");
                    wait_for("the devices listed again", || !listed().contains(&"USB DAC".to_owned())).await;
                    fake_devices();
                    click(&ui, "USB: USB DAC, hw:CARD=DAC,DEV=0");
                    assert_eq!(crate::prefs::load().output_device.as_deref(), Some(dac), "{width}px");
                    assert_eq!(marked(), [false, true], "{width}px");
                    assert_eq!(ui.get_output_name(), "USB DAC (hw:CARD=DAC,DEV=0)", "{width}px");
                    assert_eq!(ui.get_output_icon(), icons.get_usb(), "the button follows the choice at {width}px");
                    if !on_the_bar {
                        assert!(named_in_use("USB: USB DAC (hw:CARD=DAC,DEV=0)"), "{width}px");
                    }
                    // Listed again at start-up or on opening, the one saved is the one marked.
                    fake_devices();
                    assert_eq!(marked(), [false, true], "{width}px");
                    click(&ui, "Sound device");
                    click(&ui, "Speaker: Speakers, hw:CARD=PCH,DEV=0");
                    assert_eq!(crate::prefs::load().output_device.as_deref(), Some(speakers), "{width}px");
                    assert_eq!(marked(), [true, false], "{width}px");
                    wait_for("the devices listed again", || !listed().contains(&"USB DAC".to_owned())).await;
                }
                // A device on the network is listed beside the sound devices
                // with its own icon and says what does not apply to it. Played
                // to, it is the one marked, but never the choice kept: the
                // sound device chosen before stays the one saved.
                choose_saved(Some(speakers));
                let kitchen = playback::OutputDevice {
                    id: "network:cast:9f8e".into(),
                    key: "network:cast:9f8e".into(),
                    name: "Kitchen".into(),
                    detail: "No sound processing, leveling or bit-perfect".into(),
                    title: "Kitchen".into(),
                    kind: playback::OutputKind::Network(playback::NetworkProtocol::Cast),
                    same_as: Vec::new(),
                };
                let list_with_kitchen = || {
                    play::testing::show_devices(
                        &ui,
                        vec![
                            playback::OutputDevice {
                                id: speakers.into(),
                                key: speakers.into(),
                                name: "Speakers".into(),
                                detail: String::new(),
                                title: "Speakers".into(),
                                kind: playback::OutputKind::Speaker,
                                same_as: Vec::new(),
                            },
                            kitchen.clone(),
                        ],
                        Some(speakers.to_owned()),
                    );
                };
                list_with_kitchen();
                assert_eq!(listed(), ["Speakers", "Kitchen"]);
                assert_eq!(ui.get_output_devices().row_data(1).unwrap().icon, icons.get_cast());
                assert_eq!(marked(), [true, false]);
                play::testing::find_network_device(network_output::Device {
                    protocol: network_output::Protocol::Cast,
                    id: "9f8e".into(),
                    name: "Kitchen".into(),
                    model: None,
                    address: std::net::Ipv4Addr::LOCALHOST,
                    port: 8009,
                });
                ui.invoke_choose_output(1);
                assert_eq!(marked(), [false, true]);
                assert_eq!(ui.get_output_icon(), icons.get_cast());
                assert_eq!(crate::prefs::load().output_device.as_deref(), Some(speakers));
                list_with_kitchen();
                assert_eq!(marked(), [false, true], "listed again, the one played to stays marked");
                ui.invoke_choose_output(0);
                assert_eq!(marked(), [true, false]);
                assert_eq!(ui.get_output_icon(), icons.get_loudspeaker());
                // A choice saved as a route that was folded into another is
                // that other.
                choose_saved(Some(merged_away));
                fake_devices();
                assert_eq!(marked(), [false, true]);
                assert_eq!(ui.get_output_name(), "USB DAC (hw:CARD=DAC,DEV=0)");
                assert_eq!(crate::prefs::load().output_device.as_deref(), Some(merged_away));
                // One saved that isn't there now gives way to the system's.
                choose_saved(Some("alsa:hw:CARD=Gone,DEV=0"));
                fake_devices();
                assert_eq!(marked(), [true, false]);
                assert_eq!(crate::prefs::load().output_device.as_deref(), Some(speakers));
                // A system's not listed is no choice, nor is another in its place.
                choose_saved(None);
                fake_devices_beside(Some("alsa:default"));
                assert_eq!(marked(), [false, false]);
                assert_eq!(ui.get_output_name(), "");
                assert_eq!(ui.get_output_icon(), icons.get_speaker(), "none in use is a plain sound device");
                assert_eq!(crate::prefs::load().output_device, None);
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

                // A desktop's bar spans what the sidebar leaves of the window.
                let bar = button(&ui, "Now playing");
                let (bar_at, bar_size) = (bar.absolute_position(), bar.size());
                assert_eq!(bar_size.height, 80.0);
                assert_eq!(bar_at.y + bar_size.height, 760.0, "the bar ends the window");
                assert_eq!(bar_at.x + bar_size.width, 1100.0, "the bar spans the window");
                assert!(bar_size.width > 600.0);
                // A tablet's opens it wherever it is clicked but on its
                // controls, and the sheet covers it. Its artist line is text.
                ui.window().set_size(slint::LogicalSize::new(700.0, 760.0));
                idle(Duration::from_millis(400)).await;
                let bar = button(&ui, "Now playing");
                let (bar_at, bar_size) = (bar.absolute_position(), bar.size());
                assert_eq!(bar_at.x + bar_size.width, 700.0, "the bar spans the window");
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
                    click(&ui, "Close now playing");
                    idle(Duration::from_millis(400)).await;
                }
                // A desktop's stays in sight under the page, and a second
                // click closes it: here beside its cover.
                ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
                idle(Duration::from_millis(400)).await;
                let cover = button(&ui, "Album cover");
                let (cover_at, cover_size) = (cover.absolute_position(), cover.size());
                let beside_cover = slint::LogicalPosition::new(cover_at.x + cover_size.width + 6.0, cover_at.y + cover_size.height / 2.0);
                tap(beside_cover).await;
                assert!(ui.get_now_playing_open(), "a click on the bar opens it");
                idle(Duration::from_millis(400)).await;
                tap(beside_cover).await;
                assert!(!ui.get_now_playing_open(), "a second click closes it");
                idle(Duration::from_millis(400)).await;
                input.set_finger(finger);

                ui.window().set_size(slint::LogicalSize::new(800.0, 760.0));
                idle(Duration::from_millis(400)).await;
                click(&ui, "Now playing");
                idle(Duration::from_millis(400)).await;

                // A finger's swipe up past the controls of a tablet's or a
                // phone's page opens the queue, when it is long enough, and a
                // swipe down in the queue does not go back. Beside the page's
                // column nothing but the page takes the finger.
                assert!(!ui.get_queue_open(), "Now playing opens on what plays");
                let sheet_middle = slint::LogicalPosition::new(750.0, 400.0);
                let scroll_page = |by: f32| {
                    ui.window().dispatch_event(WindowEvent::PointerScrolled {
                        position: sheet_middle,
                        delta_x: 0.0,
                        delta_y: by,
                    });
                };
                scroll_page(-100000.0);
                idle(Duration::from_millis(100)).await;
                input.set_finger(true);
                drag(&ui, sheet_middle, &[(0, 0.0, -20.0), (0, 0.0, -40.0)], 100).await;
                assert!(!ui.get_queue_open(), "not far enough");
                drag(&ui, sheet_middle, &[(0, 0.0, -20.0), (0, 0.0, -70.0), (0, 0.0, -120.0)], 100).await;
                assert!(ui.get_queue_open(), "swiped up past the controls");
                let menu = format!("More for {}", ui.get_playing_title());
                assert!(find(&ui, &menu).is_none(), "the queue has no menu of the song playing");
                drag(&ui, sheet_middle, &[(0, 0.0, 20.0), (0, 0.0, 70.0), (0, 0.0, 120.0)], 100).await;
                assert!(ui.get_queue_open(), "a swipe down is the queue's to scroll");
                click(&ui, "Back to now playing");
                idle(Duration::from_millis(100)).await;
                assert!(!ui.get_queue_open());
                input.set_finger(false);
                scroll_page(-100000.0);
                idle(Duration::from_millis(100)).await;
                assert!(find(&ui, &menu).is_some(), "Now playing has it");
                scroll_page(100000.0);
                idle(Duration::from_millis(100)).await;
                input.set_finger(true);
                // Mostly sideways is no swipe up, and neither is one that
                // starts away from the page's end, however far it goes.
                drag(&ui, sheet_middle, &[(0, -100.0, -20.0), (0, -200.0, -60.0), (0, -300.0, -110.0)], 100).await;
                assert!(!ui.get_queue_open(), "a sideways swipe");
                ui.window().set_size(slint::LogicalSize::new(360.0, 500.0));
                idle(Duration::from_millis(400)).await;
                drag(&ui, slint::LogicalPosition::new(180.0, 300.0), &[(0, 0.0, -20.0), (0, 0.0, -120.0), (0, 0.0, -2000.0)], 100).await;
                assert!(!ui.get_queue_open(), "a swipe up from mid-page scrolls it");
                ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
                idle(Duration::from_millis(400)).await;
                input.set_finger(finger);

                // The seek bar has a row of its own: no title, artist, album
                // or time runs under or over it, whatever the width; nor do
                // the buttons of a desktop's bar. The artists keep to one row,
                // which shows the first; a desktop's bar draws them within it.
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
                    long("An album"),
                    "Position".to_owned(),
                    "1:05".to_owned(),
                    "4:31".to_owned(),
                ];
                let title_menu = format!("More for {}", long("A song title"));
                let bar_labels: Vec<String> = [&labels[..1], &labels[2..]]
                    .concat()
                    .into_iter()
                    .chain(["Sound device", "Sound processing", "Lyrics", "Queue", title_menu.as_str()].map(String::from))
                    .collect();
                for (width, height) in [(412.0, 915.0), (800.0, 1280.0), (900.0, 760.0), (1100.0, 760.0), (1600.0, 900.0)] {
                    ui.window().set_size(slint::LogicalSize::new(width, height));
                    idle(Duration::from_millis(400)).await;
                    let docked = width >= 900.0;
                    if !docked {
                        ui.window().dispatch_event(WindowEvent::PointerScrolled {
                            position: slint::LogicalPosition::new(width - 10.0, 300.0),
                            delta_x: 0.0,
                            delta_y: 100000.0,
                        });
                        idle(Duration::from_millis(100)).await;
                    }
                    let mut boxes: Vec<_> = if docked { &bar_labels[..] } else { &labels[..] }
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
                    if docked {
                        let row = ui
                            .root_element()
                            .query_descendants()
                            .match_inherits("NameChips")
                            .find_first()
                            .unwrap_or_else(|| panic!("no artists at {width}"));
                        boxes.push((&long_names[0], row.absolute_position(), row.size()));
                    }
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
            // The notes come in a call of their own, in either order with
            // the tracks, and push the rows down: a click they land in misses.
            wait_for("the album's notes", || !ui.get_about().is_empty()).await;
            // With Shift held, a click picks every row from the one picked
            // last to it.
            ui.invoke_select_track(0, false);
            let shift = slint::platform::Key::Shift;
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: shift.into() });
            let third = ui.get_tracks().row_data(3).unwrap().title;
            button(&ui, &third).single_click(PointerEventButton::Left).await;
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: shift.into() });
            let picked: Vec<bool> = ui.get_tracks().iter().map(|track| track.selected).collect();
            assert_eq!(picked, [true, true, true, true, false, false, false]);
            ui.invoke_clear_selection();
            // A work's row picks all its movements, by a finger held or a
            // right-click, and a click on it while selecting lets them go.
            let picks = || -> Vec<bool> { ui.get_tracks().iter().map(|t| t.selected).collect() };
            let works = || -> Vec<bool> {
                [0, 5].map(|row| ui.get_tracks().row_data(row).unwrap().work_selected).to_vec()
            };
            let headings = || {
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Button)
                    .match_predicate(|e| e.accessible_label().is_some_and(|l| l == "Symphony No. 5"))
                    .find_all()
            };
            let shift_held = |held: bool| {
                let text = slint::platform::Key::Shift.into();
                ui.window().dispatch_event(if held {
                    WindowEvent::KeyPressed { text }
                } else {
                    WindowEvent::KeyReleased { text }
                });
            };
            assert_eq!(headings().len(), 2);
            // Tall enough that the bar of the selection covers no row.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 1500.0));
            idle(Duration::from_millis(100)).await;
            ui.global::<Input>().set_touch(true);
            hold(&ui, middle(&headings()[0]), 0.0, 800).await;
            ui.global::<Input>().set_touch(false);
            assert_eq!(picks(), [true, true, false, false, false, false, false]);
            assert_eq!(ui.get_selected_tiles(), 2);
            assert_eq!(works(), [true, false]);
            hold(&ui, middle(&headings()[0]), 0.0, 0).await;
            assert_eq!(picks(), [false; 7]);
            assert_eq!(ui.get_selected_tiles(), 0);
            assert_eq!(works(), [false, false]);
            headings()[1].single_click(PointerEventButton::Right).await;
            assert_eq!(picks(), [false, false, false, false, false, true, true]);
            assert_eq!(works(), [false, true]);
            // One movement let go, the work is not selected, and picking it
            // again takes the one missing.
            ui.invoke_select_track(5, false);
            assert_eq!(works(), [false, false]);
            headings()[1].single_click(PointerEventButton::Right).await;
            assert_eq!(picks(), [false, false, false, false, false, true, true]);
            assert_eq!(ui.get_selected_tiles(), 2);
            ui.invoke_clear_selection();
            // Movements picked one by one make the work selected.
            ui.invoke_select_track(0, false);
            assert_eq!(works(), [false, false]);
            ui.invoke_select_track(1, false);
            assert_eq!(works(), [true, false]);
            ui.invoke_clear_selection();
            // A range from the work's row, and to it from either side.
            headings()[0].single_click(PointerEventButton::Right).await;
            shift_held(true);
            let bows = button(&ui, "Bows");
            bows.single_click(PointerEventButton::Left).await;
            assert_eq!(picks(), [true, true, true, true, true, false, false]);
            ui.invoke_clear_selection();
            ui.invoke_select_track(2, false);
            headings()[1].single_click(PointerEventButton::Right).await;
            assert_eq!(picks(), [false, false, true, true, true, true, true]);
            ui.invoke_clear_selection();
            shift_held(false);
            ui.invoke_select_track(6, false);
            shift_held(true);
            headings()[0].single_click(PointerEventButton::Right).await;
            assert_eq!(picks(), [true; 7]);
            // Shift from a movement of the work itself picks the whole work.
            ui.invoke_clear_selection();
            ui.invoke_select_track(5, false);
            headings()[1].single_click(PointerEventButton::Right).await;
            assert_eq!(picks(), [false, false, false, false, false, true, true]);
            // With nothing picked before, Shift picks just the work.
            ui.invoke_clear_selection();
            headings()[1].single_click(PointerEventButton::Right).await;
            shift_held(false);
            assert_eq!(picks(), [false, false, false, false, false, true, true]);
            ui.invoke_clear_selection();
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
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
            // of the last one, above the bottom bar.
            let bottom_bar = 64.0;
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
                (600.0 - bottom_bar..=640.0 - bottom_bar).contains(&bottom),
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
            assert_eq!(ui.get_section(), 7);
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
            // Where the server counts their plays, Most played, chosen from
            // their Sort menu, starts with the most played, is saved on this
            // device, and turns around; Name, chosen again, goes A to Z, and
            // turns around too.
            let composer_links = |ui: &AppWindow| -> Vec<String> {
                ui.get_tiles().iter().map(|tile| tile.link.into()).collect()
            };
            assert!(ui.get_composer_plays_counted());
            move_sort_choice(&ui, slint::platform::Key::DownArrow).await;
            assert_eq!((ui.get_composer_sort(), ui.get_composer_descending()), (1, true));
            let saved = crate::prefs::load();
            assert_eq!((saved.composer_sort, saved.composer_descending), (1, true));
            assert_eq!(composer_links(&ui), ["composer:r10", "composer:r9"]);
            ui.set_composer_descending(false);
            ui.invoke_sort_composers();
            assert_eq!(composer_links(&ui), ["composer:r9", "composer:r10"]);
            assert!(!crate::prefs::load().composer_descending);
            move_sort_choice(&ui, slint::platform::Key::UpArrow).await;
            assert_eq!((ui.get_composer_sort(), ui.get_composer_descending()), (0, false));
            ui.set_composer_descending(true);
            ui.invoke_sort_composers();
            assert_eq!(composer_links(&ui), ["composer:r10", "composer:r9"]);
            ui.set_composer_descending(false);
            ui.invoke_sort_composers();
            assert_eq!(composer_links(&ui), ["composer:r9", "composer:r10"]);
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
            // Where the server counts their plays, the Plays column, picked
            // like the others, sorts the most played first at first, which
            // is saved on this device, and then the least.
            choose(&ui, "Plays");
            let plays_heading = heading(&ui, "Plays");
            ui.invoke_sort_tracks(plays_heading as i32);
            assert_eq!(
                order(&ui),
                ["composition:w7", "composition:w5", "composition:w9"]
            );
            assert_eq!(piece(&ui, 0)[plays_heading], plain("12"));
            let saved = crate::prefs::load();
            assert_eq!(saved.composition_sort.as_deref(), Some("Plays"));
            assert!(saved.composition_descending);
            ui.invoke_sort_tracks(plays_heading as i32);
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w5", "composition:w7"]
            );
            choose(&ui, "Plays");
            ui.invoke_sort_tracks(3);
            ui.invoke_sort_tracks(3);
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w5", "composition:w7"]
            );
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
            assert!(!ui.get_selection_has_songs(), "the selection's menu plays no composition");
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
            // Each row shows its album's cover, after the row itself, and the
            // row opens the album.
            let cover_of = |row| ui.get_tracks().row_data(row).unwrap().cover.size().width;
            wait_for("the recordings' covers", || cover_of(0) > 0 && cover_of(1) > 0).await;
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
            // The plays sort the most first at first, ties by name either
            // way, and songs the server has no count of played 0 times.
            let plays = ui.get_track_choices().iter().position(|c| c.title == "Plays").unwrap() as i32;
            ui.invoke_toggle_track_column(plays);
            ui.invoke_sort_tracks(column(&ui, "Plays"));
            assert_eq!(sorted(&ui), [("Plays".to_owned(), true)]);
            assert_eq!(first(&ui), ["Song 5", "Song 3", "Song 8"]);
            ui.invoke_sort_tracks(column(&ui, "Plays"));
            assert_eq!(first(&ui), ["Fugue", "Harbour Lights", "Song 10"]);
            ui.invoke_toggle_track_column(plays);
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
            // The selection's menu plays the rows picked in the order shown,
            // as sorted, whichever was picked first.
            let shown = play::testing::page_song_ids(&ui);
            let pick_rows = || {
                ui.invoke_select_line(3, false);
                ui.invoke_select_line(1, false);
            };
            play_selection(&ui, pick_rows, &[shown[1].as_str(), shown[3].as_str()]).await;
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
            // An album's track rows pick the same ways: a finger held, or a
            // right-click, and then a click on another; a finger that moves
            // or lifts sooner picks nothing. The pick outlasts the page.
            ui.invoke_open("album:a1".into());
            wait_for("the album's rows", || ui.get_tracks().row_count() == 2).await;
            let track_picks = |ui: &AppWindow| -> Vec<bool> {
                ui.get_tracks().iter().map(|track| track.selected).collect()
            };
            let track_at = |ui: &AppWindow, row: usize| {
                middle(&button(ui, &ui.get_tracks().row_data(row).unwrap().title))
            };
            ui.global::<Input>().set_touch(true);
            hold(&ui, track_at(&ui, 0), 20.0, 800).await;
            hold(&ui, track_at(&ui, 0), 0.0, 300).await;
            assert_eq!(track_picks(&ui), [false, false]);
            hold(&ui, track_at(&ui, 0), 0.0, 800).await;
            assert_eq!(track_picks(&ui), [true, false]);
            assert_eq!(ui.get_page(), Page::Album);
            ui.global::<Input>().set_touch(false);
            hold(&ui, track_at(&ui, 1), 0.0, 0).await;
            assert_eq!(track_picks(&ui), [true, true]);
            ui.invoke_open("albums".into());
            ui.invoke_open("album:a1".into());
            wait_for("the album again", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(track_picks(&ui), [true, true]);
            let top = ui.get_tracks().row_data(0).unwrap().title;
            button(&ui, &top)
                .single_click(PointerEventButton::Right)
                .await;
            assert_eq!(track_picks(&ui), [false, true]);
            ui.invoke_clear_selection();
            assert_eq!(track_picks(&ui), [false, false]);
            // A finger held on a name under the title picks the row too. The
            // page's header names the artists as well, above the rows.
            let artist = ui.get_tracks().row_data(1).unwrap().names.row_data(0).unwrap().label;
            let names = ui
                .root_element()
                .query_descendants()
                .match_accessible_role(AccessibleRole::Button)
                .match_predicate(move |link| link.accessible_label().is_some_and(|l| l == artist))
                .find_all();
            ui.global::<Input>().set_touch(true);
            hold(&ui, middle(names.last().unwrap()), 0.0, 800).await;
            ui.global::<Input>().set_touch(false);
            ui.global::<Input>().set_finger(false);
            assert_eq!(track_picks(&ui), [false, true]);
            assert_eq!(ui.get_page(), Page::Album);
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
            assert_eq!(ui.get_section(), 9);
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
            // A selection with no songs under it puts nothing in a playlist.
            PLAYLIST_WRITES.lock().unwrap().clear();
            ui.invoke_select_line(1, false);
            ui.invoke_add_selection_to_playlist("p1".into());
            wait_for("nothing added", || {
                ui.get_notice() == "Nothing selected goes in a playlist."
            })
            .await;
            assert!(PLAYLIST_WRITES.lock().unwrap().is_empty());
            ui.invoke_clear_selection();
            click(&ui, "Mara Lind");
            wait_for("her folder", || ui.get_header_title() == "Mara Lind").await;
            assert_eq!(ui.get_section(), 9, "a folder is among the folders");
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
            // A song row goes in a playlist, and a folder's every song under
            // it, each folder gone through once.
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
            wait_for("the folder's songs added", || {
                ui.get_notice() == "Added 2 tracks to Road Trip."
            })
            .await;
            assert_eq!(
                *PLAYLIST_WRITES.lock().unwrap(),
                ["updatePlaylist playlistId=p1&songIdToAdd=t2&songIdToAdd=t1"]
            );
            // The selection's menu plays every song under a folder, in the
            // same order.
            play_selection(&ui, || ui.invoke_select_line(0, false), &["t2", "t1"]).await;
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
            // A song selected on a page left behind plays all the same.
            ui.invoke_open("folder:d2".into());
            wait_for("Night Harbour's folder", || {
                ui.get_header_title() == "Night Harbour" && ui.get_track_lines().row_count() == 2
            })
            .await;
            let rope = (0..2).find(|&row| entry(&ui, row).1 == "song:t2").unwrap();
            ui.invoke_select_line(rope as i32, false);
            click(&ui, "Back");
            wait_for("the top level once more", || {
                ui.get_header_title() == "Folders" && ui.get_track_lines().row_count() == 3
            })
            .await;
            click(&ui, "More for the selection");
            click(&ui, "Play now");
            wait_for("the song left behind", || play::testing::queued_ids().0 == ["t2"]).await;
            play::testing::clear(&ui);
            click(&ui, "Tracks");
            wait_for("the tracks", || ui.get_track_lines().row_count() == 100).await;

            // A start shows the home page kept on this device at once, while
            // the server is slow to sign in to, spanning a phone's window,
            // and then what the server says in its place, on the same visit.
            let kept_answers = answers_kept_for(&server, "joe");
            wait_for("the answers saved", || kept_answers.exists()).await;
            let phone = slint::LogicalSize::new(360.0, 640.0);
            let page_width = |ui: &AppWindow| {
                let main = ui.root_element().query_descendants();
                let page = main.match_accessible_role(AccessibleRole::Main).find_first();
                page.unwrap().size().width
            };
            let named = |ui: &AppWindow, title: &str| ui.get_tiles().iter().any(|t| t.title == title);
            let labels = |ui: &AppWindow| -> Vec<String> {
                ui.get_stats().iter().map(|stat| stat.label.into()).collect()
            };
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            ALBUM_RENAMED.store(true, Relaxed);
            PING_HELD.store(true, Relaxed);
            let slow = start().unwrap();
            slow.window().set_size(phone);
            wait_for("the kept home page", || named(&slow, "Night Harbour")).await;
            assert_eq!(slow.get_page(), Page::Home);
            assert_eq!(page_width(&slow), 360.0);
            let visit = slow.get_visit();
            let (users, queues) = (times_asked("getUser"), times_asked("getPlayQueue"));
            PING_HELD.store(false, Relaxed);
            wait_for("the home page as the server has it", || {
                named(&slow, "Night Harbour (Remastered)") && labels(&slow).len() == 5
            })
            .await;
            assert_eq!(slow.get_visit(), visit, "not opened anew");
            // Signed in, it asks for the user's roles and the queue left
            // playing too.
            wait_for("the user and the queue", || {
                times_asked("getUser") > users && times_asked("getPlayQueue") > queues
            })
            .await;
            ALBUM_RENAMED.store(false, Relaxed);
            idle(Duration::from_millis(500)).await;

            // With nothing kept, a start shows the home page's frame at once
            // while it signs in, spanning a phone's window, and fills it in
            // on the same visit.
            std::fs::remove_file(&kept_answers).unwrap();
            let extensions = times_asked("getOpenSubsonicExtensions");
            PING_HELD.store(true, Relaxed);
            let fresh = start().unwrap();
            fresh.window().set_size(phone);
            wait_for("the home page's frame", || {
                labels(&fresh) == ["Artists", "Albums", "Tracks"]
            })
            .await;
            assert!(fresh.get_sections().iter().any(|section| section == "Albums"));
            assert_eq!(fresh.get_tiles().row_count(), 0);
            assert_eq!(page_width(&fresh), 360.0);
            let visit = fresh.get_visit();
            // Signed in, it counts what the server's extensions offer too,
            // before any answer comes.
            wait_for("the extensions asked for", || {
                times_asked("getOpenSubsonicExtensions") > extensions
            })
            .await;
            ANSWERS_HELD.store(true, Relaxed);
            PING_HELD.store(false, Relaxed);
            wait_for("the frame of what they offer", || labels(&fresh).len() == 5).await;
            assert_eq!(fresh.get_tiles().row_count(), 0);
            assert!(fresh.get_sections().iter().any(|section| section == "Composers"));
            ANSWERS_HELD.store(false, Relaxed);
            wait_for("the home page filled in", || {
                fresh.get_tiles().row_count() == 24 && labels(&fresh).len() == 5
            })
            .await;
            assert_eq!(fresh.get_visit(), visit, "not opened anew");
            idle(Duration::from_millis(500)).await;

            // A second start finds the saved account, with its server down,
            // and the pages kept from over an hour before.
            wait_for("the answers saved", || kept_answers.exists()).await;
            let lists_kept = LISTS.load(Relaxed);
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            DOWN.store(true, Relaxed);
            // The most played, last in the Sort menus, come back too.
            let mut saved = crate::prefs::load();
            assert_eq!((saved.album_sort, saved.album_descending), (1, true));
            (saved.album_sort, saved.artist_sort) = (4, 1);
            (saved.composer_sort, saved.composer_descending) = (1, true);
            crate::prefs::save(&saved);
            // It removes the listens an earlier version kept for ListenBrainz.
            prefs::leave_waiting_listens();
            let again = start().unwrap();
            assert!(!prefs::has_waiting_listens());
            wait_for("the offline note", || again.get_offline() != "").await;
            assert_eq!(again.get_step(), Step::Done);
            // The home page kept stays, spanning a phone's window.
            again.window().set_size(phone);
            wait_for("the kept home page", || again.get_tiles().row_count() == 24).await;
            assert_eq!(again.get_page(), Page::Home);
            assert_eq!(page_width(&again), 360.0);
            // A page with nothing kept, as the tracks page lists the empty
            // search, says the server didn't answer.
            again
                .window()
                .set_size(slint::LogicalSize::new(1100.0, 760.0));
            click(&again, "Tracks");
            wait_for("the tracks' problem", || again.get_page_problem() != "").await;
            // Pulled down to ask for it anew, it signs in again, and says the
            // server didn't answer once more.
            let pings_while_down = || {
                let asked = ASKED_WHILE_DOWN.lock().unwrap();
                asked.iter().filter(|call| call.starts_with("ping ")).count()
            };
            let pinged = pings_while_down();
            again.invoke_refresh();
            wait_for("a sign-in while down", || pings_while_down() > pinged).await;
            wait_for("the note once more", || again.get_offline() != "").await;
            assert_eq!(again.get_page(), Page::Tracks);
            DOWN.store(false, Relaxed);
            // Set before it signs in again, which reads it.
            let rob = ListenBrainz {
                token: "rob token".into(),
                user: "rob".into(),
            };
            store::save_listenbrainz(rob).await.unwrap();
            // The page's own Try again, below the note's, signs in again,
            // and asks for the page on screen. Pressed twice while the server
            // is slow to sign in to, it signs in once: the second finds the
            // library signed in by the first.
            let tries = || {
                again
                    .root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Button)
                    .match_predicate(|button| button.accessible_label().is_some_and(|l| l == "Try again"))
                    .find_all()
            };
            assert_eq!(tries().len(), 2, "the note's and the page's");
            let (pings, users) = (times_asked("ping"), times_asked("getUser"));
            PING_HELD.store(true, Relaxed);
            // Each reads the ListenBrainz user anew just before it signs in.
            for _ in 0..2 {
                again.set_listenbrainz_user("".into());
                let page_try_again = tries().pop().expect("the page's Try again");
                page_try_again.invoke_accessible_default_action();
                wait_for("a Try again under way", || again.get_listenbrainz_user() == "rob").await;
            }
            PING_HELD.store(false, Relaxed);
            wait_for("the tracks", || again.get_track_lines().row_count() == 100).await;
            assert_eq!(again.get_page(), Page::Tracks);
            wait_for("both signed in", || times_asked("ping") >= pings + 2).await;
            idle(Duration::from_millis(500)).await;
            assert_eq!(times_asked("getUser"), users + 1, "signed in once");
            assert_eq!(again.get_offline(), "");
            assert!(again.get_sections().iter().any(|section| section == "Composers"));
            // The home page: a welcome, the library's counts and its newest
            // albums.
            click(&again, "Home");
            let stats = |ui: &AppWindow| -> Vec<(String, String)> {
                let stats = ui.get_stats();
                stats
                    .iter()
                    .map(|stat| (stat.label.into(), stat.value.into()))
                    .collect()
            };
            // The tracks are as many as the tracks page lists, which the
            // server's scan counts; the albums' own counts are short of them.
            let tracks = |ui: &AppWindow| stats(ui).into_iter().find(|(label, _)| label == "Tracks");
            wait_for("the home page", || {
                again.get_tiles().row_count() == 24
                    && stats(&again).iter().all(|(_, value)| value != "…")
                    && tracks(&again).is_some_and(|(_, value)| value != "2")
            })
            .await;
            wait_for("the library kept asked for again", || LISTS.load(Relaxed) > lists_kept).await;
            assert_eq!(again.get_offline(), "");
            assert_eq!(again.get_page(), Page::Home);
            assert_eq!(again.get_username(), "joe");
            assert_eq!(
                stats(&again),
                [
                    ("Artists", "122"),
                    ("Albums", "603"),
                    ("Tracks", "230"),
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

            // With a token set, the home page lists what rob played most on
            // ListenBrainz in the period picked, this year at first.
            let ranking = |list: usize| -> Vec<RankedEntry> {
                let lists = again.get_stat_lists();
                let list = lists.row_data(list);
                list.map_or_else(Vec::new, |list| list.entries.iter().collect())
            };
            let places = |list: usize| -> Vec<(i32, String, String, String)> {
                (ranking(list).into_iter())
                    .map(|e| (e.rank, e.title.into(), e.plays.into(), e.link.into()))
                    .collect()
            };
            let place = |rank: i32, title: &str, plays: &str, link: &str| {
                (rank, title.to_owned(), plays.to_owned(), link.to_owned())
            };
            wait_for("the albums matched", || {
                places(0).first().is_some_and(|first| first.3 == "album:b1")
            })
            .await;
            assert_eq!(again.get_stat_period(), 2);
            assert_eq!(
                places(0),
                [
                    place(1, "Sonatas (Deluxe)", "18 plays", "album:b1"),
                    place(2, "night harbour", "12 plays", "album:a1"),
                    place(3, "Away", "1 play", ""),
                ]
            );
            wait_for("the artists matched", || {
                places(1).first().is_some_and(|first| first.3 == "artist:r1")
            })
            .await;
            assert_eq!(
                places(1),
                [
                    place(1, "MARA LIND", "30 plays", "artist:r1"),
                    place(2, "A. Bach", "9 plays", "artist:r3"),
                    place(3, "Röyksopp", "2 plays", ""),
                ]
            );
            let genres = again.get_stat_lists().row_data(2).unwrap();
            assert_eq!(genres.entries.row_count(), 0);
            assert_eq!(genres.note, "ListenBrainz hasn't worked this period out yet.");
            assert!(!genres.pictured && ranking(0).len() == 3);
            // The library's cover for what it has, the archive's for the
            // album it hasn't, and none for the artist it has no picture of.
            wait_for("the pictures", || {
                [ranking(0)[1].picture.size().width, ranking(0)[2].picture.size().width, ranking(1)[0].picture.size().width]
                    .iter()
                    .all(|width| *width > 0)
            })
            .await;
            assert_eq!(ranking(0)[0].picture.size().width, 0, "b1 has no cover");
            assert_eq!(ranking(1)[2].picture.size().width, 0);
            let asked = || STATISTICS_ASKED.lock().unwrap().clone();
            // The token goes to ListenBrainz and never to the archive.
            assert_eq!(*ARCHIVE_AUTHORIZED.lock().unwrap(), [false]);
            assert_eq!(
                *STATISTICS_AUTHORIZED.lock().unwrap(),
                ["token rob token"; 3]
            );
            assert_eq!(
                asked(),
                [
                    "/1/stats/user/rob/release-groups?range=this_year&count=10",
                    "/1/stats/user/rob/artists?range=this_year&count=10",
                    "/1/stats/user/rob/genre-activity?range=this_year",
                ]
            );

            // Another period keeps the lists until its own arrive.
            assert!(comes_into_view(&again, "All time").await);
            click(&again, "All time");
            assert_eq!(again.get_stat_period(), 3);
            assert!(again.get_stat_loading());
            assert_eq!(places(0).len(), 3, "this year's albums stay meanwhile");
            wait_for("all time", || !again.get_stat_loading()).await;
            assert_eq!(places(0), [place(1, "Night Harbour", "480 plays", "album:a1")]);
            assert_eq!(
                places(2),
                [
                    place(1, "Folk", "30 plays", "genre:Folk"),
                    place(2, "Shoegaze", "4 plays", ""),
                    place(3, "Ambient", "3 plays", ""),
                    place(4, "Dub", "2 plays", ""),
                    place(5, "Drone", "1 play", ""),
                ]
            );
            assert_eq!(again.get_stat_lists().row_data(1).unwrap().note, "Nothing was played in this period.");
            assert_eq!(asked().len(), 6);

            // A place in the library opens its page, and coming back to the
            // home page doesn't ask again.
            assert!(comes_into_view(&again, "1. Night Harbour, Mara Lind, 480 plays").await);
            click(&again, "1. Night Harbour, Mara Lind, 480 plays");
            wait_for("the album", || again.get_page() == Page::Album).await;
            click(&again, "Back");
            assert_eq!(again.get_page(), Page::Home);
            idle(Duration::from_millis(250)).await;
            assert_eq!(asked().len(), 6, "the lists are not asked for again");

            // By the page's own width, not the window's: where three rows
            // don't fit across, albums and artists are side by side over the
            // genres in two columns of their own, and where two don't, the
            // lists are slides, one at a time. At 900 the docked sidebar
            // leaves the page too narrow for two.
            let (night, folk) = ("1. Night Harbour, Mara Lind, 480 plays", "1. Folk, 30 plays");
            let genres = [folk, "2. Shoegaze, 4 plays", "3. Ambient, 3 plays", "4. Dub, 2 plays", "5. Drone, 1 play"];
            // A slide next to the shown one is partly in the window, so a
            // row counts as shown when its middle is.
            let sees = |label: &str| {
                let width = again.window().size().to_logical(again.window().scale_factor()).width;
                find(&again, label).is_some_and(|row| (0.0..width).contains(&middle(&row).x))
            };
            let at = |label: &str| find(&again, label).unwrap().absolute_position();
            let near = |a: f32, b: f32| (a - b).abs() < 1.0;
            let show_at = async |width: f32| {
                again.window().set_size(slint::LogicalSize::new(width, 900.0));
                idle(Duration::from_millis(300)).await;
                assert!(comes_into_view(&again, night).await);
                // Far enough on that the rows below it are in view too.
                scroll_page(&again, -300.0);
                idle(Duration::from_millis(100)).await;
            };
            show_at(1500.0).await;
            assert!(sees(folk) && !sees("Next ranking"), "three columns need no arrows");
            let (albums, first, rows) = (at(night), at(folk), genres.map(at));
            assert!(first.x > albums.x && near(first.y, albums.y), "genres beside the albums");
            assert!(rows.windows(2).all(|pair| near(pair[0].x, pair[1].x) && pair[1].y > pair[0].y), "in one column");
            show_at(1100.0).await;
            assert!(sees(folk) && !sees("Next ranking"), "two columns need no arrows");
            let (albums, first, rows) = (at(night), at(folk), genres.map(at));
            assert!(near(first.x, albums.x) && first.y > albums.y, "genres under the albums");
            // Filled top to bottom: 1 to 3 on the left, 4 and 5 on the right.
            let (left, right) = (&rows[..3], &rows[3..]);
            assert!(left.windows(2).all(|pair| near(pair[0].x, pair[1].x) && pair[1].y > pair[0].y), "1 to 3 down the left");
            assert!(near(right[0].x, right[1].x) && right[0].x > left[0].x, "4 and 5 on the right");
            assert!(near(right[0].y, left[0].y) && near(right[1].y, left[1].y), "starting level with 1");
            show_at(900.0).await;
            assert!(!sees(folk) && sees("Next ranking"), "too narrow for two columns");

            // On a phone, the arrows and dots change the slide, and the
            // arrow at either end is disabled.
            again.window().set_size(slint::LogicalSize::new(400.0, 800.0));
            idle(Duration::from_millis(300)).await;
            // A scroll at 600 is outside a window this narrow.
            scroll_page_at(&again, 200.0, TOP_OF_PAGE);
            for _ in 0..40 {
                if sees(night) {
                    break;
                }
                idle(Duration::from_millis(30)).await;
                scroll_page_at(&again, 200.0, -150.0);
            }
            assert!(sees(night), "the lists come into view");
            // The dots are under the rows.
            scroll_page_at(&again, 200.0, -300.0);
            idle(Duration::from_millis(100)).await;
            let enabled = |label: &str| button(&again, label).accessible_enabled();
            let dots = || ["Top albums", "Top artists", "Top genres"].map(|title| button(&again, title).accessible_item_selected() == Some(true));
            assert_eq!(dots(), [true, false, false]);
            assert_eq!(enabled("Previous ranking"), Some(false));
            assert_eq!(enabled("Next ranking"), Some(true));
            click(&again, "Next ranking");
            wait_for("the artists", || !sees(night)).await;
            assert_eq!(enabled("Previous ranking"), Some(true));
            assert_eq!(dots(), [false, true, false]);
            click(&again, "Next ranking");
            wait_for("the genres", || sees(folk)).await;
            assert_eq!(enabled("Next ranking"), Some(false));
            assert_eq!(dots(), [false, false, true]);
            click(&again, "Previous ranking");
            wait_for("the artists again", || !sees(folk)).await;
            assert_eq!(dots(), [false, true, false]);
            click(&again, "Top genres");
            wait_for("the genres by its dot", || sees(folk)).await;
            assert_eq!(dots(), [false, false, true]);
            click(&again, "Top albums");
            wait_for("the albums by its dot", || sees(night)).await;
            idle(SLIDE_GLIDE).await;
            assert_eq!(enabled("Previous ranking"), Some(false));
            assert_eq!(dots(), [true, false, false]);

            // A swipe moves one slide, whether a mouse's drag or a finger's,
            // quick or slow, and opens no row under it.
            let (touch, finger) = (again.global::<Input>().get_touch(), again.global::<Input>().get_finger());
            let flicks: [Flick; 5] = [
                ("a mouse's drag", false, &[(40, -60.0, 0.0), (40, -120.0, 0.0), (40, -200.0, 0.0)], 40),
                ("a quick flick", true, &[(8, -60.0, 0.0), (8, -120.0, 0.0), (8, -200.0, 0.0)], 8),
                ("a flick of 90ms", true, &[(0, -30.0, 0.0), (45, -120.0, 0.0)], 45),
                ("a flick in steps", true, &[(30, -33.0, 0.0), (30, -66.0, 0.0), (30, -100.0, 0.0)], 0),
                ("a slow short swipe", true, &[(40, -35.0, 0.0), (40, -70.0, 0.0)], 60),
            ];
            for (name, by_finger, path, rest) in flicks {
                again.global::<Input>().set_touch(by_finger);
                again.global::<Input>().set_finger(by_finger);
                drag(&again, middle(&button(&again, night)), path, rest).await;
                wait_for(name, || !sees(night)).await;
                assert_eq!(again.get_page(), Page::Home, "{name} opened nothing");
                assert_eq!(enabled("Previous ranking"), Some(true), "{name} moved one slide");
                assert_eq!(enabled("Next ranking"), Some(true), "{name} moved only one");
                assert_eq!(dots(), [false, true, false], "{name} lit the second dot");
                click(&again, "Previous ranking");
                wait_for("the albums back", || sees(night)).await;
                idle(SLIDE_GLIDE).await;
            }
            // A drag too short to count, or the wrong way at the start,
            // stays where it is.
            drag(&again, middle(&button(&again, night)), &[(40, -20.0, 0.0), (40, -30.0, 0.0)], 60).await;
            idle(Duration::from_millis(400)).await;
            assert!(sees(night) && again.get_page() == Page::Home);
            drag(&again, middle(&button(&again, night)), &[(40, 60.0, 0.0), (40, 120.0, 0.0)], 60).await;
            idle(Duration::from_millis(400)).await;
            assert!(sees(night), "the first slide has none before it");
            again.global::<Input>().set_touch(touch);
            again.global::<Input>().set_finger(finger);
            again.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            idle(Duration::from_millis(300)).await;

            // Another account's lists don't stay while its own are on the way.
            again.set_listenbrainz_user("sue".into());
            again.invoke_show_more();
            assert_eq!(again.get_stat_lists().row_count(), 0);
            wait_for("sue's lists", || !again.get_stat_loading()).await;
            assert!(asked().last().unwrap().starts_with("/1/stats/user/sue/"));
            assert_eq!(asked().len(), 9);

            // Without the token there is no list, and nothing is asked.
            store::forget_listenbrainz().await.unwrap();
            again.set_listenbrainz_user("".into());
            assert!(!comes_into_view(&again, "All time").await);
            // The shelf's buttons below are in view again from the top.
            scroll_page(&again, TOP_OF_PAGE);
            assert_eq!(asked().len(), 9);

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
            // Short enough that the page goes on past the sheet's edge, with
            // its cover at its smallest.
            again.window().set_size(slint::LogicalSize::new(360.0, 480.0));
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
            assert_eq!(again.get_album_sort(), 4);
            assert!(again.get_album_descending());
            assert_eq!(again.get_artist_sort(), 1);
            assert_eq!((again.get_composer_sort(), again.get_composer_descending()), (1, true));
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

            // A phone has no sidebar and no menu button: a bottom bar takes
            // its place, and the settings leave the top bar for it. The top
            // bar's buttons are a finger's size, the first one's icon on the
            // pages' 24px margin.
            let navigations = || -> Vec<String> {
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Navigation)
                    .find_all()
                    .iter()
                    .filter_map(|navigation| navigation.accessible_label())
                    .map(Into::into)
                    .collect()
            };
            ui.window().set_size(slint::LogicalSize::new(360.0, 640.0));
            assert_eq!(navigations(), ["Main"]);
            assert!(find(&ui, "Menu").is_none());
            let back = button(&ui, "Back");
            assert_eq!(back.size(), slint::LogicalSize::new(48.0, 48.0));
            assert_eq!(back.absolute_position().x, 12.0);
            for entry in ["Home", "Browse", "Library", "Settings"] {
                button(&ui, entry);
            }
            click(&ui, "Library");
            assert_eq!(ui.get_page(), Page::Library);
            // The playlists page is their way in; the sidebar's entry per
            // playlist stays out of the hub.
            assert!(ui.get_sections().iter().any(|label| label == "Road Trip"));
            let hub: Vec<_> = ui.get_library_links().iter().map(|entry| entry.link).collect();
            assert!(hub.iter().any(|link| link == "playlists"), "{hub:?}");
            assert!(!hub.iter().any(|link| link.starts_with("playlist:")), "{hub:?}");
            click(&ui, "Albums");
            wait_for("the albums", || ui.get_page() == Page::Albums).await;
            assert_eq!(ui.get_bar_section(), 2, "an album's page lights Library");
            // An upright tablet's window keeps the sidebar closed until the
            // menu button opens it over the page, and Android's back closes
            // it and stays on the page.
            ui.window().set_size(slint::LogicalSize::new(800.0, 640.0));
            // `changed phone` closes the drawer once the event loop runs;
            // let it, or it closes the drawer opened next.
            idle(Duration::from_millis(400)).await;
            assert!(navigations().is_empty());
            let page = ui.get_page();
            click(&ui, "Menu");
            assert_eq!(navigations(), ["Library"]);
            press(&ui, slint::platform::Key::Back);
            assert!(navigations().is_empty());
            assert_eq!(ui.get_page(), page);
            click(&ui, "Menu");
            back::pressed(&ui, true);
            assert!(navigations().is_empty(), "back closes the drawer first");
            assert_eq!(ui.get_page(), page);
            // A drawer left open when the window turns into a phone's stays
            // closed when it widens again.
            click(&ui, "Menu");
            ui.window().set_size(slint::LogicalSize::new(360.0, 640.0));
            idle(Duration::from_millis(400)).await;
            ui.window().set_size(slint::LogicalSize::new(800.0, 640.0));
            idle(Duration::from_millis(400)).await;
            assert!(navigations().is_empty(), "the drawer stays shut");
            ui.window().set_size(slint::LogicalSize::new(360.0, 640.0));
            click(&ui, "Browse");
            assert_eq!(ui.get_page(), Page::Browse);
            click(&ui, "Genres");
            wait_for("the genres", || ui.get_page() == Page::Genres).await;
            assert_eq!(ui.get_bar_section(), 1, "Genres lights Browse");
            click(&ui, "Back");
            assert_eq!(ui.get_page(), Page::Browse);
            click(&ui, "Settings");
            assert_eq!(ui.get_page(), Page::Settings);
            assert_eq!(ui.get_bar_section(), 3);
            // A laptop's window docks the sidebar.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            assert!(find(&ui, "Menu").is_none());
            assert_eq!(navigations(), ["Library"]);

            // Home has no shelves of what was played last while the server
            // knows of no plays.
            ui.window().set_size(ALL_OF_HOME);
            let asks = RECENT_ASKS.load(Relaxed);
            ui.invoke_open("home".into());
            wait_for("the albums played last", || RECENT_ASKS.load(Relaxed) > asks).await;
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_played_albums().row_count(), 0);
            assert_eq!(ui.get_played_artists().row_count(), 0);
            assert!(text_top(&ui, "Recently added").is_some());
            assert_eq!(text_top(&ui, "Last played albums"), None);
            assert_eq!(text_top(&ui, "Last listened artists"), None);
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));

            // A song that begins is told to the server as playing now. Heard
            // for half its length, when ListenBrainz counts a listen, it is
            // told once as played, from when it began, and not before. No
            // player is left from the steps before, whose looks at nothing
            // playing would end the song's play.
            play::testing::clear(&ui);
            PLAYING_NOW.lock().unwrap().clear();
            let told = || SCROBBLES.lock().unwrap().clone();
            let playing_now = || PLAYING_NOW.lock().unwrap().clone();
            let time_told = |call: &str, id: &str| -> u128 {
                (call.strip_prefix(&format!("scrobble id={id}&time=")))
                    .and_then(|time| time.strip_suffix("&submission=true"))
                    .unwrap_or_else(|| panic!("{call}"))
                    .parse()
                    .unwrap()
            };
            let harbour: opensubsonic::Child = serde_json::from_str(
                r#"{"id":"t1","isDir":false,"title":"Harbour Lights","albumId":"a1","duration":2}"#,
            )
            .unwrap();
            let since_1970 = || std::time::UNIX_EPOCH.elapsed().unwrap().as_millis();
            let began = since_1970();
            for position in [0.0, 0.4, 0.8] {
                play::testing::hear(&harbour, position);
                idle(Duration::from_millis(400)).await;
            }
            wait_for("the song playing now told", || !playing_now().is_empty()).await;
            assert_eq!(playing_now(), ["scrobble id=t1&submission=false"]);
            assert!(told().is_empty(), "told before half of it played");
            play::testing::hear(&harbour, 1.2);
            wait_for("the play told", || !told().is_empty()).await;
            play::testing::hear(&harbour, 1.6);
            idle(Duration::from_millis(200)).await;
            assert_eq!(told().len(), 1, "{:?}", told());
            let time = time_told(&told()[0], "t1");
            assert!(time <= began + 100 && time + 1000 > began, "played from {time}, began {began}");
            assert_eq!(playing_now().len(), 1, "playing now is told once");
            // It is logged on this device too, as played from that time;
            // the history's steps below start from an empty log.
            let joe = account_of(&server, "joe");
            let logged_at = async |id: &str| -> u128 {
                let quoted = format!(r#""{id}""#);
                wait_for("the play logged", || {
                    std::fs::read_to_string(plays_kept_for(&server, "joe"))
                        .is_ok_and(|log| log.contains(&quoted))
                })
                .await;
                let logged = plays::read(&joe).await;
                let play = logged.iter().find(|play| play.id == id).unwrap();
                play.listened_at as u128 * 1000
            };
            assert_eq!(logged_at("t1").await, time);
            assert_eq!(plays::read(&joe).await.len(), 1);
            plays::forget(&joe).unwrap();

            // The next visit to Home, within the hour the empty list asked
            // for above is kept, asks the server again and shows it: the
            // albums played last, then their artists, each once, in that
            // order, as the library has them.
            let asks = RECENT_ASKS.load(Relaxed);
            ui.invoke_open("albums".into());
            ui.invoke_open("home".into());
            wait_for("the albums played last asked again", || RECENT_ASKS.load(Relaxed) > asks).await;
            wait_for("the albums played last", || ui.get_played_albums().row_count() == 2).await;
            let tile_links = |tiles: slint::ModelRc<AlbumTile>| -> Vec<String> {
                tiles.iter().map(|tile| tile.link.into()).collect()
            };
            assert_eq!(tile_links(ui.get_played_albums()), ["album:a1", "album:a9"]);
            assert_eq!(tile_links(ui.get_played_artists()), ["artist:r1", "artist:r3"]);
            let first_artist = || ui.get_played_artists().row_data(0).unwrap();
            assert_eq!(first_artist().title, "Mara Lind");
            assert_eq!(first_artist().names.row_data(0).unwrap().label, "1 album");
            wait_for("her picture", || first_artist().cover.size().width > 0).await;
            // Off Home again, which the steps below open anew.
            ui.invoke_open("albums".into());

            // The next visit to a page sorted by plays after a play shows its
            // order as it was at once, then asks the server again and puts
            // the play in place. An artist's plays are their albums'.
            let (album_sort, album_descending) = (ui.get_album_sort(), ui.get_album_descending());
            ui.invoke_open("artists".into());
            ui.set_artist_sort(1);
            ui.set_artist_descending(true);
            ui.invoke_sort_artists();
            assert_eq!(tile_links(ui.get_tiles())[..3], ["artist:r3", "artist:r1", "artist:o1"]);
            wait_for("Mara Lind's play", || tile_links(ui.get_tiles())[0] == "artist:r1").await;
            assert_eq!(tile_links(ui.get_tiles())[..3], ["artist:r1", "artist:r3", "artist:o1"]);
            ui.set_artist_descending(false);
            ui.invoke_sort_artists();
            assert_eq!(tile_links(ui.get_tiles())[..3], ["artist:r3", "artist:o1", "artist:o2"]);
            ui.set_artist_sort(0);
            ui.set_artist_descending(true);
            ui.invoke_sort_artists();
            assert_eq!(tile_links(ui.get_tiles())[..3], ["artist:o120", "artist:o119", "artist:o118"]);
            ui.invoke_open("albums".into());
            ui.set_album_sort(4);
            ui.set_album_descending(true);
            ui.invoke_sort_albums();
            assert_eq!(tile_links(ui.get_tiles())[..3], ["album:a1", "album:b2", "album:b3"]);
            let top_titles = |ui: &AppWindow| -> Vec<String> {
                (0..3).map(|row| cell(ui, row, 1)[0].0.clone()).collect()
            };
            ui.invoke_open("tracks".into());
            wait_for("the tracks", || ui.get_track_lines().row_count() == 100).await;
            ui.invoke_toggle_track_column(plays);
            ui.invoke_sort_tracks(column(&ui, "Plays"));
            assert_eq!(top_titles(&ui), ["Song 5", "Song 3", "Song 8"]);
            ui.invoke_open("albums".into());
            ui.invoke_open("tracks".into());
            assert_eq!(top_titles(&ui), ["Song 5", "Song 3", "Song 8"]);
            wait_for("Harbour Lights' play", || top_titles(&ui)[0] == "Harbour Lights").await;
            assert_eq!(top_titles(&ui), ["Harbour Lights", "Song 5", "Song 3"]);
            // A play that leaves the order as it is shows its count in place
            // too, once the server is asked again. Its log is left out of
            // the history's steps below as the first play's is.
            let song_5: opensubsonic::Child = serde_json::from_str(
                r#"{"id":"t5","isDir":false,"title":"Song 5","duration":2}"#,
            )
            .unwrap();
            for position in [0.0, 0.4, 0.8] {
                play::testing::hear(&song_5, position);
                idle(Duration::from_millis(400)).await;
            }
            play::testing::hear(&song_5, 1.2);
            wait_for("Song 5's play told", || SCROBBLES.lock().unwrap().len() == 2).await;
            wait_for("Song 5's play logged", || {
                std::fs::read_to_string(plays_kept_for(&server, "joe"))
                    .is_ok_and(|log| log.contains(r#""t5""#))
            })
            .await;
            plays::forget(&joe).unwrap();
            let plays_column = titles(&ui).iter().position(|title| title == "Plays").unwrap();
            let song_5_plays = |ui: &AppWindow| cell(ui, 1, plays_column)[0].0.clone();
            ui.invoke_open("artists".into());
            ui.invoke_open("tracks".into());
            assert_eq!(song_5_plays(&ui), "3");
            wait_for("Song 5's second play", || song_5_plays(&ui) == "4").await;
            assert_eq!(top_titles(&ui), ["Harbour Lights", "Song 5", "Song 3"]);
            ui.invoke_toggle_track_column(plays);
            ui.invoke_sort_tracks(column(&ui, "Year"));
            ui.invoke_sort_tracks(column(&ui, "Year"));
            assert_eq!(sorted(&ui), [("Year".to_owned(), true)]);
            // So do the composers, whose plays Fonoteca counts, where they
            // are sorted by them; by name they aren't asked for again.
            let composers_asked = times_asked("getComposers");
            ui.invoke_open("composers".into());
            idle(Duration::from_millis(300)).await;
            assert_eq!(times_asked("getComposers"), composers_asked);
            // When the server can't give them, the counts shown stay, and
            // the next visit asks again.
            COMPOSERS_DOWN.store(true, Relaxed);
            ui.set_composer_sort(1);
            ui.set_composer_descending(true);
            ui.invoke_sort_composers();
            assert_eq!(composer_links(&ui), ["composer:r10", "composer:r9"]);
            wait_for("the composers refused", || {
                times_asked("getComposers") > composers_asked
            })
            .await;
            idle(Duration::from_millis(300)).await;
            COMPOSERS_DOWN.store(false, Relaxed);
            assert_eq!(composer_links(&ui), ["composer:r10", "composer:r9"]);
            ui.invoke_open("setting:downloads".into());
            ui.invoke_back();
            wait_for("Beethoven's plays", || composer_links(&ui)[0] == "composer:r9").await;
            // Once they came, a visit doesn't ask for them again. An hour
            // on, so that the answer kept can't stand in for the server.
            let composers_asked = times_asked("getComposers");
            let now = answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            ui.invoke_open("setting:downloads".into());
            ui.invoke_back();
            idle(Duration::from_millis(300)).await;
            assert_eq!(times_asked("getComposers"), composers_asked);
            answers::LATER.store(now, Relaxed);
            ui.set_composer_sort(0);
            ui.set_composer_descending(false);
            ui.invoke_sort_composers();
            // The compositions' too, where their table shows the plays:
            // sorted by date, the Egmont Overture's count changes in place.
            // Without them, they aren't asked for again.
            let compositions_asked = times_asked("getCompositions");
            ui.invoke_open("compositions".into());
            idle(Duration::from_millis(300)).await;
            assert_eq!(times_asked("getCompositions"), compositions_asked);
            choose(&ui, "Plays");
            let plays_heading = heading(&ui, "Plays");
            let egmont_plays = |ui: &AppWindow| piece(ui, 0)[plays_heading].clone();
            ui.invoke_open("albums".into());
            ui.invoke_open("compositions".into());
            assert_eq!(egmont_plays(&ui), plain("3"));
            wait_for("the Egmont Overture's plays", || egmont_plays(&ui) == plain("15")).await;
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w5", "composition:w7"]
            );
            ui.invoke_sort_tracks(plays_heading as i32);
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w7", "composition:w5"]
            );
            choose(&ui, "Plays");
            ui.invoke_sort_tracks(3);
            ui.invoke_sort_tracks(3);
            ui.set_artist_sort(0);
            ui.set_artist_descending(false);
            ui.set_album_sort(album_sort);
            ui.set_album_descending(album_descending);
            ui.invoke_sort_albums();

            // Without a token the home page has no "Created for you" shelf and
            // ListenBrainz is not asked; a playlist page says why it is empty.
            let listenbrainz_root = format!("{}/1/", serve(listenbrainz));
            browse::created_for::testing::serve_from(&listenbrainz_root);
            store::forget_listenbrainz().await.unwrap();
            ui.invoke_open("home".into());
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_created_for_you().row_count(), 0);
            assert_eq!(ui.get_page_problem(), "");
            assert!(LISTENBRAINZ_ASKED.lock().unwrap().is_empty());
            ui.invoke_open("createdfor:daily".into());
            wait_for("the missing token said", || {
                ui.get_page_problem().starts_with("Set your ListenBrainz token")
            })
            .await;

            // With one, the shelf has the playlists made for the user, but not
            // the exploration one, and the request carries the token.
            store::save_listenbrainz(ListenBrainz { token: "tok".into(), user: "joe".into() })
                .await
                .unwrap();
            LATER_SLOW.store(true, Relaxed);
            // A playlist opened before Home has made its tiles has its tracks
            // at once, the tint of its kind once the tiles are made, then the
            // banner its tile has, while the last playlist's tracks are still
            // being fetched.
            ui.invoke_open("createdfor:trip".into());
            wait_for("the playlist's tracks", || ui.get_tracks().row_count() == 1).await;
            assert_eq!(ui.get_created_for_kind(), "");
            assert!(mix_banner(&ui).is_none());
            wait_for("its kind", || ui.get_created_for_kind() == "Weekly Jams").await;
            assert_eq!(ui.get_created_for_tint(), KindTint::Green);
            // The tint and name are there when the list is, ahead of the banner,
            // which waits for the playlist's tracks.
            assert_eq!(ui.get_created_for_banner().size().width, 0);
            let banner = mix_banner(&ui).expect("the banner");
            assert_eq!(banner.accessible_label().as_deref(), Some("Weekly Jams"));
            assert!(!header_has_cover(&ui), "a mix has its banner, not a cover");
            wait_for("its banner", || ui.get_created_for_banner().size().width > 0).await;
            assert!(!LATER_ANSWERED.load(Relaxed));
            wait_for("the last playlist", || LATER_ANSWERED.load(Relaxed)).await;
            LATER_SLOW.store(false, Relaxed);
            idle(Duration::from_millis(300)).await;

            ui.invoke_open("home".into());
            wait_for("the Created for you shelf", || ui.get_created_for_you().row_count() == 3).await;
            let shelf: Vec<(String, String)> = (ui.get_created_for_you().iter())
                .map(|tile| (tile.title.to_string(), tile.link.to_string()))
                .collect();
            assert_eq!(
                shelf,
                [
                    ("Daily Jams".to_owned(), "createdfor:daily".to_owned()),
                    ("Weekly Jams".to_owned(), "createdfor:trip".to_owned()),
                    ("Last Week's Jams".to_owned(), "createdfor:later".to_owned())
                ]
            );
            // Each tile keeps its playlist's own name beneath its kind's, except
            // where they are the same.
            let beneath = |row| ui.get_created_for_you().row_data(row).unwrap().names.row_data(0);
            assert_eq!(beneath(0).unwrap().label, "Daily Jams for joe, 2026-11-04 Wed");
            // The tracks of each decide the banner: the first has no artist the
            // library knows, the second Anna Bach who has no banner and Mara
            // Lind who has, the third Mara Lind again, whose banner the second
            // has taken.
            let covers = || -> Vec<bool> {
                (ui.get_created_for_you().iter())
                    .map(|tile| tile.cover.size().width > 0)
                    .collect()
            };
            wait_for("the banner", || covers()[1]).await;
            idle(Duration::from_millis(300)).await;
            assert_eq!(covers(), [false, true, false]);
            // The page and the shelf each fetched the trip playlist's tracks, the
            // shelf the others'; Home showed the shelf as kept within the hour
            // and asked for nothing.
            let asked = LISTENBRAINZ_ASKED.lock().unwrap().clone();
            let list = "GET /1/user/joe/playlists/createdfor HTTP/1.1 token=true";
            assert_eq!(asked.iter().filter(|line| *line == list).count(), 1);
            let mut fetched: Vec<&str> = (asked.iter())
                .filter_map(|line| line.strip_prefix("GET /1/playlist/"))
                .collect();
            fetched.sort();
            assert_eq!(
                fetched,
                [
                    "daily HTTP/1.1 token=true",
                    "later HTTP/1.1 token=true",
                    "trip HTTP/1.1 token=true",
                    "trip HTTP/1.1 token=true"
                ]
            );
            // Each call about a call's spacing after the one before, as ListenBrainz
            // asks: arrivals jitter by a few milliseconds, calls without the gate
            // follow each other in one or two.
            let asked_at = LISTENBRAINZ_AT.lock().unwrap().clone();
            let spaced =
                |pair: &[Instant]| pair[1] - pair[0] >= listenbrainz::STAND_IN_SPACING / 2;
            assert!(asked_at.windows(2).all(spaced), "{asked_at:?}");
            // As they arrive, the tracks of the playlists are looked up in the
            // library, before any is opened: a track the library has takes
            // one search, one it hasn't two.
            wait_for("the playlists' songs", || SEARCHES.load(Relaxed) == 8).await;

            // Home runs: the library's counts, the newest albums, the albums
            // played last and their artists, the listening stats, then the
            // playlists made for the user and the saved stations.
            ui.set_listenbrainz_user("joe".into());
            ui.window().set_size(ALL_OF_HOME);
            wait_for("the saved stations", || ui.get_saved_stations().row_count() > 0).await;
            let sections = [
                "ARTISTS",
                "Recently added",
                "Last played albums",
                "Last listened artists",
                "Your listening",
                "Created for you",
                "Saved stations",
            ];
            let tops: Vec<f32> = (sections.iter())
                .map(|section| text_top(&ui, section).unwrap_or_else(|| panic!("no {section}")))
                .collect();
            assert!(tops.is_sorted(), "{tops:?}");
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            ui.set_listenbrainz_user("".into());

            // Every track is listed in ListenBrainz's order, with the songs the
            // library has, found at once by Home: by recording, or by title and
            // artist whatever the accents. The others are greyed.
            SEARCHES.store(0, Relaxed);
            let asked = LISTENBRAINZ_ASKED.lock().unwrap().len();
            ui.invoke_open("createdfor:daily".into());
            wait_for("the playlist", || ui.get_tracks().row_count() == 4).await;
            assert_eq!(ui.get_header_title(), "Daily Jams for joe, 2026-11-04 Wed");
            assert!(!ui.get_created_for_matching());
            // The page names its kind and tints as its tile does; the tile
            // has no banner, so neither has the page.
            assert_eq!(ui.get_created_for_kind(), "Daily Jams");
            assert_eq!(ui.get_created_for_tint(), KindTint::Blue);
            assert_eq!(ui.get_created_for_banner().size().width, 0);
            let banner = mix_banner(&ui).expect("the banner");
            assert_eq!(banner.accessible_label().as_deref(), Some("Daily Jams"));
            assert_eq!(lit(&ui), "Home", "a playlist made for you belongs to Home");
            let rows: Vec<(String, bool)> = (ui.get_tracks().iter())
                .map(|track| (track.title.to_string(), track.unavailable))
                .collect();
            assert_eq!(
                rows,
                [
                    ("Familiar".to_owned(), false),
                    ("Café del Mar".to_owned(), false),
                    ("Not Here".to_owned(), true),
                    ("Familiar".to_owned(), true)
                ]
            );
            assert_eq!(ui.get_header_facts(), "2 of 4 tracks in your library");
            // Its tracks are those the shelf kept: ListenBrainz isn't asked.
            assert_eq!(LISTENBRAINZ_ASKED.lock().unwrap().len(), asked);
            // The page asks the library again all the same, a search for a
            // track it has and two for one it hasn't, and with nothing new
            // found stays as it was.
            wait_for("the tracks looked up anew", || SEARCHES.load(Relaxed) == 6).await;
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_header_facts(), "2 of 4 tracks in your library");

            // A greyed row can't be selected, by a click or in a range.
            assert_eq!(button(&ui, "Not Here").accessible_enabled(), Some(false));
            assert_eq!(button(&ui, "Café del Mar").accessible_enabled(), Some(true));
            ui.invoke_select_track(0, false);
            click(&ui, "Not Here");
            assert_eq!(ui.get_selected_tiles(), 1);
            click(&ui, "Café del Mar");
            assert_eq!(ui.get_selected_tiles(), 2);
            ui.invoke_select_track(3, true);
            let picked: Vec<bool> = ui.get_tracks().iter().map(|track| track.selected).collect();
            assert_eq!(picked, [true, true, false, false]);
            ui.invoke_clear_selection();

            // Save makes a playlist of the songs the library has, in order.
            let sent = writes().len();
            click(&ui, "Save to library");
            wait_for("the playlist saved", || ui.get_notice() != "").await;
            assert_eq!(
                ui.get_notice(),
                "Saved Daily Jams for joe, 2026-11-04 Wed with 2 of 4 tracks."
            );
            assert_eq!(
                writes()[sent..],
                ["createPlaylist name=Daily+Jams+for+joe%2C+2026-11-04+Wed&songId=lb1&songId=lb2"]
            );
            ui.set_notice("".into());

            // A song the library gains while the app runs shows when the
            // playlist is opened again: the page has the songs found before at
            // once, asks the library again, and changes once every track is
            // looked up, never track by track, here once the last, slow to
            // look up, is.
            NOT_HERE_ADDED.store(true, Relaxed);
            CHER_SLOW.store(true, Relaxed);
            let searched = SEARCHES.load(Relaxed);
            ui.invoke_open("home".into());
            ui.invoke_open("createdfor:daily".into());
            wait_for("the songs found before", || ui.get_tracks().row_count() == 4).await;
            let is_not_here_greyed = || ui.get_tracks().row_data(2).unwrap().unavailable;
            assert!(is_not_here_greyed());
            wait_for("the last track asked for", || SEARCHES.load(Relaxed) == searched + 4).await;
            assert!(is_not_here_greyed(), "not track by track");
            wait_for("all looked up", || !is_not_here_greyed()).await;
            assert_eq!(SEARCHES.load(Relaxed), searched + 5);
            assert_eq!(ui.get_header_facts(), "3 of 4 tracks in your library");
            NOT_HERE_ADDED.store(false, Relaxed);
            CHER_SLOW.store(false, Relaxed);
            // What it found is kept: opened again while the library can't be
            // searched, the page has it at once, and keeps it.
            SEARCH_DOWN.store(true, Relaxed);
            ui.invoke_open("home".into());
            ui.invoke_open("createdfor:daily".into());
            wait_for("the songs found last", || ui.get_tracks().row_count() == 4).await;
            assert!(!is_not_here_greyed());
            idle(Duration::from_millis(300)).await;
            assert!(!is_not_here_greyed());
            assert_eq!(ui.get_page_problem(), "");
            SEARCH_DOWN.store(false, Relaxed);

            // A name the server already has is left alone.
            ui.invoke_open("createdfor:trip".into());
            wait_for("the second playlist found", || {
                ui.get_tracks().row_count() == 1 && !ui.get_created_for_matching()
            })
            .await;
            // The page has the banner its tile has, under the same tint.
            let tile = ui.get_created_for_you().row_data(1).unwrap();
            assert!(tile.cover.size().width > 0);
            assert_eq!(ui.get_created_for_kind(), tile.title);
            assert_eq!(ui.get_created_for_tint(), KindTint::Green);
            assert_eq!(ui.get_created_for_banner().size(), tile.cover.size());
            let sent = writes().len();
            click(&ui, "Save to library");
            wait_for("the name taken said", || ui.get_notice() != "").await;
            assert_eq!(
                ui.get_notice(),
                "You already have a playlist named Road Trip, so nothing was saved."
            );
            assert_eq!(writes().len(), sent);
            ui.set_notice("".into());

            ui.invoke_open("createdfor:later".into());
            wait_for("the third playlist", || {
                ui.get_header_title() == "Later Jams for joe" && ui.get_tracks().row_count() == 1
            })
            .await;
            assert_eq!(button(&ui, "Save to library").accessible_enabled(), Some(true));
            let sent = writes().len();
            // A playlist of its name made elsewhere since the playlists were
            // asked for is found too: the server is asked.
            LATER_JAMS_ELSEWHERE.store(true, Relaxed);
            click(&ui, "Save to library");
            wait_for("the name taken elsewhere said", || ui.get_notice() != "").await;
            assert_eq!(
                ui.get_notice(),
                "You already have a playlist named Later Jams for joe, so nothing was saved."
            );
            assert_eq!(writes().len(), sent);
            ui.set_notice("".into());
            LATER_JAMS_ELSEWHERE.store(false, Relaxed);
            click(&ui, "Save to library");
            wait_for("the resumed playlist saved", || ui.get_notice() != "").await;
            assert_eq!(ui.get_notice(), "Saved Later Jams for joe with 1 of 1 tracks.");
            assert_eq!(writes()[sent..], ["createPlaylist name=Later+Jams+for+joe&songId=lb5"]);
            ui.set_notice("".into());

            // A playlist of the library has no banner, though the one before it
            // had.
            assert!(mix_banner(&ui).is_some());
            ui.invoke_open("playlist:p1".into());
            wait_for("the library's playlist", || {
                ui.get_page() == Page::Playlist && !ui.get_created_for()
            })
            .await;
            assert!(mix_banner(&ui).is_none());
            assert!(header_has_cover(&ui), "a library playlist keeps its cover");

            // A playlist not on the shelf has no songs kept: its tracks are
            // greyed, and Save is off, until all are looked up. A search that
            // fails stops that, so a part of the playlist is never saved; Try
            // again goes on.
            SEARCH_DOWN.store(true, Relaxed);
            ui.invoke_open("createdfor:exploring".into());
            wait_for("the failed search said", || {
                ui.get_page_problem().starts_with("Your server didn't answer")
            })
            .await;
            assert_eq!(ui.get_header_title(), "Weekly Exploration for joe");
            assert!(ui.get_created_for_matching());
            assert_eq!(button(&ui, "Save to library").accessible_enabled(), Some(false));
            // Home has no tile for it, so it has no banner either.
            assert_eq!(ui.get_created_for_kind(), "");
            assert!(mix_banner(&ui).is_none());
            let sent = writes().len();
            ui.invoke_save_created_for();
            idle(Duration::from_millis(200)).await;
            assert_eq!((writes().len(), ui.get_notice().as_str()), (sent, ""));
            SEARCH_DOWN.store(false, Relaxed);
            click(&ui, "Try again");
            wait_for("the looking up resumed", || !ui.get_created_for_matching()).await;
            assert_eq!(ui.get_page_problem(), "");
            assert!(ui.get_tracks().row_data(0).unwrap().unavailable);
            // None of its tracks are the library's, so it saves nothing.
            click(&ui, "Save to library");
            wait_for("the nothing to save said", || ui.get_notice() != "").await;
            assert_eq!(ui.get_notice(), "None of its tracks are in your library.");
            assert_eq!(writes().len(), sent);
            ui.set_notice("".into());

            // The shelf is kept on this device: within the hour Home asks
            // ListenBrainz nothing, so one that is down goes unseen.
            let asked = || LISTENBRAINZ_ASKED.lock().unwrap().clone();
            let playlist_fetches = || {
                (asked().iter())
                    .filter(|line| line.starts_with("GET /1/playlist/"))
                    .count()
            };
            let fetched = playlist_fetches();
            let before = asked().len();
            LISTENBRAINZ_DOWN.store(true, Relaxed);
            ui.invoke_open("home".into());
            idle(Duration::from_millis(400)).await;
            assert_eq!(asked().len(), before);
            assert_eq!(ui.get_created_for_you().row_count(), 3);
            assert_eq!(ui.get_page_problem(), "");
            // An hour on, the list is asked for again; while ListenBrainz is
            // down the tiles kept stay, with their banners, and nothing is
            // said.
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            ui.invoke_show_more();
            wait_for("the list asked for again", || asked().len() > before).await;
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_created_for_you().row_count(), 3);
            wait_for("the banner kept", || covers()[1]).await;
            assert_eq!(covers(), [false, true, false]);
            assert_eq!(ui.get_page_problem(), "");
            // Back up, the list is kept anew, and the playlists on it are not
            // fetched again while ListenBrainz says they haven't changed.
            LISTENBRAINZ_DOWN.store(false, Relaxed);
            ui.invoke_show_more();
            wait_for("the list kept anew", || asked().len() == before + 2).await;
            idle(Duration::from_millis(400)).await;
            assert_eq!(asked()[before + 1], "GET /1/user/joe/playlists/createdfor HTTP/1.1 token=true");
            assert_eq!(playlist_fetches(), fetched);
            // One it changed is fetched again with the list, an hour on.
            PLAYLISTS_CHANGED.lock().unwrap().push("daily");
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            ui.invoke_show_more();
            wait_for("the changed playlist fetched", || playlist_fetches() > fetched).await;
            idle(Duration::from_millis(400)).await;
            assert_eq!(
                asked()[before + 2..],
                [
                    "GET /1/user/joe/playlists/createdfor HTTP/1.1 token=true",
                    "GET /1/playlist/daily HTTP/1.1 token=true"
                ]
            );
            assert_eq!(covers(), [false, true, false]);
            let (fetched, listed) = (playlist_fetches(), asked().len());
            // The library's artists, asked for again, give the tile the banner
            // they now have; ListenBrainz is asked for the list again too.
            BANNER_CHANGED.store(true, Relaxed);
            ui.invoke_refresh();
            wait_for("the new banner asked for", || {
                BANNERS_ASKED.lock().unwrap().iter().any(|banner| banner == "ba-r1b")
            })
            .await;
            BANNER_CHANGED.store(false, Relaxed);
            wait_for("the list asked for on refresh", || asked().len() > listed).await;
            idle(Duration::from_millis(400)).await;
            assert_eq!(
                asked()[listed..],
                ["GET /1/user/joe/playlists/createdfor HTTP/1.1 token=true"]
            );
            assert_eq!(playlist_fetches(), fetched);

            // Removing the token forgets what ListenBrainz answered, on this
            // device too: with a token set again and ListenBrainz down, the
            // shelf has nothing to show and says why.
            let joes_listenbrainz = listenbrainz_kept_for(&server, "joe");
            idle(Duration::from_millis(400)).await;
            wait_for("ListenBrainz's answers saved", || {
                std::fs::read_to_string(&joes_listenbrainz)
                    .is_ok_and(|kept| kept.contains("createdfor:joe"))
            })
            .await;
            ui.set_listenbrainz_user("joe".into());
            ui.invoke_open("settings".into());
            click(&ui, "ListenBrainz");
            click(&ui, "Remove token");
            wait_for("the token removed", || ui.get_listenbrainz_user() == "").await;
            wait_for("what ListenBrainz answered forgotten", || {
                std::fs::read_to_string(&joes_listenbrainz)
                    .map_or(true, |kept| !kept.contains("createdfor:joe"))
            })
            .await;
            store::save_listenbrainz(ListenBrainz { token: "tok".into(), user: "joe".into() })
                .await
                .unwrap();
            LISTENBRAINZ_DOWN.store(true, Relaxed);
            ui.invoke_open("home".into());
            wait_for("the shelf's problem", || {
                ui.get_page_problem().starts_with("Created for you couldn't be fetched")
            })
            .await;
            assert_eq!(ui.get_created_for_you().row_count(), 0);
            LISTENBRAINZ_DOWN.store(false, Relaxed);
            ui.invoke_show_more();
            wait_for("the shelf again", || ui.get_created_for_you().row_count() == 3).await;
            wait_for("its playlists fetched anew", || playlist_fetches() == fetched + 3).await;
            assert_eq!(ui.get_page_problem(), "");

            // After a restart the shelf and its banners show at once from
            // what is kept on this device: ListenBrainz, down, isn't asked.
            idle(Duration::from_millis(400)).await;
            wait_for("ListenBrainz's answers saved", || {
                std::fs::read_to_string(&joes_listenbrainz)
                    .is_ok_and(|kept| kept.contains("createdfor:joe"))
            })
            .await;
            LISTENBRAINZ_DOWN.store(true, Relaxed);
            let before = asked().len();
            let restarted = crate::start().unwrap();
            restarted.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            let restarted_covers = || -> Vec<bool> {
                (restarted.get_created_for_you().iter())
                    .map(|tile| tile.cover.size().width > 0)
                    .collect()
            };
            wait_for("the shelf kept, after a restart", || {
                restarted.get_created_for_you().row_count() == 3
            })
            .await;
            wait_for("its banner kept", || restarted_covers()[1]).await;
            assert_eq!(restarted_covers(), [false, true, false]);
            let asked_since = asked()[before..].to_vec();
            assert!(!asked_since.iter().any(|line| line.contains("/playlist")), "{asked_since:?}");
            assert_eq!(restarted.get_page_problem(), "");
            // The library's songs of the playlists are kept too, as Home found
            // them once the token came back: a playlist's rows play them at
            // once, with the library's search down.
            SEARCH_DOWN.store(true, Relaxed);
            restarted.invoke_open("createdfor:later".into());
            wait_for("the songs kept, after a restart", || {
                restarted.get_header_title() == "Later Jams for joe"
            })
            .await;
            assert!(!restarted.get_created_for_matching());
            assert!(!restarted.get_tracks().row_data(0).unwrap().unavailable);
            assert_eq!(button(&restarted, "Salt").accessible_enabled(), Some(true));
            idle(Duration::from_millis(200)).await;
            assert_eq!(restarted.get_page_problem(), "", "the songs kept stay");
            SEARCH_DOWN.store(false, Relaxed);
            drop(restarted);
            LISTENBRAINZ_DOWN.store(false, Relaxed);

            // A heart set in the app syncs with ListenBrainz: it sends the
            // hearts it can and stars the song of a love the server lacks,
            // whose heart then shows on the page, with the rows still there.
            let searched = SEARCHES.load(Relaxed);
            ui.invoke_open("createdfor:daily".into());
            wait_for("the playlist to heart", || {
                ui.get_tracks().row_count() == 4 && SEARCHES.load(Relaxed) == searched + 6
            })
            .await;
            idle(Duration::from_millis(300)).await;
            assert!(!ui.get_tracks().row_data(0).unwrap().loved);
            ui.invoke_toggle_love(1);
            wait_for("the loved song starred", || {
                STARRED.lock().unwrap().iter().any(|id| id == "lb1")
            })
            .await;
            wait_for("its heart shown", || ui.get_tracks().row_data(0).unwrap().loved).await;
            assert_eq!(ui.get_tracks().row_count(), 4);
            assert!(
                LISTENBRAINZ_ASKED
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|asked| asked.starts_with("GET /1/feedback/user/joe/get-feedback"))
            );

            // A play heard while the server can't be reached waits on this
            // device, and goes once the server answers again, at the next song
            // that begins, with the time it began; it is logged at once, at
            // that time. With a token too, plays go to the server alone,
            // which tells ListenBrainz. The history's steps below start from
            // an empty log.
            let submits = || {
                (LISTENBRAINZ_ASKED.lock().unwrap().iter())
                    .filter(|asked| asked.starts_with("POST /1/submit-listens"))
                    .count()
            };
            play::testing::clear(&ui);
            let plays_told = told().len();
            SCROBBLE_DOWN.store(true, Relaxed);
            let tide: opensubsonic::Child = serde_json::from_str(
                r#"{"id":"t2","isDir":false,"title":"Tide","albumId":"a1","duration":2}"#,
            )
            .unwrap();
            let began = since_1970();
            for position in [0.0, 0.4, 0.8, 1.2] {
                play::testing::hear(&tide, position);
                idle(Duration::from_millis(400)).await;
            }
            wait_for("the play kept", || !prefs::waiting_plays(&joe).is_empty()).await;
            let kept = prefs::waiting_plays(&joe);
            assert_eq!(kept.len(), 1, "{kept:?}");
            let time = kept[0].started_at as u128 * 1000;
            assert!(time <= began + 100 && time + 1000 > began, "played from {time}, began {began}");
            assert_eq!(logged_at("t2").await, time);
            assert_eq!(told().len(), plays_told);
            SCROBBLE_DOWN.store(false, Relaxed);
            let announced = playing_now().len();
            play::testing::hear(&harbour, 0.0);
            wait_for("the play kept told", || {
                prefs::waiting_plays(&joe).is_empty() && playing_now().len() == announced + 1
            })
            .await;
            assert_eq!(told().len(), plays_told + 1);
            assert_eq!(time_told(&told()[plays_told], "t2"), time);
            assert_eq!(playing_now()[announced], "scrobble id=t1&submission=false");
            assert_eq!(submits(), 0);
            plays::forget(&joe).unwrap();

            // A play kept for one account waits for its own server: another
            // account's play goes without it. It goes at the next start or
            // sign in to its own account, before any song begins.
            play::testing::clear(&ui);
            SCROBBLE_DOWN.store(true, Relaxed);
            for position in [0.0, 0.4, 0.8, 1.2] {
                play::testing::hear(&tide, position);
                idle(Duration::from_millis(400)).await;
            }
            wait_for("the play kept", || !prefs::waiting_plays(&joe).is_empty()).await;
            let time = prefs::waiting_plays(&joe)[0].started_at as u128 * 1000;
            SCROBBLE_DOWN.store(false, Relaxed);
            let plays_told = told().len();
            let another = "another account";
            for position in [0.0, 0.4, 0.8, 1.2] {
                play::testing::hear_for(another, &harbour, position);
                idle(Duration::from_millis(400)).await;
            }
            wait_for("the other account's play told", || {
                told().len() > plays_told && prefs::waiting_plays(another).is_empty()
            })
            .await;
            assert_eq!(told().len(), plays_told + 1, "{:?}", told());
            assert!(told()[plays_told].starts_with("scrobble id=t1&"), "{:?}", told());
            assert_eq!(prefs::waiting_plays(&joe).len(), 1);
            let announced = playing_now().len();
            play::testing::restore(&ui);
            wait_for("the kept play told at the start", || {
                prefs::waiting_plays(&joe).is_empty()
            })
            .await;
            assert_eq!(told().len(), plays_told + 2);
            assert_eq!(time_told(&told()[plays_told + 1], "t2"), time);
            assert_eq!(playing_now().len(), announced, "no song began");
            plays::forget(&joe).unwrap();

            // The history is in Browse, right after Radio.
            let browse_group = |ui: &AppWindow| -> Vec<String> {
                (ui.get_sections().iter().zip(ui.get_section_groups().iter()))
                    .filter(|(_, group)| group == "Browse")
                    .map(|(label, _)| label.to_string())
                    .collect()
            };
            let listed = browse_group(&ui);
            let radio = listed.iter().position(|label| label == "Radio").unwrap();
            assert_eq!(listed[radio + 1], "History");
            assert!(ui.get_browse_links().iter().any(|entry| entry.link == "history"));
            assert!(!ui.get_library_links().iter().any(|entry| entry.link == "history"));

            // Its base is this device's own plays: one heard only here,
            // after all of joe's listens, and one sent to ListenBrainz,
            // which it has as one of his listens too.
            let joes_plays = plays_kept_for(&server, "joe");
            let here_only = opensubsonic::Child {
                id: "here1".into(),
                title: "Played Here".into(),
                artist: Some("Mara Lind".into()),
                ..opensubsonic::Child::default()
            };
            let sent = opensubsonic::Child {
                id: "lb5".into(),
                title: "Salt".into(),
                artist: Some("Mara Lind".into()),
                music_brainz_id: Some("00000000-0000-4000-8000-000000000005".into()),
                ..opensubsonic::Child::default()
            };
            let joe = account_of(&server, "joe");
            plays::append(&joe, &plays::Play::of(&sent, joe_listened_at(1))).unwrap();
            plays::append(&joe, &plays::Play::of(&here_only, JOE_LISTENED_LAST + 60)).unwrap();
            ui.set_listenbrainz_user("joe".into());
            idle(Duration::from_millis(20)).await;

            // Its first page has the newest hundred, the plays here among
            // joe's listens. The plays here play at once; his listens are
            // greyed and light up only once the whole page is looked up,
            // so a search that fails leaves even a listen found before
            // greyed; Try again goes on.
            SEARCH_DOWN.store(true, Relaxed);
            click(&ui, "History");
            wait_for("the failed search said", || {
                ui.get_page_problem().starts_with("Your server didn't answer")
            })
            .await;
            assert_eq!(ui.get_page(), Page::History);
            assert_eq!(lit(&ui), "History");
            assert_eq!(ui.get_bar_section(), 1, "the history lights Browse");
            assert_eq!(ui.get_tracks().row_count(), 100);
            let available = |ui: &AppWindow| -> Vec<bool> {
                ui.get_tracks().iter().map(|track| !track.unavailable).collect()
            };
            assert_eq!(available(&ui)[..4], [true, false, true, false]);
            assert_eq!(available(&ui).iter().filter(|&&lit| lit).count(), 2);
            // They are kept on this device, with how many ListenBrainz counts.
            let asked_for_history = asked();
            assert_eq!(
                asked_for_history[asked_for_history.len() - 2..],
                [
                    "GET /1/user/joe/listens?count=100 HTTP/1.1 token=true",
                    "GET /1/user/joe/listen-count HTTP/1.1 token=true"
                ]
            );
            SEARCH_DOWN.store(false, Relaxed);
            let searched = SEARCHES.load(Relaxed);
            // The problem, Try again and the pages' buttons are under the
            // hundred rows.
            let to_the_end = |ui: &AppWindow| {
                scroll_page(ui, -TOP_OF_PAGE);
                idle(Duration::from_millis(50))
            };
            to_the_end(&ui).await;
            let before = asked().len();
            click(&ui, "Try again");
            // The listens kept show at once, without asking ListenBrainz.
            wait_for("the listens found", || {
                ui.get_tracks().row_count() == 100
                    && ui.get_tracks().row_data(1).is_some_and(|track| !track.unavailable)
            })
            .await;
            assert_eq!((ui.get_page_problem().as_str(), ui.get_header_facts().as_str()), ("", ""));
            assert_eq!(asked().len(), before);
            let rows: Vec<(String, bool)> = (ui.get_tracks().iter())
                .map(|track| (track.title.to_string(), track.unavailable))
                .collect();
            assert_eq!(rows.len(), 100);
            assert_eq!(
                rows[..6],
                [
                    ("Played Here".to_owned(), false),
                    ("Familiar".to_owned(), false),
                    ("Salt".to_owned(), false),
                    ("Gone".to_owned(), true),
                    ("Café del Mar".to_owned(), false),
                    ("Familiar".to_owned(), false)
                ]
            );
            // When it was heard, where other rows have how long they last.
            assert!(!ui.get_tracks().row_data(0).unwrap().duration.contains(':'));
            // A track matched before costs no search; one the library has,
            // one; one it lacks, two; each once for the whole page. A play
            // here takes none.
            assert_eq!(SEARCHES.load(Relaxed) - searched, 3);
            assert_eq!(button(&ui, "Gone").accessible_enabled(), Some(false));

            // A row plays with the page's other rows the library has; the
            // listens it lacks are left out.
            click(&ui, "Café del Mar");
            let (queued, _) = play::testing::queued_ids();
            assert_eq!(queued.len(), 75);
            assert_eq!(queued[..5], ["here1", "lb1", "lb5", "lb2", "lb1"]);
            play::testing::clear(&ui);

            // Next has what was heard before the oldest on the page, and
            // Previous what was heard after the newest; neither is offered
            // without. Next asks for the page before the listens kept, which
            // are then kept with them; Previous is among those kept.
            to_the_end(&ui).await;
            assert_eq!(button(&ui, "Previous page").accessible_enabled(), Some(false));
            click(&ui, "Next page");
            wait_for("the older listens found", || {
                ui.get_tracks().row_count() == 51
                    && ui.get_tracks().row_data(0).is_some_and(|track| !track.unavailable)
            })
            .await;
            assert_eq!(
                *asked().last().unwrap(),
                format!("GET /1/user/joe/listens?count=100&max_ts={} HTTP/1.1 token=true", joe_listened_at(99))
            );
            assert!(ui.get_tracks().row_data(3).unwrap().unavailable);
            to_the_end(&ui).await;
            assert_eq!(button(&ui, "Next page").accessible_enabled(), Some(false));
            assert_eq!(button(&ui, "Previous page").accessible_enabled(), Some(true));
            let before = asked().len();
            click(&ui, "Previous page");
            wait_for("the newer listens found", || {
                ui.get_tracks().row_count() == 100
                    && ui.get_tracks().row_data(1).is_some_and(|track| !track.unavailable)
            })
            .await;
            assert_eq!(asked().len(), before);
            assert_eq!(ui.get_tracks().row_data(0).unwrap().title, "Played Here");
            to_the_end(&ui).await;
            assert_eq!(button(&ui, "Previous page").accessible_enabled(), Some(false));
            assert_eq!(SEARCHES.load(Relaxed) - searched, 3, "the pages again take no search");

            // An hour on, a visit shows the listens kept at once and asks
            // only for those since the newest of them, and for the count;
            // while ListenBrainz is down they stay, and nothing is said.
            let before = asked().len();
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            LISTENBRAINZ_DOWN.store(true, Relaxed);
            ui.invoke_open("history".into());
            wait_for("the listens since asked for", || asked().len() > before).await;
            wait_for("the listens kept", || !ui.get_loading()).await;
            assert_eq!(ui.get_tracks().row_count(), 100);
            assert_eq!(ui.get_tracks().row_data(0).unwrap().title, "Played Here");
            assert_eq!(ui.get_page_problem(), "");
            LISTENBRAINZ_DOWN.store(false, Relaxed);
            // Back up, the listen joe heard since goes on top of them.
            JOE_NEW_LISTEN.store(true, Relaxed);
            ui.invoke_open("settings".into());
            ui.invoke_open("history".into());
            wait_for("the listen since on top", || {
                ui.get_tracks().row_data(0).is_some_and(|track| track.title == "Café del Mar")
            })
            .await;
            assert_eq!(
                asked()[before + 1..],
                [
                    format!("GET /1/user/joe/listens?count=1000&min_ts={JOE_LISTENED_LAST} HTTP/1.1 token=true"),
                    "GET /1/user/joe/listen-count HTTP/1.1 token=true".to_owned()
                ]
            );
            assert_eq!(ui.get_tracks().row_data(1).unwrap().title, "Played Here");
            // When ListenBrainz counts other listens than those kept and
            // those since, as when one came late with an older time, the
            // newest page takes the place of those kept: the page before it
            // is asked for again.
            JOE_LATE_LISTEN.store(true, Relaxed);
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            let before = asked().len();
            ui.invoke_open("settings".into());
            ui.invoke_open("history".into());
            wait_for("the newest page asked for", || asked().len() == before + 3).await;
            assert_eq!(
                asked()[before..],
                [
                    format!(
                        "GET /1/user/joe/listens?count=1000&min_ts={} HTTP/1.1 token=true",
                        JOE_LISTENED_LAST + JOE_LISTENS_APART
                    ),
                    "GET /1/user/joe/listen-count HTTP/1.1 token=true".to_owned(),
                    "GET /1/user/joe/listens?count=100 HTTP/1.1 token=true".to_owned()
                ]
            );
            wait_for("the newest page", || !ui.get_loading()).await;
            to_the_end(&ui).await;
            click(&ui, "Next page");
            wait_for("the page before asked for", || asked().len() == before + 4).await;
            assert_eq!(
                asked()[before + 3],
                format!("GET /1/user/joe/listens?count=100&max_ts={} HTTP/1.1 token=true", joe_listened_at(98))
            );
            JOE_NEW_LISTEN.store(false, Relaxed);
            JOE_LATE_LISTEN.store(false, Relaxed);
            ui.invoke_open("history".into());
            wait_for("the newest page again", || {
                !ui.get_loading() && ui.get_tracks().row_count() == 100
            })
            .await;
            // A play the server takes passes on to ListenBrainz, so the next
            // visit asks for the listens since at once, within the hour. It
            // is heard for joe without being logged here, which would change
            // the plays this device kept.
            let low_tide: opensubsonic::Child = serde_json::from_str(
                r#"{"id":"t3","isDir":false,"title":"Low Tide","albumId":"a1","duration":2}"#,
            )
            .unwrap();
            let (before, plays_told) = (asked().len(), SCROBBLES.lock().unwrap().len());
            for position in [0.0, 0.4, 0.8, 1.2] {
                play::testing::hear_for(&joe, &low_tide, position);
                idle(Duration::from_millis(400)).await;
            }
            wait_for("the play taken", || SCROBBLES.lock().unwrap().len() > plays_told).await;
            assert_eq!(asked().len(), before);
            ui.invoke_open("settings".into());
            ui.invoke_open("history".into());
            wait_for("the listens since asked for", || asked().len() > before).await;
            assert_eq!(
                asked()[before],
                format!(
                    "GET /1/user/joe/listens?count=1000&min_ts={} HTTP/1.1 token=true",
                    JOE_LISTENED_LAST + JOE_LISTENS_APART
                )
            );
            idle(Duration::from_millis(500)).await;

            // Without the token the page keeps the plays here alone, and
            // ListenBrainz isn't asked.
            store::forget_listenbrainz().await.unwrap();
            let asked = LISTENBRAINZ_ASKED.lock().unwrap().len();
            ui.set_listenbrainz_user("".into());
            wait_for("the plays here alone", || ui.get_tracks().row_count() == 2).await;
            let rows: Vec<(String, bool)> = (ui.get_tracks().iter())
                .map(|track| (track.title.to_string(), track.unavailable))
                .collect();
            assert_eq!(rows, [("Played Here".to_owned(), false), ("Salt".to_owned(), false)]);
            assert_eq!(LISTENBRAINZ_ASKED.lock().unwrap().len(), asked);
            assert!(browse_group(&ui).contains(&"History".to_owned()));

            store::forget_listenbrainz().await.unwrap();
            ui.invoke_open("settings".into());

            let kept = ListenBrainz {
                token: "kept token".into(),
                user: "listener".into(),
            };
            store::save_listenbrainz(kept.clone()).await.unwrap();
            ui.set_listenbrainz_user("listener".into());
            // A play the server can't be told of is tried once more as the
            // account signs out, and then goes with it.
            play::testing::clear(&ui);
            SCROBBLE_DOWN.store(true, Relaxed);
            let turned_away = PLAYS_TURNED_AWAY.load(Relaxed);
            for position in [0.0, 0.4, 0.8, 1.2] {
                play::testing::hear(&tide, position);
                idle(Duration::from_millis(400)).await;
            }
            wait_for("the play kept", || {
                !prefs::waiting_plays(&joe).is_empty()
                    && PLAYS_TURNED_AWAY.load(Relaxed) > turned_away
            })
            .await;
            // Its send is over once the refusal is back.
            idle(Duration::from_millis(300)).await;
            let turned_away = PLAYS_TURNED_AWAY.load(Relaxed);
            // Lists on their way when the account signs out don't keep the
            // next sign in from loading its own.
            ui.invoke_pick_stat_period(3);
            assert!(ui.get_stat_loading());
            click(&ui, "Account");
            click(&ui, "Sign out");
            assert_eq!(ui.get_sign_out_sending(), "Sending 1 play…");
            // Signing out keeps the server, and asks for the login alone.
            wait_for("the sign-in step", || ui.get_step() == Step::SignIn).await;
            assert_eq!(ui.get_sign_out_sending(), "");
            assert_eq!(PLAYS_TURNED_AWAY.load(Relaxed), turned_away + 1, "tried once more");
            assert!(prefs::waiting_plays(&joe).is_empty(), "the play kept went with the account");
            SCROBBLE_DOWN.store(false, Relaxed);
            assert!(!joes_plays.exists(), "the plays logged went with the account");
            assert!(!joes_listenbrainz.exists(), "and what ListenBrainz answered");
            assert_eq!(ui.get_username(), "");
            assert_eq!(ui.get_address(), server.as_str());
            assert!(store::load().await.is_none());
            assert_eq!(store::load_server().await, Some(server.clone()));
            store::forget().await.expect("forgetting twice is fine");
            // It keeps the token too, and so does signing in as someone
            // else, which doesn't ask for one.
            assert_eq!(token(store::load_listenbrainz().await), Some(kept.token.clone()));
            // Her server counts no plays of its composers and compositions,
            // as Fonoteca's first version, where they were sorted by them
            // before, and the composers turned around.
            OLD_COMPOSITIONS.store(true, Relaxed);
            let mut saved = crate::prefs::load();
            (saved.composer_sort, saved.composer_descending) = (0, true);
            saved.composition_columns.as_mut().unwrap().push("Plays".into());
            (saved.composition_sort, saved.composition_descending) = (Some("Plays".into()), true);
            crate::prefs::save(&saved);
            ui.set_username("eve".into());
            ui.set_password("pw".into());
            click(&ui, "Sign in");
            wait_for("the done step", || ui.get_step() == Step::Done).await;
            wait_for("the account saved", || !ui.get_busy()).await;
            assert_eq!(ui.get_listenbrainz_user(), "listener");
            wait_for("the lists after signing in again", || {
                ui.get_stat_lists().row_count() == 3 && !ui.get_stat_loading()
            })
            .await;
            // The answer of the asks before signing out arrives too, and is
            // not taken for this one's: the period is this year's again.
            idle(Duration::from_millis(700)).await;
            // The artists played last have the library's counts after a sign
            // in too, whichever of the two lists arrived first.
            let first_artist = ui.get_played_artists().row_data(0).unwrap();
            assert_eq!(first_artist.names.row_data(0).unwrap().label, "1 album");
            let first = |ui: &AppWindow| {
                let albums = ui.get_stat_lists().row_data(0).unwrap().entries;
                albums.row_data(0).unwrap().title
            };
            assert_eq!(first(&ui), "Sonatas (Deluxe)");
            assert_eq!(ui.get_stat_period(), 2);
            ui.invoke_show_more();
            assert!(!ui.get_stat_loading());
            assert_eq!(token(store::load_listenbrainz().await), Some(kept.token));
            let signed_in_as = store::load().await.expect("saved").login;
            assert!(matches!(signed_in_as, Login::Password { ref username, .. } if username == "eve"));
            // So the composers page has no Sort menu and lists them in the
            // server's order, and the compositions table has no Plays column
            // to show or sort by, which stay picked for a server that counts
            // them.
            assert!(!ui.get_composer_plays_counted());
            click(&ui, "Composers");
            wait_for("her composers", || ui.get_tiles().row_count() == 2).await;
            assert!(!find_combo(&ui, "Sort by"));
            // Nor on a phone, where the menu goes under the filter.
            ui.window().set_size(slint::LogicalSize::new(360.0, 640.0));
            assert!(!find_combo(&ui, "Sort by"));
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            assert_eq!(composer_links(&ui), ["composer:r9", "composer:r10"]);
            click(&ui, "Compositions");
            wait_for("her compositions", || ui.get_track_lines().row_count() == 3).await;
            let cells_match_titles = |ui: &AppWindow| {
                let columns = titles(ui).len();
                (ui.get_track_lines().iter()).all(|line| line.cells.row_count() == columns)
            };
            assert!(cells_match_titles(&ui));
            assert!(!titles(&ui).contains(&"Plays".to_owned()));
            assert!(!ui.get_track_choices().iter().any(|choice| choice.title == "Plays"));
            assert!(sorted(&ui).is_empty());
            assert_eq!(
                order(&ui),
                ["composition:w9", "composition:w7", "composition:w5"]
            );
            choose(&ui, "Key");
            assert!(cells_match_titles(&ui));
            choose(&ui, "Key");
            let saved = crate::prefs::load();
            assert!(saved.composition_columns.unwrap().contains(&"Plays".to_owned()));
            assert_eq!(saved.composition_sort.as_deref(), Some("Plays"));
            assert_eq!((saved.composer_sort, saved.composer_descending), (0, true));
            OLD_COMPOSITIONS.store(false, Relaxed);
            // Only changing the server forgets it, and the token stays.
            // The downloads belong to the server, and go with it.
            let downloads_folder = crate::offline::folder().unwrap();
            std::fs::create_dir_all(&downloads_folder).unwrap();
            std::fs::write(downloads_folder.join("marker"), b"x").unwrap();
            click(&ui, "Account");
            let eves_answers = answers_kept_for(&server, "eve");
            wait_for("eve's pages kept", || eves_answers.exists()).await;
            let eves_listenbrainz = listenbrainz_kept_for(&server, "eve");
            wait_for("eve's ListenBrainz answers kept", || eves_listenbrainz.exists()).await;
            let eves_plays = plays_kept_for(&server, "eve");
            let eve = account_of(&server, "eve");
            plays::append(&eve, &plays::Play::of(&sent, 1)).unwrap();
            assert!(eves_plays.exists());
            play::testing::clear(&ui);
            SCROBBLE_DOWN.store(true, Relaxed);
            let turned_away = PLAYS_TURNED_AWAY.load(Relaxed);
            // Another song than the one counted last, which would go on.
            for position in [0.0, 0.4, 0.8, 1.2] {
                play::testing::hear(&harbour, position);
                idle(Duration::from_millis(400)).await;
            }
            wait_for("eve's play kept", || {
                !prefs::waiting_plays(&eve).is_empty()
                    && PLAYS_TURNED_AWAY.load(Relaxed) > turned_away
            })
            .await;
            idle(Duration::from_millis(300)).await;
            let turned_away = PLAYS_TURNED_AWAY.load(Relaxed);
            click(&ui, "Change server");
            assert_eq!(ui.get_change_server_sending(), "Sending 1 play…");
            wait_for("the server step", || ui.get_step() == Step::Server).await;
            assert_eq!(PLAYS_TURNED_AWAY.load(Relaxed), turned_away + 1, "tried once more");
            assert!(prefs::waiting_plays(&eve).is_empty(), "the play kept went with the server");
            SCROBBLE_DOWN.store(false, Relaxed);
            assert!(!downloads_folder.exists(), "the downloads went with the server");
            assert!(!eves_answers.exists(), "the pages kept went with the server");
            assert!(!eves_plays.exists(), "the plays logged went with the server");
            assert!(!eves_listenbrainz.exists(), "and what ListenBrainz answered");
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
            // Home was left before its composers came; the search asks for
            // them itself.
            ui.set_search_query("Ludwig".into());
            ui.invoke_open("search".into());
            wait_for("Beethoven found", || ui.get_found_composers().row_count() == 1).await;
            ui.invoke_back();

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
            // held on its frame otherwise, never blanked; on a tablet, whose
            // page always shows it.
            wait_for("the artist's banner", || ui.get_playing_banner().size().width > 0).await;
            ui.window().set_size(slint::LogicalSize::new(800.0, 760.0));
            idle(Duration::from_millis(400)).await;
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
            // A desktop's page shows the artist's banner in its place, and
            // holds it.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            idle(Duration::from_millis(400)).await;
            let frame = art_looks(&ui);
            assert_eq!(art_over(&ui, Duration::from_millis(400)).await, held(frame), "held under the banner");

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

            // A movement of a work plays under the work, with its place in
            // it; a song of no work shows as itself.
            let movement = |id: &str, number: i64| -> opensubsonic::Child {
                serde_json::from_str(&format!(
                    r#"{{"id":"{id}","isDir":false,"title":"Symphony no. 8: {id}","albumId":"a2","works":[{{"name":"Symphony no. 8"}}],"movements":[{{"name":"{id}","number":{number},"count":2}}]}}"#
                ))
                .unwrap()
            };
            let symphony = vec![movement("Allegro", 1), movement("Adagio", 2), song("pop", "a2")];
            play::testing::show_song(&ui, symphony.clone(), 1);
            assert_eq!(ui.get_playing_title(), "Symphony no. 8");
            assert_eq!(ui.get_playing_movement(), "Adagio");
            assert_eq!(ui.get_playing_movement_place(), "2 of 2");
            play::testing::show_song(&ui, symphony, 2);
            assert_eq!(ui.get_playing_title(), "pop");
            assert_eq!(ui.get_playing_movement(), "");
            assert_eq!(ui.get_playing_movement_place(), "");

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

            // The sheet's heart follows the song that plays: a love from a
            // table lands in the queue's copy, so it holds when the song comes
            // round again; a heart the server refuses goes back.
            play::testing::show_song(&ui, vec![song("h1", "a2"), song("h2", "a2")], 0);
            assert!(!ui.get_playing_loved());
            ui.invoke_song_loved("h2".into(), true);
            assert!(!ui.get_playing_loved(), "another song's heart");
            ui.invoke_song_loved("h1".into(), true);
            assert!(ui.get_playing_loved());
            assert_eq!(play::testing::loved_ids(), [true, true]);
            ui.invoke_toggle_playing_love();
            assert!(!ui.get_playing_loved(), "unloved at once");
            assert_eq!(play::testing::loved_ids(), [false, true]);
            wait_for("the refused unstar", || ui.get_playing_loved()).await;
            assert_eq!(play::testing::loved_ids(), [true, true], "refused, so loved again");

            // A desktop's page shows the banner of the artist that plays,
            // the album's cover until it comes, and the song's words; a song
            // without them says so, and one by no artist has the cover.
            let harbour = || -> opensubsonic::Child { serde_json::from_str(HARBOUR_LIGHTS).unwrap() };
            let text_in_sight = |text: &str| {
                let wanted = text.to_owned();
                ui.root_element()
                    .query_descendants()
                    .match_predicate(move |e| e.accessible_label().is_some_and(|l| l == wanted))
                    .find_first()
                    .is_some()
            };
            let words = "Lights on the water\nAnd the boats come home";
            play::testing::show_song(&ui, vec![harbour()], 0);
            assert_eq!(ui.get_playing_lyrics(), "", "the last song's words are gone");
            wait_for("its words", || ui.get_playing_lyrics() == words).await;
            assert!(text_in_sight(words), "a desktop's page shows them");
            wait_for("the artist's banner", || ui.get_playing_banner().size().width > 0).await;
            play::testing::show_song(&ui, vec![song("quiet", "a2")], 0);
            assert_eq!(ui.get_playing_banner().size().width, 0, "no artist, no banner");
            wait_for("the note", || ui.get_playing_lyrics_note() == "No lyrics for this song.").await;
            assert!(text_in_sight("No lyrics for this song."));

            // LRCLIB is asked only once the setting allows it, for a song the
            // server has no words for, and each song only once.
            let by_mara = |title: &str| -> opensubsonic::Child {
                serde_json::from_str(&format!(
                    r#"{{"id":"{title}","isDir":false,"title":"{title}","artist":"Mara Lind","album":"Night Harbour","duration":200}}"#
                ))
                .unwrap()
            };
            let moonlit = "Moon on the water\nUnder the stars";
            play::testing::show_song(&ui, vec![by_mara("Moonlit")], 0);
            wait_for("the note", || ui.get_playing_lyrics_note() == "No lyrics for this song.").await;
            assert_eq!(LRCLIB_ASKED.load(Relaxed), 0, "off, so nothing is sent");
            ui.invoke_choose_look_up_lyrics(true);
            ui.set_playing_position(0.0);
            play::testing::show_song(&ui, vec![by_mara("Moonlit")], 0);
            wait_for("LRCLIB's words", || ui.get_playing_lyrics() == moonlit).await;
            assert_eq!(ui.get_playing_lyric_lines().row_count(), 2, "the synced lines");
            assert_eq!(ui.get_playing_lyric_current(), -1, "none sung before the first");
            for (position, sung) in [(1.0, 0), (4.9, 0), (5.0, 1), (0.5, -1)] {
                ui.set_playing_position(position);
                lyrics::follow(&ui);
                assert_eq!(ui.get_playing_lyric_current(), sung, "sung at {position}");
            }
            assert!(text_in_sight("Under the stars"), "each timed line is a button of its own");
            play::testing::show_song(&ui, vec![harbour()], 0);
            wait_for("the server's words first", || ui.get_playing_lyrics() == words).await;
            assert_eq!(LRCLIB_ASKED.load(Relaxed), 1, "the server had words");
            play::testing::show_song(&ui, vec![by_mara("Moonlit")], 0);
            wait_for("the kept words", || ui.get_playing_lyrics() == moonlit).await;
            assert_eq!(ui.get_playing_lyric_lines().row_count(), 2, "and their times");
            play::testing::show_song(&ui, vec![by_mara("Silent")], 0);
            wait_for("no words found", || ui.get_playing_lyrics_note() == "No lyrics for this song.").await;
            play::testing::show_song(&ui, vec![by_mara("Silent")], 0);
            wait_for("no words kept", || ui.get_playing_lyrics_note() == "No lyrics for this song.").await;
            assert_eq!(LRCLIB_ASKED.load(Relaxed), 2, "one ask per song, even for none");
            ui.invoke_choose_look_up_lyrics(false);

            // On a desktop the bar's cover and album open the album, and an
            // artist's chip the artist's page; so do a phone's page's.
            let lowest = |label: &str| -> ElementHandle {
                let wanted = label.to_owned();
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Button)
                    .match_predicate(move |button| button.accessible_label().is_some_and(|l| l == wanted))
                    .find_all()
                    .into_iter()
                    .max_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y))
                    .unwrap_or_else(|| panic!("no {label} button"))
            };
            play::testing::show_song(&ui, vec![harbour()], 0);
            // From a page they don't lead to.
            ui.invoke_open("albums".into());
            wait_for("the albums", || ui.get_page() == Page::Albums).await;
            let links = [("Album cover", Page::Album), ("Night Harbour", Page::Album), ("Mara Lind", Page::Artist)];
            for (width, height) in [(1100.0, 760.0), (412.0, 915.0)] {
                ui.window().set_size(slint::LogicalSize::new(width, height));
                idle(Duration::from_millis(400)).await;
                for (link, page) in links {
                    if !ui.get_now_playing_open() {
                        click(&ui, "Now playing");
                        idle(Duration::from_millis(400)).await;
                    }
                    assert!(ui.get_now_playing_open());
                    let visit = ui.get_visit();
                    lowest(link).invoke_accessible_default_action();
                    wait_for(&format!("{link}'s page at {width}px"), || ui.get_visit() != visit && ui.get_page() == page).await;
                    assert!(!ui.get_now_playing_open(), "{link} closes Now playing at {width}px");
                    let opened = ui.get_visit();
                    ui.invoke_back();
                    wait_for("the page before", || ui.get_visit() != opened).await;
                    // The page's change closes Now playing once more, after it.
                    idle(Duration::from_millis(400)).await;
                }
            }
            // On a phone and a tablet, Now playing keeps each of its parts in
            // place from song to song: whether the song is part of a work or
            // not, its title long or short, by one artist or by several, whose
            // chips keep to one row. Among works a song that is none keeps the
            // line of the part; in a queue without works there is no such line.
            let on_sheet = |label: &str| -> Option<ElementHandle> {
                let wanted = label.to_owned();
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::Main)
                    .match_predicate(|page| page.accessible_label().is_some_and(|l| l == "Now playing"))
                    .find_first()?
                    .query_descendants()
                    .match_predicate(move |e| e.accessible_label().is_some_and(|l| l == wanted))
                    .find_first()
            };
            let sheet_top = |label: &str| on_sheet(label).map(|e| e.absolute_position().y);
            // How far down the cover, the album, the first artist, the
            // position, the controls and the lyrics' button are.
            let places = || -> Vec<Option<f32>> {
                let album = ui.get_playing_album().label.to_string();
                let artist = ui.get_playing_names().row_data(0).unwrap().label.to_string();
                ["Album cover", album.as_str(), artist.as_str(), "Position", "Previous", "Lyrics"]
                    .map(&sheet_top)
                    .to_vec()
            };
            // How far under the cover the album is.
            let album_under_cover = || {
                let cover = on_sheet("Album cover").unwrap();
                let album = sheet_top(ui.get_playing_album().label.as_str()).unwrap();
                album - cover.absolute_position().y - cover.size().height
            };
            let work = || -> opensubsonic::Child {
                serde_json::from_str(
                    r#"{"id":"w2","isDir":false,"title":"Sonata: Andante","album":"Symphonies","albumId":"a2","artists":[{"id":"r1","name":"Mara Lind"},{"id":"r2","name":"Ove Strand"},{"id":"r3","name":"Anna Bach"},{"id":"r4","name":"The Harbour Town Chamber Orchestra"}],"works":[{"name":"Sonata"}],"movements":[{"name":"Andante","number":2,"count":3}]}"#,
                )
                .unwrap()
            };
            let long_song = || {
                let mut song = harbour();
                song.title = "Harbour Lights over the water, all the way home again".into();
                song
            };
            for (width, height) in [(412.0, 915.0), (800.0, 1280.0)] {
                ui.window().set_size(slint::LogicalSize::new(width, height));
                idle(Duration::from_millis(400)).await;
                if !ui.get_now_playing_open() {
                    click(&ui, "Now playing");
                    idle(Duration::from_millis(400)).await;
                }
                play::testing::show_song(&ui, vec![work(), long_song()], 0);
                idle(Duration::from_millis(100)).await;
                let among_works = places();
                assert!(among_works.iter().all(Option::is_some), "all in sight at {width}px: {among_works:?}");
                let with_part_line = album_under_cover();
                play::testing::show_song(&ui, vec![work(), long_song()], 1);
                idle(Duration::from_millis(100)).await;
                assert_eq!(places(), among_works, "a song among works at {width}px");
                play::testing::show_song(&ui, vec![harbour()], 0);
                idle(Duration::from_millis(100)).await;
                assert!(album_under_cover() < with_part_line, "no line for a part over the album at {width}px");
            }
            // The queue's button stays wholly in sight under the controls, on
            // a phone and a tablet, with a work too: the cover gives way.
            play::testing::show_song(&ui, vec![work(), long_song()], 0);
            for (width, height) in [(360.0, 800.0), (412.0, 915.0), (600.0, 960.0), (800.0, 1280.0)] {
                ui.window().set_size(slint::LogicalSize::new(width, height));
                idle(Duration::from_millis(400)).await;
                let queue = on_sheet("Queue").unwrap_or_else(|| panic!("no queue's button in sight at {width}x{height}"));
                let bottom = queue.absolute_position().y + queue.size().height;
                assert!(bottom <= height, "the queue's button down to {bottom} at {width}x{height}");
                assert!(sheet_top("Album cover").is_some_and(|top| top >= 0.0), "the cover in sight at {width}x{height}");
            }
            play::testing::show_song(&ui, vec![harbour()], 0);
            press(&ui, slint::platform::Key::Escape);
            assert!(!ui.get_now_playing_open());
            // Each artist of what plays has a chip that opens their page, on
            // a phone and a desktop, for a finger and a mouse: in one row,
            // which they drag sideways to the chips it has no room for.
            play::testing::show_song(&ui, vec![work()], 0);
            let artists: Vec<String> = ui.get_playing_names().iter().map(|name| name.label.to_string()).collect();
            assert_eq!(artists.len(), 4);
            let input = ui.global::<Input>();
            for (width, height) in [(412.0, 915.0), (1100.0, 760.0)] {
                ui.window().set_size(slint::LogicalSize::new(width, height));
                idle(Duration::from_millis(400)).await;
                for finger in [false, true] {
                    let how = if finger { "a finger" } else { "a mouse" };
                    for (index, artist) in artists.iter().enumerate() {
                        if !ui.get_now_playing_open() {
                            click(&ui, "Now playing");
                            idle(Duration::from_millis(400)).await;
                        }
                        input.set_touch(finger);
                        input.set_finger(finger);
                        // Where to tap the chip once as much of it is in sight
                        // as the row has room for.
                        let mut tap_at = None;
                        for _ in 0..8 {
                            let rows: Vec<ElementHandle> = ui
                                .root_element()
                                .query_descendants()
                                .match_inherits("NameChips")
                                .find_all()
                                .into_iter()
                                .filter(|row| (0.0..height).contains(&row.absolute_position().y))
                                .collect();
                            assert_eq!(rows.len(), 1, "one row of artists in sight at {width}px");
                            let row = &rows[0];
                            let (row_left, row_width) = (row.absolute_position().x, row.size().width);
                            let row_right = row_left + row_width;
                            let chips = row.query_descendants().match_accessible_role(AccessibleRole::Button).find_all();
                            let seen = |chip: &ElementHandle| {
                                let label = chip.accessible_label().unwrap_or_default();
                                artists.iter().position(|name| name.as_str() == label.as_str())
                            };
                            let reach = row_width / 2.0 - 8.0;
                            let by = match chips.iter().find(|chip| seen(chip) == Some(index)) {
                                Some(chip) => {
                                    let left = chip.absolute_position().x;
                                    let right = left + chip.size().width;
                                    if right.min(row_right) - left.max(row_left) >= chip.size().width.min(row_width) - 1.0 {
                                        let x = (left.max(row_left) + right.min(row_right)) / 2.0;
                                        tap_at = Some(slint::LogicalPosition::new(x, middle(chip).y));
                                        break;
                                    }
                                    if right > row_right { row_right - right } else { row_left - left }
                                }
                                // Wholly out of sight, on the side of the chips
                                // before or after it.
                                None if chips.first().and_then(seen).is_some_and(|first| first > index) => reach,
                                None => -reach,
                            };
                            let from = slint::LogicalPosition::new(row_left + row_width / 2.0, middle(row).y);
                            let by = by.clamp(-reach, reach);
                            let steps: Vec<(u64, f32, f32)> = (1..=8).map(|step| (16, by * step as f32 / 8.0, 0.0)).collect();
                            drag(&ui, from, &steps, 300).await;
                            idle(Duration::from_millis(600)).await;
                            assert!(ui.get_now_playing_open(), "dragging the row at {width}px with {how} opens nothing");
                        }
                        let tap_at = tap_at.unwrap_or_else(|| panic!("{artist}'s chip out of reach at {width}px for {how}"));
                        let visit = ui.get_visit();
                        hold(&ui, tap_at, 0.0, 30).await;
                        if finger {
                            ui.window().dispatch_event(WindowEvent::PointerExited);
                        }
                        wait_for(&format!("{artist}'s page from {how} at {width}px"), || {
                            ui.get_visit() != visit && ui.get_page() == Page::Artist
                        })
                        .await;
                        input.set_touch(false);
                        input.set_finger(false);
                        let opened = ui.get_visit();
                        ui.invoke_back();
                        wait_for("the page before", || ui.get_visit() != opened).await;
                        idle(Duration::from_millis(400)).await;
                    }
                }
            }
            // Another song's artists start from the first, wherever the row
            // was dragged to.
            play::testing::show_song(&ui, vec![harbour()], 0);
            idle(Duration::from_millis(100)).await;
            let row = ui.root_element().query_descendants().match_inherits("NameChips").find_first().unwrap();
            let first = row
                .query_descendants()
                .match_accessible_role(AccessibleRole::Button)
                .find_first()
                .expect("Mara Lind's chip in sight");
            assert_eq!(first.absolute_position().x, row.absolute_position().x);
            // Where the app has no say in how the sound reaches the
            // speakers, as in a browser, neither a desktop's bar nor a
            // phone's page offers to choose it.
            let sound_offered = || find(&ui, "Sound processing").is_some();
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            idle(Duration::from_millis(400)).await;
            assert!(sound_offered(), "a desktop's bar offers it");
            ui.set_has_sound(false);
            idle(Duration::from_millis(100)).await;
            assert!(!sound_offered(), "a desktop's bar without a say");
            ui.window().set_size(slint::LogicalSize::new(412.0, 915.0));
            idle(Duration::from_millis(400)).await;
            click(&ui, "Now playing");
            idle(Duration::from_millis(400)).await;
            ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(206.0, 400.0),
                delta_x: 0.0,
                delta_y: -100000.0,
            });
            idle(Duration::from_millis(100)).await;
            assert!(!sound_offered(), "a phone's page without a say");
            ui.set_has_sound(true);
            idle(Duration::from_millis(100)).await;
            assert!(sound_offered(), "a phone's page offers it");
            press(&ui, slint::platform::Key::Escape);
            assert!(!ui.get_now_playing_open());
            // A phone's lyrics are a page of the sheet, from the row under
            // the controls.
            click(&ui, "Now playing");
            idle(Duration::from_millis(400)).await;
            assert!(!text_in_sight(words), "Now playing has no words");
            ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(206.0, 400.0),
                delta_x: 0.0,
                delta_y: -100000.0,
            });
            idle(Duration::from_millis(100)).await;
            click(&ui, "Lyrics");
            idle(Duration::from_millis(100)).await;
            assert!(ui.get_lyrics_open() && text_in_sight(words), "the words");
            assert!(find(&ui, "Album cover").is_none(), "in Now playing's place");
            press(&ui, slint::platform::Key::Escape);
            assert!(ui.get_now_playing_open() && !ui.get_lyrics_open(), "Escape goes back to Now playing");
            assert!(find(&ui, "Album cover").is_some(), "Now playing opens at its top");

            // The songs played before the one playing wait above it in the
            // queue, out of view: the queue opens on what plays, and comes
            // back to it when the song changes, with the earlier ones a
            // scroll up away. Now playing has none of them.
            let played = |title: &str| -> bool {
                let wanted = title.to_owned();
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(AccessibleRole::ListItem)
                    .match_predicate(move |row| row.accessible_label().is_some_and(|l| l == wanted))
                    .find_first()
                    .is_some()
            };
            // On a phone or a tablet the queue is a page of the sheet, from
            // the button at the end of Now playing.
            ui.window().set_size(slint::LogicalSize::new(800.0, 760.0));
            idle(Duration::from_millis(400)).await;
            let on_what_plays = || find(&ui, "Album cover").is_some();
            let open_queue = async || {
                ui.window().dispatch_event(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(750.0, 400.0),
                    delta_x: 0.0,
                    delta_y: -100000.0,
                });
                idle(Duration::from_millis(100)).await;
                click(&ui, "Queue");
                idle(Duration::from_millis(300)).await;
            };
            let tracks: Vec<_> = (0..16).map(|n| song(&format!("Track {n}"), "a2")).collect();
            play::testing::show_queue(&ui, tracks.clone(), 6);
            click(&ui, "Close now playing");
            idle(Duration::from_millis(300)).await;
            click(&ui, "Now playing");
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_previous_lines().row_count(), 6);
            assert!(on_what_plays(), "opened on what plays");
            assert!(!ui.get_queue_open());
            assert!(!played("Track 6") && !played("Track 5"), "no queue on Now playing");
            open_queue().await;
            assert!(ui.get_queue_open());
            assert!(!on_what_plays(), "the queue has no controls");
            assert!(played("Track 6"), "the song playing is at the top");
            assert!(!played("Track 5"), "the song before it is above the view");
            ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(550.0, 400.0),
                delta_x: 0.0,
                delta_y: 100000.0,
            });
            idle(Duration::from_millis(100)).await;
            assert!(played("Track 0"), "scrolled up to the first played");
            assert!(!played("Track 15"));
            play::testing::show_queue(&ui, tracks.clone(), 7);
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_previous_lines().row_count(), 7);
            assert!(played("Track 7"), "back on what plays after the song changed");
            assert!(!played("Track 6"));
            // What plays has a heading of its own, and so do the songs after
            // it. Scrolled up from those, the queue stops at what plays, by
            // the wheel and by a finger's drag or fling; only a scroll after
            // that goes on to the songs played before.
            let top_in_sight = |role: AccessibleRole, label: &str| {
                let wanted = label.to_owned();
                ui.root_element()
                    .query_descendants()
                    .match_accessible_role(role)
                    .match_predicate(move |e| e.accessible_label().is_some_and(|l| l == wanted))
                    .find_first()
                    .map(|e| e.absolute_position().y)
            };
            let tops = [
                top_in_sight(AccessibleRole::Text, "Now playing"),
                top_in_sight(AccessibleRole::ListItem, "Track 7"),
                top_in_sight(AccessibleRole::Text, "Up next"),
                top_in_sight(AccessibleRole::ListItem, "Track 8"),
            ];
            assert!(tops.iter().all(Option::is_some), "both headings in sight: {tops:?}");
            assert!(tops.is_sorted(), "what plays between its heading and the next: {tops:?}");
            let queue_top = top_in_sight(AccessibleRole::Main, "Queue");
            assert!(queue_top.is_some_and(|top| tops[0] >= Some(top)), "its heading wholly in sight");
            let longer: Vec<_> = (0..20).map(|n| song(&format!("Track {n}"), "a2")).collect();
            play::testing::show_queue(&ui, longer, 6);
            idle(Duration::from_millis(300)).await;
            let wheel = |by: f32| {
                ui.window().dispatch_event(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(550.0, 400.0),
                    delta_x: 0.0,
                    delta_y: by,
                });
            };
            let stopped_at_what_plays = || played("Track 6") && !played("Track 5");
            wheel(-100000.0);
            idle(Duration::from_millis(400)).await;
            assert!(!played("Track 6"), "down among the songs to come");
            wheel(100000.0);
            idle(Duration::from_millis(100)).await;
            assert!(stopped_at_what_plays(), "the wheel stops at what plays");
            wheel(100000.0);
            idle(Duration::from_millis(100)).await;
            assert!(stopped_at_what_plays(), "its next tick is the same scroll");
            idle(Duration::from_millis(400)).await;
            wheel(100000.0);
            idle(Duration::from_millis(100)).await;
            assert!(played("Track 0"), "a scroll after that goes on");
            let input = ui.global::<Input>();
            let on_a_row = slint::LogicalPosition::new(400.0, 120.0);
            // Slow enough that the finger, not the time since the stop, holds it.
            let slow_drag = [(0, 0.0, 6.0), (120, 0.0, 300.0), (400, 0.0, 450.0), (400, 0.0, 600.0)];
            let fling = [(0, 0.0, 6.0), (16, 0.0, 60.0), (16, 0.0, 120.0)];
            for (what, path, rest) in [("a drag", &slow_drag[..], 100), ("a fling", &fling[..], 0)] {
                wheel(-100000.0);
                idle(Duration::from_millis(400)).await;
                assert!(!played("Track 6"), "down among the songs to come");
                input.set_finger(true);
                drag(&ui, on_a_row, path, rest).await;
                idle(Duration::from_millis(1500)).await;
                input.set_finger(false);
                assert!(stopped_at_what_plays(), "{what} up stops at what plays");
            }
            input.set_finger(true);
            drag(&ui, on_a_row, &slow_drag, 100).await;
            idle(Duration::from_millis(1500)).await;
            input.set_finger(false);
            assert!(played("Track 0"), "the next drag goes on");
            play::testing::show_queue(&ui, tracks.clone(), 7);
            idle(Duration::from_millis(300)).await;
            wheel(100000.0);
            idle(Duration::from_millis(100)).await;
            assert!(played("Track 0"), "left up among the songs played");
            // The down arrow and the Now playing button go back, one each way.
            click(&ui, "Back to now playing");
            idle(Duration::from_millis(100)).await;
            assert!(on_what_plays() && !ui.get_queue_open());
            open_queue().await;
            assert!(ui.get_queue_open() && played("Track 7"), "opens on what plays again");
            assert!(!played("Track 6"), "the songs played are above it again");
            assert!(!on_what_plays());
            press(&ui, slint::platform::Key::Escape);
            idle(Duration::from_millis(100)).await;
            assert!(on_what_plays() && !ui.get_queue_open(), "Back leaves the queue first");
            assert!(ui.get_now_playing_open());
            open_queue().await;
            let sheet_button = ui
                .root_element()
                .query_descendants()
                .match_accessible_role(AccessibleRole::Button)
                .match_predicate(|button| button.accessible_label().is_some_and(|l| l == "Now playing"))
                .find_all()
                .into_iter()
                .min_by(|a, b| a.absolute_position().y.total_cmp(&b.absolute_position().y))
                .expect("no Now playing button");
            sheet_button.invoke_accessible_default_action();
            idle(Duration::from_millis(100)).await;
            assert!(on_what_plays() && !ui.get_queue_open());
            // The sheet opened from the bar shows Now playing, wherever it
            // was left, as when the page under it changed.
            open_queue().await;
            let page = ui.get_visit();
            ui.invoke_back();
            wait_for("the page before to close it", || !ui.get_now_playing_open()).await;
            ui.invoke_forward();
            wait_for("the page again", || ui.get_visit() != page).await;
            idle(Duration::from_millis(400)).await;
            click(&ui, "Now playing");
            idle(Duration::from_millis(400)).await;
            assert!(ui.get_now_playing_open() && !ui.get_queue_open() && on_what_plays());
            // The rows below are the queue's.
            open_queue().await;
            // A row played before counts back from the one playing: it plays
            // next, or goes from the queue, whole and in its place.
            let titles = |lines: slint::ModelRc<QueueLine>| -> Vec<String> {
                lines.iter().map(|line| line.title.to_string()).collect()
            };
            ui.invoke_play_queued_next(-2);
            assert_eq!(titles(ui.get_queue_lines())[..3], ["Track 7", "Track 5", "Track 8"]);
            assert_eq!(ui.get_previous_lines().row_count(), 6);
            ui.invoke_remove_queued(-1);
            assert_eq!(ui.get_previous_lines().row_count(), 5);
            assert_eq!(titles(ui.get_previous_lines())[4], "Track 4");
            assert_eq!(titles(ui.get_queue_lines())[0], "Track 7", "what plays plays on");

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
            // The open work that plays has its movements under Now playing,
            // above Up next.
            let work_under_now_playing = |list: &str| {
                let tops = [
                    top_in_sight(AccessibleRole::Text, "Now playing"),
                    top_in_sight(AccessibleRole::ListItem, "Sonata"),
                    top_in_sight(AccessibleRole::ListItem, "III"),
                    top_in_sight(AccessibleRole::Text, "Up next"),
                    top_in_sight(AccessibleRole::ListItem, "solo"),
                ];
                assert!(tops.iter().all(Option::is_some), "all in sight on the {list}: {tops:?}");
                assert!(tops.is_sorted(), "its movements under Now playing on the {list}: {tops:?}");
            };
            ui.window().dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(550.0, 400.0),
                delta_x: 0.0,
                delta_y: 100000.0,
            });
            idle(Duration::from_millis(100)).await;
            work_under_now_playing("sheet");
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
            let q1_top = queue_row("q1").absolute_position().y;
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
            assert!((copy_y - (q1_top + 2.0 * row_height + 20.0)).abs() < 0.5, "it follows the pointer");
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
            ui.window().set_size(slint::LogicalSize::new(800.0, 760.0));
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

            // On a desktop the queue is a panel at the window's right, which
            // stays open as Now playing opens and closes beside it: the bar's
            // Queue button opens and closes it, and so do its own close
            // button, Escape in it and Android's back.
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            idle(Duration::from_millis(400)).await;
            assert!(ui.get_now_playing_open() && ui.get_queue_open(), "the queue stays open");
            let panel_close = button(&ui, "Close queue");
            assert!(panel_close.absolute_position().x > 1100.0 - 420.0, "a panel at the right");
            assert!(row_in_sight("lead").is_some(), "with what plays");
            click(&ui, "Close queue");
            idle(Duration::from_millis(100)).await;
            assert!(!ui.get_queue_open() && ui.get_now_playing_open());
            assert!(row_in_sight("lead").is_none());
            click(&ui, "Queue");
            idle(Duration::from_millis(100)).await;
            assert!(ui.get_queue_open() && row_in_sight("lead").is_some());
            press(&ui, slint::platform::Key::Escape);
            assert!(!ui.get_queue_open() && ui.get_now_playing_open(), "Escape in the panel closes it");
            press(&ui, slint::platform::Key::Escape);
            assert!(!ui.get_now_playing_open(), "then the page");
            click(&ui, "Queue");
            idle(Duration::from_millis(100)).await;
            assert!(ui.get_queue_open() && !ui.get_now_playing_open());
            click(&ui, "Now playing");
            idle(Duration::from_millis(400)).await;
            assert!(ui.get_queue_open() && ui.get_now_playing_open(), "the page opens beside it");
            // The panel too comes to what plays once songs have played
            // before it, where it had none, and stops there on its way up.
            let longer: Vec<_> = (0..20).map(|n| song(&format!("Track {n}"), "a2")).collect();
            play::testing::show_queue(&ui, longer, 6);
            idle(Duration::from_millis(300)).await;
            let panel_wheel = |by: f32| {
                ui.window().dispatch_event(WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(890.0, 400.0),
                    delta_x: 0.0,
                    delta_y: by,
                });
            };
            assert!(played("Track 6") && !played("Track 5"), "the panel opens on what plays");
            panel_wheel(-100000.0);
            idle(Duration::from_millis(400)).await;
            assert!(!played("Track 6"), "down among the songs to come");
            panel_wheel(100000.0);
            idle(Duration::from_millis(100)).await;
            assert!(played("Track 6") && !played("Track 5"), "the panel stops at what plays");
            idle(Duration::from_millis(400)).await;
            panel_wheel(100000.0);
            idle(Duration::from_millis(100)).await;
            assert!(played("Track 0"), "and goes on with the next scroll");
            play::testing::play_page(&ui, pieces(), 0);
            idle(Duration::from_millis(100)).await;
            click(&ui, "Show the movements of Sonata");
            idle(Duration::from_millis(100)).await;
            work_under_now_playing("panel");
            click(&ui, "Queue");
            assert!(!ui.get_queue_open() && ui.get_now_playing_open(), "the bar's button closes it");
            click(&ui, "Queue");
            idle(Duration::from_millis(100)).await;
            back::pressed(&ui, true);
            assert!(!ui.get_queue_open() && ui.get_now_playing_open(), "back closes the panel first");
            back::pressed(&ui, true);
            assert!(!ui.get_now_playing_open());

            // So does the queue loaded anew, as on a sign in.
            let mut saved = prefs::load();
            saved.expanded_works = vec!["m1".into()];
            prefs::save(&saved);
            let asked = MOTIONS_ASKED.load(Relaxed);
            play::testing::restore(&ui);
            wait_for("its motion after the sign in", || {
                MOTIONS_ASKED.load(Relaxed) > asked
            })
            .await;

            // It is at the song it was at, as far into it, and its whole work
            // is one row again, open as it was left; closed, it stays closed.
            let row = |title: &str, work: bool, part: bool| (title.to_owned(), work, part);
            wait_for("the saved queue", || ui.get_playing_movement() == "II").await;
            assert_eq!(ui.get_playing_title(), "Sonata");
            assert_eq!(ui.get_playing_movement_place(), "2 of 3");
            assert_eq!(ui.get_playing_position(), 61.5);
            // The scrobbler sends what waits with the sign in.
            assert!(play::testing::scrobbler_calls().1 > 0);
            assert_eq!(
                rows(),
                [
                    row("Sonata", true, false),
                    row("I", false, true),
                    row("II", false, true),
                    row("III", false, true)
                ]
            );
            ui.invoke_toggle_queued_work(0);
            assert_eq!(rows(), [row("Sonata", true, false)]);
            assert!(prefs::load().expanded_works.is_empty());

            // A save the server turns down while it restarts doesn't stop the
            // next: that one goes out with where the queue is by then.
            SAVES.lock().unwrap().clear();
            SAVE_DOWN.store(true, Relaxed);
            play::testing::save(2, 70.0);
            wait_for("the save turned down", || !SAVE_DOWN.load(Relaxed)).await;
            idle(Duration::from_millis(200)).await;
            play::testing::save(3, 5.0);
            wait_for("the next save", || SAVES.lock().unwrap().len() == 2).await;
            assert_eq!(
                SAVES.lock().unwrap().clone(),
                [
                    "savePlayQueue id=c1&id=m1&id=m2&id=m3&current=m2&position=70000",
                    "savePlayQueue id=c1&id=m1&id=m2&id=m3&current=m3&position=5000"
                ]
            );

            // One that says it will never keep the queue, as when the user
            // may not, isn't asked again until the next sign in.
            SAVES.lock().unwrap().clear();
            SAVE_REFUSED.store(true, Relaxed);
            play::testing::save(2, 70.0);
            wait_for("the save refused", || SAVES.lock().unwrap().len() == 1).await;
            idle(Duration::from_millis(200)).await;
            play::testing::save(3, 5.0);
            idle(Duration::from_millis(200)).await;
            assert_eq!(SAVES.lock().unwrap().len(), 1, "not asked again");
            SAVE_REFUSED.store(false, Relaxed);
            play::testing::restore(&ui);
            wait_for("the queue loaded again", || ui.get_playing()).await;
            play::testing::save(3, 5.0);
            wait_for("a save after the sign in", || SAVES.lock().unwrap().len() == 2).await;
            assert_eq!(
                SAVES.lock().unwrap()[1],
                "savePlayQueue id=c1&id=m1&id=m2&id=m3&current=m3&position=5000"
            );

            // Played, it goes on at that song from as far into it. A player
            // that stops short of the queue's end, as without a sound device
            // or with songs that can't play, goes on from there again, not
            // from the queue's first song.
            ui.invoke_toggle_pause();
            wait_for("the player to stop short", || {
                play::testing::player_status()
                    .is_some_and(|status| status.ended && status.stopped.is_some())
            })
            .await;
            let stopped = play::testing::player_status().and_then(|status| status.stopped);
            assert_eq!(stopped, Some((2, 61.5)));
            ui.invoke_toggle_pause();
            wait_for("Play to go on where it stopped", || {
                play::testing::player_status().is_some_and(|status| {
                    let at = status.index.map(|index| (index, status.position));
                    at.or(status.stopped) == Some((2, 61.5))
                })
            })
            .await;
            // It is shown the player on each tick while there is one.
            assert!(play::testing::scrobbler_calls().0 > 0);
            // The event loop's end, as on Android when the activity goes and
            // the process stays, lets the player go.
            ui.invoke_event_loop_ended();
            assert!(play::testing::player_status().is_none());
            play::testing::clear(&ui);

            // A song with a waveform shows it as the seek bar, and one without
            // keeps the plain bar. The last song's waveform goes at once, and
            // one that comes late for a song left behind doesn't show.
            let asked = WAVEFORMS_ASKED.load(Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            wait_for("its waveform", || ui.get_playing_waveform().row_count() > 0).await;
            // Its bars keep their width whatever the bar's: a wider one has
            // more of them.
            let bars = |width: f32| {
                let path = ui.global::<Waveform>().invoke_path(ui.get_playing_waveform(), width);
                path.matches('M').count()
            };
            assert_eq!((bars(300.0), bars(900.0)), (100, 300));
            play::testing::show_song(&ui, vec![song("w2", "a1")], 0);
            assert_eq!(ui.get_playing_waveform().row_count(), 0, "the last song's is gone");
            wait_for("the waveform asked for", || {
                WAVEFORMS_ASKED.load(Relaxed) == asked + 2
            })
            .await;
            SLOW_WAVEFORM.store(true, Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            play::testing::show_song(&ui, vec![song("w2", "a1")], 0);
            idle(Duration::from_millis(1000)).await;
            assert_eq!(ui.get_playing_waveform().row_count(), 0);
            SLOW_WAVEFORM.store(false, Relaxed);

            // The simple bar asks for no waveform, and drops one that is on
            // its way; the waveform asks again for the song on the bar.
            SLOW_WAVEFORM.store(true, Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            ui.set_simple_seek_bar(true);
            ui.invoke_choose_simple_seek_bar(true);
            idle(Duration::from_millis(1000)).await;
            assert_eq!(ui.get_playing_waveform().row_count(), 0, "a reply on its way is dropped");
            assert!(prefs::load().simple_seek_bar);
            SLOW_WAVEFORM.store(false, Relaxed);
            let asked = WAVEFORMS_ASKED.load(Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            idle(Duration::from_millis(500)).await;
            assert_eq!(WAVEFORMS_ASKED.load(Relaxed), asked, "none is asked for");
            ui.set_simple_seek_bar(false);
            ui.invoke_choose_simple_seek_bar(false);
            wait_for("the waveform again", || ui.get_playing_waveform().row_count() > 0).await;
            assert!(!prefs::load().simple_seek_bar);

            // A phone's signal path opens in the sheet its menus open in,
            // wholly in sight, and Android's back closes it; a wider window
            // keeps the popup by the light.
            let steps = [
                "FLAC 24-bit 96 kHz",
                "Volume leveling",
                "Shared: USB DAC (48 kHz 24-bit)",
                "Android's mixer handles the stream and may change its rate",
            ];
            let path = steps.map(|text| PathStep { text: text.into(), quality: 2 });
            ui.set_playing_path(slint::ModelRc::new(slint::VecModel::from(path.to_vec())));
            ui.set_playing_quality("High quality".into());
            ui.set_playing_level(2);
            ui.set_playing(true);
            let sheet = || {
                ui.root_element()
                    .query_descendants()
                    .match_inherits("Sheet")
                    .find_first()
            };
            ui.window().set_size(slint::LogicalSize::new(412.0, 915.0));
            idle(Duration::from_millis(400)).await;
            click(&ui, "Signal path: High quality");
            idle(Duration::from_millis(100)).await;
            let opened = sheet().expect("the signal path's sheet");
            assert_eq!(
                (opened.absolute_position(), opened.size()),
                (slint::LogicalPosition::new(0.0, 0.0), slint::LogicalSize::new(412.0, 915.0)),
                "over the whole window"
            );
            for step in steps {
                let top = text_top(&ui, step).unwrap_or_else(|| panic!("{step} out of sight"));
                assert!(top > 915.0 / 2.0, "{step} at {top}, in the sheet at the bottom");
            }
            press(&ui, slint::platform::Key::Back);
            idle(Duration::from_millis(100)).await;
            assert!(sheet().is_none(), "Back closes it");
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            idle(Duration::from_millis(400)).await;
            click(&ui, "Signal path: High quality");
            idle(Duration::from_millis(100)).await;
            assert!(sheet().is_none(), "no sheet on a desktop");
            assert!(text_top(&ui, steps[0]).is_some(), "the popup");
            press(&ui, slint::platform::Key::Escape);
            idle(Duration::from_millis(100)).await;
            assert!(text_top(&ui, steps[0]).is_none(), "Escape closes it");
            ui.set_playing(false);
            ui.set_playing_quality("".into());
            ui.set_playing_level(-1);
            ui.set_playing_path(slint::ModelRc::default());

            // Shuffle and repeat name the state they are in, whose icon they
            // show, and a press moves each on to its next, on a desktop's bar
            // and a phone's now playing page alike.
            ui.set_playing(true);
            for (width, height) in [(1100.0, 760.0), (412.0, 915.0)] {
                ui.window().set_size(slint::LogicalSize::new(width, height));
                idle(Duration::from_millis(400)).await;
                if width < 600.0 {
                    click(&ui, "Now playing");
                    idle(Duration::from_millis(400)).await;
                }
                let states = [
                    ("Shuffle off", "Shuffle on"),
                    ("Shuffle on", "Shuffle off"),
                    ("Repeat off", "Repeat all"),
                    ("Repeat all", "Repeat one"),
                    ("Repeat one", "Repeat off"),
                ];
                for (state, next) in states {
                    click(&ui, state);
                    idle(Duration::from_millis(100)).await;
                    assert!(find(&ui, next).is_some(), "{state} goes on to {next} at {width}px");
                }
            }
            press(&ui, slint::platform::Key::Escape);
            assert!(!ui.get_now_playing_open());
            ui.window().set_size(slint::LogicalSize::new(1100.0, 760.0));
            ui.set_playing(false);

            // Without the extension the still cover stays and none is asked for.
            let asked = MOTIONS_ASKED.load(Relaxed);
            let waveforms_asked = WAVEFORMS_ASKED.load(Relaxed);
            click(&ui, "Account");
            click(&ui, "Sign out");
            wait_for("the sign-in step", || ui.get_step() == Step::SignIn).await;
            NO_MOTION.store(true, Relaxed);
            NO_WAVEFORM.store(true, Relaxed);
            NO_BANNER.store(true, Relaxed);
            OLD_GENRES.store(true, Relaxed);
            NO_COMPOSITIONS.store(true, Relaxed);
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
            assert_eq!(ui.get_playing_banner().size().width, 0, "no banner without the extension");
            NO_BANNER.store(false, Relaxed);
            // Descriptions come from the second version of the genres only.
            ui.invoke_open("genre:Jazz".into());
            wait_for("Jazz's subgenres", || ui.get_genre_tiles().row_count() == 1).await;
            assert_eq!(ui.get_about(), "");
            ui.invoke_back();
            // The search has no composers or compositions and asks for none.
            let calls = ASKED.lock().unwrap().len();
            ui.set_search_query("Ludwig".into());
            ui.invoke_open("search".into());
            wait_for("the search answered", || ui.get_search_answered()).await;
            idle(Duration::from_millis(200)).await;
            assert_eq!(ui.get_found_composers().row_count(), 0);
            assert_eq!(ui.get_found_works().row_count(), 0);
            let since = ASKED.lock().unwrap()[calls..].to_vec();
            assert!(!since.iter().any(|call| call.starts_with("getCompos")), "{since:?}");
            ui.invoke_back();
            NO_COMPOSITIONS.store(false, Relaxed);
            OLD_GENRES.store(false, Relaxed);
            NO_MOTION.store(false, Relaxed);
            play::testing::show_song(&ui, vec![song("w1", "a1")], 0);
            idle(Duration::from_millis(300)).await;
            assert_eq!(ui.get_playing_waveform().row_count(), 0);
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
            let genres = sections.iter().position(|s| s == "Genres").unwrap();
            assert_eq!(sections[genres + 1..genres + 3], ["Tags", "Radio"]);
            let groups = texts(ui.get_section_groups());
            assert_eq!(groups[genres + 1], "Browse", "Tags is in Browse");
            let browse_links: Vec<_> = ui.get_browse_links().iter().map(|e| e.link).collect();
            let library_links: Vec<_> = ui.get_library_links().iter().map(|e| e.link).collect();
            assert!(browse_links.iter().any(|link| link == "tags"), "{browse_links:?}");
            assert!(!library_links.iter().any(|link| link == "tags"), "{library_links:?}");
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
            // One shown in the folder already, but moved out elsewhere since,
            // goes in all the same: the server is asked where it is.
            wait_for("the places kept", || asked_after("movePlaylist", "getPlaylists")).await;
            idle(Duration::from_millis(100)).await;
            wait_for("the places shown", || tile_titles(&ui) == ["Trips", "Empty"]).await;
            ORGANIZED.store(false, Relaxed);
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.global::<Drag>().set_link("playlist:p1".into());
            ui.invoke_drop_on_tile(0);
            wait_for("the drop of one moved out", || !writes().is_empty()).await;
            assert_eq!(writes(), ["movePlaylist playlistId=p1&playlistFolderId=f1"]);
            ORGANIZED.store(true, Relaxed);
            ui.invoke_refresh();
            wait_for("the places kept", || tile_titles(&ui) == ["Trips", "Empty"]).await;

            // The tags page has every tag under the one it is in. Those
            // refused at sign-in and on the page come with Try again.
            ORGANIZE_WRITES.lock().unwrap().clear();
            ui.invoke_show_section((genres + 1) as i32);
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
            // After the hour, the tags kept show at once and are asked for
            // again.
            let tags_asked = times_asked("getTags");
            let now = answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            ui.invoke_open("setting:downloads".into());
            ui.invoke_back();
            assert_eq!(ui.get_page(), Page::Tags);
            assert_eq!(ui.get_tag_rows().row_count(), 3);
            wait_for("the tags asked again", || times_asked("getTags") > tags_asked).await;
            answers::LATER.store(now, Relaxed);

            // A tag's page lists its tracks and those of the tags in it.
            ui.invoke_open("tag:t1".into());
            assert_eq!(ui.get_page(), Page::Tag);
            assert_eq!(lit(&ui), "Tags");
            assert_eq!(ui.get_bar_section(), 1, "a tag's page lights Browse");
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
            idle(Duration::from_millis(100)).await;
            ORGANIZE_WRITES.lock().unwrap().clear();
            let tags_asked = times_asked("getTags");
            ui.invoke_move_tag("t2".into(), "".into());
            // Done with once the tags are asked for again after it.
            wait_for("the tag moved", || {
                !writes().is_empty() && times_asked("getTags") > tags_asked
            })
            .await;
            assert_eq!(writes(), ["updateTag tagId=t2&parentId="]);
            ui.invoke_open("tag:t1".into());
            assert_eq!(ui.get_track_lines().row_count(), 0, "asked for again");
            wait_for("the tag's tracks again", || ui.get_track_lines().row_count() == 2).await;
            // An album is downloaded from its page: its songs arrive one
            // after the other, their rows are marked and the song plays from
            // its file.
            ui.invoke_open("album:a1".into());
            wait_for("the album", || ui.get_tracks().row_count() == 2).await;
            assert!(ui.get_can_download());
            assert_eq!(ui.get_download_state(), 0);
            let marked = |ui: &AppWindow| -> Vec<bool> {
                ui.get_tracks().iter().map(|track| track.downloaded).collect()
            };
            assert_eq!(marked(&ui), [false, false]);
            download_page(&ui);
            wait_for("both songs", || ui.get_download_state() == 2).await;
            assert_eq!(marked(&ui), [true, true]);
            let kept = crate::offline::folder().unwrap();
            let files = |folder: &std::path::Path| -> usize {
                std::fs::read_dir(folder).map_or(0, |entries| entries.count())
            };
            assert_eq!(files(&kept.join("songs")), 2, "no half-fetched file stays");
            assert!(kept.join("index.json").exists());
            assert!(kept.join("CACHEDIR.TAG").exists());
            let mut played = Vec::new();
            std::io::Read::read_to_end(&mut play::testing::fetch_track("s1").unwrap(), &mut played).unwrap();
            assert_eq!(played, tiny_wav(), "a kept song is read from its file");

            // Shown again, the page knows it is kept; taking it off asks
            // first and deletes the files.
            ui.invoke_open("albums".into());
            ui.invoke_open("album:a1".into());
            wait_for("the album again", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_download_state(), 2);
            assert_eq!(marked(&ui), [true, true]);
            click(&ui, "Downloaded, remove download");
            click(&ui, "Cancel");
            assert_eq!(ui.get_download_state(), 2);
            click(&ui, "Downloaded, remove download");
            click(&ui, "Remove");
            assert_eq!(ui.get_download_state(), 0);
            assert_eq!(marked(&ui), [false, false]);
            assert_eq!(files(&kept.join("songs")), 0);

            // A song that comes to more than the limit leaves room for stops
            // there with a word, whatever size the server said it has.
            play::testing::downloads().set_limit(Some(10));
            download_page(&ui);
            let stop = "Downloads stopped: the size limit is reached.";
            wait_for("the stop", || ui.get_download_status() == stop).await;
            assert_eq!(ui.get_download_state(), 1);
            assert_eq!(files(&kept.join("songs")), 0, "no half-fetched file stays");
            ui.invoke_choose_download_limit(0);
            wait_for("the rest after the limit is lifted", || ui.get_download_state() == 2).await;
            ui.invoke_remove_all_downloads();

            // The settings tell what they take and can remove them all.
            download_page(&ui);
            wait_for("both songs again", || ui.get_download_state() == 2).await;
            ui.invoke_open("setting:downloads".into());
            assert_eq!(ui.get_download_songs(), 2);
            assert!(ui.get_download_space().ends_with(" used by 2 songs."), "{}", ui.get_download_space());
            assert_eq!(ui.get_download_status(), "");
            ui.invoke_choose_download_limit(2);
            assert_eq!(crate::prefs::load().download_limit, 2);
            click(&ui, "Remove all downloads");
            click(&ui, "Cancel");
            assert_eq!(ui.get_download_songs(), 2);
            click(&ui, "Remove all downloads");
            click(&ui, "Remove");
            assert_eq!(ui.get_download_songs(), 0);
            assert_eq!(ui.get_download_space(), "Nothing is downloaded.");
            assert!(!kept.exists());

            // Everything removed while a song's answer is still on its way
            // stops it quietly and leaves the folder gone.
            ui.invoke_open("album:a1".into());
            wait_for("the album to race", || ui.get_tracks().row_count() == 2).await;
            SLOW_DOWNLOAD.store(true, Relaxed);
            download_page(&ui);
            idle(Duration::from_millis(150)).await;
            ui.invoke_remove_all_downloads();
            idle(Duration::from_millis(1500)).await;
            SLOW_DOWNLOAD.store(false, Relaxed);
            assert_eq!(ui.get_download_status(), "", "no stop is told");
            assert!(!kept.exists(), "nothing came back");
            ui.invoke_open("album:a1".into());
            wait_for("the album after all", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(ui.get_download_state(), 0);
            assert_eq!(marked(&ui), [false, false]);

            // Downloads in another format: a server that converts sends it so,
            // and one that doesn't sends the original, kept as such, which
            // the settings say.
            let names = || -> Vec<String> {
                let songs = kept.join("songs").read_dir();
                let names = songs.into_iter().flatten().flatten();
                names.map(|file| file.file_name().to_string_lossy().into_owned()).collect()
            };
            ui.invoke_choose_download_quality(2, 4);
            let chosen = crate::prefs::load().download_quality;
            assert_eq!((chosen.format, chosen.bitrate), (2, 4));
            ui.invoke_open("album:a1".into());
            wait_for("the album to convert", || ui.get_tracks().row_count() == 2).await;
            download_page(&ui);
            wait_for("the originals", || ui.get_download_state() == 2).await;
            assert!(names().iter().all(|name| name.contains(".original.")), "{:?}", names());
            ui.invoke_open("setting:downloads".into());
            assert!(ui.get_download_status().contains("This server sends original files only."));
            ui.invoke_remove_all_downloads();
            assert!(!ui.get_download_status().contains("original files only"));
            STREAM_CONVERTS.store(true, Relaxed);
            ui.invoke_open("album:a1".into());
            wait_for("the album to convert", || ui.get_tracks().row_count() == 2).await;
            download_page(&ui);
            wait_for("the conversions", || ui.get_download_state() == 2).await;
            assert!(names().iter().all(|name| name.ends_with(".opus-128.opus")), "{:?}", names());
            ui.invoke_open("setting:downloads".into());
            assert!(!ui.get_download_status().contains("original files only"));
            STREAM_CONVERTS.store(false, Relaxed);
            ui.invoke_choose_download_quality(0, 0);
            ui.invoke_remove_all_downloads();

            // On mobile data nothing is fetched unless allowed; it waits for
            // wifi and goes on when there is some.
            crate::quality::TEST_ON_MOBILE_DATA.store(true, Relaxed);
            ui.invoke_open("album:a1".into());
            wait_for("the album to wait", || ui.get_tracks().row_count() == 2).await;
            download_page(&ui);
            wait_for("the wait for wifi", || {
                ui.get_download_status().starts_with("Waiting for wifi")
            })
            .await;
            assert_eq!(ui.get_download_state(), 1);
            assert_eq!(ui.get_download_progress(), "0 of 2");
            crate::quality::TEST_ON_MOBILE_DATA.store(false, Relaxed);
            wait_for("wifi", || ui.get_download_state() == 2).await;
            assert!(!ui.get_download_status().contains("Waiting"));
            // Allowed, it goes on over mobile data.
            ui.invoke_remove_all_downloads();
            ui.invoke_choose_download_mobile(true);
            crate::quality::TEST_ON_MOBILE_DATA.store(true, Relaxed);
            ui.invoke_open("playlists".into());
            ui.invoke_open("album:a1".into());
            wait_for("the album to take mobile data", || ui.get_tracks().row_count() == 2).await;
            download_page(&ui);
            wait_for("mobile data", || ui.get_download_state() == 2).await;
            crate::quality::TEST_ON_MOBILE_DATA.store(false, Relaxed);
            ui.invoke_choose_download_mobile(false);
            ui.invoke_remove_all_downloads();

            // A playlist downloads from its page, a song from its row's menu
            // and an album from its tile's; a song several want stays until
            // the last of them is let go.
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            download_page(&ui);
            wait_for("the playlist's songs", || ui.get_download_state() == 2).await;
            assert_eq!(marked(&ui), [true, true]);
            ui.invoke_open("album:a1".into());
            wait_for("the album", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(marked(&ui), [true, false], "s1 is kept by the playlist");
            ui.invoke_download_track(1);
            wait_for("the question", || find(&ui, "Cancel").is_some()).await;
            confirm_download(&ui);
            wait_for("the song", || marked(&ui) == [true, true]).await;
            let alone = || -> Vec<bool> { ui.get_tracks().iter().map(|t| t.downloaded_alone).collect() };
            assert_eq!(alone(), [false, true]);
            // The songs kept on their own pick like an album's.
            ui.invoke_open("downloads".into());
            wait_for("the kept song", || ui.get_page() == Page::Downloads && ui.get_tracks().row_count() == 1).await;
            let kept_song = ui.get_tracks().row_data(0).unwrap().title;
            button(&ui, &kept_song).single_click(PointerEventButton::Right).await;
            assert!(ui.get_tracks().row_data(0).unwrap().selected);
            ui.invoke_clear_selection();
            ui.invoke_open("album:a1".into());
            wait_for("the album", || ui.get_tracks().row_count() == 2).await;
            ui.invoke_download_link("album:a1".into());
            wait_for("the question", || find(&ui, "Cancel").is_some()).await;
            confirm_download(&ui);
            wait_for("the album kept", || ui.get_download_state() == 2).await;
            ui.invoke_open("albums".into());
            wait_for("the album tile", || ui.get_tiles().iter().any(|t| t.link == "album:a1")).await;
            let tile_marked = |link: &str| ui.get_tiles().iter().any(|t| t.link == link && t.downloaded);
            assert!(tile_marked("album:a1"));
            assert!(!tile_marked("album:b2"));
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist again", || ui.get_tracks().row_count() == 2).await;
            click(&ui, "Downloaded, remove download");
            click(&ui, "Remove");
            assert_eq!(ui.get_download_state(), 0);
            assert_eq!(marked(&ui), [true, false], "s9 went, s1 is the album's");
            assert_eq!(files(&kept.join("songs")), 2);

            // A folder's row has an album's menu, for every song under it:
            // it queues, is tagged, and downloads as one item that shows on
            // the downloads page.
            ui.invoke_open("folder:d1".into());
            wait_for("her folder", || ui.get_track_lines().row_count() == 2).await;
            let folder_menu = || button(&ui, "More for Night Harbour").invoke_accessible_default_action();
            let folder_line = || ui.get_track_lines().iter().find(|line| line.folder).unwrap();
            let labelled = |label: &str| -> Vec<ElementHandle> {
                let label = label.to_owned();
                (ui.root_element().query_descendants())
                    .match_accessible_role(AccessibleRole::Button)
                    .match_predicate(move |button| button.accessible_label().is_some_and(|l| l == label))
                    .find_all()
            };
            // The menu's entries come after the sidebar's of the same name.
            let choose = |entry: &str| labelled(entry).last().unwrap().invoke_accessible_default_action();
            let entries = ["Play now", "Play next", "Add to queue", "Add to playlist", "Tags", "Download"];
            let before = entries.map(|entry| labelled(entry).len());
            folder_menu();
            assert_eq!(entries.map(|entry| labelled(entry).len()), before.map(|n| n + 1), "{entries:?}");
            let queued = opensubsonic::Child {
                id: "q1".into(),
                title: "Song".into(),
                ..Default::default()
            };
            play::testing::show_song(&ui, vec![queued.clone()], 0);
            choose("Add to queue");
            wait_for("its songs queued", || play::testing::queued_ids().0 == ["q1", "t2", "t1"]).await;
            play::testing::clear(&ui);
            ORGANIZE_WRITES.lock().unwrap().clear();
            folder_menu();
            choose("Tags");
            assert!(ui.get_tag_choices_loading(), "its songs are asked for");
            wait_for("its songs' tags", || !ui.get_tag_choices_loading()).await;
            ui.invoke_toggle_tag(0);
            wait_for("the tag put on", || !writes().is_empty()).await;
            assert_eq!(writes(), ["updateTag tagId=t3&songIdToAdd=t2&songIdToAdd=t1"]);
            click(&ui, "Done");
            folder_menu();
            choose("Download");
            wait_for("the question", || find(&ui, "Cancel").is_some()).await;
            confirm_download(&ui);
            wait_for("the folder kept", || folder_line().downloaded).await;
            assert_eq!(files(&kept.join("songs")), 4);
            ui.invoke_open("downloads".into());
            wait_for("the folder's tile", || {
                ui.get_tiles().iter().any(|t| t.link == "folder:d2" && t.title == "Night Harbour" && t.downloaded)
            })
            .await;
            let folder_tile = ui.get_tiles().iter().find(|t| t.link == "folder:d2").unwrap();
            let named_by: Vec<String> = folder_tile.names.iter().map(|name| name.label.into()).collect();
            assert_eq!(named_by, ["2 tracks"], "the songs it keeps");
            ui.invoke_open("folder:d1".into());
            wait_for("her folder again", || ui.get_track_lines().row_count() == 2).await;
            // With the server out of reach, and its answers no longer fresh,
            // the folder's songs come from this device.
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            FOLDERS_UNREACHABLE.store(true, Relaxed);
            play::testing::show_song(&ui, vec![queued], 0);
            folder_menu();
            choose("Add to queue");
            wait_for("its kept songs queued", || play::testing::queued_ids().0 == ["q1", "t2", "t1"]).await;
            FOLDERS_UNREACHABLE.store(false, Relaxed);
            play::testing::clear(&ui);
            folder_menu();
            choose("Remove download");
            assert!(!folder_line().downloaded);
            assert_eq!(files(&kept.join("songs")), 2, "the album's stay");

            // A downloaded playlist follows the server: on a visit to its page
            // and when the playlists are listed, an hour after they last were,
            // new songs come and those taken off go.
            let an_hour_on = || answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            ui.invoke_remove_all_downloads();
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist", || ui.get_tracks().row_count() == 2).await;
            download_page(&ui);
            wait_for("the playlist kept", || ui.get_download_state() == 2).await;
            ROAD_TRIP_EDITED.store(true, Relaxed);
            an_hour_on();
            ui.invoke_open("albums".into());
            ui.invoke_open("playlist:p1".into());
            wait_for("the edited playlist", || ui.get_tracks().row_count() == 2 && ui.get_tracks().row_data(1).is_some_and(|t| t.title == "Rope and Anchor")).await;
            wait_for("its new song", || marked(&ui) == [true, true]).await;
            assert_eq!(files(&kept.join("songs")), 2, "Fugue was deleted");
            ui.invoke_remove_all_downloads();
            ROAD_TRIP_EDITED.store(false, Relaxed);
            an_hour_on();
            ui.invoke_open("albums".into());
            ui.invoke_open("playlist:p1".into());
            wait_for("the playlist as it was", || ui.get_tracks().row_count() == 2 && ui.get_tracks().row_data(1).is_some_and(|t| t.title == "Fugue")).await;
            download_page(&ui);
            wait_for("kept as it was", || ui.get_download_state() == 2).await;
            ROAD_TRIP_EDITED.store(true, Relaxed);
            an_hour_on();
            ui.invoke_open("playlists".into());
            let has_rope = || {
                let songs = kept.join("songs").read_dir();
                songs.is_ok_and(|songs| {
                    // "s2" in hex.
                    (songs.flatten()).any(|file| file.file_name().to_string_lossy().starts_with("7332."))
                })
            };
            wait_for("the song put in", has_rope).await;
            ROAD_TRIP_EDITED.store(false, Relaxed);
            ui.invoke_remove_all_downloads();
            ui.invoke_open("album:a1".into());
            wait_for("the album", || ui.get_tracks().row_count() == 2).await;
            download_page(&ui);
            wait_for("the album kept again", || ui.get_download_state() == 2).await;

            // With the server down, a start finds the downloads under the
            // note that it didn't answer, opens their albums without asking
            // it, and takes up the server again by itself, on the page it is.
            wait_for("the covers", || kept.join("covers").read_dir().is_ok_and(|d| d.count() == 1)).await;
            DOWN.store(true, Relaxed);
            let offline = crate::start().unwrap();
            offline
                .window()
                .set_size(slint::LogicalSize::new(1100.0, 760.0));
            wait_for("the offline note", || offline.get_offline() != "").await;
            wait_for("the downloads page", || offline.get_page() == Page::Downloads).await;
            wait_for("the downloaded album", || offline.get_tiles().row_count() == 1).await;
            assert_eq!(offline.get_tiles().row_data(0).unwrap().link, "album:a1");
            assert!(offline.get_tiles().row_data(0).unwrap().downloaded);
            assert_eq!(lit(&offline), "Downloads");
            offline.invoke_open("album:a1".into());
            wait_for("the saved album", || offline.get_tracks().row_count() == 2).await;
            assert_eq!(offline.get_download_state(), 2);
            assert_eq!(offline.get_page_problem(), "");
            assert_ne!(offline.get_offline(), "", "the note stays on the album");
            DOWN.store(false, Relaxed);
            wait_for("the server taken up again", || offline.get_offline() == "").await;
            assert_eq!(offline.get_page(), Page::Album, "no leaving the page");
            wait_for("the album from the server", || offline.get_tracks().row_count() == 2).await;
            assert_eq!(offline.get_username(), "joe");
            // Android's back closes a dialog first, then goes back a page,
            // and with none before, to Home, which forward leaves again. On
            // Home it does nothing.
            offline.invoke_ask_download("Album".into(), "".into());
            assert!(offline.global::<Input>().get_modal());
            back::pressed(&offline, true);
            assert!(!offline.global::<Input>().get_modal(), "the dialog closes");
            assert_eq!(offline.get_page(), Page::Album);
            back::pressed(&offline, false);
            assert_eq!(offline.get_page(), Page::Downloads);
            back::pressed(&offline, false);
            assert_eq!(offline.get_page(), Page::Home);
            assert!(offline.get_can_go_forward());
            let visit = offline.get_visit();
            back::pressed(&offline, false);
            back::pressed(&offline, true);
            assert_eq!((offline.get_page(), offline.get_visit()), (Page::Home, visit));
            offline.invoke_forward();
            assert_eq!(offline.get_page(), Page::Downloads);
            let listed = |ui: &AppWindow| ui.get_sections().iter().any(|section| section == "Downloads");
            assert!(listed(&ui));
            ui.invoke_remove_all_downloads();
            assert!(!listed(&ui), "the sidebar's entry goes with the downloads");

            // A pump with a song on its way leaves the background job alone,
            // and signing out then ends it.
            ui.invoke_open("album:a1".into());
            wait_for("the album for the job", || ui.get_tracks().row_count() == 2).await;
            SLOW_DOWNLOAD.store(true, Relaxed);
            let finishes = || download_job::told::FINISHES.load(Relaxed);
            let before = finishes();
            download_page(&ui);
            idle(Duration::from_millis(150)).await;
            ui.invoke_choose_download_limit(0);
            idle(Duration::from_millis(100)).await;
            assert_eq!(finishes(), before, "a pump during a fetch");
            assert!(kept_answers.exists());
            ui.invoke_sign_out();
            // The answers kept go with the account, at once with no plays
            // waiting. The windows started beside this one keep their own
            // until they close.
            assert!(!kept_answers.exists());
            assert_eq!(ui.get_sign_out_sending(), "");
            wait_for("the job ended", || finishes() > before).await;
            SLOW_DOWNLOAD.store(false, Relaxed);
            wait_for("the sign-in step after it", || ui.get_step() == Step::SignIn).await;
            ui.set_username("joe".into());
            ui.set_password("pw".into());
            click(&ui, "Sign in");
            wait_for("the ListenBrainz step again", || ui.get_step() == Step::Listenbrainz).await;
            click(&ui, "Skip");
            wait_for("signed in again", || ui.get_step() == Step::Done).await;
            wait_for("the account saved again", || !ui.get_busy()).await;
            // A scan going on doesn't replace the home page's count with
            // what it has walked so far: the count from before it stays.
            let home_tracks = || ui.get_stats().iter().find(|stat| stat.label == "Tracks").unwrap().value;
            wait_for("the tracks counted", || home_tracks() == "230").await;
            let asked = SCAN_ASKS.load(Relaxed);
            SCANNING.store(true, Relaxed);
            ui.invoke_open("albums".into());
            // The count kept is fresh, so Home would not ask: refreshing
            // ages it, which shows it at once and asks again.
            ui.invoke_refresh();
            ui.invoke_open("home".into());
            wait_for("home's look at the scan", || SCAN_ASKS.load(Relaxed) > asked).await;
            idle(Duration::from_millis(500)).await;
            SCANNING.store(false, Relaxed);
            assert_eq!(home_tracks(), "230");

            // Signing out takes the downloads with it, and the button says
            // what that frees.
            ui.invoke_open("account".into());
            assert_eq!(ui.get_sign_out_frees(), "", "nothing is kept");
            let albums = times_asked("getAlbum");
            ui.invoke_open("album:a1".into());
            wait_for("the album", || ui.get_tracks().row_count() == 2).await;
            assert_eq!(times_asked("getAlbum"), albums + 1, "asked for anew");
            download_page(&ui);
            wait_for("the album kept", || ui.get_download_state() == 2).await;
            ui.invoke_open("account".into());
            assert!(ui.get_sign_out_frees().ends_with(" MB"), "{}", ui.get_sign_out_frees());
            let frees = format!("Sign out and free {}", ui.get_sign_out_frees());
            click(&ui, &frees);
            wait_for("the sign-in step again", || ui.get_step() == Step::SignIn).await;
            assert!(!kept.exists(), "the downloads went with the account");

            // A start signed out of while it signs in, and then in elsewhere,
            // leaves the library signed in to since alone: once its sign-in
            // ends, the pages it opened ask nothing more.
            let elsewhere = serve(subsonic);
            let account = Account {
                server: server.clone(),
                login: Login::Password {
                    username: "joe".into(),
                    password: "pw".into(),
                },
                legacy_listenbrainz: None,
            };
            store::save(account).await.unwrap();
            // Whatever is kept is aged, so the folders page asks the server.
            answers::LATER.fetch_add(opensubsonic::FRESH_FOR, Relaxed);
            PING_HELD.store(true, Relaxed);
            // `super::`, as a step above binds `start` to a position.
            let left = super::start().unwrap();
            wait_for("the library opened", || left.get_step() == Step::Done).await;
            left.invoke_open("folders".into());
            left.invoke_open("account".into());
            click(&left, "Change server");
            wait_for("the server step", || left.get_step() == Step::Server).await;
            left.set_address(elsewhere.as_str().into());
            click(&left, "Continue");
            wait_for("the sign-in step", || left.get_step() == Step::SignIn).await;
            left.set_username("joe".into());
            left.set_password("pw".into());
            left.set_listenbrainz_user("".into());
            let users = times_asked("getUser");
            click(&left, "Sign in");
            wait_for("the ListenBrainz step", || left.get_step() == Step::Listenbrainz).await;
            click(&left, "Skip");
            wait_for("the library elsewhere", || times_asked("getUser") > users).await;
            wait_for("its account saved", || !left.get_busy()).await;
            let (users, folders) = (times_asked("getUser"), times_asked("getIndexes"));
            let pings = times_asked("ping");
            PING_HELD.store(false, Relaxed);
            wait_for("the sign-in left behind", || times_asked("ping") > pings).await;
            idle(Duration::from_millis(500)).await;
            assert_eq!(times_asked("getUser"), users, "the library elsewhere is asked for once");
            assert_eq!(times_asked("getIndexes"), folders, "the folders left asked nothing");

            slint::quit_event_loop().unwrap();
        })
        .unwrap();
        slint::run_event_loop_until_quit().unwrap();
    }
}
