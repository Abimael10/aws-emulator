// SPDX-License-Identifier: CC0-1.0

//! What one instance's GUEST sees of its ROOT FILESYSTEM: how large it is, how much of it is in
//! use, the space a program of it RESERVES, and the programs it reports both with.
//!
//! # Why a guest needs this
//!
//! How full a filesystem is, is a measurement NO API of this account derives: the control plane
//! knows how large the root volume is (`EbsBlockDevice$VolumeSize`) and nothing about what is on
//! it. Everything else is inside the guest, which is why reading it means running a program
//! THERE — `df` through Systems Manager Run Command — and why the CloudWatch agent publishes
//! the same reading as its `disk_` measurements ([`super::guest_cloudwatch_agent`]). A guest
//! that reports nothing about its own filesystem cannot be monitored for capacity at all,
//! however faithfully the volume is reported, and a client cannot tell an empty filesystem from
//! a program that never ran.
//!
//! # One derivation, three readers
//!
//! All three readings come from the ONE filesystem below ([`GuestFilesystem`]): its SIZE is the
//! root volume's own, and what is IN USE is what the image it booted holds
//! ([`IMAGE_FILESYSTEM_BYTES`]) plus what the machine's own storage accounts for
//! ([`storage_bytes`]) — the files its programs wrote and the space they RESERVED. So the
//! guest's own `df`, the size its `stat` reports for a file and the series the agent publishes
//! cannot disagree about one machine, and a machine whose root volume size this store does not
//! hold reports no filesystem, no program and no `disk_` measurement at all — the same answer
//! [`super::guest_memory`] gives for a memory size it does not hold.
//!
//! # Reserved space is space, not bytes
//!
//! `fallocate` "preallocates blocks to a file": it makes the filesystem account for that much
//! space without WRITING anything, which is exactly how a script fills a disk to watch a
//! capacity alarm fire. So a reservation here is a COUNT of bytes held against the filesystem
//! ([`super::guest_services::GuestRuntime::reserved`]), kept beside the machine the way its
//! files are and released when the file it was taken for is gone — never a buffer of that size
//! in this process, which is also why nothing reads back out of a file whose bytes were never
//! written.
//!
//! # No host filesystem
//!
//! Nothing here reads the filesystem of the host this process runs on, and nothing calls statfs:
//! every figure is computed from the stored instance and from the tree the session holds. The
//! programs are programs OF THE GUEST, performed by the same bounded, in-process shell session
//! the guest's own boot script runs in ([`super::guest_console`]). An invocation asking for a
//! column, a unit or a form this emulator derives nothing for answers as a command that could
//! not be run, rather than a figure nothing stands behind.

use ::std::collections::BTreeMap;

use ::service_common::posix_shell_session::{
    program_option_value, EnvironmentPrograms, ProgramFiles, ProgramInvocation, ProgramOutcome,
    ProgramStreams,
};

use super::guest_console::GuestMachine;
use super::guest_services::{GuestRuntime, GuestServices};

/// The program a guest reports how full its filesystems are with.
pub const FILESYSTEM_REPORT_PROGRAM: &str = "df";

/// The program a guest RESERVES space for a file with.
pub const ALLOCATE_PROGRAM: &str = "fallocate";

/// The program a guest reports the facts of a FILE with — the same program that reports the
/// facts of a filesystem ([`super::guest_mounts::STATUS_PROGRAM`]), whose `-f` form that module
/// answers out of the session's mount table; the file form is answered here, because a file's
/// SIZE is the filesystem's own fact.
pub const STATUS_PROGRAM: &str = super::guest_mounts::STATUS_PROGRAM;

/// The directory the root filesystem is mounted at, which is what `df` reports it as mounted on.
const ROOT_MOUNT_POINT: &str = "/";

/// The status a program of this guest exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status one of them exits with when it could not: "non-zero if an error occurred"
/// (`df(1)`, `stat(1)`), and "a nonzero value" for `fallocate(1)`.
const FAILED: i32 = 1;

/// How many bytes one gibibyte is, which is the unit a volume's size is stated in
/// (`EbsBlockDevice$VolumeSize`) — the store's own reading of that unit.
const BYTES_PER_GIB: i64 = super::BYTES_PER_GIB;

/// What the IMAGE a machine boots holds on its root filesystem before its own programs write
/// anything.
///
/// CHOSEN: no API reports how much of a root volume an image's own contents take, and a
/// filesystem reporting nothing in use would report a machine with no operating system on it.
/// It sits below the smallest root volume a launch can ask for.
pub const IMAGE_FILESYSTEM_BYTES: i64 = 2 * BYTES_PER_GIB;

/// How many files ONE machine may hold a reservation for at once.
///
/// The EMULATOR's own bound, beside the one on the files a machine keeps
/// ([`super::guest_console::MAX_GUEST_FILES`]): the paths are caller-supplied text, so what one
/// machine can make this store retain records for is bounded here. A reservation past it is
/// answered the way one the filesystem has no room for is.
const MAX_RESERVED_FILES: usize = 64;

/// The root FILESYSTEM of one machine: how many bytes it holds and how many are in use.
///
/// The size is the root volume's own (`EbsBlockDevice$VolumeSize`, in gibibytes) and what is in
/// use is what the machine's storage accounts for, so the guest's own storage and the series the
/// agent publishes about it are one fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestFilesystem {
    /// The bytes the filesystem holds.
    pub total_bytes: i64,
    /// The bytes of it in use.
    pub used_bytes: i64,
}

