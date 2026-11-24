// SPDX-License-Identifier: CC0-1.0

//! A directory's RADIUS multi-factor authentication configuration, driven over the ASSEMBLED
//! router: `EnableRadius`, `DisableRadius` and what `DescribeDirectories` then reports.
//!
//! The case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body, dispatched
//! by the multi-service router. Nothing a RESPONSE claims is read from the state struct, so
//! an answer that is not computed from persisted state cannot pass.
//!
//! RULE: the configuration is a record of the directory — absent until an enable records one,
//! then the settings that enable named with its shared secret dropped and a modelled
//! `RadiusStatus` that converges on `Completed`, and absent again once a disable has
//! withdrawn it — and a directory carries at most one, so a second enable is refused with
//! `ClientException` and changes nothing.
//!
//! What this suite adds to the owning crate's own cases
//! (`services/service-com-amazonaws-directoryservice-directoryservice-20150416/tests/all/directory_networking_and_settings.rs`,
//! which asserts the same rules over tables of directory types, settings blocks and refusal
//! classes) is the DISPATCH half: the two operations are reached through the router that
//! assembles every service, so a configuration recorded through it is read back through it.
//!
//! Error source:
//! `smithy_query(query="operation",operation="EnableRadius,DisableRadius",model_path="services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json")`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix Directory Service names an operation under: its service shape's
/// name.
const TARGET_PREFIX: &str = "DirectoryService_20150416";

/// A password the modelled `Password` pattern admits, for the create below.
const PASSWORD: &str = "Passw0rd!2345";

/// A shared secret the modelled `RadiusSharedSecret` admits (length 8–512, pattern
/// `^(\p{LD}|\p{Punct}| )+$`).
const SHARED_SECRET: &str = "radius-shared-secret-1";

/// The status a configuration reports once the transition the enable owes has fallen due:
/// the `Completed` value of the modelled `RadiusStatus` enum
/// (`smithy_query(query="shape", shape="RadiusStatus")` answers
/// `["Creating", "Completed", "Failed"]`).
const COMPLETED: &str = "Completed";

/// How long the polled status may take to converge before the case fails: far longer than any
/// span this service publishes, and well inside what a client's waiter allows. Nothing is
/// asserted about HOW LONG the transition took — only that it converges — so a loaded or a
/// fast machine reaches the same verdict.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often the poll asks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Directory Service mounted on the router, as the production router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The case exercises the service, not SigV4: requests carry a credential scope so the
    // router can name the target service, but no real signature.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_directoryservice_directoryservice_20150416::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One Directory Service control-plane call: its status and its parsed body.
async fn ds(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ds/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A call that must succeed, returning its body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = ds(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// The one description `DescribeDirectories` reports for `directory_id`.
async fn describe_one(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Value {
    let answer = ok(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
        case,
    )
    .await;
    let found = answer["DirectoryDescriptions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected DirectoryDescriptions, got {answer}"))
        .clone();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one description for {directory_id}, got {found:?}"
    );
    found[0].clone()
}

/// Wait until `directory_id` reports `awaited` as its RADIUS status, polling the reporting
/// operation the way a client with a waiter does: the CONDITION is waited for rather than a
/// span of time, so the case's outcome does not depend on how fast the host runs.
async fn poll_until_radius_status(
    router: &MultiServiceRouter,
    directory_id: &str,
    awaited: &str,
    case: &str,
) {
    let started = Instant::now();
    loop {
        let described = describe_one(router, directory_id, case).await;
        let observed = described["RadiusStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if observed == awaited {
            return;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected {directory_id} to report RadiusStatus {awaited} within \
             {POLL_DEADLINE:?}, last observed {observed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_radius_configuration_round_trips_through_the_assembled_router() {
    let case = "a Managed Microsoft AD directory configured over the router";
    let router = emulator();

    let created = ok(
        &router,
        "CreateMicrosoftAD",
        json!({
            "Name": "radius.router.example.com",
            "Password": PASSWORD,
            "VpcSettings": {
                "VpcId": "vpc-0a1b2c3d",
                "SubnetIds": ["subnet-0a1b2c3d", "subnet-1a2b3c4d"]
            }
        }),
        case,
    )
    .await;
    let id = created["DirectoryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a DirectoryId, got {created}"))
        .to_string();

    // A directory that holds no configuration describes none.
    let fresh = describe_one(&router, &id, case).await;
    assert!(
        fresh["RadiusSettings"].is_null() && fresh["RadiusStatus"].is_null(),
        "case {case:?}: expected neither RadiusSettings nor RadiusStatus before an enable, \
         got {fresh}"
    );

    ok(
        &router,
        "EnableRadius",
        json!({
            "DirectoryId": &id,
            "RadiusSettings": {
                "RadiusServers": ["radius1.example.com", "radius2.example.com"],
                "RadiusPort": 1812,
                "RadiusTimeout": 30,
                "RadiusRetries": 3,
                "SharedSecret": SHARED_SECRET,
                "AuthenticationProtocol": "MS-CHAPv2",
            }
        }),
        case,
    )
    .await;

    // The status is the service's own transition, so it converges without a further call;
    // the settings are then reported member for member, and the secret is not among them.
    poll_until_radius_status(&router, &id, COMPLETED, case).await;
    let described = describe_one(&router, &id, case).await;
    let settings = &described["RadiusSettings"];
    assert_eq!(
        settings["RadiusServers"],
        json!(["radius1.example.com", "radius2.example.com"]),
        "case {case:?}: expected the endpoints the enable named, got {described}"
    );
    assert_eq!(
        (
            settings["RadiusPort"].clone(),
            settings["RadiusTimeout"].clone(),
            settings["RadiusRetries"].clone(),
            settings["AuthenticationProtocol"].clone(),
        ),
        (json!(1812), json!(30), json!(3), json!("MS-CHAPv2")),
        "case {case:?}: expected the settings the enable named, got {described}"
    );
    assert!(
        !described.to_string().contains(SHARED_SECRET),
        "case {case:?}: expected no shared secret anywhere in the description, got {described}"
    );

    // A directory carries at most one configuration, and the refused second enable changes
    // nothing.
    let (status, refused) = ds(
        &router,
        "EnableRadius",
        json!({
            "DirectoryId": &id,
            "RadiusSettings": { "SharedSecret": SHARED_SECRET, "RadiusPort": 1645 }
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a second EnableRadius to answer 400, got {status} with \
         {refused}"
    );
    assert!(
        refused["__type"]
            .as_str()
            .unwrap_or_default()
            .contains("ClientException"),
        "case {case:?}: expected ClientException from a second EnableRadius, got {refused}"
    );
    assert_eq!(
        refused["Message"].as_str().map(|message| message.starts_with(
            &format!(
                "Could not enable radius as it is already enabled or currently being \
                 configured for directory: {id}"
            )
        )),
        Some(true),
        "case {case:?}: expected the service's sentence for a repeated enable, got {refused}"
    );
    assert_eq!(
        describe_one(&router, &id, case).await["RadiusSettings"]["RadiusPort"],
        json!(1812),
        "case {case:?}: expected the first configuration untouched by the refusal"
    );

    // The withdrawal takes it off the record, so the reads report what they did before.
    ok(&router, "DisableRadius", json!({ "DirectoryId": &id }), case).await;
    let withdrawn = describe_one(&router, &id, case).await;
    assert!(
        withdrawn["RadiusSettings"].is_null() && withdrawn["RadiusStatus"].is_null(),
        "case {case:?}: expected neither RadiusSettings nor RadiusStatus after the disable, \
         got {withdrawn}"
    );
}
