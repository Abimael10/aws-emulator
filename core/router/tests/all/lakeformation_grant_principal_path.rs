// SPDX-License-Identifier: CC0-1.0

//! Lake Formation resolves a grantee ARN against this account's IAM under BOTH halves IAM
//! holds an identity by — its name and its path — through the WHOLE assembled router.
//!
//! RULE: IAM holds an entity under a name AND a path and reports both, and an IAM entity ARN
//! carries both (`arn:aws:iam::<account>:role<path><name>`), so a
//! `DataLakePrincipalIdentifier` whose path portion is not the path IAM reports for that name
//! names no identity of this account: `GrantPermissions` refuses it with the operation's
//! modelled `InvalidInputException` (HTTP 400) and records nothing, and the refusal says the
//! path is what does not resolve. The SAME name under the path IAM holds it under is served.
//! A name IAM holds nothing under at all is the neighbouring refusal — the identity is absent
//! rather than held somewhere else — and a value IAM is not asked about (another account's
//! entity) is refused by nothing, because a refusal has to be grounded in what IAM ANSWERED.
//!
//! The rows span what the rule generalises over: the IAM entity type the identifier names
//! (role, user), how many segments the entity's path has, how many the identifier names, and
//! which of the two differs from the other — including the identifier naming the account root
//! for an entity under a path, which is the form the recorded fixture grants with.
//!
//! The principal is resolved through the in-process callback, so a single-service router has
//! no IAM to ask: that is why this suite lives here rather than in the service's own crate.
//! The identities are made with IAM's OWN wire API (`awsQuery`) and the grants with Lake
//! Formation's (`restJson1`), so a response can only be what the handlers computed from
//! stored state.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder the awsQuery calls to IAM need is the one
// `all/cloudfront_key_value_store_handshake.rs` already owns.
use crate::cloudfront_key_value_store_handshake::segment as form_encode;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// A syntactically valid account id that is NOT the account this emulator serves, whose IAM
/// this emulator is not the one to ask.
const OTHER_ACCOUNT: &str = "123456789012";

/// The permission every row grants, which an LF-tag resource carries.
const ASSOCIATE: &str = "ASSOCIATE";

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The
/// signature is never checked: this router runs with authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// Send one buffered request and answer its status, the `x-amzn-errortype` header it carries
/// (`None` when it carries none) and its body.
fn send(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    path: &str,
    content_type: &str,
    body: String,
) -> (u16, Option<String>, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://{signing_name}.{REGION}.amazonaws.com{path}"
        ))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let named_shape = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (
        status,
        named_shape,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Drive one IAM operation (`awsQuery`: the action and its parameters go in the form body).
fn iam(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, _shape, text) = send(
        router,
        runtime,
        "iam",
        "/",
        "application/x-www-form-urlencoded",
        body,
    );
    (status, text)
}

/// One Lake Formation answer: its status, the error SHAPE it named (`""` on a success, which
/// names none) and its parsed body.
struct Answered {
    status: u16,
    shape: String,
    body: Value,
}

/// Drive one Lake Formation operation (`restJson1`: the operation names the path).
///
/// `restJson1` resolves a failure's error shape from the `x-amzn-errortype` header, and the
/// BODY of a failure carries the error shape's own modelled members alone, so the shape is
/// read from the header here.
fn lakeformation(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: &Value,
) -> Answered {
    let (status, named_shape, text) = send(
        router,
        runtime,
        "lakeformation",
        &format!("/{operation}"),
        "application/json",
        body.to_string(),
    );
    Answered {
        status,
        shape: named_shape
            .unwrap_or_default()
            .rsplit(['#', ':'])
            .next()
            .unwrap_or_default()
            .to_string(),
        body: if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        },
    }
}

/// The error message a refusal carries, whichever casing the envelope names the member in.
fn error_message(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// The ARN of an IAM entity of `account`: `arn:aws:iam::<account>:<resource>`.
fn iam_arn(account: &str, resource: &str) -> String {
    format!("arn:aws:iam::{account}:{resource}")
}

/// A trust policy any service may assume, so the fixture roles are ordinary roles: what this
/// suite asks IAM is where the entity is HELD, not what may assume it.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lakeformation.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }]
    })
    .to_string()
}

/// The identities the rows resolve against: a role and a user at the account root, a role and
/// a user under a one-segment path, and a role under a two-segment path.
fn create_identities(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) {
    let document = trust_policy();
    for (role_name, path) in [
        ("root-analyst", None),
        ("team-analyst", Some("/team/")),
        ("nested-analyst", Some("/a/b/")),
    ] {
        let mut parameters = vec![
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", document.as_str()),
        ];
        if let Some(path) = path {
            parameters.push(("Path", path));
        }
        let (status, body) = iam(router, runtime, "CreateRole", &parameters);
        assert_eq!(
            status, 200,
            "setup: CreateRole {role_name} must succeed, got {status} {body}"
        );
    }
    for (user_name, path) in [("root-auditor", None), ("team-auditor", Some("/audit/"))] {
        let mut parameters = vec![("UserName", user_name)];
        if let Some(path) = path {
            parameters.push(("Path", path));
        }
        let (status, body) = iam(router, runtime, "CreateUser", &parameters);
        assert_eq!(
            status, 200,
            "setup: CreateUser {user_name} must succeed, got {status} {body}"
        );
    }
}

