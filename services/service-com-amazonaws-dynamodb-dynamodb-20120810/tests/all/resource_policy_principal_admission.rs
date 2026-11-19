// SPDX-License-Identifier: CC0-1.0

//! WHICH principals a resource-based policy may name, decided before the document is stored.
//!
//! RULE pinned here: **a resource-based policy's principal is resolved when the policy is
//! SAVED — "AWS transforms the ARN to the underlying unique principal ID when you save the
//! policy" (the IAM user guide's `Principal` element reference) — so a document whose `AWS`
//! principal names an identity of this account that the account does not hold, or an IAM group,
//! which is no principal at all ("you cannot identify a user group as a `Principal` in a
//! policy (such as a resource-based policy) because groups relate to permissions, not
//! authentication", the same guide's *IAM identities*), is refused `ValidationException` and
//! nothing is stored; every other principal value — the wildcard, an account by its id or its
//! root, an identity the account holds, an assumed-role session, a service, federated or
//! canonical-user principal, and an entity of ANOTHER account — is admitted and reported back
//! as it was attached.**
//!
//! The rows differ along what the rule generalises over: the IAM entity type the value names
//! (user, role, group), whether the account holds it (never made, made and deleted, held), the
//! form the principal element is written in (one value, a list, several principal types at
//! once, a statement's `NotPrincipal`, the second statement of a document), which principal
//! TYPE the value sits under, and which write path carries the document (`PutResourcePolicy`
//! and `CreateTable$ResourcePolicy`, the two calls that attach one).
//!
//! Driven over the wire through one `MultiServiceRouter` serving DynamoDB beside IAM on ONE
//! shared engine, exactly as `core/router` assembles the two: the identities a policy names are
//! IAM's own records, so an admission has to be grounded in what IAM holds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, error_code, message, ok, POLL_INTERVAL, REGION};

/// How long an admitted row waits for the policy it attached to become readable, and nothing
/// shorter: "`PutResourcePolicy` is an asynchronous operation", so the wait is on the REVISION
/// the write reported rather than on a span, under a deadline far longer than the lag so a
/// loaded host cannot turn the wait into a failure.
const POLICY_READ_DEADLINE: Duration = Duration::from_secs(60);

/// The policy-language version every document here states, so no row depends on the version a
/// document stating none is read under (that rule is pinned by its own suite).
const POLICY_VERSION: &str = "2012-10-17";

/// The action every document here names: a table action, and none of the policy-management
/// actions, so no document takes this caller's own ability to change the policy away and none
/// needs `ConfirmRemoveSelfResourceAccess`.
const ACTION: &str = "dynamodb:GetItem";

/// Another account than the one this emulator serves, whose entities this account's IAM is not
/// the one to hold.
const OTHER_ACCOUNT: &str = "210987654321";

/// The identities and the account one row writes its document in terms of.
#[derive(Debug)]
struct Held {
    account: String,
    /// `arn:…:user/rbp-admitted-user`, which IAM holds.
    user: String,
    /// `arn:…:role/rbp-admitted-role`, which IAM holds.
    role: String,
    /// `arn:…:group/rbp-admitted-group`, which IAM holds and which is still no principal.
    group: String,
    /// The ARN of a user IAM held and then DELETED, so the account no longer holds it.
    deleted_user: String,
}

impl Held {
    /// The ARN of the user `name` of this account, whether or not IAM holds such a user — the
    /// spelling a policy names one by.
    fn user_arn_of(&self, name: &str) -> String {
        format!("arn:aws:iam::{}:user/{name}", self.account)
    }

    /// The ARN of the role `name` of this account, held or not.
    fn role_arn_of(&self, name: &str) -> String {
        format!("arn:aws:iam::{}:role/{name}", self.account)
    }

    /// The account's own root, which names the whole account and is no identity to resolve.
    fn account_root(&self) -> String {
        format!("arn:aws:iam::{}:root", self.account)
    }
}

/// One row: the document it attaches, and the principal value its refusal must name — `None`
/// for a document every principal of which the account can resolve.
struct Row {
    case: &'static str,
    document: fn(&Held) -> Value,
    refused: Option<fn(&Held) -> String>,
}

/// A one-statement document over [`ACTION`], with `principal` written under `element`
/// (`Principal` or `NotPrincipal`) and the effect `effect`.
fn document(table_arn: &str, effect: &str, element: &str, principal: Value) -> Value {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": effect,
            element: principal,
            "Action": ACTION,
            "Resource": table_arn,
        }],
    })
}

