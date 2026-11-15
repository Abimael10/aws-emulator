// SPDX-License-Identifier: CC0-1.0

//! The UNIQUE ID a resource-based policy holds an identity principal by, and what a read of
//! that policy reports for it.
//!
//! RULE pinned here: **a resource-based policy holds every principal that names an identity
//! of this account by that identity's unique id rather than by the ARN the document spells
//! it with — "AWS transforms the ARN to the underlying unique principal ID when you save the
//! policy ... However, if you delete the principal, the relationship is broken and the policy
//! displays the unique principal ID" (the IAM user guide's `Principal` element reference) —
//! so `GetResourcePolicy` reports such a principal as the ARN the identity carrying that id
//! has NOW, and as the bare unique id once the account holds no identity with it. Every other
//! principal value, and every other byte of the document, is reported exactly as it was
//! attached.**
//!
//! The rows differ along what the rule generalises over: WHICH identity the principal names
//! (an IAM user, an IAM role), the form the principal element is written in (one value, two
//! values in a list, several principal types at once, a statement's `NotPrincipal`), and WHAT
//! happens to the identity afterwards (deleted, renamed, left alone). A document naming an
//! identity the account has NEVER held is refused by the attach itself, which is the rule
//! `resource_policy_principal_admission` pins.
//!
//! Driven over the wire through one `MultiServiceRouter` serving DynamoDB beside IAM on ONE
//! shared engine — the identities a policy names live in IAM's own store, so the users and
//! roles here are made and deleted through IAM's own API, exactly as `core/router` assembles
//! the two services.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, ok, POLL_INTERVAL};

/// How long a row waits for the policy it attached to become readable, and nothing shorter.
///
/// "`PutResourcePolicy` is an asynchronous operation", so a read made straight after the
/// write may still report the revision the resource carried before it. The wait is on the
/// REVISION the write reported rather than on a span, and the deadline is far longer than
/// the lag so a loaded host cannot turn the wait into a failure.
const POLICY_READ_DEADLINE: Duration = Duration::from_secs(60);

/// The policy-language version every document here states, so no row depends on the version
/// a document stating none is read under (that rule is pinned by its own suite).
const POLICY_VERSION: &str = "2012-10-17";

/// The action every document here names: a table action, and NOT one of the policy-management
/// actions, so no document takes this caller's own ability to change the policy away and none
/// needs `ConfirmRemoveSelfResourceAccess`.
const ACTION: &str = "dynamodb:GetItem";

/// One identity of the account, as IAM reports it when it is made.
#[derive(Debug, Clone)]
struct Identity {
    /// The ARN the identity carried when the policy was attached.
    arn: String,
    /// `User$UserId` / `Role$RoleId`: the unique id a policy holds the identity by.
    unique_id: String,
}

/// Everything one row's document and expectation are written in terms of: the account, the
/// table the policy is attached to, and the identities the row made.
#[derive(Debug)]
struct Held {
    account: String,
    table_arn: String,
    users: BTreeMap<&'static str, Identity>,
    roles: BTreeMap<&'static str, Identity>,
}

impl Held {
    /// The user `name`, which the row declared.
    fn user(&self, name: &str) -> &Identity {
        self.users
            .get(name)
            .unwrap_or_else(|| panic!("the row must have made the user {name:?}"))
    }

    /// The role `name`, which the row declared.
    fn role(&self, name: &str) -> &Identity {
        self.roles
            .get(name)
            .unwrap_or_else(|| panic!("the row must have made the role {name:?}"))
    }

    /// The ARN of the user `name` of this account, whether or not IAM holds such a user —
    /// the spelling a policy names one by.
    fn user_arn_of(&self, name: &str) -> String {
        format!("arn:aws:iam::{}:user/{name}", self.account)
    }

    /// The account's own root, which is no identity and is never resolved.
    fn account_root(&self) -> String {
        format!("arn:aws:iam::{}:root", self.account)
    }
}

