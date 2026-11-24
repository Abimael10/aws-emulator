// SPDX-License-Identifier: CC0-1.0

//! What one instance's GUEST sees of its own MEMORY.
//!
//! # Why a guest needs this
//!
//! How much memory a machine is using is a measurement NO API of this account can derive: the
//! control plane knows how much memory the instance type gives the guest
//! (`InstanceTypeInfo$MemoryInfo`), and everything else about it — how much of it is in use —
//! is inside the guest. That is why monitoring memory means reading it IN the guest: a script
//! run through Systems Manager reads `/proc/meminfo` or runs `free`, and the CloudWatch agent
//! publishes the same reading as its `mem_` measurements
//! ([`super::guest_cloudwatch_agent`]). A guest that publishes neither cannot be monitored for
//! memory at all, however faithfully the instance type is reported.
//!
//! # One derivation, two readers
//!
//! Both readings come from the ONE reading of a machine's memory below
//! ([`guest_memory`]): the TOTAL is the instance type's published size, and what is IN USE is
//! what this machine's own operating system accounts for. So the guest's own `free` and the
//! series the agent publishes cannot disagree about one machine, and a machine whose memory
//! size this emulator does not hold publishes neither file, neither program and no measurement
//! — the same answer [`super::guest_cpu`] gives for a layout it does not know.
//!
//! # No host memory
//!
//! Nothing here reads the memory of the host this process runs on: a guest's reading is a
//! reading of the EMULATED machine, computed from the stored instance. The programs below are
//! programs of the guest, performed by the same bounded, in-process shell session the guest's
//! own boot script runs in ([`super::guest_console`]).

use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

/// The path the kernel publishes a machine's memory figures at, which is where every guest
/// reader of them looks.
pub const GUEST_MEMINFO_PATH: &str = "/proc/meminfo";

/// The program that prints those figures as a table.
pub const MEMORY_REPORT_PROGRAM: &str = "free";

/// The status a program of this guest exits with when it ran.
const SUCCESS: i32 = 0;

/// How many kibibytes one mebibyte is, which is the unit `/proc/meminfo` states every figure
/// in (`kB`, the kernel's own label for a kibibyte).
const KIB_PER_MIB: i64 = 1024;

/// How many bytes one kibibyte is, which is what a measurement published in bytes is scaled
/// by.
const BYTES_PER_KIB: f64 = 1024.0;

/// What the OPERATING SYSTEM of a guest of this emulator holds: the kernel it booted and the
/// userland the image ships, which are in memory on every machine whether or not anything else
/// runs.
///
/// CHOSEN: no API of this account reports a machine's memory USE — that is the whole reason a
/// guest has to be read for it — so this emulator states what its own machines' operating
/// system accounts for. It sits below the memory of the smallest instance type offered, so
/// every machine has memory left for what it runs.
const OPERATING_SYSTEM_MEMORY_KIB: i64 = 192 * KIB_PER_MIB;

/// How much of a machine's memory may be reported in use at most, as a share of its total:
/// a machine with nothing free could not run the next program at all, so the figures a guest
/// of this emulator reads always leave some.
const MOST_MEMORY_IN_USE: f64 = 0.9;

/// The memory of one machine: how much it has and how much of it is in use, both in
/// kibibytes — the unit the kernel publishes them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestMemory {
    /// How much memory the instance type gives this guest (`MemoryInfo$SizeInMiB`, in
    /// kibibytes).
    pub total_kib: i64,
    /// How much of it is in use.
    pub used_kib: i64,
}

impl GuestMemory {
    /// How much memory is not in use, which is both what the kernel publishes as free and
    /// what it publishes as AVAILABLE: this machine keeps nothing in buffers or in a page
    /// cache, so every kibibyte that is not in use is available to the next program.
    #[must_use]
    pub fn free_kib(&self) -> i64 {
        self.total_kib.saturating_sub(self.used_kib).max(0)
    }

    /// The share of this machine's memory that is in use, as the percentage the agent's
    /// `mem_used_percent` and a guest's own `free` arithmetic both come to.
    #[must_use]
    pub fn used_percent(&self) -> f64 {
        if self.total_kib <= 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let share = self.used_kib as f64 / self.total_kib as f64;
        share * 100.0
    }

    /// The share that is available, which is the rest of it.
    #[must_use]
    pub fn available_percent(&self) -> f64 {
        100.0 - self.used_percent()
    }

    /// A figure of this machine's memory in BYTES, which is the unit the agent publishes its
    /// `mem_` measurements in.
    #[must_use]
    fn bytes(kib: i64) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let kib = kib as f64;
        kib * BYTES_PER_KIB
    }
}

