// SPDX-License-Identifier: CC0-1.0

//! The MACHINE a MicroVM's guest runs on: the identity its platform publishes to the
//! guest, the environment its image declares, and the FILESYSTEM its program reads and
//! writes.
//!
//! # Why a guest needs one
//!
//! `SuspendMicrovm` "suspends a running MicroVM, preserving its full memory and disk
//! state" and `ResumeMicrovm` restores it "with all state intact", so what a MicroVM's
//! guest holds has to outlive the one request that wrote it: a value the guest kept in a
//! variable, a file it wrote, and the identity it reports of itself (the boot the kernel
//! published to it, its own process, when that process started) are all properties of a
//! MACHINE that keeps running between requests rather than of one proxied request. The
//! guest's memory is the engine's, kept alive by the guest PROCESS
//! ([`super::guest_runtime::GuestProcess`]); everything a machine publishes to its guest,
//! and everything the guest wrote to disk, is held here.
//!
//! # What the guest reaches
//!
//! * the files of THIS tree and nothing else. There is no host path behind it: every
//!   entry was seeded here or written by the guest itself, and the tree is
//!   `service_common`'s own in-memory volume, which opens, resolves and lists nothing of
//!   the host. A guest reading a path of the machine this emulator runs on finds nothing
//!   there, because nothing of the host is in the tree.
//! * the values a kernel publishes to a process it started, SEALED so the guest may only
//!   read them: this boot's id, the guest process's own `/proc` entries, the machine's
//!   host name.
//! * the environment the image's own `environmentVariables` declare, plus the few
//!   variables a container's command is started with. Nothing of this process's own
//!   environment is in it.
//!
//! # Bounds
//!
//! What a guest's writes hold in this process is bounded by [`volume_limits`] — the
//! files, the bytes and the path length a MicroVM's disk keeps — which is host state the
//! guest cannot reach. A write past a bound does not happen and is reported to the guest
//! as the failure it is.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::service_common::posix_shell_volume::{ShellVolume, VolumeLimits};

/// The pid the guest's own program holds.
///
/// A MicroVM image is a container image whose command starts the guest
/// ([`super::guest_program`]), so the guest's program is the FIRST process the MicroVM
/// runs and holds the pid of one: its parent is the kernel rather than another process.
pub const GUEST_PID: i32 = 1;

/// The pid of the guest process's parent: a first process has none of its own.
const GUEST_PARENT_PID: i32 = 0;

/// `USER_HZ`, the ticks a second `/proc/self/stat` counts a process's instants in.
const CLOCK_TICKS_PER_SECOND: u64 = 100;

/// Where the kernel publishes the id of THIS boot, which is what a guest reads its
/// machine's boot identity from.
pub const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

/// Where the kernel publishes the guest process's own accounting, `starttime` (the
/// instant the process started, in ticks since the boot) among it.
pub const SELF_STAT_PATH: &str = "/proc/self/stat";

/// Where it publishes the same process's named fields.
const SELF_STATUS_PATH: &str = "/proc/self/status";

/// Where it publishes the command the process was started with, NUL-separated.
const SELF_CMDLINE_PATH: &str = "/proc/self/cmdline";

/// Where it publishes how long the machine has been up, which moves with the clock.
pub const UPTIME_PATH: &str = "/proc/uptime";

/// The machine's host name.
const HOSTNAME_PATH: &str = "/etc/hostname";

/// The machine's own name resolution.
const HOSTS_PATH: &str = "/etc/hosts";

/// The directory a container image's build context is copied into and its command runs
/// from, which is therefore the guest process's working directory.
pub const GUEST_WORKING_DIRECTORY: &str = "/app";

/// The directories a machine has before its guest writes anything.
const SEEDED_DIRECTORIES: [&str; 4] = [GUEST_WORKING_DIRECTORY, "/tmp", "/var/tmp", "/data"];

/// The program the image's command names, as `argv[0]` and `execPath` report it.
const GUEST_INTERPRETER: &str = "node";

/// Where that program lives on the machine.
const GUEST_INTERPRETER_PATH: &str = "/usr/local/bin/node";

