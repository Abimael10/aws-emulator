// SPDX-License-Identifier: CC0-1.0

//! The METHOD ARN an `execute-api` authorization decision is taken about, and that its refusal
//! reports, over the HTTP VERB the client sent — including a request served by the resource's
//! `ANY` catch-all.
//!
//! The RULE the rows assert, one level above any recorded case:
//!
//! > The resource an `AWS_IAM`-authorized invoke is judged against, and the one its refusal
//! > names, is `arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{VERB}/{path}` built
//! > from what the REQUEST addressed — the verb the client sent and the normalized path it
//! > asked for — never from the matched method's stored definition, whose verb may be the
//! > `ANY` catch-all and whose path may be a parameter template. A request no grant covers is
//! > refused `403` with `x-amzn-ErrorType: AccessDeniedException` and the sentence
//! > `User: {principal} is not authorized to perform: execute-api:Invoke on resource:
//! > {method-arn} {reason}`, closed by the clause naming the document class that refused it.
//!
//! `ANY` is the name a method is CONFIGURED under for every verb that has no method of its
//! own, so no request on the wire carries it and no statement can be written about it: a grant
//! naming `ANY` covers nothing, while one naming the verb the client sent, or a wildcard in
//! its place, covers exactly that request. This is the verb half of the same rule the path
//! half of which makes a grant on one concrete path admit it and deny a sibling id.
//!
//! Three services and one engine, so this is a router suite: `iam` holds the role and the one
//! document each session carries, `cognito-identity` and `sts` mint and register the session
//! the invoke is signed as (and `sts:GetCallerIdentity` answers the principal a refusal must
//! blame), and the gate that reads them is API Gateway's own `execute-api` data plane. The
//! sibling [`crate::apigateway_iam_authorized_invoke`] pins WHICH DOCUMENT decides and
//! [`crate::apigateway_invoke_grant_arn_matching`] how a grant's `Resource` PATTERN is matched;
//! both drive one verb on a method put on that verb, so what only this table reaches is the
//! verb the ARN itself names — over five verbs of one `ANY` method, a method put on its own
//! verb beside them, and the three grant spellings of a verb (its own, `ANY`, a wildcard).
//! Its harness is that first suite's, so the three cannot disagree about how a session is
//! vended or an api stood up.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling suites do.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_iam_authorized_invoke::{
    as_str, control_plane, credential_scope, dispatch, policy_document, vended_session,
    BOOT_STACK_SIZE, INVOKE_ACTION, REGION,
};

/// The stage the api is deployed to.
const STAGE: &str = "beta";

/// The literal collection, and the PATH PARAMETER under it carrying the one method every verb
/// of this suite is served by: the `ANY` catch-all.
const COLLECTION: &str = "data-agreements";
const ITEM_TEMPLATE: &str = "{agreementId}";

/// Two sibling values of that parameter: distinct resources of ONE deployed method.
const ITEM: &str = "dua-benchmark-001";
const SIBLING_ITEM: &str = "dua-002";

/// A literal resource carrying a method put on its OWN verb, which is the row the `ANY` ones
/// are judged against: there the configured verb and the addressed one are the same string.
const LITERAL: &str = "widgets";

/// The name a method is configured under to serve every verb that has no method of its own.
const ANY_METHOD: &str = "ANY";

/// The verbs the `ANY` method serves, from the HTTP methods `PutMethod` admits for a method
/// (`crate::apigateway_iam_authorized_invoke` stands its own api up on one of them). `HEAD`
/// and `OPTIONS` are left out: a `HEAD` answer carries no body to read a sentence out of, and
/// an `OPTIONS` request is the preflight the stage answers on its own.
const SERVED_VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE"];

/// The body the `MOCK` integration behind every method answers an admitted request with — what
/// proves the request reached the integration, so a refusal must never carry it.
const SERVED_BODY: &str = r#"{"served":"agreement"}"#;

/// The account every ARN of this emulator names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// How many digits of the account the `execute-api` endpoint leaves unmasked in the resource a
/// refusal reports: it publishes all but the last four as `*`.
const UNMASKED_ACCOUNT_DIGITS: usize = 4;

/// `sts:GetCallerIdentity`'s own API version, for the awsQuery call that reads back the
/// principal a vended session speaks for.
const STS_API_VERSION: &str = "2011-06-15";

/// What the gate owes one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The session's grant covers the method ARN the request named: the `MOCK` integration
    /// answered it.
    Served,
    /// It does not: `403` with the IAM authorization sentence about that same ARN.
    NotAuthorized,
}

/// One invoke: which session signs it, the verb and path it addresses, and what the gate owes
/// it.
struct Row {
    case: String,
    /// The index into the sessions vended below.
    session: usize,
    verb: &'static str,
    /// The path within the stage, leading `/` included.
    within: String,
    /// The path the ARN of this call names, with no leading `/` — the ARN's own separator is
    /// already there.
    judged: String,
    expected: Expected,
}

