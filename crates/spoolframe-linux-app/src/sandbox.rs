//! The whole app confined at start (C-15), before GTK, GStreamer or the
//! SDK start a thread, so every thread after is confined too. A crafted
//! file from a hostile server that takes over a demuxer then runs with
//! what the app needs, not with all the user has:
//!
//! - **Landlock**, every file right this kernel knows handled: the system's
//!   programs and libraries to read and run; the app's own config, data and
//!   cache directories, the runtime directory (the Wayland, PipeWire and
//!   D-Bus sockets) and a few caches (GStreamer's registry, fontconfig's,
//!   the GPU drivers' shader caches) to write; the desktop's settings,
//!   fonts and icons to read; the GPU's nodes and the security keys
//!   plugged in at start. Not the rest of `$HOME` (`~/.ssh`, browser
//!   profiles, documents), and not `/proc` beyond the app's own.
//! - **seccomp**: no new programs (`execve`, and no process but threads),
//!   and none of the calls an escape uses: `ptrace`, other processes'
//!   memory, mounts, namespaces, keyrings, `bpf`, `io_uring`, …
//!   The network stays open: the app talks to its servers.
//!
//! Links open through the desktop portal, which starts the browser outside
//! the app (see [`crate::browser`]).
//!
//! `SPOOLFRAME_NO_SANDBOX=1` turns it off, for debugging. The same rules as
//! the server's media tools (`spoolframe-media`'s `sandbox.rs`), for a
//! process that confines itself rather than a child.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use landlock::{
    ABI, Access as _, AccessFs, BitFlags, CompatLevel, Compatible as _, PathBeneath, Ruleset,
    RulesetAttr as _, RulesetCreatedAttr as _, RulesetStatus, Scope,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

/// Set to 1, the app runs unconfined.
pub const OPT_OUT: &str = "SPOOLFRAME_NO_SANDBOX";

/// The oldest Landlock ABI used: 3 (Linux 6.2) is the first to refuse
/// truncate(2) outside the rules.
const MIN_LANDLOCK_ABI: i32 = 3;

/// What the app runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Confinement {
    /// Landlock's ABI, when files are confined.
    pub landlock: Option<i32>,
    /// Whether the seccomp filter is loaded.
    pub seccomp: bool,
}

/// The security keys plugged in when Landlock confined the app; unset
/// when it is not confined. A key plugged in later cannot be opened.
static KEYS_AT_START: OnceLock<usize> = OnceLock::new();

/// How many security keys the app may open, when Landlock confines it to
/// those plugged in at start; `None` when any key can be opened.
pub fn keys_at_start() -> Option<usize> {
    KEYS_AT_START.get().copied()
}

/// What a place is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    /// Read and run: programs and libraries.
    Run,
    /// Read only.
    Read,
    /// Read and write, never run.
    Write,
    /// A device to read, write and control (ioctl).
    Device,
}

/// Where the app's own things and the desktop's are.
#[derive(Debug, Clone)]
struct Home {
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    cache: Option<PathBuf>,
    runtime: Option<PathBuf>,
    /// The app's own directories, made before the rules name them.
    app: Vec<PathBuf>,
    /// Other places the environment names: `XDG_DATA_DIRS`,
    /// `XDG_CONFIG_DIRS`, an absolute `WAYLAND_DISPLAY`, `XAUTHORITY`.
    named: Vec<(PathBuf, Use)>,
}