/// How many bytes the machine's own storage holds, as the tree a guest's program reads and
/// writes accounts for one file: its path and its contents.
///
/// The ONE reading of it, so what a published figure of the machine's storage accounts for and
/// what the agent's filesystem measurement accounts for are the same bytes.
#[must_use]
pub fn stored_bytes(files: &::std::collections::BTreeMap<String, String>) -> usize {
    files
        .iter()
        .map(|(path, contents)| path.len().saturating_add(contents.len()))
        .sum()
}

/// The memory of the machine whose instance type gives it `memory_mib` — or `None` for a
/// machine whose memory size this emulator does not hold.
///
/// What is IN USE is the OPERATING SYSTEM it booted ([`OPERATING_SYSTEM_MEMORY_KIB`]) and
/// nothing else: this emulator runs no workload in a guest, so nothing beyond the operating
/// system is using memory on one — the same reason every guest measurement of an instance's own
/// series is zero ([`super::instance_telemetry`]). That is also what makes two readings of one
/// machine at two instants the SAME reading, so a program of the guest and the agent's own
/// collection cannot disagree about how much of its memory is in use.
#[must_use]
pub fn guest_memory(memory_mib: Option<i64>) -> Option<GuestMemory> {
    let total_kib = memory_mib.filter(|size| *size > 0)?.saturating_mul(KIB_PER_MIB);
    #[allow(clippy::cast_possible_truncation)]
    let ceiling = {
        #[allow(clippy::cast_precision_loss)]
        let total = total_kib as f64;
        (total * MOST_MEMORY_IN_USE) as i64
    };
    Some(GuestMemory {
        total_kib,
        used_kib: OPERATING_SYSTEM_MEMORY_KIB.min(ceiling).max(0),
    })
}

/// What the kernel publishes at [`GUEST_MEMINFO_PATH`] for `memory`: the figures a guest's own
/// readers parse, each in the `kB` the kernel states them in.
///
/// Only the figures this emulator really derives are written: this machine keeps nothing in
/// buffers or a page cache and has no swap, so those lines are the zeroes they are rather than
/// numbers nothing measured, and `MemAvailable` is what is free.
#[must_use]
pub fn meminfo_contents(memory: GuestMemory) -> String {
    [
        ("MemTotal:", memory.total_kib),
        ("MemFree:", memory.free_kib()),
        ("MemAvailable:", memory.free_kib()),
        ("Buffers:", 0),
        ("Cached:", 0),
        ("SwapCached:", 0),
        ("SwapTotal:", 0),
        ("SwapFree:", 0),
    ]
    .into_iter()
    .map(|(label, value)| format!("{label:<16}{value:>8} kB\n"))
    .collect()
}

/// How wide the FIRST column of the memory report is — the one carrying `Mem:` and `Swap:` —
/// and how wide each figure's column is, which is how `free` lays its table out.
const REPORT_LABEL_WIDTH: usize = 15;

/// See [`REPORT_LABEL_WIDTH`].
const REPORT_FIGURE_WIDTH: usize = 12;

/// What `free` prints for `memory`, in the unit `scale` names: the header, the memory row and
/// the swap row.
///
/// The columns are `free`'s own, so a guest's `free -m | awk '/^Mem:/ {print $3}'` reads the
/// figure it reads on a real instance.
#[must_use]
pub fn memory_report_lines(memory: GuestMemory, scale: MemoryScale) -> Vec<String> {
    let mut header = " ".repeat(REPORT_LABEL_WIDTH);
    for column in ["total", "used", "free", "shared", "buff/cache", "available"] {
        header.push_str(&format!("{column:>REPORT_FIGURE_WIDTH$}"));
    }
    let row = |label: &str, figures: &[i64]| {
        let mut line = format!("{label:<REPORT_LABEL_WIDTH$}");
        for figure in figures {
            let scaled = scale.of(*figure);
            line.push_str(&format!("{scaled:>REPORT_FIGURE_WIDTH$}"));
        }
        line
    };
    vec![
        header,
        row(
            "Mem:",
            &[
                memory.total_kib,
                memory.used_kib,
                memory.free_kib(),
                0,
                0,
                memory.free_kib(),
            ],
        ),
        // This machine has no swap, so every figure of that row is zero.
        row("Swap:", &[0, 0, 0]),
    ]
}

/// The unit `free` states its figures in, as the option the invocation carried names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScale {
    /// `-b`: bytes.
    Bytes,
    /// `-k`, and the default: kibibytes, the unit the kernel publishes them in.
    Kibibytes,
    /// `-m`: mebibytes.
    Mebibytes,
    /// `-g`: gibibytes.
    Gibibytes,
}

