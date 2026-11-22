// SPDX-License-Identifier: CC0-1.0

//! What the kernel publishes to an instance's GUEST about how long that guest has been up —
//! driven through the operations that own the instance (`ec2Query`, the crate's shared
//! [`super::ec2_query`] harness) and the one seam a program of that guest runs through
//! ([`ec2::state::ComAmazonawsEc2Amazonec2State::run_in_instance_guest`]), which is what the
//! agent inside a guest delivers a script to.
//!
//! RULE: a guest reads how long it has been up from `/proc/uptime`, which "contains two numbers
//! (values in seconds): the uptime of the system … and the amount of time spent in the idle
//! process" (`proc(5)`). It is a SPAN measured from the instant that boot came up rather than a
//! value written once, so:
//!
//! * a program of a booted guest reads two seconds figures there, the idle one never below the
//!   uptime (a machine has at least one processor to be idle on);
//! * a later program of the same boot reads a LARGER uptime than an earlier one;
//! * a reboot starts a new boot, and the uptime is counted from that boot — so a program run
//!   after one reads less than a program run before it did.
//!
//! The rows differ along what the rule generalises over: the FORM the program reads the file
//! with (the whole file, the first field cut out of it, the file as a shell variable's value),
//! and which boot of the guest reads it.

use std::time::Duration;

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec, TRANSITION_DELAY};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The uptime a guest has to have reached before the reboot arm asks for a smaller one: the
/// CONDITION the test waits for, so what it then asserts does not depend on how fast this host
/// runs. The margin is wide — the read after the reboot happens one call later.
const GROWN_PAST_SECONDS: f64 = 3.0;

/// How long the waits below may go on for before the test gives up: far above the span the
/// condition needs on any host, so a slow host does not fail a claim about the guest.
const GENEROUS_DEADLINE: Duration = Duration::from_secs(60);

/// How often those waits look again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One emulator over its own engine, with one AMI of the calling account seeded the way a
/// register operation would store it — answered beside the engine and the image id.
fn emulator() -> (
    MultiServiceRouter,
    std::sync::Arc<engine::InMemoryEngine>,
    String,
) {
    let (router, engine) = super::ec2_query::emulator();
    let image_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard.insert_image(ImageSpec {
            name: "al2023-ami-2023.6.20241111.0-kernel-6.1-x86_64".to_string(),
            ..ImageSpec::default()
        })
    };
    (router, engine, image_id)
}

/// Launch one instance and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, image_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("InstanceType", "t3.micro"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
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

/// Run `script` in that instance's guest and answer the lines it wrote, requiring that it ran at
/// all and that it wrote nothing to its standard error.
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
        "case {case:?}: expected the program to succeed, got status {status} with errors {errors:?}",
        status = run.status,
        errors = run.errors
    );
    assert!(
        run.errors.is_empty(),
        "case {case:?}: expected nothing on standard error, got {errors:?}",
        errors = run.errors
    );
    run.output
}

/// The uptime the guest reports now, as the first field of the published file.
fn uptime(engine: &engine::InMemoryEngine, case: &str, instance_id: &str) -> f64 {
    let written = in_guest(engine, case, instance_id, "cut -d' ' -f1 /proc/uptime");
    let line = written
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the program must write the uptime, got nothing"));
    line.trim().parse().unwrap_or_else(|_| {
        panic!("case {case:?}: the uptime must read as a number of seconds, got {line:?}")
    })
}

