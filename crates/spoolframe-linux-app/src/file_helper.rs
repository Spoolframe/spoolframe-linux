//! Saving and opening a file the person picks (P7-08's identity record,
//! and PX-53's playlist cover, a picture opened only),
//! through a helper the app forks before it confines itself. Landlock
//! keeps the app out of the home directory, so a file chosen in GTK's
//! own chooser could be neither written nor read. The helper is not
//! confined to files, but it touches only the one the person picked: it
//! shows the desktop portal's Save or Open dialog itself and works on
//! the path the portal hands back. The app only asks it to ask, so even
//! a compromised app reads or writes nothing the person did not pick. It
//! runs under the app's seccomp filter, so it starts no program, and it
//! ends when the app does.
//!
//! One JSON line per request over a socket pair, as the key helper
//! ([`spoolframe_linux_core::fido2::broker`]). Unconfined
//! (`SPOOLFRAME_NO_SANDBOX=1`), the app asks the portal itself.

use std::io::Read as _;
use std::path::PathBuf;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};

/// The most a file saved or opened here may be: an identity record is a
/// few kilobytes.
const MAX_OPEN: u64 = 1 << 20;

/// The only kind of file the helper saves or opens: an identity record.
/// The helper checks it itself, so an app that was taken over cannot
/// offer `../.ssh/authorized_keys` as the name, nor take back whatever
/// the person was steered to pick.
pub const RECORD_EXTENSION: &str = "spoolframe-identity";

/// Whether `name` may be offered in the Save dialog: a plain file name,
/// not hidden, ending in the record's extension.
pub fn offered_name_ok(name: &str) -> bool {
    !name.contains('/') && !name.starts_with('.') && is_record(name)
}

/// Whether a file named `name` is an identity record, by its extension.
pub fn is_record(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|ext| ext == RECORD_EXTENSION)
}

fn not_a_record(name: &str) -> String {
    format!("{name} isn’t a .{RECORD_EXTENSION} file: Spoolframe saves and opens only those")
}

/// What to show in the Open dialog's filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    /// "Spoolframe identity".
    pub name: String,
    /// "*.spoolframe-identity".
    pub pattern: String,
}

#[derive(Debug, Serialize, Deserialize)]
enum Request {
    Save {
        title: String,
        name: String,
        bytes: Vec<u8>,
    },
    Open {
        title: String,
        filter: Filter,
    },
    /// A picture for a playlist's cover (PX-53): the helper offers and
    /// opens only JPEG, PNG, WebP and HEIC files, and reads at most
    /// [`MAX_IMAGE`] of one.
    OpenImage {
        title: String,
    },
}

/// The most a picture opened here may be: what the server takes for a
/// cover.
pub const MAX_IMAGE: u64 = spoolframe_linux_core::backend::MAX_ART_BYTES;

/// The pictures the helper opens, by extension, lower case.
const IMAGE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "heic", "heif"];

