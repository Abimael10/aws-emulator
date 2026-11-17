// SPDX-License-Identifier: CC0-1.0

//! Run Command END TO END from the launch that brings a node under management: an Amazon
//! EC2 instance whose IAM instance profile lets the SSM Agent register becomes an `Online`
//! managed node, a send naming that node is SERVED, and the Region's history reports the
//! commands it served.
//!
//! RULE under test, in four parts, each of which the part before it is the premise of:
//!
//! 1. an instance whose profile's role may perform the call the agent registers with is a
//!    managed node once the registration span has passed, and the node inventory report
//!    serves one row for it whose `PingStatus` is `Online`;
//! 2. a send that names a node the Region holds is accepted and mints a command, because
//!    `InvalidInstanceId` is what a node the endpoint does NOT hold is refused with;
//! 3. `ListCommands` reports the commands of the Region that meet the request's criteria,
//!    most recently requested first, and every entry carries the whole `Command`
//!    projection — a member the send left unset reported at its default rather than
//!    omitted;
//! 4. the report serves one page of at most `MaxResults` entries (`CommandMaxResults`,
//!    range 1..50), with a `NextToken` exactly when entries remain past the page and none
//!    when they do not, and that token resumes the same result where the page stopped.
//!
//! Nothing here is seeded into this service's own state: the role and its instance profile
//! are written through IAM's own API, the instance through Amazon EC2's `RunInstances`, and
//! every Systems Manager call goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body. The sibling suites that
//! seed the node inventory directly (`run_command`, `command_history`) pin the same reads
//! against a store they wrote; this one pins that the components really hand the node to
//! each other. The one thing it reaches a store for is the CLOCK — the instance's launch
//! instant, moved back past the registration span the way the sibling guest-run harness
//! moves it ([`crate::ec2_instance_managed_nodes::Emulator::launched_ago`]) — so no case
//! here waits for a span to pass or depends on how fast the host runs.
//!
//! The pagination rows differ along what the rule generalises over: a page size below the
//! entry count, one short of it, at it, past it, the modelled maximum, and a request naming
//! none. Each row walks the tokens to the end, so "the token is present" is checked
//! together with what the token then serves.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::commands;

use crate::command_history::listed_ids;
use crate::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use crate::run_command::{assert_complete_projection, unconfigured_members};

/// The document every send below runs: one Systems Manager publishes, whose steps run on a
/// Linux node, and the one a client reaches for first.
const DOCUMENT: &str = "AWS-RunShellScript";

/// The version a command reports for a send that named none: "the default version"
/// (`SendCommandRequest$DocumentVersion`, `Command$DocumentVersion`).
const DEFAULT_VERSION: &str = "$DEFAULT";

/// The command the sends supply, as the `commands` parameter the document declares.
const SCRIPT: &str = "echo run-command";

/// How many commands the fixture sends to its node.
///
/// More than the three the recorded fixture sent, so a page size at the count and one past
/// it are different rows below.
const COMMANDS: usize = 4;

/// The largest page `ListCommands` serves, taken from the crate's own model-derived bound
/// (`CommandMaxResults`, range 1..50) rather than restated here — which is also the page a
/// request naming no `MaxResults` is served up to.
const MAX_PAGE: i32 = commands::MAX_COMMAND_PAGE_SIZE;

/// The spacing the fixture dates its sends an hour apart by, so "most recently requested
/// first" is an order over instants this suite fixes rather than one that depends on how
/// much of the host's clock resolution passed between two sends.
const HOUR: Duration = Duration::from_secs(3600);

/// One node, its commands, and the emulator they live in.
struct Fixture {
    emulator: Emulator,
    /// The node every command of [`Fixture::sent`] was sent to.
    node: String,
    /// A second registered node, holding one command of its own, so a listing scoped by a
    /// node id is proved to select that node's commands and no other's.
    other_node: String,
    /// `(command id, comment)` for each command sent to [`Fixture::node`], in the order the
    /// sends were made.
    sent: Vec<(String, String)>,
    /// The id of [`Fixture::other_node`]'s one command.
    other_command: String,
}

impl Fixture {
    /// The ids of the node's commands in the order a history reports them: most recently
    /// requested first, which is the reverse of the order they were sent in.
    fn newest_first(&self) -> Vec<String> {
        self.sent.iter().rev().map(|(id, _)| id.clone()).collect()
    }

    /// One `ListCommands` call that must be served, as the body it answered with.
    async fn list(&self, case: &str, body: &Value) -> Value {
        let (status, parsed) = self.emulator.ssm(REGION, "ListCommands", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListCommands served with HTTP 200, got {status} with \
             {parsed:?}"
        );
        parsed
    }
}

