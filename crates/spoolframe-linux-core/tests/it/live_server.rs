//! The SDK backend against a running Spoolframe server, end to end: what
//! the app does, without the UI. Ignored by default; run it with the
//! server's details in the environment:
//!
//! ```sh
//! SPOOLFRAME_TEST_KEY=<endpoint id> SPOOLFRAME_TEST_ADDR=<ip:port> \
//! SPOOLFRAME_TEST_USER=<user> SPOOLFRAME_TEST_PASSWORD=<password> \
//!   cargo test -p spoolframe-linux-core --test it live_server -- --ignored --nocapture
//! ```
//!
//! It keeps the device's key in memory, so it leaves nothing in the
//! keyring, and it reads media but changes no watch state or preferences.
//! The trust test makes its throwaway device trusted for the user and
//! signs it out at the end, which removes that trust again.

use std::sync::Arc;
use std::time::Duration;

use spoolframe_client_sdk as sdk;
use spoolframe_linux_core::backend::{
    Client, ConnectionState, SearchRequest, SearchScope, ServerKey, TopResult,
};
use spoolframe_linux_core::reader::{MediaReader, Spawn};
use spoolframe_linux_core::sdk::SdkClient;
use spoolframe_types::{
    AudioCodec, ClientCapabilities, Container, Item, LibraryKind, SubtitleFormat, VideoCodec,
};

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name} to run the live test"))
}

/// What a typical Linux machine with gst-libav plays, so plans are Direct.
fn capabilities() -> ClientCapabilities {
    ClientCapabilities {
        video: [
            VideoCodec::H264,
            VideoCodec::Hevc,
            VideoCodec::Av1,
            VideoCodec::Vp9,
            VideoCodec::Mpeg2,
        ]
        .into(),
        audio: [
            AudioCodec::Aac,
            AudioCodec::Ac3,
            AudioCodec::Eac3,
            AudioCodec::Dts,
            AudioCodec::TrueHd,
            AudioCodec::Flac,
            AudioCodec::Opus,
            AudioCodec::Mp3,
            AudioCodec::Pcm,
        ]
        .into(),
        containers: [Container::Matroska, Container::Mp4, Container::MpegTs].into(),
        subtitles: [
            SubtitleFormat::Srt,
            SubtitleFormat::Ass,
            SubtitleFormat::Pgs,
        ]
        .into(),
        ..ClientCapabilities::default()
    }
}

/// Waits for a backend call: the adapter runs it on the runtime.
fn run<F: std::future::Future>(runtime: &tokio::runtime::Runtime, call: F) -> F::Output {
    runtime.block_on(call)
}

fn title(item: &Item) -> String {
    match item {
        Item::Movie(m) => m.title.clone(),
        Item::Show(s) => s.title.clone(),
        Item::Season(s) => s.title.clone(),
        Item::Episode(e) => format!("S{}E{} {}", e.season, e.episode, e.title),
        Item::Artist(a) => a.name.clone(),
        Item::Album(a) => a.title.clone(),
        Item::Track(t) => t.title.clone(),
    }
}

/// A client opened before it has heard anything lists the server once
/// discovery says the list changed, without being given an address.
#[test]
#[ignore = "needs a running Spoolframe server on this LAN; see the module docs"]
fn the_server_list_changes_when_the_lan_server_is_heard() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let device = sdk::Device {
        platform: sdk::types::Platform::Linux,
        name: "spoolframe-linux discovery test".into(),
        client_name: "spoolframe-linux".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        capabilities: capabilities(),
    };
    let client = runtime
        .block_on(sdk::Client::open(
            Arc::new(sdk::MemoryStorage::default()),
            device,
        ))
        .unwrap();
    let client = SdkClient::new(client, runtime.handle().clone());
    let key = ServerKey(env("SPOOLFRAME_TEST_KEY"));
    let mut changes = client.servers_changed();
    let heard = runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let servers = client.servers().await.unwrap();
                if servers.iter().any(|s| s.key == key && s.on_network) {
                    return servers.len();
                }
                changes.changed().await.expect("the client is open");
            }
        })
        .await
    });
    let listed = heard.expect("the server is listed within ten seconds");
    eprintln!("discovery: {listed} servers listed after a change");
}