/// What a row does to the identities its policy names, once that policy is in force.
#[derive(Debug, Clone, Copy)]
enum Change {
    /// Nothing at all: the identities the document names are all still there.
    Nothing,
    /// `iam:DeleteUser` of the user the row made under that name.
    DeleteUser(&'static str),
    /// `iam:DeleteRole` of the role the row made under that name.
    DeleteRole(&'static str),
    /// `iam:UpdateUser` renaming the user from the first name to the second, which keeps the
    /// user's unique id and changes its ARN.
    RenameUser(&'static str, &'static str),
}

/// One row: the identities it makes, the document it attaches, what it then does to those
/// identities, and the document a read must report afterwards.
struct Row {
    case: &'static str,
    users: &'static [&'static str],
    roles: &'static [&'static str],
    document: fn(&Held) -> Value,
    change: Change,
    expected: fn(&Held) -> Value,
}

/// A one-statement document over [`ACTION`] on the row's own table, with `principal` written
/// under `element` (`Principal` or `NotPrincipal`) and the effect `effect`.
fn document(held: &Held, effect: &str, element: &str, principal: Value) -> Value {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": effect,
            element: principal,
            "Action": ACTION,
            "Resource": held.table_arn,
        }],
    })
}

/// An `Allow` of [`ACTION`] to `principal`.
fn allow(held: &Held, principal: Value) -> Value {
    document(held, "Allow", "Principal", principal)
}

/// The rows: one per way the rule is reached.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a user principal whose user is then deleted",
            users: &["rbp-deleted-user"],
            roles: &[],
            document: |held| allow(held, json!({"AWS": held.user("rbp-deleted-user").arn})),
            change: Change::DeleteUser("rbp-deleted-user"),
            expected: |held| {
                allow(
                    held,
                    json!({"AWS": held.user("rbp-deleted-user").unique_id}),
                )
            },
        },
        Row {
            case: "a role principal whose role is then deleted",
            users: &[],
            roles: &["rbp-deleted-role"],
            document: |held| allow(held, json!({"AWS": held.role("rbp-deleted-role").arn})),
            change: Change::DeleteRole("rbp-deleted-role"),
            expected: |held| {
                allow(
                    held,
                    json!({"AWS": held.role("rbp-deleted-role").unique_id}),
                )
            },
        },
        Row {
            case: "a user principal whose user is then renamed",
            users: &["rbp-renamed-user"],
            roles: &[],
            document: |held| allow(held, json!({"AWS": held.user("rbp-renamed-user").arn})),
            change: Change::RenameUser("rbp-renamed-user", "rbp-renamed-user-now"),
            expected: |held| {
                allow(
                    held,
                    json!({"AWS": held.user_arn_of("rbp-renamed-user-now")}),
                )
            },
        },
        Row {
            case: "a user principal whose user is left in place",
            users: &["rbp-kept-user"],
            roles: &[],
            document: |held| allow(held, json!({"AWS": held.user("rbp-kept-user").arn})),
            change: Change::Nothing,
            expected: |held| allow(held, json!({"AWS": held.user("rbp-kept-user").arn})),
        },
        Row {
            case: "two identities in one list, one of them deleted",
            users: &["rbp-listed-user"],
            roles: &["rbp-listed-role"],
            document: |held| {
                allow(
                    held,
                    json!({"AWS": [
                        held.user("rbp-listed-user").arn,
                        held.role("rbp-listed-role").arn,
                    ]}),
                )
            },
            change: Change::DeleteUser("rbp-listed-user"),
            expected: |held| {
                allow(
                    held,
                    json!({"AWS": [
                        held.user("rbp-listed-user").unique_id,
                        held.role("rbp-listed-role").arn,
                    ]}),
                )
            },
        },
        Row {
            case: "an account root and a service principal beside a deleted user",
            users: &["rbp-mixed-user"],
            roles: &[],
            document: |held| {
                allow(
                    held,
                    json!({
                        "AWS": [held.account_root(), held.user("rbp-mixed-user").arn],
                        "Service": "dynamodb.amazonaws.com",
                    }),
                )
            },
            change: Change::DeleteUser("rbp-mixed-user"),
            expected: |held| {
                allow(
                    held,
                    json!({
                        "AWS": [
                            held.account_root(),
                            held.user("rbp-mixed-user").unique_id,
                        ],
                        "Service": "dynamodb.amazonaws.com",
                    }),
                )
            },
        },
        Row {
            case: "the NotPrincipal of a Deny whose user is then deleted",
            users: &["rbp-excepted-user"],
            roles: &[],
            document: |held| {
                document(
                    held,
                    "Deny",
                    "NotPrincipal",
                    json!({"AWS": held.user("rbp-excepted-user").arn}),
                )
            },
            change: Change::DeleteUser("rbp-excepted-user"),
            expected: |held| {
                document(
                    held,
                    "Deny",
                    "NotPrincipal",
                    json!({"AWS": held.user("rbp-excepted-user").unique_id}),
                )
            },
        },
    ]
}

