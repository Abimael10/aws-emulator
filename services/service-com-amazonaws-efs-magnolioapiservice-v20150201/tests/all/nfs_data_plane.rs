// SPDX-License-Identifier: CC0-1.0

//! The NATIVE data plane: what a client inside the VPC reaches when it MOUNTS a mount target
//! this service reports `available`.
//!
//! RULE under test: a mount target reported `available` SERVES the file system at the address
//! it reports, to a client in its own VPC and to no other, and what is reached through it is
//! the file system's ONE tree — so a file written through one client's mount is read through
//! every other client's mount of the same file system, and taking a mount away leaves both the
//! file system's files and the directory it was mounted over as they were. A mount target the
//! control plane does not report `available` serves nothing, and neither does an address no
//! mount target of the client's VPC answers at.
//!
//! The file system, the mount targets and the subnets they are placed in are created through
//! the front door — EFS's own restJson1 routes, with the VPCs and subnets minted by Amazon EC2
//! on the same engine — and the mount itself is performed the way it is performed in a machine:
//! through the client programs this service hands the component that reaches into one
//! (`state::nfs_client_programs`), over a tree of that machine's own
//! ([`::service_common::posix_shell_volume::ShellVolume`]). Nothing reaches into a state
//! struct.
//!
//! Nothing here sleeps for a lifecycle span and nothing asserts on elapsed time: the exports a
//! client is offered are read AT an instant this suite names, so the `creating` row passes the
//! instant just after the create and the serving rows pass one past the span — which is what
//! makes both deterministic however fast or loaded the host is. The one wait this suite makes
//! is the harness's own poll for the file system to become `available`, which polls the
//! CONDITION.
//!
//! The rows span what the rule generalises over: which spelling the mount options take, which
//! filesystem type the command names, whether the export is the file system's root or a
//! directory of it, which VPC the client stands in, whether the address answers at all, whether
//! the machine has the directory to mount over, and the lifecycle state of the mount target
//! itself.

use std::time::{Duration, SystemTime};

use serde_json::json;
use service_com_amazonaws_efs_magnolioapiservice_v20150201::state;
use service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles};
use service_common::posix_shell_volume::{MountFacts, MountedTree, ShellVolume, VolumeLimits};

use crate::harness::{Emulator, SubnetFacts};

/// The status a program exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status `mount` and `umount` exit with when the mount itself failed: "32 mount failure"
/// (`mount(8)`, EXIT STATUS).
const MOUNT_FAILURE: i32 = 32;

/// The status a shell answers for a command nothing resolved, which is what a mount this
/// program performs nothing of comes back as.
const COULD_NOT_RUN: i32 = 127;

/// The directory a machine of this suite mounts a file system at.
const MOUNT_POINT: &str = "/mnt/efs";

/// The options the documented Amazon EFS mount command carries.
const EFS_MOUNT_OPTIONS: &str = "nfsvers=4.1,rsize=1048576,wsize=1048576,hard,timeo=600,retrans=2";

/// How much of one machine's own tree this suite gives a case. Far above what a case writes.
const MACHINE_LIMITS: VolumeLimits = VolumeLimits {
    files: 64,
    bytes: 64 * 1024,
    path_bytes: 4096,
    directories: 64,
    device_lines: 0,
    device_line_bytes: 0,
};

/// The tree of ONE machine, as the client programs of a data plane reach it: the same seam the
/// shell session of a guest hands them ([`::service_common::posix_shell_session::ProgramFiles`]),
/// over a tree of this machine's own.
#[derive(Debug)]
struct Machine {
    tree: ShellVolume,
}

impl Machine {
    /// A machine holding the directory `MOUNT_POINT`, which is what a script makes with
    /// `mkdir -p` before it mounts anything there.
    fn new() -> Self {
        let mut tree = ShellVolume::new(MACHINE_LIMITS);
        assert!(
            tree.make_directories(MOUNT_POINT),
            "the machine must hold the directory a file system is mounted over"
        );
        Self { tree }
    }