/// A new device, with its key in memory, for the server in the
/// environment.
fn new_device(runtime: &tokio::runtime::Runtime) -> (SdkClient, ServerKey) {
    let device = sdk::Device {
        platform: sdk::types::Platform::Linux,
        name: "spoolframe-linux live test".into(),
        client_name: "spoolframe-linux".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        capabilities: capabilities(),
    };
    let client = runtime
        .block_on(sdk::Client::open(
            Arc::new(sdk::MemoryStorage::default()),
            device,
        ))
        .unwrap();
    (
        SdkClient::new(client, runtime.handle().clone()),
        ServerKey(env("SPOOLFRAME_TEST_KEY")),
    )
}

/// P4-01 as the app uses it: a sign-in that asks to be remembered, the
/// remembered profile resumed without a password, opening at start, and
/// signing out forgetting the device.
#[test]
#[ignore = "needs a running Spoolframe server; see the module docs"]
fn a_remembered_profile_resumes_and_signing_out_forgets_it() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (client, key) = new_device(&runtime);
    run(
        &runtime,
        client.add_address(&key, &env("SPOOLFRAME_TEST_ADDR")),
    )
    .unwrap();
    run(&runtime, client.pin(&key, false)).unwrap();
    let user = env("SPOOLFRAME_TEST_USER");
    let profile = run(
        &runtime,
        client.sign_in(&key, &user, &env("SPOOLFRAME_TEST_PASSWORD"), true),
    )
    .unwrap();

    let remembered = run(&runtime, client.trusted_profiles(&key)).unwrap();
    let mine = remembered
        .iter()
        .find(|t| t.username == user)
        .unwrap_or_else(|| panic!("remembering was asked, but: {remembered:?}"))
        .clone();
    eprintln!(
        "remembered: {} (opens at start: {})",
        mine.username, mine.auto_login
    );
    let listed = run(&runtime, client.profiles(&key)).unwrap();
    assert!(
        listed
            .iter()
            .any(|p| p.username == user && p.trust.is_some()),
        "Who's watching does not show it remembered: {listed:?}"
    );

    // Just signed in, so turning auto-login on needs no confirming.
    let session = client.session(&profile).unwrap();
    run(&runtime, session.set_device_trust(None, true, true)).unwrap();
    let devices = run(&runtime, session.devices()).unwrap();
    let here = devices.iter().find(|d| d.this_device).unwrap();
    assert!(here.trusted && here.auto_login, "this device: {here:?}");
    eprintln!("this device: trusted, opens at start");

    let resumed = run(&runtime, client.resume_profile(&key, &mine.user_id)).unwrap();
    let libraries = run(&runtime, client.session(&resumed).unwrap().libraries()).unwrap();
    eprintln!("resumed without a password: {} libraries", libraries.len());

    run(&runtime, client.session(&resumed).unwrap().sign_out()).unwrap();
    let remembered = run(&runtime, client.trusted_profiles(&key)).unwrap();
    assert!(
        remembered.iter().all(|t| t.username != user),
        "signing out did not forget the device: {remembered:?}"
    );
    assert!(
        run(&runtime, client.resume_profile(&key, &mine.user_id)).is_err(),
        "a signed-out profile still resumes"
    );
    eprintln!("signed out: forgotten, and no longer resumes");
}

/// PX-14: the server's passkey requests name its own relying party,
/// `<server id>.spoolframe.app`, and the origin under it. Begun only: no
/// passkey is made, and the challenges go unused.
#[test]
#[ignore = "needs a running Spoolframe server; see the module docs"]
fn passkeys_are_asked_for_the_servers_own_relying_party() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (client, key) = new_device(&runtime);
    run(
        &runtime,
        client.add_address(&key, &env("SPOOLFRAME_TEST_ADDR")),
    )
    .unwrap();
    run(&runtime, client.pin(&key, false)).unwrap();
    let sign_in = run(&runtime, client.begin_passkey_sign_in(&key, None)).unwrap();
    let profile = run(
        &runtime,
        client.sign_in(
            &key,
            &env("SPOOLFRAME_TEST_USER"),
            &env("SPOOLFRAME_TEST_PASSWORD"),
            false,
        ),
    )
    .unwrap();
    let session = client.session(&profile).unwrap();
    let creation = run(&runtime, session.begin_passkey_registration()).unwrap();
    let id = creation
        .rp_id
        .strip_suffix(".spoolframe.app")
        .unwrap_or_else(|| panic!("not a server's relying party: {}", creation.rp_id));
    assert!(
        spoolframe_protocol::ServerId::parse(id).is_ok()
            && creation.origin == format!("https://{}", creation.rp_id)
            && (sign_in.rp_id.as_str(), sign_in.origin.as_str())
                == (creation.rp_id.as_str(), creation.origin.as_str()),
        "creation {} at {}; sign-in {} at {}",
        creation.rp_id,
        creation.origin,
        sign_in.rp_id,
        sign_in.origin
    );
    eprintln!("passkeys for {} at {}", creation.rp_id, creation.origin);
    run(&runtime, session.sign_out()).unwrap();
}

