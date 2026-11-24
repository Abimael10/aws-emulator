// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the version an ALIAS-QUALIFIED `Invoke` reaches after the alias has
//! been moved: a traffic shift of the kind a deployment performs, read back through both
//! `GetAlias` and the run itself.
//!
//! The rule under test: a qualifier is resolved PER INVOCATION, so the code an
//! alias-qualified `Invoke` runs is the code of the version that alias currently names —
//! `GetAlias().FunctionVersion`, the `X-Amz-Executed-Version` the answer carries and the
//! package that produced the payload all name one version, however many times the alias has
//! been moved since it was created ("use aliases to provide clients with a function
//! identifier that you can update to invoke a different version", `CreateAlias`; "the version
//! of the function that executed. When you invoke a function with an alias, this indicates
//! which version the alias resolved to", `smithy_query(query="operation",
//! operation="Invoke")`).
//!
//! The rows differ along what the rule generalises over: which version the alias is moved to
//! and in which direction (forward one, forward two at once, back, and back to where it
//! started), which form the invocation addresses the alias by (the `:`-suffixed
//! `FunctionName` and the `Qualifier` parameter), how the version being moved to was published
//! (`CreateFunction`'s own `Publish`, and `UpdateFunctionCode` followed by a separate
//! `PublishVersion`), and whether that update's own window had closed when the publish was
//! made. Each row also invokes the version DIRECTLY, because the alias and the version it
//! names answering the same payload is what says the two agree.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module every package below holds its handler in.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The alias every row moves, which is the one a deployment shifts traffic on.
const ALIAS: &str = "live";

/// How many versions each function below publishes.
const VERSIONS: u32 = 4;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Move every stored function's activation instant into the past, so the next read
/// materialises the `Pending` -> `Active` transition the clock owes. This is what waiting
/// would do, without waiting.
fn make_activation_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// Move every stored function's update-settled instant into the past, so the next read
/// materialises the `InProgress` -> `Successful` move the clock owes — which is what a
/// client waiting for an update to be applied waits for.
fn make_update_settled(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        if function.last_update_due.is_some() {
            function.last_update_due = Some(past);
        }
    }
}

/// The handler a version reports itself from: each published version carries its own number,
/// so the value an answer returns says which version's code ran.
fn source_reporting(version: u32) -> String {
    format!("def lambda_handler(event, context):\n    return {{'v': {version}}}\n")
}

/// Create the function `name` with [`VERSIONS`] published versions, version N running the
/// handler that returns `{'v': N}`, and the alias [`ALIAS`] pointing at version 1.
///
/// Version 1 is published by `CreateFunction` itself; every later one by
/// `UpdateFunctionCode` and a separate `PublishVersion`, which is the sequence a client
/// preparing a deployment makes.
async fn create_function_with_versions(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Publish": true,
            "Timeout": 5,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_reporting(1))),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    make_activation_due(engine);
    for version in 2..=VERSIONS {
        let updated = call(
            router,
            "PUT",
            &format!("{FUNCTIONS_URI}/{name}/code"),
            &serde_json::to_vec(&json!({
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_reporting(version))),
            }))
            .expect("the update body serializes"),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "UpdateFunctionCode on {name:?} must succeed, got {} {}",
            updated.status,
            updated.text()
        );
        make_update_settled(engine);
        let published = call(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/versions"),
            b"{}",
        )
        .await;
        assert_eq!(
            published.status,
            201,
            "PublishVersion on {name:?} must succeed, got {} {}",
            published.status,
            published.text()
        );
        assert_eq!(
            published.json()["Version"],
            json!(version.to_string()),
            "the {version}th published version must be numbered {version}, got {}",
            published.json()["Version"]
        );
    }
    let alias = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &serde_json::to_vec(&json!({ "Name": ALIAS, "FunctionVersion": "1" }))
            .expect("the alias body serializes"),
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "CreateAlias on {name:?} must succeed, got {} {}",
        alias.status,
        alias.text()
    );
}

