//! `spoolframesrc`: a GStreamer source for `spoolframe://` URIs, so GstPlay
//! plays a server's file like any other URI (P1-16). Registered in-process;
//! not a system plugin.
//!
//! The player publishes a [`MediaReader`] and gets a URI for it; playbin
//! makes this element for the URI, and the element reads through the
//! reader. It is a byte-oriented, seekable BaseSrc that knows the file's
//! size, so demuxers seek in it as in a local file.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::prelude::*;
use spoolframe_linux_core::reader::{MediaReader, ReadError};

const SCHEME: &str = "spoolframe";

/// Published readers, by the number in their URI.
static READERS: LazyLock<Mutex<HashMap<u64, Arc<MediaReader>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A reader playbin can reach by [`Published::uri`]. Unpublished when
/// dropped; an element already reading keeps its own reference.
pub struct Published {
    id: u64,
    reader: Arc<MediaReader>,
}

impl Published {
    pub fn new(reader: MediaReader) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let reader = Arc::new(reader);
        readers().insert(id, reader.clone());
        Self { id, reader }
    }

    /// The reader, for its read-ahead status.
    pub fn reader(&self) -> &MediaReader {
        &self.reader
    }

    pub fn uri(&self) -> String {
        format!("{SCHEME}://media/{}", self.id)
    }
}

impl Drop for Published {
    fn drop(&mut self) {
        readers().remove(&self.id);
    }
}

fn readers() -> std::sync::MutexGuard<'static, HashMap<u64, Arc<MediaReader>>> {
    READERS.lock().expect("the reader table is never poisoned")
}

/// Makes `spoolframesrc` available to playbin. Call once, after gst::init.
pub fn register() -> Result<(), glib::BoolError> {
    gst::Element::register(
        None,
        "spoolframesrc",
        gst::Rank::PRIMARY,
        SpoolframeSrc::static_type(),
    )
}