impl Home {
    fn from_env() -> Self {
        let var = |key| std::env::var_os(key).map(PathBuf::from);
        let home = var("HOME").filter(|h| h.is_absolute());
        let under = |key, default: &str| {
            var(key)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(default)))
        };
        let mut named = Vec::new();
        for key in ["XDG_DATA_DIRS", "XDG_CONFIG_DIRS"] {
            for dir in std::env::split_paths(&std::env::var_os(key).unwrap_or_default()) {
                named.push((dir, Use::Read));
            }
        }
        if let Some(display) = var("WAYLAND_DISPLAY").filter(|d| d.is_absolute()) {
            named.push((display, Use::Write));
        }
        if let Some(auth) = var("XAUTHORITY") {
            named.push((auth, Use::Read));
        }
        use spoolframe_linux_core::paths;
        Self {
            config: under("XDG_CONFIG_HOME", ".config"),
            data: under("XDG_DATA_HOME", ".local/share"),
            cache: under("XDG_CACHE_HOME", ".cache"),
            runtime: var("XDG_RUNTIME_DIR").filter(|p| p.is_absolute()),
            app: [
                paths::config_dir(),
                paths::data_dir(),
                paths::cache_dir(),
                // Made now if it is a private one under /tmp.
                paths::runtime_dir(),
            ]
            .into_iter()
            .flatten()
            .collect(),
            home,
            named,
        }
    }
}

/// The system's programs and libraries, to read and run.
const SYSTEM: &[&str] = &[
    "/nix/store",
    "/run/current-system/sw",
    // NixOS's GPU drivers.
    "/run/opengl-driver",
    "/run/opengl-driver-32",
    // Fonts, TLS roots, the loader's cache, the time zone, name service.
    "/etc",
    "/usr",
    "/bin",
    "/lib",
    "/lib64",
];

/// Read only, outside the home directory.
const READ: &[&str] = &[
    // The GPU's PCI ids for VA-API and Mesa; udev's view of the security
    // keys.
    "/sys",
    "/run/udev",
    // /etc/resolv.conf leads here with systemd-resolved.
    "/run/systemd/resolve",
    // The app's own process (GLib's mounts, Mesa's /proc/self/exe), not
    // other processes' environments.
    "/proc/self",
    "/proc/cpuinfo",
    "/proc/meminfo",
    "/proc/stat",
    "/proc/sys",
    "/proc/filesystems",
    "/proc/driver/nvidia",
];

/// Devices every run uses: those written to, then those only read.
const DEVICES_WRITTEN: &[&str] = &["/dev/null", "/dev/full"];
const DEVICES_READ: &[&str] = &["/dev/zero", "/dev/random", "/dev/urandom"];

/// The desktop's settings and look, in the home directory, to read.
const CONFIG_READ: &[&str] = &["dconf", "gtk-4.0", "gtk-3.0", "fontconfig", "pulse"];
const DATA_READ: &[&str] = &["fonts", "icons", "themes", "vulkan"];
const HOME_READ: &[&str] = &[".fonts", ".icons", ".XCompose", ".Xauthority", ".drirc"];

/// Caches other libraries keep, to write: GStreamer's registry, fontconfig's
/// and the GPU drivers'.
const CACHE_WRITTEN: &[&str] = &[
    "gstreamer-1.0",
    "fontconfig",
    "mesa_shader_cache",
    "mesa_shader_cache_db",
    "radv_builtin_shaders",
    "nvidia",
    "gtk-4.0",
];

/// Those made if missing, as the app cannot make them once confined:
/// without GStreamer's registry every start scans every plugin, and
/// without the others fonts and shaders are found again each time.
const CACHE_MADE: &[&str] = &[
    "gstreamer-1.0",
    "fontconfig",
    "mesa_shader_cache",
    "mesa_shader_cache_db",
];

