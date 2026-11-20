// SPDX-License-Identifier: CC0-1.0

//! An HTTP api route whose integration is the first-class `Kinesis-PutRecord` subtype, driven
//! on an estate where every service the call passes through is mounted together.
//!
//! RULE: **an HTTP api route whose integration is `IntegrationType=AWS_PROXY` naming an
//! `IntegrationSubtype` ANSWERS every matching request with the outcome of that AWS service
//! action, performed as a session of the role its `CredentialsArn` names with the arguments its
//! `RequestParameters` map — so for `Kinesis-PutRecord` a role that holds the action is
//! answered `200` carrying that service's own document (the `ShardId` and `SequenceNumber`
//! Kinesis assigned) and ONE record lands on the named stream, carrying the mapped data as its
//! payload and the mapped key as its partition key, while a role that does not hold the action,
//! or a stream the account does not hold, is answered that service's own refusal — `400`
//! `AccessDeniedException` / `ResourceNotFoundException` — with nothing stored.**
//!
//! `Integration$IntegrationSubtype` is "supported only for HTTP API AWS_PROXY integrations.
//! Specifies the AWS service action to invoke", and `Integration$RequestParameters` is "a
//! key-value map specifying parameters that are passed to AWS_PROXY integrations" — the
//! arguments of that action, which for this subtype are the members of `PutRecordInput`
//! (`smithy_query(query="operation", operation="PutRecord")` on the Kinesis model). `Data` is a
//! BLOB, so the mapped value is the record's payload and the base64 a JSON protocol writes a
//! blob under is that wire's own encoding of it — "the data blob (the payload before
//! base64-encoding)" (`PutRecordInput$Data`) — which is why every assertion below compares the
//! record against the base64 of the bytes the client sent.
//!
//! Four crates have to agree for that to hold — API Gateway v2 resolves the request to the
//! route and performs the subtype, IAM holds the role and its policies, the token service mints
//! the session the call is made as, and Kinesis decides the write on that session's own
//! permissions and keeps the record — so the suite lives here rather than in any one crate.
//!
//! Nothing asserted here is a value this gateway fills in: the shard and sequence number the
//! route reports are read back off the record ANOTHER service holds, through its own
//! `ListShards`/`GetShardIterator`/`GetRecords` — at the very position the route reported,
//! which exactly one shard of the stream holds, because a sequence number identifies a record
//! within its shard (`Record$SequenceNumber`) — and a refusal is the text the service serving
//! the action stated.
//!
//! The rows span what the rule generalises over: where each parameter comes from (the request
//! body whole, a member selected out of it, a header, a path variable, a static value), what the
//! payload carries (ASCII, text outside ASCII, a key naming a different shard), whose refusal it
//! is (the role's policies, the stream named), and which half of a mapping a request can fail to
//! satisfy.
//!
//! What the owning crate's own suites pin and this one does not restate: the parameters the
//! CONTROL plane holds such an integration to
//! (`services/service-com-amazonaws-apigatewayv2-apigatewayv2/tests/all/integration_validation.rs`),
//! and every mapping expression and policy outcome over the other subtypes
//! (`.../tests/all/http_api_service_action_integration.rs`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region every call below is signed for, and the Region in the hosts of each service.
const REGION: &str = "us-east-1";

/// The subtype every route below names, and the action its role has to hold for the put to
/// happen.
const PUT_RECORD_SUBTYPE: &str = "Kinesis-PutRecord";
const PUT_RECORD_ACTION: &str = "kinesis:PutRecord";

/// The inline policy this suite writes a role's grant on.
const INLINE_POLICY: &str = "may-put";

/// The service principal API Gateway assumes a `CredentialsArn` role as.
const GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The service shape an `awsJson1_1` `X-Amz-Target` names an operation of the stream api under
/// (`smithy_query(query="service_info")` on that model).
const KINESIS_TARGET_PREFIX: &str = "Kinesis_20131202";

/// How many shards every stream below is created with, so a record's placement is one of
/// several and the read has to look where the put said.
const SHARD_COUNT: u64 = 4;

/// The status Kinesis reports for a stream whose create has fallen due.
const STREAM_ACTIVE: &str = "ACTIVE";

