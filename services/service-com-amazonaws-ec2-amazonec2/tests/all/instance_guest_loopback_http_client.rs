// SPDX-License-Identifier: CC0-1.0

//! The HTTP CLIENT in an instance's guest reaching a server of the SAME machine, driven over
//! the wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness, with the probe's answer observed where a guest's own output is
//! observable (the instance console: "the exact console output that would normally be displayed
//! on a physical monitor attached to a computer", `GetConsoleOutput`).
//!
//! RULE: a program of a guest that fetches an address of its OWN machine's loopback interface
//! is answered by the server THIS guest is listening with on that port, out of this machine's
//! own storage as the commands before it left it — so what the client writes is the document
//! that server served, a port nothing of the machine is listening on is a connection that could
//! not be made, and what the client reports it could not do is its own diagnostic and its own
//! non-zero status rather than a document.
//!
//! The rows differ along what the rule generalises over: how the address names the machine (the
//! loopback address, the name every machine resolves to it), where the client writes what it
//! fetched (its own output, a file of the machine), what it is asked to report (the document,
//! the status the server answered), which status the server answers (the document it holds, a
//! path it holds nothing at, a method it serves nothing for), and the failures that are the
//! client's own (an error status it was asked to fail on, a port nothing listens on, a form it
//! does not perform) — beside an address of another host, which nothing of this guest reaches.

use std::time::{Duration, Instant};

use base64::Engine as _;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, ImageSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements, member};

/// The Region every call here is signed for; it only selects the ec2Query envelope.
const REGION: &str = "us-west-2";

/// The document the guest's own program serves, which is what a probe that reached the server
/// writes back.
const SERVED: &str = "served-by-the-guest-4b19";

/// The port the guest's own program listens on.
const PORT: &str = "8080";

/// A port nothing of the guest is listening on.
const CLOSED_PORT: &str = "9191";

/// How long a row waits for the guest to come up and its program to run, and how often it looks
/// again. Generous on purpose — many times the span a launch owes its boot — because the
/// deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

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

/// The user data of one row: the program that serves a document on a port of this machine, then
/// makes the row's own probe.
fn user_data(probe: &str) -> String {
    format!(
        "#!/bin/bash\n\
         mkdir -p /srv/pages\n\
         echo {SERVED} > /srv/pages/probe.txt\n\
         python3 -m http.server {PORT} --directory /srv/pages &\n\
         {probe}\n"
    )
}

/// Launch one instance whose user data is `user_data`, answering its id.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    user_data: &str,
) -> String {
    let params = vec![
        p("ImageId", image_id),
        p("InstanceType", "t3.micro"),
        p("MinCount", "1"),
        p("MaxCount", "1"),
        p(
            "UserData",
            base64::engine::general_purpose::STANDARD.encode(user_data),
        ),
    ];
    let (status, body) = call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status}: {body}"
    );
    elements(&body, "instanceId")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// One instance's console as text — empty while its guest has written nothing.
