// SPDX-License-Identifier: CC0-1.0

//! What Amazon AppFlow's `StopFlow` does to a stored flow, driven over the wire.
//!
//! RULE: `StopFlow` DISARMS the armed trigger of a flow that runs ITSELF — "[f]or schedule
//! and event-triggered flows, this operation deactivates the flow"
//! (`smithy_query(query="doc", shape="StopFlow")`) — which leaves that flow `Suspended`, the
//! modelled status of a flow whose trigger is no longer armed
//! (`smithy_query(query="shape", shape="FlowStatus")`), and answers with the flow's ARN and
//! that same status, which `DescribeFlow` then reports. A flow with NO armed trigger left to
//! disarm — one whose trigger was never armed or is already disarmed, and one that runs only
//! when a caller asks for it — is refused as the state conflict it is, with the flow left
//! exactly where it was; a flow name the account does not hold is refused as not found.
//! Deactivating a flow never re-defines it and never runs it.
//!
//! Both services are mounted on the ONE engine the router mounts them on, because a flow is
//! stored only when the Amazon S3 locations it names are ones AppFlow could use: the AppFlow
//! calls go over the wire as the `restJson1` requests a client sends, the bucket and its
//! policy as the path-style Amazon S3 requests a client sends.
//!
//! The rows differ along what the rule generalises over: the trigger type (two schedules at
//! different frequencies, an event trigger, an on-demand flow), how many arm/disarm calls
//! precede the call under test, and the status those calls leave the flow in.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and the one the buckets are created in.
const REGION: &str = "us-east-1";

/// The status Amazon S3 answers a stored `PutBucketPolicy` with: the write has no body.
const POLICY_STORED: u16 = 204;

/// The actions a flow run takes on the two ends of a transfer, which the bucket policy
/// AppFlow asks a caller to place on a location it reads or writes grants.
const EVERY_RUN_ACTION: [&str; 3] = ["s3:ListBucket", "s3:GetObject", "s3:PutObject"];

/// One wire answer: the HTTP status, the wire error code (empty on success) and the body.
struct Answer {
    status: u16,
    error: String,
    body: Bytes,
}

impl Answer {
    /// The body as JSON, or `Value::Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn string(&self, member: &str) -> String {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The body as text, for the message an assertion reports when it fails.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Amazon AppFlow and Amazon S3 on ONE engine, with request authentication off so each call
/// carries a credential scope — which is what names the Region and the service — without a
/// computed signature.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_appflow_sandstoneconfigurationservicelambda::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router }
    }

    /// One call of `method` to `path` on `signing_name`'s endpoint in [`REGION`].
    async fn call(&self, method: &str, signing_name: &str, path: &str, body: Vec<u8>) -> Answer {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{signing_name}.{REGION}.amazonaws.com{path}"))
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let (parts, body) = self.router.dispatch(request).await.into_parts();
        let error = parts
            .headers
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.rsplit('/').next().unwrap_or(value).to_string())
            .or_else(|| {
                serde_json::from_slice::<Value>(&body)
                    .ok()?
                    .get("__type")?
                    .as_str()
                    .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
            })
            .unwrap_or_default();
        Answer {
            status: parts.status.as_u16(),
            error,
            body,
        }
    }

    /// One AppFlow operation, by the URI its `@http` trait binds it to.
    async fn appflow(&self, path: &str, body: &Value) -> Answer {
        self.call(
            "POST",
            "appflow",
            path,
            serde_json::to_vec(body).expect("the request body serializes"),
        )
        .await
    }

    /// `PUT /<bucket>` then `PUT /<bucket>?policy` — a bucket carrying the policy AppFlow
    /// asks a caller to place on a location it reads or writes.
    async fn bucket_granting_appflow(&self, bucket: &str) {
        let created = self
            .call("PUT", "s3", &format!("/{bucket}"), Vec::new())
            .await;
        assert_eq!(
            created.status,
            200,
            "expected CreateBucket of {bucket:?} to succeed, got {} {:?}",
            created.status,
            created.text()
        );
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowAppFlow",
                "Effect": "Allow",
                "Principal": { "Service": "appflow.amazonaws.com" },
                "Action": EVERY_RUN_ACTION,
                "Resource": [
                    format!("arn:aws:s3:::{bucket}"),
                    format!("arn:aws:s3:::{bucket}/*"),
                ],
            }],
        });
        let placed = self
            .call(
                "PUT",
                "s3",
                &format!("/{bucket}?policy"),
                policy.to_string().into_bytes(),
            )
            .await;
        assert_eq!(
            placed.status,
            POLICY_STORED,
            "expected the policy on {bucket:?} to be stored, got {} {:?}",
            placed.status,
            placed.text()
        );
    }

    /// The flow's status as `DescribeFlow` reports it — the status read through the call
    /// that did not write it.
    async fn described_status(&self, case: &str, flow_name: &str) -> String {
        let described = self
            .appflow("/describe-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_eq!(
            described.status,
            200,
            "case {case:?}: expected DescribeFlow of {flow_name:?} to succeed, got {} {:?}",
            described.status,
            described.text()
        );
        described.string("flowStatus")
    }
}