/// Send one command to `node` and return `(command id, comment)`, asserting the send was
/// SERVED — part 2 of the rule.
async fn send(
    emulator: &Emulator,
    case: &str,
    node: &str,
    comment: &str,
) -> (String, String) {
    let (status, parsed) = emulator
        .ssm(
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": DOCUMENT,
                "InstanceIds": [node],
                "Parameters": {"commands": [SCRIPT]},
                "Comment": comment,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a send naming the registered node {node} to be served with \
         HTTP 200, got {status} with {parsed:?}"
    );
    let command = &parsed["Command"];
    let id = command["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a CommandId, got {parsed:?}"))
        .to_string();
    assert_eq!(
        command["InstanceIds"],
        json!([node]),
        "case {case:?}: expected the command to name the node it was sent to, got {parsed:?}"
    );
    (id, comment.to_string())
}

/// Bring two nodes under management through Amazon EC2 and IAM — part 1 of the rule — then
/// send the fixture's commands to them.
async fn fixture() -> Fixture {
    let case = "the Run Command path";
    let emulator = Emulator::new();
    // A role Amazon EC2 may act as carrying the AWS-managed policy a client attaches to make
    // an instance a managed node.
    let profile = emulator
        .profile(
            case,
            "run-command-node-role",
            &Grant::Attached(SSM_CORE_POLICY_ARN),
        )
        .await;
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let node = emulator.launch(case, &image, Some(&profile)).await;
    let other_node = emulator.launch(case, &image, Some(&profile)).await;
    // The registration is owed a span after the launch, so move each launch back past it:
    // what this suite is about is what a registered node then does, not the span itself
    // (which `ec2_managed_node_registration` is the table for).
    for instance in [&node, &other_node] {
        emulator.launched_ago(instance, launched_long_enough_ago());
    }

    let reported = emulator.nodes(case, REGION).await;
    for instance in [&node, &other_node] {
        let row = reported.get(instance).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected {instance} reported as a managed node, got {:?}",
                reported.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(
            row["PingStatus"].as_str(),
            Some("Online"),
            "case {case:?}: expected a registered node to report PingStatus Online, got {row:?}"
        );
    }

    let anchor = SystemTime::now();
    let mut sent = Vec::new();
    for index in 0..COMMANDS {
        let (id, comment) = send(&emulator, case, &node, &format!("command {index}")).await;
        // The send is the front door's; only the INSTANT it is dated at is written here, so
        // the history under test is an order over instants an hour apart.
        date_command(&emulator, &id, anchor - HOUR * (COMMANDS - index) as u32);
        sent.push((id, comment));
    }
    let (other_command, _) = send(&emulator, case, &other_node, "the other node's command").await;

    Fixture {
        emulator,
        node,
        other_node,
        sent,
        other_command,
    }
}

/// Move the stored command `id`'s requested instant to `at`.
///
/// The CLOCK is the one thing this suite reaches a store for — the same seam the sibling
/// harness moves an instance's launch instant through — because the rule under test is an
/// order over the instants commands were requested at, and two sends made in the same
/// microsecond would leave that order to the tie-break rather than to the rule.
fn date_command(emulator: &Emulator, id: &str, at: SystemTime) {
    let state = emulator.ssm_state();
    let mut store = state.lock();
    let record = store
        .commands
        .get_mut(&(REGION.to_string(), id.to_string()))
        .unwrap_or_else(|| panic!("the send persisted command {id:?} in {REGION:?}"));
    record.requested_at = at;
}

/// The whole path of the rule, on ONE fixture: a launch brings a node under management
/// (part 1, in [`fixture`]), the node takes commands (part 2), and the Region's history
/// reports them in full (part 3) and one page at a time (part 4).
///
/// The parts are checked in one test because they share one premise: a node only a launch,
/// an instance profile and a registration can produce, and nothing below needs a second one.
#[tokio::test]
async fn the_run_command_path_from_a_registered_node_to_its_paged_history() {
    let fixture = fixture().await;
    the_history_reports_the_nodes_commands_in_full(&fixture).await;
    a_page_limited_history_hands_back_a_next_token_until_the_last_page(&fixture).await;
}