/// An `Allow` of [`ACTION`] to `principal`, over the table the row attaches to.
fn allow(principal: Value) -> Value {
    document(PLACEHOLDER_RESOURCE, "Allow", "Principal", principal)
}

/// A `Deny` of [`ACTION`] about `principal` under `element`.
fn deny(element: &str, principal: Value) -> Value {
    document(PLACEHOLDER_RESOURCE, "Deny", element, principal)
}

/// The `Resource` every row's document names: a wildcard rather than one table's ARN, so one
/// document is the same text whichever of this suite's tables it is attached to.
const PLACEHOLDER_RESOURCE: &str = "*";

/// The rows: one per way the rule is reached.
fn rows() -> Vec<Row> {
    vec![
        // ADMITTED: every value the account can resolve as a grantee.
        Row {
            case: "the wildcard in a Deny",
            document: |_| deny("Principal", json!("*")),
            refused: None,
        },
        Row {
            case: "an account by its bare id",
            document: |held| allow(json!({"AWS": held.account.clone()})),
            refused: None,
        },
        Row {
            case: "an account by its root",
            document: |held| allow(json!({"AWS": held.account_root()})),
            refused: None,
        },
        Row {
            case: "an IAM user this account holds",
            document: |held| allow(json!({"AWS": held.user.clone()})),
            refused: None,
        },
        Row {
            case: "an IAM role this account holds",
            document: |held| allow(json!({"AWS": held.role.clone()})),
            refused: None,
        },
        Row {
            case: "a held identity listed beside the account root",
            document: |held| allow(json!({"AWS": [held.account_root(), held.user.clone()]})),
            refused: None,
        },
        Row {
            case: "a user of ANOTHER account, which this IAM does not hold",
            document: |_| {
                allow(json!({"AWS": format!("arn:aws:iam::{OTHER_ACCOUNT}:user/elsewhere")}))
            },
            refused: None,
        },
        Row {
            case: "an assumed-role session of this account",
            document: |held| {
                allow(json!({"AWS": format!(
                    "arn:aws:sts::{}:assumed-role/rbp-admitted-role/probe",
                    held.account
                )}))
            },
            refused: None,
        },
        Row {
            case: "a service and a federated principal, under their own types",
            document: |_| {
                allow(json!({
                    "Service": "dynamodb.amazonaws.com",
                    "Federated": "accounts.google.com",
                }))
            },
            refused: None,
        },
        Row {
            case: "a held user under a Deny's NotPrincipal",
            document: |held| deny("NotPrincipal", json!({"AWS": held.user.clone()})),
            refused: None,
        },
        // REFUSED: a value naming an identity of this account the account does not hold, and a
        // group, which is no principal whatever the account holds under that name.
        Row {
            case: "a user of this account IAM never held",
            document: |held| allow(json!({"AWS": held.user_arn_of("rbp-never-made")})),
            refused: Some(|held| held.user_arn_of("rbp-never-made")),
        },
        Row {
            case: "a role of this account IAM never held",
            document: |held| allow(json!({"AWS": held.role_arn_of("rbp-never-made-role")})),
            refused: Some(|held| held.role_arn_of("rbp-never-made-role")),
        },
        Row {
            case: "a user under a path IAM never held",
            document: |held| allow(json!({"AWS": held.user_arn_of("team/rbp-pathed")})),
            refused: Some(|held| held.user_arn_of("team/rbp-pathed")),
        },
        Row {
            case: "a user IAM held and then deleted",
            document: |held| allow(json!({"AWS": held.deleted_user.clone()})),
            refused: Some(|held| held.deleted_user.clone()),
        },
        Row {
            case: "an IAM group this account DOES hold",
            document: |held| allow(json!({"AWS": held.group.clone()})),
            refused: Some(|held| held.group.clone()),
        },
        Row {
            case: "one unheld value listed after two that resolve",
            document: |held| {
                allow(json!({"AWS": [
                    held.account_root(),
                    held.user.clone(),
                    held.user_arn_of("rbp-listed-absent"),
                ]}))
            },
            refused: Some(|held| held.user_arn_of("rbp-listed-absent")),
        },
        Row {
            case: "an unheld value under a Deny's NotPrincipal",
            document: |held| {
                deny(
                    "NotPrincipal",
                    json!({"AWS": held.role_arn_of("rbp-excepted-absent")}),
                )
            },
            refused: Some(|held| held.role_arn_of("rbp-excepted-absent")),
        },
        Row {
            case: "an unheld value in the SECOND statement of a document",
            document: |held| {
                json!({
                    "Version": POLICY_VERSION,
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": {"AWS": held.account_root()},
                            "Action": ACTION,
                            "Resource": PLACEHOLDER_RESOURCE,
                        },
                        {
                            "Effect": "Allow",
                            "Principal": {"AWS": held.user_arn_of("rbp-second-absent")},
                            "Action": ACTION,
                            "Resource": PLACEHOLDER_RESOURCE,
                        },
                    ],
                })
            },
            refused: Some(|held| held.user_arn_of("rbp-second-absent")),
        },
    ]
}

