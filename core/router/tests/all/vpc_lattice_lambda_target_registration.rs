// SPDX-License-Identifier: CC0-1.0

//! A Lambda function registered as the TARGET of a VPC Lattice `LAMBDA` target group,
//! across the two services that own the halves of it: Lambda mints the function and the
//! ARN that names it, and VPC Lattice holds the target group that ARN is registered with.
//!
//! This suite needs more than one service by construction: what a target of a `LAMBDA`
//! target group IS, is "the ARN of a Lambda function" (`Target`'s own `id`
//! documentation), so the id under test is one Lambda itself handed out — no
//! single-service test can assert that the ARN one service mints is the one the other
//! admits as a target.
//!
//! The RULE asserted over the tables below, in one sentence: `RegisterTargets` answers
//! from the persisted target group — HTTP 200 whose `successful` carries every target the
//! call stored, which is exactly what `ListTargets` reports afterwards — and it refuses
//! only through the errors its own model declares, so no call of it answers an error the
//! model does not carry.
//!
//! The rows differ along what the rule generalises over: the event structure version the
//! target group was created with (both values the config admits, and the default an
//! omitted config takes), the identifier form the registration names the target group by
//! (the bare id and the ARN, the two forms `TargetGroupIdentifier` admits), how many
//! times the same target is registered, and — in the refusal table — the class each
//! refusal belongs to.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and the Region every ARN carries.
const REGION: &str = "us-west-2";

/// Where Lambda binds `CreateFunction` (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where VPC Lattice binds `CreateTargetGroup` (`POST /targetgroups`).
const TARGET_GROUPS_URI: &str = "/targetgroups";

/// A well-formed execution role ARN, satisfying Lambda's modelled `RoleArn` pattern.
/// Which role it is is IAM's concern, not this suite's.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lattice-target";

/// An opaque deployment package, base64-encoded as `FunctionCode$ZipFile` is on the wire.
/// Nothing here invokes the function: the target under test is the function's ARN, not
/// what the package does.
const PACKAGE_ZIP_BASE64: &str = "cGFja2FnZQ==";

/// Every error `RegisterTargets` declares, with the status each carries, as the model
/// lists them
/// (`smithy_query(query="operation",operation="RegisterTargets",
/// model_path="services/service-com-amazonaws-vpclattice-mercurycontrolplane/smithy-model/com-amazonaws-vpclattice-mercurycontrolplane.json")`).
/// A wire code outside this catalog is not an answer this operation has — the scaffold
/// channel's own `NotImplemented` (HTTP 501) among them.
const MODELLED_ERRORS: &[(&str, u16)] = &[
    ("AccessDeniedException", 403),
    ("ConflictException", 409),
    ("InternalServerException", 500),
    ("ResourceNotFoundException", 404),
    ("ServiceQuotaExceededException", 402),
    ("ThrottlingException", 429),
    ("ValidationException", 400),
];

/// Which of the two forms `TargetGroupIdentifier` admits a row names the target group by.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// The bare `tg-…` id.
    Id,
    /// The target group's ARN.
    Arn,
}

/// What one call answered: the HTTP status, the parsed body, and the wire error code.
struct Answered {
    status: u16,
    body: Value,
    code: Option<String>,
}

/// Lambda and VPC Lattice over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them: the function is created where a client creates it, and the target group holds
/// the ARN that service minted.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_vpclattice_mercurycontrolplane::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes
/// the request and what names the Region its ARNs carry. The signature is never checked
/// here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDROUTERTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `restJson1` call to the service `signing` names.
async fn call(
    router: &MultiServiceRouter,
    signing: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answered {
    let payload = body
        .map(|value| value.to_string().into_bytes())
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing}.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(signing))
        .body(bytes::Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    Answered {
        status,
        body: parsed,
        code,
    }
}

/// One Lambda call.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answered {
    call(router, "lambda", method, uri, body).await
}

/// One VPC Lattice call.
async fn lattice(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answered {
    call(router, "vpc-lattice", method, uri, body).await
}

/// An ARN used as an `@httpLabel`, encoded the way an AWS SDK encodes a non-greedy label:
/// the `/` before the resource id becomes `%2F` so it stays one path segment.
fn as_label(value: &str) -> String {
    value.replace('/', "%2F")
}

/// A function called `name`, answering the `FunctionArn` LAMBDA minted for it — the id a
/// target of a `LAMBDA` target group is.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": PACKAGE_ZIP_BASE64 },
        })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateFunction {name:?} answered {} / {}",
        created.status, created.body
    );
    created.body["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: CreateFunction {name:?} reported no FunctionArn: {}",
                created.body
            )
        })
        .to_string()
}