/// The status a performed action answers, and the media type its document is carried under.
const PERFORMED: u16 = 200;
const JSON_MEDIA_TYPE: &str = "application/json";

/// The status and the errors the stream api answers a client refusal under, relayed to the
/// caller as that service stated them (`smithy_query(query="operation", operation="PutRecord")`
/// lists both as client errors).
const ACTION_REFUSED: u16 = 400;
const ACCESS_DENIED: &str = "AccessDeniedException";
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The status and sentence the gateway answers for an integration that produced no response,
/// and for a request that cannot satisfy what its route's integration maps onto a parameter the
/// action requires.
const INTEGRATION_FAULT: u16 = 500;
const INTEGRATION_FAULT_MESSAGE: &str = "Internal Server Error";
const BAD_REQUEST: u16 = 400;
const BAD_REQUEST_MESSAGE: &str = "Bad Request";

/// The members a relayed refusal carries: the error shape, under the discriminator a JSON error
/// envelope names a shape in, and the text that service stated.
const SHAPE_MEMBER: &str = "__type";
const DETAIL_MEMBER: &str = "Message";

/// How long a clock-owed transition is waited for, and how often it is looked at while waiting.
/// Generous: the test waits for the CONDITION, and the budget is never the thing asserted.
const SETTLE_BUDGET: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// API Gateway v2, Amazon Kinesis Data Streams, IAM and the token service over ONE engine, as
/// `core/router/src/lib.rs` mounts them: the action the integration performs is dispatched back
/// into this same router, so the stream it writes to is the stream this suite created.
struct Estate {
    router: MultiServiceRouter,
}

