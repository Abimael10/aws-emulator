// SPDX-License-Identifier: CC0-1.0

//! The envelope a subscription filter's AWS Lambda destination is invoked with carries exactly
//! the ingested log events the filter's `filterPattern` selected — whichever FORM that pattern
//! is written in.
//!
//! RULE under test: **for a subscription filter whose `destinationArn` names a Lambda function,
//! the `awslogs` document that function is invoked with carries, of the events a `PutLogEvents`
//! batch stored, exactly those the filter's `filterPattern` selects under the CloudWatch Logs
//! filter and pattern syntax — the unstructured form (terms a message must carry, terms it must
//! not, alternatives at least one of which it must, and `%…%` regular expressions), the
//! space-delimited form (`[ … ]`, a field list with conditions on the fields it names) and the
//! JSON form (`{ … }`, conditions over a JSON event's `$.path` fields) alike — and carries no
//! event that form rejects.**
//!
//! `PutSubscriptionFilter$filterPattern` is "a filter pattern for subscribing to a filtered
//! stream of log events", `PutSubscriptionFilter` itself states that "using regular expressions
//! in filter patterns is supported" and points at "Filter pattern syntax for metric filters,
//! subscription filters, filter log events, and Live Tail" for what a pattern means, and it
//! states the framing the destination is handed ("When log events are sent to the receiving
//! service, they are Base64 encoded and compressed with the GZIP format"). So the selection is
//! the pattern SYNTAX's, not one special case of it: a filter is no more installed for a
//! bracketed or JSON pattern than for a bare word.
//!
//! `crate::logs_subscription_filter_to_lambda` states the hand-over itself — that the
//! destination is invoked at all, what the envelope's members are, and that an event the pattern
//! rejects is absent — over a batch's SIZE and over how the topology was wired. This suite
//! varies the other axis the rule generalises over, the pattern's own FORM, and reuses that
//! suite's wiring rather than keeping a second copy of it.
//!
//! The rows differ along that axis: a conjunction of two bare terms, a quoted string carrying
//! characters a bare term cannot, an excluded term, a set of alternatives, a regular
//! expression, a space-delimited field list with a numeric condition, and a condition over a
//! JSON event's field. Each row's batch carries lines the form selects and lines it rejects,
//! and every expected value is built from what the row wrote — nothing here asserts on any
//! recorded instance's names or markers.
//!
//! The deliverable is the EFFECT, as in the sibling suite: the destination function decodes the
//! two layers the payload is framed in and prints the envelope document, and this suite reads
//! that document back out of the function's own log group, so what is checked is what the
//! function was really handed.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::logs_subscription_filter_to_lambda::{
    cloudformation, grant_logs_invoke, iam, logs_ok, printed_at_least, published_function_arn,
    text_member, DATA_MESSAGE, DESTINATION_ID, EXECUTION_ROLE, FUNCTION, FUNCTION_TIMEOUT,
    HANDLER_SOURCE, ROLE_POLICY, TRUST_POLICY,
};
use crate::query_wire::element;

/// One line a row's batch writes: its message, and whether the row's pattern form selects it —
/// which is whether it must be inside the envelope the destination function was handed.
struct Written {
    message: &'static str,
    selected: bool,
}

/// One row: the pattern FORM under test, the `filterPattern` spelling it, and the batch whose
/// lines that form has to sort into selected and rejected.
struct Row {
    case: &'static str,
    /// Names this row's log group, log stream and subscription filter, so no two rows share one.
    name: &'static str,
    pattern: &'static str,
    batch: &'static [Written],
}