/// The two Amazon S3 locations every flow here names: a flow is stored only when both ends
/// are locations AppFlow could use.
const SOURCE_BUCKET: &str = "appflow-deactivation-source";
const DESTINATION_BUCKET: &str = "appflow-deactivation-destination";

/// A trigger that runs the flow on a schedule, at `expression`.
fn scheduled_trigger(expression: &str) -> Value {
    json!({
        "triggerType": "Scheduled",
        "triggerProperties": {
            "Scheduled": { "scheduleExpression": expression, "dataPullMode": "Incremental" }
        }
    })
}

/// A trigger that runs the flow on an event in the source application.
fn event_trigger() -> Value {
    json!({ "triggerType": "Event" })
}

/// A flow that is run only when a caller asks for it, so it has no trigger at all.
fn on_demand_trigger() -> Value {
    json!({ "triggerType": "OnDemand" })
}

/// `POST /create-flow` for a flow between the two fixture locations, with `trigger` as its
/// trigger and `description` as its own prose.
async fn create_flow(
    emulator: &Emulator,
    case: &str,
    flow_name: &str,
    trigger: Value,
    description: &str,
) -> Answer {
    let created = emulator
        .appflow(
            "/create-flow",
            &json!({
                "flowName": flow_name,
                "description": description,
                "triggerConfig": trigger,
                "sourceFlowConfig": {
                    "connectorType": "S3",
                    "sourceConnectorProperties": {
                        "S3": { "bucketName": SOURCE_BUCKET, "bucketPrefix": "inbound" }
                    }
                },
                "destinationFlowConfigList": [{
                    "connectorType": "S3",
                    "destinationConnectorProperties": {
                        "S3": { "bucketName": DESTINATION_BUCKET, "bucketPrefix": "outbound" }
                    }
                }],
                "tasks": [{
                    "taskType": "Map",
                    "sourceFields": ["id"],
                    "destinationField": "id",
                    "connectorOperator": { "S3": "PROJECTION" },
                    "taskProperties": {}
                }],
            }),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected CreateFlow of {flow_name:?} to succeed, got {} {:?}",
        created.status,
        created.text()
    );
    created
}

/// An emulator with both fixture locations standing.
async fn standing_emulator() -> Emulator {
    let emulator = Emulator::new();
    emulator.bucket_granting_appflow(SOURCE_BUCKET).await;
    emulator.bucket_granting_appflow(DESTINATION_BUCKET).await;
    emulator
}

/// A flow's ARN, as AppFlow mints it: the service, the Region the call was signed for, the
/// account the emulator presents, and the flow resource named by the flow's own name.
fn flow_arn(flow_name: &str) -> String {
    format!(
        "arn:aws:appflow:{REGION}:{account}:flow/{flow_name}",
        account = engine::EMULATOR_ACCOUNT_ID
    )
}

/// What `StopFlow` is expected to answer, and the status it leaves the flow in.
struct Expected {
    status: u16,
    /// The wire error code, empty when the call succeeds.
    error: &'static str,
    /// The status `DescribeFlow` reports once the call has been answered.
    status_after: &'static str,
}

/// One row of the deactivation table.
struct Row {
    case: &'static str,
    trigger: Value,
    /// The arm/disarm calls made before the call under test, in order.
    before: &'static [&'static str],
    /// The status those calls leave the flow in.
    status_before: &'static str,
    expected: Expected,
}

/// How AppFlow words the refusal of a call the flow's current state cannot serve: the fixed
/// sentence, then the flow name taken verbatim from the request.
fn already_deactivated(flow_name: &str) -> String {
    format!("Conflict executing request: Flow is already deactivated: {flow_name}")
}