impl Estate {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite exercises the services, not SigV4: a control-plane call carries a credential
        // scope so the router can name the target service, and the invoke of the published
        // address carries none, as a client of a published HTTP api endpoint does. The
        // INTEGRATION's own call is signed by the emulator, with the session it minted.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
    /// request. The signature is never checked here.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/{service}/aws4_request, \
             SignedHeaders=host, Signature=not-verified"
        )
    }

    /// One Kinesis call (`awsJson1_1`, `Kinesis_20131202.<Operation>`), as `(status, parsed
    /// body)`.
    async fn kinesis(&self, operation: &str, body: &Value) -> (u16, Value) {
        let host = format!("kinesis.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", host)
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("{KINESIS_TARGET_PREFIX}.{operation}"),
            )
            .header("authorization", Self::authorization("kinesis"))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, parsed_body(response.body()))
    }

    /// One Kinesis call that must be served.
    async fn kinesis_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, answered) = self.kinesis(operation, body).await;
        assert_eq!(
            status, PERFORMED,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {answered}"
        );
        answered
    }

    /// One awsQuery call to IAM that must be served.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("iam"))
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, PERFORMED,
            "case {case:?}: IAM {action} must succeed, got {status} {answered}"
        );
    }

    /// One API Gateway v2 control-plane call (`restJson1` under `/v2`), as `(status, parsed
    /// body)`.
    async fn control(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let payload = match &body {
            Some(value) => Bytes::from(value.to_string()),
            None => Bytes::new(),
        };
        let host = format!("apigateway.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization("apigateway"))
            .body(payload)
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, parsed_body(response.body()))
    }

    /// The role an integration's `CredentialsArn` names, trusted by API Gateway and carrying
    /// `granted` actions as its one inline identity policy — written with IAM's own operations,
    /// so the role a session is minted of is the record `iam:CreateRole` wrote. Answers its ARN.
    async fn role(&self, case: &str, name: &str, granted: Option<&str>) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": GATEWAY_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        if let Some(action) = granted {
            let document = json!({
                "Version": "2012-10-17",
                "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
            })
            .to_string();
            self.iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", name.to_string()),
                    ("PolicyName", INLINE_POLICY.to_string()),
                    ("PolicyDocument", document),
                ],
            )
            .await;
        }
        format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
    }

    /// A stream of [`SHARD_COUNT`] shards, created through Kinesis' own `CreateStream` and
    /// polled until its create has fallen due, so a put reaches a stream that serves shards.
    ///
    /// `on_demand` picks the capacity mode: an on-demand stream is created without a shard
    /// count and starts with as many shards as that mode opens, which is the same
    /// [`SHARD_COUNT`] (`StreamModeDetails$StreamMode`); a provisioned one asks for them.
    /// Either way the put is placed by the partition key, which is what both modes do by
    /// default.
    ///
    /// The wait is for the CONDITION: the summary is re-read until the status is `ACTIVE`, or
    /// until a budget far beyond any span this emulator owes the transition has passed.
    async fn stream(&self, case: &str, name: &str, on_demand: bool) {
        let created = if on_demand {
            json!({ "StreamName": name, "StreamModeDetails": { "StreamMode": "ON_DEMAND" } })
        } else {
            json!({ "StreamName": name, "ShardCount": SHARD_COUNT })
        };
        self.kinesis_ok(case, "CreateStream", &created).await;
        let deadline = Instant::now() + SETTLE_BUDGET;
        loop {
            let described = self
                .kinesis_ok(case, "DescribeStreamSummary", &json!({ "StreamName": name }))
                .await;
            if described["StreamDescriptionSummary"]["StreamStatus"] == json!(STREAM_ACTIVE) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {name} to reach {STREAM_ACTIVE}, got {described}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }

    /// An HTTP api carrying one `Kinesis-PutRecord` integration per `(route key, parameters)`
    /// pair, performed under `role_arn`, deployed to the `$default` stage with auto-deploy on;
    /// answers the address its `ApiEndpoint` announces.
    async fn api(&self, case: &str, role_arn: &str, routes: &[(&str, Value)]) -> String {
        let (status, api) = self
            .control(
                "POST",
                "/v2/apis",
                Some(json!({ "name": "ingest-api", "protocolType": "HTTP" })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateApi must answer 201, got {status} {api}"
        );
        let api_id = text(case, &api, "apiId");
        let endpoint = text(case, &api, "apiEndpoint");
        for (route_key, parameters) in routes {
            let (status, created) = self
                .control(
                    "POST",
                    &format!("/v2/apis/{api_id}/integrations"),
                    Some(json!({
                        "integrationType": "AWS_PROXY",
                        "integrationSubtype": PUT_RECORD_SUBTYPE,
                        "payloadFormatVersion": "1.0",
                        "credentialsArn": role_arn,
                        "requestParameters": parameters,
                    })),
                )
                .await;
            assert_eq!(
                status, 201,
                "case {case:?}: CreateIntegration must answer 201, got {status} {created}"
            );
            let integration_id = text(case, &created, "integrationId");
            // The reader a client comes back to holds the first-class integration as it was
            // configured, which is what the route's dispatch then performs.
            let (status, read) = self
                .control(
                    "GET",
                    &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
                    None,
                )
                .await;
            assert_eq!(
                status, PERFORMED,
                "case {case:?}: GetIntegration must answer 200, got {status} {read}"
            );
            for (member, expected) in [
                ("integrationType", json!("AWS_PROXY")),
                ("integrationSubtype", json!(PUT_RECORD_SUBTYPE)),
                ("credentialsArn", json!(role_arn)),
                ("requestParameters", parameters.clone()),
            ] {
                assert_eq!(
                    read[member], expected,
                    "case {case:?}: GetIntegration must report {member} as {expected}, got {read}"
                );
            }
            let (status, route) = self
                .control(
                    "POST",
                    &format!("/v2/apis/{api_id}/routes"),
                    Some(json!({
                        "routeKey": route_key,
                        "target": format!("integrations/{integration_id}"),
                    })),
                )
                .await;
            assert_eq!(
                status, 201,
                "case {case:?}: CreateRoute must answer 201, got {status} {route}"
            );
        }
        let (status, stage) = self
            .control(
                "POST",
                &format!("/v2/apis/{api_id}/stages"),
                Some(json!({ "stageName": "$default", "autoDeploy": true })),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateStage must answer 201, got {status} {stage}"
        );
        endpoint
    }

    /// One client request to a route of the deployed stage, carrying `body` — the invoke the
    /// integration is performed for. No credential: the route's `AuthorizationType` is the
    /// default `NONE`.
    ///
    /// Handed straight to the router IN PROCESS, so no socket is opened and nothing leaves this
    /// test: the authority is only the address the request carries.
    async fn invoke(
        &self,
        endpoint: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, String, Value) {
        let authority = endpoint
            .split_once("://")
            .map_or(endpoint, |(_scheme, rest)| rest)
            .trim_end_matches('/')
            .to_string();
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{authority}{path}"))
            .header("host", authority.as_str())
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let media_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        (status, media_type, parsed_body(response.body()))
    }

    /// The shards the stream `name` holds, by id, as its own `ListShards` reports them.
    async fn shards(&self, case: &str, name: &str) -> Vec<String> {
        let listed = self
            .kinesis_ok(case, "ListShards", &json!({ "StreamName": name }))
            .await;
        let shards = listed["Shards"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            shards.len(),
            usize::try_from(SHARD_COUNT).unwrap_or_default(),
            "case {case:?}: expected the stream's {SHARD_COUNT} shards, got {listed}"
        );
        shards
            .iter()
            .map(|shard| text(case, shard, "ShardId"))
            .collect()
    }

    /// The record the stream `name` holds AT `sequence_number`, with the shard it is on, read
    /// the way a client comes back for the record it just put: an iterator at that very
    /// sequence number (`AT_SEQUENCE_NUMBER` — "start reading from the position denoted by a
    /// specific sequence number", `GetShardIteratorInput$ShardIteratorType`) on each shard the
    /// stream holds. `None` when no shard holds a record under it.
    ///
    /// A sequence number identifies a record WITHIN ITS SHARD (`Record$SequenceNumber`), so
    /// exactly ONE shard of the stream answers an iterator for it and every other refuses it:
    /// a reader locating a record by its position reads the shard that owns it rather than
    /// another shard's records, which is asserted here as the read is made.
    async fn record_at(
        &self,
        case: &str,
        name: &str,
        sequence_number: &str,
    ) -> Option<(String, Value)> {
        let mut found: Option<(String, Value)> = None;
        let mut accepting: Vec<String> = Vec::new();
        for shard_id in self.shards(case, name).await {
            let (status, iterator) = self
                .kinesis(
                    "GetShardIterator",
                    &json!({
                        "StreamName": name,
                        "ShardId": shard_id,
                        "ShardIteratorType": "AT_SEQUENCE_NUMBER",
                        "StartingSequenceNumber": sequence_number,
                    }),
                )
                .await;
            // A shard the sequence number is no position in answers no iterator at all: that
            // is a shard without the record, not a failure of the read.
            if status != PERFORMED {
                assert_eq!(
                    status, ACTION_REFUSED,
                    "case {case:?}: expected {shard_id} to refuse a position of another shard \
                     as a client refusal, got {status} with {iterator}"
                );
                continue;
            }
            accepting.push(shard_id.clone());
            let read = self
                .kinesis_ok(
                    case,
                    "GetRecords",
                    &json!({ "ShardIterator": iterator["ShardIterator"] }),
                )
                .await;
            let record = read["Records"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .find(|record| record["SequenceNumber"] == json!(sequence_number));
            if let Some(record) = record {
                found = Some((shard_id, record));
            }
        }
        assert_eq!(
            accepting.len(),
            1,
            "case {case:?}: expected exactly one shard to hold the position {sequence_number}, \
             got {accepting:?}"
        );
        found
    }

    /// Every record the stream `name` holds, as `(the shard holding it, the record)`, read
    /// through Kinesis' own `ListShards`/`GetShardIterator`/`GetRecords` — so what this answers
    /// is what THAT service holds rather than what the route said.
    async fn records(&self, case: &str, name: &str) -> Vec<(String, Value)> {
        let mut held = Vec::new();
        for shard_id in self.shards(case, name).await {
            let iterator = self
                .kinesis_ok(
                    case,
                    "GetShardIterator",
                    &json!({
                        "StreamName": name,
                        "ShardId": shard_id,
                        "ShardIteratorType": "TRIM_HORIZON",
                    }),
                )
                .await;
            let read = self
                .kinesis_ok(
                    case,
                    "GetRecords",
                    &json!({ "ShardIterator": iterator["ShardIterator"] }),
                )
                .await;
            for record in read["Records"].as_array().cloned().unwrap_or_default() {
                held.push((shard_id.clone(), record));
            }
        }
        held
    }
}

/// One response body as the JSON it is, or the text it is for a body that is not JSON.
fn parsed_body(body: &[u8]) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).to_string()))
}

