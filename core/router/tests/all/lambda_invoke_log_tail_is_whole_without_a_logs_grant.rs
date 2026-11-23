// SPDX-License-Identifier: CC0-1.0

//! What a client asking for an invocation's log gets back when the function's execution role is
//! short of a CloudWatch Logs grant: the whole log of the run, on the invocation's own answer.
//!
//! RULE under test: **the answer to `Invoke` — its payload, and the `LogResult` a
//! `LogType=Tail` call asks for ("the last 4 KB of the execution log, which is base64-encoded")
//! — is the RUN's own, produced by running the handler and complete whatever became of the
//! delivery of that log: the `START RequestId: <id> Version: <version>` frame, every record the
//! handler wrote, the `END RequestId: <id>` frame and the `REPORT RequestId: <id>` frame. What
//! the function's execution ROLE decides is only the DELIVERY to Amazon CloudWatch Logs, which
//! Lambda makes for the function: a role that admits `logs:CreateLogGroup` leaves the
//! function's group holding the run's records, and a role that admits none of the delivery's
//! calls leaves no such group at all — while both invocations are answered alike.**
//!
//! A client driving the ordinary "invoke with `LogType=Tail`, then read the function's log
//! group" loop has to get BOTH halves of that: an answer whose tail is short of the run's
//! records leaves the run undiagnosable from the API, and an answer that never comes at all —
//! because the tail or the delivery of a function whose role grants nothing never settles —
//! hangs the client instead of showing it that an ungranted role writes no log. Neither half
//! may wait on the other: the delivery is not part of the answer.
//!
//! The rows differ along what the rule generalises over rather than restating one template: the
//! FORM the role's grant takes — AWS's own managed policy for the purpose, attached; an
//! equivalent document embedded in the role; a role carrying no permissions at all; and a role
//! carrying a document that grants an action none of the three are — so each half of the rule
//! is driven on a granted and on an ungranted role.
//!
//! Three services have to agree for the rule to hold — AWS Lambda runs the handler, answers the
//! tail and makes the delivery, IAM answers for what the execution role may do, and Amazon
//! CloudWatch Logs is where the records either are or are not — so the suite lives here rather
//! than in any one crate; CloudFormation deploys each row's role and the function running as
//! it. What the neighbours pin and this one does not restate:
//! [`crate::cloudformation_lambda_function_log_destination`] drives WHERE a deployed function's
//! records land (and owns the wire harness, the deploy-and-wait and the group readers this
//! suite reuses), `cfn_deployed_lambda_log_group_retention_and_role_gate` the table of grant
//! documents the delivery is gated by,
//! [`crate::lambda_log_records_delivered_under_the_roles_logs_grant`] the FORM each delivered
//! record takes (and owns the grant policy and the framing this suite names), and
//! [`crate::lambda_invoke_log_tail_is_the_runs_own_log`] the tail of a run the handler FAILED.
//! What only this suite reaches is the tail and the group of ONE run read together, which is
//! what a client short of a logs grant depends on.
//!
//! No expected value here comes from a recording: each function name and marker is this suite's
//! own, every asserted record is built from them and from the id the row's own call answered
//! with.

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE assembled emulator, wire harness, deploy-and-wait and group readers this test crate
// owns for a deployed function's log, rather than second copies of them.
use crate::cloudformation_lambda_function_log_destination::{
    dispatch, emulator, filtered, given_stack_function, groups_named, logs_policy,
    messages_at_least, messages_in, trust_policy, Answered, FUNCTION_TIMEOUT, HANDLER_SOURCE,
    RECORDS_PER_RUN, REPORT_PREFIX,
};
// The ONE statement of the policy AWS publishes for a function that writes its own logs, of the
// default group a function logs to, and of the fixed text of the frames one run is framed by.
use crate::lambda_log_records_delivered_under_the_roles_logs_grant::{
    default_group, BASIC_EXECUTION_POLICY, END_PREFIX, START_PREFIX,
};

/// An action that is none of the delivery's three, carried by the row whose role holds a
/// document granting something else entirely.
const UNRELATED_ACTION: &str = "s3:GetObject";

/// The logical ids each row's template declares.
const ROLE_ID: &str = "ExecutionRole";
const FUNCTION_ID: &str = "Logger";

/// The request header `Invoke$LogType` is bound to, and the value that asks for the tail
/// (`smithy_query(query="operation",operation="Invoke")`).
const LOG_TYPE_HEADER: &str = "x-amz-log-type";
/// See [`LOG_TYPE_HEADER`].
const LOG_TYPE_TAIL: &str = "Tail";

/// The response headers the tail and the invocation's own id are bound to. What AWS Lambda
/// reported in `X-Amz-Function-Error` is read off the answer the harness already keeps it on.
const LOG_RESULT_HEADER: &str = "x-amz-log-result";
/// See [`LOG_RESULT_HEADER`].
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

