// SPDX-License-Identifier: CC0-1.0

//! The METERED SIZE a file system reports.
//!
//! RULE under test: `FileSystemSize$Value` is "the latest known metered size (in bytes) of data
//! stored in the file system", so the size a file system reports is the bytes of the data it
//! holds: a file system nothing has written to reports none, and a write, a rewrite, a second
//! file and a removal each take it to the bytes the file system then holds, with
//! `FileSystemSize$Timestamp` — "the time at which the size of data, returned in the `Value`
//! field, was determined" — carried once a metering has determined one. All of that data is in
//! the Standard storage class (`ValueInStandard`), and the other classes hold none.
//!
//! Member and doc source: `smithy_query(query="shape", shape="FileSystemSize")`.
//!
//! Nothing here reaches into a state struct: the file system, its mount target and the subnet it
//! is placed in are created through the front door, the DATA is written through the NFS client
//! programs this service hands the component that reaches into a machine — the same seam
//! `all/nfs_data_plane.rs` drives, which is the only way a client stores anything in a file
//! system — and the size is read back through `DescribeFileSystems`' own route.
//!
//! Nothing here sleeps for a lifecycle span and nothing asserts on elapsed time: the exports the
//! client is offered are read AT an instant this suite names, and the one wait it makes is the
//! harness's poll for the file system to become `available`, which polls the CONDITION.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_efs_magnolioapiservice_v20150201::state;
use service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles};
use service_common::posix_shell_volume::{MountFacts, MountedTree, ShellVolume, VolumeLimits};

use crate::harness::Emulator;

/// Where the machine of this suite mounts the file system.
const MOUNT_POINT: &str = "/mnt/efs";

/// The status a program exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// How much of the machine's OWN tree a case may use. Far above what one writes, and nothing
/// written here lands on the machine's own storage anyway: every path below is under the mount.
const MACHINE_LIMITS: VolumeLimits = VolumeLimits {
    files: 64,
    bytes: 64 * 1024,
    path_bytes: 4096,
    directories: 64,
    device_lines: 0,
    device_line_bytes: 0,
};

/// The tree of ONE machine, as the client programs of a data plane reach it: the seam a guest's
/// own shell session hands them, over a tree of this machine's own.
#[derive(Debug)]
struct Machine {
    tree: ShellVolume,
}

impl Machine {
    /// A machine holding the directory the file system is mounted over, which is what a script
    /// makes with `mkdir -p` before it mounts anything there.
    fn new() -> Self {
        let mut tree = ShellVolume::new(MACHINE_LIMITS);
        assert!(
            tree.make_directories(MOUNT_POINT),
            "the machine must hold the directory the file system is mounted over"
        );
        Self { tree }
    }
}

impl ProgramFiles for Machine {
    fn read(&self, path: &str) -> Option<String> {
        self.tree.read(path)
    }

    fn write(&mut self, path: &str, contents: &str) -> bool {
        self.tree.write(path, contents, false)
    }

    fn remove(&mut self, path: &str) -> bool {
        self.tree.remove_file(path)
    }

    fn entries_in(&self, path: &str) -> Vec<String> {
        self.tree.entry_names(path)
    }

    fn make_directories(&mut self, path: &str) -> bool {
        self.tree.make_directories(path)
    }

    fn mount(&mut self, path: &str, tree: MountedTree) -> bool {
        self.tree.mount(path, tree)
    }

    fn unmount(&mut self, path: &str) -> bool {
        self.tree.unmount(path)
    }

    fn mounted_at(&self, path: &str) -> Option<MountFacts> {
        self.tree.mount_facts(path).cloned()
    }

    fn file_system_at(&self, path: &str) -> Option<MountFacts> {
        self.tree.file_system_facts(path).cloned()
    }
}

