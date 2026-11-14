// SPDX-License-Identifier: CC0-1.0

//! The log of a run that ALREADY HAPPENED, when the function's execution role changes while
//! that run is still in flight.
//!
//! RULE under test: **the lines an invocation wrote belong to that invocation — whether Lambda
//! may write them is decided from the policies in force on the execution role when the
//! invocation is accepted, before the handler runs — so a change made to that role after the
//! invocation was accepted does not erase the log of a run that already happened: the group
//! holds the run's own lines and its `START`/`REPORT` framing either way.**
//!
//! That is what a client reads on AWS of any run whose role goes away right behind it, and a
//! CloudFormation rollback does exactly that: the provider function's own `Delete` answer is
//! what lets the rollback delete the function's role, and the provider's log is where a caller
//! then reads which request the provider was sent.
//!
//! The rows differ along WHAT changed about the role after the invocation was accepted: the
//! log writes withdrawn, the log writes explicitly denied, the role deleted outright, and —
//! the control — nothing changed at all. Every row drives the asynchronous surface, whose run
//! happens off the response path: the accepting call is answered before the run, so the change
//! the row makes next lands between the acceptance and the delivery without anything here
//! waiting a fixed span for it.
//!
//! Every row RUNS a handler, so the suite takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, iam};
use crate::invocation_log_delivery::{
    create_function, delivered, emulator, groups, streams, Created, LOGS, LOG_WRITES,
    NODE_RUNTIME,
};

/// The Region every invocation below is addressed in.
const REGION: &str = "us-east-1";

/// Where `Invoke` is bound.
const INVOCATIONS: &str = "/2015-03-31/functions";

/// The header that asks for the asynchronous surface, and the value that names it: "Event —
/// invoke the function asynchronously" (`InvokeInput$InvocationType`).
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";
const EVENT_TYPE: &str = "Event";

/// The name of the one inline policy each role below carries, put by the suite itself: every
/// change a row makes to the role's grants REPLACES that one policy, so what the role allows
/// is exactly what the row says it allows.
const POLICY_NAME: &str = "the-log-writes";

/// How long a row waits for the queued run's log to reach the group, and how often it looks.
/// The wait is on the CONDITION — the lines being there — and the budget is generous: nothing
/// here is timed.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);

/// The fixed framing the runtime writes around every run it finishes.
const START_PREFIX: &str = "START RequestId: ";
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// What the role may do about the log writes once the row has changed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Changed {
    /// Nothing is changed: the control.
    Nothing,
    /// The inline policy is replaced by one that allows nothing of CloudWatch Logs.
    GrantWithdrawn,
    /// The inline policy is replaced by one that DENIES the log writes outright.
    WritesDenied,
    /// The policy is removed and the role itself is deleted, which is what the teardown of a
    /// stack that declared the role does.
    RoleDeleted,
}

/// One row: what is changed about the role while the accepted run is still in flight.
struct Row {
    case: &'static str,
    /// The suffix the row's own role, function and marker are named by.
    name: &'static str,
    changed: Changed,
}

const ROWS: &[Row] = &[
    Row {
        case: "nothing changed about the role",
        name: "untouched",
        changed: Changed::Nothing,
    },
    Row {
        case: "the role's log writes withdrawn behind the run",
        name: "withdrawn",
        changed: Changed::GrantWithdrawn,
    },
    Row {
        case: "the role's log writes denied behind the run",
        name: "denied",
        changed: Changed::WritesDenied,
    },
    Row {
        case: "the role deleted behind the run",
        name: "deleted",
        changed: Changed::RoleDeleted,
    },
];

/// Accept one asynchronous invocation of `name`: "a success response (202 Accepted) indicates
/// that the request is queued for invocation", so the run itself is still to come when this
/// answers.
async fn accepted(router: &MultiServiceRouter, case: &str, name: &str) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{INVOCATIONS}/{name}/invocations"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .header("content-type", "application/json")
        .header(INVOCATION_TYPE_HEADER, EVENT_TYPE)
        .body(bytes::Bytes::from_static(b"{}"))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        202,
        "case {case:?}: expected the asynchronous invocation to be accepted with 202, got \
         {status} with {:?}",
        String::from_utf8_lossy(response.body())
    );
}

