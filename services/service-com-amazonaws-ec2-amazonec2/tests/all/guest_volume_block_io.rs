// SPDX-License-Identifier: CC0-1.0

//! What a guest can DO with a volume attached to its instance: address the device behind it,
//! write bytes through it, read them back, and have the `AWS/EBS` series of that volume report
//! the I/O — driven over the wire the service speaks (`ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness), through the one seam a program of a guest runs through
//! (`run_in_instance_guest`, which is what the agent inside a guest delivers a script to), and
//! read back through Amazon CloudWatch's own `GetMetricStatistics`.
//!
//! THE RULE: a volume whose attachment the control plane reports is a block device of that
//! instance's guest, addressable by the stable link the kernel publishes for it —
//! `/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_<serial>` on an instance built on the Nitro
//! System, whose serial is "the volume ID … in the serial number field" written without its `-`
//! (EC2 User Guide, "Amazon EBS and NVMe on Linux instances"). Bytes written through that device
//! land ON the volume, so reading the same range back answers them; and the operations that moved
//! them are published into `AWS/EBS` under exactly `{VolumeId}`, each measurement being of
//! operations something really performed — reads and writes on series of their own,
//! `VolumeConsumedReadWriteOps` for a Provisioned IOPS volume type only, and nothing at all for a
//! volume nothing read or wrote.
//!
//! The rows differ along what the rule generalises over: the volume TYPE (and so whether the
//! consumed-IOPS series exists at all), the volume's size, the payload size and the block size the
//! copy moves it in, and which of read and write the guest performed.
//!
//! Nothing here reads the metric store directly: Amazon CloudWatch is mounted BESIDE Amazon EC2 on
//! one engine, exactly as the assembled router mounts them.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::element;
use super::instance_metric_publication::{cloudwatch_ok, ec2_ok, emulator, p};

/// The instance type the rows launch: one built on the Nitro System, whose volumes the guest's
/// kernel presents as NVMe devices.
const NITRO_INSTANCE_TYPE: &str = "m5.large";

/// The image every launch names.
const AMI: &str = "ami-0123456789abcdef0";

/// The namespace a volume's own series are published under.
const NAMESPACE: &str = "AWS/EBS";

/// The one dimension they are dimensioned on.
const VOLUME_ID_DIMENSION: &str = "VolumeId";

/// The directory the kernel publishes a block device's stable link under, and what the link of an
/// EBS volume on a Nitro instance is named — spelled out here rather than imported, so a wrong
/// constant in the implementation cannot make this test pass.
const BY_ID_LINK: &str = "/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_";

/// The digest of NO bytes at all, which is what a read that answered nothing would come to.
const EMPTY_DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// How long the polls below keep asking before giving up: far above the span any move they wait
/// for is owed in, so a loaded host is not what decides a case.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How often they look again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The window the datapoints are read over, either side of the instant the I/O happened.
const READ_WINDOW_SECS: i64 = 900;

/// The period they are read at.
const READ_PERIOD_SECS: i64 = 60;

/// One row: a volume to attach and write through.
struct Row {
    /// The case, named in every assertion.
    case: &'static str,
    /// The `VolumeType` the volume is created with.
    volume_type: &'static str,
    /// The `Iops` it is created with, for a type that provisions them.
    iops: Option<&'static str>,
    /// Its size in GiB.
    size_gib: i64,
    /// The device the attachment asks for.
    device: &'static str,
    /// The block size the copy moves the payload in, as `dd` reads one.
    block: &'static str,
    /// How many of those blocks the payload is.
    blocks: usize,
    /// Whether Amazon EBS publishes the consumed-IOPS series for this volume type.
    consumed_iops: bool,
}

/// The rows: two volume types that provision IOPS and two that do not, in different sizes and
/// written in different blocks.
const ROWS: &[Row] = &[
    Row {
        case: "a gp3 volume written in kibibyte blocks",
        volume_type: "gp3",
        iops: None,
        size_gib: 8,
        device: "/dev/sdf",
        block: "64K",
        blocks: 8,
        consumed_iops: false,
    },
    Row {
        case: "an io2 volume written in the same instant",
        volume_type: "io2",
        iops: Some("200"),
        size_gib: 4,
        device: "/dev/sdg",
        block: "128K",
        blocks: 4,
        consumed_iops: true,
    },
    Row {
        case: "a gp2 volume written one short block at a time",
        volume_type: "gp2",
        iops: None,
        size_gib: 1,
        device: "/dev/sdh",
        block: "512",
        blocks: 3,
        consumed_iops: false,
    },
];