/// What ONE row of the table below does to the file system's data.
#[derive(Debug)]
enum Stored {
    /// Write `contents` at the path under the mount, replacing whatever is there.
    Write(&'static str, String),
    /// Remove the file at the path under the mount.
    Remove(&'static str),
}

/// The `SizeInBytes` the file system `file_system_id` reports, read through
/// `DescribeFileSystems`' own route.
async fn reported_size(emulator: &Emulator, file_system_id: &str, case: &str) -> Value {
    let read = emulator
        .describe_file_systems(&format!("FileSystemId={file_system_id}"))
        .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: DescribeFileSystems must be served, got {} {:?}",
        read.status, read.body
    );
    read.get("FileSystems")[0]["SizeInBytes"].clone()
}

/// RULE: the size a file system reports is the bytes of the DATA it holds, metered, with every
/// per-storage-class member reported beside it.
///
/// The rows differ along what the rule generalises over: how many bytes are stored, in how many
/// files, and whether the last change ADDED data (a first file, a longer rewrite of it, a second
/// file) or TOOK it away (one file removed, then the last of them) — down to a file system that
/// holds nothing again.
#[tokio::test]
async fn the_size_a_file_system_reports_is_the_data_it_holds() {
    let emulator = Emulator::new();
    let subnet = emulator.account_subnet(0).await;
    let file_system = emulator.available_file_system("metered-size").await;
    let created = emulator
        .create_mount_target(json!({
            "FileSystemId": file_system,
            "SubnetId": subnet.subnet_id,
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "CreateMountTarget must be served, got {} {:?}",
        created.status, created.body
    );
    let address = created.string("IpAddress");

    // A file system NOTHING has written to: the size is none and no metering has determined
    // one, so there is no `Timestamp` to report.
    let empty = reported_size(&emulator, &file_system, "nothing written").await;
    assert_eq!(
        empty,
        json!({ "Value": 0, "ValueInIA": 0, "ValueInStandard": 0, "ValueInArchive": 0 }),
        "case {:?}: expected a file system holding no data to report none, got {empty:?}",
        "nothing written"
    );

    // The client that stores the data: a machine of the mount target's own VPC, mounting the
    // file system at the instant the exports are read at — past the mount target's `creating`
    // span, which is what makes the mount deterministic however fast the host runs.
    let serving_at = SystemTime::now() + state::CREATING_FOR + Duration::from_millis(1);
    let client: Box<dyn EnvironmentPrograms> =
        emulator.nfs_client(serving_at, Some(&subnet.vpc_id));
    let mut machine = Machine::new();
    let mount = client.run(
        "mount",
        &[
            "-t".to_string(),
            state::NFS4_FILE_SYSTEM_TYPE.to_string(),
            format!("{address}:/"),
            MOUNT_POINT.to_string(),
        ],
        &mut machine,
    );
    assert_eq!(
        mount.map(|outcome| outcome.status),
        Some(SUCCESS),
        "the machine must mount the file system through its own mount target"
    );

    // The bytes each row stores, written out so the expected size is this suite's own arithmetic
    // over its own inputs rather than a second reading of the implementation.
    let payload = "a-payload-the-client-wrote\n".to_string();
    let longer = format!("{}\n", "x".repeat(200));
    let beside = "beside\n".to_string();

    // (case, what the row stores, the bytes the file system then holds)
    let rows = [
        (
            "one file of its own bytes",
            Stored::Write("payload", payload.clone()),
            payload.len(),
        ),
        (
            "the same file rewritten longer",
            Stored::Write("payload", longer.clone()),
            longer.len(),
        ),
        (
            "a second file beside it",
            Stored::Write("beside", beside.clone()),
            longer.len() + beside.len(),
        ),
        (
            "the second file removed",
            Stored::Remove("beside"),
            longer.len(),
        ),
        ("the last file removed", Stored::Remove("payload"), 0),
    ];

    let mut last_timestamp: Option<f64> = None;
    for (case, stored, expected) in rows {
        match &stored {
            Stored::Write(name, contents) => assert!(
                machine.write(&format!("{MOUNT_POINT}/{name}"), contents),
                "case {case:?}: the client must be able to write through its mount"
            ),
            Stored::Remove(name) => assert!(
                machine.remove(&format!("{MOUNT_POINT}/{name}")),
                "case {case:?}: the client must be able to remove through its mount"
            ),
        }
        let expected = i64::try_from(expected).expect("a case stores far fewer bytes than that");
        let size = reported_size(&emulator, &file_system, case).await;
        assert_eq!(
            size["Value"].as_i64(),
            Some(expected),
            "case {case:?}: expected the file system to report {expected} bytes of stored data, \
             got {size:?}"
        );
        // "The latest known metered size (in bytes) of data stored in the Standard storage
        // class": all of a file system's data is in that class here, and the other two hold
        // none.
        assert_eq!(
            size["ValueInStandard"].as_i64(),
            Some(expected),
            "case {case:?}: expected the Standard class to hold all {expected} bytes, got {size:?}"
        );
        assert_eq!(
            (size["ValueInIA"].as_i64(), size["ValueInArchive"].as_i64()),
            (Some(0), Some(0)),
            "case {case:?}: expected no data in the Infrequent Access and Archive classes, got \
             {size:?}"
        );
        // "The time at which the size of data, returned in the Value field, was determined. The
        // value is the integer number of seconds since 1970-01-01T00:00:00Z."
        let timestamp = size["Timestamp"].as_f64();
        assert!(
            timestamp.is_some_and(|seconds| seconds > 0.0),
            "case {case:?}: expected the metering that determined this size to report when, got \
             {size:?}"
        );
        assert!(
            last_timestamp.is_none_or(|earlier| timestamp.is_some_and(|now| now >= earlier)),
            "case {case:?}: expected the metering time not to go backwards from {last_timestamp:?}, \
             got {size:?}"
        );
        last_timestamp = timestamp;

        // A second read of an unchanged file system reports the same description: the size is a
        // measurement of the data, so reading it changes nothing a later caller sees.
        let again = reported_size(&emulator, &file_system, case).await;
        assert_eq!(
            again, size,
            "case {case:?}: expected a second read of the unchanged file system to report the \
             same size, got {again:?} after {size:?}"
        );
    }
}
