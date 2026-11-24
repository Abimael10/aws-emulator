// SPDX-License-Identifier: CC0-1.0

//! What a script run in an instance's GUEST reads out of what its own programs wrote: the
//! figure a basic regular expression selects out of a program's report, and the instant the
//! machine's clock states — driven through the one seam a program of a guest runs through
//! (`run_in_instance_guest`, which is what the agent inside a guest delivers a script to).
//!
//! THE RULE: a guest's own text tools and clock are the ones the image ships, so a line a
//! client wrote for them reads there what it reads on the instance. Two of those forms are
//! pinned here, because every marker a Run Command script emits is built out of them:
//!
//! * a BASIC regular expression gives the escaped spellings `\+`, `\?`, `\|`, `\(`, `\)`,
//!   `\{`, `\}` the meaning an extended expression gives the bare character ("in basic regular
//!   expressions the meta-characters `?`, `+`, `{`, `|`, `(`, and `)` lose their special
//!   meaning; instead use the backslashed versions", GNU grep manual), so
//!   `grep -o '[0-9]\+ bytes'` selects the figure out of a report — while the bare character
//!   stays a literal;
//! * `date` writes the instant it ran at in the form the invocation asks for (`+FORMAT`), which
//!   is how a script states the window its work happened in.
//!
//! The rows differ along what each rule generalises over: the escape the pattern carries and
//! what it is read out of, and the format the clock is asked for.

use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::element;
use super::instance_metric_publication::{ec2_ok, emulator, p};

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// How long the poll below keeps asking before giving up: far above the span the boot it waits
/// for is owed in.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How often it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Launch one instance and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str) -> String {
    let body = ec2_ok(
        router,
        case,
        "RunInstances",
        &[
            p("ImageId", AMI),
            p("InstanceType", "t3.micro"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
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

/// Run `script` in that guest and answer the lines it wrote.
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
        "case {case:?}: expected the script to succeed, got status {} with errors {:?}",
        run.status, run.errors
    );
    run.output
}

/// THE RULE, over a table of the escapes a basic expression carries.
#[tokio::test]
async fn a_guest_selects_a_figure_out_of_what_its_programs_wrote() {
    let (router, engine) = emulator();
    let case = "a booted guest reading its own program's report";
    let instance_id = launch(&router, case).await;
    until_the_guest_is_up(&engine, case, &instance_id).await;

    // (case, the pattern, the line it is read out of, what it must select)
    let rows: [(&str, &str, &str, &str); 5] = [
        (
            "the digits of a figure, quantified with the escaped plus",
            r"[0-9]\+ bytes",
            "16777216 bytes (17 MB, 16 MiB) copied, 0.04 s, 389 MB/s",
            "16777216 bytes",
        ),
        (
            "an alternation inside an escaped group",
            r"\(copied\|written\)",
            "16777216 bytes copied",
            "copied",
        ),
        (
            "an escaped repetition",
            r"i-[0-9a-f]\{17\}",
            "the host is i-0123456789abcdef0 today",
            "i-0123456789abcdef0",
        ),
        (
            "an escaped question mark",
            r"colou\?r",
            "the colour of it",
            "colour",
        ),
        (
            // The other half of the same rule: the BARE character is a literal.
            "a bare plus, which a basic expression holds as a literal",
            "16+0",
            "16+0 records out",
            "16+0",
        ),
    ];
    for (row, pattern, line, selected) in rows {
        let script = format!("echo '{line}' | grep -o '{pattern}'");
        let written = in_guest(&engine, row, &instance_id, &script);
        assert_eq!(
            written.first().map(String::as_str),
            Some(selected),
            "case {row:?}: expected {selected:?} selected out of {line:?}, got {written:?}"
        );
    }

    // ... and the machine's clock states the instant the script ran, in the form it asked for.
    let written = in_guest(
        &engine,
        case,
        &instance_id,
        "echo \"AT $(date +%s)\"\necho \"ISO $(date -u +%Y-%m-%dT%H:%M:%SZ)\"",
    );
    let at = written
        .iter()
        .find_map(|line| line.strip_prefix("AT "))
        .unwrap_or_default()
        .trim()
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("case {case:?}: expected the epoch seconds, got {written:?}"));
    // The clock the guest reads is this emulator's own, so the instant it states is one this
    // test's own clock brackets rather than a value written into the suite.
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or_default();
    assert!(
        at > 0 && (now - at).abs() < 600,
        "case {case:?}: expected the instant the script ran, got {at} against {now}"
    );
    let iso = written
        .iter()
        .find_map(|line| line.strip_prefix("ISO "))
        .unwrap_or_default()
        .trim()
        .to_string();
    assert!(
        iso.len() == 20 && iso.ends_with('Z') && iso.contains('T'),
        "case {case:?}: expected the instant in the form asked for, got {iso:?}"
    );
}
