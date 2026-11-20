// SPDX-License-Identifier: CC0-1.0

//! What an instance's GUEST sees of its own CPUs, driven over the wire the service actually
//! speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness — and
//! observed where a client observes anything a guest wrote: that instance's CONSOLE ("the
//! instance console output displays the exact console output that would normally be displayed
//! on a physical monitor attached to a computer", `GetConsoleOutput`).
//!
//! The rule under test: **the CPU topology a launch asked for is the topology the GUEST's own
//! operating system reports** — "the CPU options for the instance"
//! (`RunInstancesRequest$CpuOptions`), whose `ThreadsPerCore` of `1` disables multithreading,
//! is a request about the machine, so a program inside the guest reads back that many
//! processors from `nproc`, that range from `/sys/devices/system/cpu/online`, that many
//! `processor` blocks and that many `siblings` from `/proc/cpuinfo`, and those counts from
//! `lscpu`. The control plane's own `DescribeInstances` and the guest cannot disagree: both
//! are read from the one topology the instance holds.
//!
//! The rows differ along what the rule generalises over: the instance type, whether the
//! launch asks for a topology at all, how many cores it asks for, and whether it asks for one
//! thread per core or the type's default — so the licence-halving single-thread case is one
//! row of the table rather than the specification.

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, TRANSITION_DELAY};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, described_instance_type, element, member};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The instance type the pipeline rows launch: a type that runs two threads per core and
/// offers several cores, so turning multithreading off and asking for fewer cores are both
/// observable.
const INSTANCE_TYPE: &str = "m5.4xlarge";

/// The program every launch here carries: it reports the machine's CPU layout the four ways
/// a guest's own scripts read it, each on the console.
const TOPOLOGY_PROGRAM: &str = "#cloud-config\n\
    bootcmd:\n\
    \x20 - |\n\
    \x20   echo \"GUEST-NPROC $(nproc)\" > /dev/console\n\
    \x20   echo \"GUEST-ONLINE $(cat /sys/devices/system/cpu/online)\" > /dev/console\n\
    \x20   lscpu > /dev/console\n\
    \x20   cat /proc/cpuinfo > /dev/console\n";

/// The same layout read the way a CLIENT'S OWN SCRIPT reads it: the utility pipelines an
/// operator writes, over the paths and the labels a real instance publishes. A layout
/// published under a label or a path those pipelines do not find is one no licence check
/// inside the guest can read, so the forms are part of the rule.
const READING_PROGRAM: &str = "#cloud-config\n\
    bootcmd:\n\
    \x20 - |\n\
    \x20   echo \"READ-NPROC $(nproc)\" > /dev/console\n\
    \x20   echo \"READ-ONLINE $(cat /sys/devices/system/cpu/online)\" > /dev/console\n\
    \x20   echo \"READ-PROCESSORS $(grep -c ^processor /proc/cpuinfo)\" > /dev/console\n\
    \x20   echo \"READ-SIBLINGS $(cat /sys/devices/system/cpu/cpu0/topology/thread_siblings_list)\" > /dev/console\n\
    \x20   echo \"READ-CORE $(cat /sys/devices/system/cpu/cpu0/topology/core_id)\" > /dev/console\n\
    \x20   echo \"READ-CPUS $(lscpu | grep '^CPU(s):' | cut -d: -f2)\" > /dev/console\n\
    \x20   echo \"READ-TPC $(lscpu | grep 'Thread(s) per core' | cut -d: -f2)\" > /dev/console\n\
    \x20   echo \"READ-CPS $(lscpu | grep 'Core(s) per socket' | cut -d: -f2)\" > /dev/console\n";

/// One emulator over its own engine, with one AMI of the calling account seeded the way a
/// register operation would store it — answered beside its image id.
fn emulator() -> (MultiServiceRouter, String) {
    let (router, engine) = super::ec2_query::emulator();
    let image_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard.insert_image(ImageSpec {
            name: "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64".to_string(),
            ..ImageSpec::default()
        })
    };
    (router, image_id)
}