glib::wrapper! {
    pub struct SpoolframeSrc(ObjectSubclass<imp::SpoolframeSrc>)
        @extends gst_base::BaseSrc, gst::Element, gst::Object,
        @implements gst::URIHandler;
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SpoolframeSrc {
        uri: Mutex<Option<String>>,
        reader: Mutex<Option<Arc<MediaReader>>>,
    }

    impl SpoolframeSrc {
        fn reader(&self) -> Option<Arc<MediaReader>> {
            self.reader.lock().expect("never poisoned").clone()
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SpoolframeSrc {
        const NAME: &'static str = "SpoolframeSrc";
        type Type = super::SpoolframeSrc;
        type ParentType = gst_base::BaseSrc;
        type Interfaces = (gst::URIHandler,);
    }

    impl ObjectImpl for SpoolframeSrc {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_format(gst::Format::Bytes);
        }
    }

    impl GstObjectImpl for SpoolframeSrc {}

    impl ElementImpl for SpoolframeSrc {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "Spoolframe source",
                    "Source/Network",
                    "Reads a Spoolframe server's media through its range reads",
                    "Spoolframe",
                )
            });
            Some(&*METADATA)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                vec![
                    gst::PadTemplate::new(
                        "src",
                        gst::PadDirection::Src,
                        gst::PadPresence::Always,
                        &gst::Caps::new_any(),
                    )
                    .expect("a valid template"),
                ]
            });
            TEMPLATES.as_ref()
        }
    }

    impl BaseSrcImpl for SpoolframeSrc {
        fn start(&self) -> Result<(), gst::ErrorMessage> {
            match self.reader() {
                Some(_) => Ok(()),
                None => Err(gst::error_msg!(
                    gst::ResourceError::NotFound,
                    [
                        "no Spoolframe playback at {:?}",
                        self.uri.lock().expect("never poisoned")
                    ]
                )),
            }
        }

        fn is_seekable(&self) -> bool {
            true
        }

        fn size(&self) -> Option<u64> {
            self.reader().map(|r| r.size())
        }

        fn unlock(&self) -> Result<(), gst::ErrorMessage> {
            if let Some(reader) = self.reader() {
                reader.set_flushing(true);
            }
            Ok(())
        }

        fn unlock_stop(&self) -> Result<(), gst::ErrorMessage> {
            if let Some(reader) = self.reader() {
                reader.set_flushing(false);
            }
            Ok(())
        }

        fn fill(
            &self,
            offset: u64,
            length: u32,
            buffer: &mut gst::BufferRef,
        ) -> Result<gst::FlowSuccess, gst::FlowError> {
            let reader = self.reader().ok_or(gst::FlowError::NotNegotiated)?;
            // Through an underrun the reader reports how far it got;
            // GstPlay pauses on a buffering message under 100 and plays
            // again at 100, and shows the percentage through its signal.
            let element = self.obj();
            let mut buffering = |percent: u8| {
                let message = gst::message::Buffering::builder(percent.into())
                    .src(&*element)
                    .build();
                let _ = element.post_message(message);
            };
            let bytes = match reader.read_reporting(offset, length.into(), &mut buffering) {
                Ok(bytes) => bytes,
                Err(ReadError::Flushing) => return Err(gst::FlowError::Flushing),
                Err(ReadError::Backend(e)) => {
                    gst::element_imp_error!(self, gst::ResourceError::Read, ["{e}"]);
                    return Err(gst::FlowError::Error);
                }
            };
            if bytes.is_empty() {
                return Err(gst::FlowError::Eos);
            }
            buffer.set_size(bytes.len());
            buffer
                .copy_from_slice(0, &bytes)
                .map_err(|_| gst::FlowError::Error)?;
            Ok(gst::FlowSuccess::Ok)
        }
    }

    impl URIHandlerImpl for SpoolframeSrc {
        const URI_TYPE: gst::URIType = gst::URIType::Src;

        fn protocols() -> &'static [&'static str] {
            &[SCHEME]
        }

        fn uri(&self) -> Option<String> {
            self.uri.lock().expect("never poisoned").clone()
        }

        fn set_uri(&self, uri: &str) -> Result<(), glib::Error> {
            let id = uri
                .strip_prefix(&format!("{SCHEME}://media/"))
                .and_then(|n| n.parse::<u64>().ok())
                .ok_or_else(|| {
                    glib::Error::new(
                        gst::URIError::BadUri,
                        &format!("not a Spoolframe URI: {uri}"),
                    )
                })?;
            let reader = readers().get(&id).cloned().ok_or_else(|| {
                glib::Error::new(
                    gst::URIError::BadReference,
                    &format!("no playback at {uri}"),
                )
            })?;
            *self.reader.lock().expect("never poisoned") = Some(reader);
            *self.uri.lock().expect("never poisoned") = Some(uri.to_owned());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use spoolframe_linux_core::backend::{Client, Profile, Selection, ServerKey};
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::reader::Spawn;
    use spoolframe_types::{MediaId, MediaKind, Namespace};

    use super::*;

    /// The fake's futures finish without waiting.
    fn ready<T>(future: impl Future<Output = T>) -> T {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the fake backend never waits"),
        }
    }

    fn thread_spawn() -> Spawn {
        Arc::new(|future| {
            std::thread::spawn(move || {
                let mut future = future;
                while pin!(&mut future)
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
                {
                    std::thread::yield_now();
                }
            });
        })
    }

    /// Runs `pipeline` until end of stream; panics on an error or after
    /// `limit`.
    fn run_to_eos(pipeline: &gst::Element, limit: Duration) {
        let bus = pipeline.bus().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = bus
            .timed_pop_filtered(
                gst::ClockTime::from_nseconds(limit.as_nanos() as u64),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("the pipeline neither ended nor failed in time");
        pipeline.set_state(gst::State::Null).unwrap();
        if let gst::MessageView::Error(e) = message.view() {
            panic!("{} ({:?})", e.error(), e.debug());
        }
    }

    /// Two seconds of H.264 in Matroska.
    fn clip(path: &Path) {
        let pipeline = gst::parse::launch(&format!(
            "videotestsrc num-buffers=60 ! video/x-raw,width=320,height=240,framerate=30/1 \
             ! videoconvert ! video/x-raw,format=I420 ! x264enc ! h264parse \
             ! matroskamux ! filesink location={}",
            path.display()
        ))
        .unwrap();
        run_to_eos(&pipeline, Duration::from_secs(30));
    }

    fn published(path: PathBuf) -> Published {
        let client = FakeClient::new(Some(path));
        let profile = Profile {
            server: ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into()),
            username: "andrey".into(),
            display_name: "Andrey".into(),
        };
        let session = client.session(&profile).unwrap();
        let item = MediaId::new(Namespace::Plex, MediaKind::Movie, "1").unwrap();
        let start = ready(session.start_playback(&item, None)).unwrap();
        Published::new(MediaReader::new(
            session,
            item,
            Selection::default(),
            start,
            thread_spawn(),
        ))
    }

    #[test]
    fn playbin_plays_a_spoolframe_uri_to_the_end() {
        gst::init().unwrap();
        register().unwrap();
        let path = std::env::temp_dir().join(format!("spoolframe-src-{}.mkv", std::process::id()));
        clip(&path);
        let published = published(path);
        let playbin = gst::ElementFactory::make("playbin3")
            .property("uri", published.uri())
            .property(
                "video-sink",
                gst::ElementFactory::make("fakesink").build().unwrap(),
            )
            .property(
                "audio-sink",
                gst::ElementFactory::make("fakesink").build().unwrap(),
            )
            .build()
            .unwrap();
        run_to_eos(&playbin, Duration::from_secs(30));
    }

    #[test]
    fn a_seek_into_the_middle_continues_to_the_end() {
        gst::init().unwrap();
        register().unwrap();
        let path = std::env::temp_dir().join(format!("spoolframe-seek-{}.mkv", std::process::id()));
        clip(&path);
        let published = published(path);
        let playbin = gst::ElementFactory::make("playbin3")
            .property("uri", published.uri())
            .property(
                "video-sink",
                gst::ElementFactory::make("fakesink")
                    .property("sync", false)
                    .build()
                    .unwrap(),
            )
            .property(
                "audio-sink",
                gst::ElementFactory::make("fakesink").build().unwrap(),
            )
            .build()
            .unwrap();
        playbin.set_state(gst::State::Paused).unwrap();
        let (changed, _, _) = playbin.state(gst::ClockTime::from_seconds(20));
        changed.expect("the file prerolls");
        playbin
            .seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_seconds(1),
            )
            .unwrap();
        run_to_eos(&playbin, Duration::from_secs(30));
    }

    /// Plays a few seconds of the first movie of a running server through
    /// playbin: the whole playback path but the window. Set the same
    /// variables as spoolframe-linux-core's live test and run with
    /// `--ignored`.
    #[test]
    #[ignore = "needs a running Spoolframe server"]
    fn a_live_servers_movie_plays_through_playbin() {
        use spoolframe_client_sdk as sdk;
        use spoolframe_linux_core::sdk::SdkClient;

        let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        gst::init().unwrap();
        register().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let device = sdk::Device {
            name: "spoolframe-linux playbin test".into(),
            client_name: "spoolframe-linux".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            capabilities: crate::capabilities::capabilities(),
        };
        let client = runtime
            .block_on(sdk::Client::open(
                Arc::new(sdk::MemoryStorage::default()),
                device,
            ))
            .unwrap();
        let client = SdkClient::new(client, runtime.handle().clone());
        let key = ServerKey(env("SPOOLFRAME_TEST_KEY"));
        runtime
            .block_on(client.add_address(&key, &env("SPOOLFRAME_TEST_ADDR")))
            .unwrap();
        let profile = runtime
            .block_on(client.sign_in(
                &key,
                &env("SPOOLFRAME_TEST_USER"),
                &env("SPOOLFRAME_TEST_PASSWORD"),
            ))
            .unwrap();
        let session = client.session(&profile).unwrap();
        let libraries = runtime.block_on(session.libraries()).unwrap();
        let movies = libraries
            .iter()
            .find(|l| l.kind == spoolframe_types::LibraryKind::Movies)
            .expect("a movie library");
        let (items, _) = runtime.block_on(session.items(&movies.id, 0, 1)).unwrap();
        let item = items.into_iter().next().expect("a movie");
        let start = runtime
            .block_on(session.start_playback(item.id(), None))
            .unwrap();
        eprintln!(
            "playing {} ({} bytes, {:?})",
            item.id(),
            start.size,
            start.plan
        );
        let selection = start.chosen.map(|c| c.selection).unwrap_or_default();
        let handle = runtime.handle().clone();
        let spawn: Spawn = Arc::new(move |future| {
            handle.spawn(future);
        });
        let published = Published::new(MediaReader::new(
            session,
            item.id().clone(),
            selection,
            start,
            spawn,
        ));
        let playbin = gst::ElementFactory::make("playbin3")
            .property("uri", published.uri())
            .property(
                "video-sink",
                gst::ElementFactory::make("fakesink")
                    .property("sync", true)
                    .build()
                    .unwrap(),
            )
            .property(
                "audio-sink",
                gst::ElementFactory::make("fakesink")
                    .property("sync", true)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let bus = playbin.bus().unwrap();
        playbin.set_state(gst::State::Playing).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut played = gst::ClockTime::ZERO;
        while std::time::Instant::now() < deadline && played < gst::ClockTime::from_seconds(3) {
            if let Some(message) = bus.timed_pop_filtered(
                gst::ClockTime::from_mseconds(200),
                &[gst::MessageType::Error, gst::MessageType::Eos],
            ) {
                playbin.set_state(gst::State::Null).unwrap();
                match message.view() {
                    gst::MessageView::Error(e) => panic!("{} ({:?})", e.error(), e.debug()),
                    _ => panic!("the movie ended before three seconds"),
                }
            }
            played = playbin
                .query_position::<gst::ClockTime>()
                .unwrap_or_default();
        }
        playbin.set_state(gst::State::Null).unwrap();
        eprintln!("played {played}");
        assert!(
            played >= gst::ClockTime::from_seconds(3),
            "only {played} played"
        );
    }
}