/// The forms, and the spellings of each, come from the filter and pattern syntax the
/// `filterPattern` members point at: an unstructured pattern's terms (a bare word, a `"…"`
/// string, a `%…%` regular expression, a `-` exclusion and a `?` alternative), the
/// space-delimited `[ … ]` field list with conditions, and the JSON `{ … }` conditions over
/// `$.path` fields.
const ROWS: &[Row] = &[
    Row {
        case: "two bare terms, both of which a selected line carries",
        name: "both-terms",
        pattern: "ERROR Timeout",
        batch: &[
            Written {
                message: "ERROR upstream Timeout reached",
                selected: true,
            },
            Written {
                message: "ERROR upstream refused the connection",
                selected: false,
            },
            Written {
                message: "INFO upstream Timeout raised to 30s",
                selected: false,
            },
            Written {
                message: "Timeout ERROR in the other order",
                selected: true,
            },
        ],
    },
    Row {
        case: "a quoted string term carrying characters a bare term cannot",
        name: "quoted",
        pattern: "\"status=500, retrying\"",
        batch: &[
            Written {
                message: "gateway said status=500, retrying in 2s",
                selected: true,
            },
            Written {
                message: "gateway said status=500 and gave up",
                selected: false,
            },
            Written {
                message: "gateway said status=204, retrying in 2s",
                selected: false,
            },
        ],
    },
    Row {
        case: "a required term beside an excluded one",
        name: "excluded",
        pattern: "ERROR -Retrying",
        batch: &[
            Written {
                message: "ERROR the write was rejected",
                selected: true,
            },
            Written {
                message: "ERROR the write was rejected, Retrying",
                selected: false,
            },
            Written {
                message: "WARN the write was slow",
                selected: false,
            },
        ],
    },
    Row {
        case: "alternatives, at least one of which a selected line carries",
        name: "alternatives",
        pattern: "?ALPHA ?BETA",
        batch: &[
            Written {
                message: "release ALPHA was promoted",
                selected: true,
            },
            Written {
                message: "release BETA was promoted",
                selected: true,
            },
            Written {
                message: "release ALPHA and BETA were promoted together",
                selected: true,
            },
            Written {
                message: "release GAMMA was promoted",
                selected: false,
            },
        ],
    },
    Row {
        case: "a regular-expression term",
        name: "expression",
        pattern: "%ORDER-[0-9]{4}\\b%",
        batch: &[
            Written {
                message: "shipped ORDER-1234 to the depot",
                selected: true,
            },
            Written {
                message: "shipped ORDER-99 to the depot",
                selected: false,
            },
            Written {
                message: "shipped ORDER-ABCD to the depot",
                selected: false,
            },
            Written {
                message: "cancelled ORDER-0007 at the depot",
                selected: true,
            },
        ],
    },
    Row {
        case: "a space-delimited field list with a numeric condition on a named field",
        name: "space-delimited",
        pattern: "[level, code = 500, ...]",
        batch: &[
            Written {
                message: "ERROR 500 payment declined upstream",
                selected: true,
            },
            Written {
                message: "ERROR 404 payment route is unknown",
                selected: false,
            },
            Written {
                message: "INFO 500 payment retried once",
                selected: true,
            },
            Written {
                message: "ERROR",
                selected: false,
            },
        ],
    },
    Row {
        case: "a JSON condition over a field of a JSON event",
        name: "json",
        pattern: "{ $.level = \"ERROR\" }",
        batch: &[
            Written {
                message: r#"{"level":"ERROR","detail":"the ledger is behind"}"#,
                selected: true,
            },
            Written {
                message: r#"{"level":"INFO","detail":"the ledger caught up"}"#,
                selected: false,
            },
            Written {
                message: r#"{"detail":"the ledger says nothing about its level"}"#,
                selected: false,
            },
            Written {
                message: "ERROR but this line is no JSON document",
                selected: false,
            },
        ],
    },
];

impl Row {
    /// This row's log group.
    fn group(&self) -> String {
        format!("/aws/emulator/subscription-pattern-forms/{}", self.name)
    }

    /// The log stream this row's batch is written to.
    fn stream(&self) -> String {
        format!("{}-stream", self.name)
    }

    /// This row's subscription filter.
    fn filter(&self) -> String {
        format!("{}-to-lambda", self.name)
    }

    /// The statement id the invoke permission for this row's group is added under.
    fn statement_id(&self) -> String {
        format!("{}-logs-invoke", self.name)
    }

    /// The messages this row's pattern form selects, in the order they were written — exactly
    /// what the envelope handed to the destination function must carry.
    fn selected(&self) -> Vec<String> {
        self.batch
            .iter()
            .filter(|written| written.selected)
            .map(|written| written.message.to_string())
            .collect()
    }

    /// The messages the form REJECTS, which the envelope must not carry.
    fn rejected(&self) -> Vec<String> {
        self.batch
            .iter()
            .filter(|written| !written.selected)
            .map(|written| written.message.to_string())
            .collect()
    }
}

/// Deploy the destination function and answer its ARN once AWS Lambda reports it invocable.
///
/// A CloudFormation template is what archives inline source into a deployment package this
/// emulator can run, so the function comes from a one-resource stack; every row's group, filter
/// and invoke permission is then put through its own service's API, which is the wiring this
/// suite's axis is independent of. The two waits the deployment owes — the stack reporting
/// itself complete, and Lambda reporting the function `Active` — are the sibling suite's
/// (`published_function_arn`).
async fn deploy_destination(router: &MultiServiceRouter, role_arn: &str) -> String {
    let stack = "logs-subscription-pattern-forms";
    let function_arn = json!({ "Fn::GetAtt": [DESTINATION_ID, "Arn"] });
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            DESTINATION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": function_arn },
        },
    })
    .to_string();
    cloudformation(
        router,
        "setup",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    published_function_arn(router, "setup", stack).await
}

