// SPDX-License-Identifier: CC0-1.0

//! What a guest sees of its own ROOT FILESYSTEM, and what the CloudWatch agent inside it
//! publishes about that filesystem — driven over the wire the service speaks (`ec2Query`,
//! through the crate's shared [`super::ec2_query`] harness), through the one seam a program of a
//! guest runs through (`run_in_instance_guest`, which is what the agent inside a guest delivers
//! a script to), and read back through Amazon CloudWatch's own `GetMetricStatistics`.
//!
//! THE RULE: a guest reports its ROOT FILESYSTEM out of the volume behind it — `df` writes the
//! size of that filesystem, how much of it is used, how much is available and the share used
//! (rounded up to the whole per cent, which is what the utility writes), in the columns and the
//! unit the invocation asked for. Space a program RESERVES on it (`fallocate -l LENGTH PATH`,
//! which "preallocates blocks to a file") is space in use from then on, so the used figure rises
//! by exactly what was reserved, the available figure falls by it, the file reports that size,
//! and a reservation the filesystem has no room for is refused and changes nothing. The
//! CloudWatch agent's `disk_` measurements are the SAME reading — a measurement no configuration
//! declares is never published at all — which is what makes a capacity alarm built on
//! `disk_used_percent` follow what the guest's own `df` says.
//!
//! The rows differ along what the rule generalises over: the SIZE of the root volume behind the
//! filesystem, how much space is reserved on it, and the unit and the columns each report is
//! asked for. Every expectation is a relation among the report's own figures and the volume size
//! the row launched — never a constant of the emulator's own — so a wrong figure cannot make a
//! case pass.
//!
//! Nothing here reads either service's state for its answers: Amazon CloudWatch is mounted
//! BESIDE Amazon EC2 on one engine, exactly as the assembled router mounts them, and every
//! datapoint is read back through CloudWatch's own API.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use serde_json::json;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::element;
use super::instance_metric_publication::{cloudwatch_ok, ec2_ok, emulator, p};

/// The instance type every row launches.
const INSTANCE_TYPE: &str = "t3.micro";

/// The package the agent is installed as, and the control program it ships — spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make a case pass.
const AGENT_PACKAGE: &str = "amazon-cloudwatch-agent";

/// See [`AGENT_PACKAGE`].
const AGENT_CONTROL: &str = "/opt/aws/amazon-cloudwatch-agent/bin/amazon-cloudwatch-agent-ctl";

/// How often the agent each row configures takes its measurements, in seconds.
const COLLECTION_INTERVAL: u64 = 1;

/// The path the fill is written at, which is under a directory every guest of this platform
/// already has.
const FILL_PATH: &str = "/var/tmp/fill";

/// How long the polls below keep asking before giving up: far above the span any move they wait
/// for is owed in, so a loaded host is not what decides a case.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How often they look again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The window the datapoints are read over, either side of the instant the collection happened.
const READ_WINDOW_SECS: i64 = 3_600;

/// The period they are read at.
const READ_PERIOD_SECS: i64 = 60;

/// How many kibibytes one gibibyte is, which is the unit `df` reports blocks in by default.
const KIB_PER_GIB: i64 = 1024 * 1024;

/// One row: a root volume to launch and a reservation to take on the filesystem it carries.
struct Row {
    /// The case, named in every assertion.
    case: &'static str,
    /// The size of the root volume the image restores, in GiB — which is the size of the
    /// filesystem on it.
    volume_gib: i32,
    /// How many GiB the row reserves on that filesystem.
    fill_gib: i64,
    /// The namespace the agent of this row publishes under.
    namespace: &'static str,
}

/// The rows: three filesystems of different sizes, filled by different amounts.
const ROWS: &[Row] = &[
    Row {
        case: "the published default root volume filled by two gibibytes",
        volume_gib: 8,
        fill_gib: 2,
        namespace: "Dcwagent/EightGiB",
    },
    Row {
        case: "a small root volume filled by one gibibyte",
        volume_gib: 4,
        fill_gib: 1,
        namespace: "Dcwagent/FourGiB",
    },
    Row {
        case: "a large root volume filled by sixteen gibibytes",
        volume_gib: 64,
        fill_gib: 16,
        namespace: "Dcwagent/SixtyFourGiB",
    },
];

