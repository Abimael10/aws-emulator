// SPDX-License-Identifier: CC0-1.0

//! The Region OPT-IN gate ahead of every operation, driven end to end over the wire.
//!
//! RULE: an account reaches a Region only once that Region is turned on for it — the
//! Regions AWS turns on for every account, plus the opt-in Regions the account has enabled
//! through the Account API. A request addressed to a Region the account has NOT turned on
//! is REFUSED, with the code and sentence AWS answers a credential that is not valid where
//! it was presented; it is never served, and in particular never answered with an empty
//! result, because an empty result is byte-identical to what an available, empty Region
//! answers and a client walking the partition's Regions would read a Region it cannot use
//! as one it can. Which Regions are on for the account is the ACCOUNT API's state
//! (`EnableRegion` / `DisableRegion`), so a Region enabled through it serves this service's
//! requests from then on, and one disabled again stops serving them.
//!
//! The rows differ along what the rule generalises over:
//!
//! * WHICH Region — four opt-in Regions from four geographies, four Regions enabled by
//!   default, and Regions that carry no opt-in status at all (another partition's, and a
//!   well-formed code naming no published Region), none of them the one the issue cited;
//! * WHICH field of the endpoint names the Region — the SigV4 credential scope, the
//!   endpoint host, and the scope a presigned request carries — because the gate is about
//!   the endpoint a request reached rather than about one header;
//! * WHICH operation the request names — an account-wide read, a read that names a table, a
//!   control-plane MUTATION and a data-plane write — because the gate is ahead of all of
//!   them;
//! * and the account's own opt-in STATE: a Region never opted into, one enabled through the
//!   Account API, and one disabled again.
//!
//! The Region catalogue below is written out here, with its source named, rather than
//! imported from either crate, so a wrong catalogue in the implementation cannot make these
//! assertions pass.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_common::authentication_refusal::UNRESOLVED_CREDENTIAL_MESSAGE;
use service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{error_code, message};

/// Regions the AWS General Reference's Region table and the Account Management guide
/// ("Specify which AWS Regions your account can use") mark as needing an opt-in: an account
/// starts with them disabled. Four rows from four geographies, and none of them the Region
/// the issue this suite was written for cited.
const OPT_IN_REGIONS: [&str; 4] = ["ap-east-1", "eu-central-2", "il-central-1", "me-south-1"];

/// Regions the same source marks enabled by default, so every account reaches them without
/// opting in. Four rows from four geographies.
const DEFAULT_ENABLED_REGIONS: [&str; 4] = ["us-east-1", "eu-west-3", "ap-northeast-3", "sa-east-1"];

/// Region codes that carry no opt-in status for this account at all: Regions of the
/// GovCloud, China and isolated partitions (an account of the commercial partition reaches
/// none of them, and the Account API publishes no opt-in for them), and a well-formed code
/// naming no published Region. The gate answers what AWS publishes about Regions rather
/// than reading authorization out of caller-controlled endpoint text, so these are served.
const UNPUBLISHED_REGIONS: [&str; 4] = [
    "us-gov-west-1",
    "cn-north-1",
    "us-iso-east-1",
    "zz-central-9",
];

/// The Region a row opts in and out of again, which is an opt-in Region of the catalogue
/// above that no other test here enables.
const OPTED_IN_REGION: &str = "ap-southeast-4";

/// The code AWS answers a request whose credential is not valid where it was presented
/// with, on an `awsJson` endpoint — this service's own unknown-credential code, the same one
/// it answers an access key id that resolves to nothing with.
const UNRECOGNIZED_CLIENT: &str = "UnrecognizedClientException";

/// How long a row waits for the account's opt-in state to take effect on this service before
/// calling the gate broken. Generous slack over the Account API's own transition span, which
/// this suite deliberately does not import: a row polls for the OBSERVED answer to change
/// rather than for a duration it was told, so a loaded machine is not a failure.
const SETTLE_DEADLINE: Duration = Duration::from_secs(10);

/// How long a row waits between polls.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One emulator holding this service AND the Account API, over ONE shared engine — the way
/// `core/router` mounts them. The Account API is what turns a Region on for the account, and
/// this service reads that state, so a suite about the gate needs both.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_account_account::EmulatedService::new(
        engine,
    ));
    router
}

/// Which field of its endpoint a request names its Region in — one variant per tier the
/// framework reads a request's endpoint from.
#[derive(Debug, Clone, Copy)]
enum Addressed {
    /// `Authorization: AWS4-HMAC-SHA256 Credential=…/<region>/dynamodb/aws4_request, …`
    SignedScope,
    /// `Host: dynamodb.<region>.amazonaws.com`, with no credential scope at all.
    RegionalHost,
    /// The credential scope a presigned request carries in `X-Amz-Credential`.
    PresignedScope,
}