#[tokio::test]
async fn a_lambda_destination_is_handed_what_each_filter_pattern_form_selects() {
    let router = crate::logs_subscription_filter_to_lambda::emulator();
    let account = engine::emulator_account_id();

    // The execution role the destination function runs as, and under which its own log is
    // written: a role short of the logs grants leaves nothing for a client to read, which would
    // be that rule rather than this one.
    let created = iam(
        &router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    let role_arn = element(&created, "Arn");
    iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", "basic-execution"),
            ("PolicyDocument", ROLE_POLICY),
        ],
    )
    .await;

    let function_arn = deploy_destination(&router, &role_arn).await;

    // One log group, one invoke permission for it, one filter carrying this row's pattern form,
    // and the stream the row's batch is written to.
    for row in ROWS {
        let case = row.case;
        logs_ok(
            &router,
            case,
            "CreateLogGroup",
            json!({ "logGroupName": row.group() }),
        )
        .await;
        grant_logs_invoke(&router, case, &row.statement_id(), &row.group(), account).await;
        logs_ok(
            &router,
            case,
            "PutSubscriptionFilter",
            json!({
                "logGroupName": row.group(),
                "filterName": row.filter(),
                "filterPattern": row.pattern,
                // A Lambda destination carries no `roleArn`: the function is invoked directly,
                // so the grant above is what authorizes the hand-over.
                "destinationArn": function_arn,
            }),
        )
        .await;
        logs_ok(
            &router,
            case,
            "CreateLogStream",
            json!({ "logGroupName": row.group(), "logStreamName": row.stream() }),
        )
        .await;
    }

    // One batch per row, every event stamped with the same instant inside the ingestion window,
    // so what the function is handed is decided by the pattern and not by admission.
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64");
    for row in ROWS {
        let events: Vec<Value> = row
            .batch
            .iter()
            .map(|written| json!({ "timestamp": now_ms, "message": written.message }))
            .collect();
        logs_ok(
            &router,
            row.case,
            "PutLogEvents",
            json!({
                "logGroupName": row.group(),
                "logStreamName": row.stream(),
                "logEvents": events,
            }),
        )
        .await;
    }

    // Each row's selected lines are small enough to travel as ONE envelope, so one row owes one
    // printed document. The wait is for all of them to be there — the hand-over is asynchronous,
    // so this polls for the CONDITION rather than for a span.
    let (envelopes, other_lines) = printed_at_least(&router, ROWS.len()).await;
    assert_eq!(
        envelopes.len(),
        ROWS.len(),
        "expected the destination function to have printed one envelope per subscribed log \
         group ({} of them), got {envelopes:?} beside {other_lines:?}",
        ROWS.len()
    );

    for row in ROWS {
        let case = row.case;
        let group = row.group();
        let matching: Vec<&Value> = envelopes
            .iter()
            .filter(|envelope| envelope.get("logGroup").and_then(Value::as_str) == Some(&group))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "case {case:?}: expected exactly one envelope naming the log group {group:?}, got \
             {envelopes:?}"
        );
        let envelope = matching[0];

        assert_eq!(
            text_member(case, envelope, "messageType"),
            DATA_MESSAGE,
            "case {case:?}: an envelope carrying ingested log events is a {DATA_MESSAGE}, got \
             {envelope}"
        );
        assert_eq!(
            text_member(case, envelope, "owner"),
            account,
            "case {case:?}: the envelope's owner is the account holding the log group, got \
             {envelope}"
        );
        assert_eq!(
            text_member(case, envelope, "logStream"),
            row.stream(),
            "case {case:?}: the envelope names the stream the batch was written to, got \
             {envelope}"
        );
        let selecting: Vec<String> = envelope
            .get("subscriptionFilters")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            selecting,
            vec![row.filter()],
            "case {case:?}: the envelope names exactly the filter that selected the events, got \
             {envelope}"
        );

        // The rule itself: the envelope carries exactly the lines the row's pattern FORM selects,
        // in the order they were written, and none of the lines it rejects. Both halves are read
        // off the same batch's one hand-over, so a rejected line's absence is the form
        // withholding it rather than a delivery still in flight.
        let carried: Vec<String> = envelope
            .get("logEvents")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event.get("message").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            carried,
            row.selected(),
            "case {case:?}: expected the envelope to carry exactly the events the pattern {:?} \
             selects, got {carried:?}",
            row.pattern
        );
        for rejected in row.rejected() {
            assert!(
                !carried.contains(&rejected),
                "case {case:?}: the pattern {:?} rejects {rejected:?}, so the envelope must not \
                 carry it, got {carried:?}",
                row.pattern
            );
        }
    }
}