/// The measurements every row's configuration declares, of the agent's `disk` section.
const DECLARED: &[&str] = &["used_percent", "total", "used", "free"];

/// A measurement no row's configuration declares, which must therefore never be published.
const UNDECLARED_MEASUREMENT: &str = "mem_used_percent";

/// The configuration document a client writes for the agent of `row`: the namespace, the one
/// dimension it appends and the `disk` measurements it declares.
fn agent_document(row: &Row) -> String {
    let declared = DECLARED
        .iter()
        .map(|measurement| format!("\"{measurement}\""))
        .collect::<Vec<String>>()
        .join(", ");
    format!(
        "{{\n\
         \x20 \"agent\": {{\"metrics_collection_interval\": {COLLECTION_INTERVAL}}},\n\
         \x20 \"metrics\": {{\n\
         \x20   \"namespace\": \"{}\",\n\
         \x20   \"append_dimensions\": {{\"InstanceId\": \"${{aws:InstanceId}}\"}},\n\
         \x20   \"metrics_collected\": {{\n\
         \x20     \"disk\": {{\"resources\": [\"/\"], \"measurement\": [{declared}]}}\n\
         \x20   }}\n\
         \x20 }}\n\
         }}",
        row.namespace
    )
}

/// The script that installs the agent, hands it `row`'s configuration, starts it and then reads
/// the filesystem the way a client's own script reads it — the columns through `--output`, the
/// default report through its own fifth column, and the human-readable size.
fn install_script(row: &Row) -> String {
    format!(
        "set -e\n\
         dnf install -y {AGENT_PACKAGE}\n\
         cat > /tmp/cwagent.json <<'EOF'\n{document}\nEOF\n\
         {AGENT_CONTROL} -a fetch-config -m ec2 -s -c file:/tmp/cwagent.json\n\
         echo \"RPM $(rpm -q {AGENT_PACKAGE})\"\n\
         {REPORT_SCRIPT}",
        document = agent_document(row),
    )
}

/// The lines that read the filesystem's own figures, each written under a marker a client reads
/// it back by — every one of them a form the documented probes are written in.
const REPORT_SCRIPT: &str = "echo \"PCENT $(df --output=pcent / | tail -1 | tr -dc 0-9.)\"\n\
     echo \"AVAIL $(df --output=avail / | tail -1 | tr -dc 0-9)\"\n\
     echo \"AVAIL-MIB $(df --output=avail -m / | tail -1 | tr -dc 0-9)\"\n\
     echo \"USED $(df --output=used / | tail -1 | tr -dc 0-9)\"\n\
     echo \"SIZE $(df --output=size / | tail -1 | tr -dc 0-9)\"\n\
     echo \"DEFAULT-PCENT $(df / | tail -1 | tr -s ' ' | cut -d' ' -f5 | tr -dc 0-9)\"\n\
     echo \"HUMAN-SIZE $(df -h --output=size / | tail -1 | tr -d ' ')\"\n\
     echo \"MOUNT $(df / | tail -1 | tr -s ' ' | cut -d' ' -f6)\"\n";

/// The script that RESERVES `fill_gib` gibibytes on the filesystem, flushes it and reads both
/// the file's own size and the filesystem's figures again.
fn fill_script(fill_gib: i64) -> String {
    format!(
        "set -e\n\
         fallocate -l {fill_gib}G {FILL_PATH}\n\
         sync\n\
         echo \"FILL-SIZE $(stat -c %s {FILL_PATH})\"\n\
         {REPORT_SCRIPT}",
    )
}

/// What one reading of the filesystem came to, as the guest's own report states it.
#[derive(Debug, Clone, PartialEq)]
struct Report {
    /// The share in use, from the `pcent` column.
    pcent: f64,
    /// The available blocks, in kibibytes.
    avail_kib: i64,
    /// The same figure asked for in mebibytes.
    avail_mib: i64,
    /// The used blocks, in kibibytes.
    used_kib: i64,
    /// The filesystem's size, in kibibytes.
    size_kib: i64,
    /// The share in use as the DEFAULT report's own fifth column carries it.
    default_pcent: f64,
    /// The size as the human-readable form writes it.
    human_size: String,
    /// Where the filesystem is mounted, from the default report's last column.
    mount: String,
}