impl MemoryScale {
    /// The figure `kib` states in this unit, as `free` rounds one: down to the whole unit.
    #[must_use]
    fn of(self, kib: i64) -> i64 {
        match self {
            Self::Bytes => kib.saturating_mul(1024),
            Self::Kibibytes => kib,
            Self::Mebibytes => kib / 1024,
            Self::Gibibytes => kib / (1024 * 1024),
        }
    }

    /// The unit the option `word` names, or `None` for a word that names none of them.
    #[must_use]
    fn named(word: &str) -> Option<Self> {
        match word {
            "-b" | "--bytes" => Some(Self::Bytes),
            "-k" | "--kibi" | "--kilo" => Some(Self::Kibibytes),
            "-m" | "--mebi" | "--mega" => Some(Self::Mebibytes),
            "-g" | "--gibi" | "--giga" => Some(Self::Gibibytes),
            _ => None,
        }
    }
}

/// The measurements of MEMORY the CloudWatch agent takes, as the names it publishes each
/// under, with the unit of each.
///
/// The names are the agent's own `mem` measurements (the CloudWatch agent configuration
/// reference's `mem` section), and only the ones this emulator really derives are here: a
/// figure of a page cache or of swap this machine does not keep is not a measurement nothing
/// took.
pub const MEMORY_MEASUREMENTS: &[(&str, &str)] = &[
    ("mem_total", super::instance_telemetry::BYTES_UNIT),
    ("mem_used", super::instance_telemetry::BYTES_UNIT),
    ("mem_free", super::instance_telemetry::BYTES_UNIT),
    ("mem_available", super::instance_telemetry::BYTES_UNIT),
    ("mem_used_percent", super::instance_telemetry::PERCENT_UNIT),
    (
        "mem_available_percent",
        super::instance_telemetry::PERCENT_UNIT,
    ),
];

/// What the agent's memory measurement `name` measures on a machine whose memory is `memory`,
/// with the unit it is published in — and `None` for a name that is not one of
/// [`MEMORY_MEASUREMENTS`].
#[must_use]
pub fn memory_measurement(name: &str, memory: GuestMemory) -> Option<(f64, &'static str)> {
    let unit = MEMORY_MEASUREMENTS
        .iter()
        .find(|(measurement, _)| *measurement == name)
        .map(|(_, unit)| *unit)?;
    let value = match name {
        "mem_total" => GuestMemory::bytes(memory.total_kib),
        "mem_used" => GuestMemory::bytes(memory.used_kib),
        "mem_free" | "mem_available" => GuestMemory::bytes(memory.free_kib()),
        "mem_used_percent" => memory.used_percent(),
        "mem_available_percent" => memory.available_percent(),
        _ => return None,
    };
    Some((value, unit))
}

/// The programs a guest reads its own memory with, for a session that runs a program of that
/// guest — `None` for a machine whose memory size this emulator does not hold, which has no
/// program that answers one either.
#[must_use]
pub fn memory_programs(memory: Option<GuestMemory>) -> Option<Box<dyn EnvironmentPrograms>> {
    Some(Box::new(MemoryPrograms { memory: memory? }))
}

/// The memory programs of one machine's guest.
///
/// Holds that machine's memory and nothing else — no store guard, no engine and no host handle
/// is reachable from a program.
#[derive(Debug)]
struct MemoryPrograms {
    /// The machine's memory, read once when the programs were installed.
    memory: GuestMemory,
}

