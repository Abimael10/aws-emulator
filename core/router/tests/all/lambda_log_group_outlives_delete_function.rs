// SPDX-License-Identifier: CC0-1.0

//! The whole ordinary lifecycle of a function's log, driven to a VERDICT: invoke, read the
//! records in Amazon CloudWatch Logs, delete the function, read them again.
//!
//! RULE under test: **every call of that sequence is a synchronous modelled operation that
//! ANSWERS — `Invoke` answers the run (HTTP 200 with the handler's payload), `DeleteFunction`
//! answers its documented 204, and the CloudWatch Logs reads answer 200 — and `DeleteFunction`
//! "deletes a Lambda function" and nothing else: the log group an invocation's records were
//! delivered to is a resource of CloudWatch Logs with a lifecycle of its own, so that group,
//! its streams and every event in them stay exactly as readable through `DescribeLogGroups`,
//! `DescribeLogStreams` and `GetLogEvents` after the function is gone, while the function
//! itself stops resolving (`ResourceNotFoundException`, `Function not found: <arn>`).**
//!
//! A client doing the ordinary thing — invoke a function, then read its logs, then clean the
//! function up — has to get an answer to each of those calls: a call that never returns, or a
//! condition a poller waits on that can never become true, leaves such a client with no status,
//! no error code and no payload at all, which is strictly worse than a refusal. And the log of
//! work that already happened is the only record of it: a group taken down with the function
//! would make a post-mortem of a deleted function impossible, while a group that quietly
//! vanished only on a later read would make log retention unrelyable in either direction.
//!
//! Three services have to agree for the rule to hold — AWS Lambda runs the function and
//! delivers its log, IAM answers for the execution role the delivery is made under, and Amazon
//! CloudWatch Logs holds the group, its streams and its events, with CloudFormation
//! provisioning the role, the function and (in one row) the group — so the suite lives here
//! rather than in any one crate. What the neighbours pin and this one does not restate:
//! [`crate::cloudformation_lambda_function_log_destination`] drives WHERE a deployed function's
//! records land (and owns the assembled-emulator wire harness this suite reuses),
//! [`crate::lambda_log_records_delivered_under_the_roles_logs_grant`] the exact bytes of each
//! delivered record, and [`crate::lambda_invoke_request_id_correlates_with_its_log_group`] the
//! id that correlates one run with its own records. What only this suite reaches is the
//! lifecycle BOUNDARY between the two services: what the delete of a function does, and does
//! not do, to the log it left behind.
//!
//! The rows differ along what the rule generalises over rather than restating one template: the
//! managed runtime that wrote the records, how many invocations were logged before the delete,
//! and where the group came from — one created by the first invocation's own delivery under the
//! default name for the function, one created by that delivery under a name the function's
//! `LoggingConfig` states, and one the SAME stack declared as an `AWS::Logs::LogGroup` before
//! anything ran.
//!
//! No expected value here comes from a recording: every marker, function name and group name is
//! built by the row, and every read-back is compared with what this suite's own runs wrote.
//! Nothing waits a span of its own choosing — each wait looks again for the state it asserts —
//! and nothing asserts on elapsed time.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The ONE assembled emulator, wire harness and group readers this test crate owns for a
// deployed function's log, rather than second copies of them.
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, filtered, groups_named, invoke, lambda, logs, messages_at_least,
    messages_in, trust_policy, wait_until_active, Answered, CREATE_COMPLETE,
    DEFAULT_GROUP_PREFIX, FUNCTION_TIMEOUT, HANDLER_SOURCE, RECORDS_PER_RUN, REPORT_PREFIX,
};
use crate::query_wire::outputs_of;

/// The logical ids the templates below declare.
const ROLE_ID: &str = "ExecutionRole";
const GROUP_ID: &str = "Group";
const FUNCTION_ID: &str = "Logger";

/// The status a successful `DeleteFunction` answers with: its output's `StatusCode` is the
/// operation's `@httpResponseCode`
/// (`smithy_query(query="operation",operation="DeleteFunction")`), and AWS answers a completed
/// delete 204 No Content.
const DELETED_STATUS: u16 = 204;

