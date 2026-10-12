//! A helper process the app forks before it confines itself, for work
//! its own sandbox keeps from it: the security keys
//! ([`crate::fido2::broker`]), the files the person picks (the app's
//! `file_helper`). The helper confines itself its own way, answers one
//! JSON line per request over a socket pair, and dies with the app.
//!
//! Every line is wiped once written or read, as a request may carry a
//! PIN.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use serde::Serialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

/// The app's end of a helper's socket.
pub struct Helper(Mutex<BufReader<UnixStream>>);

/// Forks a helper that runs `confine`, then answers each request with
/// `handle` until the app's end closes. Call before anything starts a
/// thread, and before the app confines itself. Returns in the app.
pub fn fork<Req, Rep, E>(
    confine: impl FnOnce(),
    handle: impl Fn(Req) -> Result<Rep, E>,
    unreadable: impl Fn(serde_json::Error) -> E,
) -> std::io::Result<Helper>
where
    Req: DeserializeOwned,
    Rep: Serialize,
    E: Serialize,
{
    let (app, helper) = UnixStream::pair()?;
    // SAFETY: called single-threaded (see above); the child only confines
    // itself and serves, then exits without unwinding into the app's code.
    match unsafe { libc::fork() } {
        -1 => Err(std::io::Error::last_os_error()),
        0 => {
            drop(app);
            // SAFETY: plain prctl; the helper dies with the app.
            unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
            confine();
            serve(helper, handle, unreadable);
            // SAFETY: the child's end; no destructors of the app run.
            unsafe { libc::_exit(0) }
        }
        _ => {
            drop(helper);
            Ok(Helper(Mutex::new(BufReader::new(app))))
        }
    }
}

/// The helper's loop: a request a line, its answer a line.
fn serve<Req, Rep, E>(
    stream: UnixStream,
    handle: impl Fn(Req) -> Result<Rep, E>,
    unreadable: impl Fn(serde_json::Error) -> E,
) where
    Req: DeserializeOwned,
    Rep: Serialize,
    E: Serialize,
{
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Zeroizing::new(String::new());
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let answer = serde_json::from_str(&line)
            .map_err(&unreadable)
            .and_then(&handle);
        let Ok(bytes) = serde_json::to_vec(&answer) else {
            return;
        };
        let mut bytes = Zeroizing::new(bytes);
        bytes.push(b'\n');
        if out.write_all(&bytes).is_err() {
            return;
        }
    }
}

impl Helper {
    /// Sends `request` and waits for its answer; `gone` when the helper
    /// is no longer there to answer.
    pub fn call<Req, Rep, E>(&self, request: &Req, gone: impl Fn() -> E) -> Result<Rep, E>
    where
        Req: Serialize,
        Rep: DeserializeOwned,
        E: DeserializeOwned,
    {
        let mut stream = self.0.lock().expect("never poisoned");
        let mut bytes = Zeroizing::new(serde_json::to_vec(request).map_err(|_| gone())?);
        bytes.push(b'\n');
        stream.get_mut().write_all(&bytes).map_err(|_| gone())?;
        let mut line = Zeroizing::new(String::new());
        if stream.read_line(&mut line).map_err(|_| gone())? == 0 {
            return Err(gone());
        }
        serde_json::from_str::<Result<Rep, E>>(&line).map_err(|_| gone())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper served on a thread instead of a forked process: the
    /// test runner has threads, so it may not fork.
    fn doubling() -> Helper {
        let (app, helper) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            serve(
                helper,
                |n: u32| {
                    if n < 100 {
                        Ok(n * 2)
                    } else {
                        Err(format!("{n} is too big"))
                    }
                },
                |e| format!("unreadable: {e}"),
            )
        });
        Helper(Mutex::new(BufReader::new(app)))
    }

    #[test]
    fn a_request_comes_back_answered() {
        let answer: Result<u32, String> = doubling().call(&21u32, || "gone".into());
        assert_eq!(answer, Ok(42));
    }

    #[test]
    fn the_helpers_refusal_comes_back_as_its_error() {
        let answer: Result<u32, String> = doubling().call(&200u32, || "gone".into());
        assert_eq!(answer, Err("200 is too big".into()));
    }

    #[test]
    fn a_request_it_cannot_read_says_so() {
        let answer: Result<u32, String> = doubling().call(&"twenty", || "gone".into());
        assert!(answer.unwrap_err().starts_with("unreadable: "));
    }
}