async fn console(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
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

/// Look at the console until it carries a line the guest's own probe wrote — one beginning with
/// the row's label — and answer that line, up to a deadline far longer than the span a launch
/// owes its boot.
async fn probed(router: &MultiServiceRouter, case: &str, instance_id: &str, label: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let text = console(router, case, instance_id).await;
        if let Some(line) = text
            .lines()
            .find(|line| line.starts_with(label))
            .map(str::to_string)
        {
            return line;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the guest's probe to write a {label:?} line, got {text:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row: the probe the guest's program makes, and the line it must write.
struct Row {
    case: &'static str,
    /// The label the probe's line begins with, so it can be told from the guest's own log.
    label: &'static str,
    /// The command the program makes the probe with, which ends by writing one labelled line to
    /// the console device.
    probe: String,
    /// The whole line the console must then carry.
    expected: String,
}

/// RULE: a program of the guest fetching an address of its own machine's loopback interface is
/// answered by the server this guest is listening with there, out of this machine's own storage.
#[tokio::test]
async fn a_guest_program_fetches_a_server_of_its_own_machine() {
    let rows = [
        // The document itself, by the loopback ADDRESS.
        Row {
            case: "the document the server serves, by the loopback address",
            label: "BODY:",
            probe: format!(
                "echo \"BODY: $(curl -s http://127.0.0.1:{PORT}/probe.txt)\" > /dev/console"
            ),
            expected: format!("BODY: {SERVED}"),
        },
        // The same server, by the NAME every machine resolves to that interface.
        Row {
            case: "the same server, by the loopback name",
            label: "BYNAME:",
            probe: format!(
                "echo \"BYNAME: $(curl -s http://localhost:{PORT}/probe.txt)\" > /dev/console"
            ),
            expected: format!("BYNAME: {SERVED}"),
        },
        // What it fetched, written into a FILE of the machine and read back from there.
        Row {
            case: "what it fetched, saved in a file of the machine",
            label: "SAVED:",
            probe: format!(
                "curl -s -o /tmp/body http://127.0.0.1:{PORT}/probe.txt\n\
                 echo \"SAVED: $(cat /tmp/body)\" > /dev/console"
            ),
            expected: format!("SAVED: {SERVED}"),
        },
        // The STATUS the server answered, which is what the write-out format reports.
        Row {
            case: "the status the server answered for a document it holds",
            label: "CODE:",
            probe: format!(
                "echo \"CODE: $(curl -s -o /dev/null -w '%{{http_code}}' \
                 http://127.0.0.1:{PORT}/probe.txt)\" > /dev/console"
            ),
            expected: "CODE: 200".to_string(),
        },
        Row {
            case: "the status it answers for a path the served directory holds nothing at",
            label: "MISSING:",
            probe: format!(
                "echo \"MISSING: $(curl -s -o /dev/null -w '%{{http_code}}' \
                 http://127.0.0.1:{PORT}/not-there.txt)\" > /dev/console"
            ),
            expected: "MISSING: 404".to_string(),
        },
        // An error status the invocation asked to FAIL on is the client's own failure, and its
        // status says so.
        Row {
            case: "an error status the invocation asked to fail on",
            label: "FAILED:",
            probe: format!(
                "curl -sf http://127.0.0.1:{PORT}/not-there.txt\n\
                 echo \"FAILED: $?\" > /dev/console"
            ),
            expected: "FAILED: 22".to_string(),
        },
        // A port nothing of the machine is listening on: a connection that could not be made.
        Row {
            case: "a port nothing of the machine is listening on",
            label: "REFUSED:",
            probe: format!(
                "curl -s http://127.0.0.1:{CLOSED_PORT}/probe.txt\n\
                 echo \"REFUSED: $?\" > /dev/console"
            ),
            expected: "REFUSED: 7".to_string(),
        },
        // An address of ANOTHER host is not a server of this machine, and nothing of this
        // guest reaches it.
        Row {
            case: "an address of another host",
            label: "ELSEWHERE:",
            probe: "curl -s http://example.invalid/probe.txt\n\
                 echo \"ELSEWHERE: $?\" > /dev/console"
                .to_string(),
            expected: "ELSEWHERE: 6".to_string(),
        },
        // A request carrying a BODY asks for a method of its own, which a static server of the
        // guest answers for no document: the status it answered is what the client reports,
        // and no document comes back.
        Row {
            case: "a request carrying a body, which asks for a method the server serves nothing for",
            label: "POSTED:",
            probe: format!(
                "echo \"POSTED: $(curl -s -w '%{{http_code}}' -d name=value \
                 http://127.0.0.1:{PORT}/probe.txt)\" > /dev/console"
            ),
            expected: "POSTED: 405".to_string(),
        },
        // A form this client does not perform is reported as a transfer that was not made,
        // with nothing on standard output.
        Row {
            case: "a headers-only request, which this client does not perform",
            label: "HEADERS:",
            probe: format!(
                "curl -s -I http://127.0.0.1:{PORT}/probe.txt\n\
                 echo \"HEADERS: $?\" > /dev/console"
            ),
            expected: "HEADERS: 2".to_string(),
        },
    ];

    let (router, image_id) = emulator();
    for row in &rows {
        let instance_id = launch(&router, row.case, &image_id, &user_data(&row.probe)).await;
        let line = probed(&router, row.case, &instance_id, row.label).await;
        assert_eq!(
            line.trim_end(),
            row.expected,
            "case {:?}: expected the console to carry {:?}, got {line:?}",
            row.case,
            row.expected
        );
    }
}

/// RULE: the module's own default directory is the one the session is IN, so a program that
/// changes directory and then starts the server serves that directory — and a probe of its own
/// machine fetches the document from there.
#[tokio::test]
async fn a_server_started_after_a_change_of_directory_serves_that_directory() {
    let case = "the working directory";
    let (router, image_id) = emulator();
    let instance_id = launch(
        &router,
        case,
        &image_id,
        &user_data(
            "cd /srv/pages\n\
             python3 -m http.server 8099 &\n\
             echo \"CHDIR: $(curl -s http://127.0.0.1:8099/probe.txt)\" > /dev/console",
        ),
    )
    .await;

    let line = probed(&router, case, &instance_id, "CHDIR:").await;
    assert_eq!(
        line.trim_end(),
        format!("CHDIR: {SERVED}"),
        "case {case:?}: expected the document of the directory the program was in, got {line:?}"
    );
}

/// RULE: what the client writes is the document the server serves out of the machine's storage
/// AS IT IS NOW, so a document a later command of the same program wrote is what a later probe
/// fetches — the round trip a single fetch cannot establish on its own.
#[tokio::test]
async fn the_document_a_later_command_wrote_is_what_the_next_probe_fetches() {
    let case = "the live tree";
    let (router, image_id) = emulator();
    let instance_id = launch(
        &router,
        case,
        &image_id,
        &user_data(&format!(
            "echo \"FIRST: $(curl -s http://127.0.0.1:{PORT}/probe.txt)\" > /dev/console\n\
             echo replaced-{SERVED} > /srv/pages/probe.txt\n\
             echo \"SECOND: $(curl -s http://127.0.0.1:{PORT}/probe.txt)\" > /dev/console"
        )),
    )
    .await;

    let first = probed(&router, case, &instance_id, "FIRST:").await;
    assert_eq!(
        first.trim_end(),
        format!("FIRST: {SERVED}"),
        "case {case:?}: expected the document the program first wrote, got {first:?}"
    );
    let second = probed(&router, case, &instance_id, "SECOND:").await;
    assert_eq!(
        second.trim_end(),
        format!("SECOND: replaced-{SERVED}"),
        "case {case:?}: expected the document the program wrote next, got {second:?}"
    );
}