impl GuestFilesystem {
    /// The root filesystem of a machine whose root volume is `size_gib` and whose own storage
    /// accounts for `stored_bytes` ([`storage_bytes`]) — `None` for a machine this store holds
    /// no root volume size for.
    #[must_use]
    pub fn of(size_gib: Option<i32>, stored_bytes: usize) -> Option<Self> {
        let total_bytes =
            i64::from(size_gib.filter(|size| *size > 0)?).saturating_mul(BYTES_PER_GIB);
        let stored = i64::try_from(stored_bytes).unwrap_or(i64::MAX);
        Some(Self {
            total_bytes,
            used_bytes: IMAGE_FILESYSTEM_BYTES
                .saturating_add(stored)
                .min(total_bytes),
        })
    }

    /// The bytes not in use.
    #[must_use]
    pub(super) fn free_bytes(&self) -> i64 {
        self.total_bytes.saturating_sub(self.used_bytes).max(0)
    }

    /// The share of the filesystem in use, as the percentage `disk_used_percent` carries and
    /// `df` writes in its `pcent` column.
    #[must_use]
    pub(super) fn used_percent(&self) -> f64 {
        if self.total_bytes <= 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let share = self.used_bytes as f64 / self.total_bytes as f64;
        share * 100.0
    }
}

/// How many bytes the storage of the machine in `runtime` accounts for, given the `files` it
/// holds: the files themselves, as the tree accounts for one ([`super::guest_memory::stored_bytes`]),
/// and the space its programs RESERVED for files whose bytes were never written.
///
/// The ONE reading of it, so what the guest's own `df` reports and what the agent's filesystem
/// measurement publishes are the same bytes.
#[must_use]
pub fn storage_bytes(files: &BTreeMap<String, String>, runtime: &GuestRuntime) -> usize {
    let reserved = usize::try_from(reserved_bytes(runtime)).unwrap_or(usize::MAX);
    super::guest_memory::stored_bytes(files).saturating_add(reserved)
}

/// How many bytes of a machine's filesystem are RESERVED: the reservations its programs hold,
/// summed.
#[must_use]
pub fn reserved_bytes(runtime: &GuestRuntime) -> i64 {
    runtime
        .reserved
        .values()
        .fold(0_i64, |held, bytes| held.saturating_add(*bytes))
}

/// Release the reservations held for files the machine no longer holds, `files` being its
/// storage now.
///
/// Space is reserved FOR a file, so removing that file gives the space back, exactly as it does
/// on a machine. Called where a run's tree becomes the machine's storage again, which is the one
/// place this store learns a file is gone.
pub fn release_removed_files(runtime: &mut GuestRuntime, files: &BTreeMap<String, String>) {
    runtime.reserved.retain(|path, _| files.contains_key(path));
}

/// The programs a guest reports its own root filesystem with, for a session that runs a program
/// of that guest — `None` for a machine whose root volume size this store does not hold, which
/// has no program that answers one either.
///
/// `services` is the machine's own runtime as the session's programs change it, which is where a
/// reservation is held: a program that takes one is one of this machine's, so the space it took
/// is on the machine and not in this process.
#[must_use]
pub fn filesystem_programs(
    machine: &GuestMachine,
    services: &GuestServices,
) -> Option<Box<dyn EnvironmentPrograms>> {
    let (device_name, size_gib) = machine.root_device.clone()?;
    // WHICH device the kernel of this guest carries that filesystem on is the guest's own name
    // for it ([`super::guest_block_devices`]), so `df` and the guest's own `lsblk` name one
    // device; the attachment's own device name is what a machine whose devices this emulator
    // does not name reports.
    let device = machine
        .block_devices
        .iter()
        .find(|device| device.root)
        .map_or(device_name, super::guest_block_devices::GuestBlockDevice::path);
    Some(Box::new(FilesystemPrograms {
        device,
        size_gib,
        services: services.clone(),
    }))
}

/// The filesystem programs of one machine's guest.
///
/// Holds that machine's root device, the size of the volume behind it and the machine's own
/// runtime — no store guard, no engine and no host handle is reachable from a program.
#[derive(Debug)]
struct FilesystemPrograms {
    /// The device the root filesystem is on, as the guest's kernel names it.
    device: String,
    /// The size of the volume behind it, in gibibytes.
    size_gib: i32,
    /// The machine's runtime, which holds the space its programs reserved.
    services: GuestServices,
}

impl EnvironmentPrograms for FilesystemPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        // What this form of the seam reports is the program's standard OUTPUT alone, which is
        // what a caller asking it reads; the form the session asks carries both streams.
        self.program(name, arguments, files)
            .map(|streams| ProgramOutcome {
                status: streams.status,
                output: streams.output,
            })
    }

    fn run_invocation(
        &self,
        invocation: &ProgramInvocation<'_>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        self.program(invocation.name, invocation.arguments, files)
    }
}

impl FilesystemPrograms {
    /// What the program `name` of this guest answers for `arguments`, or `None` when this guest
    /// has no program of that name — or performs nothing of that invocation of it.
    fn program(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        // A program named by its absolute path is the same program, as a shell resolving a name
        // on `PATH` finds.
        let name = name.rsplit('/').next().unwrap_or(name);
        match name {
            FILESYSTEM_REPORT_PROGRAM => self.reported(arguments, &*files),
            ALLOCATE_PROGRAM => self.allocated(arguments, files),
            STATUS_PROGRAM => self.file_status(arguments, &*files),
            _ => None,
        }
    }

