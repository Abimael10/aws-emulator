// SPDX-License-Identifier: CC0-1.0

//! WHICH BUS a running handler's execution-role grant covers: the `Resource` element of the
//! statement that allows `events:PutEvents` is matched against the ARN of the event bus the
//! ENTRY addressed, so one grant admits some buses of the account and refuses the rest.
//!
//! RULE: a publication is the action `events:PutEvents` on the event bus the entry names, so
//! it is allowed only where a statement in force on the calling principal names BOTH that
//! action and a resource covering that bus's own ARN — matched the way IAM matches a
//! `Resource` element, case-sensitively with `*` standing for any run of characters and `?`
//! for exactly one — and is otherwise refused with `AccessDeniedException` naming the bus the
//! entry addressed, with nothing of the request published. The principal is the same either
//! way: a call an AWS SDK makes from inside a running function is signed as a session of that
//! function's execution role, so the identity a refusal blames is
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
//!
//! `PutEventsRequestEntry$EventBusName` is "the name or ARN of the event bus to receive the
//! event … If you omit this, the default event bus is used"
//! (`smithy_query(query="shape", shape="PutEventsRequestEntry")`), which is what makes the
//! resource of one request's authorization a property of the ENTRY; `PutEvents` declares no
//! authorization error of its own (`smithy_query(query="operation", operation="PutEvents")`
//! names `InternalException` only), so the refusal is the AWS-common `AccessDeniedException`,
//! and with neither an identity-based nor a resource-based allow a request is denied (IAM User
//! Guide, *Policy evaluation logic*).
//!
//! The rows differ along what the rule generalises over: the FORM of the statement's
//! `Resource` element (one bus's ARN, the other's, a list carrying both, the bare wildcard,
//! a run wildcard over every bus of the account, a run wildcard over a bus-name prefix, a
//! single-character wildcard, another service's resource, and no statement at all). Every
//! row drives the SAME handler against BOTH buses, so each row states which buses that one
//! grant covers rather than only whether it covers anything, and each half is read twice:
//! what the SDK gave the handler, and which events reached the queue the buses' rules target.
//!
//! The harness — the five services on one engine, the deployment package, the handler source
//! and the ARN helpers — is the one `handler_execution_role_publication` owns; that sibling
//! pins that a handler's publication is judged on its execution role's policies at all, with
//! the grant over every resource, and this suite is the RESOURCE dimension of the same
//! decision, over custom buses, which is where a least-privilege publisher's role is scoped.

use std::time::Instant;

use serde_json::{json, Value};

use crate::handler_execution_role_publication::{
    bus_arn, implicit_deny_message, session_arn, Emulator, ACCESS_DENIED, BUDGET, LOOK_AGAIN,
    PUBLISHER_SOURCE, PUT_EVENTS_ACTION, REGION,
};

/// The execution role the function runs as, the inline policy each row rewrites on it, and
/// the function's own name — the session a handler's calls are made under is named for both
/// the role and the function.
const ROLE_NAME: &str = "handler-bus-scope-publisher";
const INLINE_POLICY: &str = "publish";
const FUNCTION_NAME: &str = "publishes-to-either-bus";

/// The two custom buses of this account the handler publishes onto. Their names share a
/// prefix and differ from their first distinguishing character on, so a prefix wildcard and a
/// single-character wildcard can each cover one of them and not the other.
const FIRST_BUS: &str = "handler-scope-alpha";
const SECOND_BUS: &str = "handler-scope-beta";

/// The rule each bus carries, and the shared target id and queue they both deliver to — the
/// place a publication this service ACCEPTED is read back from.
const FIRST_RULE: &str = "handler-scope-alpha-receives";
const SECOND_RULE: &str = "handler-scope-beta-receives";
const TARGET_ID: &str = "t1";
const QUEUE: &str = "handler-scope-publications";

/// The source and detail type the handler publishes under, which is all the rules match on —
/// so a message on the queue is one of these publications and nothing else.
const SOURCE: &str = "com.demo.handler-bus-scope";
const DETAIL_TYPE: &str = "Handler Published";