/// Carry out the row's change to the role `name`, through IAM's own API.
async fn change(router: &MultiServiceRouter, case: &str, name: &str, changed: Changed) {
    match changed {
        Changed::Nothing => {}
        Changed::GrantWithdrawn => {
            put_policy(router, case, name, json!([allow("s3", &["GetObject"], "*")])).await;
        }
        Changed::WritesDenied => {
            let denied = json!([{
                "Effect": "Deny",
                "Action": LOG_WRITES
                    .iter()
                    .map(|action| format!("{LOGS}:{action}"))
                    .collect::<Vec<String>>(),
                "Resource": "*",
            }]);
            put_policy(router, case, name, denied).await;
        }
        Changed::RoleDeleted => {
            // A role is deleted without its inline policies, so the policy goes first — the
            // order a teardown of the stack that declared both takes them in.
            let (status, answered) = iam(
                router,
                "DeleteRolePolicy",
                &[("RoleName", name), ("PolicyName", POLICY_NAME)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DeleteRolePolicy to answer 200, got {status}: \
                 {answered}"
            );
            let (status, answered) = iam(router, "DeleteRole", &[("RoleName", name)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DeleteRole to answer 200, got {status}: {answered}"
            );
        }
    }
}

/// Replace the one inline policy of the role `name` with `statements`.
async fn put_policy(router: &MultiServiceRouter, case: &str, name: &str, statements: Value) {
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, answered) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutRolePolicy to answer 200, got {status}: {answered}"
    );
}

/// The lines the one stream of `group` holds, looked for until they are there or [`DEADLINE`]
/// passes — which is what a client polling for a queued run's output does.
async fn logged_lines(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        if !groups(router, case, group).await.is_empty() {
            let held = streams(router, case, group).await;
            if let Some(stream) = held.first() {
                let (lines, _) = delivered(router, case, group, stream).await;
                if !lines.is_empty() {
                    return lines;
                }
            }
        }
        if Instant::now() >= giving_up_at {
            return Vec::new();
        }
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_finished_runs_log_survives_a_change_to_the_execution_role_behind_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        let role_name = format!("log-after-{}", row.name);
        let role = given_role(&router, case, &role_name, None).await;
        put_policy(
            &router,
            case,
            &role_name,
            json!([allow(LOGS, &LOG_WRITES, "*")]),
        )
        .await;
        let function = format!("log-after-{}", row.name);
        let marker = format!("the run of {} wrote this", row.name);
        let source = format!(
            "exports.handler = async () => {{\n    console.log('{marker}');\n    return \
             {{ ok: true }};\n}};"
        );
        create_function(
            &router,
            &Created {
                case,
                name: &function,
                source: &source,
                runtime: NODE_RUNTIME,
                role: &role,
                log_group: None,
            },
        )
        .await;

        // The invocation is ACCEPTED, so the run is still to come...
        accepted(&router, case, &function).await;
        // ...and the role changes right behind it, which is where a rollback changes it.
        change(&router, case, &role_name, row.changed).await;

        // The lines the run wrote are in its group whatever became of the role: the decision
        // was taken when the invocation was accepted.
        let group = format!("/aws/lambda/{function}");
        let lines = logged_lines(&router, case, &group).await;
        assert!(
            lines.iter().any(|line| line.contains(&marker)),
            "case {case:?}: expected the group {group:?} to hold the line the run wrote \
             ({marker:?}), got {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.starts_with(START_PREFIX)),
            "case {case:?}: expected the group {group:?} to hold the run's {START_PREFIX:?} \
             line, got {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.starts_with(REPORT_PREFIX)),
            "case {case:?}: expected the group {group:?} to hold the run's {REPORT_PREFIX:?} \
             line, got {lines:?}"
        );
    }
}