/// A `LAMBDA` target group called `name`, created with `config`, answering its id and its
/// ARN as the service minted them.
async fn given_lambda_target_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    config: Option<Value>,
) -> (String, String) {
    let mut body = json!({ "name": name, "type": "LAMBDA" });
    if let Some(config) = config {
        body["config"] = config;
    }
    let created = lattice(router, "POST", TARGET_GROUPS_URI, Some(&body)).await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateTargetGroup {body} answered {} / {}",
        created.status, created.body
    );
    let id = created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the target group has no id: {}", created.body))
        .to_string();
    let arn = created.body["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the target group has no arn: {}", created.body))
        .to_string();
    (id, arn)
}

/// `RegisterTargets` against the target group `identifier` names, with the targets this
/// row submits.
async fn register_targets(
    router: &MultiServiceRouter,
    identifier: &str,
    targets: Value,
) -> Answered {
    lattice(
        router,
        "POST",
        &format!("/targetgroups/{identifier}/registertargets"),
        Some(&json!({ "targets": targets })),
    )
    .await
}

/// The target ids one member of a response carries, sorted so two answers compare by
/// content rather than by order.
fn target_ids(case: &str, answer: &Answered, member: &str) -> Vec<String> {
    let entries = answer.body[member].as_array().cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the member {member} to be a list, got {}",
            answer.body
        )
    });
    let mut ids: Vec<String> = entries
        .iter()
        .map(|entry| {
            entry["id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a target without an id: {entry}"))
                .to_string()
        })
        .collect();
    ids.sort();
    ids
}

/// The target ids `ListTargets` reports for `identifier`, read back out of a LATER
/// request: a registration that was never stored fails this read.
async fn listed_target_ids(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<String> {
    let listed = lattice(
        router,
        "POST",
        &format!("/targetgroups/{identifier}/listtargets"),
        Some(&json!({})),
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListTargets through {identifier:?} answered {} / {}",
        listed.status, listed.body
    );
    target_ids(case, &listed, "items")
}

/// Assert that one refused call answered an error the model declares, with the status that
/// error carries — and so never the scaffold channel's own 501.
fn assert_modelled_refusal(case: &str, answer: &Answered, expected: &str) {
    let catalogued = MODELLED_ERRORS
        .iter()
        .find(|(code, _)| Some(*code) == answer.code.as_deref());
    let Some((code, status)) = catalogued else {
        panic!(
            "case {case:?}: expected one of the errors the model declares {MODELLED_ERRORS:?}, \
             got {:?} with status {} / {}",
            answer.code, answer.status, answer.body
        );
    };
    assert_eq!(
        *code, expected,
        "case {case:?}: expected the {expected} class, got {code} / {}",
        answer.body
    );
    assert_eq!(
        answer.status, *status,
        "case {case:?}: expected {expected} to carry HTTP {status}, got {} / {}",
        answer.status, answer.body
    );
}

/// The message a refusal carries, which every error shape of this service declares
/// required.
fn message_of(case: &str, answer: &Answered) -> String {
    answer.body["message"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the exception must carry the required message member, got {}",
                answer.body
            )
        })
        .to_string()
}

