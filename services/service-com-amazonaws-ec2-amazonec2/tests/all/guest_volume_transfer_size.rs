// SPDX-License-Identifier: CC0-1.0

//! How MANY bytes a copy inside a guest moves through an attached volume's device, driven over
//! the wire the service speaks (`ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness), through the one seam a program of a guest runs through (`run_in_instance_guest`,
//! which is what the agent inside a guest delivers a script to), and read back through Amazon
//! CloudWatch's own `GetMetricStatistics`.
//!
//! THE RULE: a copy through a guest block device moves every byte the invocation asked for
//! wherever the device has room for them, reports exactly the bytes it moved, and is SHORT — the
//! bytes it did take, a non-zero status and "No space left on device" — only where the device has
//! no room for the rest. "After you attach an Amazon EBS volume to your instance, it is exposed as
//! a block device" (EC2 User Guide, "Make an Amazon EBS volume available for use") and a block
//! device takes what is written to it until its end; a copy cut short of the bytes the device
//! could have taken makes a guest's own report of its I/O — and the `AWS/EBS` series published
//! from it — disagree with what the script asked for.
//!
//! The rows differ along what the rule generalises over: HOW MANY bytes one copy asks for, from a
//! single block up to everything a device of this emulator takes, and where in the device it is
//! written — the last row straddling the device's own end, which is the one place the copy is
//! short.
//!
//! Nothing here reads the metric store directly: Amazon CloudWatch is mounted BESIDE Amazon EC2 on
//! one engine, exactly as the assembled router mounts them.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::element;
use super::instance_metric_publication::{cloudwatch_ok, ec2_ok, emulator, p};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The instance type the rows launch: one built on the Nitro System, whose volumes the guest's
/// kernel presents as NVMe devices.
const NITRO_INSTANCE_TYPE: &str = "m5.large";

/// The image the launch names.
const AMI: &str = "ami-0123456789abcdef0";

/// The namespace a volume's own series are published under, and the one dimension they carry.
const NAMESPACE: &str = "AWS/EBS";
const VOLUME_ID_DIMENSION: &str = "VolumeId";

/// The stable link the kernel publishes for an EBS volume on a Nitro instance — spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make this suite pass.
const BY_ID_LINK: &str = "/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_";

/// The bytes of one mebibyte, which is the block every copy below moves its bytes in.
const MIB: usize = 1024 * 1024;

/// How many mebibytes of its guest's data ONE volume of this emulator holds, which is what a copy
/// through one of its devices may move at once — stated here as the figure this suite expects
/// rather than imported, for the same reason as the link above.
const MIB_A_VOLUME_HOLDS: usize = 64;

/// What `dd` reports when the destination cannot take the bytes, which is what the end of a device
/// answers.
const NO_SPACE: &str = "No space left on device";

/// How long the polls below keep asking before giving up: far above the span any move they wait
/// for is owed in, so a loaded host is not what decides a case.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How often they look again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The window the datapoints are read over, either side of the instant the I/O happened, and the
/// period they are read at.
const READ_WINDOW_SECS: i64 = 900;
const READ_PERIOD_SECS: i64 = 60;

/// The marker each value the guest's program reports is written under.
const DEVICE_MARKER: &str = "EBS-DEV";
const STATUS_MARKER: &str = "EBS-RC";
const BYTES_MARKER: &str = "EBS-BYTES";
const NO_SPACE_MARKER: &str = "EBS-NO-SPACE";

/// One row: a volume to attach, and the copy the guest performs through it.
struct Row {
    /// The case, named in every assertion.
    case: &'static str,
    /// The volume's size in GiB.
    size_gib: usize,
    /// The device the attachment asks for.
    device: &'static str,
    /// How many mebibyte blocks the copy asks for (`count=N` at `bs=1M`).
    asked_mib: usize,
    /// How many of those blocks into the device it writes (`seek=N`).
    seek_mib: usize,
    /// How many mebibytes the device has room for, which is what the copy must move and report.
    moved_mib: usize,
}

