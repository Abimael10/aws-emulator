// SPDX-License-Identifier: CC0-1.0

//! Amazon AppFlow resolves every resource a request NAMES, driven over the wire.
//!
//! RULE: a name a request uses to address an AppFlow resource — the `connectorProfileName`
//! a flow definition's source and each of its destinations name, and the `flowName` a flow
//! read addresses — is resolved against the account's own store before the request is
//! served, and a name that resolves to nothing is refused with the modelled
//! `ResourceNotFoundException` (`errorKind` client, `httpError` 404 on `CreateFlow`,
//! `UpdateFlow` and `DescribeFlow` — `smithy_query(query="operation",
//! operation="CreateFlow,DescribeFlow,UpdateFlow")`), carrying the `x-amzn-errortype`
//! header that names the error shape and AppFlow's own sentence for that resource kind,
//! and nothing the refused request would have written reaches state.
//!
//! The rows differ along what the rule generalises over: WHICH resource kind the missing
//! name addresses (a connector profile, a flow), WHERE in the definition the name sits
//! (the source, the only destination, the second of two destinations, both ends at once),
//! how the name misses (never created, taken away by a delete, differing from a held name
//! only in case) and which operation does the resolving (`CreateFlow`, `UpdateFlow`,
//! `DescribeFlow`).
//!
//! Both services are mounted on the ONE engine the router mounts them on, because a flow is
//! stored only when the Amazon S3 locations it names are ones AppFlow could use: the AppFlow
//! calls go over the wire as the `restJson1` requests a client sends, the buckets and their
//! policies as the path-style Amazon S3 requests a client sends.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_appflow_sandstoneconfigurationservicelambda::ops::types::ConnectorProfile;
use service_com_amazonaws_appflow_sandstoneconfigurationservicelambda::state::service_state;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and the one the buckets are created in.
const REGION: &str = "us-east-1";

/// The status Amazon S3 answers a stored `PutBucketPolicy` with: the write has no body.
const POLICY_STORED: u16 = 204;

/// The actions a flow run takes on the two ends of a transfer, which the bucket policy
/// AppFlow asks a caller to place on a location it reads or writes grants.
const EVERY_RUN_ACTION: [&str; 3] = ["s3:ListBucket", "s3:GetObject", "s3:PutObject"];

/// The connector profile this account HOLDS, and the connector it is a profile for.
///
/// Seeded into the store directly because `CreateConnectorProfile` is still a scaffold
/// stub: a flow definition that names a profile is served only in an account that holds it
/// ("You must create a connector profile before calling this API" —
/// `smithy_query(query="doc", shape="CreateFlow")`), so the positive rows need one profile
/// to be there while every negative row names one that is not.
const HELD_PROFILE: &str = "salesforce-profile";
const HELD_PROFILE_CONNECTOR_TYPE: &str = "Salesforce";

/// The two Amazon S3 locations the flows here name: a flow is stored only when both ends
/// are locations AppFlow could use.
const SOURCE_BUCKET: &str = "appflow-lookups-source";
const DESTINATION_BUCKET: &str = "appflow-lookups-destination";

/// One wire answer: the HTTP status, the error shape the `x-amzn-errortype` header names
/// (absent on a served call) and the body.
struct Answer {
    status: u16,
    /// The `x-amzn-errortype` header's shape name, read from the HEADER alone — never
    /// recovered from the body — because the header is itself part of what a refusal owes
    /// the client.
    error_type: Option<String>,
    body: Bytes,
}

