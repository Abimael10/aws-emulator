// SPDX-License-Identifier: CC0-1.0

//! What an instance's GUEST sees of its own BLOCK DEVICES, driven over the wire the service
//! actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness — and
//! observed where a client observes anything a guest wrote: that instance's CONSOLE ("the
//! instance console output displays the exact console output that would normally be displayed
//! on a physical monitor attached to a computer", `GetConsoleOutput`).
//!
//! The rule under test: **a volume attached to an instance is a BLOCK DEVICE of its guest,
//! whose size is that volume's size** — "after you attach an Amazon EBS volume to your
//! instance, it is exposed as a block device", and the documented way to find it is `lsblk`
//! inside the guest. So the guest of an instance lists one device per volume attached to it —
//! its root volume included, which is the device its root filesystem is mounted from — each
//! under the name the instance type's own device model gives it ("EBS volumes are exposed as
//! NVMe block devices on instances built on the Nitro System", where the name the request asked
//! for is not the name the guest sees, while a pre-Nitro instance sees `/dev/sd*` renamed to
//! `/dev/xvd*`), and the kernel publishes each one's size in 512-byte sectors under
//! `/sys/block`.
//!
//! The rows differ along what the rule generalises over: whether the instance has anything
//! attached beside its root volume, how many volumes are attached, how large each of them is,
//! which device names the attachments ask for, whether they were attached in the order those
//! names sort in, and whether the instance type presents its volumes as NVMe devices or as Xen
//! ones — so no row is the specification.
//!
//! The guest's own program is performed by the one bounded shell session this workspace runs
//! caller-supplied shell text in, and it opens with `set -eu`: the options a script states are
//! part of what a guest's program needs, so a program whose first line is not honoured is one
//! no client's boot script survives.

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, TRANSITION_DELAY};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, member};
use super::volume_lifecycle::{past_the_transition, p, zone_name, REGION};

/// The markers the guest's program writes the `lsblk` listings between, so what the program
/// wrote is read off the console without reading the lines the kernel and the init system
/// wrote around it.
const DISKS_BEGIN: &str = "GUEST-DISKS-BEGIN";
const DISKS_END: &str = "GUEST-DISKS-END";
const TABLE_BEGIN: &str = "GUEST-TABLE-BEGIN";
const TABLE_END: &str = "GUEST-TABLE-END";

/// The marker one line carrying what the KERNEL publishes about one device is written under.
const SECTORS_MARKER: &str = "GUEST-SECTORS";

/// The bytes of one sector, which is the unit `/sys/block/<device>/size` is published in.
const SECTOR_BYTES: i64 = 512;

/// The program the launch of `nodes` carries as its user data: it lists the machine's block
/// devices the two ways a client's own script lists them — the byte sizes a script parses, and
/// the table an operator reads — and then reads what the kernel publishes about each device it
/// is to have.
///
/// Written as a cloud-config `bootcmd`, so EVERY boot of the guest runs it: that is what makes
/// the devices of a machine that has just had a volume attached observable.
fn listing_program(nodes: &[&str]) -> String {
    let mut program = String::from(
        "#cloud-config\n\
         bootcmd:\n\
         \x20 - |\n\
         \x20   set -eu\n",
    );
    for line in [
        format!("echo {DISKS_BEGIN} > /dev/console"),
        "lsblk -b -d -n -o NAME,SIZE > /dev/console".to_string(),
        format!("echo {DISKS_END} > /dev/console"),
        format!("echo {TABLE_BEGIN} > /dev/console"),
        "lsblk > /dev/console".to_string(),
        format!("echo {TABLE_END} > /dev/console"),
    ] {
        program.push_str(&format!("\x20   {line}\n"));
    }
    for node in nodes {
        // A device the machine has not got yet — the boot before the volume was attached —
        // publishes nothing, and asking about it is a question rather than a failure of the
        // program.
        program.push_str(&format!(
            "\x20   if test -f /sys/block/{node}/size; then echo \"{SECTORS_MARKER} {node} \
             $(cat /sys/block/{node}/size)\" > /dev/console; fi\n"
        ));
    }
    program
}

/// One emulator over its own engine, with one AMI of the calling account seeded the way a
/// register operation would store it — answered beside its image id.
fn emulator() -> (MultiServiceRouter, String) {
    let (router, engine) = super::ec2_query::emulator();
    let image_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard.insert_image(ImageSpec {
            name: "guest-block-devices".to_string(),
            ..ImageSpec::default()
        })
    };
    (router, image_id)
}