/// Every place the app may use, and how. Places that do not exist are left
/// out when the rules are made.
fn places(home: &Home, keys: &[PathBuf]) -> Vec<(PathBuf, Use)> {
    let mut places = Vec::new();
    let mut add = |path: PathBuf, how| places.push((path, how));
    for dir in SYSTEM {
        add(dir.into(), Use::Run);
    }
    for path in READ.iter().chain(DEVICES_READ) {
        add(path.into(), Use::Read);
    }
    for device in DEVICES_WRITTEN {
        add(device.into(), Use::Write);
    }
    add("/dev/dri".into(), Use::Device);
    add("/dev/shm".into(), Use::Write);
    add("/tmp/.X11-unix".into(), Use::Write);
    for nvidia in matching("/dev", "nvidia") {
        add(nvidia, Use::Device);
    }
    for key in keys {
        add(key.clone(), Use::Device);
    }
    if let Some(config) = &home.config {
        for dir in CONFIG_READ {
            add(config.join(dir), Use::Read);
        }
    }
    if let Some(data) = &home.data {
        for dir in DATA_READ {
            add(data.join(dir), Use::Read);
        }
    }
    if let Some(cache) = &home.cache {
        for dir in CACHE_WRITTEN {
            add(cache.join(dir), Use::Write);
        }
    }
    if let Some(dir) = &home.home {
        for file in HOME_READ {
            add(dir.join(file), Use::Read);
        }
        add(dir.join(".nv"), Use::Write);
    }
    if let Some(runtime) = &home.runtime {
        add(runtime.clone(), Use::Write);
    }
    for dir in &home.app {
        add(dir.clone(), Use::Write);
    }
    for (path, how) in &home.named {
        // A search path the user pointed into their home would open it.
        let inside = |root: &Path| {
            std::fs::canonicalize(path).is_ok_and(|real| real.starts_with(root))
                || path.starts_with(root)
        };
        if !home.home.as_deref().is_some_and(inside) || home.runtime.as_deref().is_some_and(inside)
        {
            add(path.clone(), *how);
        }
    }
    places
}

/// The entries of `dir` whose names start with `prefix`.
fn matching(dir: &str, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect()
}

/// FIDO's HID usage page (0xF1D0) as a report descriptor names it.
const FIDO_USAGE_PAGE: [u8; 3] = [0x06, 0xd0, 0xf1];

/// The security keys plugged in now, as hidraw nodes: by their report
/// descriptor, so other HID devices (keyboards a program could flash) stay
/// out. A key plugged in later is not among them; see `confine`.
fn security_keys() -> Vec<PathBuf> {
    matching("/sys/class/hidraw", "hidraw")
        .into_iter()
        .filter(|node| {
            let mut descriptor = Vec::new();
            File::open(node.join("device/report_descriptor"))
                .and_then(|mut f| f.read_to_end(&mut descriptor))
                .is_ok_and(|_| {
                    descriptor
                        .windows(FIDO_USAGE_PAGE.len())
                        .any(|w| w == FIDO_USAGE_PAGE)
                })
        })
        .filter_map(|node| Some(Path::new("/dev").join(node.file_name()?)))
        .collect()
}

/// Confines the app, as far as this kernel allows, unless `OPT_OUT` says
/// not to, and logs how. Call first, before anything starts a thread.
pub fn confine() -> Confinement {
    let off = Confinement {
        landlock: None,
        seccomp: false,
    };
    if std::env::var_os(OPT_OUT).is_some_and(|v| v == "1") {
        tracing::warn!(
            "{OPT_OUT}=1: the app runs unconfined, with all of the user's files and the right \
             to start programs"
        );
        return off;
    }
    // Landlock and a seccomp filter loaded with prctl(2) bind the calling
    // thread and the threads it starts, not those already running.
    match threads() {
        Some(1) => {}
        threads => tracing::error!(
            ?threads,
            "confining with other threads running: they stay unconfined"
        ),
    }
    let home = Home::from_env();
    for dir in &home.app {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, dir = %dir.display(), "cannot make the app's directory");
        }
    }
    if let Some(cache) = &home.cache {
        for dir in CACHE_MADE {
            let _ = std::fs::create_dir_all(cache.join(dir));
        }
    }
    let keys = security_keys();
    let landlock = landlock_abi().and_then(|abi| {
        if abi < MIN_LANDLOCK_ABI {
            return Err(format!(
                "this kernel's Landlock is ABI {abi}; confining needs ABI {MIN_LANDLOCK_ABI} \
                 (Linux 6.2) or later"
            ));
        }
        restrict(abi, &places(&home, &keys)).map(|()| abi)
    });
    let seccomp = filters().and_then(|filters| install(&filters).map_err(|e| e.to_string()));
    let confinement = Confinement {
        landlock: landlock.as_ref().ok().copied(),
        seccomp: seccomp.is_ok(),
    };
    if confinement.landlock.is_some() {
        let _ = KEYS_AT_START.set(keys.len());
    }
    match (landlock, seccomp) {
        (Ok(abi), Ok(())) => tracing::info!(
            landlock_abi = abi,
            security_keys = keys.len(),
            "confined: Landlock ABI {abi} and a seccomp filter; a security key must be plugged \
             in before the app starts"
        ),
        (landlock, seccomp) => {
            if let Err(reason) = landlock {
                tracing::error!(
                    reason,
                    "THE APP RUNS WITHOUT LANDLOCK: a demuxer bug reached by a crafted file \
                     could read the user's files"
                );
            }
            if let Err(reason) = seccomp {
                tracing::error!(
                    reason,
                    "THE APP RUNS WITHOUT SECCOMP: a demuxer bug reached by a crafted file \
                     could start programs"
                );
            }
        }
    }
    confinement
}