/// The status the modelled `ResourceNotFoundException` of a Lambda operation carries
/// (`smithy_query(query="operation",operation="Invoke")`: `httpError` 404), and the fixed text
/// of the message that refusal names a function this service holds no record of by
/// (`Function not found: <arn>`).
const NOT_FOUND_STATUS: u16 = 404;
/// See [`NOT_FOUND_STATUS`].
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// What the handler prints for the marker its event carries, which is the one line of a run a
/// client looks for in the group ([`HANDLER_SOURCE`]).
const PRINTED_PREFIX: &str = "logged ";

/// The payload an `Invoke` of an already-deleted function carries: the call still has to be a
/// well-formed invocation, so that what it is answered with is about the function being gone.
const AFTER_DELETE_MARKER: &str = "after-the-delete";

/// What the execution role may do: the three CloudWatch Logs calls an invocation's log delivery
/// makes, which is what the AWS-managed basic execution policy grants a function. A role short
/// of them leaves nothing for a client to read at all, which is that rule rather than this one
/// ([`crate::lambda_log_records_delivered_under_the_roles_logs_grant`] owns it).
fn logs_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*"
        }]
    })
}

/// Where a row's group came from, which is what the rule has to hold over: the group's own
/// provenance cannot decide whether it outlives the function that logged to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// Nothing declares a group and the function declares no `LoggingConfig`, so the first
    /// invocation's delivery creates the default group for the function's own name.
    CreatedByTheRun,
    /// The function's `LoggingConfig` names this group and nothing declares it, so the first
    /// invocation's delivery creates it under that name.
    NamedByTheFunction(&'static str),
    /// The SAME stack declares an `AWS::Logs::LogGroup` under this name and the function's
    /// `LoggingConfig` reaches it by `Ref`, so it exists before anything runs.
    DeclaredByTheStack(&'static str),
}

/// One row: the runtime that writes the records, how many runs are logged before the delete, and
/// where the group came from.
struct Row {
    case: &'static str,
    function: &'static str,
    runtime: &'static str,
    /// How many invocations are made, and so how many runs' records have to outlive the delete.
    runs: usize,
    group: Group,
}

const ROWS: &[Row] = &[
    Row {
        case: "one python 3.12 run, logged into the default group its own delivery created",
        function: "log-group-outlives-default",
        runtime: "python3.12",
        runs: 1,
        group: Group::CreatedByTheRun,
    },
    Row {
        case: "two python 3.13 runs, logged into the default group its own delivery created",
        function: "log-group-outlives-twice",
        runtime: "python3.13",
        runs: 2,
        group: Group::CreatedByTheRun,
    },
    Row {
        case: "one python 3.12 run, logged into a group the function's own logging \
               configuration names",
        function: "log-group-outlives-named",
        runtime: "python3.12",
        runs: 1,
        group: Group::NamedByTheFunction("/xsvc/log-group-outlives/named"),
    },
    Row {
        case: "two python 3.12 runs, logged into a group the same stack declared before \
               anything ran",
        function: "log-group-outlives-declared",
        runtime: "python3.12",
        runs: 2,
        group: Group::DeclaredByTheStack("/xsvc/log-group-outlives/declared"),
    },
];

impl Row {
    /// The group this row's runs are delivered to, given the function id the stack published.
    fn expected_group(&self, function: &str) -> String {
        match self.group {
            Group::CreatedByTheRun => format!("{DEFAULT_GROUP_PREFIX}{function}"),
            Group::NamedByTheFunction(name) | Group::DeclaredByTheStack(name) => name.to_string(),
        }
    }

    /// The template this row deploys: the execution role, the group it declares (where it
    /// declares one), and the function pointed at its destination.
    fn template(&self) -> String {
        let mut function_properties = serde_json::Map::new();
        function_properties.insert("FunctionName".to_string(), json!(self.function));
        function_properties.insert(
            "Role".to_string(),
            json!({ "Fn::GetAtt": [ROLE_ID, "Arn"] }),
        );
        function_properties.insert("Runtime".to_string(), json!(self.runtime));
        function_properties.insert("Handler".to_string(), json!("index.handler"));
        function_properties.insert("Timeout".to_string(), json!(FUNCTION_TIMEOUT));
        function_properties.insert("Code".to_string(), json!({ "ZipFile": HANDLER_SOURCE }));
        match self.group {
            Group::CreatedByTheRun => {}
            Group::NamedByTheFunction(name) => {
                function_properties.insert("LoggingConfig".to_string(), json!({ "LogGroup": name }));
            }
            Group::DeclaredByTheStack(_) => {
                function_properties.insert(
                    "LoggingConfig".to_string(),
                    json!({ "LogGroup": { "Ref": GROUP_ID } }),
                );
            }
        }

        let mut resources = serde_json::Map::new();
        resources.insert(
            ROLE_ID.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "basic-execution",
                        "PolicyDocument": logs_policy()
                    }]
                }
            }),
        );
        if let Group::DeclaredByTheStack(name) = self.group {
            resources.insert(
                GROUP_ID.to_string(),
                json!({
                    "Type": "AWS::Logs::LogGroup",
                    "Properties": { "LogGroupName": name }
                }),
            );
        }
        resources.insert(
            FUNCTION_ID.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": Value::Object(function_properties)
            }),
        );

        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": Value::Object(resources),
            "Outputs": {
                "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
            }
        })
        .to_string()
    }
}