/// Launch one instance of `instance_type` carrying `program` as its user data and
/// `cpu_options`, and answer its id.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    instance_type: &str,
    program: &str,
    cpu_options: &[Param],
) -> String {
    let mut params = vec![
        param("ImageId", image_id),
        param("InstanceType", instance_type),
        param("MinCount", "1"),
        param("MaxCount", "1"),
        param(
            "UserData",
            &base64::engine::general_purpose::STANDARD.encode(program),
        ),
    ];
    params.extend_from_slice(cpu_options);
    let (status, body) = call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status}: {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// One instance's console as text — empty for an instance whose guest has not written one
/// yet, which is an instance whose answer carries no `output` member at all.
async fn console(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "GetConsoleOutput",
        &[param("InstanceId", instance_id), param("Latest", "true")],
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

/// Wait until the boot the launch owes the guest has written `marker` to the console, and
/// answer the console text.
///
/// The boot is work the CLOCK owes the instance, so the console is looked at AGAIN in a loop
/// until the guest's own line is on it or a generous deadline passes — nothing here depends
/// on how fast the host runs. The deadline is a multiple of the service's own transition
/// span, far enough out that a loaded host is never the reason a case fails.
async fn console_carrying(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    marker: &str,
) -> String {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let text = console(router, case, instance_id).await;
        if text.lines().any(|line| line.contains(marker)) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the guest of {instance_id} must write {marker:?} to its console, \
             got {text:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    }
}

/// The value the console line beginning `marker ` carries, or `None` when no line does.
fn marked(text: &str, marker: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once(&format!("{marker} ")))
        .map(|(_, value)| value.trim().to_string())
        .next()
}

/// The value the `lscpu` line labelled `label` carries on the console.
fn labelled(text: &str, label: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line.split_once(label))
        .map(|(_, value)| value.trim().to_string())
        .next()
}

