// SPDX-License-Identifier: CC0-1.0

//! WHICH `execute-api:Invoke` grants reach one method of a deployed REST stage: the matching of
//! a statement's `Resource` against the method ARN the request named.
//!
//! The RULE the rows assert, one level above any recorded case:
//!
//! > Every request that reaches a deployed stage is answered, and the answer to a request on a
//! > method whose `authorizationType` is `AWS_IAM` is decided by comparing the method ARN the
//! > REQUEST named —
//! > `arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{verb}/{path}`, built from the
//! > normalized path the client addressed — against the `Resource` of each statement in force
//! > on the principal the credential speaks for. That comparison is IAM's: the pattern is
//! > matched over the WHOLE ARN, case-sensitively, with `*` standing for any run of characters
//! > and `?` for exactly one, and nothing else implied — so a grant naming a parent path, a
//! > differently cased verb or path, another stage, or an api id the account holds no api for
//! > covers nothing, while one carrying a wildcard covers exactly what that wildcard spans. A
//! > request no grant covers is refused `403` with the sentence IAM words an authorization
//! > failure in, naming the principal, the `execute-api:Invoke` action and that same method ARN.
//!
//! Three services and one engine, so this is a router suite: `iam` holds the role and the one
//! document each row rewrites, `cognito-identity` and `sts` mint and register the session the
//! invoke is signed as, and the gate that reads them is API Gateway's own `execute-api` data
//! plane. The sibling [`crate::apigateway_iam_authorized_invoke`] pins the OTHER half of the
//! same rule — which DOCUMENT decides (a grant of everything, a grant of nothing, an explicit
//! deny, no credential at all) — and this suite reuses its harness rather than keeping a second
//! copy of it, so the two cannot disagree about how a session is vended or an api stood up.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the granted ARN (the
//! method's own, the verb in another case, the path in another case, the parent path with and
//! without a trailing wildcard, a wildcard in place of the verb, the single-character wildcard,
//! another stage, an api id no api of the account carries) and the SPELLING of the path the
//! client addressed (the plain one, and one carrying a doubled slash, which is normalized before
//! the decision is taken). Every row asserts a status and a body, so a request the stage answers
//! with nothing fails here.
//!
//! API Gateway's own `tests/all/stage_invoke_identity_policy.rs` already reads the grants whose
//! ARN names another stage, another verb or another path of the same api, and
//! `tests/all/stage_invoke_denial_sentence.rs` the doubled-slash spellings of the path a client
//! addresses, so those rows stand here as the baseline the wildcard ones are judged against —
//! the doubled-slash ones beside the SHAPE rows, because the two dimensions meet only here. What
//! only this table reaches is the shape of the pattern itself: the verb and the path in another
//! CASE, the parent path with and without a trailing wildcard, a wildcard in place of the verb,
//! the single-character wildcard, and an api id no api of the account carries.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling suite does.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_iam_authorized_invoke::{
    as_str, control_plane, credential_scope, dispatch, policy_document, vended_session,
    ASSUMED_ROLE, BOOT_STACK_SIZE, INVOKE_ACTION, NOT_AUTHORIZED_CLAUSE, REGION,
};

/// The stage the api is deployed to.
const STAGE: &str = "s";

/// The collection resource, which carries no method of its own, and the item under it, which
/// carries the one `AWS_IAM` `GET` the stage serves.
const COLLECTION: &str = "agreements";
const ITEM: &str = "one";

/// The body the `MOCK` integration behind that method answers an admitted request with — what
/// proves the request reached the integration, so a refusal must never carry it.
const SERVED_BODY: &str = r#"{"served":"agreements/one"}"#;

/// An api id no api of this account holds: ten lowercase alphanumerics, the width and alphabet
/// an API Gateway api id is minted in, so a statement naming it is a document IAM stores like
/// any other and the stage is the only thing that can refuse it.
const UNHELD_API_ID: &str = "zzz9999999";

/// The account every ARN of this emulator names, which the method ARN a decision is taken about
/// and the refusal that reports it both carry.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// What the gate owes one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The grant covers the method ARN the request named: the `MOCK` integration answered it.
    Served,
    /// It does not: `403` with the IAM authorization sentence about that same ARN.
    NotAuthorized,
}