/// One `action` call signed for the Region the suite works in.
async fn op(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// Launch one instance of `instance_type` carrying `program` as its user data, and answer its
/// id.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    instance_type: &str,
    program: &str,
) -> String {
    let (status, body) = op(
        router,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("InstanceType", instance_type),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p(
                "UserData",
                base64::engine::general_purpose::STANDARD.encode(program),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status}: {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// One instance's console as text — empty for an instance whose guest has written none yet.
async fn console(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = op(
        router,
        "GetConsoleOutput",
        &[p("InstanceId", instance_id), p("Latest", "true")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetConsoleOutput must succeed, got {status}: {body}"
    );
    let Some(encoded) = member(&body, "output") else {
        return String::new();
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: output must be base64 ({error})"));
    String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("case {case:?}: the console text must be UTF-8: {error}"))
}

/// The lines each run of the program wrote between `begin` and `end`, one entry per run, in
/// the order the console carries them.
fn blocks(text: &str, begin: &str, end: &str) -> Vec<Vec<String>> {
    let mut written: Vec<Vec<String>> = Vec::new();
    let mut open: Option<Vec<String>> = None;
    for line in text.lines() {
        if line.trim() == begin {
            open = Some(Vec::new());
            continue;
        }
        if line.trim() == end {
            if let Some(block) = open.take() {
                written.push(block);
            }
            continue;
        }
        if let Some(block) = open.as_mut() {
            block.push(line.to_string());
        }
    }
    written
}

/// Wait until the guest has written `count` runs of its device listing to the console, and
/// answer the console text.
///
/// A boot is work the CLOCK owes the instance, so the console is looked at AGAIN in a loop
/// until the guest's own lines are on it or a generous deadline passes — nothing here depends
/// on how fast the host runs.
async fn console_with_listings(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    count: usize,
) -> String {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let text = console(router, case, instance_id).await;
        if blocks(&text, DISKS_BEGIN, DISKS_END).len() >= count {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the guest of {instance_id} must write {count} device listing(s) to \
             its console, got {text:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    }
}

/// The volume the control plane reports as `instance_id`'s ROOT volume, beside its size in
/// GiB — read off the control plane rather than written into this suite, so the size the guest
/// reports is compared with the size the service holds.
async fn root_volume(router: &MultiServiceRouter, case: &str, instance_id: &str) -> (String, i64) {
    let (status, body) = op(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must succeed, got {status}: {body}"
    );
    let volume_id = element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: the instance must report a root volume: {body}"));
    (volume_id.clone(), volume_size(router, case, &volume_id).await)
}

/// The size in GiB `DescribeVolumes` reports for `volume_id`.
async fn volume_size(router: &MultiServiceRouter, case: &str, volume_id: &str) -> i64 {
    let (status, body) = op(router, "DescribeVolumes", &[p("VolumeId.1", volume_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeVolumes must succeed, got {status}: {body}"
    );
    element(&body, "size")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: the volume must report a size: {body}"))
}

/// Create one volume of `size` GiB in the zone the launch places its instance in, attach it to
/// `instance_id` at `device`, and answer its id — past the spans the clock owes the volume's
/// `creating` -> `available` and the attachment's `attaching` -> `attached` moves, since a
/// device the guest can see is one whose attachment has arrived.
async fn attached_volume(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    device: &str,
    size: i64,
) -> String {
    let (status, body) = op(
        router,
        "CreateVolume",
        &[p("AvailabilityZone", zone_name(0)), p("Size", size.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVolume must succeed, got {status}: {body}"
    );
    let volume_id = element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume must report a volume: {body}"));
    past_the_transition().await;
    let (status, body) = op(
        router,
        "AttachVolume",
        &[
            p("VolumeId", &volume_id),
            p("InstanceId", instance_id),
            p("Device", device),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AttachVolume must succeed, got {status}: {body}"
    );
    past_the_transition().await;
    volume_id
}

/// One row: the instance type, the volumes the row attaches as `(device, size in GiB)`, and
/// the device names the guest is then to report, the root volume's first.
struct Row {
    case: &'static str,
    instance_type: &'static str,
    attached: &'static [(&'static str, i64)],
    nodes: &'static [&'static str],
}

/// THE RULE, over the table.
#[tokio::test]
async fn a_guest_sees_one_block_device_per_volume_attached_to_its_instance() {
    let rows = [
        Row {
            case: "an NVMe instance with nothing but its root volume",
            instance_type: "m5.large",
            attached: &[],
            nodes: &["nvme0n1"],
        },
        Row {
            case: "an NVMe instance with one volume attached",
            instance_type: "m5.large",
            attached: &[("/dev/sdf", 1)],
            nodes: &["nvme0n1", "nvme1n1"],
        },
        Row {
            case: "an NVMe instance with two volumes of different sizes",
            instance_type: "m5.xlarge",
            attached: &[("/dev/sdf", 4), ("/dev/xvdg", 16)],
            nodes: &["nvme0n1", "nvme1n1", "nvme2n1"],
        },
        Row {
            // A pre-Nitro type: its guest sees the names the attachments asked for, with
            // `sd` renamed to `xvd`, rather than NVMe namespaces.
            case: "a Xen instance with one volume attached",
            instance_type: "t2.micro",
            attached: &[("/dev/sdf", 2)],
            nodes: &["xvda", "xvdf"],
        },
        Row {
            // The NVMe names follow the order the volumes were ATTACHED, not the order their
            // device names sort in: this row attaches the LATER name first, so a listing that
            // ordered them by name would give each volume the other one's node.
            case: "an NVMe instance whose volumes were attached out of name order",
            instance_type: "m5.xlarge",
            attached: &[("/dev/sdz", 1), ("/dev/sdf", 6)],
            nodes: &["nvme0n1", "nvme1n1", "nvme2n1"],
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, image_id) = emulator();
        let instance_id = launch(
            &router,
            case,
            &image_id,
            row.instance_type,
            &listing_program(row.nodes),
        )
        .await;
        // The first boot: the machine holds its root volume and nothing else, whatever the row
        // goes on to attach. The root volume and its size are read off the control plane once
        // that boot has happened, the instance being `pending` until then.
        let text = console_with_listings(&router, case, &instance_id, 1).await;
        let (_root_volume, root_size) = root_volume(&router, case, &instance_id).await;
        let listed = blocks(&text, DISKS_BEGIN, DISKS_END);
        let first = listed
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the guest must list its devices: {text:?}"));
        assert_eq!(
            first,
            &vec![format!(
                "{} {}",
                row.nodes[0],
                root_size * 1024 * 1024 * 1024
            )],
            "case {case:?}: before anything is attached the guest holds its root volume alone, \
             got {first:?}"
        );

        // Every volume the row attaches, each with its own size.
        let mut expected: Vec<(String, i64)> = vec![(row.nodes[0].to_string(), root_size)];
        for (index, (device, size)) in row.attached.iter().enumerate() {
            let volume_id = attached_volume(&router, case, &instance_id, device, *size).await;
            let held = volume_size(&router, case, &volume_id).await;
            assert_eq!(
                held, *size,
                "case {case:?}: the volume attached at {device} must hold {size} GiB, got {held}"
            );
            expected.push((row.nodes[index + 1].to_string(), *size));
        }

        if row.attached.is_empty() {
            continue;
        }

        // The guest reads its devices again when its operating system comes up again, which is
        // where the volumes attached since the first boot are observable.
        let (status, body) = op(&router, "RebootInstances", &[p("InstanceId.1", &instance_id)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: RebootInstances must succeed, got {status}: {body}"
        );
        let text = console_with_listings(&router, case, &instance_id, 2).await;
        let listed = blocks(&text, DISKS_BEGIN, DISKS_END);
        let latest = listed
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: the guest must list its devices: {text:?}"));
        let written: Vec<(String, i64)> = latest
            .iter()
            .filter_map(|line| {
                let (node, size) = line.split_once(' ')?;
                Some((node.trim().to_string(), size.trim().parse::<i64>().ok()?))
            })
            .collect();
        let as_bytes: Vec<(String, i64)> = expected
            .iter()
            .map(|(node, size)| (node.clone(), size * 1024 * 1024 * 1024))
            .collect();
        assert_eq!(
            written, as_bytes,
            "case {case:?}: the guest must list every volume attached to it, got {latest:?}"
        );

        // What the KERNEL publishes about each of those devices: the same size, in the sectors
        // it is published in.
        for (node, size) in &as_bytes {
            let marked = text
                .lines()
                .filter_map(|line| line.split_once(&format!("{SECTORS_MARKER} {node} ")))
                .map(|(_, value)| value.trim().to_string())
                .next_back();
            assert_eq!(
                marked.as_deref(),
                Some((size / SECTOR_BYTES).to_string().as_str()),
                "case {case:?}: the kernel must publish {} sectors for {node}, got {marked:?}",
                size / SECTOR_BYTES
            );
        }

        // The table an operator reads carries the same devices, each as a whole disk, with the
        // root filesystem's own mount point on the device that carries it.
        let table = blocks(&text, TABLE_BEGIN, TABLE_END);
        let latest = table
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: the guest must write a table: {text:?}"));
        let heading = latest
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the table must carry a heading: {latest:?}"));
        for column in ["NAME", "MAJ:MIN", "RM", "SIZE", "RO", "TYPE", "MOUNTPOINTS"] {
            assert!(
                heading.contains(column),
                "case {case:?}: the table's heading must carry {column}, got {heading:?}"
            );
        }
        assert_eq!(
            latest.len(),
            expected.len() + 1,
            "case {case:?}: the table must carry one row per device beside its heading, got \
             {latest:?}"
        );
        for (index, (node, size)) in expected.iter().enumerate() {
            let row = latest
                .get(index + 1)
                .unwrap_or_else(|| panic!("case {case:?}: the table must carry {node}: {latest:?}"));
            let words: Vec<&str> = row.split_whitespace().collect();
            assert_eq!(
                words.first().copied(),
                Some(node.as_str()),
                "case {case:?}: the row must name {node}, got {row:?}"
            );
            assert!(
                words.contains(&format!("{size}G").as_str()) && words.contains(&"disk"),
                "case {case:?}: the row for {node} must report {size}G and a whole disk, got \
                 {row:?}"
            );
            let mounted = words.last().copied() == Some("/");
            assert_eq!(
                mounted,
                index == 0,
                "case {case:?}: only the root volume's device is mounted at /, got {row:?}"
            );
        }
    }
}

/// RULE: a device the machine has not got is not one its guest reports — an `lsblk` naming it
/// answers the status the utility answers for a device it did not find, and writes no row.
#[tokio::test]
async fn a_device_the_machine_has_not_got_is_not_listed() {
    let case = "a device no volume of this instance is attached at";
    let (router, image_id) = emulator();
    // The program asks about a device the machine cannot have — the second NVMe namespace of
    // an instance with nothing attached — and says so when `lsblk` did not find it.
    let program = "#cloud-config\n\
         bootcmd:\n\
         \x20 - |\n\
         \x20   lsblk -b -d -n -o SIZE /dev/nvme1n1 > /dev/console || echo GUEST-MISSING > /dev/console\n\
         \x20   lsblk -b -d -n -o SIZE /dev/nvme0n1 > /dev/console\n";
    let instance_id = launch(&router, case, &image_id, "m5.large", program).await;
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    let text = loop {
        let text = console(&router, case, &instance_id).await;
        if text.contains("GUEST-MISSING") {
            break text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the guest must report what lsblk answered, got {text:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    };
    assert!(
        text.lines().any(|line| line.trim() == "GUEST-MISSING"),
        "case {case:?}: lsblk must answer a failure for a device it did not find, got {text:?}"
    );
    let (_volume, root_size) = root_volume(&router, case, &instance_id).await;
    let bytes = root_size * 1024 * 1024 * 1024;
    assert!(
        text.lines().any(|line| line.trim() == bytes.to_string()),
        "case {case:?}: the device the machine HAS must still be listed as {bytes} bytes, got \
         {text:?}"
    );
}

/// RULE: the devices a guest sees follow the ATTACHMENTS the control plane holds, so the two
/// cannot disagree — every volume `DescribeInstances` reports as attached to the instance is a
/// device of its guest, under its own size.
#[tokio::test]
async fn the_devices_a_guest_sees_are_the_attachments_the_control_plane_reports() {
    let case = "two volumes attached at the device names a client chose";
    let nodes = ["nvme0n1", "nvme1n1", "nvme2n1"];
    let (router, image_id) = emulator();
    let instance_id = launch(
        &router,
        case,
        &image_id,
        "m5.large",
        &listing_program(&nodes),
    )
    .await;
    console_with_listings(&router, case, &instance_id, 1).await;
    for (device, size) in [("/dev/sdh", 3), ("/dev/sdi", 5)] {
        attached_volume(&router, case, &instance_id, device, size).await;
    }
    let (status, body) = op(
        &router,
        "DescribeInstances",
        &[p("InstanceId.1", &instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must succeed, got {status}: {body}"
    );
    // Every volume the control plane reports for the instance, with the size it holds.
    let mut reported: Vec<i64> = Vec::new();
    for volume_id in elements(&body, "volumeId") {
        reported.push(volume_size(&router, case, &volume_id).await);
    }
    reported.sort_unstable();
    let (status, body) = op(
        &router,
        "RebootInstances",
        &[p("InstanceId.1", &instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RebootInstances must succeed, got {status}: {body}"
    );
    let text = console_with_listings(&router, case, &instance_id, 2).await;
    let listed = blocks(&text, DISKS_BEGIN, DISKS_END);
    let latest = listed
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: the guest must list its devices: {text:?}"));
    let mut seen: Vec<i64> = latest
        .iter()
        .filter_map(|line| {
            let (_node, size) = line.split_once(' ')?;
            Some(size.trim().parse::<i64>().ok()? / (1024 * 1024 * 1024))
        })
        .collect();
    seen.sort_unstable();
    assert_eq!(
        seen, reported,
        "case {case:?}: the guest's devices must be the attachments the control plane reports \
         ({reported:?}), got {latest:?}"
    );
}