/// The text of the string member `member` of `document`; panics quoting the document.
fn text(case: &str, document: &Value, member: &str) -> String {
    document[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member:?} in {document}"))
        .to_string()
}

/// `data` as `Record$Data` carries it: base64, which is how a JSON protocol states a blob — so
/// the record read off the shard is compared against the bytes the client sent rather than
/// against itself. Encoded by the smithy runtime's own coder, which is what that wire uses.
fn as_blob(data: &str) -> Value {
    json!(aws_smithy_types::base64::encode(data.as_bytes()))
}

/// RULE: a route whose integration names `Kinesis-PutRecord` puts ONE record on the stream its
/// `RequestParameters` map — the mapped value's own bytes as the record's data and the mapped
/// key as its partition key — and answers that action's own document, so the shard and sequence
/// number the client reads name the record Kinesis kept.
#[tokio::test]
async fn a_put_record_subtype_route_lands_the_record_its_request_parameters_map() {
    /// One case: the route, the request made to it, the parameters the integration maps, and the
    /// data and key the record on the shard must carry.
    struct Row {
        case: &'static str,
        stream: &'static str,
        /// Whether the stream is created in the ON_DEMAND capacity mode rather than with a
        /// shard count of its own.
        on_demand: bool,
        route_key: &'static str,
        path: &'static str,
        headers: &'static [(&'static str, &'static str)],
        body: &'static str,
        parameters: fn(&str) -> Value,
        data: &'static str,
        partition_key: &'static str,
    }
    // Rows differ along WHERE each parameter comes from, WHAT the posted body carries and which
    // capacity mode the stream has: two members selected out of the body on an on-demand
    // stream (the recorded case), the whole body as the record over a key named in a header, a
    // member of a non-ASCII body over a key taken from a path variable, and two static values
    // over an empty document. The keys differ, so the records are not all placed in one shard.
    let rows: &[Row] = &[
        Row {
            case: "two members of the body on an on-demand stream",
            stream: "ingest",
            on_demand: true,
            route_key: "POST /put",
            path: "/put",
            headers: &[],
            body: r#"{"data":"a-marker","pk":"a-key"}"#,
            parameters: |stream| {
                json!({
                    "StreamName": stream,
                    "Data": "$request.body.data",
                    "PartitionKey": "$request.body.pk",
                })
            },
            data: "a-marker",
            partition_key: "a-key",
        },
        Row {
            case: "the whole body as the record and a header as the key",
            stream: "orders",
            on_demand: false,
            route_key: "POST /put",
            path: "/put",
            headers: &[("x-partition-key", "key-from-header")],
            body: r#"{"orderId":"o-1","lines":[1,2,3]}"#,
            parameters: |stream| {
                json!({
                    "StreamName": stream,
                    "Data": "$request.body",
                    "PartitionKey": "$request.header.x-partition-key",
                })
            },
            data: r#"{"orderId":"o-1","lines":[1,2,3]}"#,
            partition_key: "key-from-header",
        },
        Row {
            case: "a member of a non-ASCII body as the record and a path variable as the key",
            stream: "invoices",
            on_demand: false,
            route_key: "POST /put/{key}",
            path: "/put/key-from-path",
            headers: &[],
            body: r#"{"payload":"caf\u00e9 au lait","other":1}"#,
            parameters: |stream| {
                json!({
                    "StreamName": stream,
                    "Data": "$request.body.payload",
                    "PartitionKey": "$request.path.key",
                })
            },
            data: "café au lait",
            partition_key: "key-from-path",
        },
        Row {
            case: "a static record and a static key over an empty document",
            stream: "heartbeats",
            on_demand: true,
            route_key: "POST /beat",
            path: "/beat",
            headers: &[],
            body: "{}",
            parameters: |stream| {
                json!({
                    "StreamName": stream,
                    "Data": "a-static-record",
                    "PartitionKey": "a-static-key",
                })
            },
            data: "a-static-record",
            partition_key: "a-static-key",
        },
    ];
    for row in rows {
        let case = row.case;
        let estate = Estate::new();
        estate.stream(case, row.stream, row.on_demand).await;
        let role = estate
            .role(case, "apigatewayv2-puts-records", Some(PUT_RECORD_ACTION))
            .await;
        let endpoint = estate
            .api(
                case,
                &role,
                &[(row.route_key, (row.parameters)(row.stream))],
            )
            .await;
        let (status, media_type, answered) = estate
            .invoke(&endpoint, row.path, row.headers, row.body)
            .await;
        assert_eq!(
            status, PERFORMED,
            "case {case:?}: expected the put's own answer, got {status} with {answered}"
        );
        assert_eq!(
            media_type, JSON_MEDIA_TYPE,
            "case {case:?}: expected the action's own media type, got {media_type:?}"
        );
        // The two members `PutRecordOutput` marks required, which is what a client reads to know
        // where its record went.
        let shard_id = text(case, &answered, "ShardId");
        let sequence_number = text(case, &answered, "SequenceNumber");
        // What the client holds resolves to the record Kinesis kept: one record on the stream,
        // on the shard the answer named, under the sequence number it stated, carrying the
        // mapped data and key.
        let held = estate.records(case, row.stream).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one record on the stream, got {held:?}"
        );
        let (holding_shard, record) = &held[0];
        assert_eq!(
            *holding_shard, shard_id,
            "case {case:?}: expected the record on the shard the answer named, got \
             {holding_shard:?} with {record}"
        );
        assert_eq!(
            record["SequenceNumber"],
            json!(sequence_number),
            "case {case:?}: expected the sequence number the answer stated, got {record}"
        );
        assert_eq!(
            record["Data"],
            as_blob(row.data),
            "case {case:?}: expected the mapped data, got {record}"
        );
        assert_eq!(
            record["PartitionKey"],
            json!(row.partition_key),
            "case {case:?}: expected the mapped partition key, got {record}"
        );
        // And the same record is what a client that comes back for the position it was given
        // reads: an iterator AT that sequence number, on the shard that holds it.
        let read_at = estate.record_at(case, row.stream, &sequence_number).await;
        let (iterated_shard, iterated) = read_at.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the record at {sequence_number} to be readable at its \
                 own position, got nothing on the stream's shards"
            )
        });
        assert_eq!(
            iterated_shard, shard_id,
            "case {case:?}: expected the position to name the shard the answer did, got \
             {iterated_shard:?} with {iterated}"
        );
        assert_eq!(
            iterated, *record,
            "case {case:?}: expected the same record at its own position, got {iterated}"
        );
    }
}

