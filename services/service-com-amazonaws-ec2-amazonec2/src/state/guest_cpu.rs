// SPDX-License-Identifier: CC0-1.0

//! What one instance's GUEST sees of its own CPUs.
//!
//! # Why a guest needs this
//!
//! "The CPU options for the instance. Both the core count and threads per core must be
//! specified in the request" (`CpuOptionsRequest`) is a request about the MACHINE: a launch
//! that asks for one thread per core is asking for a guest whose operating system sees one
//! logical processor per core, which is what halves a per-core licence and what gives
//! deterministic single-thread performance. A program inside the guest is how that is read —
//! `nproc`, `lscpu`, `/proc/cpuinfo` and `/sys/devices/system/cpu/online` are where every
//! licence check, every thread-pool sizing and every placement script looks — so a guest that
//! publishes none of them cannot tell its own topology, however faithfully the control plane
//! reports it.
//!
//! # One derivation, two readers
//!
//! Every value here comes from the ONE reading of an instance's CPU topology this crate has
//! ([`super::InstanceDetail::cpu_options`]): the cores and threads per core the LAUNCH asked
//! for, and its instance type's published counts for a member the launch left out. So
//! `DescribeInstances` and the guest's own `lscpu` cannot disagree about one instance, and
//! nothing here is echoed from a request or invented for a machine whose layout this emulator
//! does not know — a machine with no layout publishes neither file and neither program.
//!
//! # No processor, no host
//!
//! The programs below are PROGRAMS OF THE GUEST, performed by the same bounded, in-process
//! shell session the guest's own boot script runs in ([`super::guest_console`]): they start no
//! process, read no host path and report only what the stored instance says. The fields the
//! published files carry are therefore the ones this emulator really derives — the processors,
//! how they sit on cores and sockets, and which of them are online — and no field is written
//! for a fact nothing here holds.

use ::service_common::posix_shell_session::{
    EnvironmentPrograms, ProgramFiles, ProgramOutcome,
};

use super::guest_console::GuestMachine;

/// The path the kernel publishes one line per logical processor at, which is where a guest
/// reads its own processors, their sockets and their cores from.
pub const GUEST_CPUINFO_PATH: &str = "/proc/cpuinfo";

/// The path the kernel publishes the range of logical processors that are ONLINE at.
pub const GUEST_CPU_ONLINE_PATH: &str = "/sys/devices/system/cpu/online";

/// The directory the kernel publishes each logical processor's own files under.
pub const GUEST_CPU_DIRECTORY: &str = "/sys/devices/system/cpu";

/// The program that prints how many processors a guest can use.
pub const CPU_COUNT_PROGRAM: &str = "nproc";

/// The program that prints a guest's whole CPU layout.
pub const CPU_LAYOUT_PROGRAM: &str = "lscpu";

/// The one option `nproc` takes that changes nothing here: "print the number of installed
/// processors" rather than the number available to the process, which on this machine is the
/// same count.
const ALL_PROCESSORS_OPTION: &str = "--all";

/// The status a program of this guest exits with when it ran.
const SUCCESS: i32 = 0;

/// How wide the label column of the CPU layout is, which is how `lscpu` lays its report out:
/// the label, then its value in a second column.
const LAYOUT_LABEL_WIDTH: usize = 22;

/// The sockets a guest of this emulator sees its cores on.
///
/// CHOSEN: no EC2 API publishes how many processor sockets an instance's cores are presented
/// on, and every CPU option a launch asks for is stated in cores and threads per core
/// (`CpuOptionsRequest`), so the cores a launch asked for are presented on one socket.
const GUEST_SOCKETS: i32 = 1;

/// The hypervisor a guest of this emulator reports running under, which is the one its
/// console's kernel lines report detecting ([`super::guest_console`]).
const GUEST_HYPERVISOR: &str = "KVM";

/// The most logical processors a guest publishes a layout for.
///
/// A capacity bound rather than an AWS limit: the kernel publishes one file per processor
/// ([`published_cpu_files`]), so the cost of that seed is bounded here rather than by
/// whatever count a record carries. It sits above the vCPUs of every instance type this
/// emulator offers, so no machine a launch can mint is held to it — what it keeps out is a
/// layout no published instance type has.
pub const MAX_GUEST_PROCESSORS: i32 = 1_024;

/// The virtualization a guest reports under that hypervisor.
const GUEST_VIRTUALIZATION: &str = "full";