/// RULE: see the module docs — every row attaches a document, changes the identities it
/// names, and reads the policy back.
///
/// Every row attaches first and is read afterwards, so the one span an attach takes to become
/// readable is waited out once for all of them rather than once per row.
#[tokio::test]
async fn a_policy_reports_each_identity_principal_from_the_unique_id_it_holds() {
    let router = emulator_with_iam();
    let rows = rows();
    let mut attached: Vec<(&Row, Held, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let table = format!("rbp-principal-ids-{index}");
        let mut held = Held {
            account: engine::emulator_account_id().to_string(),
            table_arn: create_table(&router, row.case, &table).await,
            users: BTreeMap::new(),
            roles: BTreeMap::new(),
        };
        for name in row.users {
            let made = create_user(&router, row.case, name).await;
            held.users.insert(name, made);
        }
        for name in row.roles {
            let made = create_role(&router, row.case, name).await;
            held.roles.insert(name, made);
        }
        let attach = (row.document)(&held).to_string();
        let put = ok(
            &router,
            "PutResourcePolicy",
            json!({"ResourceArn": &held.table_arn, "Policy": attach}),
        )
        .await;
        let revision = put["RevisionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {}: the attach reports a revision id, got {put}", row.case))
            .to_string();
        attached.push((row, held, revision));
    }

    for (row, held, revision) in &attached {
        read_when_revised(&router, row.case, &held.table_arn, revision).await;
    }
    for (row, held, _) in &attached {
        apply(&router, row.case, held, row.change).await;
    }
    for (row, held, revision) in &attached {
        let read = ok(
            &router,
            "GetResourcePolicy",
            json!({"ResourceArn": &held.table_arn}),
        )
        .await;
        let reported = reported_document(row.case, &read);
        let expected = (row.expected)(held);
        assert_eq!(
            reported, expected,
            "case {:?}: expected the policy to read as {expected}, got {reported}",
            row.case
        );
        assert_eq!(
            read["RevisionId"],
            json!(revision),
            "case {:?}: a read reports the revision in force and revises nothing, got {read}",
            row.case
        );
    }
}