    /// A machine holding no directory at all beyond its root.
    fn without_a_mount_point() -> Self {
        Self {
            tree: ShellVolume::new(MACHINE_LIMITS),
        }
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

/// Run one command line — the words as a shell would hand them over — through `client`, and
/// answer the status it exited with, or [`COULD_NOT_RUN`] for an invocation that client
/// performs nothing of.
fn run(client: &dyn EnvironmentPrograms, machine: &mut Machine, line: &str) -> i32 {
    let mut words = line.split_whitespace().map(str::to_string);
    let name = words.next().expect("a command line names a program");
    let arguments: Vec<String> = words.collect();
    client
        .run(&name, &arguments, machine)
        .map_or(COULD_NOT_RUN, |outcome| outcome.status)
}

/// One mount target of `file_system_id` in `subnet`, as the control plane minted it: its id and
/// the address it reports.
async fn mount_target(emulator: &Emulator, file_system_id: &str, subnet: &SubnetFacts) -> String {
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
    let address = created.string("IpAddress");
    assert!(
        subnet.holds(&address),
        "the mount target's address must be one of subnet {}'s own, got {address:?}",
        subnet.subnet_id
    );
    address
}

/// One subnet of a VPC of this account that is NOT the default one, minted by Amazon EC2 on the
/// same engine: the network a client standing somewhere ELSE is in.
async fn subnet_of_another_vpc(emulator: &Emulator, zone: &str) -> SubnetFacts {
    let ec2 = emulator.ec2();
    let vpc = ec2
        .create_vpc()
        .cidr_block("10.91.0.0/16")
        .send()
        .await
        .expect("EC2 must create a VPC")
        .vpc
        .and_then(|vpc| vpc.vpc_id)
        .expect("the created VPC must report its id");
    let subnet = ec2
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block("10.91.7.0/24")
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
        availability_zone_id: subnet
            .availability_zone_id
            .clone()
            .expect("a zone id"),
        cidr_block: subnet.cidr_block.clone().expect("a CIDR block"),
    }
}

/// RULE: an `available` mount target serves the file system at the address it reports, to a
/// client of its own VPC — and nothing else does.
#[tokio::test]
async fn an_available_mount_target_serves_the_file_system_at_its_own_address() {
    let emulator = Emulator::new();
    let subnet = emulator.account_subnet(0).await;
    let elsewhere = subnet_of_another_vpc(&emulator, &subnet.zone_name).await;
    let file_system = emulator.available_file_system("nfs-data-plane").await;
    let address = mount_target(&emulator, &file_system, &subnet).await;
    // The instant just after the create: the mount target is still `creating` at it, whatever
    // the host does next, because this is the instant the exports are read AT.
    let created_at = SystemTime::now();
    // READ FIRST, at that instant: a mount target the control plane still holds `creating`
    // serves nothing, and a transition the clock makes due later is not undone, so this is the
    // one row that has to be read before any instant past the span is.
    {
        let creating = emulator.nfs_client(created_at, Some(&subnet.vpc_id));
        let mut machine = Machine::new();
        let status = run(
            &*creating,
            &mut machine,
            &format!("mount -t nfs4 {address}:/ {MOUNT_POINT}"),
        );
        assert_eq!(
            status, MOUNT_FAILURE,
            "a mount target the control plane still holds creating must serve nothing, and the \
             mount exited {status}"
        );
        assert_eq!(
            machine.mounted_at(MOUNT_POINT),
            None,
            "nothing must be mounted when the mount failed"
        );
    }
    // A file system of its own for the other VPC's mount target: a file system takes at most
    // one mount target per Availability Zone, and what that row is about is the VPC.
    let elsewhere_file_system = emulator
        .available_file_system("nfs-data-plane-elsewhere")
        .await;
    let elsewhere_address = mount_target(&emulator, &elsewhere_file_system, &elsewhere).await;
    let serving_at = SystemTime::now() + state::CREATING_FOR + Duration::from_millis(1);

    // A directory of the file system, made through a mount of its root, so the row that mounts
    // a directory of the export mounts one the file system really holds.
    let client = emulator.nfs_client(serving_at, Some(&subnet.vpc_id));
    let mut machine = Machine::new();
    assert_eq!(
        run(
            &*client,
            &mut machine,
            &format!("mount -t nfs4 {address}:/ {MOUNT_POINT}")
        ),
        SUCCESS,
        "the mount that makes the directory must succeed"
    );
    assert!(
        machine.make_directories(&format!("{MOUNT_POINT}/team")),
        "a directory must be creatable through the mount"
    );
    assert_eq!(
        run(&*client, &mut machine, &format!("umount {MOUNT_POINT}")),
        SUCCESS,
        "the mount that made the directory must come away again"
    );

    // (case, the instant the exports are read at, the client's VPC, the command, the status)
    let rows: Vec<(&str, SystemTime, Option<&str>, String, i32)> = vec![
        (
            "the documented Amazon EFS mount command",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount -t nfs4 -o {EFS_MOUNT_OPTIONS} {address}:/ {MOUNT_POINT}"),
            SUCCESS,
        ),
        (
            "the options joined to their letters",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount -tnfs4 -onfsvers=4.1 {address}:/ {MOUNT_POINT}"),
            SUCCESS,
        ),
        (
            "the other spelling of the type the kernel serves",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount -t nfs {address}:/ {MOUNT_POINT}"),
            SUCCESS,
        ),
        (
            "no type at all, which the source itself names",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount {address}:/ {MOUNT_POINT}"),
            SUCCESS,
        ),
        (
            "a directory of the file system as the export",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount -t nfs4 {address}:/team {MOUNT_POINT}"),
            SUCCESS,
        ),
        (
            "a directory the file system does not hold",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount -t nfs4 {address}:/absent {MOUNT_POINT}"),
            MOUNT_FAILURE,
        ),
        (
            "an address of the subnet no mount target answers at",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!(
                "mount -t nfs4 {}:/ {MOUNT_POINT}",
                subnet.address_at(201)
            ),
            MOUNT_FAILURE,
        ),
        (
            "the mount target of another file system in another VPC",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount -t nfs4 {elsewhere_address}:/ {MOUNT_POINT}"),
            MOUNT_FAILURE,
        ),
        (
            "a client this emulator can place in no VPC at all",
            serving_at,
            None,
            format!("mount -t nfs4 {address}:/ {MOUNT_POINT}"),
            MOUNT_FAILURE,
        ),
        (
            "a filesystem type this data plane does not serve",
            serving_at,
            Some(subnet.vpc_id.as_str()),
            format!("mount -t ext4 {address}:/ {MOUNT_POINT}"),
            COULD_NOT_RUN,
        ),
    ];

    for (case, at, vpc_id, line, expected) in rows {
        let client = emulator.nfs_client(at, vpc_id);
        let mut machine = Machine::new();
        let status = run(&*client, &mut machine, &line);
        assert_eq!(
            status, expected,
            "case {case:?}: expected {line:?} to exit {expected}, got {status}"
        );
        let mounted = machine.mounted_at(MOUNT_POINT);
        assert_eq!(
            mounted.is_some(),
            expected == SUCCESS,
            "case {case:?}: expected the machine to hold a mount at {MOUNT_POINT} = {}, got \
             {mounted:?}",
            expected == SUCCESS
        );
        if let Some(facts) = mounted {
            assert_eq!(
                (
                    facts.file_system_type.as_str(),
                    facts.statfs_type.as_str()
                ),
                (state::NFS4_FILE_SYSTEM_TYPE, state::NFS_STATFS_TYPE),
                "case {case:?}: expected the mount to report the types an NFS mount reports, \
                 got {facts:?}"
            );
            assert!(
                line.contains(&facts.source),
                "case {case:?}: expected the mount to report what the command mounted, got \
                 {facts:?}"
            );
        }
    }

    // A machine with no directory to mount over: the one case whose machine differs.
    let mut bare = Machine::without_a_mount_point();
    let status = run(
        &*client,
        &mut bare,
        &format!("mount -t nfs4 {address}:/ {MOUNT_POINT}"),
    );
    assert_eq!(
        status, MOUNT_FAILURE,
        "a mount over a directory the machine does not have must fail, got {status}"
    );
}

/// RULE: what is reached through a mount is the file system's ONE tree, so what one client
/// writes through its own mount every other client of the same file system reads through
/// theirs — and taking a mount away leaves the file system's files with the file system.
#[tokio::test]
async fn two_clients_of_one_file_system_read_each_others_files() {
    let emulator = Emulator::new();
    let subnets = emulator.account_subnets().await;
    let [first, second] = match subnets.as_slice() {
        [first, second, ..] => [first.clone(), second.clone()],
        other => panic!("the account must have at least two subnets, got {other:?}"),
    };
    let file_system = emulator.available_file_system("nfs-coherence").await;
    let first_address = mount_target(&emulator, &file_system, &first).await;
    let second_address = mount_target(&emulator, &file_system, &second).await;
    let serving_at = SystemTime::now() + state::CREATING_FOR + Duration::from_millis(1);

    let client = emulator.nfs_client(serving_at, Some(&first.vpc_id));
    let mut node_a = Machine::new();
    let mut node_b = Machine::new();
    assert_eq!(
        run(
            &*client,
            &mut node_a,
            &format!("mount -t nfs4 -o {EFS_MOUNT_OPTIONS} {first_address}:/ {MOUNT_POINT}")
        ),
        SUCCESS,
        "node A must mount the file system through its own mount target"
    );
    assert_eq!(
        run(
            &*client,
            &mut node_b,
            &format!("mount -t nfs4 -o {EFS_MOUNT_OPTIONS} {second_address}:/ {MOUNT_POINT}")
        ),
        SUCCESS,
        "node B must mount the same file system through the other mount target"
    );

    // What one node writes, the other reads — through two mounts of one file system.
    let marker = "written-by-node-a";
    assert!(
        node_a.write(&format!("{MOUNT_POINT}/from-a"), marker),
        "node A must be able to write through its mount"
    );
    assert_eq!(
        node_b.read(&format!("{MOUNT_POINT}/from-a")).as_deref(),
        Some(marker),
        "node B must read what node A wrote through the shared file system, and read \
         {:?} instead",
        node_b.read(&format!("{MOUNT_POINT}/from-a"))
    );
    assert!(
        node_b.entries_in(MOUNT_POINT).iter().any(|name| name == "from-a"),
        "a listing of node B's mount must name what node A created, got {:?}",
        node_b.entries_in(MOUNT_POINT)
    );
    let answer = "written-by-node-b";
    assert!(
        node_b.write(&format!("{MOUNT_POINT}/from-b"), answer),
        "node B must be able to write through its mount"
    );
    assert_eq!(
        node_a.read(&format!("{MOUNT_POINT}/from-b")).as_deref(),
        Some(answer),
        "node A must read what node B wrote, and read {:?} instead",
        node_a.read(&format!("{MOUNT_POINT}/from-b"))
    );

    // Unmounting leaves the directory it was mounted over as it was, and the files with the
    // file system — which the other node still reads.
    assert_eq!(
        run(&*client, &mut node_a, &format!("umount {MOUNT_POINT}")),
        SUCCESS,
        "node A must be able to unmount"
    );
    assert_eq!(
        node_a.mounted_at(MOUNT_POINT),
        None,
        "nothing must be mounted at {MOUNT_POINT} on node A after the unmount"
    );
    assert_eq!(
        node_a.read(&format!("{MOUNT_POINT}/from-a")).as_deref(),
        None,
        "the file system's files must not be left behind on the machine, got {:?}",
        node_a.read(&format!("{MOUNT_POINT}/from-a"))
    );
    assert_eq!(
        node_b.read(&format!("{MOUNT_POINT}/from-a")).as_deref(),
        Some(marker),
        "node B must still read the file after node A unmounted"
    );
    // A second unmount of the same path has nothing to take away.
    assert_eq!(
        run(&*client, &mut node_a, &format!("umount {MOUNT_POINT}")),
        MOUNT_FAILURE,
        "unmounting a path nothing is mounted at must fail the way umount fails"
    );
    // And the mount can be made again, reaching the same files.
    assert_eq!(
        run(
            &*client,
            &mut node_a,
            &format!("mount -t nfs4 {first_address}:/ {MOUNT_POINT}")
        ),
        SUCCESS,
        "node A must be able to mount the file system again"
    );
    assert_eq!(
        node_a.read(&format!("{MOUNT_POINT}/from-b")).as_deref(),
        Some(answer),
        "the remounted file system must still hold what node B wrote"
    );
}