/// One invoke: the `Resource` of the single grant its session carries, the path it addresses
/// inside the stage, and what the gate owes it.
struct Row {
    case: &'static str,
    resource: String,
    path: String,
    expected: Expected,
}

/// The method ARN of `verb` on the path `within` names inside [`STAGE`] of `api` — the resource
/// an authorization decision about such a call is taken about, and the one its refusal reports.
fn method_arn(api: &str, verb: &str, within: &str) -> String {
    format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/{STAGE}/{verb}/{within}")
}

/// Stand up one api carrying a methodless collection with an `AWS_IAM` `GET` item under it,
/// behind a `MOCK` integration that answers [`SERVED_BODY`], deployed to [`STAGE`]. Answers the
/// api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let api = control_plane(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "invoke-grant-arn-matching" })),
        201,
    )
    .await;
    let api_id = as_str("CreateRestApi", &api, "id");
    let root = as_str("CreateRestApi", &api, "rootResourceId");

    // The collection exists as a RESOURCE and carries no method: the shape a parent of a
    // per-item method has, so a grant naming its path names a path the deployment really holds.
    let collection = control_plane(
        router,
        "CreateResource(collection)",
        "POST",
        &format!("/restapis/{api_id}/resources/{root}"),
        Some(json!({ "pathPart": COLLECTION })),
        201,
    )
    .await;
    let collection_id = as_str("CreateResource(collection)", &collection, "id");
    let item = control_plane(
        router,
        "CreateResource(item)",
        "POST",
        &format!("/restapis/{api_id}/resources/{collection_id}"),
        Some(json!({ "pathPart": ITEM })),
        201,
    )
    .await;
    let item_id = as_str("CreateResource(item)", &item, "id");

    let method = format!("/restapis/{api_id}/resources/{item_id}/methods/GET");
    control_plane(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "AWS_IAM" })),
        201,
    )
    .await;
    control_plane(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({ "type": "MOCK" })),
        201,
    )
    .await;
    control_plane(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    control_plane(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({ "responseTemplates": { "application/json": SERVED_BODY } })),
        201,
    )
    .await;
    control_plane(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api_id}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api_id
}

/// The rows, over the api `api_id`: one grant shape per row, and the path its client addresses.
fn rows(api_id: &str) -> Vec<Row> {
    let served_path = format!("/{COLLECTION}/{ITEM}");
    let within = format!("{COLLECTION}/{ITEM}");
    vec![
        Row {
            case: "the method's own ARN, exactly",
            resource: method_arn(api_id, "GET", &within),
            path: served_path.clone(),
            expected: Expected::Served,
        },
        Row {
            case: "the same ARN with the verb spelled lowercase",
            resource: method_arn(api_id, "get", &within),
            path: served_path.clone(),
            expected: Expected::NotAuthorized,
        },
        Row {
            case: "the same ARN with the path spelled in another case",
            resource: method_arn(api_id, "GET", &format!("Agreements/{ITEM}")),
            path: served_path.clone(),
            expected: Expected::NotAuthorized,
        },
        Row {
            case: "the parent path, carrying no wildcard",
            resource: method_arn(api_id, "GET", COLLECTION),
            path: served_path.clone(),
            expected: Expected::NotAuthorized,
        },
        Row {
            case: "the parent path and a trailing wildcard",
            resource: method_arn(api_id, "GET", &format!("{COLLECTION}/*")),
            path: served_path.clone(),
            expected: Expected::Served,
        },
        Row {
            case: "a wildcard in place of the verb",
            resource: method_arn(api_id, "*", &within),
            path: served_path.clone(),
            expected: Expected::Served,
        },
        Row {
            case: "the single-character wildcard over the last character of the path",
            resource: method_arn(api_id, "GET", &format!("{COLLECTION}/on?")),
            path: served_path.clone(),
            expected: Expected::Served,
        },
        Row {
            case: "the same method of another stage",
            resource: format!(
                "arn:aws:execute-api:{REGION}:{ACCOUNT}:{api_id}/other/GET/{within}"
            ),
            path: served_path.clone(),
            expected: Expected::NotAuthorized,
        },
        Row {
            case: "an api id this account holds no api for",
            resource: method_arn(UNHELD_API_ID, "GET", &within),
            path: served_path.clone(),
            expected: Expected::NotAuthorized,
        },
        Row {
            case: "the method's own ARN, addressed through a doubled slash",
            resource: method_arn(api_id, "GET", &within),
            path: format!("/{COLLECTION}//{ITEM}"),
            expected: Expected::Served,
        },
        Row {
            case: "the method's own ARN, addressed with a doubled slash ahead of the path",
            resource: method_arn(api_id, "GET", &within),
            path: format!("//{COLLECTION}/{ITEM}"),
            expected: Expected::Served,
        },
    ]
}