/// Whether a file named `name` is a picture the helper opens, by its
/// extension in any case.
pub fn is_image(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Whether `bytes` start as a JPEG, PNG, WebP or HEIC/HEIF does, so the
/// helper hands back nothing else, whatever the file is named.
pub fn looks_like_image(bytes: &[u8]) -> bool {
    let at = |range: std::ops::Range<usize>| bytes.get(range);
    bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || (at(0..4) == Some(b"RIFF") && at(8..12) == Some(b"WEBP"))
        || (at(4..8) == Some(b"ftyp")
            && at(8..12).is_some_and(|brand| {
                [
                    b"heic", b"heix", b"hevc", b"heim", b"heis", b"mif1", b"msf1",
                ]
                .iter()
                .any(|b| brand == *b)
            }))
}

/// A file opened: its name, without the folder, and its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
enum Reply {
    /// Where it was saved, by name; `None`: they cancelled.
    Saved(Option<String>),
    Opened(Option<Opened>),
}

static HELPER: OnceLock<spoolframe_linux_core::helper::Helper> = OnceLock::new();

/// Forks the helper (see [`spoolframe_linux_core::helper::fork`]): it
/// runs `confine`, then serves until the app goes.
pub fn start(confine: impl FnOnce()) -> std::io::Result<()> {
    let helper = spoolframe_linux_core::helper::fork(
        confine,
        |request: Request| handle(&request),
        |e| format!("the file helper could not read: {e}"),
    )?;
    let _ = HELPER.set(helper);
    Ok(())
}

fn handle(request: &Request) -> Result<Reply, String> {
    match request {
        Request::Save { title, name, bytes } => save_here(title, name, bytes).map(Reply::Saved),
        Request::Open { title, filter } => open_here(title, filter).map(Reply::Opened),
        Request::OpenImage { title } => open_image_here(title).map(Reply::Opened),
    }
}

/// Through the helper; without one (unconfined, or it could not start),
/// the app asks itself.
fn call(request: &Request) -> Result<Reply, String> {
    match HELPER.get() {
        Some(helper) => helper.call(request, || {
            "the file helper stopped; restart Spoolframe".to_owned()
        }),
        None => handle(request),
    }
}

/// Asks where to save `bytes`, offering `name`, and saves them there.
/// Blocks until the person answers: call off the main loop. The file's
/// name, or `None` if they cancelled.
pub fn save(title: &str, name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
    match call(&Request::Save {
        title: title.to_owned(),
        name: name.to_owned(),
        bytes: bytes.to_vec(),
    })? {
        Reply::Saved(saved) => Ok(saved),
        Reply::Opened(_) => Err("the file helper answered something else".into()),
    }
}

/// Asks for a file to open, and reads it. Blocks: call off the main
/// loop. `None` if they cancelled.
pub fn open(title: &str, filter: &Filter) -> Result<Option<Opened>, String> {
    match call(&Request::Open {
        title: title.to_owned(),
        filter: filter.clone(),
    })? {
        Reply::Opened(opened) => Ok(opened),
        Reply::Saved(_) => Err("the file helper answered something else".into()),
    }
}

/// Asks for a picture to open, and reads it. Blocks: call off the main
/// loop. `None` if they cancelled.
pub fn open_image(title: &str) -> Result<Option<Opened>, String> {
    match call(&Request::OpenImage {
        title: title.to_owned(),
    })? {
        Reply::Opened(opened) => Ok(opened),
        Reply::Saved(_) => Err("the file helper answered something else".into()),
    }
}

fn save_here(title: &str, name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
    if !offered_name_ok(name) {
        return Err(not_a_record(name));
    }
    if bytes.len() as u64 > MAX_OPEN {
        return Err("that is too big to be an identity record".into());
    }
    let options = glib::VariantDict::new(None);
    options.insert("current_name", name);
    let Some(path) = pick("SaveFile", title, options)? else {
        return Ok(None);
    };
    if !is_record(&file_name(&path)) {
        return Err(not_a_record(&file_name(&path)));
    }
    std::fs::write(&path, bytes).map_err(|e| format!("couldn’t save {}: {e}", path.display()))?;
    Ok(Some(file_name(&path)))
}

fn open_here(title: &str, filter: &Filter) -> Result<Option<Opened>, String> {
    let options = glib::VariantDict::new(None);
    // Only records: the helper opens nothing else.
    let filters = vec![(filter.name.clone(), vec![(0u32, filter.pattern.clone())])];
    options.insert_value("filters", &filters.to_variant());
    let Some(path) = pick("OpenFile", title, options)? else {
        return Ok(None);
    };
    if !is_record(&file_name(&path)) {
        return Err(not_a_record(&file_name(&path)));
    }
    let file =
        std::fs::File::open(&path).map_err(|e| format!("couldn’t open {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_OPEN + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("couldn’t read {}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_OPEN {
        return Err(format!(
            "{} is too big to be an identity record",
            file_name(&path)
        ));
    }
    Ok(Some(Opened {
        name: file_name(&path),
        bytes,
    }))
}

fn open_image_here(title: &str) -> Result<Option<Opened>, String> {
    let options = glib::VariantDict::new(None);
    let patterns: Vec<(u32, String)> = IMAGE_EXTENSIONS
        .iter()
        .flat_map(|ext| {
            [
                format!("*.{ext}"),
                format!("*.{}", ext.to_ascii_uppercase()),
            ]
        })
        .map(|pattern| (0u32, pattern))
        .collect();
    let filters = vec![("Pictures".to_owned(), patterns)];
    options.insert_value("filters", &filters.to_variant());
    let Some(path) = pick("OpenFile", title, options)? else {
        return Ok(None);
    };
    let name = file_name(&path);
    if !is_image(&name) {
        return Err(format!(
            "{name} isn’t a picture: choose a JPEG, PNG, WebP or HEIC"
        ));
    }
    let file =
        std::fs::File::open(&path).map_err(|e| format!("couldn’t open {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("couldn’t read {}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_IMAGE {
        return Err(format!(
            "{name} is larger than {} MB, the most a cover may be",
            MAX_IMAGE >> 20
        ));
    }
    if !looks_like_image(&bytes) {
        return Err(format!(
            "{name} isn’t a picture: choose a JPEG, PNG, WebP or HEIC"
        ));
    }
    Ok(Some(Opened { name, bytes }))
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// The request object the portal answers on, for `token`: the bus name
/// with its colon dropped and dots as underscores, as the portal spec
/// has it.
pub fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
}

/// Shows the portal's file chooser (`method`: SaveFile or OpenFile) and
/// waits for the answer, on a main context of its own so it runs on any
/// thread. The path picked, or `None` if they cancelled.
fn pick(method: &str, title: &str, options: glib::VariantDict) -> Result<Option<PathBuf>, String> {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| pick_on(&context, method, title, options))
        .map_err(|e| e.to_string())?
}

fn pick_on(
    context: &glib::MainContext,
    method: &str,
    title: &str,
    options: glib::VariantDict,
) -> Result<Option<PathBuf>, String> {
    let bus = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)
        .map_err(|e| format!("no session bus for the file chooser: {e}"))?;
    let token = format!("spoolframe{}", glib::random_int());
    options.insert("handle_token", token.as_str());
    let path = request_path(bus.unique_name().as_deref().unwrap_or_default(), &token);
    let answer: std::rc::Rc<std::cell::RefCell<Option<(u32, glib::Variant)>>> = Default::default();
    let main = glib::MainLoop::new(Some(context), false);
    let subscribe = |on: &str| {
        let (answer, main) = (answer.clone(), main.clone());
        bus.subscribe_to_signal(
            Some("org.freedesktop.portal.Desktop"),
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(on),
            None,
            gio::DBusSignalFlags::NO_MATCH_RULE,
            move |signal| {
                if let Some((code, results)) = signal.parameters.get::<(u32, glib::VariantDict)>() {
                    *answer.borrow_mut() = Some((code, results.end()));
                }
                main.quit();
            },
        )
    };
    // Subscribed before asking, so the answer cannot come first.
    let mut subscription = subscribe(&path);
    let arguments = ("", title, options.end()).to_variant();
    let asked = bus
        .call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.FileChooser",
            method,
            Some(&arguments),
            Some(glib::VariantTy::new("(o)").expect("a valid type")),
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
        )
        .map_err(|e| format!("the desktop has no file chooser portal: {e}"))?;
    // A portal that ignored the token answers on its own path: follow it,
    // or this would wait forever. Its answer comes only once the person
    // picks, long after this.
    if let Some((answered_on,)) = asked.get::<(glib::variant::ObjectPath,)>()
        && answered_on.as_str() != path
    {
        subscription = subscribe(answered_on.as_str());
    }
    main.run();
    drop(subscription);
    let Some((code, results)) = answer.borrow_mut().take() else {
        return Ok(None);
    };
    // 0: picked; 1: cancelled; 2: anything else.
    if code != 0 {
        return Ok(None);
    }
    let uri = glib::VariantDict::new(Some(&results))
        .lookup::<Vec<String>>("uris")
        .ok()
        .flatten()
        .and_then(|uris| uris.into_iter().next())
        .ok_or("the file chooser picked nothing")?;
    gio::File::for_uri(&uri)
        .path()
        .map(Some)
        .ok_or_else(|| format!("{uri} isn’t a local file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_name_may_be_offered() {
        assert!(offered_name_ok("alice.spoolframe-identity"));
    }

    #[test]
    fn a_name_that_climbs_out_is_refused() {
        assert!(!offered_name_ok("../.ssh/authorized_keys"));
    }

    #[test]
    fn a_hidden_record_is_refused() {
        assert!(!offered_name_ok(".spoolframe-identity"));
    }

    #[test]
    fn a_file_that_is_not_a_record_is_not_opened() {
        assert!(!is_record("id_ed25519"));
    }

    #[test]
    fn a_picture_is_opened_by_its_extension_in_any_case() {
        assert!(
            ["cover.jpg", "Cover.PNG", "a.webp", "IMG_1.HEIC"]
                .into_iter()
                .all(is_image)
        );
    }

    #[test]
    fn a_file_that_is_not_a_picture_is_not_opened() {
        assert!(
            !["id_ed25519", "notes.txt", "cover.png.sh", "jpg"]
                .into_iter()
                .any(is_image)
        );
    }

    #[test]
    fn a_picture_is_known_by_its_first_bytes() {
        let heic = b"\0\0\0\x18ftypheic\0\0\0\0";
        let webp = b"RIFF\0\0\0\0WEBPVP8 ";
        assert!(
            [&b"\xff\xd8\xff\xe0"[..], b"\x89PNG\r\n\x1a\n\0", webp, heic]
                .into_iter()
                .all(looks_like_image)
        );
    }

    #[test]
    fn something_named_as_a_picture_but_not_one_is_refused() {
        assert!(
            ![&b"-----BEGIN OPENSSH"[..], b"", b"\0\0\0\x18ftypisom"]
                .into_iter()
                .any(looks_like_image)
        );
    }

    #[test]
    fn the_request_path_is_the_bus_name_escaped_then_the_token() {
        assert_eq!(
            request_path(":1.42", "spoolframe7"),
            "/org/freedesktop/portal/desktop/request/1_42/spoolframe7"
        );
    }

    #[test]
    fn a_reply_survives_the_line_between_app_and_helper() {
        let opened = Opened {
            name: "alice.spoolframe-identity".into(),
            bytes: b"{}".to_vec(),
        };
        let line =
            serde_json::to_string(&Ok::<_, String>(Reply::Opened(Some(opened.clone())))).unwrap();
        let back: Result<Reply, String> = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, Ok(Reply::Opened(Some(o))) if o == opened));
    }
}
