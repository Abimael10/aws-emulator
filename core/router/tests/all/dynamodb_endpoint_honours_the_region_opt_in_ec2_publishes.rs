// SPDX-License-Identifier: CC0-1.0

//! A DynamoDB endpoint in a Region the account has not opted into refuses every request,
//! for EVERY Region the emulator publishes as needing an opt-in.
//!
//! RULE: regional opt-in is enforced at the ENDPOINT. An account reaches a Region only once
//! that Region is turned on for it — the Regions AWS turns on for every account, plus the
//! opt-in Regions the account has enabled — so a request addressed to a Region of the
//! partition the account has not turned on is refused before it reaches an operation, and a
//! Region that needs an opt-in carries that opt status wherever the account's state is
//! reported. A caller therefore never reads a table list for a Region it cannot use.
//!
//! This suite lives beside the router because the rule spans three services that must agree
//! about ONE fact — which Regions this account can use: Amazon EC2 publishes each Region's
//! opt-in status (`DescribeRegions`), the Account API owns the opt-in itself (`EnableRegion`
//! / `DisableRegion` / `GetRegionOptStatus`), and the DynamoDB endpoint is gated on it. A
//! Region one of them publishes as needing an opt-in and another knows nothing about is the
//! defect this suite pins: the endpoint then serves it, and a client sweeping the partition
//! reads a Region it cannot use as one it can.
//!
//! The rows are DERIVED from what EC2 publishes rather than written out here, so every
//! published opt-in Region is a row — a Region the emulator gains later included — and the
//! cases span what the rule generalises over: every published opt-in Region, every Region
//! that needs no opt-in, the four operation classes of the gated service (an account-wide
//! read, a read naming a table, a control-plane mutation and a data-plane write), and the
//! account's own opt-in state (never opted in, enabled through the Account API, disabled
//! again).
//!
//! Sources: `smithy_query(query="operation", operation="DescribeRegions")` on the EC2 model
//! for the `opt-in-status` filter and the three values `Region$OptInStatus` carries;
//! `smithy_query(query="operation", operation="GetRegionOptStatus")` on the account model for
//! `RegionOptStatus`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// The Region every SETUP call is addressed to: one AWS turns on for every account, so the
/// calls that discover the Region catalogue are themselves never gated.
const SETUP_REGION: &str = "us-east-1";

/// The account every generated emulator presents (`engine::emulator_account_id`), written
/// out so the ARN a created table reports is compared to the Region it was addressed in
/// rather than to the helper the handler mints it with.
const ACCOUNT: &str = "000000000000";

/// The EC2 API version an ec2Query body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// `Region$OptInStatus` for a Region that requires an opt-in the account has not made, and
/// for one no account has to opt into at all — two of the three values that member's own
/// documentation lists.
const NOT_OPTED_IN: &str = "not-opted-in";
const OPT_IN_NOT_REQUIRED: &str = "opt-in-not-required";

/// The `RegionOptStatus` the Account API reports for an opt-in Region nobody has enabled, and
/// the one it reports for a Region that needs no opt-in — two of the five values that enum
/// carries.
const DISABLED: &str = "DISABLED";
const ENABLED_BY_DEFAULT: &str = "ENABLED_BY_DEFAULT";

/// How AWS answers a request addressed to a Region the account has not turned on: the
/// credential is not valid where it was presented. Written out rather than imported, so a
/// wrong refusal in the implementation cannot make these assertions pass.
const UNRECOGNIZED_CLIENT: &str = "UnrecognizedClientException";
const INVALID_TOKEN_MESSAGE: &str = "The security token included in the request is invalid.";

/// How long a case waits for an opt-in or an opt-out to take effect on the gated endpoint
/// before calling the gate broken — generous slack over the Account API's own transition,
/// which this suite deliberately does not import: it polls for the OBSERVED answer to
/// change rather than for a duration it was told.
const SETTLE_DEADLINE: Duration = Duration::from_secs(10);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The table every DynamoDB case names. The rule is about the Region a request addresses,
/// so nothing else about the table varies.
const TABLE: &str = "RegionOptInSweep";