    /// The machine's root filesystem as it stands while this program runs: its size, and what
    /// the image, the tree's own files and the reservations come to.
    ///
    /// The tree is read for the files rather than the stored instance, so a program that reads
    /// the filesystem after an earlier command of the same script wrote to it reads what that
    /// command left — and a tree that keeps no account of its bytes leaves the files out of the
    /// reading rather than reporting a filesystem nothing is on.
    fn filesystem(&self, files: &dyn ProgramFiles) -> Option<GuestFilesystem> {
        let stored = files.stored_byte_count().unwrap_or(0);
        let reserved = usize::try_from(self.reserved_now(files)).unwrap_or(usize::MAX);
        GuestFilesystem::of(Some(self.size_gib), stored.saturating_add(reserved))
    }

    /// How much space this machine's filesystem holds right now: the reservations it has for
    /// files the tree STILL holds, summed.
    ///
    /// Space is reserved FOR a file, so a reservation whose file an earlier command of this
    /// same script removed is already the filesystem's again — the record of it goes when the
    /// run's tree becomes the machine's storage
    /// ([`release_removed_files`]), and what this program reports cannot wait for that.
    fn reserved_now(&self, files: &dyn ProgramFiles) -> i64 {
        self.services
            .taken()
            .reserved
            .iter()
            .filter(|(path, _)| files.read(path).is_some())
            .fold(0_i64, |held, (_, bytes)| held.saturating_add(*bytes))
    }

    /// Perform `df`: write the columns asked for about the filesystem the paths named lie on.
    ///
    /// `None` for an invocation this program performs nothing of ([`ReportInvocation`] states
    /// which those are), and for one naming a path on a filesystem another component owns — its
    /// capacity is that component's fact and not this store's, the same answer
    /// [`super::guest_mounts`] gives for the machine's own storage.
    fn reported(&self, arguments: &[String], files: &dyn ProgramFiles) -> Option<ProgramStreams> {
        let invocation = report_invocation(arguments)?;
        for path in &invocation.paths {
            if files.file_system_at(path).is_some() {
                return None;
            }
        }
        let filesystem = self.filesystem(files)?;
        Some(ProgramStreams {
            status: SUCCESS,
            output: report_lines(&invocation, &self.device, filesystem),
            errors: Vec::new(),
        })
    }

    /// Perform `fallocate -l LENGTH PATH`: reserve that much space on the filesystem for that
    /// path, which the filesystem reports in use from now on.
    ///
    /// `None` for an invocation this program performs nothing of ([`allocate_invocation`]). A
    /// reservation the filesystem has no room for is the utility's own failure
    /// ([`NO_SPACE_DIAGNOSTIC`]), which is what a script that asked for more than the volume
    /// holds reads — and so is one past [`MAX_RESERVED_FILES`].
    fn allocated(
        &self,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        let (length, path) = allocate_invocation(arguments)?;
        let filesystem = self.filesystem(&*files)?;
        // The space this path already holds is not taken twice: a second reservation of the
        // same file replaces the first, as the utility sets the file's allocation — and a
        // reservation whose file is gone was already counted as released above.
        let held = files
            .read(&path)
            .and_then(|_| self.services.reserved_at(&path))
            .unwrap_or(0);
        let room = filesystem.free_bytes().saturating_add(held);
        if length > room {
            return Some(ProgramStreams {
                status: FAILED,
                output: Vec::new(),
                errors: vec![format!("{ALLOCATE_PROGRAM}: {NO_SPACE_DIAGNOSTIC}")],
            });
        }
        // The file itself: a path the tree already holds keeps what it holds — the utility
        // allocates space and writes nothing — and one it does not is created by the
        // allocation, the way the utility creates it.
        if files.read(&path).is_none() && !files.write(&path, "") {
            return Some(ProgramStreams {
                status: FAILED,
                output: Vec::new(),
                errors: vec![format!(
                    "{ALLOCATE_PROGRAM}: cannot open {path}: No such file or directory"
                )],
            });
        }
        if !self.services.reserve(&path, length, MAX_RESERVED_FILES) {
            return Some(ProgramStreams {
                status: FAILED,
                output: Vec::new(),
                errors: vec![format!("{ALLOCATE_PROGRAM}: {NO_SPACE_DIAGNOSTIC}")],
            });
        }
        Some(ProgramStreams {
            status: SUCCESS,
            output: Vec::new(),
            errors: Vec::new(),
        })
    }

    /// Perform the FILE form of `stat`: write the format asked for about the file the path
    /// names.
    ///
    /// `None` for the filesystem form (`-f`, which [`super::guest_mounts`] answers out of the
    /// session's mount table), for an invocation with no format at all — whose whole report
    /// this emulator derives nothing of — and for a format naming a specifier it derives
    /// nothing for. A path the machine does not hold is the utility's own failure, which is
    /// what a script checking for a file it never wrote reads.
    fn file_status(&self, arguments: &[String], files: &dyn ProgramFiles) -> Option<ProgramStreams> {
        let (format, path) = file_status_invocation(arguments)?;
        let Some(contents) = files.read(&path) else {
            return Some(ProgramStreams {
                status: FAILED,
                output: Vec::new(),
                errors: vec![format!(
                    "{STATUS_PROGRAM}: cannot statx '{path}': No such file or directory"
                )],
            });
        };
        // A file's SIZE is the bytes it holds, and for a file whose space was RESERVED it is
        // the space reserved for it: that is what the filesystem accounts for.
        let size = i64::try_from(contents.len())
            .unwrap_or(i64::MAX)
            .max(self.services.reserved_at(&path).unwrap_or(0));
        let written = formatted_status(&format, &path, size)?;
        Some(ProgramStreams {
            status: SUCCESS,
            output: vec![written],
            errors: Vec::new(),
        })
    }
}