/// The CPU layout of one machine: the cores it was given and the threads each one runs.
///
/// Both are the instance's own ([`super::InstanceDetail::cpu_options`]), never the instance
/// type's where the launch asked for something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestCpu {
    /// "The number of CPU cores for the instance" (`CpuOptions$CoreCount`).
    pub cores: i32,
    /// "The number of threads per CPU core" (`CpuOptions$ThreadsPerCore`).
    pub threads_per_core: i32,
}

impl GuestCpu {
    /// The CPU layout `options` states, or `None` when they state neither count — a machine
    /// whose layout this emulator does not know publishes none of it, and so does one whose
    /// processors are more than a guest publishes a layout for
    /// ([`MAX_GUEST_PROCESSORS`]).
    #[must_use]
    pub fn of(options: &crate::ops::types::CpuOptions) -> Option<Self> {
        let cpu = Self {
            cores: options.core_count?.max(1),
            threads_per_core: options.threads_per_core?.max(1),
        };
        (cpu.vcpus() <= MAX_GUEST_PROCESSORS).then_some(cpu)
    }

    /// The LOGICAL processors the guest's operating system sees: one per thread of every
    /// core, which is what `nproc` prints and what the online range covers.
    #[must_use]
    pub fn vcpus(&self) -> i32 {
        self.cores.saturating_mul(self.threads_per_core)
    }

    /// The threads one socket runs — `siblings` in `/proc/cpuinfo` — which on a machine whose
    /// cores sit on one socket ([`GUEST_SOCKETS`]) is every thread it has.
    #[must_use]
    pub fn siblings(&self) -> i32 {
        self.vcpus()
    }

    /// The CORE the logical processor `processor` is a thread of.
    ///
    /// The kernel enumerates a machine's processors by THREAD and then by core — the first
    /// thread of every core, then the second thread of every core — which is the enumeration
    /// an instance's own `/proc/cpuinfo` and `lscpu` report, and what makes the siblings of
    /// one core a stride apart.
    #[must_use]
    pub fn core_of(&self, processor: i32) -> i32 {
        processor % self.cores.max(1)
    }

    /// The processors that are threads of the SAME core as `processor`, as the
    /// comma-separated list the kernel publishes at `topology/thread_siblings_list`.
    #[must_use]
    pub fn thread_siblings_list(&self, processor: i32) -> String {
        let cores = self.cores.max(1);
        let core = self.core_of(processor);
        (0..self.threads_per_core.max(1))
            .map(|thread| (core + thread.saturating_mul(cores)).to_string())
            .collect::<Vec<String>>()
            .join(",")
    }
}

/// What the kernel publishes at [`GUEST_CPU_ONLINE_PATH`]: the range of logical processors
/// that are online, which on a machine whose processors are all up is every one of them.
///
/// The kernel writes a RANGE (`0-1`), and a single processor as the one number it is — the
/// form a guest's own scripts parse.
#[must_use]
pub fn cpu_online_contents(cpu: GuestCpu) -> String {
    let last = cpu.vcpus().saturating_sub(1).max(0);
    if last == 0 {
        return "0\n".to_string();
    }
    format!("0-{last}\n")
}

/// What the kernel publishes at [`GUEST_CPUINFO_PATH`]: one block per logical processor,
/// carrying that processor's number, the socket it is on, the core of that socket it is a
/// thread of, how many cores the socket holds and how many threads it runs.
///
/// Each block is the kernel's own `<field>\t: <value>` layout, blocks separated by a blank
/// line, so a guest's `grep ^processor` and `grep ^siblings` read what they read on a real
/// instance. The fields are exactly the ones this emulator derives: nothing is written for a
/// processor model, a frequency or a flag set nothing here holds.
#[must_use]
pub fn cpuinfo_contents(cpu: GuestCpu) -> String {
    let mut written = String::new();
    for processor in 0..cpu.vcpus() {
        for (field, value) in [
            ("processor", processor.to_string()),
            ("physical id", (GUEST_SOCKETS - 1).to_string()),
            ("siblings", cpu.siblings().to_string()),
            ("core id", cpu.core_of(processor).to_string()),
            ("cpu cores", cpu.cores.to_string()),
        ] {
            written.push_str(&format!("{field}\t: {value}\n"));
        }
        written.push('\n');
    }
    written
}