/// The marker each value the guest's program reports is written under.
const DEVICE_MARKER: &str = "EBS-DEV";
const SERIAL_MARKER: &str = "EBS-SERIAL";
const PAYLOAD_MARKER: &str = "EBS-PAYLOAD";
const WROTE_MARKER: &str = "EBS-WROTE";
const BYTES_MARKER: &str = "EBS-BYTES";
const READBACK_MARKER: &str = "EBS-READBACK";

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
    let mut params = vec![
        p("AvailabilityZone", zone),
        p("Size", row.size_gib.to_string()),
        p("VolumeType", row.volume_type),
    ];
    if let Some(iops) = row.iops {
        params.push(p("Iops", iops));
    }
    let body = ec2_ok(router, case, "CreateVolume", &params).await;
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
    until_attached(router, case, &volume_id).await;
    volume_id
}

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// Look again until `DescribeVolumes` reports the attachment of `volume_id` as `attached`.
async fn until_attached(router: &MultiServiceRouter, case: &str, volume_id: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let body = ec2_ok(
            router,
            case,
            "DescribeVolumes",
            &[p("VolumeId.1", volume_id)],
        )
        .await;
        if super::ec2_query::elements(&body, "status")
            .iter()
            .any(|state| state == "attached")
        {
            return;
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
        "case {case:?}: expected the program to succeed, got status {} with errors {:?} and \
         output {:?}",
        run.status, run.errors, run.output
    );
    run.output
}

/// The program a client runs to use an attached volume: find the device behind the volume's own
/// link, check which volume it carries, write a payload through it and read that range back.
fn block_io_program(volume_id: &str, row: &Row) -> String {
    let link = format!("{BY_ID_LINK}{}", serial_of(volume_id));
    let (block, blocks) = (row.block, row.blocks);
    format!(
        "set -u\n\
         LINK={link}\n\
         for attempt in $(seq 60); do\n\
         \x20 if test -e $LINK; then break; fi\n\
         \x20 sleep 1\n\
         done\n\
         DEV=$(readlink -f $LINK)\n\
         echo \"{DEVICE_MARKER} $DEV\"\n\
         echo \"{SERIAL_MARKER} $(lsblk -dno SERIAL $DEV)\"\n\
         dd if=/dev/urandom of=/tmp/payload bs={block} count={blocks} status=none\n\
         echo \"{PAYLOAD_MARKER} $(sha256sum /tmp/payload | cut -d' ' -f1)\"\n\
         REPORT=$(dd if=/tmp/payload of=$DEV bs={block} oflag=direct conv=fsync 2>&1)\n\
         echo \"{WROTE_MARKER} $?\"\n\
         echo \"{BYTES_MARKER} $(echo \"$REPORT\" | tail -1 | cut -d' ' -f1)\"\n\
         echo \"{READBACK_MARKER} $(dd if=$DEV bs={block} count={blocks} status=none | \
         sha256sum | cut -d' ' -f1)\"\n"
    )
}