/// The reading the markers of `written` carry.
fn report_of(case: &str, written: &[String]) -> Report {
    let marker = |name: &str| -> String {
        written
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{name} ")))
            .map(str::trim)
            .map(str::to_string)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the guest must report {name}, got {written:?}")
            })
    };
    let figure = |name: &str| -> f64 {
        let held = marker(name);
        held.parse().unwrap_or_else(|_| {
            panic!("case {case:?}: {name} must be a figure, got {held:?} in {written:?}")
        })
    };
    let whole = |name: &str| -> i64 {
        let held = marker(name);
        held.parse().unwrap_or_else(|_| {
            panic!("case {case:?}: {name} must be a whole number, got {held:?} in {written:?}")
        })
    };
    Report {
        pcent: figure("PCENT"),
        avail_kib: whole("AVAIL"),
        avail_mib: whole("AVAIL-MIB"),
        used_kib: whole("USED"),
        size_kib: whole("SIZE"),
        default_pcent: figure("DEFAULT-PCENT"),
        human_size: marker("HUMAN-SIZE"),
        mount: marker("MOUNT"),
    }
}

/// Whether `held` and `expected` are the same figure to within one block, which is what two
/// figures a filesystem states in whole blocks can differ by when the bytes behind them are not
/// a whole number of blocks.
fn within_a_block(held: i64, expected: i64) -> bool {
    (held - expected).abs() <= 1
}

/// One emulator over its own engine, with an AMI whose root volume is `volume_gib` seeded the
/// way a register operation would store it — answered beside its image id.
fn seeded_image(engine: &engine::InMemoryEngine, name: &str, volume_gib: i32) -> String {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    guard.insert_image(ImageSpec {
        name: name.to_string(),
        root_volume_size: volume_gib,
        ..ImageSpec::default()
    })
}

/// Launch one instance from `image_id` and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, image_id: &str) -> String {
    let body = ec2_ok(
        router,
        case,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("InstanceType", INSTANCE_TYPE),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("Monitoring.Enabled", "true"),
        ],
    )
    .await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
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

/// Run `script` in that instance's guest — the seam the agent inside a guest delivers a script
/// to — and answer the lines it wrote, with the status it ended on.
fn in_guest(
    engine: &engine::InMemoryEngine,
    case: &str,
    instance_id: &str,
    script: &str,
) -> (i32, Vec<String>, Vec<String>) {
    let state = ec2::state::service_state(engine).expect("the EC2 store");
    let mut store = state.lock();
    let run = store
        .run_in_instance_guest(instance_id, &[script.to_string()])
        .unwrap_or_else(|| panic!("case {case:?}: the guest of {instance_id} must run a program"));
    (run.status, run.output, run.errors)
}

/// Run `script` in that guest, which must succeed, and answer the lines it wrote.
fn in_guest_ok(
    engine: &engine::InMemoryEngine,
    case: &str,
    instance_id: &str,
    script: &str,
) -> Vec<String> {
    let (status, output, errors) = in_guest(engine, case, instance_id, script);
    assert_eq!(
        status, 0,
        "case {case:?}: expected the script to succeed, got status {status} with errors \
         {errors:?} and output {output:?}"
    );
    output
}