// The fixed text of the frames AWS Lambda writes around one run of a plain-text-logging
// function — the least the platform itself leaves in a tail — is named through the suites that
// own it: `START_PREFIX` and `END_PREFIX` beside the records they frame, and `REPORT_PREFIX`
// from the harness.

/// The version an unqualified invocation executes, which the opening frame names.
const LATEST: &str = "$LATEST";

/// What the closing report separates its fields with, which is what makes the frame one record
/// of fields rather than a sentence.
const REPORT_FIELD_SEPARATOR: char = '\t';

/// The error shape `FilterLogEvents` answers for a group Amazon CloudWatch Logs holds nothing
/// under (`smithy_query(query="operation",operation="FilterLogEvents")`), which is what the
/// default group of a function whose role created none resolves to.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// What an execution role is given, which is what decides whether the delivery of a run's log
/// is made at all — and nothing about the answer the invocation gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// AWS's own managed policy for the purpose, attached by `ManagedPolicyArns`.
    ManagedBasicExecution,
    /// An equivalent document embedded in the role itself.
    EmbeddedPolicy,
    /// Nothing at all: the role carries its trust document and no permissions.
    NoPolicies,
    /// An embedded document granting an action that is none of the delivery's three.
    UnrelatedPolicy,
}

impl Grant {
    /// Whether it admits the delivery's calls, which is what leaves a group to read.
    fn delivers(self) -> bool {
        matches!(self, Self::ManagedBasicExecution | Self::EmbeddedPolicy)
    }

    /// The `AWS::IAM::Role` properties a row declares for it.
    fn role_properties(self) -> Value {
        let mut properties = Map::new();
        properties.insert("AssumeRolePolicyDocument".to_string(), trust_policy());
        let embedded = |document: Value| {
            json!([{ "PolicyName": "delivery", "PolicyDocument": document }])
        };
        match self {
            Self::ManagedBasicExecution => {
                properties.insert(
                    "ManagedPolicyArns".to_string(),
                    json!([BASIC_EXECUTION_POLICY]),
                );
            }
            Self::EmbeddedPolicy => {
                properties.insert("Policies".to_string(), embedded(logs_policy()));
            }
            Self::NoPolicies => {}
            Self::UnrelatedPolicy => {
                properties.insert(
                    "Policies".to_string(),
                    embedded(json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": [UNRELATED_ACTION],
                            "Resource": "*"
                        }]
                    })),
                );
            }
        }
        Value::Object(properties)
    }
}

/// One row: the function the stack names, and the grant its execution role carries.
struct Row {
    case: &'static str,
    /// The `FunctionName` the template states, which is also the subject of its group's name.
    function: &'static str,
    grant: Grant,
}

/// The rows driven: each form a grant of the delivery takes, and each form a role short of one
/// takes.
const ROWS: &[Row] = &[
    Row {
        case: "a role carrying AWS's own basic execution policy",
        function: "xsvc-tail-granted-managed",
        grant: Grant::ManagedBasicExecution,
    },
    Row {
        case: "a role carrying an equivalent document of its own",
        function: "xsvc-tail-granted-embedded",
        grant: Grant::EmbeddedPolicy,
    },
    Row {
        case: "a role carrying no permissions at all",
        function: "xsvc-tail-bare-role",
        grant: Grant::NoPolicies,
    },
    Row {
        case: "a role whose only document grants an unrelated action",
        function: "xsvc-tail-unrelated-grant",
        grant: Grant::UnrelatedPolicy,
    },
];

impl Row {
    /// The template this row deploys: its execution role, and the function that runs as it.
    fn template(&self) -> String {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                ROLE_ID: {
                    "Type": "AWS::IAM::Role",
                    "Properties": self.grant.role_properties()
                },
                FUNCTION_ID: {
                    "Type": "AWS::Lambda::Function",
                    "Properties": {
                        "FunctionName": self.function,
                        "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                        "Runtime": "python3.12",
                        "Handler": "index.handler",
                        "Timeout": FUNCTION_TIMEOUT,
                        "Code": { "ZipFile": HANDLER_SOURCE }
                    }
                }
            },
            "Outputs": {
                "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
            }
        })
        .to_string()
    }
}

/// Invoke `function` asking for the tail of the run, over AWS Lambda's own `restJson1` wire —
/// through the harness's own request builder, with the one header its other callers do not send.
async fn invoke_with_tail(
    router: &MultiServiceRouter,
    function: &str,
    marker: &str,
) -> Answered {
    dispatch(
        router,
        "lambda",
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        &[
            ("content-type", "application/json"),
            (LOG_TYPE_HEADER, LOG_TYPE_TAIL),
        ],
        Bytes::from(json!({ "marker": marker }).to_string()),
    )
    .await
}