/// What a grant to one identifier must answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The identifier names the identity IAM holds, so the grant is recorded.
    Recorded,
    /// IAM holds that name under another path, so the identifier names the path rather than
    /// the identity.
    RefusedPath,
    /// IAM holds nothing under that name at all.
    RefusedAbsent,
}

/// One row: the `DataLakePrincipalIdentifier` a grant names and what it must answer.
struct PathCase {
    case: &'static str,
    principal: String,
    outcome: Outcome,
}

fn path_cases(account: &str) -> Vec<PathCase> {
    let row = |case: &'static str, principal: String, outcome: Outcome| PathCase {
        case,
        principal,
        outcome,
    };
    vec![
        row(
            "a role held at the root, named at the root",
            iam_arn(account, "role/root-analyst"),
            Outcome::Recorded,
        ),
        row(
            "a role held at the root, named under a path",
            iam_arn(account, "role/team/root-analyst"),
            Outcome::RefusedPath,
        ),
        row(
            "a role held under a path, named under that path",
            iam_arn(account, "role/team/team-analyst"),
            Outcome::Recorded,
        ),
        row(
            // The form the recorded fixture grants with.
            "a role held under a path, named at the root",
            iam_arn(account, "role/team-analyst"),
            Outcome::RefusedPath,
        ),
        row(
            "a role held under a path, named under another path",
            iam_arn(account, "role/other/team-analyst"),
            Outcome::RefusedPath,
        ),
        row(
            "a role held under a nested path, named under that nested path",
            iam_arn(account, "role/a/b/nested-analyst"),
            Outcome::Recorded,
        ),
        row(
            "a role held under a nested path, named under a prefix of it",
            iam_arn(account, "role/a/nested-analyst"),
            Outcome::RefusedPath,
        ),
        row(
            "a user held at the root, named at the root",
            iam_arn(account, "user/root-auditor"),
            Outcome::Recorded,
        ),
        row(
            "a user held at the root, named under a path",
            iam_arn(account, "user/audit/root-auditor"),
            Outcome::RefusedPath,
        ),
        row(
            "a user held under a path, named under that path",
            iam_arn(account, "user/audit/team-auditor"),
            Outcome::Recorded,
        ),
        row(
            "a user held under a path, named at the root",
            iam_arn(account, "user/team-auditor"),
            Outcome::RefusedPath,
        ),
        row(
            // The neighbouring refusal: nothing is held under that name, at any path.
            "a role nobody created, named at the root",
            iam_arn(account, "role/ghost-analyst"),
            Outcome::RefusedAbsent,
        ),
        row(
            "a role nobody created, named under a path",
            iam_arn(account, "role/team/ghost-analyst"),
            Outcome::RefusedAbsent,
        ),
        row(
            // Another account's IAM is not this emulator's to ask, so nothing is known about
            // the name OR the path.
            "a role of another account under a path",
            iam_arn(OTHER_ACCOUNT, "role/team/team-analyst"),
            Outcome::Recorded,
        ),
    ]
}

/// The LF-tag resource row `index` grants on: its own record, so no two rows share one.
fn lf_tag_resource(index: usize) -> Value {
    json!({ "LFTag": { "TagKey": format!("path-row-{index}"), "TagValues": ["prod"] } })
}

/// The LF-tag keys the grants `principal` holds are recorded against.
fn held_tag_keys(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    principal: &str,
) -> Vec<String> {
    let answered = lakeformation(
        router,
        runtime,
        "ListPermissions",
        &json!({ "Principal": { "DataLakePrincipalIdentifier": principal } }),
    );
    assert_eq!(
        answered.status, 200,
        "ListPermissions for {principal} must answer 200, got {} {}",
        answered.status, answered.body
    );
    let mut keys: Vec<String> = answered.body["PrincipalResourcePermissions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "PrincipalResourcePermissions must be a list, got {}",
                answered.body
            )
        })
        .iter()
        .filter_map(|entry| Some(entry["Resource"]["LFTag"]["TagKey"].as_str()?.to_string()))
        .collect();
    keys.sort();
    keys
}

/// The sentence this service refuses an identifier whose path IAM does not hold the entity
/// under with: fixed text, and the one variable part is the identifier the request carried.
fn invalid_path_message(identifier: &str) -> String {
    format!("Invalid path for user, arn: {identifier}")
}

/// The sentence for a name IAM holds nothing under, the neighbouring refusal.
fn invalid_principal_message(identifier: &str) -> String {
    format!("Invalid principal, arn: {identifier}")
}

