// SPDX-License-Identifier: CC0-1.0

//! The round trip a client performs to CONFIRM a mount target, driven as a SCRIPT: mount the
//! address the control plane reported, write a file through the mount, read it back, and ask
//! the two independent questions about the filesystem it landed on.
//!
//! RULE under test: the instant `DescribeMountTargets` reports a mount target `available` is
//! the instant the file system is served at the `IpAddress` that same read reports — so a
//! client in that mount target's VPC mounts it on the FIRST attempt of a retry loop, reads
//! back byte for byte what it wrote through the mount, and both probes of the mount report a
//! network filesystem (`findmnt`'s own `FSTYPE` is the mount's type, `stat -f`'s `%T` the type
//! a statfs of it reports). An address no `available` mount target of the client's own VPC
//! answers at serves nothing however long the client retries, so such a loop exhausts and the
//! script ends on the status `mount` itself failed with.
//!
//! What this suite drives that `all/nfs_data_plane.rs` does not:
//!
//! * the mount is performed by a SCRIPT in a shell session over a machine's tree — the same
//!   session a delivered command is performed in — rather than by one call of the mount
//!   program, so the loop, the probes and the script's own exit status are the client's own;
//! * the exports are read at the instant the CONTROL PLANE reported `available` through its own
//!   documented read, rather than at an instant this suite computed from the lifecycle span.
//!   That is what makes `ATTEMPTS:1` the assertion it is: a data plane that starts serving
//!   later than the state it is announced by would need a second attempt.
//!
//! Nothing here waits for a span or asserts on elapsed time: the one wait is the harness's own
//! poll of the CONDITION (`LifeCycleState`), and the retry loop's own `sleep` is time the
//! guest's program STATES it took — no thread waits for it.
//!
//! The rows span what the rule generalises over: the bytes written through the mount (a plain
//! marker, one carrying punctuation, one carrying spaces, and a long one), the address mounted
//! (a serving mount target's own, and an address of its subnet nothing answers at), and where
//! the client stands (the mount target's VPC, and another VPC of the same account).

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use service_com_amazonaws_ec2_amazonec2::state::guest_mounts::mount_programs;
use service_common::posix_shell_session::{
    layered, EnvironmentPrograms, OutputLimits, SessionConfig, ShellSession,
};
use service_common::posix_shell_volume::{ShellVolume, VolumeLimits};

use crate::harness::{Emulator, SubnetFacts};

/// The status a script ends on when every command of it did what it was asked.
const SUCCESS: i32 = 0;

/// The status `mount` fails with: "32 mount failure" (`mount(8)`, EXIT STATUS), which is the
/// status the script below ends on when its retry window is exhausted.
const MOUNT_FAILURE: i32 = 32;

/// The directory the script mounts the file system at.
const MOUNT_POINT: &str = "/mnt/sim2real-efs-test";

/// The options the documented Amazon EFS mount command carries.
const MOUNT_OPTIONS: &str =
    "nfsvers=4.1,rsize=1048576,wsize=1048576,hard,timeo=600,retrans=2,noresvport";

/// How many times the script tries the mount before it gives up, and how long it states it
/// waits between two attempts — the client's own retry window, not a span of this emulator.
const MOUNT_ATTEMPTS: u32 = 6;
const RETRY_SECONDS: u32 = 10;

/// What the two probes of an Amazon EFS mount report: the mount's own filesystem TYPE, and the
/// type a statfs of a path on it reports. They are different strings on purpose — a local
/// directory standing in for a network filesystem answers neither of them this way.
const MOUNT_FSTYPE: &str = "nfs4";
const STATFS_TYPE: &str = "nfs";

/// How much of one machine's own tree a case is given. Far above what a script writes.
const MACHINE_LIMITS: VolumeLimits = VolumeLimits {
    files: 64,
    bytes: 256 * 1024,
    path_bytes: 4096,
    directories: 64,
    device_lines: 0,
    device_line_bytes: 0,
};