/// The rows: copies of four sizes, three of them with room for every byte they ask for — one of a
/// single block, one past the figure a copy of this emulator used to stop at, one of everything a
/// device takes — and one straddling the end of the device, which has room for a single block of
/// it.
const ROWS: &[Row] = &[
    Row {
        case: "one block, well inside the device",
        size_gib: 8,
        device: "/dev/sdf",
        asked_mib: 1,
        seek_mib: 0,
        moved_mib: 1,
    },
    Row {
        case: "thirty-three mebibytes, written one block in",
        size_gib: 8,
        device: "/dev/sdg",
        asked_mib: 33,
        seek_mib: 1,
        moved_mib: 33,
    },
    Row {
        case: "everything a device of this emulator takes",
        size_gib: 8,
        device: "/dev/sdh",
        asked_mib: MIB_A_VOLUME_HOLDS,
        seek_mib: 0,
        moved_mib: MIB_A_VOLUME_HOLDS,
    },
    Row {
        case: "a copy straddling the end of the device",
        size_gib: 1,
        device: "/dev/sdi",
        asked_mib: 4,
        // One block before the end of a one gibibyte device.
        seek_mib: 1023,
        moved_mib: 1,
    },
];

/// The serial the device carrying `volume_id` presents: the volume id without its `-`.
fn serial_of(volume_id: &str) -> String {
    volume_id.replace('-', "")
}

/// The value the guest wrote under `marker`, or `None` when it wrote the marker with nothing after
/// it.
fn marker(written: &[String], marker: &str) -> Option<String> {
    written
        .iter()
        .filter_map(|line| line.strip_prefix(&format!("{marker} ")))
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// Launch one Nitro instance and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let body = ec2_ok(
        router,
        case,
        "RunInstances",
        &[
            p("ImageId", AMI),
            p("InstanceType", NITRO_INSTANCE_TYPE),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ],
    )
    .await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// The Availability Zone the control plane placed `instance_id` in, which is the zone a volume
