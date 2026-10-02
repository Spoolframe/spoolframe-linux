//! The SDK backend against a running Spoolframe server, end to end: what
//! the app does, without the UI. Ignored by default; run it with the
//! server's details in the environment:
//!
//! ```sh
//! SPOOLFRAME_TEST_KEY=<endpoint id> SPOOLFRAME_TEST_ADDR=<ip:port> \
//! SPOOLFRAME_TEST_USER=<user> SPOOLFRAME_TEST_PASSWORD=<password> \
//!   cargo test -p spoolframe-linux-core --test live_server -- --ignored --nocapture
//! ```
//!
//! It keeps the device's key in memory, so it leaves nothing in the
//! keyring, and it reads media but changes no watch state or preferences.

use std::sync::Arc;
use std::time::Duration;

use spoolframe_client_sdk as sdk;
use spoolframe_linux_core::backend::{Client, ConnectionState, ServerKey};
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

#[test]
#[ignore = "needs a running Spoolframe server; see the module docs"]
fn the_app_backend_works_against_a_live_server() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let device = sdk::Device {
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
    let client = SdkClient::new(client, runtime.handle().clone());
    let key = ServerKey(env("SPOOLFRAME_TEST_KEY"));

    // Add by key and address, as "Add a server by its key…" does.
    let server = run(
        &runtime,
        client.add_address(&key, &env("SPOOLFRAME_TEST_ADDR")),
    )
    .unwrap();
    eprintln!(
        "server: {} ({}), pinned {}",
        server.name,
        key.short(),
        server.pinned
    );
    assert!(server.pinned, "connecting pins the server");

    let profile = run(
        &runtime,
        client.sign_in(
            &key,
            &env("SPOOLFRAME_TEST_USER"),
            &env("SPOOLFRAME_TEST_PASSWORD"),
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

    let hits = run(
        &runtime,
        session.search(&title(&item).chars().take(4).collect::<String>(), 10),
    )
    .unwrap();
    eprintln!("search: {} hits", hits.len());

    let tracks = run(&runtime, session.tracks(item.id())).unwrap();
    eprintln!(
        "tracks: {} audio, {} subtitles",
        tracks.audio.len(),
        tracks.subtitles.len()
    );
    let (plan, chosen) = run(&runtime, session.playback_plan(item.id(), None)).unwrap();
    eprintln!("plan: {plan:?}\nchosen: {chosen:?}");

    let start = run(&runtime, session.start_playback(item.id(), None)).unwrap();
    eprintln!("started: {} bytes", start.size);
    assert!(start.size > 0);
    let selection = start.chosen.map(|c| c.selection).unwrap_or_default();
    let handle = runtime.handle().clone();
    let spawn: Spawn = Arc::new(move |future| {
        handle.spawn(future);
    });
    let reader = MediaReader::new(session.clone(), item.id().clone(), selection, start, spawn);
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
