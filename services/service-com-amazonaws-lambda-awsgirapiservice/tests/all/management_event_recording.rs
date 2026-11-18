// SPDX-License-Identifier: CC0-1.0

//! The AUDIT leg of this front door: every control-plane call it serves is a management
//! event of `lambda.amazonaws.com`, recorded in the account's event history.
//!
//! RULE: a service that serves a management (control-plane) API call records that call in
//! the account's shared management-event log — the `eventName` CloudTrail PUBLISHES for the
//! operation (for this service the operation's name plus the API date of its own HTTP
//! binding, and the `v2` marker for the operations AWS revised in place), this service's
//! modelled `eventSource`, the `Management` category, `readOnly` from the operation's own
//! semantics, the Region and the credential the call was served for, the account that
//! served it, the members the request carried as `requestParameters`, what a MUTATING call
//! answered as `responseElements` (a reporting call has none), the fault SHAPE's name as
//! `errorCode` for a call that failed, and an `eventID` of its own. A call that RUNS a
//! function is no management event at all: it is a data event, which never reaches event
//! history.
//!
//! The tables below span what the rule generalises over rather than the one read that
//! motivated it: the kind of call (create, read, list, publish, tag, grant, a read of the
//! whole account), both read/write classes, five of the API versions this service binds,
//! the revised operations and the unrevised ones, both outcomes (served and refused), and
//! an invocation as the control.
//!
//! Every call goes over the wire through the same router the production emulator uses (the
//! harness `function_lifecycle.rs` publishes); the records are read back out of the SHARED
//! log in the engine the service was built from — the one store CloudTrail's `LookupEvents`
//! reads. The end-to-end leg through that operation is
//! `core/router/tests/all/cloudtrail_lambda_management_events.rs`, which needs CloudTrail
//! mounted beside this service.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};

use service_com_amazonaws_lambda_awsgirapiservice::service::management_events::{
    published_event_name, EVENT_SOURCE, MANAGEMENT_EVENT_OPERATIONS,
};
use service_common::audited_operation::CallKind;
use service_common::management_events::{ManagementEvent, ManagementEventLog};

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, make_update_due, package_bytes, router_on,
    zip_file, Answer, ACCOUNT, REGION, ROLE,
};

/// The `eventCategory` CloudTrail stamps on a control-plane call's record.
const MANAGEMENT: &str = "Management";

/// The `eventType` of a recorded API call.
const AWS_API_CALL: &str = "AwsApiCall";

/// The access key id the shared request builder signs every call with, which is what the
/// recorded caller of each call below is.
const ACCESS_KEY_ID: &str = "emulator-test-key";

/// The header every answer of this service names the request it answered in, which a record
/// carries as its `requestID`.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// This service's Smithy model, as it ships beside this crate: the source the API date of
/// every published event name is held against.
const MODEL_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/smithy-model/com-amazonaws-lambda-awsgirapiservice.json"
);

/// The operations that serve no management event of this service: the two invocations,
/// which are DATA events of the function they run, and the response-streaming one, which
/// implements no typed operation at all.
const INVOCATION_OPERATIONS: [&str; 3] = ["Invoke", "InvokeAsync", "InvokeWithResponseStream"];

/// A `Code.ZipFile` payload the runtime can READ: the 22-byte empty archive, base64-encoded
/// as `restJson1` carries a blob. What a run of it answers is the handler's business and not
/// this suite's — only that the invocation was SERVED is asserted below.
const EMPTY_ZIP: &str = "UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";

/// Every record in the shared log, oldest first, as the CloudTrail event document a lookup
/// reports.
fn records(engine: &Arc<engine::InMemoryEngine>) -> Vec<Value> {
    let log = engine.state::<ManagementEventLog>();
    let held = log.lock();
    held.iter()
        .map(ManagementEvent::to_cloudtrail_event_json)
        .collect()
}

/// The records whose `eventName` is `event_name`, oldest first.
fn records_of(engine: &Arc<engine::InMemoryEngine>, event_name: &str) -> Vec<Value> {
    records(engine)
        .into_iter()
        .filter(|event| event.get("eventName") == Some(&json!(event_name)))
        .collect()
}

