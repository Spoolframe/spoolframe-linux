//! Saving and opening a file the person picks (P7-08's identity record),
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

use std::io::{BufRead, BufReader, Read as _, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use gtk::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};

/// The most a file opened here may be: an identity record is a few
/// kilobytes.
const MAX_OPEN: u64 = 1 << 20;

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

static HELPER: OnceLock<Mutex<BufReader<UnixStream>>> = OnceLock::new();

/// Forks the helper. Call before anything starts a thread, and before
/// the app confines itself; the helper runs `confine` then serves.
pub fn start(confine: impl FnOnce()) -> std::io::Result<()> {
    let (app, helper) = UnixStream::pair()?;
    // SAFETY: called single-threaded, as the key helper's start; the
    // child confines itself and serves, then exits without unwinding
    // into the app's code.
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error()),
        0 => {
            drop(app);
            // SAFETY: plain prctl; the helper dies with the app.
            unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
            confine();
            serve(helper);
            // SAFETY: the child's end; no destructors of the app run.
            unsafe { libc::_exit(0) }
        }
        _ => {
            drop(helper);
            let _ = HELPER.set(Mutex::new(BufReader::new(app)));
            Ok(())
        }
    }
}

fn serve(stream: UnixStream) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        let answer: Result<Reply, String> = match serde_json::from_str(&line) {
            Ok(request) => handle(request),
            Err(e) => Err(format!("the file helper could not read: {e}")),
        };
        let Ok(mut bytes) = serde_json::to_vec(&answer) else {
            return;
        };
        bytes.push(b'\n');
        if out.write_all(&bytes).is_err() {
            return;
        }
    }
}

fn handle(request: Request) -> Result<Reply, String> {
    match request {
        Request::Save { title, name, bytes } => save_here(&title, &name, &bytes).map(Reply::Saved),
        Request::Open { title, filter } => open_here(&title, &filter).map(Reply::Opened),
    }
}

fn call(request: &Request) -> Result<Reply, String> {
    let Some(helper) = HELPER.get() else {
        return handle_unhelped(request);
    };
    let gone = || "the file helper stopped; restart Spoolframe".to_owned();
    let mut stream = helper.lock().expect("never poisoned");
    let mut bytes = serde_json::to_vec(request).map_err(|_| gone())?;
    bytes.push(b'\n');
    stream.get_mut().write_all(&bytes).map_err(|_| gone())?;
    let mut line = String::new();
    if stream.read_line(&mut line).map_err(|_| gone())? == 0 {
        return Err(gone());
    }
    serde_json::from_str::<Result<Reply, String>>(&line).map_err(|_| gone())?
}

/// No helper (unconfined, or it could not start): the app asks itself.
fn handle_unhelped(request: &Request) -> Result<Reply, String> {
    match request {
        Request::Save { title, name, bytes } => save_here(title, name, bytes).map(Reply::Saved),
        Request::Open { title, filter } => open_here(title, filter).map(Reply::Opened),
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

fn save_here(title: &str, name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
    let options = glib::VariantDict::new(None);
    options.insert("current_name", name);
    let Some(path) = pick("SaveFile", title, options)? else {
        return Ok(None);
    };
    std::fs::write(&path, bytes).map_err(|e| format!("couldn’t save {}: {e}", path.display()))?;
    Ok(Some(file_name(&path)))
}

fn open_here(title: &str, filter: &Filter) -> Result<Option<Opened>, String> {
    let options = glib::VariantDict::new(None);
    let filters = vec![
        (filter.name.clone(), vec![(0u32, filter.pattern.clone())]),
        ("All files".to_owned(), vec![(0u32, "*".to_owned())]),
    ];
    options.insert_value("filters", &filters.to_variant());
    let Some(path) = pick("OpenFile", title, options)? else {
        return Ok(None);
    };
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
    // Subscribed before asking, so the answer cannot come first.
    let answer: std::rc::Rc<std::cell::RefCell<Option<(u32, glib::Variant)>>> = Default::default();
    let main = glib::MainLoop::new(Some(context), false);
    let subscription = {
        let (answer, main) = (answer.clone(), main.clone());
        bus.subscribe_to_signal(
            Some("org.freedesktop.portal.Desktop"),
            Some("org.freedesktop.portal.Request"),
            Some("Response"),
            Some(&path),
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
    let arguments = ("", title, options.end()).to_variant();
    let asked = bus.call_sync(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileChooser",
        method,
        Some(&arguments),
        Some(glib::VariantTy::new("(o)").expect("a valid type")),
        gio::DBusCallFlags::NONE,
        -1,
        None::<&gio::Cancellable>,
    );
    if let Err(e) = asked {
        drop(subscription);
        return Err(format!("the desktop has no file chooser portal: {e}"));
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