/// The deactivation table: the status `StopFlow` leaves a flow in, for every state a flow's
/// trigger can be in when the call arrives.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a scheduled flow whose schedule was armed",
            trigger: scheduled_trigger("rate(6hours)"),
            before: &["/start-flow"],
            status_before: "Active",
            expected: Expected {
                status: 200,
                error: "",
                status_after: "Suspended",
            },
        },
        Row {
            case: "a flow on a different schedule, armed",
            trigger: scheduled_trigger("rate(30minutes)"),
            before: &["/start-flow"],
            status_before: "Active",
            expected: Expected {
                status: 200,
                error: "",
                status_after: "Suspended",
            },
        },
        Row {
            case: "an event-triggered flow whose trigger was armed",
            trigger: event_trigger(),
            before: &["/start-flow"],
            status_before: "Active",
            expected: Expected {
                status: 200,
                error: "",
                status_after: "Suspended",
            },
        },
        Row {
            case: "a scheduled flow armed, disarmed and armed again",
            trigger: scheduled_trigger("rate(6hours)"),
            before: &["/start-flow", "/stop-flow", "/start-flow"],
            status_before: "Active",
            expected: Expected {
                status: 200,
                error: "",
                status_after: "Suspended",
            },
        },
        Row {
            case: "a scheduled flow whose schedule was never armed",
            trigger: scheduled_trigger("rate(6hours)"),
            before: &[],
            status_before: "Draft",
            expected: Expected {
                status: 409,
                error: "ConflictException",
                status_after: "Draft",
            },
        },
        Row {
            case: "an event-triggered flow whose trigger was never armed",
            trigger: event_trigger(),
            before: &[],
            status_before: "Draft",
            expected: Expected {
                status: 409,
                error: "ConflictException",
                status_after: "Draft",
            },
        },
        Row {
            case: "a scheduled flow whose schedule is already disarmed",
            trigger: scheduled_trigger("rate(6hours)"),
            before: &["/start-flow", "/stop-flow"],
            status_before: "Suspended",
            expected: Expected {
                status: 409,
                error: "ConflictException",
                status_after: "Suspended",
            },
        },
        Row {
            case: "a flow that runs only on demand, which has no trigger to disarm",
            trigger: on_demand_trigger(),
            before: &[],
            status_before: "Active",
            expected: Expected {
                status: 409,
                error: "ConflictException",
                status_after: "Active",
            },
        },
    ]
}

/// `StopFlow` disarms an armed trigger and refuses a flow that has none, and either way the
/// status it answers with is the status `DescribeFlow` then reports.
#[tokio::test]
async fn stop_flow_disarms_an_armed_trigger_and_refuses_a_flow_that_has_none() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let emulator = standing_emulator().await;
        let flow_name = format!("deactivation-flow-{index}");
        create_flow(
            &emulator,
            case,
            &flow_name,
            row.trigger,
            "a flow whose trigger this case deactivates",
        )
        .await;

        for path in row.before {
            let answer = emulator
                .appflow(path, &json!({ "flowName": flow_name }))
                .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: expected {path} to succeed while preparing the flow, got {} {:?}",
                answer.status,
                answer.text()
            );
        }
        let prepared = emulator.described_status(case, &flow_name).await;
        assert_eq!(
            prepared, row.status_before,
            "case {case:?}: expected the prepared flow to be {}, got {prepared:?}",
            row.status_before,
        );

        let stopped = emulator
            .appflow("/stop-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_eq!(
            (stopped.status, stopped.error.as_str()),
            (row.expected.status, row.expected.error),
            "case {case:?}: expected StopFlow to answer {} {:?}, got {} {:?}",
            row.expected.status,
            row.expected.error,
            stopped.status,
            stopped.text()
        );
        if row.expected.error.is_empty() {
            // A served call answers with the status it left the flow in, and with the flow's
            // own ARN.
            assert_eq!(
                stopped.string("flowStatus"),
                row.expected.status_after,
                "case {case:?}: expected StopFlow to report {}, got {:?}",
                row.expected.status_after,
                stopped.text()
            );
            assert_eq!(
                stopped.string("flowArn"),
                flow_arn(&flow_name),
                "case {case:?}: expected StopFlow to report the flow's ARN, got {:?}",
                stopped.text()
            );
        } else {
            // A refused call states WHICH conflict it is, in AppFlow's own sentence.
            assert_eq!(
                stopped.string("message"),
                already_deactivated(&flow_name),
                "case {case:?}: expected AppFlow's conflict sentence, got {:?}",
                stopped.text()
            );
        }
        let left_at = emulator.described_status(case, &flow_name).await;
        assert_eq!(
            left_at, row.expected.status_after,
            "case {case:?}: expected DescribeFlow to report {} after the call, got {left_at:?}",
            row.expected.status_after,
        );
    }
}