/// How many threads this process has, from /proc.
fn threads() -> Option<usize> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))?
        .trim()
        .parse()
        .ok()
}

/// This kernel's Landlock ABI, or why it has none.
fn landlock_abi() -> Result<i32, String> {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    // SAFETY: landlock_create_ruleset(2) with no attributes and the version
    // flag only reads its arguments.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi >= 0 {
        return i32::try_from(abi).map_err(|_| format!("Landlock ABI {abi}?"));
    }
    let error = std::io::Error::last_os_error();
    Err(match error.raw_os_error() {
        Some(libc::ENOSYS) => "this kernel has no Landlock".to_owned(),
        Some(libc::EOPNOTSUPP) => {
            "Landlock is not enabled on this kernel (add it to the lsm= boot parameter)".to_owned()
        }
        _ => format!("landlock_create_ruleset(2): {error}"),
    })
}

/// Confines this thread, and those it starts, to `places`.
fn restrict(abi: i32, places: &[(PathBuf, Use)]) -> Result<(), String> {
    let abi = ABI::from(abi);
    let all = AccessFs::from_all(abi);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(all)
        .map_err(|e| e.to_string())?;
    // No signal to a process outside the app (ABI 6). Abstract Unix
    // sockets stay open: X11 and D-Bus may use them.
    if Scope::from_all(abi).contains(Scope::Signal) {
        ruleset = ruleset.scope(Scope::Signal).map_err(|e| e.to_string())?;
    }
    let mut ruleset = ruleset.create().map_err(|e| e.to_string())?;
    let run = AccessFs::from_read(abi);
    let read = AccessFs::ReadFile | AccessFs::ReadDir;
    for (path, how) in places {
        let access: BitFlags<AccessFs> = match how {
            Use::Run => run,
            Use::Read => read,
            // Everything but running what was written.
            Use::Write => all & !AccessFs::Execute,
            Use::Device => {
                AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ReadDir | AccessFs::IoctlDev
            }
        };
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut access = access & all;
        let is_dir = file.metadata().map_err(|e| e.to_string())?.is_dir();
        if !is_dir {
            access &= AccessFs::from_file(abi);
        }
        if access.is_empty() {
            continue;
        }
        (&mut ruleset)
            .add_rule(PathBeneath::new(file, access))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    match ruleset.restrict_self() {
        Ok(status) if status.ruleset == RulesetStatus::FullyEnforced => Ok(()),
        Ok(status) => Err(format!("Landlock is only {:?}", status.ruleset)),
        Err(e) => Err(e.to_string()),
    }
}

/// The most instructions a seccomp filter may have (BPF_MAXINSNS).
const BPF_MAXINSNS: usize = 4096;

/// The seccomp filters: the calls refused with `EPERM`, then `clone3`
/// refused with `ENOSYS`, so that the C library starts threads with
/// `clone`, whose flags the first filter can see.
struct Filters(Vec<BpfProgram>);

fn filters() -> Result<Filters, String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e| e.to_string())?;
    let refused = SeccompFilter::new(
        refused_calls().map_err(|e| e.to_string())?,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| e.to_string())?;
    let clone3 = SeccompFilter::new(
        BTreeMap::from([(libc::SYS_clone3, Vec::new())]),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        arch,
    )
    .map_err(|e| e.to_string())?;
    let programs = [refused, clone3]
        .into_iter()
        .map(|filter| {
            let program: BpfProgram = filter
                .try_into()
                .map_err(|e: seccompiler::BackendError| e.to_string())?;
            if program.is_empty() || program.len() > BPF_MAXINSNS {
                return Err(format!(
                    "a seccomp filter of {} instructions",
                    program.len()
                ));
            }
            Ok(program)
        })
        .collect::<Result<_, String>>()?;
    Ok(Filters(programs))
}

