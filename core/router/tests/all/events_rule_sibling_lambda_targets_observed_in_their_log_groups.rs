// SPDX-License-Identifier: CC0-1.0

//! What a client SEES of the Lambda function targets of ONE EventBridge rule: the sibling the
//! function's own resource-based policy admits is RUN, and the handler's own output is readable
//! in that function's CloudWatch Logs log group, while a sibling no document admits leaves
//! nothing in its own group at all.
//!
//! The rule pinned here: **a `PutEvents` entry is accepted onto its bus (`FailedEntryCount` 0
//! and one `PutEventsResultEntry` carrying an `EventId` per entry, which is all `PutEvents`
//! models) and is then matched against every rule of that bus; each target of every matching
//! ENABLED rule is handed the WHOLE event envelope, whose `source`, `detail-type` and `detail`
//! are the ones the entry supplied and nothing else; and for a Lambda function target "for
//! Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
//! resource-based policies" (`PutTargets`), so the handover is made as the service principal
//! `events.amazonaws.com` and happens exactly where the FUNCTION's own policy allows THAT
//! principal `lambda:InvokeFunction` on it for the INVOKING rule. Every target is decided on
//! its own document, so two siblings of one rule are invoked independently — and because an
//! invoked function RUNS, what its handler printed is readable in the log group it logs to ("by
//! default, Lambda functions send logs to a default log group named
//! `/aws/lambda/<function name>`", `LoggingConfig$LogGroup`), which is how the permission
//! boundary becomes observable: the admitted sibling's group holds the line and the refused
//! one's holds nothing.**
//!
//! Five crates have to agree for that to hold — CloudFormation archives the inline handler into
//! a deployment package this emulator can run, EventBridge matches the entry and hands each
//! target over, Lambda decides each invocation against that function's policy and then runs the
//! handler, CloudWatch Logs holds what the run reported, and IAM answers for the execution role
//! the run's own log delivery is judged against — so the suite lives here rather than in any one
//! crate. Every observation is the one a client makes: what `PutEvents` answered and what each
//! function's log group holds, never an emulator internal.
//!
//! The rows differ along exactly what the rule generalises over. The TARGET table varies what
//! each sibling's document says — the invocation granted unscoped, the same grant scoped by
//! `SourceArn` to the invoking rule, the same scoped to another rule, another action of that
//! service for this principal, and no document at all — with all five wired as targets of the
//! SAME rule (the most targets a rule takes, which `PutTargets` answers
//! `LimitExceededException` past), so the admitted and the refused siblings are each other's
//! control. The ENTRY table varies the published envelope —
//! the form of its `detail` (flat members; a nested sub-object, an array, a number and a
//! boolean), and which member of it the rule's pattern does not name (`detail.state`, `source`,
//! `detail-type`) — with every entry published in ONE `PutEvents` request, so what an admitted
//! sibling logged is read against what the unmatched entries must never have delivered.
//!
//! `crate::events_lambda_target_resource_policy` is the suite for the handover's own recorded
//! outcome under each policy shape, and `crate::events_sns_lambda_delivery_observed_in_logs` for
//! a fan-out across two target KINDS; this one is the suite for the function siblings of ONE
//! rule, read where a client reads them — each function's own log group.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one the shared stack reader
/// ([`wait_for_stack_status`]) and the shared awsQuery helpers sign for.
const REGION: &str = "us-east-1";

/// The one stack this suite deploys, and the prefix every name it uses takes from it.
const STACK: &str = "events-sibling-targets-observed";

/// The envelope members the rule's pattern names: a `source`, a `detail-type` and the
/// `detail.state` nested inside the entry's own document.
const SOURCE: &str = "sim2real.events.sibling";
const DETAIL_TYPE: &str = "Sibling Target Probe";
const STATE_MEMBER: &str = "state";
const MATCHED_STATE: &str = "Deployed";

/// A `source`, a `detail-type` and a `detail.state` the pattern does NOT name.
const OTHER_SOURCE: &str = "sim2real.events.sibling.other";
const OTHER_DETAIL_TYPE: &str = "Sibling Target Probe Of Another Type";
const OTHER_STATE: &str = "InProgress";

/// The member of each entry's `detail` that tells one published entry from another.
const MARKER_MEMBER: &str = "probe";

/// The service principal EventBridge hands a resource-policy-governed target over as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The action an invocation of a function performs — what a grant for this service has to name
/// — and another action of that same service, which a row grants instead.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
const OTHER_ACTION: &str = "lambda:GetFunction";