/// Deactivating a flow moves only its STATUS: the stored definition is answered unchanged
/// afterwards, and no run was recorded — disarming a trigger does not run the flow.
#[tokio::test]
async fn stop_flow_moves_only_the_status_of_the_flow_it_deactivates() {
    let case = "a scheduled flow armed and then deactivated";
    let emulator = standing_emulator().await;
    let flow_name = "deactivation-definition-flow";
    let description = "the definition this case expects to survive a deactivation";
    create_flow(
        &emulator,
        case,
        flow_name,
        scheduled_trigger("rate(6hours)"),
        description,
    )
    .await;
    let armed = emulator
        .appflow("/start-flow", &json!({ "flowName": flow_name }))
        .await;
    assert_eq!(
        armed.status,
        200,
        "case {case:?}: expected StartFlow to arm the schedule, got {} {:?}",
        armed.status,
        armed.text()
    );

    let before = emulator
        .appflow("/describe-flow", &json!({ "flowName": flow_name }))
        .await;
    assert_eq!(
        before.status,
        200,
        "case {case:?}: expected DescribeFlow to succeed, got {} {:?}",
        before.status,
        before.text()
    );

    let stopped = emulator
        .appflow("/stop-flow", &json!({ "flowName": flow_name }))
        .await;
    assert_eq!(
        (stopped.status, stopped.string("flowStatus").as_str()),
        (200, "Suspended"),
        "case {case:?}: expected StopFlow to suspend the flow, got {} {:?}",
        stopped.status,
        stopped.text()
    );

    let after = emulator
        .appflow("/describe-flow", &json!({ "flowName": flow_name }))
        .await;
    assert_eq!(
        after.status,
        200,
        "case {case:?}: expected DescribeFlow to succeed, got {} {:?}",
        after.status,
        after.text()
    );
    // Every member of the stored DEFINITION, and every value the create settled, is answered
    // exactly as it was before the deactivation.
    for member in [
        "flowArn",
        "flowName",
        "description",
        "triggerConfig",
        "sourceFlowConfig",
        "destinationFlowConfigList",
        "tasks",
        "createdAt",
        "createdBy",
        "lastUpdatedAt",
        "lastUpdatedBy",
        "schemaVersion",
    ] {
        assert_eq!(
            after.json().get(member),
            before.json().get(member),
            "case {case:?}: expected {member} to be untouched by StopFlow, got {:?}",
            after.json().get(member)
        );
    }
    assert_eq!(
        after.string("description"),
        description,
        "case {case:?}: expected the flow's own prose to be answered, got {:?}",
        after.text()
    );

    let runs = emulator
        .appflow(
            "/describe-flow-execution-records",
            &json!({ "flowName": flow_name }),
        )
        .await;
    assert_eq!(
        runs.json()
            .get("flowExecutions")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "case {case:?}: expected deactivating the flow to have run nothing, got {:?}",
        runs.text()
    );
}

/// `StopFlow` on a name the account holds no flow under is refused as not found, in AppFlow's
/// own sentence — a flow that was never created and one a delete has taken away are the same
/// absence to the caller.
#[tokio::test]
async fn stop_flow_naming_a_flow_the_account_does_not_hold_is_not_found() {
    let emulator = standing_emulator().await;

    let deleted_flow = "deactivation-deleted-flow";
    create_flow(
        &emulator,
        "a flow a delete took away",
        deleted_flow,
        on_demand_trigger(),
        "a flow this case deletes before deactivating it",
    )
    .await;
    let deleted = emulator
        .appflow(
            "/delete-flow",
            &json!({ "flowName": deleted_flow, "forceDelete": true }),
        )
        .await;
    assert_eq!(
        deleted.status,
        200,
        "expected the forced DeleteFlow to succeed, got {} {:?}",
        deleted.status,
        deleted.text()
    );

    for (case, flow_name) in [
        ("a flow nothing ever created", "deactivation-absent-flow"),
        ("a flow a delete took away", deleted_flow),
    ] {
        let refused = emulator
            .appflow("/stop-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_eq!(
            (refused.status, refused.error.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected StopFlow to answer 404 ResourceNotFoundException, got {} \
             {:?}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.string("message"),
            format!("There is no flow with the name {flow_name}"),
            "case {case:?}: expected AppFlow's not-found sentence, got {:?}",
            refused.text()
        );
    }
}