/// The two custom buses, the queue the events are read back from, and one rule per bus
/// delivering to it; answers the queue's url.
async fn estate(emulator: &Emulator, case: &str) -> String {
    for bus in [FIRST_BUS, SECOND_BUS] {
        let created = emulator
            .events(case, "CreateEventBus", &json!({ "Name": bus }))
            .await;
        assert_eq!(
            created["EventBusArn"].as_str(),
            Some(bus_arn(bus).as_str()),
            "case {case:?}: expected CreateEventBus to report {}, got {created}",
            bus_arn(bus)
        );
    }
    let (url, arn) = emulator.events_queue(case, QUEUE).await;
    // One rule per bus, each matching exactly the event the handler publishes and each
    // delivering to the one queue: a message on it names, in its marker, the bus that
    // accepted the publication.
    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    for (bus, rule) in [(FIRST_BUS, FIRST_RULE), (SECOND_BUS, SECOND_RULE)] {
        emulator
            .events(
                case,
                "PutRule",
                &json!({
                    "Name": rule,
                    "EventBusName": bus,
                    "EventPattern": pattern,
                    "State": "ENABLED",
                }),
            )
            .await;
        let targets = emulator
            .events(
                case,
                "PutTargets",
                &json!({
                    "Rule": rule,
                    "EventBusName": bus,
                    "Targets": [{ "Id": TARGET_ID, "Arn": arn }],
                }),
            )
            .await;
        assert_eq!(
            targets["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets reported a failed entry on {bus:?}: {targets}"
        );
    }
    url
}

/// The markers the buses' rules delivered to the queue at `url`, taken off it: looked for
/// again until `expected` of them have arrived or the deadline passes, so a row that expects a
/// delivery never turns on how fast the host runs. A row expecting none is answered by the
/// first look, which is what the rows that DO expect one establish is enough.
async fn delivered_markers(
    emulator: &Emulator,
    case: &str,
    url: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut taken: Vec<String> = Vec::new();
    loop {
        taken.extend(emulator.drained_markers(case, url, SOURCE).await);
        if taken.len() >= expected {
            taken.sort();
            return taken;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} delivered marker(s), only ever saw {taken:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The payload the handler is invoked with: one event published onto `bus` and carrying the
/// bus's own name as its marker, so a message on the queue names the bus that took it.
fn publication_event(bus: &str) -> Value {
    json!({
        "bus": bus,
        "source": SOURCE,
        "detail_type": DETAIL_TYPE,
        "detail": {"marker": bus},
    })
}

/// The document one row puts in force on the execution role: an `Allow` of the publication
/// scoped to `resource`.
fn grant(resource: &Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": PUT_EVENTS_ACTION,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// One row: the `Resource` element one statement scopes the publication by, and which of the
/// two buses that statement covers.
struct Row {
    /// What the row is called in every assertion.
    case: &'static str,
    /// The statement's `Resource`, or `None` for a role carrying no statement of this service
    /// at all.
    resource: Option<Value>,
    /// Whether a publication onto [`FIRST_BUS`] is allowed under it.
    first_allowed: bool,
    /// Whether a publication onto [`SECOND_BUS`] is allowed under it.
    second_allowed: bool,
}

/// The table: the forms an IAM `Resource` element takes over the buses of one account.
///
/// The two wildcards are IAM's own, documented for the element this table varies: "a
/// multi-character match wildcard (`*`) or a single-character match wildcard (`?`)", matched
/// case-sensitively against the resource's ARN. Every other row is an exact ARN, a list of
/// them, or an ARN of another service — nothing a bus of this account can ever be.
fn rows() -> Vec<Row> {
    let first = bus_arn(FIRST_BUS);
    let second = bus_arn(SECOND_BUS);
    vec![
        Row {
            case: "the ARN of the first bus",
            resource: Some(json!(first)),
            first_allowed: true,
            second_allowed: false,
        },
        Row {
            case: "the ARN of the second bus",
            resource: Some(json!(second)),
            first_allowed: false,
            second_allowed: true,
        },
        Row {
            case: "a list carrying both bus ARNs",
            resource: Some(json!([first, second])),
            first_allowed: true,
            second_allowed: true,
        },
        Row {
            case: "the bare wildcard over every resource",
            resource: Some(json!("*")),
            first_allowed: true,
            second_allowed: true,
        },
        Row {
            case: "a run wildcard over every bus of the account",
            resource: Some(json!(bus_arn("*"))),
            first_allowed: true,
            second_allowed: true,
        },
        Row {
            case: "a run wildcard over a prefix only the first bus's name carries",
            resource: Some(json!(bus_arn("handler-scope-al*"))),
            first_allowed: true,
            second_allowed: false,
        },
        Row {
            case: "a single-character wildcard standing for the first bus's last character",
            resource: Some(json!(bus_arn("handler-scope-alph?"))),
            first_allowed: true,
            second_allowed: false,
        },
        Row {
            case: "another service's resource",
            resource: Some(json!(format!(
                "arn:aws:sqs:{REGION}:{}:{QUEUE}",
                engine::emulator_account_id()
            ))),
            first_allowed: false,
            second_allowed: false,
        },
        Row {
            case: "no statement beyond the managed policy the role was created with",
            resource: None,
            first_allowed: false,
            second_allowed: false,
        },
    ]
}

/// RULE: the `Resource` of the statement that allows `events:PutEvents` is matched against the
/// ARN of the bus the ENTRY addressed — IAM's own comparison, with `*` for any run and `?` for
/// one character — so one grant in force on a function's execution role admits a publication
/// onto the buses it covers and refuses one onto every other bus with `AccessDeniedException`
/// naming that bus, publishing nothing of it.
#[tokio::test]
async fn a_handlers_publication_is_scoped_to_the_buses_its_grant_names() {
    let emulator = Emulator::new();
    let role = emulator.execution_role("setup", ROLE_NAME).await;
    let queue = estate(&emulator, "setup").await;
    emulator
        .function("setup", FUNCTION_NAME, &role, PUBLISHER_SOURCE)
        .await;

    for row in rows() {
        let case = row.case;
        let document = row.resource.as_ref().map(grant);
        emulator
            .role_policy(case, ROLE_NAME, INLINE_POLICY, document.as_deref())
            .await;
        let mut accepted: Vec<String> = Vec::new();
        for (bus, allowed) in [
            (FIRST_BUS, row.first_allowed),
            (SECOND_BUS, row.second_allowed),
        ] {
            let (signal, payload) = emulator
                .invoke(case, FUNCTION_NAME, &publication_event(bus))
                .await;
            // The invocation itself is never an error of the run: the handler returned.
            assert_eq!(
                signal, None,
                "case {case:?} on {bus:?}: expected no FunctionError, got {signal:?} with \
                 {payload}"
            );
            assert_eq!(
                payload["ok"],
                json!(allowed),
                "case {case:?} on {bus:?}: expected the publication to be served={allowed}, got \
                 {payload}"
            );
            if allowed {
                assert_eq!(
                    payload["failed_count"],
                    json!(0),
                    "case {case:?} on {bus:?}: expected every entry published, got {payload}"
                );
                accepted.push(bus.to_string());
                continue;
            }
            assert_eq!(
                payload["error_code"],
                json!(ACCESS_DENIED),
                "case {case:?} on {bus:?}: expected the code {ACCESS_DENIED}, got {payload}"
            );
            // The refusal names the bus the entry addressed, not whatever bus the statement
            // happens to cover — which is the whole of resource-scoped authorization.
            let expected = implicit_deny_message(
                &session_arn(ROLE_NAME, FUNCTION_NAME),
                PUT_EVENTS_ACTION,
                &bus_arn(bus),
            );
            assert_eq!(
                payload["message"].as_str().unwrap_or_default(),
                expected,
                "case {case:?} on {bus:?}: expected the refusal {expected:?}, got {payload}"
            );
        }
        // The second half of each row: what the buses' rules actually delivered. A refusal
        // that still published, and a grant that answered without publishing, both fail here.
        accepted.sort();
        let delivered = delivered_markers(&emulator, case, &queue, accepted.len()).await;
        assert_eq!(
            delivered, accepted,
            "case {case:?}: expected the rules to have delivered {accepted:?}, got {delivered:?}"
        );
    }
}