/// A string member of a record, or `None` when the record does not carry it.
fn at(event: &Value, pointer: &str) -> Option<String> {
    event
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// One call of the table: what it sends, what the caller sees, and what the record of it
/// must carry.
struct Row {
    case: &'static str,
    /// The operation's modelled name — the catalog's key, NOT the published event name.
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Value,
    /// The HTTP status the caller must see.
    status: u16,
    /// The operation's own semantics, as the record's `readOnly`.
    read_only: bool,
    /// The fault shape's name for a refused call, `None` for one that was served.
    error_code: Option<&'static str>,
    /// A `(json pointer, value)` the record's `requestParameters` must carry.
    parameter: Option<(&'static str, Value)>,
}

/// RULE (the whole record of one served management call), over a table of calls.
#[tokio::test]
async fn every_control_plane_call_is_recorded_as_a_management_event() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let package = package_bytes(96);

    // The subject of the reads below, created through this service's own API — so its
    // create is itself one of the rows asserted on.
    let created = create_function(
        &router,
        &json!({
            "FunctionName": "audited",
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(&package) },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction must succeed, got {} with {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&engine);
    let function_arn = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:audited");

    let rows = vec![
        Row {
            case: "a create, which is a write under the oldest API date",
            operation: "CreateFunction",
            method: "GET",
            uri: String::new(),
            body: Value::Null,
            status: 201,
            read_only: false,
            error_code: None,
            parameter: Some(("/functionName", json!("audited"))),
        },
        Row {
            case: "the read of one function, a REVISED operation",
            operation: "GetFunction",
            method: "GET",
            uri: "/2015-03-31/functions/audited".to_string(),
            body: Value::Null,
            status: 200,
            read_only: true,
            error_code: None,
            parameter: Some(("/functionName", json!("audited"))),
        },
        Row {
            case: "the read of one function's configuration, also revised",
            operation: "GetFunctionConfiguration",
            method: "GET",
            uri: "/2015-03-31/functions/audited/configuration".to_string(),
            body: Value::Null,
            status: 200,
            read_only: true,
            error_code: None,
            parameter: Some(("/functionName", json!("audited"))),
        },
        Row {
            case: "a read addressing the function by its ARN with a qualifier",
            operation: "ListVersionsByFunction",
            method: "GET",
            uri: format!("/2015-03-31/functions/{function_arn}/versions"),
            body: Value::Null,
            status: 200,
            read_only: true,
            error_code: None,
            parameter: Some(("/functionName", json!(function_arn.clone()))),
        },
        Row {
            case: "a list over the account's functions, which names no resource",
            operation: "ListFunctions",
            method: "GET",
            uri: "/2015-03-31/functions".to_string(),
            body: Value::Null,
            status: 200,
            read_only: true,
            error_code: None,
            parameter: None,
        },
        Row {
            case: "a read of the whole account, under an API date of its own",
            operation: "GetAccountSettings",
            method: "GET",
            uri: "/2016-08-19/account-settings".to_string(),
            body: Value::Null,
            status: 200,
            read_only: true,
            error_code: None,
            parameter: None,
        },
        Row {
            case: "a publish, which is a write that answers a new version",
            operation: "PublishVersion",
            method: "POST",
            uri: "/2015-03-31/functions/audited/versions".to_string(),
            body: json!({}),
            status: 201,
            read_only: false,
            error_code: None,
            parameter: Some(("/functionName", json!("audited"))),
        },
        Row {
            case: "a grant on the function's policy, a revised write",
            operation: "AddPermission",
            method: "POST",
            uri: "/2015-03-31/functions/audited/policy".to_string(),
            body: json!({
                "StatementId": "audit",
                "Action": "lambda:InvokeFunction",
                "Principal": "s3.amazonaws.com",
            }),
            status: 201,
            read_only: false,
            error_code: None,
            parameter: Some(("/statementId", json!("audit"))),
        },
        Row {
            case: "the read of that policy, a revised read",
            operation: "GetPolicy",
            method: "GET",
            uri: "/2015-03-31/functions/audited/policy".to_string(),
            body: Value::Null,
            status: 200,
            read_only: true,
            error_code: None,
            parameter: Some(("/functionName", json!("audited"))),
        },
        Row {
            case: "a tagging write, under the tagging API date",
            operation: "TagResource",
            method: "POST",
            uri: format!("/2017-03-31/tags/{function_arn}"),
            body: json!({ "Tags": { "owner": "audit" } }),
            status: 204,
            read_only: false,
            error_code: None,
            parameter: Some(("/resource", json!(function_arn.clone()))),
        },
        Row {
            case: "the read of those tags",
            operation: "ListTags",
            method: "GET",
            uri: format!("/2017-03-31/tags/{function_arn}"),
            body: Value::Null,
            status: 200,
            read_only: true,
            error_code: None,
            parameter: Some(("/resource", json!(function_arn.clone()))),
        },
        Row {
            case: "a layer publish, under the layer API date",
            operation: "PublishLayerVersion",
            method: "POST",
            uri: "/2018-10-31/layers/audited-layer/versions".to_string(),
            body: json!({ "Content": { "ZipFile": zip_file(&package) } }),
            status: 201,
            read_only: false,
            error_code: None,
            parameter: Some(("/layerName", json!("audited-layer"))),
        },
        Row {
            case: "a read the service REFUSED, which is recorded with its fault",
            operation: "GetAlias",
            method: "GET",
            uri: "/2015-03-31/functions/audited/aliases/no-such-alias".to_string(),
            body: Value::Null,
            status: 404,
            read_only: true,
            error_code: Some("ResourceNotFoundException"),
            parameter: Some(("/name", json!("no-such-alias"))),
        },
    ];

    for row in &rows {
        // The create is the row made in the setup above, so it is asserted on and not
        // repeated: making it twice would be a second create of one function.
        if row.uri.is_empty() {
            continue;
        }
        make_update_due(&engine);
        let answered = json_call(&router, row.method, &row.uri, &row.body).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            row.case,
            row.status,
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
        assert_recorded(&engine, row, &answered);
    }
    // The create, asserted against the answer the setup got.
    assert_recorded(&engine, &rows[0], &created);

    // Every record names its OWN call: no two records share an `eventID`.
    let all = records(&engine);
    let ids: BTreeSet<String> = all.iter().filter_map(|event| at(event, "/eventID")).collect();
    assert_eq!(
        ids.len(),
        all.len(),
        "case every record: expected one unique eventID per record, got {ids:?} over {} \
         records",
        all.len()
    );
}

/// Assert everything the record of `row` must carry, against the answer its caller got.
fn assert_recorded(engine: &Arc<engine::InMemoryEngine>, row: &Row, answered: &Answer) {
    let published = published_event_name(row.operation);
    let found = records_of(engine, &published);
    assert_eq!(
        found.len(),
        1,
        "case {:?}: expected exactly one record under the published event name \
         {published:?}, got {found:?} — every record so far: {:?}",
        row.case,
        records(engine)
            .iter()
            .filter_map(|event| at(event, "/eventName"))
            .collect::<Vec<String>>()
    );
    let event = &found[0];
    assert_eq!(
        at(event, "/eventSource").as_deref(),
        Some(EVENT_SOURCE),
        "case {:?}: the record names this service as its source, got {event}",
        row.case
    );
    assert_eq!(
        at(event, "/eventCategory").as_deref(),
        Some(MANAGEMENT),
        "case {:?}: a control-plane call is a management event, got {event}",
        row.case
    );
    assert_eq!(
        event.get("managementEvent"),
        Some(&json!(true)),
        "case {:?}: a management event says so, got {event}",
        row.case
    );
    assert_eq!(
        at(event, "/eventType").as_deref(),
        Some(AWS_API_CALL),
        "case {:?}: a recorded API call is an AwsApiCall, got {event}",
        row.case
    );
    assert_eq!(
        at(event, "/awsRegion").as_deref(),
        Some(REGION),
        "case {:?}: the record is in the Region the call was served in, got {event}",
        row.case
    );
    assert_eq!(
        at(event, "/recipientAccountId").as_deref(),
        Some(ACCOUNT),
        "case {:?}: the record names the account the call was served for, got {event}",
        row.case
    );
    assert_eq!(
        at(event, "/userIdentity/accessKeyId").as_deref(),
        Some(ACCESS_KEY_ID),
        "case {:?}: the record names the credential the call was signed with, got {event}",
        row.case
    );
    assert_eq!(
        event.get("readOnly"),
        Some(&json!(row.read_only)),
        "case {:?}: expected readOnly {}, got {event}",
        row.case,
        row.read_only
    );
    assert_eq!(
        at(event, "/errorCode").as_deref(),
        row.error_code,
        "case {:?}: expected errorCode {:?}, got {event}",
        row.case,
        row.error_code
    );
    assert_eq!(
        at(event, "/requestID"),
        answered.header(REQUEST_ID_HEADER),
        "case {:?}: the record names the request the caller was answered under, got {event}",
        row.case
    );
    assert!(
        at(event, "/eventID").is_some_and(|id| !id.is_empty()),
        "case {:?}: every record carries an eventID of its own, got {event}",
        row.case
    );
    if let Some((pointer, expected)) = &row.parameter {
        assert_eq!(
            event.pointer(&format!("/requestParameters{pointer}")),
            Some(expected),
            "case {:?}: expected requestParameters{pointer} = {expected}, got {event}",
            row.case
        );
    }
    // What a call ANSWERED is recorded for a write and for nothing else: a read's
    // `responseElements` is null, and so is a refused call's.
    let reported = event.get("responseElements").unwrap_or(&Value::Null);
    let reports_an_answer = !row.read_only && row.error_code.is_none();
    assert_eq!(
        reported.is_object(),
        reports_an_answer,
        "case {:?}: expected responseElements {}, got {reported}",
        row.case,
        if reports_an_answer {
            "to report what the call answered"
        } else {
            "to be null"
        }
    );
}

/// RULE (the other half): a call that RUNS a function is a data event, so it never enters
/// the management-event log — however many times it is invoked and whichever invocation
/// type asked for the run.
#[tokio::test]
async fn an_invocation_is_never_a_management_event() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let created = create_function(
        &router,
        &json!({
            "FunctionName": "not-audited",
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": EMPTY_ZIP },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction must succeed, got {} with {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&engine);

    for invocation_type in ["RequestResponse", "Event", "DryRun"] {
        let answered = crate::function_lifecycle::send(
            &router,
            "POST",
            "/2015-03-31/functions/not-audited/invocations",
            "application/json",
            br#"{"probe":true}"#.to_vec(),
            &[("x-amz-invocation-type", invocation_type.to_string())],
        )
        .await;
        assert!(
            (200..300).contains(&answered.status),
            "case {invocation_type:?}: the invocation must be served, got {} with {}",
            answered.status,
            String::from_utf8_lossy(&answered.body)
        );
    }

    let recorded: Vec<String> = records(&engine)
        .iter()
        .filter_map(|event| at(event, "/eventName"))
        .collect();
    for operation in INVOCATION_OPERATIONS {
        assert!(
            !recorded.iter().any(|name| name.starts_with(operation)),
            "case {operation:?}: an invocation is a data event and never event history, \
             got {recorded:?}"
        );
    }
    assert!(
        recorded.contains(&published_event_name("CreateFunction")),
        "case the control: the control-plane create is still recorded, got {recorded:?}"
    );
}

/// RULE (what a record withholds): the members this service's records leave out carry no
/// key at all — neither in the parameters a call was dispatched with nor in what it
/// answered — whichever operation carried them.
#[tokio::test]
async fn a_record_withholds_the_package_and_the_environment() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let created = create_function(
        &router,
        &json!({
            "FunctionName": "withheld",
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Environment": { "Variables": { "SECRET_STAGE": "prod" } },
            "Code": { "ZipFile": zip_file(&package_bytes(128)) },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction must succeed, got {} with {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&engine);
    let layer = json_call(
        &router,
        "POST",
        "/2018-10-31/layers/withheld-layer/versions",
        &json!({ "Content": { "ZipFile": zip_file(&package_bytes(96)) } }),
    )
    .await;
    assert_eq!(
        layer.status, 201,
        "setup: PublishLayerVersion must succeed, got {} with {}",
        layer.status,
        String::from_utf8_lossy(&layer.body)
    );
    // A read of the function reports its environment back, so the omission is asserted on a
    // record of a read as well as on one of a write.
    let read = json_call(
        &router,
        "GET",
        "/2015-03-31/functions/withheld",
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "setup: GetFunction must succeed, got {} with {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );

    for (case, operation) in [
        ("a function create", "CreateFunction"),
        ("a layer publish", "PublishLayerVersion"),
        ("a function read", "GetFunction"),
    ] {
        let found = records_of(&engine, &published_event_name(operation));
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: expected one record of {operation}, got {found:?}"
        );
        let document = found[0].to_string();
        for withheld in ["zipFile", "variables", "SECRET_STAGE", "prod"] {
            assert!(
                !document.contains(withheld),
                "case {case:?}: expected the record to carry no {withheld:?}, got {document}"
            );
        }
    }
}

/// RULE (the published name is the MODEL's API version): every operation this service
/// records a management event for publishes its calls under its own name followed by the
/// API date of its own HTTP binding, so the catalog cannot drift from the model it is read
/// from; and every operation the model declares `@readonly` is recorded as a call that only
/// REPORTS state.
///
/// The model is read here rather than embedded: it is the source of the dates, and holding
/// every row of the catalog against it covers the operations no test calls as well as the
/// ones that do.
#[test]
fn every_published_event_name_carries_the_modelled_api_version() {
    let model = std::fs::read_to_string(MODEL_PATH)
        .expect("the model that generated this crate ships beside it");
    let model: Value =
        serde_json::from_str(&model).expect("the model that generated this crate is JSON");
    let shapes = model["shapes"]
        .as_object()
        .expect("a Smithy model declares shapes");

    let mut checked = 0_usize;
    for (id, shape) in shapes {
        if shape["type"] != json!("operation") {
            continue;
        }
        let operation = id.rsplit('#').next().unwrap_or_default();
        if INVOCATION_OPERATIONS.contains(&operation) {
            continue;
        }
        let uri = shape
            .pointer("/traits/smithy.api#http/uri")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {operation}: a REST operation binds a URI"));
        // The API DATE is the first segment of the operation's own URI
        // (`/2015-03-31/functions/{FunctionName}`), with the dashes dropped.
        let api_date: String = uri
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default()
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let row = MANAGEMENT_EVENT_OPERATIONS
            .iter()
            .find(|(named, _, _)| *named == operation)
            .unwrap_or_else(|| {
                panic!(
                    "case {operation}: every modelled control-plane operation is in the \
                     management-event catalog; this one is missing, so its calls would be \
                     recorded under a name AWS does not publish"
                )
            });
        let published = published_event_name(operation);
        assert!(
            published == format!("{operation}{api_date}")
                || published == format!("{operation}{api_date}v2"),
            "case {operation}: expected the published name to carry the modelled API date \
             {api_date:?}, got {published:?}"
        );
        assert_eq!(
            row.2, api_date,
            "case {operation}: expected the cataloged API date {api_date:?} (from {uri:?}), \
             got {:?}",
            row.2
        );
        if shape.pointer("/traits/smithy.api#readonly").is_some() {
            assert_eq!(
                row.1,
                CallKind::Reporting,
                "case {operation}: the model declares it @readonly, so its records carry \
                 readOnly true"
            );
        }
        checked += 1;
    }
    assert!(
        checked > 80,
        "case the model's surface: expected this service's whole control-plane surface, got \
         {checked} operations"
    );
    assert_eq!(
        checked,
        MANAGEMENT_EVENT_OPERATIONS.len(),
        "case the catalog: expected one row per modelled control-plane operation, got \
         {} rows for {checked} operations",
        MANAGEMENT_EVENT_OPERATIONS.len()
    );
}