impl Answer {
    /// The body as JSON, or `Value::Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn member(&self, member: &str) -> Value {
        self.json().get(member).cloned().unwrap_or(Value::Null)
    }

    fn string(&self, member: &str) -> String {
        self.member(member)
            .as_str()
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
        if let Some(state) = service_state(&*engine) {
            state.lock().connector_profiles.insert(
                HELD_PROFILE.to_string(),
                ConnectorProfile {
                    connector_profile_name: Some(HELD_PROFILE.to_string()),
                    connector_type: Some(HELD_PROFILE_CONNECTOR_TYPE.to_string()),
                    ..ConnectorProfile::default()
                },
            );
        }
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
        let error_type = parts
            .headers
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.rsplit('/').next().unwrap_or(value).to_string());
        Answer {
            status: parts.status.as_u16(),
            error_type,
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

    /// The flows the account holds, by name, as `ListFlows` enumerates them.
    async fn listed_flow_names(&self) -> Vec<String> {
        let listed = self.appflow("/list-flows", &json!({})).await;
        assert_eq!(
            listed.status,
            200,
            "expected ListFlows to succeed, got {} {:?}",
            listed.status,
            listed.text()
        );
        listed
            .member("flows")
            .as_array()
            .map(|flows| {
                flows
                    .iter()
                    .map(|flow| {
                        flow.get("flowName")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// An emulator with both fixture locations standing and the held connector profile in the
/// account.
async fn standing_emulator() -> Emulator {
    let emulator = Emulator::new();
    emulator.bucket_granting_appflow(SOURCE_BUCKET).await;
    emulator.bucket_granting_appflow(DESTINATION_BUCKET).await;
    emulator
}

/// An Amazon S3 source location, which is a connector AppFlow vends and so names no
/// connector profile at all.
fn s3_source() -> Value {
    json!({
        "connectorType": "S3",
        "sourceConnectorProperties": {
            "S3": { "bucketName": SOURCE_BUCKET, "bucketPrefix": "inbound" }
        }
    })
}

/// An Amazon S3 destination location, which likewise names no connector profile.
fn s3_destination() -> Value {
    json!({
        "connectorType": "S3",
        "destinationConnectorProperties": {
            "S3": { "bucketName": DESTINATION_BUCKET, "bucketPrefix": "outbound" }
        }
    })
}

/// A source on a connector that is reached THROUGH a connector profile, named by
/// `connector_profile_name`.
fn salesforce_source(connector_profile_name: &str) -> Value {
    json!({
        "connectorType": "Salesforce",
        "apiVersion": "57.0",
        "connectorProfileName": connector_profile_name,
        "sourceConnectorProperties": { "Salesforce": { "object": "Account" } }
    })
}

/// A destination on a connector that is reached THROUGH a connector profile.
fn salesforce_destination(connector_profile_name: &str) -> Value {
    json!({
        "connectorType": "Salesforce",
        "connectorProfileName": connector_profile_name,
        "destinationConnectorProperties": { "Salesforce": { "object": "Contact" } }
    })
}

/// A trigger that runs the flow only when a caller asks for it.
fn on_demand_trigger() -> Value {
    json!({ "triggerType": "OnDemand" })
}

/// A `Map` task moving one field, with an operator the model lists for the connector
/// (`smithy_query(query="shape", shape="S3ConnectorOperator")` — `PROJECTION`).
fn map_task() -> Value {
    json!({
        "taskType": "Map",
        "sourceFields": ["id"],
        "destinationField": "id",
        "connectorOperator": { "S3": "PROJECTION" },
        "taskProperties": {}
    })
}

/// A whole flow definition, as `CreateFlow` and `UpdateFlow` both carry it.
fn definition(flow_name: &str, source: &Value, destinations: &Value) -> Value {
    json!({
        "flowName": flow_name,
        "triggerConfig": on_demand_trigger(),
        "sourceFlowConfig": source,
        "destinationFlowConfigList": destinations,
        "tasks": [map_task()],
    })
}

/// How AppFlow words the refusal of a request naming a connector profile the account does
/// not hold: the fixed sentence around the profile name taken verbatim from the request.
fn no_such_connector_profile(connector_profile_name: &str) -> String {
    format!("Connector Profile with name {connector_profile_name} does not exist.")
}

/// How AppFlow words the refusal of a request addressing a flow the account does not hold:
/// the fixed sentence, then the flow name taken verbatim from the request.
fn no_such_flow(flow_name: &str) -> String {
    format!("There is no flow with the name {flow_name}")
}

/// Every assertion a refusal owes: the status, the `x-amzn-errortype` header and the
/// message.
fn assert_not_found(case: &str, what: &str, answer: &Answer, message: &str) {
    assert_eq!(
        answer.status,
        404,
        "case {case:?}: expected {what} to answer 404, got {} {:?}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.error_type.as_deref(),
        Some("ResourceNotFoundException"),
        "case {case:?}: expected {what} to carry the modelled error shape in \
         x-amzn-errortype, got {:?}",
        answer.error_type
    );
    assert_eq!(
        answer.string("message"),
        message,
        "case {case:?}: expected {what} to state AppFlow's sentence, got {:?}",
        answer.text()
    );
}

/// One row of the unresolved-connector-profile table: the definition's two ends, and the
/// profile name the refusal has to name.
struct ProfileRow {
    case: &'static str,
    source: Value,
    destinations: Value,
    /// The profile the account does not hold, which the refusal names.
    missing: String,
}

/// The unresolved-connector-profile table. The rows differ along WHERE the definition names
/// the profile the account does not hold, how many profiles it names, and how the name
/// misses.
fn profile_rows() -> Vec<ProfileRow> {
    vec![
        ProfileRow {
            case: "the source names a profile nothing created",
            source: salesforce_source("no-such-source-profile"),
            destinations: json!([s3_destination()]),
            missing: "no-such-source-profile".to_string(),
        },
        ProfileRow {
            case: "the only destination names a profile nothing created",
            source: s3_source(),
            destinations: json!([salesforce_destination("no-such-destination-profile")]),
            missing: "no-such-destination-profile".to_string(),
        },
        ProfileRow {
            case: "the source holds its profile and the destination does not",
            source: salesforce_source(HELD_PROFILE),
            destinations: json!([salesforce_destination("no-such-paired-profile")]),
            missing: "no-such-paired-profile".to_string(),
        },
        ProfileRow {
            case: "the second of two destinations names a profile nothing created",
            source: s3_source(),
            destinations: json!([
                s3_destination(),
                salesforce_destination("no-such-second-destination-profile"),
            ]),
            missing: "no-such-second-destination-profile".to_string(),
        },
        ProfileRow {
            case: "both ends name a profile nothing created, and the source is named first",
            source: salesforce_source("no-such-first-profile"),
            destinations: json!([salesforce_destination("no-such-last-profile")]),
            missing: "no-such-first-profile".to_string(),
        },
        ProfileRow {
            case: "the name differs from a held profile only in case",
            source: salesforce_source("Salesforce-Profile"),
            destinations: json!([s3_destination()]),
            missing: "Salesforce-Profile".to_string(),
        },
    ]
}

/// `CreateFlow` resolves every connector profile the definition names, refuses the first one
/// the account does not hold as not found, and writes no flow — so the name the refused
/// create carried is still a name the account holds no flow under.
#[tokio::test]
async fn create_flow_refuses_a_definition_naming_a_connector_profile_the_account_does_not_hold() {
    for (index, row) in profile_rows().into_iter().enumerate() {
        let case = row.case;
        let emulator = standing_emulator().await;
        let flow_name = format!("lookups-refused-flow-{index}");

        let refused = emulator
            .appflow(
                "/create-flow",
                &definition(&flow_name, &row.source, &row.destinations),
            )
            .await;
        assert_not_found(
            case,
            "CreateFlow",
            &refused,
            &no_such_connector_profile(&row.missing),
        );

        // Nothing was written: the read of that one name misses, and the account holds no
        // flow at all.
        let described = emulator
            .appflow("/describe-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_not_found(
            case,
            "the DescribeFlow after the refused create",
            &described,
            &no_such_flow(&flow_name),
        );
        assert_eq!(
            emulator.listed_flow_names().await,
            Vec::<String>::new(),
            "case {case:?}: expected the refused create to leave the account with no flow"
        );
    }
}

/// The other side of the same rule: a definition whose connector profiles the account DOES
/// hold — including one that names no profile at all, because its connectors are ones
/// AppFlow vends — is stored, and every member of it is answered by the read that did not
/// write it.
#[tokio::test]
async fn create_flow_stores_a_definition_whose_connector_profiles_resolve() {
    for (case, flow_name, source, destinations, expected_profile) in [
        (
            "a source reached through the profile the account holds",
            "lookups-held-profile-flow",
            salesforce_source(HELD_PROFILE),
            json!([s3_destination()]),
            Some(HELD_PROFILE),
        ),
        (
            "a definition naming no connector profile at all",
            "lookups-vended-connector-flow",
            s3_source(),
            json!([s3_destination()]),
            None,
        ),
    ] {
        let emulator = standing_emulator().await;
        let created = emulator
            .appflow("/create-flow", &definition(flow_name, &source, &destinations))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected CreateFlow to store the flow, got {} {:?}",
            created.status,
            created.text()
        );

        let described = emulator
            .appflow("/describe-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_eq!(
            described.status,
            200,
            "case {case:?}: expected DescribeFlow to read the stored flow, got {} {:?}",
            described.status,
            described.text()
        );
        assert_eq!(
            described
                .member("sourceFlowConfig")
                .get("connectorProfileName")
                .and_then(Value::as_str),
            expected_profile,
            "case {case:?}: expected the stored source to name {expected_profile:?}, got {:?}",
            described.text()
        );
        assert_eq!(
            emulator.listed_flow_names().await,
            vec![flow_name.to_string()],
            "case {case:?}: expected the account to hold exactly the stored flow"
        );
    }
}

/// `DescribeFlow` resolves the flow name it is given against the account's flows, and a name
/// that resolves to nothing is refused as not found — whatever made it miss.
#[tokio::test]
async fn describe_flow_refuses_a_flow_name_the_account_does_not_hold() {
    let emulator = standing_emulator().await;

    // A flow that is there, so each refusal below is about the NAME rather than about the
    // operation.
    let held_flow = "lookups-held-flow";
    let created = emulator
        .appflow(
            "/create-flow",
            &definition(held_flow, &s3_source(), &json!([s3_destination()])),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "expected CreateFlow of the held flow to succeed, got {} {:?}",
        created.status,
        created.text()
    );

    // A flow a forced delete took away.
    let deleted_flow = "lookups-deleted-flow";
    let created = emulator
        .appflow(
            "/create-flow",
            &definition(deleted_flow, &s3_source(), &json!([s3_destination()])),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "expected CreateFlow of the flow this case deletes to succeed, got {} {:?}",
        created.status,
        created.text()
    );
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

    // A name a create that was itself refused carried, which is the path a client takes
    // after a definition naming an absent connector profile was turned away.
    let refused_flow = "lookups-refused-create-flow";
    let refused_create = emulator
        .appflow(
            "/create-flow",
            &definition(
                refused_flow,
                &salesforce_source("no-such-profile-for-this-flow"),
                &json!([s3_destination()]),
            ),
        )
        .await;
    assert_not_found(
        "a create refused for an absent connector profile",
        "CreateFlow",
        &refused_create,
        &no_such_connector_profile("no-such-profile-for-this-flow"),
    );

    for (case, flow_name) in [
        ("a flow nothing ever created", "lookups-absent-flow"),
        ("a flow a forced delete took away", deleted_flow),
        ("a flow whose create was refused", refused_flow),
        (
            "a name differing from a held flow only in case",
            "Lookups-Held-Flow",
        ),
    ] {
        let refused = emulator
            .appflow("/describe-flow", &json!({ "flowName": flow_name }))
            .await;
        assert_not_found(case, "DescribeFlow", &refused, &no_such_flow(flow_name));
    }

    // And the flow that IS held still reads back, so the refusals above are the lookups
    // missing rather than the read path being broken.
    let described = emulator
        .appflow("/describe-flow", &json!({ "flowName": held_flow }))
        .await;
    assert_eq!(
        (described.status, described.string("flowName").as_str()),
        (200, held_flow),
        "expected the held flow to read back, got {} {:?}",
        described.status,
        described.text()
    );
}

/// The same resolution on the other operation that STORES a definition: `UpdateFlow` refuses
/// a re-definition naming a connector profile the account does not hold, and leaves the
/// stored definition exactly where it was.
#[tokio::test]
async fn update_flow_refuses_a_definition_naming_a_connector_profile_the_account_does_not_hold() {
    let case = "an update whose new source names a profile nothing created";
    let emulator = standing_emulator().await;
    let flow_name = "lookups-updated-flow";
    let created = emulator
        .appflow(
            "/create-flow",
            &definition(flow_name, &s3_source(), &json!([s3_destination()])),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected CreateFlow to store the flow, got {} {:?}",
        created.status,
        created.text()
    );
    let before = emulator
        .appflow("/describe-flow", &json!({ "flowName": flow_name }))
        .await;
    assert_eq!(
        before.status,
        200,
        "case {case:?}: expected DescribeFlow to read the stored flow, got {} {:?}",
        before.status,
        before.text()
    );

    let refused = emulator
        .appflow(
            "/update-flow",
            &definition(
                flow_name,
                &salesforce_source("no-such-updated-profile"),
                &json!([s3_destination()]),
            ),
        )
        .await;
    assert_not_found(
        case,
        "UpdateFlow",
        &refused,
        &no_such_connector_profile("no-such-updated-profile"),
    );

    let after = emulator
        .appflow("/describe-flow", &json!({ "flowName": flow_name }))
        .await;
    for member in [
        "sourceFlowConfig",
        "destinationFlowConfigList",
        "triggerConfig",
        "tasks",
        "schemaVersion",
        "lastUpdatedAt",
    ] {
        assert_eq!(
            after.json().get(member),
            before.json().get(member),
            "case {case:?}: expected the refused update to leave {member} where it was, got {:?}",
            after.text()
        );
    }

    // And an update naming a profile the account HOLDS is served, so the refusal above is
    // the lookup missing rather than the update path being closed.
    let updated = emulator
        .appflow(
            "/update-flow",
            &definition(
                flow_name,
                &salesforce_source(HELD_PROFILE),
                &json!([s3_destination()]),
            ),
        )
        .await;
    assert_eq!(
        updated.status,
        200,
        "case {case:?}: expected the update naming the held profile to succeed, got {} {:?}",
        updated.status,
        updated.text()
    );
    let described = emulator
        .appflow("/describe-flow", &json!({ "flowName": flow_name }))
        .await;
    assert_eq!(
        described
            .member("sourceFlowConfig")
            .get("connectorProfileName")
            .and_then(Value::as_str),
        Some(HELD_PROFILE),
        "case {case:?}: expected the served update to have re-defined the source, got {:?}",
        described.text()
    );
}