/// A device that asks to be remembered, approved by a signed-in one that
/// unticks the box: it is signed in just this once (P4-01, P4-02).
#[test]
#[ignore = "needs a running Spoolframe server; see the module docs"]
fn the_approver_decides_whether_a_device_is_remembered() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let user = env("SPOOLFRAME_TEST_USER");
    let (approver, key) = new_device(&runtime);
    let (asking, _) = new_device(&runtime);
    for client in [&approver, &asking] {
        run(
            &runtime,
            client.add_address(&key, &env("SPOOLFRAME_TEST_ADDR")),
        )
        .unwrap();
        run(&runtime, client.pin(&key, false)).unwrap();
    }
    let profile = run(
        &runtime,
        approver.sign_in(&key, &user, &env("SPOOLFRAME_TEST_PASSWORD"), false),
    )
    .unwrap();
    let session = approver.session(&profile).unwrap();

    let begun = run(&runtime, asking.begin_device_sign_in(&key, true)).unwrap();
    let code = begun.display_code.replace('-', "");
    let request = run(&runtime, session.lookup_device_sign_in(&code)).unwrap();
    assert!(
        request.remember,
        "the request lost its remember: {request:?}"
    );
    run(&runtime, session.approve_device_sign_in(&code, false)).unwrap();
    let (signed_in, remembered) = run(&runtime, asking.await_device_sign_in(&begun)).unwrap();
    assert_eq!(signed_in.username, user);
    assert_eq!(remembered, Some(false), "the approver unticked the box");
    assert!(
        run(&runtime, asking.trusted_profiles(&key))
            .unwrap()
            .iter()
            .all(|t| t.username != user),
        "a device signed in just this once is remembered"
    );
    eprintln!("approved, not remembered: {}", signed_in.username);
    for (client, profile) in [(&asking, &signed_in), (&approver, &profile)] {
        run(&runtime, client.session(profile).unwrap().sign_out()).unwrap();
    }
}