impl EnvironmentPrograms for MemoryPrograms {
    /// `free` prints the machine's memory figures, in the unit its options name.
    ///
    /// A program named by its absolute path is the same program, as a shell resolving a name
    /// on `PATH` finds. An invocation carrying an option this program does not perform answers
    /// `None` — a form this guest performs nothing of — rather than a table that would not be
    /// the answer to what was asked.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let _ = files;
        if name.rsplit('/').next().unwrap_or(name) != MEMORY_REPORT_PROGRAM {
            return None;
        }
        let mut scale = MemoryScale::Kibibytes;
        for word in arguments {
            scale = MemoryScale::named(word)?;
        }
        Some(ProgramOutcome {
            status: SUCCESS,
            output: memory_report_lines(self.memory, scale),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memory of a machine of `memory_mib`.
    fn idle(memory_mib: i64) -> GuestMemory {
        guest_memory(Some(memory_mib)).expect("a machine whose memory size is known has memory")
    }

    /// RULE: the total is the instance type's published size and what is in use is what the
    /// machine's own state accounts for — over a table spanning the memory sizes of the
    /// instance types this emulator offers.
    #[test]
    fn the_reading_follows_from_the_instance_type_and_what_the_machine_runs() {
        // (case, the type's memory in MiB)
        let rows = [
            ("the smallest machine", 512_i64),
            ("a t3.micro", 1_024),
            ("an m5.large", 8_192),
            ("a large machine", 131_072),
        ];
        for (case, memory_mib) in rows {
            let memory = idle(memory_mib);
            assert_eq!(
                memory.total_kib,
                memory_mib * KIB_PER_MIB,
                "case {case:?}: expected the type's own size, got {memory:?}"
            );
            assert!(
                memory.used_kib > 0 && memory.used_kib < memory.total_kib,
                "case {case:?}: expected part of the memory in use, got {memory:?}"
            );
            let percent = memory.used_percent();
            assert!(
                percent > 0.0 && percent <= 100.0,
                "case {case:?}: expected a share in (0, 100], got {percent}"
            );
            // The two halves of the same reading: what is free is the rest of it.
            assert_eq!(
                memory.free_kib(),
                memory.total_kib - memory.used_kib,
                "case {case:?}: expected the rest free, got {memory:?}"
            );
        }
        // A machine whose memory size this emulator does not hold has no reading at all.
        assert!(
            guest_memory(None).is_none(),
            "case \"no published size\": a machine with none has no memory reading"
        );
        assert!(
            memory_programs(None).is_none(),
            "case \"no published size\": such a machine has no memory programs either"
        );
    }

    /// RULE: two readings of one machine are the SAME reading, and a machine never reports all
    /// of its memory in use.
    #[test]
    fn two_readings_of_one_machine_are_one_reading() {
        let idle_machine = idle(1_024);
        let again = guest_memory(Some(1_024)).expect("a known size");
        assert_eq!(
            again, idle_machine,
            "expected two readings of one machine to be one reading, got {again:?} against \
             {idle_machine:?}"
        );
        // A machine with less memory than its operating system accounts for still reports
        // memory free.
        let crowded = guest_memory(Some(64)).expect("a size");
        assert!(
            crowded.used_kib < crowded.total_kib && crowded.free_kib() > 0,
            "expected a machine to keep memory free, got {crowded:?}"
        );
    }

    /// RULE: every reader of one machine reads the same figures — the published file, the
    /// report program and the agent's measurement.
    #[test]
    fn the_file_the_program_and_the_measurement_agree() {
        let memory = idle(1_024);
        let published = meminfo_contents(memory);
        let figure = |label: &str| {
            published
                .lines()
                .find(|line| line.starts_with(label))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or_else(|| panic!("{label} must be published, got {published:?}"))
        };
        assert_eq!(
            (figure("MemTotal:"), figure("MemFree:")),
            (memory.total_kib, memory.free_kib()),
            "expected the published file to carry the machine's figures, got {published:?}"
        );
        assert_eq!(
            figure("MemAvailable:"),
            figure("MemFree:"),
            "expected what is free to be available, got {published:?}"
        );
        // The report states the same figures, in the unit the invocation asked for.
        let lines = memory_report_lines(memory, MemoryScale::Mebibytes);
        let row = lines
            .iter()
            .find(|line| line.starts_with("Mem:"))
            .expect("the report has a memory row");
        let figures: Vec<i64> = row
            .split_whitespace()
            .skip(1)
            .filter_map(|word| word.parse().ok())
            .collect();
        assert_eq!(
            figures.first().copied(),
            Some(memory.total_kib / 1024),
            "expected the total in mebibytes, got {row:?}"
        );
        assert_eq!(
            figures.get(1).copied(),
            Some(memory.used_kib / 1024),
            "expected what is in use in mebibytes, got {row:?}"
        );
        // ... and the agent's measurement is that same reading, in the unit it publishes.
        let (used_percent, unit) =
            memory_measurement("mem_used_percent", memory).expect("the measurement is taken");
        assert!(
            (used_percent - memory.used_percent()).abs() < f64::EPSILON,
            "expected the agent's share to be the machine's, got {used_percent}"
        );
        assert_eq!(
            unit,
            super::super::instance_telemetry::PERCENT_UNIT,
            "expected a percentage to be published as one, got {unit}"
        );
        let (used_bytes, unit) =
            memory_measurement("mem_used", memory).expect("the measurement is taken");
        #[allow(clippy::cast_precision_loss)]
        let expected = memory.used_kib as f64 * BYTES_PER_KIB;
        assert!(
            (used_bytes - expected).abs() < f64::EPSILON,
            "expected the bytes in use, got {used_bytes}"
        );
        assert_eq!(
            unit,
            super::super::instance_telemetry::BYTES_UNIT,
            "expected bytes to be published as bytes, got {unit}"
        );
        assert!(
            memory_measurement("mem_not_a_measurement", memory).is_none(),
            "a name the agent takes no measurement for is measured by nothing"
        );
    }
}