/// What `fallocate` writes when the filesystem has no room for what was asked: "fallocate
/// failed: No space left on device", the diagnostic it writes for the error the system call
/// answers with.
const NO_SPACE_DIAGNOSTIC: &str = "fallocate failed: No space left on device";

/// The specifier the FILE form of `stat` writes a file's size for: "%s  total size, in bytes"
/// (`stat(1)`).
const SIZE_SPECIFIER: &str = "%s";

/// The specifier either form writes the NAME it was asked about for: "%n  file name".
const FILE_NAME_SPECIFIER: &str = "%n";

/// `format` with the specifiers this program derives substituted, or `None` when it names one
/// this emulator derives nothing for — which makes the whole invocation one the program performs
/// nothing of.
fn formatted_status(format: &str, path: &str, size: i64) -> Option<String> {
    let mut written = String::new();
    let mut rest = format;
    while let Some(at) = rest.find('%') {
        written.push_str(&rest[..at]);
        let specifier = rest.get(at..at + 2)?;
        match specifier {
            SIZE_SPECIFIER => written.push_str(&size.to_string()),
            FILE_NAME_SPECIFIER => written.push_str(path),
            // `%%` is one literal per cent, the one specifier that names no value.
            "%%" => written.push('%'),
            _ => return None,
        }
        rest = &rest[at + 2..];
    }
    written.push_str(rest);
    Some(written)
}

/// The `stat` invocation `arguments` describes, as the format it writes and the path it is about
/// — `None` for the filesystem form and for anything but the single-path form with a format.
fn file_status_invocation(arguments: &[String]) -> Option<(String, String)> {
    let mut format: Option<String> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            operands.extend(words.map(String::clone));
            break;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-c", "--format", "--printf"])
        {
            format = Some(named?);
            continue;
        }
        // The FILESYSTEM form is another module's ([`super::guest_mounts`]).
        if word == "-f" || word == "--file-system" {
            return None;
        }
        if let Some(letters) = word.strip_prefix('-').filter(|word| !word.starts_with('-')) {
            let mut remaining = letters.chars().peekable();
            while let Some(letter) = remaining.next() {
                match letter {
                    'f' => return None,
                    // The last letter of the group is the one that may take the value.
                    'c' if remaining.peek().is_none() => {
                        format = Some(words.next()?.clone());
                    }
                    _ => return None,
                }
            }
            continue;
        }
        if word.starts_with('-') {
            return None;
        }
        operands.push(word.clone());
    }
    let [path] = operands.as_slice() else {
        return None;
    };
    Some((format?, path.clone()))
}

/// The LENGTH and the path one `fallocate` invocation names, or `None` for one this program
/// performs nothing of: an invocation naming no length or no path, a length that is not a size,
/// an option it does not read (a punch, a collapse or a zeroing of a RANGE of a file changes the
/// bytes of that file, which a reservation does not), and anything but the single-path form.
fn allocate_invocation(arguments: &[String]) -> Option<(i64, String)> {
    let mut length: Option<i64> = None;
    let mut operands: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            operands.extend(words.map(String::clone));
            break;
        }
        if let Some(named) = program_option_value(word, &mut words, &["-l", "--length"]) {
            length = Some(size_in_bytes(&named?)?);
            continue;
        }
        if word.starts_with('-') {
            return None;
        }
        operands.push(word.clone());
    }
    let [path] = operands.as_slice() else {
        return None;
    };
    // A reservation of no space at all is not one the utility makes either.
    Some((length.filter(|length| *length > 0)?, path.clone()))
}

/// The multiplicative suffixes a size is written with: "KiB=1024, MiB=1024*1024, and so on …
/// (the 'iB' is optional, e.g., 'K' has the same meaning as 'KiB') or the suffixes KB=1000,
/// MB=1000*1000, and so on" (`fallocate(1)`), longest spelling first so a prefix of another is
/// not read for it.
const SIZE_SUFFIXES: &[(&str, i64)] = &[
    ("KiB", 1024),
    ("MiB", 1024 * 1024),
    ("GiB", 1024 * 1024 * 1024),
    ("TiB", 1024_i64.pow(4)),
    ("KB", 1000),
    ("MB", 1000 * 1000),
    ("GB", 1000 * 1000 * 1000),
    ("TB", 1000_i64.pow(4)),
    ("K", 1024),
    ("M", 1024 * 1024),
    ("G", 1024 * 1024 * 1024),
    ("T", 1024_i64.pow(4)),
];

/// The bytes `written` states, in any of the spellings above — and `None` for a word that states
/// no size at all, which is the utility's own usage failure.
fn size_in_bytes(written: &str) -> Option<i64> {
    let written = written.trim();
    for (suffix, unit) in SIZE_SUFFIXES {
        if let Some(figure) = written.strip_suffix(suffix) {
            return figure.trim().parse::<i64>().ok()?.checked_mul(*unit);
        }
    }
    written.parse().ok()
}