/// Every file the kernel publishes about this machine's processors, as `(path, contents)`:
/// the processor blocks, the online range, and one topology directory per logical processor.
///
/// The topology directory is where a guest's own scripts read a processor's place from —
/// which core of which socket it is a thread of, and which other processors are threads of
/// the same core (`thread_siblings_list`) or sit on the same socket (`core_siblings_list`).
/// Each list is written as the comma-separated processor list the kernel writes there.
#[must_use]
pub fn published_cpu_files(cpu: GuestCpu) -> Vec<(String, String)> {
    let mut published = vec![
        (GUEST_CPUINFO_PATH.to_string(), cpuinfo_contents(cpu)),
        (GUEST_CPU_ONLINE_PATH.to_string(), cpu_online_contents(cpu)),
    ];
    let every_processor = (0..cpu.vcpus())
        .map(|processor| processor.to_string())
        .collect::<Vec<String>>()
        .join(",");
    for processor in 0..cpu.vcpus() {
        let directory = format!("{GUEST_CPU_DIRECTORY}/cpu{processor}/topology");
        for (file, contents) in [
            ("core_id", cpu.core_of(processor).to_string()),
            ("physical_package_id", (GUEST_SOCKETS - 1).to_string()),
            ("thread_siblings_list", cpu.thread_siblings_list(processor)),
            ("core_siblings_list", every_processor.clone()),
        ] {
            published.push((format!("{directory}/{file}"), format!("{contents}\n")));
        }
    }
    published
}

/// What `lscpu` prints for `machine`: its architecture, its processors, which of them are
/// online, how they sit on cores and sockets, and the hypervisor it runs under.
///
/// The labels are `lscpu`'s own, each followed by its value in the second column
/// ([`LAYOUT_LABEL_WIDTH`]), so a guest's `lscpu | awk '/^CPU\(s\):/ {print $2}'` reads the
/// count it reads on a real instance.
#[must_use]
pub fn cpu_layout_lines(machine: &GuestMachine, cpu: GuestCpu) -> Vec<String> {
    let online = cpu_online_contents(cpu).trim_end().to_string();
    [
        ("Architecture:", machine.architecture.clone()),
        ("CPU(s):", cpu.vcpus().to_string()),
        ("On-line CPU(s) list:", online),
        ("Thread(s) per core:", cpu.threads_per_core.to_string()),
        ("Core(s) per socket:", cpu.cores.to_string()),
        ("Socket(s):", GUEST_SOCKETS.to_string()),
        ("Hypervisor vendor:", GUEST_HYPERVISOR.to_string()),
        ("Virtualization type:", GUEST_VIRTUALIZATION.to_string()),
    ]
    .into_iter()
    .map(|(label, value)| format!("{label:<LAYOUT_LABEL_WIDTH$}{value}"))
    .collect()
}

/// The programs a guest reads its own CPU layout with, for a session that runs a program of
/// that guest.
///
/// `None` for a machine whose layout this emulator does not know: a guest that cannot be
/// told its topology has no program that answers one either, which is the same answer a
/// shell gives for a utility it has not got.
#[must_use]
pub fn cpu_programs(machine: &GuestMachine) -> Option<Box<dyn EnvironmentPrograms>> {
    let cpu = machine.cpu?;
    Some(Box::new(CpuPrograms {
        machine: machine.clone(),
        cpu,
    }))
}

/// The CPU programs of one machine's guest.
///
/// Holds the machine and its layout and nothing else — no store guard, no engine and no host
/// handle is reachable from a program.
#[derive(Debug)]
struct CpuPrograms {
    /// The machine whose layout these programs report.
    machine: GuestMachine,
    /// That machine's layout, read once when the programs were installed.
    cpu: GuestCpu,
}