/// The method ARN of `verb` on the path `within` names inside [`STAGE`] of `api`: the resource
/// a statement names such a call by, and the one a decision about it is taken about.
fn method_arn(api: &str, verb: &str, within: &str) -> String {
    format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/{STAGE}/{verb}/{within}")
}

/// The same resource as a refusal REPORTS it, with the account masked down to its last digits.
fn reported_resource(api: &str, verb: &str, within: &str) -> String {
    let masked = format!(
        "{mask}{tail}",
        mask = "*".repeat(ACCOUNT.len().saturating_sub(UNMASKED_ACCOUNT_DIGITS)),
        tail = &ACCOUNT[ACCOUNT.len().saturating_sub(UNMASKED_ACCOUNT_DIGITS)..],
    );
    method_arn(api, verb, within).replacen(&format!(":{ACCOUNT}:"), &format!(":{masked}:"), 1)
}

/// The whole sentence an invoke nothing allows is refused with: who was refused, the action,
/// the resource, and the clause naming the document class that withheld the grant.
fn implicit_deny_sentence(principal: &str, reported: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {INVOKE_ACTION} on resource: \
         {reported} because no identity-based policy allows the {INVOKE_ACTION} action"
    )
}

/// The `Arn` `sts:GetCallerIdentity` answers for `access_key_id`: the principal that
/// credential speaks for, and so the one a refusal of a request signed with it must blame.
async fn caller_arn(router: &MultiServiceRouter, access_key_id: &str) -> String {
    let authorization = credential_scope(access_key_id, "sts");
    let answer = dispatch(
        router,
        &format!("sts.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", authorization.as_str()),
        ],
        format!("Action=GetCallerIdentity&Version={STS_API_VERSION}").into_bytes(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: GetCallerIdentity under a vended credential must answer 200, got {}",
        answer.summary()
    );
    let body = answer.text;
    let start = body
        .find("<Arn>")
        .unwrap_or_else(|| panic!("setup: GetCallerIdentity answered no <Arn>: {body}"))
        + "<Arn>".len();
    let end = body[start..]
        .find("</Arn>")
        .unwrap_or_else(|| panic!("setup: GetCallerIdentity answered no </Arn>: {body}"))
        + start;
    body[start..end].to_string()
}

/// Put one `AWS_IAM` method on `resource_id` under `verb`, answered by a `MOCK` integration
/// that serves [`SERVED_BODY`].
async fn mock_method(router: &MultiServiceRouter, api: &str, resource_id: &str, verb: &str) {
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/{verb}");
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
}

/// One resource under `parent` of `api` carrying `path_part`; answers its id.
async fn resource_under(
    router: &MultiServiceRouter,
    api: &str,
    parent: &str,
    path_part: &str,
) -> String {
    let created = control_plane(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{parent}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    as_str("CreateResource", &created, "id")
}

/// Stand up one api: a path parameter under a literal collection carrying the `ANY` catch-all,
/// and a literal resource carrying a `GET` of its own, both `AWS_IAM` behind `MOCK`
/// integrations, deployed to [`STAGE`]. Answers the api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let api = control_plane(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "invoke-denial-request-verb" })),
        201,
    )
    .await;
    let api_id = as_str("CreateRestApi", &api, "id");
    let root = as_str("CreateRestApi", &api, "rootResourceId");

    let collection = resource_under(router, &api_id, &root, COLLECTION).await;
    let item = resource_under(router, &api_id, &collection, ITEM_TEMPLATE).await;
    mock_method(router, &api_id, &item, ANY_METHOD).await;

    let literal = resource_under(router, &api_id, &root, LITERAL).await;
    mock_method(router, &api_id, &literal, "GET").await;

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