/// The streams `DescribeLogStreams` reports in `group`, which a client reads a group's events
/// through.
async fn streams_of(router: &MultiServiceRouter, case: &str, group: &str) -> BTreeSet<String> {
    let described = logs(
        router,
        "DescribeLogStreams",
        json!({ "logGroupName": group }),
    )
    .await;
    assert_eq!(
        described.status,
        200,
        "case {case:?}: expected HTTP 200 from DescribeLogStreams for {group:?}, got {} with {:?}",
        described.status,
        described.text()
    );
    described
        .json()
        .get("logStreams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| stream.get("logStreamName").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The messages `GetLogEvents` reports for one stream of `group`, read from its head so the
/// whole stream is answered rather than its tail.
async fn events_of(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
) -> Vec<String> {
    let read = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": group,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetLogEvents for {group:?}/{stream:?}, got {} \
         with {:?}",
        read.status,
        read.text()
    );
    read.json()
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Every stream of `group` with the messages it holds, which is the whole of what a client can
/// read out of a group right now.
async fn read_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, Vec<String>)> {
    let mut read = Vec::new();
    for stream in streams_of(router, case, group).await {
        let messages = events_of(router, case, group, &stream).await;
        read.push((stream, messages));
    }
    read
}

/// Assert that `answered` is the refusal AWS Lambda names a function it holds no record of by:
/// the modelled not-found status, and a message carrying its fixed text and the function the
/// call addressed.
fn assert_function_gone(case: &str, call: &str, function: &str, answered: &Answered) {
    assert_eq!(
        answered.status, NOT_FOUND_STATUS,
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS} from {call} of the deleted \
         {function:?}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let document = answered.json();
    let message = document
        .get("message")
        .or_else(|| document.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.starts_with(NOT_FOUND_PREFIX) && message.contains(function),
        "case {case:?}: expected {call} of the deleted {function:?} to be refused \
         {NOT_FOUND_PREFIX:?} naming it, got {:?}",
        answered.text()
    );
}

#[tokio::test]
async fn a_functions_log_group_and_its_events_outlive_the_delete_of_the_function() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function and the
    // group a row reads are still that row's alone.
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;

        // 1. The stack: the execution role, the group it declares where it declares one, and
        //    the function pointed at its destination.
        let stack = format!("lambda-log-group-outlives-{index}");
        let template = row.template();
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template),
                // The template declares an IAM role, which is acknowledged with CAPABILITY_IAM.
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );
        let (reached, described) = wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
        );
        let function = outputs_of(&described)
            .get("FunctionName")
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the stack published no function name in {described}")
            });
        let group = row.expected_group(&function);

        // 2. The function has to be invocable before any of this can be read: "During this
        //    time, you can't invoke or modify the function" (`CreateFunction`).
        wait_until_active(&router, case, &function).await;

        // 3. Every invocation ANSWERS: HTTP 200, no `FunctionError`, and the handler's own
        //    payload — which is the first half of the lifecycle reaching a verdict at all.
        let markers: Vec<String> = (0..row.runs).map(|run| format!("{stack}-{run}")).collect();
        for marker in &markers {
            invoke(&router, case, &function, marker).await;
        }

        // 4. ... and the records of those runs become readable in the ONE group the function
        //    logs to: each run's own printed line, and the runtime's closing frame.
        let held = messages_at_least(&router, &group, RECORDS_PER_RUN * row.runs).await;
        for marker in &markers {
            let printed = format!("{PRINTED_PREFIX}{marker}");
            assert!(
                held.iter().any(|message| message.contains(&printed)),
                "case {case:?}: expected {group:?} to hold the line the run printed, \
                 {printed:?}, got {held:?}"
            );
        }
        assert!(
            held.iter()
                .filter(|message| message.starts_with(REPORT_PREFIX))
                .count()
                >= row.runs,
            "case {case:?}: expected {group:?} to hold one {REPORT_PREFIX:?} frame per run, got \
             {held:?}"
        );

        // 5. What a client can read of the group right now: the group record itself, its
        //    streams, and the events of each stream. This is the snapshot the delete must not
        //    change.
        let group_before = groups_named(&router, case, &group).await;
        assert_eq!(
            group_before.len(),
            1,
            "case {case:?}: expected exactly one group named {group:?} before the delete, got \
             {group_before:?}"
        );
        let streams_before = streams_of(&router, case, &group).await;
        assert!(
            !streams_before.is_empty(),
            "case {case:?}: expected the runs' records to be in a stream of {group:?}, got none"
        );
        let read_before = read_group(&router, case, &group).await;

        // 6. `DeleteFunction` answers its documented status.
        let deleted = lambda(
            &router,
            "DELETE",
            &format!("/2015-03-31/functions/{function}"),
            None,
        )
        .await;
        assert_eq!(
            deleted.status,
            DELETED_STATUS,
            "case {case:?}: expected HTTP {DELETED_STATUS} from DeleteFunction of {function:?}, \
             got {} with {:?}",
            deleted.status,
            deleted.text()
        );

        // 7. ... and the function really is gone: every call that takes it as a handle is
        //    answered the modelled refusal rather than hanging or succeeding.
        let read = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        assert_function_gone(case, "GetFunctionConfiguration", &function, &read);
        let invoked = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            Some(json!({ "marker": AFTER_DELETE_MARKER })),
        )
        .await;
        assert_function_gone(case, "Invoke", &function, &invoked);

        // 8. The group OUTLIVES it: it still resolves by its own name, unchanged.
        let group_after = groups_named(&router, case, &group).await;
        assert_eq!(
            group_after, group_before,
            "case {case:?}: expected DescribeLogGroups to report {group:?} unchanged after the \
             function was deleted, got {group_after:?} where it reported {group_before:?}"
        );

        // 9. ... with every stream it had, and every event each stream held. A later delivery
        //    may add records, so what is asserted is that NOTHING the function left behind was
        //    taken down with it.
        let streams_after = streams_of(&router, case, &group).await;
        assert!(
            streams_before.is_subset(&streams_after),
            "case {case:?}: expected every stream of {group:?} to outlive the function, got \
             {streams_after:?} where it held {streams_before:?}"
        );
        let read_after = read_group(&router, case, &group).await;
        for (stream, messages) in &read_before {
            let after = read_after
                .iter()
                .find(|(name, _)| name == stream)
                .map(|(_, messages)| messages.clone())
                .unwrap_or_default();
            for message in messages {
                assert!(
                    after.contains(message),
                    "case {case:?}: expected GetLogEvents on {group:?}/{stream:?} to still \
                     report {message:?} after the function was deleted, got {after:?}"
                );
            }
        }

        // 10. ... and a client that reads the group as a whole is answered, with each run's own
        //     line still in it: the marker the scenario is about outliving the function.
        let filtered_after = filtered(&router, &group).await;
        assert_eq!(
            filtered_after.status,
            200,
            "case {case:?}: expected HTTP 200 from FilterLogEvents on {group:?} after the \
             function was deleted, got {} with {:?}",
            filtered_after.status,
            filtered_after.text()
        );
        let held_after = messages_in(&router, &group).await;
        for marker in &markers {
            let printed = format!("{PRINTED_PREFIX}{marker}");
            assert!(
                held_after.iter().any(|message| message.contains(&printed)),
                "case {case:?}: expected {group:?} to still hold {printed:?} after the function \
                 was deleted, got {held_after:?}"
            );
        }
        // ... and the run the deleted function could not take leaves no record of its own.
        assert!(
            !held_after
                .iter()
                .any(|message| message.contains(AFTER_DELETE_MARKER)),
            "case {case:?}: the invocation of a deleted function ran nothing, so {group:?} must \
             hold no record of {AFTER_DELETE_MARKER:?}, got {held_after:?}"
        );
    }
}