/// One invocation of `df` this program performs.
#[derive(Debug)]
struct ReportInvocation {
    /// The columns to write, in the order they are written.
    fields: Vec<Field>,
    /// Whether the invocation named the columns itself (`--output`), which is what decides the
    /// headings the size columns carry.
    named_fields: bool,
    /// The unit the size columns are written in.
    scale: Scale,
    /// Whether a heading line is written, which `--no-headings` is what leaves out.
    headings: bool,
    /// The paths the invocation asked about, which decide WHICH filesystem is reported.
    paths: Vec<String>,
}

/// One column of a `df` report, as the field list names it (`df --output`'s own field names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// `source`: "the source of the mount point", which is the device the filesystem is on.
    Source,
    /// `size`: "total number of blocks".
    Size,
    /// `used`: "number of used blocks".
    Used,
    /// `avail`: "number of available blocks".
    Avail,
    /// `pcent`: "percentage of USED divided by SIZE".
    Pcent,
    /// `target`: "the mount point".
    Target,
}

/// The columns `df` writes when the invocation names none: "the default output … is equivalent
/// to `--output=source,size,used,avail,pcent,target`" (`df(1)`).
const DEFAULT_FIELDS: &[Field] = &[
    Field::Source,
    Field::Size,
    Field::Used,
    Field::Avail,
    Field::Pcent,
    Field::Target,
];

impl Field {
    /// The field the name `written` names, or `None` for one this program writes no column for:
    /// the inode columns of a filesystem whose inodes this emulator does not model, the
    /// filesystem TYPE, and the FILE the operand named, none of which this store holds a value
    /// for.
    #[must_use]
    fn named(written: &str) -> Option<Self> {
        match written.trim() {
            "source" => Some(Self::Source),
            "size" => Some(Self::Size),
            "used" => Some(Self::Used),
            "avail" => Some(Self::Avail),
            "pcent" => Some(Self::Pcent),
            "target" => Some(Self::Target),
            _ => None,
        }
    }

    /// The heading this column carries. The size columns are headed by the BLOCK SIZE the
    /// report is in where the invocation named no columns, which is how `df` heads them, and by
    /// the field's own name where it named them with `--output`.
    #[must_use]
    fn heading(self, scale: Scale, named_fields: bool) -> String {
        match self {
            Self::Source => "Filesystem".to_string(),
            Self::Size if named_fields => "Size".to_string(),
            Self::Size => scale.size_heading(),
            Self::Used => "Used".to_string(),
            Self::Avail if named_fields => "Avail".to_string(),
            Self::Avail => "Available".to_string(),
            Self::Pcent => "Use%".to_string(),
            Self::Target => "Mounted on".to_string(),
        }
    }

    /// What this column reports for `filesystem` on `device`, written as `df` writes it.
    #[must_use]
    fn value(self, device: &str, filesystem: GuestFilesystem, scale: Scale) -> String {
        match self {
            Self::Source => device.to_string(),
            Self::Size => scale.of(filesystem.total_bytes),
            Self::Used => scale.of(filesystem.used_bytes),
            Self::Avail => scale.of(filesystem.free_bytes()),
            // "The percentage is rounded UP", so a filesystem holding anything at all is never
            // reported as empty.
            Self::Pcent => format!("{}%", percentage(filesystem)),
            Self::Target => ROOT_MOUNT_POINT.to_string(),
        }
    }
}

/// The share of `filesystem` in use as `df` writes it: the whole per cent, rounded up.
#[must_use]
fn percentage(filesystem: GuestFilesystem) -> u64 {
    let total = u64::try_from(filesystem.total_bytes).unwrap_or(0);
    let used = u64::try_from(filesystem.used_bytes).unwrap_or(0);
    if total == 0 {
        return 0;
    }
    used.saturating_mul(100).div_ceil(total)
}

/// The unit the size columns of a report are written in, as the option the invocation carried
/// names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scale {
    /// `-k`, and the default: "scale sizes by 1K before printing them" — the blocks `df`
    /// reports in when nothing else is asked for.
    Kibibytes,
    /// `-m`: "like --block-size=1M".
    Mebibytes,
    /// `-h`: "print sizes in powers of 1024".
    Human,
}

impl Scale {
    /// The unit the option `word` names, or `None` for a word that names none of them. `-P`
    /// asks for "the POSIX output format", whose blocks are the 1024-byte ones this report is
    /// in anyway.
    #[must_use]
    fn named(word: &str) -> Option<Self> {
        match word {
            "-k" | "--kilobytes" | "-P" | "--portability" => Some(Self::Kibibytes),
            "-m" | "--megabytes" => Some(Self::Mebibytes),
            "-h" | "--human-readable" => Some(Self::Human),
            _ => None,
        }
    }

    /// The heading the size column carries in this unit, as `df` writes it.
    #[must_use]
    fn size_heading(self) -> String {
        match self {
            Self::Kibibytes => "1K-blocks".to_string(),
            Self::Mebibytes => "1M-blocks".to_string(),
            Self::Human => "Size".to_string(),
        }
    }

    /// `bytes` written in this unit: the whole blocks it comes to, rounded UP as `df` rounds a
    /// figure it states in blocks, or the largest power of 1024 it fits in for the human form.
    #[must_use]
    fn of(self, bytes: i64) -> String {
        let blocks = u64::try_from(bytes.max(0)).unwrap_or(0);
        match self {
            Self::Kibibytes => blocks.div_ceil(1024).to_string(),
            Self::Mebibytes => blocks.div_ceil(1024 * 1024).to_string(),
            Self::Human => human_size(bytes.max(0)),
        }
    }
}