/// How long a poll loop waits before looking again, and the generous deadline it gives the
/// condition it polls. The deadline bounds nothing a case measures: it only turns a transition
/// that never comes into a failure instead of a hang.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const DEADLINE: Duration = Duration::from_secs(60);

/// What performing one script in a guest came to, as the two stream members of a delivered
/// command's report carry it.
#[derive(Debug)]
struct GuestRun {
    status: i32,
    output: Vec<String>,
    errors: Vec<String>,
}

impl GuestRun {
    /// Whether the script wrote `line` on standard output, exactly.
    fn wrote(&self, line: &str) -> bool {
        self.output.iter().any(|written| written == line)
    }

    /// Whether any line of either stream names `text`.
    fn mentions(&self, text: &str) -> bool {
        self.output
            .iter()
            .chain(self.errors.iter())
            .any(|line| line.contains(text))
    }

    /// How many lines of standard error name `text` — the script's own count of failed
    /// attempts.
    fn error_lines_naming(&self, text: &str) -> usize {
        self.errors
            .iter()
            .filter(|line| line.contains(text))
            .count()
    }
}

/// The configuration a delivered command's session is given: both streams kept apart, because
/// the report a client reads carries a member for each.
fn session_config() -> SessionConfig {
    SessionConfig {
        output: Some(OutputLimits {
            lines: 256,
            line_bytes: 8 * 1024,
        }),
        ..SessionConfig::default()
    }
}

/// Perform `script` in the guest of ONE machine: a shell session over a tree of that machine's
/// own, with the programs its guest has — the mount client of this service's data plane beside
/// the programs a guest asks about its own mounts with.
fn run_in_guest(programs: Box<dyn EnvironmentPrograms>, script: &str) -> GuestRun {
    let volume = ShellVolume::new(MACHINE_LIMITS);
    let mut session = ShellSession::in_volume(BTreeMap::new(), session_config(), volume)
        .with_environment_programs(programs)
        .reporting_both_streams();
    let mut status = session.run(script);
    if let Some(exited) = session.exit_status() {
        // The script ended the session itself (`exit`): that status is the run's.
        status = exited;
    }
    GuestRun {
        status,
        output: session.output().to_vec(),
        errors: session.errors().to_vec(),
    }
}

/// The programs the guest of a machine standing in `vpc_id` has, with this service's exports
/// read AT `at`: the machine's own mount reporters ([`mount_programs`], the service that owns
/// instances) and the NFS client this service hands the component that reaches into a machine.
fn guest_programs(
    emulator: &Emulator,
    at: SystemTime,
    vpc_id: Option<&str>,
) -> Box<dyn EnvironmentPrograms> {
    layered(mount_programs(), emulator.nfs_client(at, vpc_id))
}

/// The script a client confirms a mount with: mount the address, retrying a bounded number of
/// times, write `content` into `file` through the mount, read it back, ask both probes about
/// the mount point, and take the mount away again.
fn round_trip_script(address: &str, file: &str, content: &str) -> String {
    let attempts: Vec<String> = (1..=MOUNT_ATTEMPTS).map(|number| number.to_string()).collect();
    let attempts = attempts.join(" ");
    format!(
        "set -e\n\
         sudo mkdir -p {MOUNT_POINT}\n\
         mounted=no\n\
         for attempt in {attempts}; do\n\
         if sudo mount -t nfs4 -o {MOUNT_OPTIONS} {address}:/ {MOUNT_POINT}; then\n\
         echo ATTEMPTS:$attempt\n\
         mounted=yes\n\
         break\n\
         fi\n\
         echo \"mount attempt $attempt failed, retrying in {RETRY_SECONDS}s\" >&2\n\
         sleep {RETRY_SECONDS}\n\
         done\n\
         if [ \"$mounted\" = no ]; then\n\
         echo \"mount never succeeded within the retry window\" >&2\n\
         exit {MOUNT_FAILURE}\n\
         fi\n\
         echo \"{content}\" | sudo tee {MOUNT_POINT}/{file}\n\
         echo CONTENT:$(cat {MOUNT_POINT}/{file})\n\
         echo FSTYPE:$(findmnt -no FSTYPE {MOUNT_POINT})\n\
         echo MAGIC:$(stat -f -c %T {MOUNT_POINT})\n\
         sudo umount {MOUNT_POINT}\n"
    )
}