/// RULE: such a route performs the action as a session of the role its `CredentialsArn` names,
/// so the refusal Kinesis itself states — for a call that role may not make, or for a stream the
/// account does not hold — is the answer the caller gets, relayed with that service's own
/// status, error shape and text, and nothing lands on the stream.
#[tokio::test]
async fn a_refused_put_is_the_stream_apis_own_answer_relayed() {
    /// One case: whether the role carries the grant, the stream the mapping names, and the error
    /// Kinesis answers.
    struct Row {
        case: &'static str,
        granted: bool,
        mapped_stream: &'static str,
        error: &'static str,
    }
    const STREAM: &str = "audit";
    // Rows differ along WHOSE refusal it is: the role's policies (no grant at all) and the
    // stream the mapping names (one this account does not hold).
    let rows: &[Row] = &[
        Row {
            case: "a role without the grant",
            granted: false,
            mapped_stream: STREAM,
            error: ACCESS_DENIED,
        },
        Row {
            case: "a stream this account does not hold",
            granted: true,
            mapped_stream: "no-such-stream",
            error: RESOURCE_NOT_FOUND,
        },
    ];
    for row in rows {
        let case = row.case;
        let estate = Estate::new();
        estate.stream(case, STREAM, false).await;
        let role = estate
            .role(
                case,
                "apigatewayv2-maybe-puts-records",
                row.granted.then_some(PUT_RECORD_ACTION),
            )
            .await;
        let endpoint = estate
            .api(
                case,
                &role,
                &[(
                    "POST /put",
                    json!({
                        "StreamName": row.mapped_stream,
                        "Data": "$request.body",
                        "PartitionKey": "a-static-key",
                    }),
                )],
            )
            .await;
        let (status, media_type, answered) = estate.invoke(&endpoint, "/put", &[], "{}").await;
        assert_eq!(
            status, ACTION_REFUSED,
            "case {case:?}: expected the stream api's own status, got {status} with {answered}"
        );
        assert_eq!(
            media_type, JSON_MEDIA_TYPE,
            "case {case:?}: expected the relayed document's media type, got {media_type:?}"
        );
        assert!(
            answered[SHAPE_MEMBER]
                .as_str()
                .unwrap_or_default()
                .ends_with(row.error),
            "case {case:?}: expected the {} Kinesis named, got {answered}",
            row.error
        );
        assert!(
            answered[DETAIL_MEMBER].is_string(),
            "case {case:?}: expected the service's own detail text, got {answered}"
        );
        let held = estate.records(case, STREAM).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected a refused put to land nothing, got {held:?}"
        );
    }
}