#[test]
#[ignore = "needs a running Spoolframe server; see the module docs"]
fn the_app_backend_works_against_a_live_server() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (client, key) = new_device(&runtime);

    // Add by key and address, as "Add a server by its key…" does.
    let server = run(
        &runtime,
        client.add_address(&key, &env("SPOOLFRAME_TEST_ADDR")),
    )
    .unwrap();
    eprintln!("server: {} ({})", server.name, key.short());
    // Adding remembers an address only: the key is pinned on the user's
    // word, after the fingerprint check (P1-21).
    let server = run(&runtime, client.pin(&key, false)).unwrap();
    assert!(server.pinned, "pinning pins the server");

    let profile = run(
        &runtime,
        client.sign_in(
            &key,
            &env("SPOOLFRAME_TEST_USER"),
            &env("SPOOLFRAME_TEST_PASSWORD"),
            false,
        ),
    )
    .unwrap();
    eprintln!("signed in: {} ({})", profile.display_name, profile.username);
    let profiles = run(&runtime, client.profiles(&key)).unwrap();
    assert!(
        profiles.contains(&profile),
        "the new session is a profile: {profiles:?}"
    );
    let state = *client.connection(&key).borrow();
    let session = client.session(&profile).unwrap();

    let libraries = run(&runtime, session.libraries()).unwrap();
    eprintln!(
        "libraries: {:?}",
        libraries.iter().map(|l| &l.title).collect::<Vec<_>>()
    );
    assert!(!libraries.is_empty());

    let home = run(&runtime, session.home()).unwrap();
    eprintln!(
        "home: {} continue, {} next up, {} recently added",
        home.continue_watching.len(),
        home.next_up.len(),
        home.recently_added.len()
    );

    // A movie, or an episode of the first show: something that plays.
    let mut playable = None;
    for library in &libraries {
        let (items, total) = run(&runtime, session.items(&library.id, 0, 5)).unwrap();
        eprintln!(
            "{}: {total} items, first {:?}",
            library.title,
            items.first().map(title)
        );
        if playable.is_some() || items.is_empty() {
            continue;
        }
        playable = match library.kind {
            LibraryKind::Movies => items.into_iter().next(),
            LibraryKind::Shows => {
                let (_, seasons) = run(&runtime, session.item(items[0].id())).unwrap();
                let season = seasons.into_iter().next().expect("a show has a season");
                let (_, episodes) = run(&runtime, session.item(season.id())).unwrap();
                episodes.into_iter().next()
            }
            LibraryKind::Music => None,
        };
    }
    let item = playable.expect("a movie or an episode to play");
    eprintln!("playing: {}", title(&item));

    let request = SearchRequest {
        query: title(&item).chars().take(4).collect(),
        limit: 10,
        scope: SearchScope::All,
        kind: None,
        library: None,
    };
    let found = run(&runtime, session.search(&request)).unwrap();
    eprintln!(
        "search: top {:?}, {} groups",
        found.top.as_ref().map(|top| match top {
            TopResult::Item(item) => title(item),
            TopResult::Person(person) => person.name.clone(),
        }),
        found.groups.len()
    );

    let tracks = run(&runtime, session.tracks(item.id())).unwrap();
    eprintln!(
        "tracks: {} audio, {} subtitles",
        tracks.audio.len(),
        tracks.subtitles.len()
    );
    let (plan, chosen) = run(&runtime, session.playback_plan(item.id(), None)).unwrap();
    eprintln!("plan: {plan:?}\nchosen: {chosen:?}");

    let start = run(&runtime, session.start_playback(item.id(), None, None)).unwrap();
    eprintln!("started: {} bytes", start.size);
    assert!(start.size > 0);
    let selection = start.chosen.map(|c| c.selection).unwrap_or_default();
    let handle = runtime.handle().clone();
    let spawn: Spawn = Arc::new(move |future| {
        handle.spawn(future);
    });
    let reader = MediaReader::new(
        runtime.block_on(async {
            spoolframe_client_sdk::ManagedPlayback::adopt(
                spoolframe_linux_core::playback::backend(session.clone()),
                item.id().clone(),
                Some(selection),
                Default::default(),
                start,
            )
        }),
        spawn,
        None,
    );
    let head = reader.read(0, 64 * 1024).unwrap();
    assert_eq!(head.len(), 64 * 1024);
    let middle = reader.read(reader.size() / 2, 4096).unwrap();
    assert_eq!(middle.len(), 4096);
    let tail = reader.read(reader.size() - 100, 4096).unwrap();
    assert_eq!(tail.len(), 100, "a read at the end returns what is left");
    eprintln!("read: start, middle and end of {} bytes", reader.size());

    let artwork = libraries.iter().find_map(|library| {
        let (items, _) = run(&runtime, session.items(&library.id, 0, 5)).ok()?;
        items.into_iter().find_map(|i| match i {
            Item::Movie(m) => m.artwork.into_iter().next(),
            Item::Show(s) => s.artwork.into_iter().next(),
            _ => None,
        })
    });
    match artwork {
        Some(art) => {
            let bytes = run(&runtime, session.artwork(&art.id, sdk::ArtworkSize::Small)).unwrap();
            eprintln!("artwork: {} bytes", bytes.len());
            assert!(!bytes.is_empty());
        }
        None => eprintln!("artwork: none of the first items has any"),
    }

    // The connection was used, so it is up now.
    std::thread::sleep(Duration::from_millis(200));
    let now = *client.connection(&key).borrow();
    eprintln!("connection: {state:?} at first, {now:?} after use");
    assert_eq!(now, ConnectionState::Connected);
}