/// Move the alias onto `version`, the way a traffic shift of a deployment does.
async fn move_alias(router: &MultiServiceRouter, name: &str, version: &str) {
    let moved = call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/aliases/{ALIAS}"),
        &serde_json::to_vec(&json!({ "FunctionVersion": version }))
            .expect("the alias body serializes"),
    )
    .await;
    assert_eq!(
        moved.status,
        200,
        "UpdateAlias to {version:?} must succeed, got {} {}",
        moved.status,
        moved.text()
    );
}

/// The `FunctionVersion` `GetAlias` reports for the alias of `name`.
async fn alias_version(router: &MultiServiceRouter, name: &str) -> String {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/aliases/{ALIAS}"),
        b"",
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "GetAlias on {name:?} must succeed, got {} {}",
        read.status,
        read.text()
    );
    read.json()["FunctionVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `Invoke` the reference `reference` names, synchronously, naming `qualifier` as the
/// `Qualifier` parameter when there is one.
async fn invoke(router: &MultiServiceRouter, reference: &str, qualifier: Option<&str>) -> Answer {
    let uri = match qualifier {
        Some(qualifier) => {
            format!("{FUNCTIONS_URI}/{reference}/invocations?Qualifier={qualifier}")
        }
        None => format!("{FUNCTIONS_URI}/{reference}/invocations"),
    };
    call(router, "POST", &uri, b"{}").await
}

/// Assert that invoking `reference` runs the code of `expected`: the payload the version's
/// own handler returns, and the `X-Amz-Executed-Version` naming it.
async fn assert_runs_version(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    qualifier: Option<&str>,
    expected: &str,
) {
    let answer = invoke(router, reference, qualifier).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: Invoke of {reference:?} must answer 200, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.header("x-amz-function-error"),
        None,
        "case {case:?}: Invoke of {reference:?} must report no function error, got {:?}",
        answer.header("x-amz-function-error")
    );
    assert_eq!(
        answer.header("x-amz-executed-version").as_deref(),
        Some(expected),
        "case {case:?}: Invoke of {reference:?} must report ExecutedVersion {expected:?}, got \
         {:?}",
        answer.header("x-amz-executed-version")
    );
    assert_eq!(
        answer.json(),
        json!({ "v": expected.parse::<u32>().expect("the expected version is a number") }),
        "case {case:?}: Invoke of {reference:?} must run the code version {expected} froze, \
         got {}",
        answer.text()
    );
}

/// Once the alias names a version, every invocation of it runs THAT version's code: the
/// alias read, the executed version reported and the payload produced agree after each move.
#[tokio::test]
async fn an_alias_qualified_invoke_runs_the_version_the_alias_reads() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "alias-shift-invoke";
    create_function_with_versions(&router, &engine, name).await;

    // (the case, the version the alias is moved onto)
    let rows: &[(&str, &str)] = &[
        ("the version the alias was created on", "1"),
        ("one version forward", "2"),
        ("the next version forward", "3"),
        ("two versions back at once", "1"),
        ("two versions forward at once", "3"),
        ("the newest version", "4"),
        ("back to the version the alias started on", "1"),
    ];
    for (case, version) in rows {
        move_alias(&router, name, version).await;
        let read = alias_version(&router, name).await;
        assert_eq!(
            read, *version,
            "case {case:?}: GetAlias must report FunctionVersion {version:?}, got {read:?}"
        );
        // The two ways a caller addresses the same alias-qualified invocation, and the
        // version itself: all three run the one version the alias names.
        assert_runs_version(&router, case, &format!("{name}:{ALIAS}"), None, version).await;
        assert_runs_version(&router, case, name, Some(ALIAS), version).await;
        assert_runs_version(&router, case, &format!("{name}:{version}"), None, version).await;
    }
}