/// The units the human-readable form writes a size in, smallest first — "powers of 1024"
/// (`df(1)`), each with the letter `df` names it by.
const HUMAN_UNITS: &[(char, i64)] = &[
    ('K', 1024),
    ('M', 1024 * 1024),
    ('G', 1024 * 1024 * 1024),
    ('T', 1024_i64.pow(4)),
];

/// `bytes` as the human-readable form writes it: the largest unit it fits in, "with a maximum of
/// three digits" — one decimal below ten of that unit and the whole number above it.
#[must_use]
fn human_size(bytes: i64) -> String {
    let mut written = format!("{bytes}");
    for (letter, unit) in HUMAN_UNITS {
        if bytes < *unit {
            break;
        }
        #[allow(clippy::cast_precision_loss)]
        let scaled = bytes as f64 / *unit as f64;
        written = if scaled < 10.0 {
            format!("{scaled:.1}{letter}")
        } else {
            format!("{scaled:.0}{letter}")
        };
    }
    written
}

/// The `df` invocation `arguments` describes, or `None` for one this program performs nothing
/// of: an option it does not read (the inode form, a block size of its own, a listing of
/// filesystem TYPES), and a field list naming a column this emulator derives nothing for.
fn report_invocation(arguments: &[String]) -> Option<ReportInvocation> {
    let mut fields: Option<Vec<Field>> = None;
    let mut scale = Scale::Kibibytes;
    let mut headings = true;
    let mut paths: Vec<String> = Vec::new();
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            paths.extend(words.map(String::clone));
            break;
        }
        // `--output` with no field list at all writes every column there is, which is a report
        // this emulator derives only part of.
        if let Some(named) = program_option_value(word, &mut words, &["--output"]) {
            fields = Some(field_list(&named?)?);
            continue;
        }
        if word == "--no-headings" {
            headings = false;
            continue;
        }
        if word.starts_with('-') {
            scale = Scale::named(word)?;
            continue;
        }
        paths.push(word.clone());
    }
    let named_fields = fields.is_some();
    Some(ReportInvocation {
        fields: fields.unwrap_or_else(|| DEFAULT_FIELDS.to_vec()),
        named_fields,
        scale,
        headings,
        paths,
    })
}

/// The fields one `--output` value lists, comma-separated — `None` when it names one this
/// program writes no column for, which makes the whole invocation one it performs nothing of.
fn field_list(value: &str) -> Option<Vec<Field>> {
    let fields: Vec<Field> = value
        .split(',')
        .filter(|name| !name.trim().is_empty())
        .map(Field::named)
        .collect::<Option<Vec<Field>>>()?;
    (!fields.is_empty()).then_some(fields)
}