/// Each target's retry allowance, at the published lower bound of both members
/// (`Target$RetryPolicy` -> `RetryPolicy`: `MaximumRetryAttempts` range 0..185,
/// `MaximumEventAgeInSeconds` range 60..86400), so a handover no document admits is given up on
/// rather than retried for the life of the test.
const MAXIMUM_RETRY_ATTEMPTS: i64 = 0;
const MAXIMUM_EVENT_AGE_IN_SECONDS: i64 = 60;

/// The execution role every declared function is created with, provisioned in IAM before the
/// stack so the template names a role that service really holds.
const ROLE_NAME: &str = "events-sibling-targets-exec";

/// The prefix the handler prints its envelope behind, which is what makes one of its lines
/// readable as a delivered envelope rather than as any other output of the run.
const ENVELOPE_PREFIX: &str = "ENVELOPE:";

/// How long a wait for work the CLOCK owes goes on — a function leaving its creation
/// transition, a delivery, a run's own log line.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until the thing it waits for is there or this deadline passes, so a slow or loaded host is
/// waited for and only something that never arrives at all fails the suite.
const WAIT_DEADLINE: Duration = Duration::from_secs(180);

/// How long the suite keeps LOOKING at a refused sibling's log group before concluding nothing
/// was ever delivered to it: a span the whole delivery wave has already been observed to fit
/// inside, since every admitted sibling has logged its envelopes by the time this starts.
const QUIET_BUDGET: Duration = Duration::from_secs(5);

/// The whole emulator as it is ASSEMBLED, on one engine: the log delivery an invoked function's
/// own run makes is dispatched back into this same router, so the groups this suite reads are
/// the groups those runs wrote.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `X-Amz-Target` RPC call of `signing_name`, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", &format!("AWSEvents.{operation}"), body).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`), with whatever status it
/// answered: a group no run has created yet is a refusal this suite reads THROUGH rather than
/// asserts on, because that is exactly what a client polling for a line that never came sees.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "logs", &format!("Logs_20140328.{operation}"), body).await
}

/// Create the execution role the template names, where IAM holds it: trusted by Lambda, and
/// granting the CloudWatch Logs calls a run's own log delivery makes — a role short of them
/// leaves nothing for a client to read, which is not the boundary under test.
async fn execution_role(router: &MultiServiceRouter) {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "write-its-own-logs"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
}

/// The handler every declared function runs: it prints the WHOLE envelope it was handed behind
/// [`ENVELOPE_PREFIX`], so what a delivery carried is readable in that function's log group.
fn handler_source() -> String {
    format!(
        "import json\n\n\ndef lambda_handler(event, context):\n    \
         print(\"{ENVELOPE_PREFIX}\" + json.dumps(event))\n    return True\n"
    )
}

/// What one target's function policy says before the entries are published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No `AddPermission` at all, so the function carries no policy.
    None,
    /// This service's principal allowed the invocation, unscoped.
    Invoke,
    /// The same grant, scoped by `SourceArn` to the rule that invokes it.
    InvokeFromThisRule,
    /// The same grant, scoped by `SourceArn` to another rule of the same bus.
    InvokeFromAnotherRule,
    /// Another action of this function's service, allowed to this service's principal.
    OtherActionForThisPrincipal,
}

/// One row of the target table: what its function's document says, and whether that function is
/// therefore invoked.
struct Target {
    case: &'static str,
    /// The suffix its function name and its target id take, which keeps each row's own log
    /// group apart from its siblings'.
    suffix: &'static str,
    grant: Grant,
    invoked: bool,
}

fn targets() -> Vec<Target> {
    vec![
        Target {
            case: "the policy allows this service the invocation, unscoped",
            suffix: "unscoped",
            grant: Grant::Invoke,
            invoked: true,
        },
        Target {
            case: "the grant is scoped by SourceArn to the invoking rule",
            suffix: "this-rule",
            grant: Grant::InvokeFromThisRule,
            invoked: true,
        },
        Target {
            case: "the grant is scoped by SourceArn to another rule",
            suffix: "other-rule",
            grant: Grant::InvokeFromAnotherRule,
            invoked: false,
        },
        Target {
            case: "the grant names another action of this function's service",
            suffix: "other-action",
            grant: Grant::OtherActionForThisPrincipal,
            invoked: false,
        },
        Target {
            case: "the function carries no resource-based policy at all",
            suffix: "ungranted",
            grant: Grant::None,
            invoked: false,
        },
    ]
}

/// The function name row `target` declares.
fn function_name(target: &Target) -> String {
    format!("{STACK}-{}", target.suffix)
}