/// The package a version FROZE is the one the update before the publish uploaded, whether or
/// not that update's window had closed when the publish was made: a version is a snapshot of
/// the package the function was given, never of the one it replaced.
///
/// This is the other half of the agreement an alias-qualified invocation needs — the alias
/// names a version, and that version carries the code its publish froze — so the row that
/// publishes inside the update window is the one that would show a snapshot taken one update
/// behind.
#[tokio::test]
async fn a_published_version_runs_the_package_the_update_before_it_uploaded() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "alias-publish-snapshot";
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Publish": true,
            "Timeout": 5,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_reporting(1))),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    make_activation_due(&engine);
    let alias = call(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &serde_json::to_vec(&json!({ "Name": ALIAS, "FunctionVersion": "1" }))
            .expect("the alias body serializes"),
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "CreateAlias on {name:?} must succeed, got {} {}",
        alias.status,
        alias.text()
    );

    // (the case, the version this row publishes, whether the update's window is closed before
    // the publish is made)
    let rows: &[(&str, u32, bool)] = &[
        ("a publish after the update settled", 2, true),
        ("a publish made inside the update's own window", 3, false),
        ("a settled update again, after one published in flight", 4, true),
    ];
    for (case, version, settle_first) in rows {
        let updated = call(
            &router,
            "PUT",
            &format!("{FUNCTIONS_URI}/{name}/code"),
            &serde_json::to_vec(&json!({
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&source_reporting(*version))),
            }))
            .expect("the update body serializes"),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: UpdateFunctionCode must succeed, got {} {}",
            updated.status,
            updated.text()
        );
        if *settle_first {
            make_update_settled(&engine);
        }
        let published = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/versions"),
            b"{}",
        )
        .await;
        assert_eq!(
            published.status,
            201,
            "case {case:?}: PublishVersion must succeed, got {} {}",
            published.status,
            published.text()
        );
        let label = version.to_string();
        assert_eq!(
            published.json()["Version"],
            json!(label),
            "case {case:?}: the publish must be numbered {label}, got {}",
            published.json()["Version"]
        );
        // The version itself, and the alias once it is moved onto it, both run the package
        // this row uploaded.
        assert_runs_version(&router, case, &format!("{name}:{label}"), None, &label).await;
        move_alias(&router, name, &label).await;
        let read = alias_version(&router, name).await;
        assert_eq!(
            read, label,
            "case {case:?}: GetAlias must report FunctionVersion {label:?}, got {read:?}"
        );
        assert_runs_version(&router, case, &format!("{name}:{ALIAS}"), None, &label).await;
        // The window every row left open is settled before the next one's update, which a
        // function with an update in flight would refuse.
        make_update_settled(&engine);
    }
}

/// A SECOND invocation of the same alias, after it moved again, resolves the qualifier
/// afresh: the first run cannot pin the version the next one reaches.
#[tokio::test]
async fn a_moved_alias_is_resolved_again_on_the_next_invocation() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "alias-shift-reresolved";
    create_function_with_versions(&router, &engine, name).await;

    // Each step invokes the alias BEFORE and AFTER the move, so a cached resolution from the
    // invocation just made would be read by the invocation that follows it.
    let steps: &[(&str, &str)] = &[
        ("the first shift", "2"),
        ("the shift the issue's deployment performs", "3"),
        ("a shift that rolls back", "2"),
    ];
    assert_runs_version(
        &router,
        "before any shift",
        &format!("{name}:{ALIAS}"),
        None,
        "1",
    )
    .await;
    for (case, version) in steps {
        move_alias(&router, name, version).await;
        let read = alias_version(&router, name).await;
        assert_eq!(
            read, *version,
            "case {case:?}: GetAlias must report FunctionVersion {version:?}, got {read:?}"
        );
        assert_runs_version(&router, case, &format!("{name}:{ALIAS}"), None, version).await;
        assert_runs_version(&router, case, &format!("{name}:{ALIAS}"), None, version).await;
    }
}