/// RULE: the same holding applies to the policy a CREATE attaches
/// (`CreateTable$ResourcePolicy`), whose application is strongly consistent — and the reported
/// document is a FIXED POINT: putting back the document a read reported stores exactly that
/// document, because a unique id names no ARN to resolve.
#[tokio::test]
async fn a_policy_attached_with_the_table_holds_its_principal_by_its_unique_id_too() {
    let case = "a policy attached while creating the table";
    let router = emulator_with_iam();
    let user = create_user(&router, case, "rbp-created-with-table").await;
    let table = "rbp-principal-ids-on-create";
    let mut held = Held {
        account: engine::emulator_account_id().to_string(),
        table_arn: format!(
            "arn:aws:dynamodb:{}:{}:table/{table}",
            crate::emulator::REGION,
            engine::emulator_account_id()
        ),
        users: BTreeMap::new(),
        roles: BTreeMap::new(),
    };
    held.users.insert("rbp-created-with-table", user.clone());
    let attach = allow(&held, json!({"AWS": user.arn})).to_string();
    let created = ok(
        &router,
        "CreateTable",
        json!({
            "TableName": table,
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "ResourcePolicy": attach,
        }),
    )
    .await;
    assert_eq!(
        created["TableDescription"]["TableArn"],
        json!(&held.table_arn),
        "case {case:?}: the created table must carry the ARN the policy names, got {created}"
    );

    // "When you attach a resource-based policy while creating a table, the policy application
    // is strongly consistent", so the create's own policy is readable at once.
    let read = ok(
        &router,
        "GetResourcePolicy",
        json!({"ResourceArn": &held.table_arn}),
    )
    .await;
    assert_eq!(
        reported_document(case, &read),
        allow(&held, json!({"AWS": user.arn})),
        "case {case:?}: the policy the create attached must read as it was attached, got {read}"
    );

    delete_user(&router, case, "rbp-created-with-table").await;
    let read = ok(
        &router,
        "GetResourcePolicy",
        json!({"ResourceArn": &held.table_arn}),
    )
    .await;
    let deleted_form = allow(&held, json!({"AWS": user.unique_id}));
    assert_eq!(
        reported_document(case, &read),
        deleted_form,
        "case {case:?}: expected the deleted principal's unique id, got {read}"
    );

    // The fixed point: the document the read reported, put back, is stored as it stands.
    let put = ok(
        &router,
        "PutResourcePolicy",
        json!({
            "ResourceArn": &held.table_arn,
            "Policy": deleted_form.to_string(),
        }),
    )
    .await;
    let revision = put["RevisionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the attach reports a revision id, got {put}"))
        .to_string();
    let read = read_when_revised(&router, case, &held.table_arn, &revision).await;
    assert_eq!(
        reported_document(case, &read),
        deleted_form,
        "case {case:?}: re-attaching the document a read reported must store it unchanged, got \
         {read}"
    );
}