/// What `df` writes for `filesystem` on `device`: the heading line the invocation asked for and
/// one row for that filesystem, each column right-aligned in a width that holds its heading and
/// its value, which is how the utility lays its table out.
#[must_use]
fn report_lines(
    invocation: &ReportInvocation,
    device: &str,
    filesystem: GuestFilesystem,
) -> Vec<String> {
    let headings: Vec<String> = invocation
        .fields
        .iter()
        .map(|field| field.heading(invocation.scale, invocation.named_fields))
        .collect();
    let values: Vec<String> = invocation
        .fields
        .iter()
        .map(|field| field.value(device, filesystem, invocation.scale))
        .collect();
    let widths: Vec<usize> = headings
        .iter()
        .zip(&values)
        .map(|(heading, value)| heading.chars().count().max(value.chars().count()))
        .collect();
    // The first column is the device and the last is the mount point, both of which the utility
    // writes LEFT-aligned; every figure between them is right-aligned under its heading.
    let row = |cells: &[String]| -> String {
        let last = cells.len().saturating_sub(1);
        let mut line = String::new();
        for (index, cell) in cells.iter().enumerate() {
            if index > 0 {
                line.push(' ');
            }
            let width = widths.get(index).copied().unwrap_or(0);
            if index == 0 || index == last {
                line.push_str(&format!("{cell:<width$}"));
            } else {
                line.push_str(&format!("{cell:>width$}"));
            }
        }
        line.trim_end().to_string()
    };
    let mut lines: Vec<String> = Vec::new();
    if invocation.headings {
        lines.push(row(&headings));
    }
    lines.push(row(&values));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the `df` table: the arguments written as a shell hands them over, and the
    /// columns they ask for beside whether a heading is written — or `None` for an invocation
    /// this program performs nothing of.
    type ReportRow = (&'static str, &'static str, Option<(Vec<Field>, bool)>);

    /// One row of the `fallocate` table: the arguments, and the length and the path they name.
    type AllocateRow = (&'static str, &'static str, Option<(i64, &'static str)>);

    /// One row of the `stat` table: the arguments, and the format and the path they name.
    type StatusRow = (&'static str, &'static str, Option<(&'static str, &'static str)>);

    /// The filesystem of a machine whose root volume is `size_gib` and whose storage accounts
    /// for `stored_bytes`.
    fn filesystem(size_gib: i32, stored_bytes: usize) -> GuestFilesystem {
        GuestFilesystem::of(Some(size_gib), stored_bytes)
            .expect("a machine whose root volume size is known has a filesystem")
    }

    /// RULE: the size is the root volume's own and what is in use is what the image and the
    /// machine's own storage account for — over a table spanning the root volume sizes a launch
    /// can ask for.
    #[test]
    fn the_reading_follows_from_the_volume_and_what_the_machine_holds() {
        // (case, the volume size in GiB, the bytes the machine's storage accounts for)
        let rows = [
            ("the smallest root volume", 1_i32, 0_usize),
            ("the published Amazon Linux default", 8, 4_096),
            ("a volume filled by a reservation", 8, 2 * 1024 * 1024 * 1024),
            ("a large root volume", 16_384, 1_024),
        ];
        for (case, size_gib, stored) in rows {
            let held = filesystem(size_gib, stored);
            assert_eq!(
                held.total_bytes,
                i64::from(size_gib) * BYTES_PER_GIB,
                "case {case:?}: expected the volume's own size, got {held:?}"
            );
            assert!(
                held.used_bytes > 0 && held.used_bytes <= held.total_bytes,
                "case {case:?}: expected part of the filesystem in use, got {held:?}"
            );
            assert_eq!(
                held.free_bytes(),
                held.total_bytes - held.used_bytes,
                "case {case:?}: expected the rest free, got {held:?}"
            );
            let percent = held.used_percent();
            assert!(
                percent > 0.0 && percent <= 100.0,
                "case {case:?}: expected a share in (0, 100], got {percent}"
            );
        }
        // A machine whose root volume size this store does not hold has no filesystem at all,
        // and so no program that reports one.
        assert!(
            GuestFilesystem::of(None, 0).is_none(),
            "case \"no root volume size\": such a machine has no filesystem reading"
        );
        assert!(
            GuestFilesystem::of(Some(0), 0).is_none(),
            "case \"a root volume of no size\": neither has that one"
        );
    }

    /// RULE: more bytes accounted for is more of the filesystem in use, and the share `df`
    /// writes is the share the agent publishes.
    #[test]
    fn what_the_machine_holds_raises_the_share_in_use() {
        let empty = filesystem(8, 0);
        let filled = filesystem(8, 3 * 1024 * 1024 * 1024);
        assert!(
            filled.used_percent() > empty.used_percent(),
            "expected a filled filesystem to report a larger share, got {filled:?} against \
             {empty:?}"
        );
        // The column `df` writes is that same share, rounded up to the whole per cent.
        let written = Field::Pcent.value("/dev/nvme0n1", filled, Scale::Kibibytes);
        let rounded = format!("{}%", filled.used_percent().ceil() as i64);
        assert_eq!(
            written, rounded,
            "expected the column to carry the share the agent publishes, got {written}"
        );
    }

    /// RULE: the invocations `df` performs are the ones whose columns and units this emulator
    /// derives, in every spelling the options take.
    #[test]
    fn the_invocations_df_performs_are_the_ones_it_derives() {
        // (case, the arguments, the columns and whether a heading is written)
        let rows: Vec<ReportRow> = vec![
            (
                "no option at all",
                "",
                Some((DEFAULT_FIELDS.to_vec(), true)),
            ),
            (
                "the documented probe of how full the root filesystem is",
                "--output=pcent /",
                Some((vec![Field::Pcent], true)),
            ),
            (
                "the available blocks in mebibytes",
                "--output=avail -m /",
                Some((vec![Field::Avail], true)),
            ),
            (
                "several columns at once",
                "--output=size,used,avail,pcent /",
                Some((
                    vec![Field::Size, Field::Used, Field::Avail, Field::Pcent],
                    true,
                )),
            ),
            (
                "the field list written as the next word",
                "--output pcent /",
                Some((vec![Field::Pcent], true)),
            ),
            (
                "the human-readable form with no heading",
                "-h --no-headings /",
                Some((DEFAULT_FIELDS.to_vec(), false)),
            ),
            ("the POSIX form", "-P /", Some((DEFAULT_FIELDS.to_vec(), true))),
            ("a column this emulator derives nothing for", "--output=itotal /", None),
            ("a field list naming nothing", "--output= /", None),
            ("an option this program does not read", "-i /", None),
            ("a block size of its own", "--block-size=512 /", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let parsed = report_invocation(&words);
            let reported = parsed
                .as_ref()
                .map(|invocation| (invocation.fields.clone(), invocation.headings));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: a report carries the figures of the filesystem it was asked about, in the unit the
    /// invocation named, and the value is on its own line under its heading.
    #[test]
    fn a_report_carries_the_figures_of_the_filesystem() {
        let held = filesystem(8, 0);
        // (case, the arguments, the last line's digits)
        let rows = [
            ("the share in use", "--output=pcent /", "25"),
            ("the available mebibytes", "--output=avail -m /", "6144"),
            ("the size in kibibytes", "--output=size /", "8388608"),
            ("the used bytes in kibibytes", "--output=used /", "2097152"),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let invocation =
                report_invocation(&words).unwrap_or_else(|| panic!("case {case:?}: a form"));
            let lines = report_lines(&invocation, "/dev/nvme0n1", held);
            let last = lines.last().cloned().unwrap_or_default();
            let digits: String = last
                .chars()
                .filter(|character| character.is_ascii_digit() || *character == '.')
                .collect();
            assert_eq!(
                digits, expected,
                "case {case:?}: expected {expected:?} on the last line, got {lines:?}"
            );
            assert_eq!(
                lines.len(),
                2,
                "case {case:?}: expected a heading and one row, got {lines:?}"
            );
        }
        // The default report names the device and the mount point beside the figures.
        let invocation = report_invocation(&[]).expect("the default form");
        let lines = report_lines(&invocation, "/dev/nvme0n1", held);
        let row = lines.last().cloned().unwrap_or_default();
        assert!(
            row.starts_with("/dev/nvme0n1") && row.ends_with(ROOT_MOUNT_POINT),
            "expected the device and the mount point on the row, got {lines:?}"
        );
    }

    /// RULE: a size is read in every spelling the utility takes it in, and a word that states no
    /// size is no invocation of it.
    #[test]
    fn a_reservation_names_a_length_and_a_path() {
        // (case, the arguments, the length and the path)
        let rows: Vec<AllocateRow> = vec![
            (
                "the documented fill",
                "-l 2G /var/tmp/fill",
                Some((2 * 1024 * 1024 * 1024, "/var/tmp/fill")),
            ),
            (
                "the length glued to the option",
                "-l512M /var/tmp/fill",
                Some((512 * 1024 * 1024, "/var/tmp/fill")),
            ),
            (
                "the long spelling",
                "--length=1MiB /tmp/held",
                Some((1024 * 1024, "/tmp/held")),
            ),
            (
                "the decimal suffix",
                "-l 1MB /tmp/held",
                Some((1_000_000, "/tmp/held")),
            ),
            ("a length in plain bytes", "-l 4096 /tmp/held", Some((4_096, "/tmp/held"))),
            ("no length at all", "/tmp/held", None),
            ("no path at all", "-l 2G", None),
            ("a length that is not a size", "-l big /tmp/held", None),
            ("a length of nothing", "-l 0 /tmp/held", None),
            ("an option this program does not read", "-p -l 2G /tmp/held", None),
            ("two paths at once", "-l 2G /tmp/one /tmp/two", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let parsed = allocate_invocation(&words);
            let reported = parsed
                .as_ref()
                .map(|(length, path)| (*length, path.as_str()));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: the `stat` invocations this program performs are the FILE ones with a format, and
    /// the format writes the values this emulator derives.
    #[test]
    fn the_file_form_of_stat_writes_the_size() {
        // (case, the arguments, the format and the path)
        let rows: Vec<StatusRow> = vec![
            (
                "the documented probe of a file's size",
                "-c %s /var/tmp/fill",
                Some(("%s", "/var/tmp/fill")),
            ),
            (
                "the long spelling of the format",
                "--format=%s /var/tmp/fill",
                Some(("%s", "/var/tmp/fill")),
            ),
            // The FILESYSTEM form is another module's.
            ("the filesystem form", "-f -c %T /mnt/efs", None),
            ("no format at all", "/var/tmp/fill", None),
            ("no path at all", "-c %s", None),
        ];
        for (case, arguments, expected) in rows {
            let words: Vec<String> = arguments.split_whitespace().map(str::to_string).collect();
            let parsed = file_status_invocation(&words);
            let reported = parsed
                .as_ref()
                .map(|(format, path)| (format.as_str(), path.as_str()));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
        // (case, the format, what it writes for a file of 2 GiB)
        let formats = [
            ("the size alone", "%s", Some("2147483648")),
            ("the name and the size", "%n is %s", Some("/var/tmp/fill is 2147483648")),
            ("a literal per cent", "100%%", Some("100%")),
            ("a specifier this emulator derives nothing for", "%b", None),
        ];
        for (case, format, expected) in formats {
            let written = formatted_status(format, "/var/tmp/fill", 2 * 1024 * 1024 * 1024);
            assert_eq!(
                written.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: the human-readable form writes the largest power of 1024 a size fits in.
    #[test]
    fn the_human_readable_form_writes_the_largest_unit_the_size_fits_in() {
        // (case, the bytes, what is written)
        let rows = [
            ("under a kibibyte", 512_i64, "512"),
            ("a kibibyte exactly", 1_024, "1.0K"),
            ("some mebibytes", 5 * 1024 * 1024, "5.0M"),
            ("a whole number of gibibytes", 8 * BYTES_PER_GIB, "8.0G"),
            ("past ten gibibytes", 26 * BYTES_PER_GIB, "26G"),
            ("a tebibyte", 1024 * BYTES_PER_GIB, "1.0T"),
        ];
        for (case, bytes, expected) in rows {
            let written = human_size(bytes);
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: what the machine's storage accounts for is its files and the space its programs
    /// reserved, and a reservation is released when the file it was taken for is gone.
    #[test]
    fn the_storage_accounts_for_the_files_and_the_reservations() {
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        files.insert("/tmp/held".to_string(), "written".to_string());
        let mut runtime = GuestRuntime::default();
        let with_files = storage_bytes(&files, &runtime);
        assert!(
            with_files > 0,
            "expected the files to be accounted for, got {with_files}"
        );
        runtime
            .reserved
            .insert("/var/tmp/fill".to_string(), 2 * BYTES_PER_GIB);
        let reserved = storage_bytes(&files, &runtime);
        assert_eq!(
            reserved,
            with_files + usize::try_from(2 * BYTES_PER_GIB).unwrap_or(usize::MAX),
            "expected the reservation to be accounted for beside the files, got {reserved}"
        );
        // The file the space was reserved for is not on the machine, so the space is the
        // filesystem's again.
        release_removed_files(&mut runtime, &files);
        assert_eq!(
            storage_bytes(&files, &runtime),
            with_files,
            "expected a removed file's reservation to be released, got {:?}",
            runtime.reserved
        );
        // ... while a reservation whose file the machine still holds is kept.
        runtime
            .reserved
            .insert("/tmp/held".to_string(), BYTES_PER_GIB);
        release_removed_files(&mut runtime, &files);
        assert_eq!(
            reserved_bytes(&runtime),
            BYTES_PER_GIB,
            "expected a held file's reservation to be kept, got {:?}",
            runtime.reserved
        );
    }
}