/// RULE: the mapping is the api's own configuration and the request is what has to satisfy it —
/// so a request carrying no value for the expression the integration maps onto a parameter the
/// action requires is a bad request, while a mapping this gateway cannot carry into the action
/// at all is an integration that produced no response; neither puts anything on the stream.
#[tokio::test]
async fn a_mapping_this_request_or_this_gateway_cannot_satisfy_puts_nothing() {
    /// One case: the parameters the integration maps, the body posted to it, and what the caller
    /// must be answered.
    struct Row {
        case: &'static str,
        parameters: fn(&str) -> Value,
        body: &'static str,
        status: u16,
        message: &'static str,
    }
    const STREAM: &str = "ledger";
    // Rows differ along WHICH half of the mapping fails: a required parameter whose expression
    // this request carries no value for (the data, then the key), and a member of the action's
    // own input this gateway does not carry into the call.
    let rows: &[Row] = &[
        Row {
            case: "the data mapped from a member no body holds",
            parameters: |stream| {
                json!({
                    "StreamName": stream,
                    "Data": "$request.body.payload",
                    "PartitionKey": "a-static-key",
                })
            },
            body: r#"{"other":1}"#,
            status: BAD_REQUEST,
            message: BAD_REQUEST_MESSAGE,
        },
        Row {
            case: "the key mapped from a header the request does not carry",
            parameters: |stream| {
                json!({
                    "StreamName": stream,
                    "Data": "$request.body",
                    "PartitionKey": "$request.header.x-partition-key",
                })
            },
            body: "{}",
            status: BAD_REQUEST,
            message: BAD_REQUEST_MESSAGE,
        },
        Row {
            case: "a parameter this gateway does not carry into the put",
            parameters: |stream| {
                json!({
                    "StreamName": stream,
                    "Data": "$request.body",
                    "PartitionKey": "a-static-key",
                    "DryRun": "true",
                })
            },
            body: "{}",
            status: INTEGRATION_FAULT,
            message: INTEGRATION_FAULT_MESSAGE,
        },
    ];
    for row in rows {
        let case = row.case;
        let estate = Estate::new();
        estate.stream(case, STREAM, true).await;
        let role = estate
            .role(case, "apigatewayv2-puts-records", Some(PUT_RECORD_ACTION))
            .await;
        let endpoint = estate
            .api(case, &role, &[("POST /put", (row.parameters)(STREAM))])
            .await;
        let (status, _media_type, answered) =
            estate.invoke(&endpoint, "/put", &[], row.body).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected {}, got {status} with {answered}",
            row.status
        );
        assert_eq!(
            answered["message"],
            json!(row.message),
            "case {case:?}: expected the gateway's own sentence, got {answered}"
        );
        let held = estate.records(case, STREAM).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected nothing to be put, got {held:?}"
        );
    }
}