/// The script a client READS a file of the file system with: mount the address, write what the
/// file holds, and take the mount away again.
fn read_back_script(address: &str, file: &str) -> String {
    format!(
        "set -e\n\
         sudo mkdir -p {MOUNT_POINT}\n\
         sudo mount -t nfs4 -o {MOUNT_OPTIONS} {address}:/ {MOUNT_POINT}\n\
         echo SEEN:$(cat {MOUNT_POINT}/{file})\n\
         sudo umount {MOUNT_POINT}\n"
    )
}

/// One mount target as the control plane reports it once it is `available`: its id and the
/// address that same read reports, read back through `DescribeMountTargets` — the documented
/// way a client learns both.
#[derive(Debug)]
struct ServingMountTarget {
    mount_target_id: String,
    address: String,
    /// The instant the control plane reported it `available`, which is the instant the address
    /// the client was handed has to serve at.
    announced_at: SystemTime,
}

/// Create one mount target of `file_system_id` in `subnet` and look again through
/// `DescribeMountTargets` until it reports `available`, answering what that read reported.
async fn serving_mount_target(
    emulator: &Emulator,
    file_system_id: &str,
    subnet: &SubnetFacts,
) -> ServingMountTarget {
    let created = emulator
        .create_mount_target(json!({
            "FileSystemId": file_system_id,
            "SubnetId": subnet.subnet_id,
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "CreateMountTarget in {} must be served, got {} {:?}",
        subnet.subnet_id, created.status, created.body
    );
    let mount_target_id = created.string("MountTargetId");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emulator
            .describe_mount_targets(&format!("MountTargetId={mount_target_id}"))
            .await;
        let reported = &described.get("MountTargets")[0];
        if reported["LifeCycleState"].as_str() == Some("available") {
            let address = reported["IpAddress"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                subnet.holds(&address),
                "the address {mount_target_id} reports must be one of subnet {}'s own, got \
                 {address:?}",
                subnet.subnet_id
            );
            return ServingMountTarget {
                mount_target_id,
                address,
                // Read AFTER the state was reported, so nothing of what follows can be served
                // by an instant earlier than the announcement.
                announced_at: SystemTime::now(),
            };
        }
        assert!(
            Instant::now() < deadline,
            "mount target {mount_target_id} never reached available, last read {:?}",
            described.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A subnet of another VPC of the same account, minted by Amazon EC2 on the same engine: where
/// a client that has no network path to the mount target stands.
async fn subnet_of_another_vpc(emulator: &Emulator, zone: &str) -> SubnetFacts {
    let ec2 = emulator.ec2();
    let vpc = ec2
        .create_vpc()
        .cidr_block("10.93.0.0/16")
        .send()
        .await
        .expect("EC2 must create a VPC")
        .vpc
        .and_then(|vpc| vpc.vpc_id)
        .expect("the created VPC must report its id");
    let subnet = ec2
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block("10.93.4.0/24")
        .availability_zone(zone)
        .send()
        .await
        .expect("EC2 must create a subnet")
        .subnet
        .expect("the created subnet must be reported");
    SubnetFacts {
        subnet_id: subnet.subnet_id.clone().expect("a subnet id"),
        vpc_id: subnet.vpc_id.clone().expect("a VPC id"),
        zone_name: subnet.availability_zone.clone().expect("a zone name"),
        availability_zone_id: subnet.availability_zone_id.clone().expect("a zone id"),
        cidr_block: subnet.cidr_block.clone().expect("a CIDR block"),
    }
}

/// What a row expects of the script: a mount that was made and a round trip that completed, or
/// a retry window that was exhausted because nothing answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    RoundTrip,
    NothingAnswers,
}

/// One row of the table below: the case, the address the script mounts, the VPC the client
/// stands in, the file it writes through the mount, the bytes it writes, and what the script
/// must come to.
type RoundTripRow<'a> = (
    &'a str,
    &'a str,
    Option<&'a str>,
    &'a str,
    &'a str,
    Expected,
);

/// RULE: a mount target the control plane reports `available` serves the file system at the
/// address that read reports, to a client of its own VPC, from that instant on — so the
/// script's first attempt mounts it, what it writes through the mount it reads back byte for
/// byte, and both probes report the network filesystem. Nothing answers at an address no such
/// mount target of the client's VPC holds.
#[tokio::test]
async fn an_available_mount_target_completes_the_scripted_round_trip() {
    let emulator = Emulator::new();
    let subnet = emulator.account_subnet(0).await;
    let elsewhere = subnet_of_another_vpc(&emulator, &subnet.zone_name).await;
    let file_system = emulator.available_file_system("scripted-round-trip").await;
    let serving = serving_mount_target(&emulator, &file_system, &subnet).await;
    // An address of the mount target's own subnet that no mount target answers at.
    let unanswered = subnet.address_at(203);

    // (case, the address mounted, the VPC the client stands in, the file, the bytes, what the
    // script must come to)
    let rows: Vec<RoundTripRow<'_>> = vec![
        (
            "a plain marker",
            serving.address.as_str(),
            Some(subnet.vpc_id.as_str()),
            "plain.txt",
            "sim2real-efs-round-trip",
            Expected::RoundTrip,
        ),
        (
            "bytes carrying punctuation",
            serving.address.as_str(),
            Some(subnet.vpc_id.as_str()),
            "punctuated.txt",
            "marker-7.3_ok+done=yes",
            Expected::RoundTrip,
        ),
        (
            "bytes carrying spaces",
            serving.address.as_str(),
            Some(subnet.vpc_id.as_str()),
            "spaced.txt",
            "the quick brown fox wrote this line",
            Expected::RoundTrip,
        ),
        (
            "a long line",
            serving.address.as_str(),
            Some(subnet.vpc_id.as_str()),
            "long.txt",
            LONG_LINE,
            Expected::RoundTrip,
        ),
        (
            "an address of the subnet nothing answers at",
            unanswered.as_str(),
            Some(subnet.vpc_id.as_str()),
            "unanswered.txt",
            "never-written",
            Expected::NothingAnswers,
        ),
        (
            "the serving address, from a client in another VPC",
            serving.address.as_str(),
            Some(elsewhere.vpc_id.as_str()),
            "elsewhere.txt",
            "never-written",
            Expected::NothingAnswers,
        ),
        (
            "the serving address, from a client in no VPC at all",
            serving.address.as_str(),
            None,
            "nowhere.txt",
            "never-written",
            Expected::NothingAnswers,
        ),
    ];

    for (case, address, vpc_id, file, content, expected) in rows {
        let programs = guest_programs(&emulator, serving.announced_at, vpc_id);
        let run = run_in_guest(programs, &round_trip_script(address, file, content));
        match expected {
            Expected::RoundTrip => {
                assert_eq!(
                    run.status, SUCCESS,
                    "case {case:?}: expected the script to succeed, got {run:?}"
                );
                // The mount was made at the FIRST attempt: the control plane had already
                // announced the address, so no retry was needed.
                assert!(
                    run.wrote("ATTEMPTS:1"),
                    "case {case:?}: expected the first attempt to mount {address}, got {run:?}"
                );
                assert!(
                    run.wrote(&format!("CONTENT:{content}")),
                    "case {case:?}: expected the bytes written through the mount to be read \
                     back, got {run:?}"
                );
                assert!(
                    run.wrote(&format!("FSTYPE:{MOUNT_FSTYPE}")),
                    "case {case:?}: expected the mount's own type, got {run:?}"
                );
                assert!(
                    run.wrote(&format!("MAGIC:{STATFS_TYPE}")),
                    "case {case:?}: expected the type a statfs of the mount reports, got {run:?}"
                );
                assert!(
                    run.errors.is_empty(),
                    "case {case:?}: expected the script to report nothing it could not do, got \
                     {:?}",
                    run.errors
                );
            }
            Expected::NothingAnswers => {
                assert_eq!(
                    run.status, MOUNT_FAILURE,
                    "case {case:?}: expected the script to end on the status mount fails with, \
                     got {run:?}"
                );
                assert_eq!(
                    run.error_lines_naming("mount attempt"),
                    MOUNT_ATTEMPTS as usize,
                    "case {case:?}: expected every attempt of the retry window to be reported, \
                     got {:?}",
                    run.errors
                );
                assert!(
                    run.mentions("mount never succeeded within the retry window"),
                    "case {case:?}: expected the exhausted window to be reported, got {run:?}"
                );
                for probe in ["CONTENT:", "FSTYPE:", "MAGIC:", "ATTEMPTS:"] {
                    assert!(
                        !run.mentions(probe),
                        "case {case:?}: expected no {probe:?} value when nothing answered, got \
                         {run:?}"
                    );
                }
            }
        }
    }

    // The mount target is still the one the control plane reported, and still `available`: the
    // round trips above neither consumed it nor moved it.
    let described = emulator
        .describe_mount_targets(&format!("MountTargetId={}", serving.mount_target_id))
        .await;
    assert_eq!(
        described.get("MountTargets")[0]["LifeCycleState"].as_str(),
        Some("available"),
        "the mount target must still be available after the round trips, got {:?}",
        described.body
    );
    assert_eq!(
        described.get("MountTargets")[0]["IpAddress"].as_str(),
        Some(serving.address.as_str()),
        "the mount target must still report the address the client mounted, got {:?}",
        described.body
    );
}

/// A line far longer than a marker, written through the mount in one of the rows above.
const LONG_LINE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\
                         0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\
                         0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// RULE: what the script wrote through the mount is the FILE SYSTEM's, not the machine's — so
/// another machine of the same VPC, mounting the same file system through another of its mount
/// targets, reads those bytes back.
#[tokio::test]
async fn a_second_machine_reads_what_the_first_wrote_through_the_mount() {
    let emulator = Emulator::new();
    let subnets = emulator.account_subnets().await;
    let [first, second] = match subnets.as_slice() {
        [first, second, ..] => [first.clone(), second.clone()],
        other => panic!("the account must have at least two subnets, got {other:?}"),
    };
    let file_system = emulator.available_file_system("scripted-handover").await;
    let writer = serving_mount_target(&emulator, &file_system, &first).await;
    let reader = serving_mount_target(&emulator, &file_system, &second).await;
    let content = "written-through-the-first-mount";
    let file = "handover.txt";

    let wrote = run_in_guest(
        guest_programs(&emulator, writer.announced_at, Some(&first.vpc_id)),
        &round_trip_script(&writer.address, file, content),
    );
    assert_eq!(
        wrote.status, SUCCESS,
        "the writing script must succeed, got {wrote:?}"
    );
    assert!(
        wrote.wrote(&format!("CONTENT:{content}")),
        "the writing script must read its own bytes back, got {wrote:?}"
    );

    // Another machine, another mount target of the same file system, and the bytes are there —
    // through a mount made after the first machine took its own away.
    let read = run_in_guest(
        guest_programs(&emulator, reader.announced_at, Some(&second.vpc_id)),
        &read_back_script(&reader.address, file),
    );
    assert_eq!(
        read.status, SUCCESS,
        "the reading script must succeed, got {read:?}"
    );
    assert!(
        read.wrote(&format!("SEEN:{content}")),
        "the second machine must read what the first wrote, got {read:?}"
    );

    // A machine that mounted nothing has no such path at all: the bytes are the file system's.
    let without = run_in_guest(
        guest_programs(&emulator, reader.announced_at, Some(&second.vpc_id)),
        &format!("cat {MOUNT_POINT}/{file}\n"),
    );
    assert_ne!(
        without.status, SUCCESS,
        "a machine that mounted nothing must hold no file of the file system, got {without:?}"
    );
    assert!(
        !without.mentions(content),
        "a machine that mounted nothing must read none of the file system's bytes, got \
         {without:?}"
    );
}