/// attached to it has to be created in.
async fn zone_of(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let body = ec2_ok(
        router,
        case,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    element(&body, "availabilityZone")
        .unwrap_or_else(|| panic!("case {case:?}: the instance must report its zone: {body}"))
}

/// Create the volume `row` describes in `zone` and attach it to `instance_id`, answering its id —
/// waiting for the CONDITION a device the guest can see rests on, which is the attachment the
/// control plane reports as `attached`.
async fn attached_volume(
    router: &MultiServiceRouter,
    row: &Row,
    zone: &str,
    instance_id: &str,
) -> String {
    let case = row.case;
    let body = ec2_ok(
        router,
        case,
        "CreateVolume",
        &[
            p("AvailabilityZone", zone),
            p("Size", row.size_gib.to_string()),
            p("VolumeType", "gp3"),
        ],
    )
    .await;
    let volume_id = element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume must report a volume: {body}"));
    // A volume reaches `available` a transition span after its create, so the attach is asked for
    // again until it is taken rather than after a wait of this suite's own choosing.
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = super::ec2_query::call(
            router,
            REGION,
            "AttachVolume",
            &[
                p("VolumeId", &volume_id),
                p("InstanceId", instance_id),
                p("Device", row.device),
            ],
        )
        .await;
        if status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: AttachVolume never succeeded for {volume_id}, last answer {status}: \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let body = ec2_ok(
            router,
            case,
            "DescribeVolumes",
            &[p("VolumeId.1", &volume_id)],
        )
        .await;
        if super::ec2_query::elements(&body, "status")
            .iter()
            .any(|state| state == "attached")
        {
            return volume_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the attachment of {volume_id} never reached `attached`: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the guest of `instance_id` can run a program at all.
async fn until_the_guest_is_up(engine: &engine::InMemoryEngine, case: &str, instance_id: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let up = {
            let state = ec2::state::service_state(engine).expect("the EC2 store");
            let mut store = state.lock();
            store.materialize_due_transitions();
            store.instance_guest_is_up(instance_id)
        };
        if up {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the guest of {instance_id} never came up"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `script` in that instance's guest and answer the lines it wrote.
fn in_guest(
    engine: &engine::InMemoryEngine,
    case: &str,
    instance_id: &str,
    script: &str,
) -> Vec<String> {
    let state = ec2::state::service_state(engine).expect("the EC2 store");
    let mut store = state.lock();
    let run = store
        .run_in_instance_guest(instance_id, &[script.to_string()])
        .unwrap_or_else(|| panic!("case {case:?}: the guest of {instance_id} must run a program"));
    assert_eq!(
        run.status, 0,
        "case {case:?}: expected the script to run, got status {} with errors {:?} and output \
         {:?}",
        run.status, run.errors, run.output
    );
    run.output
}

/// The program a client runs to copy `row`'s bytes through the device behind `volume_id`'s own
/// link: resolve the link, perform the copy, and report the status it exited with, the bytes its
/// own report states it moved, and whether it reported the end of the device.
fn transfer_program(volume_id: &str, row: &Row) -> String {
    let link = format!("{BY_ID_LINK}{}", serial_of(volume_id));
    let (asked, seek) = (row.asked_mib, row.seek_mib);
    format!(
        "set -u\n\
         DEV=$(readlink -f {link})\n\
         echo \"{DEVICE_MARKER} $DEV\"\n\
         REPORT=$(dd if=/dev/urandom of=$DEV bs=1M count={asked} seek={seek} oflag=direct \
         conv=fsync 2>&1)\n\
         echo \"{STATUS_MARKER} $?\"\n\
         echo \"{BYTES_MARKER} $(echo \"$REPORT\" | tail -1 | cut -d' ' -f1)\"\n\
         echo \"{NO_SPACE_MARKER} $(echo \"$REPORT\" | grep -c '{NO_SPACE}')\"\n"
    )
}

/// The sum of `metric` for `volume_id` over the window around the instant the I/O happened, and
/// zero for a series carrying no datapoint at all.
async fn volume_sum(router: &MultiServiceRouter, case: &str, volume_id: &str, metric: &str) -> f64 {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or_default();
    let answer = cloudwatch_ok(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": VOLUME_ID_DIMENSION, "Value": volume_id}],
            "StartTime": now - READ_WINDOW_SECS,
            "EndTime": now + READ_WINDOW_SECS,
            "Period": READ_PERIOD_SECS,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    answer["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| point["Sum"].as_f64().unwrap_or_default())
        .sum()
}

/// THE RULE, over the table.
#[tokio::test]
async fn a_copy_moves_every_byte_the_device_has_room_for() {
    let (router, engine) = emulator();
    let case = "a Nitro instance with volumes attached";
    let instance_id = launch(&router, case).await;
    until_the_guest_is_up(&engine, case, &instance_id).await;
    let zone = zone_of(&router, case, &instance_id).await;

    for row in ROWS {
        let case = row.case;
        let volume_id = attached_volume(&router, row, &zone, &instance_id).await;
        let written = in_guest(&engine, case, &instance_id, &transfer_program(&volume_id, row));

        // The link resolves to the device of this guest that carries the volume, so the copy below
        // was performed through that volume and no other.
        let device = marker(&written, DEVICE_MARKER).unwrap_or_default();
        assert!(
            device.starts_with("/dev/nvme"),
            "case {case:?}: expected the link to resolve to the NVMe device carrying {volume_id}, \
             got {device:?} of {written:?}"
        );

        // The copy REPORTS what it moved: every byte it asked for wherever the device had room,
        // and the bytes it did take where it had not.
        let moved = row.moved_mib * MIB;
        assert_eq!(
            marker(&written, BYTES_MARKER).as_deref(),
            Some(moved.to_string().as_str()),
            "case {case:?}: expected the copy to report the {moved} bytes the device had room \
             for of the {asked} it asked for, got {written:?}",
            asked = row.asked_mib * MIB
        );
        // ... and its STATUS, and the end-of-device diagnostic, follow from whether the device
        // had room for the whole of it: a copy the device took whole is no failure.
        let short = row.moved_mib < row.asked_mib;
        assert_eq!(
            marker(&written, STATUS_MARKER).as_deref(),
            Some(if short { "1" } else { "0" }),
            "case {case:?}: expected the copy to exit {expected} for a transfer of {moved} of \
             {asked} bytes, got {written:?}",
            expected = i32::from(short),
            asked = row.asked_mib * MIB
        );
        assert_eq!(
            marker(&written, NO_SPACE_MARKER).as_deref(),
            Some(if short { "1" } else { "0" }),
            "case {case:?}: expected {NO_SPACE:?} to be reported only for a copy the device had \
             no room for the rest of, got {written:?}"
        );

        // The bytes are the VOLUME's: the series it publishes is of the write that really
        // happened, so what the device took and what the copy reported are one fact. The
        // datapoints are published off the answer to a request, so they are asked for again until
        // they are there, never waited for.
        let bytes = u32::try_from(moved).map(f64::from).unwrap_or_default();
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let published = volume_sum(&router, case, &volume_id, "VolumeWriteBytes").await;
            if (published - bytes).abs() < f64::EPSILON {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {bytes} bytes written on {volume_id}, got {published}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        let ops = volume_sum(&router, case, &volume_id, "VolumeWriteOps").await;
        assert!(
            ops > 0.0,
            "case {case:?}: expected the operations that moved them to be published for \
             {volume_id}, got {ops}"
        );
    }
}