/// Wait until the guest's own program can run at all — the instance is `running` and its
/// operating system has come up — looking again until it does or [`GENEROUS_DEADLINE`] passes.
async fn until_the_guest_is_up(engine: &engine::InMemoryEngine, case: &str, instance_id: &str) {
    let deadline = std::time::Instant::now() + GENEROUS_DEADLINE;
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
            std::time::Instant::now() < deadline,
            "case {case:?}: the guest of {instance_id} never came up"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The rule: a booted guest publishes its own uptime, and every form a program reads it with
/// reads the same published value.
#[tokio::test]
async fn a_booted_guest_publishes_the_seconds_it_has_been_up() {
    let case = "a booted guest reading its own uptime";
    let (router, engine, image_id) = emulator();
    let instance_id = launch(&router, case, &image_id).await;
    until_the_guest_is_up(&engine, case, &instance_id).await;

    // The whole file: two seconds figures, the idle one never below the uptime.
    let written = in_guest(&engine, case, &instance_id, "cat /proc/uptime");
    let line = written
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the file must have contents, got nothing"));
    let fields: Vec<f64> = line
        .split_whitespace()
        .map(|field| {
            field.parse().unwrap_or_else(|_| {
                panic!("case {case:?}: every field must be a number of seconds, got {line:?}")
            })
        })
        .collect();
    assert_eq!(
        fields.len(),
        2,
        "case {case:?}: expected the uptime and the idle figure, got {line:?}"
    );
    assert!(
        fields[1] >= fields[0],
        "case {case:?}: the idle figure covers every processor, so it is never below the \
         uptime; got {line:?}"
    );
    assert!(
        fields[0] >= 0.0,
        "case {case:?}: an uptime is never negative, got {line:?}"
    );

    // Every other form a program reads it with reads the same published file: the first field
    // cut out of it, and the file read into a shell variable.
    for (form, script) in [
        ("the first field", "cut -d' ' -f1 /proc/uptime"),
        (
            "the file as a variable's value",
            "UP=$(cut -d' ' -f1 /proc/uptime); echo $UP",
        ),
    ] {
        let case = format!("{case}, read as {form}");
        let written = in_guest(&engine, &case, &instance_id, script);
        let read: f64 = written
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a value, got nothing"))
            .trim()
            .parse()
            .unwrap_or_else(|_| {
                panic!("case {case:?}: expected a number of seconds, got {written:?}")
            });
        assert!(
            read >= fields[0],
            "case {case:?}: a later read is never behind an earlier one; got {read} after \
             {earlier}",
            earlier = fields[0]
        );
    }
}

/// The rule, over time and over boots: the uptime GROWS while a boot is up, and a reboot starts
/// it again from that new boot.
#[tokio::test]
async fn the_uptime_grows_and_a_reboot_starts_it_again() {
    let case = "a guest whose uptime grows";
    let (router, engine, image_id) = emulator();
    let instance_id = launch(&router, case, &image_id).await;
    until_the_guest_is_up(&engine, case, &instance_id).await;

    let first = uptime(&engine, case, &instance_id);
    // Wait for the CONDITION the next assertion rests on — the boot having been up for longer
    // than the margin the reboot arm needs — by looking again, never by sleeping for it.
    let deadline = std::time::Instant::now() + GENEROUS_DEADLINE;
    let mut grown = first;
    while grown <= GROWN_PAST_SECONDS {
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the uptime never grew past {GROWN_PAST_SECONDS} seconds; last \
             read {grown}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
        grown = uptime(&engine, case, &instance_id);
    }
    assert!(
        grown > first,
        "case {case:?}: a later read reports a larger uptime; got {grown} after {first}"
    );

    // A reboot restarts the operating system, so the uptime is counted from the NEW boot: it is
    // below the margin the loop above waited past.
    let case = "a guest that has just rebooted";
    let (status, body) = call(
        &router,
        REGION,
        "RebootInstances",
        &[p("InstanceId.1", &instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RebootInstances must succeed, got {status}: {body}"
    );
    // The reboot is queued, so the boot it starts falls due one transition span later; the wait
    // is for the new boot to BE there, and the store's own span is what it is derived from.
    let deadline = std::time::Instant::now() + GENEROUS_DEADLINE;
    loop {
        let boots = {
            let state = ec2::state::service_state(&*engine).expect("the EC2 store");
            let mut store = state.lock();
            store.materialize_due_transitions();
            store
                .instances
                .get(&instance_id)
                .map_or(0, |instance| instance.boots.len())
        };
        if boots >= 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the queued reboot never brought the guest up again (one \
             transition span is {TRANSITION_DELAY:?})"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let after_reboot = uptime(&engine, case, &instance_id);
    assert!(
        after_reboot < GROWN_PAST_SECONDS,
        "case {case:?}: the uptime is counted from the boot that has just happened, so it is \
         below the {GROWN_PAST_SECONDS} seconds the guest had been up for; got {after_reboot}"
    );
}