/// The calls the first filter refuses: each always, or when its rule
/// holds. The server's list for its media tools, but the network: the app
/// connects to its servers.
fn refused_calls() -> Result<BTreeMap<i64, Vec<SeccompRule>>, seccompiler::BackendError> {
    let mut calls: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for call in [
        // New programs.
        libc::SYS_execve,
        libc::SYS_execveat,
        // Another process's memory, or the kernel's.
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_pidfd_getfd,
        libc::SYS_kcmp,
        libc::SYS_perf_event_open,
        libc::SYS_bpf,
        libc::SYS_userfaultfd,
        // The artwork cache reads and writes with psync, not io_uring.
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        // Mounts and namespaces: a user namespace would give the app
        // rights over a view of its own.
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_mount_setattr,
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        // Keyrings (the Secret Service is reached over D-Bus), modules,
        // the clock, the machine.
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_kexec_load,
        libc::SYS_kexec_file_load,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_acct,
        libc::SYS_quotactl,
        libc::SYS_syslog,
        libc::SYS_settimeofday,
        libc::SYS_clock_settime,
        libc::SYS_clock_adjtime,
        libc::SYS_adjtimex,
        libc::SYS_sethostname,
        libc::SYS_setdomainname,
        libc::SYS_vhangup,
        libc::SYS_fanotify_init,
    ] {
        calls.insert(call, Vec::new());
    }
    // No process but threads: the app starts no program, and GStreamer
    // scans its plugins in-process when its scanner cannot start.
    #[cfg(target_arch = "x86_64")]
    for call in [
        libc::SYS_fork,
        libc::SYS_vfork,
        libc::SYS_iopl,
        libc::SYS_ioperm,
        libc::SYS_uselib,
    ] {
        calls.insert(call, Vec::new());
    }
    calls.insert(
        libc::SYS_clone,
        vec![SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Qword,
            SeccompCmpOp::MaskedEq(libc::CLONE_THREAD as u64),
            0,
        )?])?],
    );
    // Typing into the terminal the app was started from.
    calls.insert(
        libc::SYS_ioctl,
        [libc::TIOCSTI, libc::TIOCLINUX]
            .into_iter()
            .map(|request| {
                #[allow(
                    clippy::unnecessary_cast,
                    reason = "an ioctl request is a u64 here, an i32 in other C libraries"
                )]
                let request = request as u64;
                SeccompRule::new(vec![SeccompCondition::new(
                    1,
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Eq,
                    request,
                )?])
            })
            .collect::<Result<_, _>>()?,
    );
    Ok(calls)
}