/// The largest value the series `metric` of `namespace` holds for that instance over a window
/// the whole case falls in, and `None` for a series carrying no datapoint at all.
async fn published(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    instance_id: &str,
) -> Option<f64> {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or_default();
    let answered = cloudwatch_ok(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": namespace,
            "MetricName": metric,
            "Dimensions": [{"Name": "InstanceId", "Value": instance_id}],
            "StartTime": now - READ_WINDOW_SECS,
            "EndTime": now + READ_PERIOD_SECS,
            "Period": READ_PERIOD_SECS,
            "Statistics": ["Maximum"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["Maximum"].as_f64())
        .fold(None, |held: Option<f64>, value| {
            Some(held.map_or(value, |held| held.max(value)))
        })
}

/// Look again until the agent has published `metric`, answering the largest value it carries.
///
/// A collection is work the CLOCK owes the agent, and every request of the service publishes
/// what is owed, so the series is read AGAIN in a loop until it carries a datapoint or a
/// generous deadline passes — nothing here depends on how fast the host runs.
async fn when_published(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    instance_id: &str,
) -> f64 {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        if let Some(value) = published(router, case, namespace, metric, instance_id).await {
            return value;
        }
        // Every request of the service publishes the collections the agent already owes.
        let _ = ec2_ok(
            router,
            case,
            "DescribeInstances",
            &[p("InstanceId.1", instance_id)],
        )
        .await;
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the agent to publish {namespace}/{metric} for \
             {instance_id}, and it published nothing"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// THE RULE, over the table.
#[tokio::test]
async fn a_guest_reports_its_root_filesystem_and_the_agent_publishes_the_same_reading() {
    let (router, engine) = emulator();
    for row in ROWS {
        let case = row.case;
        let image_id = seeded_image(&engine, case, row.volume_gib);
        let instance_id = launch(&router, case, &image_id).await;
        until_the_guest_is_up(&engine, case, &instance_id).await;

        // The filesystem the guest reports is the one on the volume behind it, and its figures
        // are consistent: the size is the volume's own, what is used and what is available come
        // to it, and the share is the used share rounded up to the whole per cent.
        let written = in_guest_ok(&engine, case, &instance_id, &install_script(row));
        let before = report_of(case, &written);
        assert_eq!(
            before.size_kib,
            i64::from(row.volume_gib) * KIB_PER_GIB,
            "case {case:?}: expected the filesystem to be the size of its volume, got {before:?}"
        );
        assert!(
            before.pcent > 0.0 && before.pcent < 100.0,
            "case {case:?}: expected a share strictly inside (0, 100), got {before:?}"
        );
        assert!(
            within_a_block(before.used_kib + before.avail_kib, before.size_kib),
            "case {case:?}: expected what is used and what is available to come to the size, to \
             the whole block, got {before:?}"
        );
        assert!(
            within_a_block(before.avail_mib, before.avail_kib.div_euclid(1024)),
            "case {case:?}: expected the same figure in mebibytes, got {before:?}"
        );
        // The share is the used share rounded UP to the whole per cent, which is the figure the
        // utility writes — read off the report's own columns rather than any constant.
        #[allow(clippy::cast_precision_loss)]
        let share_of_columns = (before.used_kib * 100) as f64 / before.size_kib as f64;
        assert!(
            before.pcent >= share_of_columns && before.pcent < share_of_columns + 2.0,
            "case {case:?}: expected the share to be the used share ({share_of_columns}) rounded \
             up, got {before:?}"
        );
        assert!(
            (before.default_pcent - before.pcent).abs() < f64::EPSILON,
            "case {case:?}: expected the default report to carry the same share, got {before:?}"
        );
        assert_eq!(
            before.mount, "/",
            "case {case:?}: expected the filesystem to be reported mounted on /, got {before:?}"
        );
        assert!(
            before.human_size.ends_with('G') || before.human_size.ends_with('T'),
            "case {case:?}: expected the human-readable size in the largest unit it fits in, \
             got {before:?}"
        );
        // There is room for what this row is about to reserve.
        assert!(
            before.avail_kib >= row.fill_gib * KIB_PER_GIB,
            "case {case:?}: expected room for {} GiB, got {before:?}",
            row.fill_gib
        );

        // The agent's own measurement of the SAME filesystem, which is what a capacity alarm
        // watches.
        let share = when_published(
            &router,
            case,
            row.namespace,
            "disk_used_percent",
            &instance_id,
        )
        .await;
        assert!(
            (share - before.pcent).abs() <= 1.0,
            "case {case:?}: expected the agent's disk_used_percent to be the share the guest \
             reports ({}), got {share}",
            before.pcent
        );
        #[allow(clippy::cast_precision_loss)]
        let size_bytes = (before.size_kib * 1024) as f64;
        let total = when_published(&router, case, row.namespace, "disk_total", &instance_id).await;
        assert!(
            (total - size_bytes).abs() < f64::EPSILON,
            "case {case:?}: expected the agent's disk_total to be the filesystem's size \
             ({size_bytes}), got {total}"
        );

        // RESERVING space on the filesystem is space in use: the used figure rises by exactly
        // what was reserved, the available figure falls by it, and the file reports that size.
        let written = in_guest_ok(&engine, case, &instance_id, &fill_script(row.fill_gib));
        let after = report_of(case, &written);
        let fill_bytes = row.fill_gib * 1024 * 1024 * 1024;
        let reported_size = written
            .iter()
            .find_map(|line| line.strip_prefix("FILL-SIZE "))
            .map(str::trim)
            .and_then(|held| held.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: the file's size must be reported: {written:?}"));
        assert_eq!(
            reported_size, fill_bytes,
            "case {case:?}: expected the file to be the size reserved for it, got \
             {reported_size}"
        );
        assert!(
            within_a_block(after.used_kib - before.used_kib, fill_bytes / 1024),
            "case {case:?}: expected the used figure to rise by what was reserved, got {after:?} \
             against {before:?}"
        );
        assert!(
            within_a_block(before.avail_kib - after.avail_kib, fill_bytes / 1024),
            "case {case:?}: expected the available figure to fall by what was reserved, got \
             {after:?} against {before:?}"
        );
        assert!(
            after.pcent > before.pcent,
            "case {case:?}: expected the share in use to rise, got {after:?} against {before:?}"
        );
        assert_eq!(
            after.size_kib, before.size_kib,
            "case {case:?}: expected the filesystem's size to be unchanged, got {after:?}"
        );

        // ... and the agent publishes that same risen share.
        let deadline = Instant::now() + POLL_DEADLINE;
        let risen = loop {
            let held = when_published(
                &router,
                case,
                row.namespace,
                "disk_used_percent",
                &instance_id,
            )
            .await;
            if (held - after.pcent).abs() <= 1.0 {
                break held;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the agent to publish the risen share ({}), last read \
                 {held}",
                after.pcent
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert!(
            risen > before.pcent,
            "case {case:?}: expected the published share to rise with the filesystem, got \
             {risen} against {}",
            before.pcent
        );

        // A reservation the filesystem has no room for is refused, and changes nothing.
        let (status, _output, errors) = in_guest(
            &engine,
            case,
            &instance_id,
            &format!("fallocate -l {}G /var/tmp/past-the-end\n", row.volume_gib),
        );
        assert_ne!(
            status, 0,
            "case {case:?}: expected a reservation past the filesystem to fail, got {status}"
        );
        assert!(
            errors
                .iter()
                .any(|line| line.contains("No space left on device")),
            "case {case:?}: expected the utility's own diagnostic, got {errors:?}"
        );
        let refused = report_of(
            case,
            &in_guest_ok(&engine, case, &instance_id, REPORT_SCRIPT),
        );
        assert_eq!(
            refused.used_kib, after.used_kib,
            "case {case:?}: expected a refused reservation to change nothing, got {refused:?} \
             against {after:?}"
        );

        // Removing the file the space was reserved for gives that space back.
        let released = report_of(
            case,
            &in_guest_ok(
                &engine,
                case,
                &instance_id,
                &format!("set -e\nrm {FILL_PATH}\n{REPORT_SCRIPT}"),
            ),
        );
        assert!(
            within_a_block(released.avail_kib - after.avail_kib, fill_bytes / 1024),
            "case {case:?}: expected the reserved space back once the file is gone, got \
             {released:?} against {after:?}"
        );
        assert!(
            within_a_block(released.avail_kib, before.avail_kib),
            "case {case:?}: expected the filesystem as it was before the reservation, got \
             {released:?} against {before:?}"
        );

        // A measurement no configuration declares is never published, whatever the guest can
        // measure: the agent publishes what its document declares and nothing else.
        let undeclared = published(
            &router,
            case,
            row.namespace,
            UNDECLARED_MEASUREMENT,
            &instance_id,
        )
        .await;
        assert!(
            undeclared.is_none(),
            "case {case:?}: a measurement no configuration declares must never appear, got \
             {undeclared:?}"
        );
    }
}