/// One row of the entry table: one entry of the single `PutEvents` request, and whether the
/// rule's pattern matches it.
struct Entry {
    case: &'static str,
    /// The value of the `detail` member [`MARKER_MEMBER`], which tells this entry's envelope
    /// from every other one's.
    marker: &'static str,
    source: &'static str,
    detail_type: &'static str,
    /// Everything the entry's `detail` carries BESIDE its marker: the state the pattern is
    /// judged against, and whatever else this row's form adds.
    rest_of_detail: Value,
    matched: bool,
}

fn entries() -> Vec<Entry> {
    vec![
        Entry {
            case: "a flat detail the pattern matches",
            marker: "flat-detail",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            rest_of_detail: json!({ STATE_MEMBER: MATCHED_STATE, "eTag": "E1AAAAAAAAAAAA" }),
            matched: true,
        },
        Entry {
            case: "a matched detail carrying a nested sub-object, an array, a number and a \
                   boolean",
            marker: "nested-detail",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            rest_of_detail: json!({
                STATE_MEMBER: MATCHED_STATE,
                "attempt": 3,
                "aliases": ["one.example", "two.example"],
                "origin": { "domainName": "origin.example", "secure": true },
            }),
            matched: true,
        },
        Entry {
            case: "a detail whose state the pattern does not name",
            marker: "other-state",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            rest_of_detail: json!({ STATE_MEMBER: OTHER_STATE }),
            matched: false,
        },
        Entry {
            case: "a source the pattern does not name",
            marker: "other-source",
            source: OTHER_SOURCE,
            detail_type: DETAIL_TYPE,
            rest_of_detail: json!({ STATE_MEMBER: MATCHED_STATE }),
            matched: false,
        },
        Entry {
            case: "a detail-type the pattern does not name",
            marker: "other-detail-type",
            source: SOURCE,
            detail_type: OTHER_DETAIL_TYPE,
            rest_of_detail: json!({ STATE_MEMBER: MATCHED_STATE }),
            matched: false,
        },
    ]
}

/// The whole `detail` document row `entry` publishes: its marker beside the rest of its form.
fn detail_of(entry: &Entry) -> Value {
    let mut detail = entry
        .rest_of_detail
        .as_object()
        .cloned()
        .expect("every row's rest_of_detail is a JSON object");
    detail.insert(MARKER_MEMBER.to_string(), json!(entry.marker));
    Value::Object(detail)
}

/// The template the one stack is deployed with: one function per target row, each with the
/// handler above.
///
/// The functions come from a template because that is what archives inline source into a
/// deployment package this emulator can run: `Code.ZipFile` in a template is the function's
/// SOURCE TEXT while the API's member is the archive's bytes. The rule, its targets and the
/// grants are then wired through each service's OWN API below, the way a client wires them.
fn template(rows: &[Target]) -> String {
    let source = handler_source();
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    for (index, row) in rows.iter().enumerate() {
        let logical = format!("Handler{index}");
        resources.insert(
            logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(row),
                    "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 10,
                    "Code": { "ZipFile": source },
                },
            }),
        );
        outputs.insert(
            format!("Arn{index}"),
            json!({ "Value": { "Fn::GetAtt": [logical, "Arn"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// Deploy that stack and answer the ARN of each row's function, indexed as `rows` is.
async fn deployed_functions(router: &MultiServiceRouter, rows: &[Target]) -> Vec<String> {
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template(rows))],
    )
    .await;
    let (status, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "expected the handler stack to deploy, got {status} with {described}"
    );
    let published = outputs_of(&described);
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            published
                .get(&format!("Arn{index}"))
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected the stack to publish Arn{index}, got {described}",
                        row.case
                    )
                })
        })
        .collect()
}