/// Loads the seccomp filters on this thread. Through prctl(2), as the
/// server does.
fn install(filters: &Filters) -> std::io::Result<()> {
    // SAFETY: prctl(2) with integer arguments, and with a pointer to a
    // filter that lives as long as `filters`.
    unsafe {
        if libc::prctl(
            libc::PR_SET_NO_NEW_PRIVS,
            1 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        for program in &filters.0 {
            let fprog = libc::sock_fprog {
                // At most BPF_MAXINSNS, which `filters` checked.
                len: program.len() as libc::c_ushort,
                filter: program.as_ptr().cast::<libc::sock_filter>().cast_mut(),
            };
            if libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER as libc::c_ulong,
                &raw const fprog,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// `--self-check PROBE`: after [`confine`], what the app can and cannot do,
/// one line each. True when it is confined as it should be: `probe` (a file
/// in the home directory) cannot be read, no program can start, the app's
/// data directory can be written, and GStreamer still starts.
pub fn self_check(confinement: Confinement, probe: &Path) -> bool {
    let mut ok = true;
    let mut say = |what: &str, good: bool| {
        println!("{what}");
        ok &= good;
    };
    say(
        &format!(
            "landlock: {}",
            confinement
                .landlock
                .map_or("off".into(), |abi| format!("ABI {abi}"))
        ),
        confinement.landlock.is_some(),
    );
    say(
        &format!(
            "seccomp: {}",
            if confinement.seccomp { "on" } else { "off" }
        ),
        confinement.seccomp,
    );
    match std::fs::read(probe) {
        Ok(_) => say("probe: read", false),
        Err(e) => say(&format!("probe: refused ({e})"), true),
    }
    match std::process::Command::new(std::env::current_exe().unwrap_or_default())
        .arg("--version")
        .output()
    {
        Ok(_) => say("exec: started", false),
        Err(e) => say(&format!("exec: refused ({e})"), true),
    }
    match spoolframe_linux_core::paths::data_dir().map(|dir| dir.join("self-check")) {
        Some(file) => match std::fs::write(&file, b"ok").and_then(|()| std::fs::read(&file)) {
            Ok(_) => say("data: written", true),
            Err(e) => say(&format!("data: {e}"), false),
        },
        None => say("data: no directory", false),
    }
    match crate::playback::init()
        .map_err(|e| e.to_string())
        .and_then(|()| {
            gst::ElementFactory::make("matroskademux")
                .build()
                .map_err(|e| e.to_string())
        }) {
        Ok(_) => say("gstreamer: ok", true),
        Err(e) => say(&format!("gstreamer: {e}"), false),
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Home {
        Home {
            home: Some("/home/u".into()),
            config: Some("/home/u/.config".into()),
            data: Some("/home/u/.local/share".into()),
            cache: Some("/home/u/.cache".into()),
            runtime: Some("/run/user/1000".into()),
            app: vec![
                "/home/u/.config/spoolframe-relm4".into(),
                "/home/u/.local/share/spoolframe-relm4".into(),
                "/home/u/.cache/spoolframe-relm4".into(),
            ],
            named: vec![
                ("/home/u/.nix-profile-not-here/share".into(), Use::Read),
                ("/home/u".into(), Use::Read),
                ("/run/current-system/sw/share".into(), Use::Read),
            ],
        }
    }

    #[test]
    fn the_home_directory_is_open_only_where_the_app_needs_it() {
        let home = home();
        for (path, how) in places(&home, &[]) {
            // Nothing that holds ~/.ssh, ~/Documents or a browser profile.
            for private in [".ssh", "Documents", ".mozilla", ".local/share/keyrings"] {
                assert!(
                    !Path::new("/home/u").join(private).starts_with(&path),
                    "{path:?} ({how:?}) opens ~/{private}"
                );
            }
        }
    }

    #[test]
    fn the_apps_own_directories_are_written() {
        let places = places(&home(), &[]);
        for dir in &home().app {
            assert!(places.contains(&(dir.clone(), Use::Write)), "{dir:?}");
        }
    }

    #[test]
    fn a_search_path_outside_the_home_directory_is_read() {
        let places = places(&home(), &[]);
        assert!(places.contains(&("/run/current-system/sw/share".into(), Use::Read)));
    }

    #[test]
    fn the_filters_compile_for_this_machine() {
        let filters = filters().unwrap();
        assert_eq!(filters.0.len(), 2);
        assert!(filters.0.iter().all(|f| !f.is_empty()));
    }
}