/// The `PATH` a container's command is started with.
const GUEST_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The home directory of the user a container's command runs as.
const GUEST_HOME: &str = "/root";

/// The CPU architecture a guest reports of itself, in the form `process.arch` names, for
/// the `Architecture` its image version was built for.
///
/// `Architecture` enumerates `ARM_64` and nothing else
/// (`smithy_query(query="shape",shape="Architecture")`, mirrored by
/// `crate::ops::types::ARCHITECTURE_VALUES`), which is the 64-bit ARM a `GRAVITON`
/// chipset runs — named `arm64` on the surface a guest reads it through. A value this
/// mapping does not name is reported in the lower-case form that surface uses, so a
/// generation this API adds is reported rather than mis-reported.
#[must_use]
pub fn guest_arch(architecture: &str) -> String {
    match architecture {
        "ARM_64" => "arm64".to_string(),
        "X86_64" => "x64".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

/// The platform a guest reports of itself: a MicroVM runs a Linux kernel.
const GUEST_PLATFORM: &str = "linux";

/// The kernel release the machine publishes.
const GUEST_KERNEL_RELEASE: &str = "6.1.0";

/// The runtime line a guest reports of itself, as `process.version` words it.
///
/// The runtime a guest's program is given here implements the NODE surface
/// ([`super::guest_runtime`]) — `node` is the command a MicroVM image's container
/// specification starts the guest with — so this is the line that surface implements,
/// reported in the form the guests that read it parse.
pub const GUEST_NODE_VERSION: &str = "v24.0.0";

/// How much of a guest's own writes this process holds for one MicroVM: the EMULATOR's
/// numbers, held here rather than in the guest's reach.
#[must_use]
pub fn volume_limits() -> VolumeLimits {
    VolumeLimits {
        files: 512,
        bytes: 1024 * 1024,
        path_bytes: 1024,
        directories: 256,
        device_lines: 512,
        device_line_bytes: 4 * 1024,
    }
}

/// What reading one path of a guest's filesystem reports of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    /// Whether the tree holds anything at the path at all.
    pub exists: bool,
    /// Whether what it holds is a file.
    pub file: bool,
    /// Whether what it holds is a directory.
    pub directory: bool,
    /// How many bytes the file holds.
    pub size: usize,
}

/// The machine one MicroVM's guest runs on. See the module docs.
#[derive(Debug)]
pub struct GuestMachine {
    /// This boot's id, as [`BOOT_ID_PATH`] publishes it.
    pub boot_id: String,
    /// The machine's host name, which is the MicroVM's own id.
    pub hostname: String,
    /// The module the image's command starts, as the guest's command line names it.
    pub entry: String,
    /// The CPU architecture the guest runs on, as `process.arch` names it: the
    /// `Architecture` its image version was built for, through [`guest_arch`].
    pub arch: String,
    /// The environment the guest's process was started with.
    pub env: BTreeMap<String, String>,
    /// When the MicroVM's kernel booted.
    booted_at: Instant,
    /// When the guest's own process started.
    started_at: Instant,
    /// The tree the guest reads and writes.
    files: Mutex<ShellVolume>,
}

impl GuestMachine {
    /// The machine a MicroVM that has just started RUNNING publishes to its guest:
    /// `boot_id` for this boot, `hostname` the MicroVM's own id, `program` the guest its
    /// image version holds (whose modules its working directory carries and whose entry
    /// its command line names), `architecture` the `Architecture` that image version was
    /// built for and `env` the image's own `environmentVariables`.
    ///
    /// `booted_at` is when the MicroVM's kernel came up and `now` when its guest process
    /// started, which is what `/proc` reports of each.
    #[must_use]
    pub fn boot(
        boot_id: String,
        hostname: String,
        program: Option<&super::guest_program::GuestProgram>,
        architecture: &str,
        env: BTreeMap<String, String>,
        booted_at: Instant,
        now: Instant,
    ) -> Self {
        let machine = Self {
            boot_id,
            hostname,
            entry: program.map(|program| program.entry.clone()).unwrap_or_default(),
            arch: guest_arch(architecture),
            env: Self::environment(env),
            booted_at,
            started_at: now,
            files: Mutex::new(ShellVolume::new(volume_limits())),
        };
        machine.seed(program, now);
        machine
    }

    /// The environment the guest's process is started with: the image's own declarations
    /// over the few variables a container's command always carries. Nothing of the
    /// environment of the process this emulator runs in takes part.
    fn environment(declared: BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut env = BTreeMap::from([
            ("PATH".to_string(), GUEST_PATH.to_string()),
            ("HOME".to_string(), GUEST_HOME.to_string()),
            ("PWD".to_string(), GUEST_WORKING_DIRECTORY.to_string()),
        ]);
        env.extend(declared);
        env
    }

    /// Seed what the machine published to its guest before any of the guest's own code
    /// ran: its directories, the MODULES its image's build produced (under the working
    /// directory its command runs from, which is where a container's build context is
    /// copied), and the kernel's own read-only entries.
    fn seed(&self, program: Option<&super::guest_program::GuestProgram>, now: Instant) {
        let mut files = self.hold();
        for directory in SEEDED_DIRECTORIES {
            let _ = files.make_directories(directory);
        }
        for (path, source) in program.map(|program| &program.modules).into_iter().flatten() {
            // The guest's own code, where its command runs from: a module it reads back
            // (its own source, a JSON document beside it) is the one its image holds. A
            // module past this machine's bounds is not seeded, which the guest reads as
            // the absent file it is.
            let _ = files.put_file(&format!("{GUEST_WORKING_DIRECTORY}/{path}"), source);
        }
        let _ = files.seal_file(BOOT_ID_PATH, &format!("{}\n", self.boot_id));
        let _ = files.seal_file(HOSTNAME_PATH, &format!("{}\n", self.hostname));
        let _ = files.seal_file(
            HOSTS_PATH,
            &format!("127.0.0.1\tlocalhost\n127.0.1.1\t{}\n", self.hostname),
        );
        let _ = files.seal_file(
            SELF_CMDLINE_PATH,
            &format!("{GUEST_INTERPRETER}\0{}\0", self.entry_path()),
        );
        let _ = files.seal_file(SELF_STAT_PATH, &self.self_stat());
        let _ = files.seal_file(SELF_STATUS_PATH, &self.self_status());
        drop(files);
        self.republish(now);
    }

    /// The guest process's own `/proc/self/stat` line: its identity, and `starttime` —
    /// the field that says when it started, in ticks since the machine booted.
    fn self_stat(&self) -> String {
        let starttime = self.ticks(self.started_at.saturating_duration_since(self.booted_at));
        // The fields a process's accounting line carries, in the kernel's order: pid,
        // comm, state, ppid, pgrp, session, tty, tpgid, flags, the fault and time
        // counters, priority, nice, threads, the interval timer, and then `starttime`.
        format!(
            "{GUEST_PID} ({GUEST_INTERPRETER}) S {GUEST_PARENT_PID} {GUEST_PID} {GUEST_PID} \
             0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 {starttime}\n"
        )
    }

    /// The same process's named fields.
    fn self_status(&self) -> String {
        format!(
            "Name:\t{GUEST_INTERPRETER}\nState:\tS (sleeping)\nTgid:\t{GUEST_PID}\n\
             Pid:\t{GUEST_PID}\nPPid:\t{GUEST_PARENT_PID}\nThreads:\t1\n"
        )
    }

    /// Write the entries the CLOCK moves, which are owed at this instant whether or not
    /// the guest reads them: how long the machine has been up.
    fn republish(&self, now: Instant) {
        let up = now.saturating_duration_since(self.booted_at).as_secs_f64();
        let mut files = self.hold();
        let _ = files.seal_file(UPTIME_PATH, &format!("{up:.2} {up:.2}\n"));
    }

    /// `duration` in the ticks `/proc` counts in.
    fn ticks(&self, duration: Duration) -> u64 {
        duration
            .as_millis()
            .saturating_mul(u128::from(CLOCK_TICKS_PER_SECOND))
            .checked_div(1_000)
            .unwrap_or_default()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// The tree, with a lock poisoned by an earlier panic taken rather than propagated: a
    /// guest's filesystem outlives one request, so one failed request does not take the
    /// machine's disk with it.
    fn hold(&self) -> ::std::sync::MutexGuard<'_, ShellVolume> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hand this machine's tree to `run` — a session RUNS IN a volume
    /// ([`::service_common::posix_shell_session::ShellSession::in_volume`]) rather than
    /// borrowing one — and put back the tree it answers with, so every file the session's
    /// commands wrote is the MACHINE's from then on, exactly as the guest's own writes
    /// are.
    ///
    /// The tree is the machine's disk, so it is held for the whole run: another run waits
    /// for this one rather than working in a copy of the disk that would be dropped. A
    /// `run` that answers no tree (a session that was given none to begin with) leaves the
    /// machine the empty one its disk was swapped for, which is what a session that
    /// consumed the tree left behind.
    pub fn in_volume<T>(&self, run: impl FnOnce(ShellVolume) -> (T, Option<ShellVolume>)) -> T {
        let mut held = self.hold();
        let tree = ::std::mem::replace(&mut *held, ShellVolume::new(volume_limits()));
        let (answered, left) = run(tree);
        if let Some(left) = left {
            *held = left;
        }
        answered
    }

    /// `path` as the guest's own process resolves it: a relative path against the working
    /// directory its command runs from ([`GUEST_WORKING_DIRECTORY`], which is what
    /// `process.cwd()` reports), and an absolute one as it is.
    fn at(&self, path: &str) -> String {
        if path.starts_with('/') {
            return path.to_string();
        }
        format!("{GUEST_WORKING_DIRECTORY}/{path}")
    }

    /// The path the guest's entry module is held at, which is what its command line and
    /// its `argv` name.
    fn entry_path(&self) -> String {
        self.at(&self.entry)
    }

    /// What the guest reads at `path`, or `None` where its machine holds nothing there.
    #[must_use]
    pub fn read(&self, path: &str) -> Option<String> {
        self.republish(Instant::now());
        self.hold().read(&self.at(path))
    }

    /// Write `contents` at `path`, replacing what is there or appending to it. `false` is
    /// a write that did not happen: a sealed path, a directory, a path whose directory
    /// the machine does not have, or one past this machine's bounds.
    pub fn write(&self, path: &str, contents: &str, append: bool) -> bool {
        self.hold().write(&self.at(path), contents, append)
    }

    /// Remove the file at `path`, answering whether there was one to remove.
    pub fn remove(&self, path: &str) -> bool {
        self.hold().remove_file(&self.at(path))
    }

    /// Make `path` a directory, with every directory above it.
    pub fn make_directories(&self, path: &str) -> bool {
        self.hold().make_directories(&self.at(path))
    }

    /// What the machine holds at `path`.
    #[must_use]
    pub fn stat(&self, path: &str) -> FileStat {
        self.republish(Instant::now());
        let path = self.at(path);
        let files = self.hold();
        FileStat {
            exists: files.exists(&path),
            file: files.is_file(&path),
            directory: files.is_directory(&path),
            size: files.read(&path).map_or(0, |held| held.len()),
        }
    }

    /// How long the GUEST's own process has been running, in seconds — what
    /// `process.uptime()` reports.
    #[must_use]
    pub fn process_uptime(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.started_at).as_secs_f64()
    }

    /// How long the MACHINE has been up, in seconds — what `os.uptime()` reports.
    #[must_use]
    pub fn machine_uptime(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.booted_at).as_secs_f64()
    }

    /// The command line the guest's process was started with.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        vec![GUEST_INTERPRETER_PATH.to_string(), self.entry_path()]
    }

    /// The interpreter the image's command named.
    #[must_use]
    pub fn interpreter_path(&self) -> &'static str {
        GUEST_INTERPRETER_PATH
    }

    /// The platform the guest reports of itself.
    #[must_use]
    pub fn platform(&self) -> &'static str {
        GUEST_PLATFORM
    }

    /// The architecture it reports of itself.
    #[must_use]
    pub fn arch(&self) -> &str {
        &self.arch
    }

    /// The kernel release the machine publishes.
    #[must_use]
    pub fn kernel_release(&self) -> &'static str {
        GUEST_KERNEL_RELEASE
    }

    /// The runtime line the guest reports of itself.
    #[must_use]
    pub fn node_version(&self) -> &'static str {
        GUEST_NODE_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One guest program, as the build of an image version holds it.
    fn program(entry: &str, modules: Vec<(&str, &str)>) -> super::super::guest_program::GuestProgram {
        super::super::guest_program::GuestProgram {
            modules: modules
                .into_iter()
                .map(|(path, source)| (path.to_string(), source.to_string()))
                .collect(),
            entry: entry.to_string(),
            language: super::super::guest_program::GuestLanguage::JavaScript,
        }
    }

    /// A machine booted a moment ago, as one MicroVM's guest is handed it.
    fn machine() -> GuestMachine {
        let now = Instant::now();
        GuestMachine::boot(
            "11111111-2222-4333-8444-555555555555".to_string(),
            "mvm-0123456789abcdef".to_string(),
            Some(&program(
                "server.js",
                vec![("server.js", "// the guest"), ("package.json", "{}")],
            )),
            "ARM_64",
            BTreeMap::from([("MARKER".to_string(), "a-value".to_string())]),
            now - Duration::from_millis(500),
            now,
        )
    }

    /// RULE: the architecture a guest reports of itself is the `Architecture` its image
    /// version was built for, in the form the surface it reads it through names.
    #[test]
    fn the_architecture_is_the_one_the_image_was_built_for() {
        // (case, the Architecture the version carries, what the guest reports)
        // `ARCHITECTURE_VALUES` enumerates `ARM_64` and nothing else, so the rest are the
        // forms a value this API has not published yet is reported in.
        let rows: Vec<(&str, &str, &str)> = vec![
            ("the only architecture this API names", "ARM_64", "arm64"),
            ("the 64-bit Intel form a Node surface names", "X86_64", "x64"),
            ("a value this mapping does not name", "ARM_32", "arm_32"),
        ];
        for (case, architecture, expected) in rows {
            assert_eq!(
                guest_arch(architecture),
                expected,
                "case {case:?}: expected {expected:?} for {architecture:?}"
            );
        }
        assert_eq!(
            machine().arch(),
            "arm64",
            "a machine reports the architecture its image was built for"
        );
    }

    /// RULE: the machine holds the MODULES its image's build produced, under the working
    /// directory its command runs from, so a guest reads its own code and the documents
    /// beside it — by the path its `argv` names, and by a path relative to the directory
    /// its process reports.
    #[test]
    fn the_guests_own_modules_are_where_its_command_runs_from() {
        let machine = machine();
        // (case, the path the guest reads, what it must carry)
        let rows: Vec<(&str, String, &str)> = vec![
            (
                "its own module by the path argv names",
                machine.argv().into_iter().next_back().unwrap_or_default(),
                "// the guest",
            ),
            (
                "its own module relative to its working directory",
                "server.js".to_string(),
                "// the guest",
            ),
            (
                "a document beside it",
                "package.json".to_string(),
                "{}",
            ),
        ];
        for (case, path, expected) in rows {
            assert_eq!(
                machine.read(&path).as_deref(),
                Some(expected),
                "case {case:?}: expected {path} to carry {expected:?}"
            );
        }
        assert!(
            !machine.stat("absent.js").exists,
            "a module the image does not hold is not on the machine"
        );
    }

    /// RULE: a machine publishes the kernel's own entries to its guest READ-ONLY — the
    /// boot id, the guest process's accounting and the host name are there to be read and
    /// a guest's write to one does not happen — while the directories it seeds are the
    /// guest's to write in.
    #[test]
    fn the_kernels_own_entries_are_published_read_only() {
        let machine = machine();

        // (case, the path, what reading it must contain)
        let published: Vec<(&str, &str, String)> = vec![
            ("this boot's id", BOOT_ID_PATH, machine.boot_id.clone()),
            ("the host name", "/etc/hostname", machine.hostname.clone()),
            ("the process's accounting", SELF_STAT_PATH, "(node)".to_string()),
            ("the process's status", "/proc/self/status", "Pid:".to_string()),
            ("the command line", "/proc/self/cmdline", "server.js".to_string()),
            ("the machine's uptime", UPTIME_PATH, ".".to_string()),
        ];
        for (case, path, expected) in published {
            let read = machine.read(path);
            let holds = read
                .as_deref()
                .is_some_and(|text| text.contains(expected.as_str()));
            assert!(
                holds,
                "case {case:?}: expected {path} to carry {expected:?}, got {read:?}"
            );
            assert!(
                !machine.write(path, "the guest's own value", false),
                "case {case:?}: a guest's write to {path} must not happen"
            );
            let after = machine.read(path);
            assert_eq!(
                after, read,
                "case {case:?}: {path} must still carry what the kernel published"
            );
        }

        for directory in SEEDED_DIRECTORIES {
            let path = format!("{directory}/kept");
            assert!(
                machine.write(&path, "a value", false),
                "the guest writes its own files in {directory}"
            );
            assert_eq!(
                machine.read(&path).as_deref(),
                Some("a value"),
                "what the guest wrote at {path} is what it reads back"
            );
        }
    }

    /// RULE: `starttime` is the instant the guest's process started, in ticks since the
    /// machine booted, so a machine whose guest started one span after the boot reports
    /// that span — and the same line is reported however often it is read.
    #[test]
    fn the_processs_start_is_reported_in_ticks_since_the_boot() {
        // (case, how long after the boot the guest started, the ticks that is)
        let rows: Vec<(&str, Duration, u64)> = vec![
            ("the same instant as the boot", Duration::ZERO, 0),
            ("a tick after it", Duration::from_millis(10), 1),
            ("half a second after it", Duration::from_millis(500), 50),
            ("ten seconds after it", Duration::from_secs(10), 1_000),
        ];
        for (case, after, expected) in rows {
            let now = Instant::now();
            let machine = GuestMachine::boot(
                "11111111-2222-4333-8444-555555555555".to_string(),
                "mvm-0123456789abcdef".to_string(),
                Some(&program("index.js", vec![("index.js", "// the guest")])),
                "ARM_64",
                BTreeMap::new(),
                now - after,
                now,
            );
            let stat = machine.read(SELF_STAT_PATH).unwrap_or_default();
            let starttime: u64 = stat
                .split_whitespace()
                .nth(21)
                .and_then(|field| field.parse().ok())
                .unwrap_or_default();
            assert_eq!(
                starttime, expected,
                "case {case:?}: expected starttime {expected}, got {starttime} in {stat:?}"
            );
            assert_eq!(
                machine.read(SELF_STAT_PATH).unwrap_or_default(),
                stat,
                "case {case:?}: the process's accounting is the same line on every read"
            );
        }
    }

    /// RULE: the environment is the image's own declarations over the variables a
    /// container's command carries, and holds nothing of the environment this emulator's
    /// own process runs in.
    #[test]
    fn the_environment_is_the_images_own() {
        let machine = GuestMachine::boot(
            "11111111-2222-4333-8444-555555555555".to_string(),
            "mvm-0123456789abcdef".to_string(),
            Some(&program("index.js", vec![("index.js", "// the guest")])),
            "ARM_64",
            BTreeMap::from([
                ("MARKER".to_string(), "a-value".to_string()),
                ("HOME".to_string(), "/srv".to_string()),
            ]),
            Instant::now(),
            Instant::now(),
        );
        assert_eq!(
            machine.env.get("MARKER").map(String::as_str),
            Some("a-value"),
            "the image's own variable is in the guest's environment, got {:?}",
            machine.env
        );
        assert_eq!(
            machine.env.get("HOME").map(String::as_str),
            Some("/srv"),
            "the image's declaration wins over the default, got {:?}",
            machine.env
        );
        assert!(
            machine.env.contains_key("PATH"),
            "a container's command carries a PATH, got {:?}",
            machine.env
        );
        // The WHOLE environment is the image's declarations over those defaults: nothing
        // of the process this emulator runs in is in it, because nothing of it is read.
        let names: Vec<&str> = machine.env.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            vec!["HOME", "MARKER", "PATH", "PWD"],
            "a guest's environment holds only what its machine published, got {:?}",
            machine.env
        );
    }
}
