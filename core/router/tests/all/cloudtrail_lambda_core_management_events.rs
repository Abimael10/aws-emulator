// SPDX-License-Identifier: CC0-1.0

//! A Lambda Core control-plane call read back out of CloudTrail event history, beside a
//! call of the CLASSIC Lambda control plane that audits under the very same event source.
//!
//! RULE: CloudTrail's `LookupEvents` "looks up management events … captured by CloudTrail"
//! for the Region the request is made in, and every management (control-plane) call of an
//! account is captured in event history whether or not a trail exists. So after a
//! successful call of a service whose model declares
//! `awsService.cloudTrailEventSource: lambda.amazonaws.com`, a lookup filtered on that
//! event source — or on the `eventName` the serving service publishes the operation's calls
//! under — answers an event whose `CloudTrailEvent` carries `eventCategory` `Management`,
//! that event source, the published `eventName`, `readOnly` from the operation's own
//! semantics (true exactly for the operations the model declares `@readonly`), no
//! `errorCode` for a call that succeeded, the Region the request was made in, the caller's
//! account as `recipientAccountId`, an `eventID` of its own, and the members the request
//! carried as `requestParameters`.
//!
//! The second half of the same rule, and why these two services are in ONE suite: an event
//! source is the SERVICE's modelled fact and not a crate's name, so Lambda Core and the
//! Lambda control plane record under one `eventSource` — and stay apart by the rest of the
//! record. Their `eventName`s differ (each operation's own published name), their `eventID`s
//! differ, and each record's `requestParameters` names only the resource ITS call addressed.
//!
//! Only a cross-service suite can pin this: the calls are served by Lambda Core and by AWS
//! Lambda (both `restJson1`), the records are read back through `cloudtrail:LookupEvents`
//! (`awsJson1_1`), and the three meet in the one shared management-event log of the engine
//! `core/router/src/lib.rs` mounts them all on. The Lambda-Core-side half of the rule —
//! which class each operation's calls are recorded with — is
//! `services/service-com-amazonaws-lambdacore-lambdacoreapiservice/src/service/management_events.rs`.
//!
//! The rows span what the rule generalises over rather than the one create that motivated
//! it: over Lambda Core's whole surface, the three writes (a create, an update, a delete)
//! and the three `@readonly` reads (one connector, the account's connectors, one
//! connector's versions), plus a write served by the other service under the shared event
//! source. Every page of each lookup is read, paging `NextToken` to exhaustion, because a
//! record a client has to find is a record it finds however the service pages.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, which is the Region the records are filed
/// in and the one the lookups are made in.
const REGION: &str = "us-west-2";

/// The account every emulated resource and every record belongs to.
const ACCOUNT: &str = "000000000000";

/// The event source of every record below: the `awsService.cloudTrailEventSource` BOTH
/// models declare.
const LAMBDA_EVENT_SOURCE: &str = "lambda.amazonaws.com";

/// The `eventCategory` a lookup serves.
const MANAGEMENT_CATEGORY: &str = "Management";

/// The URI prefix every Lambda Core network-connector operation is bound under.
const CONNECTOR_PREFIX: &str = "/2026-04-04/network-connectors";

/// The connector the write and the reads below are made on, and the one the delete is made
/// on: a delete needs its own, because it takes the connector away.
const CONNECTOR: &str = "ct-core-connector";
const DOOMED_CONNECTOR: &str = "ct-core-retired";

/// The layer the classic Lambda control plane publishes a version of, under the shared
/// event source. Deliberately sharing no substring with the connector names, so "this record
/// names only its own resource" is a meaningful assertion.
const LAYER: &str = "zz-classic-layer";

/// A subnet and a security group of EC2's own id form. No EC2 is mounted here (this suite is
/// about the audit record, not about reference resolution), so nothing resolves them and
/// nothing is refused on them.
const SUBNET: &str = "subnet-0123456789abcdef0";
const SECURITY_GROUP: &str = "sg-0123456789abcdef0";

/// The `OperatorRole` a connector of the one modelled type cannot be created without,
/// satisfying the modelled `NetworkConnectorRoleArn` pattern.
const OPERATOR_ROLE: &str = "arn:aws:iam::000000000000:role/lambda-connector-operator";