/// Wait until Lambda reports every declared function `Active`: "During this time, you can't
/// invoke or modify the function" (`CreateFunction`), so a handover withheld before that would
/// be the lifecycle's refusal rather than the policy's.
async fn awaited_active(router: &MultiServiceRouter, rows: &[Target]) {
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    for row in rows {
        let name = function_name(row);
        loop {
            let (_, reported) = lambda(
                router,
                "GET",
                &format!("/2015-03-31/functions/{name}/configuration"),
                None,
            )
            .await;
            if reported.get("State").and_then(Value::as_str) == Some("Active") {
                break;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {:?}: Lambda never reported {name} Active, last answer {reported}",
                row.case
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
}

/// Add one statement to `function`'s resource-based policy, granting `principal` `action`,
/// scoped by `source_arn` when the row names one — which is how a client grants this service
/// the invocation.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
    principal: &str,
    action: &str,
    source_arn: Option<&str>,
) {
    let mut body = json!({
        "StatementId": statement_id,
        "Action": action,
        "Principal": principal,
    });
    if let Some(source_arn) = source_arn {
        body["SourceArn"] = json!(source_arn);
    }
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(body),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission({principal}, {action}), got \
         {status} with {answered}"
    );
}

/// Every envelope the log group `group` holds right now, parsed out of the text each line
/// carries behind [`ENVELOPE_PREFIX`]. A group no run has created is no envelopes rather than a
/// failure.
async fn envelopes(router: &MultiServiceRouter, group: &str) -> Vec<Value> {
    let (status, answered) =
        logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    if status != 200 {
        return Vec::new();
    }
    let held = answered["events"].as_array().cloned().unwrap_or_default();
    let mut parsed = Vec::new();
    for event in &held {
        let Some(message) = event["message"].as_str() else {
            continue;
        };
        let Some(at) = message.find(ENVELOPE_PREFIX) else {
            continue;
        };
        let text = message[at + ENVELOPE_PREFIX.len()..].trim();
        if let Ok(envelope) = serde_json::from_str::<Value>(text) {
            parsed.push(envelope);
        }
    }
    parsed
}

/// Poll `function`'s own log group until it holds `wanted` envelopes, and answer what it held
/// last — every envelope it has, whether that is `wanted` of them or not.
async fn awaited_envelopes(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    wanted: usize,
) -> Vec<Value> {
    let group = format!("/aws/lambda/{function}");
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    loop {
        let held = envelopes(router, &group).await;
        if held.len() >= wanted || Instant::now() >= giving_up_at {
            assert!(
                held.len() == wanted,
                "case {case:?}: expected {wanted} envelope(s) in {group}, got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Look at `function`'s own log group again and again for [`QUIET_BUDGET`], and answer the first
/// envelopes that ever appear in it — none at all, for a function nothing invoked.
async fn stayed_empty(router: &MultiServiceRouter, function: &str) -> Vec<Value> {
    let group = format!("/aws/lambda/{function}");
    let giving_up_at = Instant::now() + QUIET_BUDGET;
    loop {
        let held = envelopes(router, &group).await;
        if !held.is_empty() || Instant::now() >= giving_up_at {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every envelope of `held`, keyed by the marker its `detail` carries.
fn by_marker(case: &str, held: &[Value]) -> BTreeMap<String, Value> {
    let mut keyed = BTreeMap::new();
    for envelope in held {
        let marker = envelope["detail"][MARKER_MEMBER]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a delivered envelope to carry a marker in its \
                        detail, got {envelope}")
            })
            .to_string();
        keyed.insert(marker, envelope.clone());
    }
    keyed
}

/// The whole boundary, in one wave: one rule, five function targets differing only in what each
/// function's own document says, and one `PutEvents` request whose entries differ in whether the
/// rule's pattern matches them.
#[tokio::test]
async fn each_sibling_function_target_runs_only_where_its_own_policy_admits_eventbridge() {
    let rows = targets();
    let published_entries = entries();
    let router = build_router();
    execution_role(&router).await;
    let function_arns = deployed_functions(&router, &rows).await;
    awaited_active(&router, &rows).await;

    // The rule the entries are judged against, and a second rule of the same bus whose ARN is
    // what the `InvokeFromAnotherRule` row's grant is scoped to.
    let rule = format!("{STACK}-rule");
    let other_rule = format!("{STACK}-other-rule");
    let pattern = json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": { STATE_MEMBER: [MATCHED_STATE] },
    })
    .to_string();
    let created = events(
        &router,
        "PutRule",
        &json!({ "Name": rule, "EventPattern": pattern, "State": "ENABLED" }),
    )
    .await;
    let rule_arn = created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected PutRule to answer a RuleArn, got {created}"))
        .to_string();
    let created = events(
        &router,
        "PutRule",
        &json!({ "Name": other_rule, "EventPattern": pattern, "State": "ENABLED" }),
    )
    .await;
    let other_rule_arn = created["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected PutRule to answer a RuleArn, got {created}"))
        .to_string();

    for row in &rows {
        let case = row.case;
        let function = function_name(row);
        match row.grant {
            Grant::None => {}
            Grant::Invoke => {
                add_permission(
                    &router,
                    case,
                    &function,
                    "events-invoke",
                    EVENTS_PRINCIPAL,
                    INVOKE_ACTION,
                    None,
                )
                .await;
            }
            Grant::InvokeFromThisRule => {
                add_permission(
                    &router,
                    case,
                    &function,
                    "events-invoke-this-rule",
                    EVENTS_PRINCIPAL,
                    INVOKE_ACTION,
                    Some(&rule_arn),
                )
                .await;
            }
            Grant::InvokeFromAnotherRule => {
                add_permission(
                    &router,
                    case,
                    &function,
                    "events-invoke-other-rule",
                    EVENTS_PRINCIPAL,
                    INVOKE_ACTION,
                    Some(&other_rule_arn),
                )
                .await;
            }
            Grant::OtherActionForThisPrincipal => {
                add_permission(
                    &router,
                    case,
                    &function,
                    "events-other-action",
                    EVENTS_PRINCIPAL,
                    OTHER_ACTION,
                    Some(&rule_arn),
                )
                .await;
            }
        }
    }

    // Every row's function, wired as a target of the ONE rule, so the six documents are read
    // against the same matched entries.
    let wired: Vec<Value> = rows
        .iter()
        .zip(&function_arns)
        .map(|(row, arn)| {
            json!({
                "Id": row.suffix,
                "Arn": arn,
                "RetryPolicy": {
                    "MaximumRetryAttempts": MAXIMUM_RETRY_ATTEMPTS,
                    "MaximumEventAgeInSeconds": MAXIMUM_EVENT_AGE_IN_SECONDS,
                },
            })
        })
        .collect();
    let targeted = events(
        &router,
        "PutTargets",
        &json!({ "Rule": rule, "Targets": wired }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "expected every target to be accepted, got {targeted}"
    );

    // One request carrying every entry row, matched and unmatched alike.
    let payload: Vec<Value> = published_entries
        .iter()
        .map(|entry| {
            json!({
                "Source": entry.source,
                "DetailType": entry.detail_type,
                "Detail": detail_of(entry).to_string(),
            })
        })
        .collect();
    let answered = events(&router, "PutEvents", &json!({ "Entries": payload })).await;
    assert_eq!(
        answered["FailedEntryCount"].as_i64(),
        Some(0),
        "expected every entry to be accepted whatever its deliveries come to, got {answered}"
    );
    let receipts = answered["Entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        receipts.len(),
        published_entries.len(),
        "expected one result entry per published entry, got {answered}"
    );
    for (entry, receipt) in published_entries.iter().zip(&receipts) {
        let event_id = receipt["EventId"].as_str().unwrap_or_default();
        assert!(
            !event_id.is_empty(),
            "case {:?}: expected the accepted entry to carry an EventId, got {receipt}",
            entry.case
        );
    }

    let matched: Vec<&Entry> = published_entries.iter().filter(|row| row.matched).collect();

    // The admitted siblings: each one RAN, once per matched entry, and what it was handed is the
    // envelope the entry supplied.
    for row in rows.iter().filter(|row| row.invoked) {
        let case = row.case;
        let function = function_name(row);
        let held = awaited_envelopes(&router, case, &function, matched.len()).await;
        let keyed = by_marker(case, &held);
        for entry in &matched {
            let envelope = keyed.get(entry.marker).unwrap_or_else(|| {
                panic!(
                    "case {case:?}, entry {:?}: expected an envelope carrying {:?} in \
                     /aws/lambda/{function}, got {held:?}",
                    entry.case, entry.marker
                )
            });
            assert_eq!(
                envelope["source"],
                json!(entry.source),
                "case {case:?}, entry {:?}: expected the envelope's source to be the published \
                 one, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["detail-type"],
                json!(entry.detail_type),
                "case {case:?}, entry {:?}: expected the envelope's detail-type to be the \
                 published one, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["detail"],
                detail_of(entry),
                "case {case:?}, entry {:?}: expected the envelope's detail to be the published \
                 document unchanged, got {envelope}",
                entry.case
            );
        }
        for entry in published_entries.iter().filter(|row| !row.matched) {
            assert!(
                !keyed.contains_key(entry.marker),
                "case {case:?}, entry {:?}: expected no envelope for an entry the pattern does \
                 not match, got {held:?}",
                entry.case
            );
        }
    }

    // The refused siblings, read once the whole wave has been observed to have landed: their own
    // groups hold nothing, which is the only way a client tells a denied target from a permitted
    // one.
    for row in rows.iter().filter(|row| !row.invoked) {
        let case = row.case;
        let function = function_name(row);
        let held = stayed_empty(&router, &function).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected /aws/lambda/{function} to hold no envelope at all, got \
             {held:?}"
        );
    }
}