/// The rows: the sessions are indexed as they are vended in the test body — `0` carries no
/// document at all, `1` a grant on the `GET` of one item, `2` a grant naming the `ANY` the
/// method is configured under, and `3` a wildcard in the verb's place.
fn rows() -> Vec<Row> {
    let item = format!("{COLLECTION}/{ITEM}");
    let addressed = format!("/{item}");
    let mut rows: Vec<Row> = Vec::new();

    // Every verb one `ANY` method serves, signed by a session nothing grants: the ARN each
    // refusal reports names the verb the CLIENT sent.
    for verb in SERVED_VERBS {
        rows.push(Row {
            case: format!("an ungranted {verb} served by the ANY catch-all"),
            session: 0,
            verb,
            within: addressed.clone(),
            judged: item.clone(),
            expected: Expected::NotAuthorized,
        });
    }
    // The same reading where the method is put on the verb itself: the row the ANY ones are
    // judged against.
    rows.push(Row {
        case: "an ungranted GET of a method put on GET".to_string(),
        session: 0,
        verb: "GET",
        within: format!("/{LITERAL}"),
        judged: LITERAL.to_string(),
        expected: Expected::NotAuthorized,
    });

    // A grant on the GET of one item reaches that one request and no other verb or id of the
    // same deployed method.
    rows.push(Row {
        case: "the granted GET of the granted item".to_string(),
        session: 1,
        verb: "GET",
        within: addressed.clone(),
        judged: item.clone(),
        expected: Expected::Served,
    });
    rows.push(Row {
        case: "a POST of the item whose GET is granted".to_string(),
        session: 1,
        verb: "POST",
        within: addressed.clone(),
        judged: item.clone(),
        expected: Expected::NotAuthorized,
    });
    rows.push(Row {
        case: "a GET of the sibling item the grant does not name".to_string(),
        session: 1,
        verb: "GET",
        within: format!("/{COLLECTION}/{SIBLING_ITEM}"),
        judged: format!("{COLLECTION}/{SIBLING_ITEM}"),
        expected: Expected::NotAuthorized,
    });

    // A grant naming the name the method is CONFIGURED under covers no request at all.
    for verb in ["GET", "DELETE"] {
        rows.push(Row {
            case: format!("a {verb} under a grant naming the ANY the method is put on"),
            session: 2,
            verb,
            within: addressed.clone(),
            judged: item.clone(),
            expected: Expected::NotAuthorized,
        });
    }

    // A wildcard in the verb's place covers every verb a client can send.
    for verb in ["GET", "DELETE"] {
        rows.push(Row {
            case: format!("a {verb} under a grant wildcarding the verb"),
            session: 3,
            verb,
            within: addressed.clone(),
            judged: item.clone(),
            expected: Expected::Served,
        });
    }
    rows
}

/// RULE: the method ARN an `AWS_IAM` invoke is judged against — and the one its refusal
/// reports, with the clause that refusal closes on — names the verb the CLIENT SENT and the
/// path it addressed, never the `ANY` the serving method is configured under or the parameter
/// template its resource is declared by.
#[test]
fn an_invoke_is_judged_and_refused_about_the_verb_the_request_carried() {
    std::thread::Builder::new()
        .name("apigateway-invoke-denial-request-verb".to_string())
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
                let item = format!("{COLLECTION}/{ITEM}");
                // One session per document the rows are decided on, each carrying that
                // document and nothing else.
                let documents: [(&str, Option<String>); 4] = [
                    ("no-grant", None),
                    (
                        "get-one-item",
                        Some(policy_document(
                            "Allow",
                            INVOKE_ACTION,
                            &method_arn(&api_id, "GET", &item),
                        )),
                    ),
                    (
                        "any-verb-grant",
                        Some(policy_document(
                            "Allow",
                            INVOKE_ACTION,
                            &method_arn(&api_id, ANY_METHOD, &item),
                        )),
                    ),
                    (
                        "wildcard-verb-grant",
                        Some(policy_document(
                            "Allow",
                            INVOKE_ACTION,
                            &method_arn(&api_id, "*", &item),
                        )),
                    ),
                ];
                let mut sessions: Vec<(String, String)> = Vec::new();
                for (name, document) in documents {
                    let key_id = vended_session(&router, name, document).await;
                    let principal = caller_arn(&router, &key_id).await;
                    sessions.push((key_id, principal));
                }

                let invoke_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
                for row in rows() {
                    let case = &row.case;
                    let (key_id, principal) = &sessions[row.session];
                    let authorization = credential_scope(key_id, "execute-api");
                    let answer = dispatch(
                        &router,
                        &invoke_host,
                        row.verb,
                        &format!("/{STAGE}{}", row.within),
                        &[("authorization", authorization.as_str())],
                        Vec::new(),
                    )
                    .await;
                    match row.expected {
                        Expected::Served => {
                            assert_eq!(
                                answer.status,
                                200,
                                "case {case:?}: the session's grant covers the method ARN the \
                                 request named, so the integration owes it a 200, got {}",
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
                                "case {case:?}: no grant covers the method ARN the request \
                                 named, so a 403 is owed, got {}",
                                answer.summary()
                            );
                            let message = answer.message();
                            assert_eq!(
                                message,
                                implicit_deny_sentence(
                                    principal,
                                    &reported_resource(&api_id, row.verb, &row.judged)
                                ),
                                "case {case:?}: expected the refusal to name the method ARN of \
                                 the {} the request carried and to close on the \
                                 identity-based clause, got {message:?}",
                                row.verb
                            );
                            assert!(
                                !answer.text.contains(ITEM_TEMPLATE),
                                "case {case:?}: the resource's parameter template is a path no \
                                 client can address, so no refusal may quote it back, got {}",
                                answer.summary()
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