/// A `LayerVersionContentInput.ZipFile`: the 22-byte empty archive, base64-encoded as
/// `restJson1` carries a blob. What the layer HOLDS is not this suite's subject.
const ZIP_FILE: &str = "UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";

/// How often, and for how long at most, a poll looks for a state the emulator's own clock
/// owes (a created connector reaching `ACTIVE`). The deadline is far above the span the
/// service takes, so a loaded host waits longer rather than failing.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 1_200;

/// How many events one lookup page carries, and how many pages are read at most: bounds on
/// this test, not on the API — the log holds a few hundred records here (every poll of a
/// read is a record too), and a service that paged without end would fail here rather than
/// loop.
const PAGE: usize = 50;
const MAX_PAGES: usize = 200;

/// One answer as its caller reads it.
struct Answered {
    status: u16,
    text: String,
    body: Value,
}

/// Lambda Core, the classic Lambda control plane and CloudTrail over one shared engine,
/// exactly as `core/router/src/lib.rs` mounts them. No trail is created: event history holds
/// an account's management calls "whether or not a trail exists", which is the fact this
/// suite pins.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential
    // scope is still carried, because it is what names the Region and the caller.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambdacore_lambdacoreapiservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
/// routes the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Dispatch one request and read back its status and body.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> Answered {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answered { status, text, body }
}

/// One call on an endpoint signing as `lambda` (`restJson1`), answered as it stands. Which
/// of the two services serves it is decided by the URI path space, exactly as in production.
async fn lambda_api(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the request body serializes")
    };
    dispatch(
        router,
        method,
        "lambda",
        uri,
        "application/json",
        payload,
        &[],
    )
    .await
}

/// One CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must be
/// served.
async fn cloudtrail(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answered = dispatch(
        router,
        "POST",
        "cloudtrail",
        "/",
        "application/x-amz-json-1.1",
        body.to_string().into_bytes(),
        &[("x-amz-target", &format!("CloudTrail_20131101.{operation}"))],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {} with {}",
        answered.status, answered.text
    );
    answered.body
}

/// The configuration a connector is created and updated with: the one modelled
/// configuration union (`VpcEgressConfiguration`), over the subnets given.
fn configuration(subnets: &[&str]) -> Value {
    json!({
        "VpcEgressConfiguration": {
            "SubnetIds": subnets,
            "SecurityGroupIds": [SECURITY_GROUP],
            "NetworkProtocol": "IPv4",
            "AssociatedComputeResourceTypes": ["MicroVm"],
        }
    })
}

/// Create the connector `name` and wait until Lambda Core reports it `ACTIVE` — the state an
/// update and a delete need. Waits for the CONDITION, never for a span of time.
async fn active_connector(router: &MultiServiceRouter, name: &str) {
    let created = lambda_api(
        router,
        "POST",
        CONNECTOR_PREFIX,
        &json!({
            "Name": name,
            "Configuration": configuration(&[SUBNET]),
            "OperatorRole": OPERATOR_ROLE,
        }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup: CreateNetworkConnector({name:?}) must succeed, got {} with {}",
        created.status, created.text
    );
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let read = lambda_api(
            router,
            "GET",
            &format!("{CONNECTOR_PREFIX}/{name}"),
            &Value::Null,
        )
        .await;
        if read.body["State"].as_str() == Some("ACTIVE") {
            return;
        }
        last = read.body;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda Core must report {name:?} ACTIVE, last reported {last}");
}

/// Every event of `attributes`, paging `NextToken` to exhaustion, as the parsed
/// `CloudTrailEvent` document of each one beside the `EventName` the event itself reports.
async fn looked_up(
    router: &MultiServiceRouter,
    case: &str,
    attributes: &Value,
) -> Vec<(String, Value)> {
    let mut found: Vec<(String, Value)> = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = json!({ "LookupAttributes": [attributes], "MaxResults": PAGE });
        if let Some(token) = &next {
            request["NextToken"] = json!(token);
        }
        let answered = cloudtrail(router, case, "LookupEvents", &request).await;
        for event in answered["Events"].as_array().unwrap_or(&Vec::new()) {
            let document = event["CloudTrailEvent"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every event carries its CloudTrailEvent, got {event}")
            });
            let parsed: Value = serde_json::from_str(document).unwrap_or_else(|error| {
                panic!("case {case:?}: a CloudTrailEvent is JSON ({error}): {document}")
            });
            found.push((
                event["EventName"].as_str().unwrap_or_default().to_string(),
                parsed,
            ));
        }
        next = answered["NextToken"].as_str().map(ToString::to_string);
        if next.is_none() {
            return found;
        }
    }
    panic!(
        "case {case:?}: the lookup must page to exhaustion, got {} events",
        found.len()
    );
}