/// The datapoints of `metric` for `volume_id` around the instant the I/O happened, as
/// `(sum, sample count, unit)` folded over the window — empty when the series carries none.
async fn volume_datapoints(
    router: &MultiServiceRouter,
    case: &str,
    volume_id: &str,
    metric: &str,
) -> Vec<(f64, f64, String)> {
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
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    answer["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            (
                point["Sum"].as_f64().unwrap_or_default(),
                point["SampleCount"].as_f64().unwrap_or_default(),
                point["Unit"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The sum of `metric` over that window, and zero for a series carrying no datapoint at all.
async fn volume_sum(
    router: &MultiServiceRouter,
    case: &str,
    volume_id: &str,
    metric: &str,
) -> f64 {
    volume_datapoints(router, case, volume_id, metric)
        .await
        .iter()
        .map(|(sum, _, _)| *sum)
        .sum()
}

/// THE RULE, over the table.
#[tokio::test]
async fn a_guest_writes_through_an_attached_volume_and_its_series_report_the_bytes() {
    let (router, engine) = emulator();
    let case = "a Nitro instance with volumes attached";
    let instance_id = launch(&router, case).await;
    until_the_guest_is_up(&engine, case, &instance_id).await;
    let zone = zone_of(&router, case, &instance_id).await;

    for row in ROWS {
        let case = row.case;
        let volume_id = attached_volume(&router, row, &zone, &instance_id).await;
        let serial = serial_of(&volume_id);
        let written = in_guest(
            &engine,
            case,
            &instance_id,
            &block_io_program(&volume_id, row),
        );

        // The device behind the volume's own link is a device of this guest, and it reports the
        // volume it carries.
        let device = marker(&written, DEVICE_MARKER).unwrap_or_default();
        assert!(
            device.starts_with("/dev/nvme"),
            "case {case:?}: expected the link to resolve to the NVMe device carrying \
             {volume_id}, got {device:?} of {written:?}"
        );
        let reported = marker(&written, SERIAL_MARKER).unwrap_or_default();
        assert_eq!(
            reported, serial,
            "case {case:?}: expected the device behind the link to report serial {serial:?}, got \
             {reported:?} of {written:?}"
        );

        // The bytes written through it landed on the volume: reading that range back answers
        // exactly them.
        let payload = marker(&written, PAYLOAD_MARKER).unwrap_or_default();
        let readback = marker(&written, READBACK_MARKER).unwrap_or_default();
        assert_eq!(
            marker(&written, WROTE_MARKER).as_deref(),
            Some("0"),
            "case {case:?}: expected the write to succeed, got {written:?}"
        );
        // The copy REPORTS the bytes it moved, which is what a script reads the count out of.
        let moved = row.blocks * usize::try_from(block_bytes(row.block)).unwrap_or_default();
        assert_eq!(
            marker(&written, BYTES_MARKER).as_deref(),
            Some(moved.to_string().as_str()),
            "case {case:?}: expected the copy to report the {moved} bytes it moved, got \
             {written:?}"
        );
        assert!(
            payload != EMPTY_DIGEST && !payload.is_empty(),
            "case {case:?}: expected a payload of real bytes, got {payload:?} of {written:?}"
        );
        assert_eq!(
            readback, payload,
            "case {case:?}: expected the read-back of the written range to be the payload, got \
             {readback:?} against {payload:?}"
        );

        // ... and the operations that moved them are published under the VOLUME's own dimension.
        // The datapoints are published off the answer to a request, so one call later they are
        // there; it is asked again until they are, never waited for.
        let bytes = f64::from(
            i32::try_from(
                row.blocks
                    * usize::try_from(block_bytes(row.block)).unwrap_or_default(),
            )
            .unwrap_or_default(),
        );
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
        // The read is its own series, and so is the write: the read-back moved the same bytes.
        let read_bytes = volume_sum(&router, case, &volume_id, "VolumeReadBytes").await;
        assert!(
            (read_bytes - bytes).abs() < f64::EPSILON,
            "case {case:?}: expected {bytes} bytes read back on {volume_id}, got {read_bytes}"
        );
        for (metric, unit) in [
            ("VolumeWriteOps", "Count"),
            ("VolumeReadOps", "Count"),
            ("VolumeWriteBytes", "Bytes"),
            ("VolumeReadBytes", "Bytes"),
        ] {
            let points = volume_datapoints(&router, case, &volume_id, metric).await;
            assert!(
                !points.is_empty() && points.iter().all(|(sum, count, _)| *sum > 0.0 && *count > 0.0),
                "case {case:?}: expected {metric} to report the operations performed, got \
                 {points:?}"
            );
            assert!(
                points.iter().all(|(_, _, carried)| carried == unit),
                "case {case:?}: expected {metric} in {unit}, got {points:?}"
            );
        }
        // The consumed-IOPS series exists for a Provisioned IOPS volume type ALONE.
        let consumed = volume_sum(
            &router,
            case,
            &volume_id,
            "VolumeConsumedReadWriteOps",
        )
        .await;
        if row.consumed_iops {
            assert!(
                consumed > 0.0,
                "case {case:?}: a {type_name} volume publishes the consumed operations, got \
                 {consumed}",
                type_name = row.volume_type
            );
        } else {
            assert!(
                consumed.abs() < f64::EPSILON,
                "case {case:?}: a {type_name} volume publishes no consumed-IOPS series at all, \
                 got {consumed}",
                type_name = row.volume_type
            );
        }
    }

    // A volume attached to the same instance that nothing read or wrote carries NO series at all:
    // a measurement of I/O is a measurement of operations something performed.
    let case = "a volume nothing wrote";
    let quiet = attached_volume(
        &router,
        &Row {
            case,
            volume_type: "gp3",
            iops: None,
            size_gib: 2,
            device: "/dev/sdi",
            block: "512",
            blocks: 1,
            consumed_iops: false,
        },
        &zone,
        &instance_id,
    )
    .await;
    for metric in [
        "VolumeWriteBytes",
        "VolumeWriteOps",
        "VolumeReadBytes",
        "VolumeReadOps",
        "VolumeConsumedReadWriteOps",
    ] {
        let points = volume_datapoints(&router, case, &quiet, metric).await;
        assert!(
            points.is_empty(),
            "case {case:?}: expected {metric} to carry no datapoint for {quiet}, got {points:?}"
        );
    }
}

/// The bytes the block size `named` states, as `dd` reads one — written here rather than imported,
/// so the size this suite expects is its own.
fn block_bytes(named: &str) -> i64 {
    let digits: String = named.chars().take_while(char::is_ascii_digit).collect();
    let count: i64 = digits.parse().unwrap_or_default();
    match named.trim_start_matches(|character: char| character.is_ascii_digit()) {
        "" => count,
        "K" => count * 1024,
        "M" => count * 1024 * 1024,
        other => panic!("this suite writes no block size spelled {other:?}"),
    }
}

/// RULE, at the attach/detach boundary: a volume that is not attached is not a device of any
/// guest, so nothing can be read or written through it — and the data and the series FOLLOW the
/// volume when it is attached to another instance.
#[tokio::test]
async fn the_device_and_its_data_follow_the_volume_across_a_move() {
    let (router, engine) = emulator();
    let case = "a volume moved to a second instance";
    let first = launch(&router, case).await;
    let second = launch(&router, case).await;
    until_the_guest_is_up(&engine, case, &first).await;
    until_the_guest_is_up(&engine, case, &second).await;
    let zone = zone_of(&router, case, &first).await;
    let row = Row {
        case,
        volume_type: "io1",
        iops: Some("150"),
        size_gib: 4,
        device: "/dev/sdf",
        block: "64K",
        blocks: 2,
        consumed_iops: true,
    };
    let volume_id = attached_volume(&router, &row, &zone, &first).await;
    let serial = serial_of(&volume_id);
    let link = format!("{BY_ID_LINK}{serial}");

    // The first instance's guest writes a payload through the device.
    let written = in_guest(&engine, case, &first, &block_io_program(&volume_id, &row));
    let payload = marker(&written, PAYLOAD_MARKER).unwrap_or_default();
    assert_eq!(
        marker(&written, READBACK_MARKER).unwrap_or_default(),
        payload,
        "case {case:?}: the first instance must read back what it wrote, got {written:?}"
    );

    // The SECOND instance has no such device while the volume is attached to the first: nothing
    // of it is in that guest at all.
    let before = in_guest(
        &engine,
        case,
        &second,
        &format!(
            "if test -e {link}; then echo \"{DEVICE_MARKER} $(readlink -f {link})\"; else echo \
             \"{DEVICE_MARKER} none\"; fi"
        ),
    );
    assert_eq!(
        marker(&before, DEVICE_MARKER).as_deref(),
        Some("none"),
        "case {case:?}: a volume attached elsewhere is no device of this guest, got {before:?}"
    );

    // Detach it, attach it to the second instance, and the SAME bytes are behind the same link
    // there: the data is the volume's.
    let body = ec2_ok(
        &router,
        case,
        "DetachVolume",
        &[p("VolumeId", &volume_id), p("InstanceId", &first)],
    )
    .await;
    assert!(
        !body.is_empty(),
        "case {case:?}: DetachVolume must answer the attachment it took down"
    );
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = super::ec2_query::call(
            &router,
            REGION,
            "AttachVolume",
            &[
                p("VolumeId", &volume_id),
                p("InstanceId", &second),
                p("Device", "/dev/sdf"),
            ],
        )
        .await;
        if status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the volume never attached to {second}, last answer {status}: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    until_attached(&router, case, &volume_id).await;
    let after = in_guest(
        &engine,
        case,
        &second,
        &format!(
            "DEV=$(readlink -f {link})\n\
             echo \"{SERIAL_MARKER} $(lsblk -dno SERIAL $DEV)\"\n\
             echo \"{READBACK_MARKER} $(dd if=$DEV bs={block} count={blocks} status=none | \
             sha256sum | cut -d' ' -f1)\"\n",
            block = row.block,
            blocks = row.blocks
        ),
    );
    assert_eq!(
        marker(&after, SERIAL_MARKER).as_deref(),
        Some(serial.as_str()),
        "case {case:?}: the device in the second guest must carry the same volume, got {after:?}"
    );
    assert_eq!(
        marker(&after, READBACK_MARKER).unwrap_or_default(),
        payload,
        "case {case:?}: the bytes are the volume's, so the second guest reads back what the \
         first wrote; got {after:?}"
    );
}