/// The message a row's outcome must carry, and the empty string for a served grant.
fn expected_message(outcome: Outcome, principal: &str) -> String {
    match outcome {
        Outcome::Recorded => String::new(),
        Outcome::RefusedPath => invalid_path_message(principal),
        Outcome::RefusedAbsent => invalid_principal_message(principal),
    }
}

#[test]
fn a_grant_resolves_the_path_iam_holds_the_grantee_under() {
    std::thread::Builder::new()
        .name("lakeformation-grant-path".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            create_identities(&router, &runtime);

            let account = engine::emulator_account_id().to_string();
            for (index, row) in path_cases(&account).into_iter().enumerate() {
                let expected_key = format!("path-row-{index}");
                let answered = lakeformation(
                    &router,
                    &runtime,
                    "GrantPermissions",
                    &json!({
                        "Principal": { "DataLakePrincipalIdentifier": &row.principal },
                        "Resource": lf_tag_resource(index),
                        "Permissions": [ASSOCIATE],
                    }),
                );

                if row.outcome == Outcome::Recorded {
                    assert_eq!(
                        answered.status, 200,
                        "case {:?} ({}): the grant must be served, got {} {}",
                        row.case, row.principal, answered.status, answered.body
                    );
                } else {
                    assert_eq!(
                        (answered.status, answered.shape.as_str()),
                        (400, "InvalidInputException"),
                        "case {:?} ({}): expected a 400 InvalidInputException, got {} {:?} {}",
                        row.case,
                        row.principal,
                        answered.status,
                        answered.shape,
                        answered.body
                    );
                    let expected = expected_message(row.outcome, &row.principal);
                    let actual = error_message(&answered.body);
                    assert_eq!(
                        actual, expected,
                        "case {:?}: expected the refusal {expected:?}, got {actual:?}",
                        row.case
                    );
                }

                // What the principal holds afterwards: the row's own LF-tag when the grant
                // was served, and nothing at all when it was refused.
                let held = held_tag_keys(&router, &runtime, &row.principal);
                let expected: Vec<String> = if row.outcome == Outcome::Recorded {
                    vec![expected_key]
                } else {
                    Vec::new()
                };
                assert_eq!(
                    held, expected,
                    "case {:?} ({}): expected the recorded grants {expected:?}, got {held:?}",
                    row.case, row.principal
                );
            }
        })
        .expect("spawn the grant-path thread")
        .join()
        .expect("every Lake Formation grantee must resolve to the path IAM holds it under");
}

#[test]
fn a_batch_entry_naming_another_path_fails_alone() {
    // The batch grants the same thing one entry at a time, so the same rule decides each
    // entry: the entry whose identifier names a path IAM does not hold the role under is
    // reported in `Failures` with the same sentence, and the entry that names the path IAM
    // does hold it under is applied.
    std::thread::Builder::new()
        .name("lakeformation-batch-grant-path".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            create_identities(&router, &runtime);

            let account = engine::emulator_account_id().to_string();
            let held = iam_arn(&account, "role/team/team-analyst");
            let at_another_path = iam_arn(&account, "role/team-analyst");
            let entry = |principal: &str, key: &str| {
                json!({
                    // `BatchPermissionsRequestEntry$Id` is the one member the model requires.
                    "Id": key,
                    "Principal": { "DataLakePrincipalIdentifier": principal },
                    "Resource": { "LFTag": { "TagKey": key, "TagValues": ["prod"] } },
                    "Permissions": [ASSOCIATE],
                })
            };
            let answered = lakeformation(
                &router,
                &runtime,
                "BatchGrantPermissions",
                &json!({
                    "Entries": [
                        entry(&held, "batch-path-held"),
                        entry(&at_another_path, "batch-path-elsewhere"),
                    ],
                }),
            );
            assert_eq!(
                answered.status, 200,
                "a partial-failure batch is served, got {} {}",
                answered.status, answered.body
            );
            let failures = answered.body["Failures"]
                .as_array()
                .unwrap_or_else(|| panic!("Failures must be a list, got {}", answered.body))
                .clone();
            assert_eq!(
                failures.len(),
                1,
                "exactly the entry naming another path fails, got {}",
                answered.body
            );
            let reported = failures[0]["Error"]["ErrorMessage"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                reported,
                invalid_path_message(&at_another_path),
                "the failed entry must carry the path refusal, got {}",
                answered.body
            );

            // The entry that resolved was applied, and the one that did not recorded nothing.
            assert_eq!(
                held_tag_keys(&router, &runtime, &held),
                vec!["batch-path-held".to_string()],
                "the entry naming the path IAM holds the role under must be applied"
            );
            assert_eq!(
                held_tag_keys(&router, &runtime, &at_another_path),
                Vec::<String>::new(),
                "the entry naming another path must record nothing"
            );
        })
        .expect("spawn the batch-grant-path thread")
        .join()
        .expect("a batch entry naming another path must fail alone");
}