/// Every field the rows span.
const ADDRESSINGS: [Addressed; 3] = [
    Addressed::SignedScope,
    Addressed::RegionalHost,
    Addressed::PresignedScope,
];

impl Addressed {
    /// Put this Region field on a request under construction.
    fn apply(self, builder: ::http::request::Builder, region: &str) -> ::http::request::Builder {
        match self {
            Self::SignedScope => builder.uri("/").header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/dynamodb/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
                ),
            ),
            Self::RegionalHost => builder
                .uri("/")
                .header("host", format!("dynamodb.{region}.amazonaws.com")),
            Self::PresignedScope => builder.uri(format!(
                "/?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDTEST%2F20260101%2F\
                 {region}%2Fdynamodb%2Faws4_request&X-Amz-SignedHeaders=host&\
                 X-Amz-Signature=0000"
            )),
        }
    }
}

/// Send one DynamoDB operation addressed to `region` through `addressed`, answering the HTTP
/// status and the parsed body.
async fn call(
    router: &MultiServiceRouter,
    addressed: Addressed,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let builder = ::http::Request::builder()
        .method("POST")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"));
    let request = addressed
        .apply(builder, region)
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Send one ACCOUNT API call (`restJson1`, one URI per operation), answering the HTTP status
/// and the parsed body. The credential scope names that service, which is what routes it
/// there.
async fn account_call(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", "account.us-east-1.amazonaws.com")
        .header("content-type", "application/json")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/us-east-1/account/aws4_request, \
             SignedHeaders=host, Signature=0000",
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The operations a row presents to a Region, and the body each carries: an account-wide
/// read, a read that names a table, a control-plane MUTATION, and a data-plane write — so
/// the gate is shown to land ahead of each class rather than only ahead of a read.
fn calls() -> Vec<(&'static str, Value)> {
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

/// The table every row names. The rule is about the Region a request addresses, so nothing
/// else about the table varies.
const TABLE: &str = "RegionOptInGate";

/// The simplest `CreateTable` body there is, under `name`.
fn create_table_body(name: &str) -> Value {
    json!({
        "TableName": name,
        "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "Id", "AttributeType": "S"}],
        "BillingMode": "PAY_PER_REQUEST",
    })
}

/// Assert that `status` and `body` are the refusal a Region the account has not turned on
/// answers with: the status, the code and the sentence, which are the whole answer.
fn assert_refused(case: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, error_code(body).as_str(), message(body).as_str()),
        (403, UNRECOGNIZED_CLIENT, UNRESOLVED_CREDENTIAL_MESSAGE),
        "case {case}: expected 403 {UNRECOGNIZED_CLIENT} with AWS's own sentence, got {status} \
         with {body}"
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

/// Every operation addressed to a Region the account has not opted into is refused — and
/// refused the same way whichever field of the endpoint names that Region.
#[tokio::test]
async fn a_region_the_account_has_not_opted_into_refuses_every_operation() {
    let router = emulator();

    for region in OPT_IN_REGIONS {
        for addressed in ADDRESSINGS {
            for (operation, body) in calls() {
                let (status, answered) = call(&router, addressed, region, operation, &body).await;
                assert_refused(
                    &format!("{operation} in {region} addressed by {addressed:?}"),
                    status,
                    &answered,
                );
                assert!(
                    answered.get("TableNames").is_none(),
                    "case {operation} in {region} addressed by {addressed:?}: a Region the \
                     account cannot use must not answer a result a client could read as an \
                     empty Region, got {answered}"
                );
            }
        }
    }
}

/// A Region the account CAN use serves every one of those operations: the Regions AWS turns
/// on by default, and the Region codes the Account API publishes no opt-in for at all.
#[tokio::test]
async fn a_region_the_account_can_use_serves_its_requests() {
    let router = emulator();

    for region in DEFAULT_ENABLED_REGIONS.iter().chain(&UNPUBLISHED_REGIONS) {
        for addressed in ADDRESSINGS {
            let (status, listed) = call(&router, addressed, region, "ListTables", &json!({})).await;
            assert_eq!(
                status, 200,
                "case ListTables in {region} addressed by {addressed:?}: a Region this account \
                 can use serves its requests, got {status} with {listed}"
            );
        }
        // ...and a MUTATION in it is served too, which is what a Region being open means:
        // the table is created there and read back from there.
        let name = format!("{TABLE}{}", region.replace('-', ""));
        let (status, created) = call(
            &router,
            Addressed::SignedScope,
            region,
            "CreateTable",
            &create_table_body(&name),
        )
        .await;
        assert_eq!(
            (status, created["TableDescription"]["TableArn"].clone()),
            (
                200,
                json!(engine::emulator_arn(
                    "dynamodb",
                    region,
                    &format!("table/{name}")
                ))
            ),
            "case CreateTable in {region}: an open Region creates the table, got {status} with \
             {created}"
        );
        let (status, described) = call(
            &router,
            Addressed::SignedScope,
            region,
            "ListTables",
            &json!({}),
        )
        .await;
        assert_eq!(
            (status, table_names(&described)),
            (200, vec![name.clone()]),
            "case ListTables in {region} after the create: the table is read back where it was \
             created, got {status} with {described}"
        );
    }
}

/// Enabling an opt-in Region through the ACCOUNT API opens it for this service, and
/// disabling it again closes it — the gate reads the account's own opt-in state rather than
/// a copy of a catalogue.
#[tokio::test]
async fn enabling_a_region_through_the_account_api_opens_it_for_this_service() {
    let router = emulator();

    // Before any opt-in: refused, and the refused MUTATION writes nothing.
    let (status, refused) = call(
        &router,
        Addressed::SignedScope,
        OPTED_IN_REGION,
        "CreateTable",
        &create_table_body(TABLE),
    )
    .await;
    assert_refused(
        &format!("CreateTable in {OPTED_IN_REGION} before the opt-in"),
        status,
        &refused,
    );

    // The opt-in itself, made through the API that owns it.
    let (status, enabled) = account_call(
        &router,
        "/enableRegion",
        &json!({"RegionName": OPTED_IN_REGION}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case EnableRegion {OPTED_IN_REGION}: the opt-in must be accepted, got {status} with \
         {enabled}"
    );

    // The opt-in completes on the Account API's own schedule, so the row polls for the
    // answer to change rather than waiting out a span it was told.
    let listed = poll_until_listed(&router, "after the opt-in", OPTED_IN_REGION).await;
    assert!(
        table_names(&listed).is_empty(),
        "case ListTables in {OPTED_IN_REGION} after the opt-in: the Region a refused create \
         addressed holds nothing, got {listed}"
    );

    // An open Region serves the whole surface, control plane and data plane: the table is
    // created there, read back there, and the item written to it is read back from it.
    let (status, created) = call(
        &router,
        Addressed::SignedScope,
        OPTED_IN_REGION,
        "CreateTable",
        &create_table_body(TABLE),
    )
    .await;
    assert_eq!(
        (status, created["TableDescription"]["TableArn"].clone()),
        (
            200,
            json!(engine::emulator_arn(
                "dynamodb",
                OPTED_IN_REGION,
                &format!("table/{TABLE}")
            ))
        ),
        "case CreateTable in {OPTED_IN_REGION} after the opt-in: got {status} with {created}"
    );
    let (status, described) = call(
        &router,
        Addressed::SignedScope,
        OPTED_IN_REGION,
        "DescribeTable",
        &json!({"TableName": TABLE}),
    )
    .await;
    assert_eq!(
        (status, described["Table"]["TableName"].clone()),
        (200, json!(TABLE)),
        "case DescribeTable in {OPTED_IN_REGION} after the opt-in: got {status} with {described}"
    );

    // Disabling it again closes the Region: the endpoint stops serving this account.
    let (status, disabled) = account_call(
        &router,
        "/disableRegion",
        &json!({"RegionName": OPTED_IN_REGION}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case DisableRegion {OPTED_IN_REGION}: the opt-out must be accepted, got {status} with \
         {disabled}"
    );
    let (status, after) = poll_until_refused(&router, OPTED_IN_REGION).await;
    assert_refused(
        &format!("ListTables in {OPTED_IN_REGION} after the opt-out"),
        status,
        &after,
    );
}

/// Poll `ListTables` in `region` until it is SERVED, answering the body it answered then.
///
/// The condition, not a duration: a Region whose opt-in has not settled yet is still
/// refused, so the row looks again until the Region serves it or the generous
/// [`SETTLE_DEADLINE`] passes.
async fn poll_until_listed(router: &MultiServiceRouter, case: &str, region: &str) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < SETTLE_DEADLINE {
        let (status, body) = call(router, Addressed::SignedScope, region, "ListTables", &json!({}))
            .await;
        if status == 200 {
            return body;
        }
        last = body;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case}: {region} never served ListTables within {SETTLE_DEADLINE:?}, last {last}")
}

/// Poll `ListTables` in `region` until it is REFUSED, answering the status and body it was
/// refused with — the mirror of [`poll_until_listed`] for an opt-out that has to settle.
async fn poll_until_refused(router: &MultiServiceRouter, region: &str) -> (u16, Value) {
    let started = Instant::now();
    let mut last = (0, Value::Null);
    while started.elapsed() < SETTLE_DEADLINE {
        let (status, body) = call(router, Addressed::SignedScope, region, "ListTables", &json!({}))
            .await;
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