/// RULE: the holding decides the DATA PLANE as well as the read — "this helps mitigate the risk
/// of someone escalating their privileges by removing and recreating the principal" (the IAM
/// user guide's `Principal` reference). A statement naming an identity grants the identity its
/// unique id belongs to: the same identity under a NEW name is still granted, and a DIFFERENT
/// identity remade under the ARN the document spells is not.
#[tokio::test]
async fn the_data_plane_grants_the_identity_the_held_unique_id_belongs_to() {
    let case = "a table policy granting one identity";
    let router = emulator_with_iam();
    let table = "rbp-principal-ids-data-plane";
    let mut held = Held {
        account: engine::emulator_account_id().to_string(),
        table_arn: create_table(&router, case, table).await,
        users: BTreeMap::new(),
        roles: BTreeMap::new(),
    };
    let granted = create_user(&router, case, "rbp-decider").await;
    held.users.insert("rbp-decider", granted.clone());
    let their_key = create_access_key(&router, case, "rbp-decider").await;
    // The grant is the TABLE's own policy and nothing else: the user carries no
    // identity-based document, so "the resource-based policy and the identity-based policy
    // are evaluated together, and access is granted if either allows it" is what admits it.
    let put = ok(
        &router,
        "PutResourcePolicy",
        json!({
            "ResourceArn": &held.table_arn,
            "Policy": allow(&held, json!({"AWS": granted.arn})).to_string(),
        }),
    )
    .await;
    let revision = put["RevisionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the attach reports a revision id, got {put}"))
        .to_string();
    read_when_revised(&router, case, &held.table_arn, &revision).await;
    crate::emulator::describe_when_active(&router, case, table).await;

    let (status, body) = get_item_as(&router, &their_key, table).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the identity the policy names must be granted the read, got {status} \
         with {body}"
    );

    // Renamed: a new ARN, the same unique id — so the same statement still grants it.
    let (status, body) = iam(
        &router,
        "UpdateUser",
        &[
            ("UserName", "rbp-decider"),
            ("NewUserName", "rbp-decider-now"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the user must be renamed, got {status} with {body}"
    );
    let (status, body) = get_item_as(&router, &their_key, table).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the renamed identity carries the unique id the policy holds, so the \
         grant stands, got {status} with {body}"
    );
    let read = ok(
        &router,
        "GetResourcePolicy",
        json!({"ResourceArn": &held.table_arn}),
    )
    .await;
    assert_eq!(
        reported_document(case, &read),
        allow(&held, json!({"AWS": held.user_arn_of("rbp-decider-now")})),
        "case {case:?}: the read must report the ARN the held id resolves to now, got {read}"
    );

    // A DIFFERENT identity, made under the ARN the document spells: another unique id, so
    // the statement does not name it and nothing else grants it.
    let other = create_user(&router, case, "rbp-decider").await;
    assert_eq!(
        other.arn, granted.arn,
        "case {case:?}: the remade user must carry the ARN the document names, got {other:?}"
    );
    assert_ne!(
        other.unique_id, granted.unique_id,
        "case {case:?}: a remade identity must carry a unique id of its own, got {other:?}"
    );
    let their_key = create_access_key(&router, case, "rbp-decider").await;
    let (status, body) = get_item_as(&router, &their_key, table).await;
    assert_eq!(
        status, 400,
        "case {case:?}: the remade identity is not the one the policy holds, so the read must \
         be refused, got {status} with {body}"
    );
    assert_eq!(
        crate::emulator::error_code(&body),
        "AccessDeniedException",
        "case {case:?}: expected the modelled authorization refusal, got {body}"
    );
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
/// read — the reader is eventually consistent, so the wait is for the CONDITION under a
/// generous deadline ([`POLICY_READ_DEADLINE`]) rather than for a span.
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

/// Apply one row's change to the identities it made.
async fn apply(router: &MultiServiceRouter, case: &str, held: &Held, change: Change) {
    match change {
        Change::Nothing => {}
        Change::DeleteUser(name) => delete_user(router, case, name).await,
        Change::DeleteRole(name) => {
            let (status, body) = iam(router, "DeleteRole", &[("RoleName", name)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the role {name:?} must be deleted, got {status} with {body}"
            );
        }
        Change::RenameUser(name, renamed) => {
            let (status, body) = iam(
                router,
                "UpdateUser",
                &[("UserName", name), ("NewUserName", renamed)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the user {name:?} must be renamed, got {status} with {body}"
            );
            assert_eq!(
                held.user(name).arn,
                held.user_arn_of(name),
                "case {case:?}: the renamed user's ARN must be the one IAM reported for the name \
                 it was made under"
            );
        }
    }
}

/// Make one IAM user and answer the ARN and unique id IAM reports for it.
async fn create_user(router: &MultiServiceRouter, case: &str, name: &str) -> Identity {
    let (status, body) = iam(router, "CreateUser", &[("UserName", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the user {name:?} must be created, got {status} with {body}"
    );
    Identity {
        arn: element(case, &body, "Arn"),
        unique_id: element(case, &body, "UserId"),
    }
}

/// `iam:DeleteUser` of one user, which is what breaks a policy's hold on it.
async fn delete_user(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = iam(router, "DeleteUser", &[("UserName", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the user {name:?} must be deleted, got {status} with {body}"
    );
}

/// `iam:CreateAccessKey` for one user, answering the access key id a request speaks for that
/// user with.
async fn create_access_key(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = iam(router, "CreateAccessKey", &[("UserName", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a key for {name:?} must be vended, got {status} with {body}"
    );
    element(case, &body, "AccessKeyId")
}

/// One `GetItem` on `table`, presented as signed by `akid` — the data-plane call the table's
/// own policy decides. The key addresses no item, so a 200 is the authorization answer and
/// not a stored item.
async fn get_item_as(router: &MultiServiceRouter, akid: &str, table: &str) -> (u16, Value) {
    let body = json!({"TableName": table, "Key": {"pk": {"S": "absent"}}});
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", "DynamoDB_20120810.GetItem")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={akid}/20260101/{}/dynamodb/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000",
                crate::emulator::REGION
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Make one IAM role and answer the ARN and unique id IAM reports for it. The trust policy is
/// the one member a create requires beside the name.
async fn create_role(router: &MultiServiceRouter, case: &str, name: &str) -> Identity {
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
    Identity {
        arn: element(case, &body, "Arn"),
        unique_id: element(case, &body, "RoleId"),
    }
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
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