/// RULE: the grant's `Resource` is matched against the method ARN the request named, over the
/// whole ARN, case-sensitively, with `*` and `?` the only wildcards — and whatever that
/// comparison answers, the stage answers the request.
#[test]
fn an_invoke_grants_resource_is_matched_against_the_method_arn_the_request_named() {
    std::thread::Builder::new()
        .name("apigateway-invoke-grant-arn-matching".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs, as the sibling
                // suites of this target build theirs.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                let api_id = deployed(&router).await;
                let invoke_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
                // The two halves of the method ARN a refusal reports, around the masked
                // account field: the service and Region fields, and everything the request
                // itself named.
                let refused_arn_prefix = format!("arn:aws:execute-api:{REGION}:");
                let refused_arn_tail =
                    format!("{api_id}/{STAGE}/GET/{COLLECTION}/{ITEM}");

                for (index, row) in rows(&api_id).iter().enumerate() {
                    let case = row.case;
                    // One session per row, carrying that row's grant and nothing else.
                    let key_id = vended_session(
                        &router,
                        &format!("grant-shape-{index}"),
                        Some(policy_document("Allow", INVOKE_ACTION, &row.resource)),
                    )
                    .await;
                    let authorization = credential_scope(&key_id, "execute-api");
                    let answer = dispatch(
                        &router,
                        &invoke_host,
                        "GET",
                        &format!("/{STAGE}{}", row.path),
                        &[("authorization", authorization.as_str())],
                        Vec::new(),
                    )
                    .await;
                    match row.expected {
                        Expected::Served => {
                            assert_eq!(
                                answer.status,
                                200,
                                "case {case:?}: the grant {:?} covers the method ARN the \
                                 request named, so the integration owes it a 200, got {}",
                                row.resource,
                                answer.summary()
                            );
                            assert_eq!(
                                answer.json(),
                                serde_json::from_str::<Value>(SERVED_BODY)
                                    .expect("SERVED_BODY is the integration's JSON body"),
                                "case {case:?}: expected the integration's own body, got {}",
                                answer.summary()
                            );
                        }
                        Expected::NotAuthorized => {
                            assert_eq!(
                                answer.status,
                                403,
                                "case {case:?}: the grant {:?} covers no method ARN the \
                                 request named, so a 403 is owed, got {}",
                                row.resource,
                                answer.summary()
                            );
                            let message = answer.message();
                            assert!(
                                message.contains(NOT_AUTHORIZED_CLAUSE),
                                "case {case:?}: expected the refusal to name the \
                                 {INVOKE_ACTION} action, got {message:?}"
                            );
                            assert!(
                                message.contains(ASSUMED_ROLE),
                                "case {case:?}: expected the refusal to name the federated \
                                 session the request was signed as, got {message:?}"
                            );
                            assert!(
                                message.contains(&refused_arn_prefix)
                                    && message.contains(&refused_arn_tail),
                                "case {case:?}: expected the refusal to name the method ARN of \
                                 the request — {refused_arn_prefix:?} followed by \
                                 {refused_arn_tail:?} — got {message:?}"
                            );
                            assert!(
                                !answer.text.contains(SERVED_BODY),
                                "case {case:?}: a refused request must never reach the \
                                 integration, got {}",
                                answer.summary()
                            );
                        }
                    }
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