/// Part 3: one entry per command sent to the node, most recently requested first, each
/// carrying the whole projection — and no command another node took.
async fn the_history_reports_the_nodes_commands_in_full(fixture: &Fixture) {
    let case = "scoped by the node id";
    let parsed = fixture
        .list(case, &json!({"InstanceId": fixture.node}))
        .await;
    let listed = listed_ids(case, &parsed);
    assert_eq!(
        listed,
        fixture.newest_first(),
        "case {case:?}: expected the node's commands newest requested first, got {listed:?}"
    );
    assert!(
        !listed.contains(&fixture.other_command),
        "case {case:?}: expected no command of another node, got {listed:?}"
    );

    let entries = parsed["Commands"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Commands list, got {parsed:?}"));

    // The order the entries are reported in is the order of what they report: each entry's
    // `RequestedDateTime` is no later than the one before it.
    let instants: Vec<f64> = entries
        .iter()
        .map(|entry| {
            entry["RequestedDateTime"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: expected RequestedDateTime, got {entry:?}")
            })
        })
        .collect();
    assert!(
        instants.windows(2).all(|pair| pair[0] >= pair[1]),
        "case {case:?}: expected RequestedDateTime to fall across the page, got {instants:?}"
    );

    // Every entry reports the whole `Command` projection, with what the send configured
    // echoed and what it left unset at that member's default.
    let comments: BTreeMap<&str, &str> = fixture
        .sent
        .iter()
        .map(|(id, comment)| (id.as_str(), comment.as_str()))
        .collect();
    for entry in entries {
        let id = entry["CommandId"].as_str().unwrap_or_default();
        let case = format!("the entry for {id}");
        assert_complete_projection(&case, entry);
        assert_eq!(
            entry["DocumentName"].as_str(),
            Some(DOCUMENT),
            "case {case:?}: expected the document the send ran, got {entry:?}"
        );
        assert_eq!(
            entry["DocumentVersion"].as_str(),
            Some(DEFAULT_VERSION),
            "case {case:?}: expected the default version for a send that named none, got \
             {entry:?}"
        );
        assert_eq!(
            entry["InstanceIds"],
            json!([fixture.node]),
            "case {case:?}: expected the node the command was sent to, got {entry:?}"
        );
        assert_eq!(
            entry["Parameters"],
            json!({"commands": [SCRIPT]}),
            "case {case:?}: expected the parameters the send supplied, got {entry:?}"
        );
        assert_eq!(
            entry["Comment"].as_str(),
            comments.get(id).copied(),
            "case {case:?}: expected the comment this command's own send carried, got {entry:?}"
        );
        // `Comment` is the one member of the set below this fixture's sends configured, so
        // it is checked against the request just above instead of against its default.
        for (member, default) in unconfigured_members(REGION)
            .into_iter()
            .filter(|(member, _)| *member != "Comment")
        {
            assert_eq!(
                entry[member], default,
                "case {case:?}: expected {member} reported as {default}, got {entry:?}"
            );
        }
    }

    // The same read scoped by the other node selects that node's one command and nothing
    // the first node took.
    let case = "scoped by the other node id";
    let parsed = fixture
        .list(case, &json!({"InstanceId": fixture.other_node}))
        .await;
    assert_eq!(
        listed_ids(case, &parsed),
        vec![fixture.other_command.clone()],
        "case {case:?}: expected the other node's one command, got {parsed:?}"
    );
}

/// Part 4: a page-limited history hands back a `NextToken` while entries remain, none once
/// they do not, and walking the tokens reports every command once in the history's own
/// order.
async fn a_page_limited_history_hands_back_a_next_token_until_the_last_page(fixture: &Fixture) {
    let whole = fixture.newest_first();

    // One row per page size: below the entry count, one short of it, at it, past it, the
    // modelled maximum, and a request naming no `MaxResults` at all — which is served up to
    // that maximum.
    let rows: Vec<(&str, Option<i32>)> = vec![
        ("one entry a page", Some(1)),
        ("one short of the history", Some((COMMANDS - 1) as i32)),
        ("exactly the history", Some(COMMANDS as i32)),
        ("more than the history holds", Some(COMMANDS as i32 + 1)),
        ("the modelled maximum", Some(MAX_PAGE)),
        ("no page size at all", None),
    ];
    for (case, max_results) in rows {
        let size = max_results.unwrap_or(MAX_PAGE) as usize;
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut body = json!({"InstanceId": fixture.node});
            if let Some(max_results) = max_results {
                body["MaxResults"] = json!(max_results);
            }
            if let Some(token) = token.as_deref() {
                body["NextToken"] = json!(token);
            }
            let parsed = fixture.list(case, &body).await;
            let page = listed_ids(case, &parsed);
            let remaining = whole.len() - walked.len();
            assert_eq!(
                page.len(),
                remaining.min(size),
                "case {case:?}: expected a page of {} entries, got {page:?}",
                remaining.min(size)
            );
            token = parsed["NextToken"].as_str().map(str::to_string);
            // The token is present exactly when entries remain past this page.
            assert_eq!(
                token.is_some(),
                remaining > size,
                "case {case:?}: expected NextToken present={} after {} of {} entries, got {:?}",
                remaining > size,
                walked.len() + page.len(),
                whole.len(),
                parsed["NextToken"]
            );
            walked.extend(page);
            pages += 1;
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, whole,
            "case {case:?}: expected every command once, newest first, got {walked:?}"
        );
        assert_eq!(
            pages,
            whole.len().div_ceil(size),
            "case {case:?}: expected {} pages, got {pages}",
            whole.len().div_ceil(size)
        );
    }
}
