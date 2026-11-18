// SPDX-License-Identifier: CC0-1.0

//! What a client PROBING a stack's progress reads while one of its resources is held at a
//! resource-signal wait, end to end over the awsQuery wire.
//!
//! RULE: **a resource whose declaration asks the operation to wait for success signals is
//! reported `CREATE_IN_PROGRESS` until those signals arrive, however ready the service that
//! owns it reports it to be** — "CloudFormation doesn't proceed with a stack creation or update
//! until resources receive the required number of signals or the timeout period is exceeded"
//! (`SignalResource`). A resource of any type can be held that way, and a resource that is also
//! held for its owning service to report it ready has TWO gates: being ready releases one of
//! them and finishes nothing.
//!
//! Why it matters: the wait is observable only through what a client reads while it is open. A
//! resource reported `CREATE_COMPLETE` mid-wait tells a deployment dashboard the resource is up
//! when nothing has signalled it, and — because that completion is the resource's own terminal
//! event — the failure the wait's expiry owes it has nowhere left to be recorded, so a client
//! triaging the timeout afterwards cannot see WHICH resource timed out or why.
//!
//! The rows span what the rule generalises over: WHICH declaration opens the wait (a
//! `CreationPolicy` on a resource whose owning service reports it ready on its own, and an
//! `AWS::CloudFormation::WaitCondition`, which no service reports anything about) and how many
//! signals it waits for.
//!
//! Nothing here asserts on the wall clock: the probing loop is a bounded number of reads, and
//! the release is waited for by looking again until the stack reports it.

use std::time::{Duration, Instant};

use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, resource_event_statuses, router, xml_text,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

/// How many times a row READS the stack while its wait is open. Each read is also what
/// materialises everything the stack's clock owes, so this is the client whose polling used to
/// record a completion nothing had signalled.
const PROBES: usize = 24;

/// How long a release is waited for, and how often it is looked for. Generous: the deadline is
/// a safety net, never a measurement — the loop stops the instant the condition holds.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// `SignalResource` for one resource of one stack.
async fn signal(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    unique_id: &str,
) -> (u16, String) {
    call(
        router,
        "SignalResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
            member("UniqueId", unique_id),
            member("Status", "SUCCESS"),
        ],
    )
    .await
}

/// The `StackStatus` `DescribeStacks` reports for `stack`.
async fn stack_status(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    let (code, body) = describe_stack(router, stack).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks({stack}) must answer 200, got {code} {body}"
    );
    xml_text(&body, "StackStatus").unwrap_or_default()
}

/// One row: the stack, the template, the resource the wait is on and the signals that release
/// it.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    gated: &'static str,
    /// The `UniqueId`s the row signals, which are as many as the declaration's `Count`.
    signals: &'static [&'static str],
}

const ROWS: &[Row] = &[
    // The resource the fix is about: Amazon EC2 reports the instance running while the wait is
    // still short of its signal, so the resource has one gate released and one open.
    Row {
        case: "a CreationPolicy on a resource its owning service reports ready",
        stack: "gated-creation-policy",
        template: r#"{"Resources":{"Gate":{
            "Type":"AWS::EC2::Instance",
            "Properties":{"ImageId":"ami-0123456789abcdef0","InstanceType":"t3.micro"},
            "CreationPolicy":{"ResourceSignal":{"Count":"1","Timeout":"PT10M"}}}}}"#,
        gated: "Gate",
        signals: &["i-0000000000000000a"],
    },
    // ...and the declaration that waits for signals and nothing else, with a Count above one so
    // the row also covers a wait that is still short after a signal has arrived.
    Row {
        case: "a WaitCondition's own Count and Timeout",
        stack: "gated-wait-condition",
        template: r#"{"Resources":{
            "Handle":{"Type":"AWS::CloudFormation::WaitConditionHandle"},
            "Gate":{
              "Type":"AWS::CloudFormation::WaitCondition",
              "Properties":{"Handle":{"Ref":"Handle"},"Count":"2","Timeout":"600"}}}}"#,
        gated: "Gate",
        signals: &["signal-one", "signal-two"],
    },
];

/// A resource held at a signal wait is never reported complete before its signals arrive, and
/// is reported complete once they have.
#[tokio::test]
async fn a_probed_signal_wait_reports_no_completion_before_its_signals() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let (code, created) = create_stack(&router, row.stack, row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to succeed, got {code} {created}"
        );

        // The client probes the stack while the wait is open: every read materialises what the
        // clock owes, and none of them may report the gated resource as finished.
        for probe in 0..PROBES {
            let statuses = resource_event_statuses(&router, case, row.stack, row.gated).await;
            assert!(
                !statuses
                    .iter()
                    .any(|recorded| recorded == resource_status::CREATE_COMPLETE),
                "case {case:?}: probe {probe} read {:?} as {:?} with no signal sent; its \
                 recorded statuses are {statuses:?}",
                row.gated,
                resource_status::CREATE_COMPLETE
            );
            let reported = stack_status(case, &router, row.stack).await;
            assert_eq!(
                reported, status::CREATE_IN_PROGRESS,
                "case {case:?}: probe {probe} must find the create still in flight, got \
                 {reported:?}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }

        // ...and the signals the declaration asks for release it.
        for unique_id in row.signals {
            let (code, signalled) = signal(&router, row.stack, row.gated, unique_id).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected signal {unique_id:?} to be accepted, got {code} \
                 {signalled}"
            );
        }
        let deadline = Instant::now() + BUDGET;
        loop {
            let reported = stack_status(case, &router, row.stack).await;
            if reported == status::CREATE_COMPLETE {
                break;
            }
            assert!(
                Instant::now() < deadline && reported == status::CREATE_IN_PROGRESS,
                "case {case:?}: the signalled create must complete, got {reported:?}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
        let statuses = resource_event_statuses(&router, case, row.stack, row.gated).await;
        assert!(
            statuses
                .iter()
                .any(|recorded| recorded == resource_status::CREATE_COMPLETE),
            "case {case:?}: the released resource must be reported complete, got {statuses:?}"
        );
    }
}