/// The `cpuOptions` the control plane reports for `instance_id`, as
/// `(coreCount, threadsPerCore)`.
async fn reported_cpu_options(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (i32, i32) {
    let (status, body) = call(
        router,
        REGION,
        "DescribeInstances",
        &[param("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must succeed, got {status}: {body}"
    );
    let parsed = document(case, &body);
    let options = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "cpuOptions")
        .unwrap_or_else(|| panic!("case {case:?}: the instance reports no cpuOptions: {body}"));
    let number = |name: &str| -> i32 {
        child_text(options, name)
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: cpuOptions carries no {name}: {body}"))
    };
    (number("coreCount"), number("threadsPerCore"))
}

/// The counts `DescribeInstanceTypes` publishes as `instance_type`'s defaults.
async fn published_defaults(
    router: &MultiServiceRouter,
    case: &str,
    instance_type: &str,
) -> (i32, i32) {
    let body = described_instance_type(case, router, REGION, instance_type).await;
    let number = |name: &str| -> i32 {
        element(&body, name)
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or_else(|| panic!("case {case:?}: {instance_type} publishes no {name}: {body}"))
    };
    (number("defaultCores"), number("defaultThreadsPerCore"))
}

/// One row: the case, the instance type, and the `CpuOptions` the launch asks for — `None`
/// for a launch that asks for none, whose expectation is the type's own published defaults.
struct Row {
    case: &'static str,
    instance_type: &'static str,
    asked: Option<(i32, i32)>,
}

/// THE RULE, over the table.
#[tokio::test]
async fn a_guest_reports_the_cpu_topology_its_launch_asked_for() {
    let rows = [
        Row {
            case: "a launch that asks for no topology at all",
            instance_type: "m5.large",
            asked: None,
        },
        Row {
            case: "one core with multithreading turned off",
            instance_type: "m5.large",
            asked: Some((1, 1)),
        },
        Row {
            case: "two cores with multithreading turned off",
            instance_type: "m5.4xlarge",
            asked: Some((2, 1)),
        },
        Row {
            case: "two cores with multithreading left on",
            instance_type: "m5.4xlarge",
            asked: Some((2, 2)),
        },
        Row {
            case: "every core of a larger type, multithreading off",
            instance_type: "c5.2xlarge",
            asked: Some((4, 1)),
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, image_id) = emulator();
        let fields: Vec<Param> = match row.asked {
            Some((cores, threads)) => vec![
                param("CpuOptions.CoreCount", &cores.to_string()),
                param("CpuOptions.ThreadsPerCore", &threads.to_string()),
            ],
            None => Vec::new(),
        };
        let instance_id = launch(
            &router,
            case,
            &image_id,
            row.instance_type,
            TOPOLOGY_PROGRAM,
            &fields,
        )
        .await;
        // What the launch asked for, or the type's own published defaults where it asked for
        // nothing — never a count written into this suite.
        let (cores, threads_per_core) = match row.asked {
            Some(asked) => asked,
            None => published_defaults(&router, case, row.instance_type).await,
        };
        let vcpus = cores * threads_per_core;
        let online = if vcpus == 1 {
            "0".to_string()
        } else {
            format!("0-{}", vcpus - 1)
        };

        // The control plane's own answer first: the guest is to agree with THIS.
        let reported = reported_cpu_options(&router, case, &instance_id).await;
        assert_eq!(
            reported,
            (cores, threads_per_core),
            "case {case:?}: DescribeInstances must report {cores}/{threads_per_core}, got \
             {reported:?}"
        );

        let text = console_carrying(&router, case, &instance_id, "GUEST-NPROC").await;
        assert_eq!(
            marked(&text, "GUEST-NPROC").as_deref(),
            Some(vcpus.to_string().as_str()),
            "case {case:?}: the guest's nproc must report {vcpus}, got {text:?}"
        );
        assert_eq!(
            marked(&text, "GUEST-ONLINE").as_deref(),
            Some(online.as_str()),
            "case {case:?}: the guest's online processors must be {online:?}, got {text:?}"
        );
        for (label, expected) in [
            ("CPU(s):", vcpus),
            ("Thread(s) per core:", threads_per_core),
            ("Core(s) per socket:", cores),
        ] {
            assert_eq!(
                labelled(&text, label).as_deref(),
                Some(expected.to_string().as_str()),
                "case {case:?}: lscpu must report {label} {expected}, got {text:?}"
            );
        }
        let processors = text
            .lines()
            .filter(|line| line.starts_with("processor\t: "))
            .count();
        assert_eq!(
            processors,
            usize::try_from(vcpus).unwrap_or_default(),
            "case {case:?}: /proc/cpuinfo must carry {vcpus} processor block(s), got {text:?}"
        );
        assert!(
            text.lines()
                .any(|line| line == format!("siblings\t: {vcpus}")),
            "case {case:?}: /proc/cpuinfo must report {vcpus} siblings, got {text:?}"
        );
        assert!(
            text.lines()
                .any(|line| line == format!("cpu cores\t: {cores}")),
            "case {case:?}: /proc/cpuinfo must report {cores} cores, got {text:?}"
        );
    }
}

/// THE SAME RULE through the utility PIPELINES a client's own script writes
/// ([`READING_PROGRAM`]): a layout published under a label or a path those pipelines do not
/// find is a layout no licence check or thread-pool sizing inside the guest can read.
///
/// The rows differ along whether the launch turns multithreading off and how many cores it
/// asks for.
#[tokio::test]
async fn a_guests_own_script_reads_that_layout_with_the_utilities_it_has() {
    // (case, cores, threads per core)
    let rows = [
        ("two cores with multithreading turned off", 2, 1),
        ("two cores with multithreading left on", 2, 2),
        ("one core with multithreading turned off", 1, 1),
    ];

    for (case, cores, threads_per_core) in rows {
        let (router, image_id) = emulator();
        let instance_id = launch(
            &router,
            case,
            &image_id,
            INSTANCE_TYPE,
            READING_PROGRAM,
            &[
                param("CpuOptions.CoreCount", &cores.to_string()),
                param("CpuOptions.ThreadsPerCore", &threads_per_core.to_string()),
            ],
        )
        .await;
        let text = console_carrying(&router, case, &instance_id, "READ-NPROC").await;

        let vcpus = cores * threads_per_core;
        let online = if vcpus == 1 {
            "0".to_string()
        } else {
            format!("0-{}", vcpus - 1)
        };
        // The siblings of the first processor: the threads of core 0, a stride of one core
        // apart, which is how the kernel enumerates them.
        let siblings = (0..threads_per_core)
            .map(|thread| (thread * cores).to_string())
            .collect::<Vec<String>>()
            .join(",");
        for (marker, expected) in [
            ("READ-NPROC", vcpus.to_string()),
            ("READ-ONLINE", online),
            ("READ-PROCESSORS", vcpus.to_string()),
            ("READ-SIBLINGS", siblings),
            ("READ-CORE", "0".to_string()),
            ("READ-CPUS", vcpus.to_string()),
            ("READ-TPC", threads_per_core.to_string()),
            ("READ-CPS", cores.to_string()),
        ] {
            assert_eq!(
                marked(&text, marker).as_deref(),
                Some(expected.as_str()),
                "case {case:?}: {marker} must read {expected:?}, got {text:?}"
            );
        }
    }
}