/// One emulator carrying Amazon EC2, the Account API and DynamoDB over ONE shared engine,
/// as the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_account_account::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`,
/// which is what routes the request to that service's endpoint in that Region; the
/// signature itself is never checked.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `DescribeRegions` call that must succeed, answered with its XML body.
async fn describe_regions(router: &MultiServiceRouter, params: &[(String, String)]) -> String {
    let mut body = format!("Action=DescribeRegions&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{SETUP_REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2", SETUP_REGION))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 DescribeRegions must succeed, got {status} with {text}"
    );
    text
}

/// The Region codes EC2 publishes with the opt-in status `status`, in Region-code order.
///
/// Read through the operation's own `opt-in-status` filter, so each set comes back as a
/// listing of its class rather than from a pairing of two element lists.
async fn published_regions(router: &MultiServiceRouter, status: &str) -> Vec<String> {
    let params = vec![
        ("AllRegions".to_string(), "true".to_string()),
        ("Filter.1.Name".to_string(), "opt-in-status".to_string()),
        ("Filter.1.Value.1".to_string(), status.to_string()),
    ];
    let body = describe_regions(router, &params).await;
    let mut names = elements(&body, "regionName");
    names.sort();
    names
}

/// One DynamoDB call addressed to `region`, answered with the HTTP status and the parsed
/// body. The credential scope and the endpoint host both name that Region, which is how a
/// client addresses a regional endpoint.
async fn dynamodb(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization("dynamodb", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Account API call, answered with the HTTP status and the parsed body.
async fn account(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("account.{SETUP_REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("account", SETUP_REGION))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The awsJson error code of a body, with any shape namespace stripped.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The error envelope's message, or `""` when there is none.
fn message(body: &Value) -> &str {
    body.get("message")
        .and_then(Value::as_str)
        .or_else(|| body.get("Message").and_then(Value::as_str))
        .unwrap_or_default()
}

/// The simplest `CreateTable` body there is, under `name`.
fn create_table_body(name: &str) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "Id", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST",
    })
}

/// The operations each gated case presents to a Region: an account-wide read, a read that
/// names a table, a control-plane mutation and a data-plane write — so the gate is shown to
/// stand ahead of each class rather than only ahead of a read.
fn gated_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("ListTables", json!({})),
        ("DescribeTable", json!({"TableName": TABLE})),
        ("CreateTable", create_table_body(TABLE)),
        (
            "PutItem",
            json!({"TableName": TABLE, "Item": {"Id": {"S": "one"}}}),
        ),
    ]
}

/// Assert that `status` and `body` are the refusal a Region the account has not turned on
/// answers with — the status, the code and the sentence, which are the whole answer — and
/// that the answer carries no result a client could read as an available, empty Region.
fn assert_refused(case: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, error_code(body).as_str(), message(body)),
        (403, UNRECOGNIZED_CLIENT, INVALID_TOKEN_MESSAGE),
        "case {case}: expected 403 {UNRECOGNIZED_CLIENT} with AWS's own sentence, got {status} \
         with {body}"
    );
    assert!(
        body.get("TableNames").is_none() && body.get("TableDescription").is_none(),
        "case {case}: a Region the account cannot use must answer no result at all, got {body}"
    );
}

