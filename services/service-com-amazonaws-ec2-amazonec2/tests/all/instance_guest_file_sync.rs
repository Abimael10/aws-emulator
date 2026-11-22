// SPDX-License-Identifier: CC0-1.0

//! The file utilities a program running in an instance's GUEST can count on, over the one that
//! FLUSHES what was written — driven over the wire the service actually speaks: `ec2Query`, and
//! observed where a guest's own output is observable, on that instance's console
//! ("the instance console output displays the exact console output that would normally be
//! displayed on a physical monitor attached to a computer", `GetConsoleOutput`).
//!
//! RULE: every utility the guest's shell performs over that machine's storage is a command the
//! guest PROVIDES — a name it performs but does not provide would be looked up as a program and
//! reported as one the shell could not find, which under `set -e` ends the program at that line.
//! `sync` is such a utility: "sync() causes all pending modifications to filesystem metadata and
//! cached file data to be written", and a machine whose storage takes a write the instant it is
//! made has nothing pending — so it writes nothing, exits successfully, and the lines after it
//! run.
//!
//! The rows differ along what the rule generalises over: the FORM the invocation takes (alone
//! between a write and a read-back, naming the file it flushed, with its exit status reported,
//! and as a stage of an `&&` chain), plus the negative control — a name the guest really has no
//! program for, which must end the program exactly where the rule says `sync` does not.

use std::time::Duration;

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, member};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The file the guest's program writes, reads back and flushes.
const SENTINEL_PATH: &str = "/var/lib/sim2real/sentinel";

/// What it writes there — a value the console line is then matched on, so the line can only
/// have come from reading the file back.
const SENTINEL: &str = "sentinel-4b19";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// How long a wait below may go on for before the test gives up: far above the span this store
/// owes a guest its boot on any host, so a slow host does not fail a claim about the guest.
const GENEROUS_DEADLINE: Duration = Duration::from_secs(60);

/// How often that wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The text every booted guest's console carries, whatever its program did: the init system's
/// own lines. Its presence is the CONDITION "this guest has booted and its program has run",
/// which is what the assertions below wait for rather than waiting a span.
const INIT_SYSTEM_LINES: &str = "cloud-init[";

/// One emulator over its own engine with one AMI of the calling account seeded the way a
/// register operation would store it, answered beside its image id.
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

/// Launch one instance whose user data is `program`, and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, image_id: &str, program: &str) -> String {
    let (status, body) = api(
        router,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("InstanceType", "t3.micro"),
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

/// One instance's console as text — empty for a guest that has written nothing.
async fn console(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = api(
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

/// That instance's console once its guest has BOOTED: the text, waited for by looking again
/// until it carries the init system's own lines or [`GENEROUS_DEADLINE`] passes.
async fn booted_console(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let deadline = std::time::Instant::now() + GENEROUS_DEADLINE;
    loop {
        let text = console(router, case, instance_id).await;
        if text.contains(INIT_SYSTEM_LINES) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the guest of {instance_id} never booted; its console is {text:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table: the case, the lines the guest's program runs after writing the
/// sentinel, and the line the console must then carry — or `None` for a program that must not
/// get as far as writing one.
struct Row {
    case: &'static str,
    /// The lines between writing the sentinel and the marker line, as the program carries them.
    between: String,
    /// The marker the console must carry, or `None` when the program must stop before it.
    marker: Option<String>,
}

/// The program one row launches: write the sentinel, then the row's own lines, then the marker
/// line that reads the sentinel back — all under `set -e`, so a line that fails ends it.
fn program(row: &Row) -> String {
    format!(
        "#!/bin/sh\n\
         set -e\n\
         mkdir -p /var/lib/sim2real\n\
         printf '%s\\n' {SENTINEL} > {SENTINEL_PATH}\n\
         {between}\
         echo \"GUEST-SENTINEL $(cat {SENTINEL_PATH})\" > /dev/console\n",
        between = row.between,
    )
}

/// The rule: a guest program that flushes what it wrote runs on past the flush, and the value it
/// wrote is still there to read back.
#[tokio::test]
async fn a_guest_program_can_flush_what_it_wrote_and_read_it_back() {
    let marker = format!("GUEST-SENTINEL {SENTINEL}");
    let rows: Vec<Row> = vec![
        Row {
            case: "sync alone, between the write and the read-back",
            between: "sync\n".to_string(),
            marker: Some(marker.clone()),
        },
        Row {
            case: "sync naming the file it flushed",
            between: format!("sync {SENTINEL_PATH}\n"),
            marker: Some(marker.clone()),
        },
        Row {
            case: "sync between two writes, the later one still visible after it",
            between: format!(
                "sync\nprintf '%s\\n' {SENTINEL} > {SENTINEL_PATH}\nsync\n"
            ),
            marker: Some(marker.clone()),
        },
        Row {
            case: "sync as a stage of an && chain",
            between: format!("sync && printf '%s\\n' {SENTINEL} > {SENTINEL_PATH}\n"),
            marker: Some(marker.clone()),
        },
        Row {
            // The control: a name the guest really provides no program for ends the program
            // under `set -e`, which is exactly what `sync` must NOT do.
            case: "a name the guest has no program for at all",
            between: "definitely-not-a-program\n".to_string(),
            marker: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, image_id) = emulator();
        let instance_id = launch(&router, case, &image_id, &program(row)).await;
        let text = booted_console(&router, case, &instance_id).await;
        match row.marker.as_deref() {
            Some(expected) => {
                assert!(
                    text.lines().any(|line| line == expected),
                    "case {case:?}: expected the console to carry {expected:?}, got {text:?}"
                );
            }
            None => {
                assert!(
                    !text.lines().any(|line| line.starts_with("GUEST-SENTINEL ")),
                    "case {case:?}: expected the program to end before the marker, got {text:?}"
                );
            }
        }
        // Whichever row it is, the guest booted and its init system ran the program: an empty
        // console would make the assertion above vacuous — which is the very condition
        // [`booted_console`] waited for.
        assert!(
            text.contains(INIT_SYSTEM_LINES),
            "case {case:?}: expected the init system's own lines on the console, got {text:?}"
        );
    }
}

/// The rule, on the status: the flush SUCCEEDS, which is what lets the lines after it run —
/// observed through a conditional on the flush itself, which only takes its `then` branch for a
/// command that answered success.
#[tokio::test]
async fn the_flush_answers_the_status_of_a_utility_that_succeeded() {
    let case = "a program that branches on the flush's own status";
    let (router, image_id) = emulator();
    let row = Row {
        case,
        between: "if sync; then echo \"GUEST-FLUSHED\" > /dev/console; fi\n".to_string(),
        marker: Some(format!("GUEST-SENTINEL {SENTINEL}")),
    };
    let instance_id = launch(&router, case, &image_id, &program(&row)).await;
    let text = booted_console(&router, case, &instance_id).await;
    assert!(
        text.lines().any(|line| line == "GUEST-FLUSHED"),
        "case {case:?}: expected the flush to answer success, got {text:?}"
    );
    let marker = format!("GUEST-SENTINEL {SENTINEL}");
    assert!(
        text.lines().any(|line| line == marker),
        "case {case:?}: expected the program to run on past the flush, got {text:?}"
    );
}