/// The rule's success half, over a table that varies the event structure version the
/// target group carries and the identifier form the registration names it by.
#[tokio::test]
async fn a_lambda_functions_own_arn_registers_as_a_target_and_reads_back() {
    let router = emulator();

    // (case, the config the target group is created with, how the registration names it)
    let cases: Vec<(&str, Option<Value>, Named)> = vec![
        (
            "no config at all, the target group named by its id",
            None,
            Named::Id,
        ),
        (
            "the V1 event structure, named by its id",
            Some(json!({ "lambdaEventStructureVersion": "V1" })),
            Named::Id,
        ),
        (
            "the V1 event structure, named by its ARN",
            Some(json!({ "lambdaEventStructureVersion": "V1" })),
            Named::Arn,
        ),
        (
            "the V2 event structure, named by its ARN",
            Some(json!({ "lambdaEventStructureVersion": "V2" })),
            Named::Arn,
        ),
    ];

    for (index, (case, config, named)) in cases.into_iter().enumerate() {
        let function_arn = given_function(&router, case, &format!("lattice-target-{index}")).await;
        let (id, arn) =
            given_lambda_target_group(&router, case, &format!("lattice-lambda-{index}"), config)
                .await;
        let identifier = match named {
            Named::Id => id.clone(),
            Named::Arn => as_label(&arn),
        };

        let registered =
            register_targets(&router, &identifier, json!([{ "id": function_arn }])).await;
        assert_eq!(
            registered.status, 200,
            "case {case:?}: registering {function_arn} answered {} / {}",
            registered.status, registered.body
        );
        assert_eq!(
            registered.code, None,
            "case {case:?}: a stored registration carries no error code, got {:?} / {}",
            registered.code, registered.body
        );
        assert_eq!(
            target_ids(case, &registered, "successful"),
            vec![function_arn.clone()],
            "case {case:?}: successful must carry the function ARN, got {}",
            registered.body
        );
        assert_eq!(
            target_ids(case, &registered, "unsuccessful"),
            Vec::<String>::new(),
            "case {case:?}: no target of this registration fails, got {}",
            registered.body
        );
        // "This doesn't apply if the target is a Lambda function" (`Target`'s `port`), so
        // the stored target is reached on no port of its own.
        assert_eq!(
            registered.body["successful"][0]["port"],
            Value::Null,
            "case {case:?}: a function target is reached on no port, got {}",
            registered.body
        );

        // The registration is state, not an answer: both identifier forms read it back.
        for reader in [id.clone(), as_label(&arn)] {
            let listed = listed_target_ids(&router, case, &reader).await;
            assert_eq!(
                listed,
                vec![function_arn.clone()],
                "case {case:?}: ListTargets through {reader:?} must report the registered \
                 function, got {listed:?}"
            );
        }

        // A target is its id together with the port it is reached on, so registering the
        // same pair again holds ONE target and still reports it as stored.
        let again = register_targets(&router, &identifier, json!([{ "id": function_arn }])).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the repeated registration answered {} / {}",
            again.status, again.body
        );
        assert_eq!(
            target_ids(case, &again, "successful"),
            vec![function_arn.clone()],
            "case {case:?}: the repeated registration reports the same target, got {}",
            again.body
        );
        let listed = listed_target_ids(&router, case, &id).await;
        assert_eq!(
            listed,
            vec![function_arn.clone()],
            "case {case:?}: the target group still holds one target, got {listed:?}"
        );
    }
}

/// The rule's refusal half: every class this operation refuses through answers an error
/// the model declares, with that error's own status and a message built from the request.
#[tokio::test]
async fn registering_a_target_refuses_only_through_the_errors_the_model_declares() {
    let router = emulator();
    let case = "the estate every row below is refused against";
    let held = given_function(&router, case, "lattice-target-held").await;
    let spare = given_function(&router, case, "lattice-target-spare").await;
    let (id, _arn) =
        given_lambda_target_group(&router, case, "lattice-lambda-refusals", None).await;
    let registered = register_targets(&router, &id, json!([{ "id": held }])).await;
    assert_eq!(
        registered.status, 200,
        "case {case:?}: the first registration answered {} / {}",
        registered.status, registered.body
    );

    // A well-formed target group id that nothing ever created: 17 `[0-9a-z]` characters
    // after the prefix, which is what `TargetGroupIdentifier`'s pattern demands, so the
    // request reaches the handler instead of being refused at the boundary.
    let absent = format!("tg-{}", "0".repeat(17));

    let refused = register_targets(&router, &absent, json!([{ "id": spare }])).await;
    let case = "a target group nothing created";
    assert_modelled_refusal(case, &refused, "ResourceNotFoundException");
    let message = message_of(case, &refused);
    assert!(
        message.contains(&absent),
        "case {case:?}: the refusal must name the submitted identifier {absent:?}, \
         got {message:?}"
    );

    let case = "a target that is not the form a LAMBDA target group documents";
    let foreign = "i-0123456789abcdef0";
    let refused = register_targets(&router, &id, json!([{ "id": foreign }])).await;
    assert_modelled_refusal(case, &refused, "ValidationException");
    let message = message_of(case, &refused);
    assert!(
        message.contains(foreign),
        "case {case:?}: the refusal must name the submitted target {foreign:?}, got {message:?}"
    );

    let case = "a second function in a target group whose type holds one";
    let refused = register_targets(&router, &id, json!([{ "id": spare }])).await;
    assert_modelled_refusal(case, &refused, "ValidationException");
    let message = message_of(case, &refused);
    assert!(
        message.contains(&id) && message.contains("LAMBDA"),
        "case {case:?}: the refusal must name the target group {id:?} and its type, \
         got {message:?}"
    );

    // Every refusal above left the registration this target group already held exactly as
    // it was.
    let listed = listed_target_ids(&router, case, &id).await;
    assert_eq!(
        listed,
        vec![held.clone()],
        "case {case:?}: a refused registration changes nothing, got {listed:?}"
    );
}