/// The `TableNames` a `ListTables` answer reports.
fn table_names(listed: &Value) -> Vec<String> {
    listed["TableNames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|name| name.as_str().map(ToString::to_string))
        .collect()
}

/// The opt status is reported for EVERY Region of the partition this emulator publishes, and
/// for the same classes: the Account API's Region listing is exactly the Regions EC2
/// publishes, each `DISABLED` where EC2 publishes it as needing an opt-in and
/// `ENABLED_BY_DEFAULT` where EC2 publishes it as needing none.
///
/// A Region one of them publishes and the other knows nothing about is what leaves the gated
/// endpoints open: a Region with no opt status is a Region nothing can refuse a request to.
#[tokio::test]
async fn the_account_reports_an_opt_status_for_every_region_ec2_publishes() {
    let router = emulator();
    let opt_in = published_regions(&router, NOT_OPTED_IN).await;
    let open = published_regions(&router, OPT_IN_NOT_REQUIRED).await;
    let mut expected: Vec<(String, String)> = opt_in
        .iter()
        .map(|region| (region.clone(), DISABLED.to_string()))
        .chain(
            open.iter()
                .map(|region| (region.clone(), ENABLED_BY_DEFAULT.to_string())),
        )
        .collect();
    expected.sort();
    assert!(
        expected.len() >= 30,
        "setup: EC2 must publish the partition's Regions, got {expected:?}"
    );

    let (status, listed) = account(&router, "/listRegions", &json!({})).await;
    let mut reported: Vec<(String, String)> = listed["Regions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|region| {
            (
                region["RegionName"].as_str().unwrap_or_default().to_string(),
                region["RegionOptStatus"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    reported.sort();
    assert_eq!(
        (status, reported),
        (200, expected),
        "case \"the account's Region listing\": every published Region must carry the opt \
         status its class reports, got {status} with {listed}"
    );
}

/// Every Region EC2 publishes as needing an opt-in is refused by the DynamoDB endpoint, and
/// the Account API reports that same Region as an opt-in Region nobody has enabled.
///
/// One row per published opt-in Region, crossed with the four operation classes: the set is
/// read off EC2 rather than written here, so a Region the emulator publishes but no opt-in
/// state covers is a failing row rather than a silently served Region.
#[tokio::test]
async fn every_region_published_as_needing_an_opt_in_refuses_dynamodb() {
    let router = emulator();
    let opt_in = published_regions(&router, NOT_OPTED_IN).await;
    assert!(
        opt_in.len() >= 10,
        "setup: EC2 must publish the partition's opt-in Regions, got {opt_in:?}"
    );

    for region in &opt_in {
        for (operation, body) in gated_calls() {
            let (status, answered) = dynamodb(&router, region, operation, &body).await;
            assert_refused(&format!("{operation} in {region}"), status, &answered);
        }

        // The Account API owns the opt-in, so a Region published as needing one has to
        // carry that opt status there: a Region it knows nothing about has no opt-in to
        // make and no status for an endpoint to be gated on.
        let (status, reported) = account(
            &router,
            "/getRegionOptStatus",
            &json!({"RegionName": region}),
        )
        .await;
        assert_eq!(
            (
                status,
                reported["RegionName"].clone(),
                reported["RegionOptStatus"].clone(),
            ),
            (200, json!(region), json!(DISABLED)),
            "case {region}: a Region published as needing an opt-in must report {DISABLED} \
             until the account enables it, got {status} with {reported}"
        );
    }
}

/// Every Region EC2 publishes as needing NO opt-in serves the account: the endpoint answers
/// its reads, and a table created there is read back from there.
#[tokio::test]
async fn every_region_published_as_needing_no_opt_in_serves_dynamodb() {
    let router = emulator();
    let open = published_regions(&router, OPT_IN_NOT_REQUIRED).await;
    assert!(
        open.len() >= 10,
        "setup: EC2 must publish the Regions that need no opt-in, got {open:?}"
    );
    let opt_in = published_regions(&router, NOT_OPTED_IN).await;
    assert!(
        open.iter().all(|region| !opt_in.contains(region)),
        "setup: a Region cannot be published in both classes, got {open:?} and {opt_in:?}"
    );

    for region in &open {
        let (status, listed) = dynamodb(&router, region, "ListTables", &json!({})).await;
        assert_eq!(
            (status, table_names(&listed)),
            (200, Vec::<String>::new()),
            "case ListTables in {region}: a Region needing no opt-in serves its requests, got \
             {status} with {listed}"
        );

        let name = format!("{TABLE}{}", region.replace('-', ""));
        let (status, created) = dynamodb(&router, region, "CreateTable", &create_table_body(&name))
            .await;
        assert_eq!(
            (status, created["TableDescription"]["TableArn"].clone()),
            (
                200,
                json!(format!("arn:aws:dynamodb:{region}:{ACCOUNT}:table/{name}"))
            ),
            "case CreateTable in {region}: an open Region creates the table, got {status} with \
             {created}"
        );
        let (status, listed) = dynamodb(&router, region, "ListTables", &json!({})).await;
        assert_eq!(
            (status, table_names(&listed)),
            (200, vec![name.clone()]),
            "case ListTables in {region} after the create: the table is read back where it was \
             created, got {status} with {listed}"
        );
    }
}

/// The opt-in itself is what opens a published opt-in Region for DynamoDB: enabling it
/// through the Account API makes the endpoint serve that Region, and disabling it again
/// closes it. The Region is the first one EC2 publishes as needing an opt-in, so this case
/// names none of its own.
#[tokio::test]
async fn an_account_opt_in_opens_a_published_opt_in_region_for_dynamodb() {
    let router = emulator();
    let opt_in = published_regions(&router, NOT_OPTED_IN).await;
    let region = opt_in
        .first()
        .cloned()
        .expect("setup: EC2 publishes at least one opt-in Region");

    // Before the opt-in: refused, and the refused mutation wrote nothing.
    let (status, refused) = dynamodb(&router, &region, "CreateTable", &create_table_body(TABLE))
        .await;
    assert_refused(
        &format!("CreateTable in {region} before the opt-in"),
        status,
        &refused,
    );

    let (status, enabled) = account(&router, "/enableRegion", &json!({"RegionName": region})).await;
    assert_eq!(
        status, 200,
        "case EnableRegion {region}: the opt-in must be accepted, got {status} with {enabled}"
    );

    // The opt-in settles on the Account API's own schedule, so this waits for the CONDITION.
    let listed = poll_until_served(&router, &region).await;
    assert!(
        table_names(&listed).is_empty(),
        "case ListTables in {region} after the opt-in: the Region a refused create addressed \
         holds nothing, got {listed}"
    );

    let (status, created) = dynamodb(&router, &region, "CreateTable", &create_table_body(TABLE))
        .await;
    assert_eq!(
        (status, created["TableDescription"]["TableName"].clone()),
        (200, json!(TABLE)),
        "case CreateTable in {region} after the opt-in: got {status} with {created}"
    );
    let (status, described) = dynamodb(
        &router,
        &region,
        "DescribeTable",
        &json!({"TableName": TABLE}),
    )
    .await;
    assert_eq!(
        (status, described["Table"]["TableName"].clone()),
        (200, json!(TABLE)),
        "case DescribeTable in {region} after the opt-in: got {status} with {described}"
    );

    let (status, disabled) = account(&router, "/disableRegion", &json!({"RegionName": region}))
        .await;
    assert_eq!(
        status, 200,
        "case DisableRegion {region}: the opt-out must be accepted, got {status} with {disabled}"
    );
    let (status, after) = poll_until_refused(&router, &region).await;
    assert_refused(
        &format!("ListTables in {region} after the opt-out"),
        status,
        &after,
    );
}

/// Poll `ListTables` in `region` until it is SERVED, answering the body it answered then.
async fn poll_until_served(router: &MultiServiceRouter, region: &str) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < SETTLE_DEADLINE {
        let (status, body) = dynamodb(router, region, "ListTables", &json!({})).await;
        if status == 200 {
            return body;
        }
        last = body;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case the opt-in of {region}: it never served ListTables within {SETTLE_DEADLINE:?}, \
         last {last}"
    )
}

/// Poll `ListTables` in `region` until it is REFUSED, answering what it was refused with.
async fn poll_until_refused(router: &MultiServiceRouter, region: &str) -> (u16, Value) {
    let started = Instant::now();
    let mut last = (0, Value::Null);
    while started.elapsed() < SETTLE_DEADLINE {
        let (status, body) = dynamodb(router, region, "ListTables", &json!({})).await;
        if status != 200 {
            return (status, body);
        }
        last = (status, body);
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case the opt-out of {region}: it never stopped serving ListTables within \
         {SETTLE_DEADLINE:?}, last {last:?}"
    )
}