/// One call this suite makes and reads back, and what the record of it must say.
struct Row {
    case: &'static str,
    /// The `eventName` CloudTrail publishes the call under: the name a client filters a
    /// lookup by.
    event_name: &'static str,
    /// The operation's own semantics, as the record's `readOnly`.
    read_only: bool,
    /// A `(json pointer, value)` the record's `requestParameters` must carry.
    parameter: Option<(&'static str, Value)>,
    /// The resource the call did NOT address, which its record must therefore not name.
    foreign: &'static str,
}

/// Assert the record `document` satisfies every part of the rule for `row`.
fn assert_record(row: &Row, document: &Value) {
    assert_eq!(
        document.get("eventSource"),
        Some(&json!(LAMBDA_EVENT_SOURCE)),
        "case {:?}: the record names the service's modelled event source, got {document}",
        row.case
    );
    assert_eq!(
        document.get("eventCategory"),
        Some(&json!(MANAGEMENT_CATEGORY)),
        "case {:?}: a lookup serves management events, got {document}",
        row.case
    );
    assert_eq!(
        document.get("eventName"),
        Some(&json!(row.event_name)),
        "case {:?}: the document names the published operation, got {document}",
        row.case
    );
    assert_eq!(
        document.get("readOnly"),
        Some(&json!(row.read_only)),
        "case {:?}: expected readOnly {}, got {document}",
        row.case,
        row.read_only
    );
    assert_eq!(
        document.get("errorCode"),
        None,
        "case {:?}: a served call carries no errorCode, got {document}",
        row.case
    );
    assert_eq!(
        document.get("awsRegion"),
        Some(&json!(REGION)),
        "case {:?}: the record is filed in the Region the call was made in, got {document}",
        row.case
    );
    assert_eq!(
        document.get("recipientAccountId"),
        Some(&json!(ACCOUNT)),
        "case {:?}: the record names the caller's account, got {document}",
        row.case
    );
    assert!(
        document
            .get("eventID")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "case {:?}: every record carries an eventID, got {document}",
        row.case
    );
    if let Some((pointer, expected)) = &row.parameter {
        assert_eq!(
            document.pointer(&format!("/requestParameters{pointer}")),
            Some(expected),
            "case {:?}: expected requestParameters{pointer} = {expected}, got {document}",
            row.case
        );
    }
    assert!(
        !document.to_string().contains(row.foreign),
        "case {:?}: a record names only the resource its own call addressed, but this one \
         names {:?}: {document}",
        row.case,
        row.foreign
    );
}

#[tokio::test]
async fn a_lambda_core_management_call_is_served_back_by_lookup_events() {
    let router = router();
    // The two connectors the rows act on, each ACTIVE: the writes below need that state, and
    // the reads report it.
    active_connector(&router, CONNECTOR).await;
    active_connector(&router, DOOMED_CONNECTOR).await;

    // Every remaining call of Lambda Core's surface, in the order a client makes them.
    let calls: Vec<(&str, &str, String, Value, u16)> = vec![
        (
            "the read of one connector",
            "GET",
            format!("{CONNECTOR_PREFIX}/{CONNECTOR}"),
            Value::Null,
            200,
        ),
        (
            "the list over the account's connectors",
            "GET",
            CONNECTOR_PREFIX.to_string(),
            Value::Null,
            200,
        ),
        (
            "the list over one connector's versions",
            "GET",
            format!("{CONNECTOR_PREFIX}/{CONNECTOR}/versions"),
            Value::Null,
            200,
        ),
        (
            "the update of one connector",
            "PUT",
            format!("{CONNECTOR_PREFIX}/{CONNECTOR}"),
            json!({ "Configuration": configuration(&[SUBNET, "subnet-0fedcba9876543210"]) }),
            202,
        ),
        (
            "the delete of the other connector",
            "DELETE",
            format!("{CONNECTOR_PREFIX}/{DOOMED_CONNECTOR}"),
            Value::Null,
            202,
        ),
        (
            "a layer publish served by the classic Lambda control plane",
            "POST",
            format!("/2018-10-31/layers/{LAYER}/versions"),
            json!({ "Content": { "ZipFile": ZIP_FILE } }),
            201,
        ),
    ];
    for (case, method, uri, body, status) in &calls {
        let answered = lambda_api(&router, method, uri, body).await;
        assert_eq!(
            answered.status, *status,
            "case {case:?}: expected HTTP {status} from {method} {uri}, got {} with {}",
            answered.status, answered.text
        );
    }

    let rows = vec![
        Row {
            case: "the create of a connector",
            event_name: "CreateNetworkConnector",
            read_only: false,
            parameter: Some(("/name", json!(CONNECTOR))),
            foreign: LAYER,
        },
        Row {
            case: "the read of one connector",
            event_name: "GetNetworkConnector",
            read_only: true,
            parameter: Some(("/identifier", json!(CONNECTOR))),
            foreign: LAYER,
        },
        Row {
            case: "the list over the account's connectors",
            event_name: "ListNetworkConnectors",
            read_only: true,
            parameter: None,
            foreign: LAYER,
        },
        Row {
            case: "the list over one connector's versions",
            event_name: "ListNetworkConnectorVersions",
            read_only: true,
            parameter: Some(("/identifier", json!(CONNECTOR))),
            foreign: LAYER,
        },
        Row {
            case: "the update of one connector",
            event_name: "UpdateNetworkConnector",
            read_only: false,
            parameter: Some(("/identifier", json!(CONNECTOR))),
            foreign: LAYER,
        },
        Row {
            case: "the delete of the other connector",
            event_name: "DeleteNetworkConnector",
            read_only: false,
            parameter: Some(("/identifier", json!(DOOMED_CONNECTOR))),
            foreign: LAYER,
        },
        Row {
            case: "a layer publish under the same event source",
            event_name: "PublishLayerVersion20181031",
            read_only: false,
            parameter: Some(("/layerName", json!(LAYER))),
            foreign: CONNECTOR,
        },
    ];

    // ONE lookup over the whole event source, as an audit client makes it.
    let by_source = looked_up(
        &router,
        "a lookup by event source",
        &json!({ "AttributeKey": "EventSource", "AttributeValue": LAMBDA_EVENT_SOURCE }),
    )
    .await;
    let names: BTreeSet<&str> = by_source.iter().map(|(name, _)| name.as_str()).collect();

    for row in &rows {
        // The record of THIS row's call: the first event under its published name whose
        // parameters name the resource the row's call addressed, so the create of one
        // connector is never read as the create of the other.
        let found: Vec<&Value> = by_source
            .iter()
            .filter(|(name, _)| name == row.event_name)
            .map(|(_, document)| document)
            .filter(|document| match &row.parameter {
                Some((pointer, expected)) => {
                    document.pointer(&format!("/requestParameters{pointer}")) == Some(expected)
                }
                None => true,
            })
            .collect();
        assert!(
            !found.is_empty(),
            "case {:?}: expected a record of {} in event history, got {names:?}",
            row.case,
            row.event_name
        );
        assert_record(row, found[0]);
    }

    // Every record is its own: no two of them share an `eventID`. That is what keeps two
    // services recording under ONE event source apart, record by record.
    let ids: BTreeSet<&str> = by_source
        .iter()
        .filter_map(|(_, document)| document.get("eventID").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids.len(),
        by_source.len(),
        "case every record: expected one eventID per record, got {} ids over {} records",
        ids.len(),
        by_source.len()
    );

    // And the lookup a client filters by the published NAME answers that call's record,
    // which is how an audit workflow finds its own write rather than reading the whole
    // source. Both services' names are asked for, because the point of the shared source is
    // that the NAME still separates them.
    for row in &rows {
        let by_name = looked_up(
            &router,
            row.case,
            &json!({ "AttributeKey": "EventName", "AttributeValue": row.event_name }),
        )
        .await;
        assert!(
            !by_name.is_empty(),
            "case {:?}: a lookup naming {} answers its records, got none while the source \
             holds {names:?}",
            row.case,
            row.event_name
        );
        for (name, document) in &by_name {
            assert_eq!(
                name, row.event_name,
                "case {:?}: every event answers the name asked for, got {document}",
                row.case
            );
        }
    }
}