/// The refusal AWS words for a principal that is no identity of this account: the
/// parameter-value clause over the sentence that a principal of the document could not be
/// resolved. It is FIXED text in its entirety — AWS names neither the offending value nor the
/// type it was named under, so every refused row below expects the same sentence.
const INVALID_PRINCIPAL: &str =
    "One or more parameter values were invalid: Invalid principal in policy document.";

/// RULE: see the module docs — `PutResourcePolicy` admits a document only when every principal
/// it names is one the account can resolve, and the refusal names the first value that is not.
#[tokio::test]
async fn put_resource_policy_admits_only_principals_the_account_can_resolve() {
    let router = emulator_with_iam();
    let held = given_identities(&router, "put").await;
    let rows = rows();

    // Every admitted row attaches to its OWN table, so the one span an attach takes to become
    // readable is waited out once for all of them rather than once per row.
    let mut admitted: Vec<(&Row, String, String, Value)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let table = format!("rbp-admission-{index}");
        let table_arn = create_table(&router, row.case, &table).await;
        let attach = (row.document)(&held);
        let (status, body) = call(
            &router,
            "PutResourcePolicy",
            json!({"ResourceArn": &table_arn, "Policy": attach.to_string()}),
        )
        .await;
        match row.refused {
            Some(named) => {
                let named = named(&held);
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, "ValidationException"),
                    "case {:?}: expected 400 ValidationException for {named}, got {status} \
                     with {body}",
                    row.case
                );
                assert_eq!(
                    message(&body),
                    INVALID_PRINCIPAL,
                    "case {:?}: expected AWS's invalid-principal sentence for {named}, got {:?}",
                    row.case,
                    message(&body)
                );
                // Nothing was stored: the resource carries no policy at all, which is the
                // refusal the reader words for a resource without one.
                let (status, read) = call(
                    &router,
                    "GetResourcePolicy",
                    json!({"ResourceArn": &table_arn}),
                )
                .await;
                assert_eq!(
                    (status, error_code(&read).as_str()),
                    (400, "PolicyNotFoundException"),
                    "case {:?}: a refused attach must store nothing, got {status} with {read}",
                    row.case
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: every principal resolves, so the attach must be admitted, got \
                     {status} with {body}",
                    row.case
                );
                let revision = body["RevisionId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {:?}: no RevisionId in {body}", row.case))
                    .to_string();
                admitted.push((row, table_arn, revision, attach));
            }
        }
    }

    for (row, table_arn, revision, attach) in admitted {
        let read = read_when_revised(&router, row.case, &table_arn, &revision).await;
        assert_eq!(
            reported_document(row.case, &read),
            attach,
            "case {:?}: the admitted document must be reported as it was attached, got {read}",
            row.case
        );
    }
}