/// The tail `answered` carries, decoded: the execution log of the run, as text.
fn tail_of(case: &str, answered: &Answered) -> String {
    let encoded = answered.header(LOG_RESULT_HEADER).unwrap_or_else(|| {
        panic!(
            "case {case:?}: LogType=Tail must answer the run's log, got the headers {:?}",
            answered.headers
        )
    });
    let bytes = ::aws_smithy_types::base64::decode(&encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the tail must be base64, got {error}"));
    String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("case {case:?}: the tail must be text, got {error}"))
}

/// RULE: one `LogType=Tail` invocation is answered with the whole log of the run whatever its
/// execution role allows of the delivery, and the function's group holds that run's records only
/// while the role allows the delivery's calls.
#[tokio::test]
async fn an_invokes_tail_is_whole_while_only_the_delivery_follows_the_roles_grant() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function, the role
    // and the group a row reads are still that row's alone.
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("xsvc-tail-without-grant-{index}");
        let function = given_stack_function(&router, case, &stack, &row.template()).await;
        let group = default_group(&function);
        let marker = format!("{function}-{index}");
        // What the handler this suite deploys prints and answers for that marker.
        let printed = format!("logged {marker}");

        // 1. The invocation itself: answered 200 with the handler's own result and no
        //    `FunctionError`, whatever the role allows of the delivery.
        let answered = invoke_with_tail(&router, &function, &marker).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke of {function:?}, got {} with {}",
            answered.status,
            answered.text()
        );
        let signal = answered.function_error.as_deref();
        assert!(
            signal.is_none(),
            "case {case:?}: a run the handler returned from carries no function error, got \
             {signal:?} with {}",
            answered.text()
        );
        let payload: Value = serde_json::from_slice(&answered.body).unwrap_or(Value::Null);
        assert_eq!(
            payload,
            json!({ "logged": marker }),
            "case {case:?}: expected the handler's own answer, got {}",
            answered.text()
        );
        let request_id = answered.header(REQUEST_ID_HEADER).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the invocation must report its own id, got the headers {:?}",
                answered.headers
            )
        });

        // 2. The tail: the whole log of THIS run — the frames naming the id the call answered
        //    with, and the record the handler wrote in between.
        let tail = tail_of(case, &answered);
        let start = format!("{START_PREFIX}{request_id} Version: {LATEST}");
        assert!(
            tail.lines().any(|line| line == start),
            "case {case:?}: expected the opening frame {start:?}, got {tail:?}"
        );
        assert!(
            tail.lines().any(|line| line == printed),
            "case {case:?}: expected the handler's own record {printed:?}, got {tail:?}"
        );
        let end = format!("{END_PREFIX}{request_id}");
        assert!(
            tail.lines().any(|line| line == end),
            "case {case:?}: expected the closing frame {end:?}, got {tail:?}"
        );
        let report = format!("{REPORT_PREFIX}{request_id}{REPORT_FIELD_SEPARATOR}");
        assert!(
            tail.lines().any(|line| line.starts_with(&report)),
            "case {case:?}: expected a report frame opening {report:?}, got {tail:?}"
        );

        // 3. What the function's group holds, which is the half the ROLE decides. The delivery
        //    is made before the invocation is answered, so the group is read as it stands now.
        if row.grant.delivers() {
            let held = messages_at_least(&router, &group, RECORDS_PER_RUN).await;
            // Each delivered record is the write that produced it, closed by its line break.
            let terminated = format!("{printed}\n");
            assert!(
                held.contains(&terminated),
                "case {case:?}: expected {group:?} to hold the handler's record {terminated:?}, \
                 got {held:?}"
            );
            assert!(
                held.iter()
                    .any(|message| message.starts_with(&format!("{REPORT_PREFIX}{request_id}"))),
                "case {case:?}: expected {group:?} to hold this run's report, got {held:?}"
            );
            let named = groups_named(&router, case, &group).await;
            assert_eq!(
                named.len(),
                1,
                "case {case:?}: expected the function's own group {group:?} to resolve by name, \
                 got {named:?}"
            );
        } else {
            // A role allowing none of the delivery's calls leaves NO group behind: not an empty
            // one, and none a client holding the name can find.
            let named = groups_named(&router, case, &group).await;
            assert!(
                named.is_empty(),
                "case {case:?}: a role short of the delivery's grant creates no log group, got \
                 {named:?}"
            );
            let refused = filtered(&router, &group).await;
            assert_ne!(
                refused.status,
                200,
                "case {case:?}: expected no such group to read at all, got {}",
                refused.text()
            );
            // The refusal is the modelled one for a group CloudWatch Logs does not hold, so a
            // group that stopped existing for some other reason fails this row too.
            let named_error = refused.text();
            assert!(
                named_error.contains(RESOURCE_NOT_FOUND),
                "case {case:?}: expected {RESOURCE_NOT_FOUND} for a group nothing created, got \
                 {named_error}"
            );
            assert!(
                messages_in(&router, &group).await.is_empty(),
                "case {case:?}: expected nothing of the run to be readable in {group:?}"
            );
        }
    }
}