impl EnvironmentPrograms for CpuPrograms {
    /// `nproc` prints the logical processors the guest has; `lscpu` prints the whole layout.
    ///
    /// A program named by its absolute path is the same program, as a shell resolving a name
    /// on `PATH` finds. An invocation carrying an option neither program here performs
    /// answers `None` — a form this guest performs nothing of — rather than a count that
    /// would not be the answer to what was asked.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let _ = files;
        let name = name.rsplit('/').next().unwrap_or(name);
        match name {
            CPU_COUNT_PROGRAM => {
                if arguments
                    .iter()
                    .any(|argument| argument != ALL_PROCESSORS_OPTION)
                {
                    return None;
                }
                Some(ProgramOutcome {
                    status: SUCCESS,
                    output: vec![self.cpu.vcpus().to_string()],
                })
            }
            CPU_LAYOUT_PROGRAM => {
                if !arguments.is_empty() {
                    return None;
                }
                Some(ProgramOutcome {
                    status: SUCCESS,
                    output: cpu_layout_lines(&self.machine, self.cpu),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine of the shape a launch stores, with `cpu` as its layout.
    fn machine(cpu: Option<GuestCpu>) -> GuestMachine {
        GuestMachine {
            instance_id: "i-0123456789abcdef0".to_string(),
            instance_type: "m5.large".to_string(),
            image_id: None,
            architecture: "x86_64".to_string(),
            availability_zone: "us-west-2a".to_string(),
            private_ip: "10.0.0.5".to_string(),
            private_dns_name: "ip-10-0-0-5.us-west-2.compute.internal".to_string(),
            public_ip: None,
            launch_time: ::std::time::SystemTime::UNIX_EPOCH,
            kernel_release: "6.1.0".to_string(),
            root_device: None,
            processor: None,
            cpu,
            kernel_id: None,
            ramdisk_id: None,
            metadata_endpoint_enabled: true,
            metadata_token_required: false,
            instance_profile: None,
            block_devices: Vec::new(),
        }
    }

    /// RULE: every published fact of the layout follows from the cores and the threads per
    /// core, over a table spanning one core with multithreading off, one core with it on,
    /// and several cores.
    #[test]
    fn the_published_layout_follows_from_the_cores_and_threads() {
        // (case, cores, threads per core, processors, the online range)
        let rows = [
            ("one core, multithreading off", 1, 1, 1, "0"),
            ("one core, multithreading on", 1, 2, 2, "0-1"),
            ("four cores, multithreading off", 4, 1, 4, "0-3"),
            ("eight cores, multithreading on", 8, 2, 16, "0-15"),
        ];
        for (case, cores, threads_per_core, vcpus, online) in rows {
            let cpu = GuestCpu {
                cores,
                threads_per_core,
            };
            assert_eq!(
                cpu.vcpus(),
                vcpus,
                "case {case:?}: expected {vcpus} processors, got {}",
                cpu.vcpus()
            );
            assert_eq!(
                cpu_online_contents(cpu),
                format!("{online}\n"),
                "case {case:?}: expected the online range {online:?}, got {:?}",
                cpu_online_contents(cpu)
            );
            let cpuinfo = cpuinfo_contents(cpu);
            let processors = cpuinfo
                .lines()
                .filter(|line| line.starts_with("processor"))
                .count();
            assert_eq!(
                processors, vcpus as usize,
                "case {case:?}: expected {vcpus} processor blocks, got {cpuinfo:?}"
            );
            assert!(
                cpuinfo
                    .lines()
                    .any(|line| line == format!("siblings\t: {vcpus}")),
                "case {case:?}: expected siblings {vcpus}, got {cpuinfo:?}"
            );
            assert!(
                cpuinfo
                    .lines()
                    .any(|line| line == format!("cpu cores\t: {cores}")),
                "case {case:?}: expected {cores} cores per socket, got {cpuinfo:?}"
            );
            let layout = cpu_layout_lines(&machine(Some(cpu)), cpu);
            for expected in [
                format!("CPU(s):{:width$}{vcpus}", "", width = LAYOUT_LABEL_WIDTH - 7),
                format!(
                    "Thread(s) per core:{:width$}{threads_per_core}",
                    "",
                    width = LAYOUT_LABEL_WIDTH - 19
                ),
                format!(
                    "Core(s) per socket:{:width$}{cores}",
                    "",
                    width = LAYOUT_LABEL_WIDTH - 19
                ),
            ] {
                assert!(
                    layout.contains(&expected),
                    "case {case:?}: expected the line {expected:?}, got {layout:?}"
                );
            }
        }
    }

    /// RULE: a machine whose layout this emulator does not know has no CPU programs at all —
    /// nothing answers a count it does not hold — and neither has one whose processors are
    /// more than a guest publishes a layout for.
    #[test]
    fn a_machine_with_no_known_layout_has_no_cpu_programs() {
        assert!(
            cpu_programs(&machine(None)).is_none(),
            "case \"no layout\": a guest with no known layout publishes no CPU programs"
        );
        assert!(
            cpu_programs(&machine(Some(GuestCpu {
                cores: 1,
                threads_per_core: 1
            })))
            .is_some(),
            "case \"a known layout\": a guest with one publishes them"
        );
        let options = |cores, threads_per_core| crate::ops::types::CpuOptions {
            core_count: Some(cores),
            threads_per_core: Some(threads_per_core),
            amd_sev_snp: None,
            nested_virtualization: None,
        };
        assert_eq!(
            GuestCpu::of(&options(MAX_GUEST_PROCESSORS, 1)),
            Some(GuestCpu {
                cores: MAX_GUEST_PROCESSORS,
                threads_per_core: 1
            }),
            "case \"the most processors a layout is published for\": it is published"
        );
        assert_eq!(
            GuestCpu::of(&options(MAX_GUEST_PROCESSORS, 2)),
            None,
            "case \"one past that bound\": no layout is published at all"
        );
    }
}