/// RULE: the same admission decides `CreateTable$ResourcePolicy`, the other call that attaches a
/// document — and a refused create leaves no table behind.
#[tokio::test]
async fn create_table_admits_the_same_principals_as_the_attach() {
    let router = emulator_with_iam();
    let held = given_identities(&router, "create").await;

    for (index, row) in rows().iter().enumerate() {
        let table = format!("rbp-created-{index}");
        let document = (row.document)(&held).to_string();
        let (status, body) = call(
            &router,
            "CreateTable",
            json!({
                "TableName": &table,
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "BillingMode": "PAY_PER_REQUEST",
                "ResourcePolicy": document,
            }),
        )
        .await;
        let Some(named) = row.refused else {
            assert_eq!(
                status, 200,
                "case {:?}: the create carries a document every principal of which resolves, \
                 got {status} with {body}",
                row.case
            );
            continue;
        };
        let named = named(&held);
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {:?}: expected 400 ValidationException for {named}, got {status} with {body}",
            row.case
        );
        assert_eq!(
            message(&body),
            INVALID_PRINCIPAL,
            "case {:?}: expected AWS's invalid-principal sentence for {named}, got {:?}",
            row.case,
            message(&body)
        );
        // The create was refused as a whole, so the table it would have made is not there.
        let (status, described) = call(&router, "DescribeTable", json!({"TableName": &table})).await;
        assert_eq!(
            (status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "case {:?}: a refused create must leave no table, got {status} with {described}",
            row.case
        );
    }
}

/// Make the identities every row is written in terms of: a user, a role and a group the account
/// holds, and one user it held and then deleted.
async fn given_identities(router: &MultiServiceRouter, case: &str) -> Held {
    let account = engine::EMULATOR_ACCOUNT_ID.to_string();
    let user = create_user(router, case, "rbp-admitted-user").await;
    let role = create_role(router, case, "rbp-admitted-role").await;
    let group = create_group(router, case, "rbp-admitted-group").await;
    let deleted_user = create_user(router, case, "rbp-deleted-principal").await;
    let (status, body) = iam(router, "DeleteUser", &[("UserName", "rbp-deleted-principal")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the user to delete must be deleted, got {status} with {body}"
    );
    Held {
        account,
        user,
        role,
        group,
        deleted_user,
    }
}

/// One emulator serving DynamoDB beside IAM over ONE shared engine: the identities a policy
/// names are IAM's records, so the policy store resolves them where IAM holds them.
fn emulator_with_iam() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suites drive unsigned requests, exactly as the shared harness does.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Create one table and answer the ARN its own description reports for it.
async fn create_table(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    created["TableDescription"]["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create reports the table's ARN, got {created}"))
        .to_string()
}

/// The `Policy` of a `GetResourcePolicy` answer, parsed — the document is compared as JSON, so
/// nothing here depends on how the members of one object are ordered.
fn reported_document(case: &str, read: &Value) -> Value {
    let document = read["Policy"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the read reports a policy document, got {read}"));
    serde_json::from_str(document).unwrap_or_else(|error| {
        panic!("case {case:?}: the reported document must be JSON, got {error} for {document}")
    })
}

/// Poll `GetResourcePolicy` until the revision `revision` is the one in force, and answer that
/// read — the reader is eventually consistent, so the wait is for the CONDITION under a generous
/// deadline ([`POLICY_READ_DEADLINE`]) rather than for a span.
async fn read_when_revised(
    router: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
    revision: &str,
) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < POLICY_READ_DEADLINE {
        let (status, answered) = call(
            router,
            "GetResourcePolicy",
            json!({"ResourceArn": resource_arn}),
        )
        .await;
        if status == 200 && answered["RevisionId"] == json!(revision) {
            return answered;
        }
        last = answered;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the revision {revision} never came into force within \
         {POLICY_READ_DEADLINE:?}, last {last}"
    )
}

/// Make one IAM user and answer the ARN IAM reports for it.
async fn create_user(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = iam(router, "CreateUser", &[("UserName", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the user {name:?} must be created, got {status} with {body}"
    );
    element(case, &body, "Arn")
}

/// Make one IAM role and answer the ARN IAM reports for it. The trust policy is the one member a
/// create requires beside the name.
async fn create_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let trust = json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "dynamodb.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role {name:?} must be created, got {status} with {body}"
    );
    element(case, &body, "Arn")
}

/// Make one IAM group and answer the ARN IAM reports for it.
async fn create_group(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = iam(router, "CreateGroup", &[("GroupName", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group {name:?} must be created, got {status} with {body}"
    );
    element(case, &body, "Arn")
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed IAM request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Percent-encode one `application/x-www-form-urlencoded` component, through the maintained
/// encoder rather than by hand.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<tag>` element of an XML body.
fn element(case: &str, body: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let (_, rest) = body
        .split_once(&open)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry <{tag}>, got {body}"));
    let (text, _) = rest
        .split_once(&close)
        .unwrap_or_else(|| panic!("case {case:?}: <{tag}> must be closed, got {body}"));
    text.to_string()
}
