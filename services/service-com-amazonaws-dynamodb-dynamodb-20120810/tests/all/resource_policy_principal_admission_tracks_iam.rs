// SPDX-License-Identifier: CC0-1.0

//! WHEN a resource-based policy's principals are resolved, and the sentence a document naming
//! one the account cannot resolve is refused with.
//!
//! RULE pinned here: **a resource-based policy's principals are resolved against the identities
//! the account holds AT THE INSTANT OF THE WRITE — "AWS transforms the ARN to the underlying
//! unique principal ID when you save the policy" (the IAM user guide's `Principal` element
//! reference) — so one and the same document is refused HTTP 400 `ValidationException` with
//! AWS's invalid-principal sentence while a principal of it names an identity this account does
//! not hold, with nothing attached, and is admitted and readable once the account holds that
//! identity. That sentence is FIXED text in its entirety: it states the CLASS of the defect and
//! takes nothing from the document the caller sent — neither the offending value, nor the name
//! or type it was written under, nor the clause the other document-defect classes are stated
//! behind — and a resource carrying no policy is reported to the reader as
//! `PolicyNotFoundException` naming the `ResourceArn` of that request.**
//!
//! The rows differ along what the rule generalises over: the IAM entity type the unresolvable
//! value names (user, role), the path it is made under (the default path, a path of its own),
//! the form the principal element is written in (one value, a value listed after one that
//! resolves, the second statement of a document, a `Deny`'s `NotPrincipal`), and the write path
//! that carries the document (`PutResourcePolicy` and `CreateTable$ResourcePolicy`, the two
//! calls that attach one).
//!
//! The admission over a FIXED set of identities — which values resolve and which do not — is
//! `resource_policy_principal_admission`'s rule, and what a read reports for a principal held by
//! a unique id is `resource_policy_principal_unique_ids`'; this suite is about the same document
//! crossing from refused to admitted as IAM changes under it, which is the sequence a client's
//! retry loop runs while a just-created principal is still unresolvable. The harness, the
//! refusal sentence and the poll are the admission suite's, so the two cannot word one rule two
//! ways.
//!
//! Driven over the wire through one `MultiServiceRouter` serving DynamoDB beside IAM on ONE
//! shared engine: the identities a policy names are IAM's own records, so both the refusal and
//! the admission are grounded in what IAM holds when the write arrives.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, error_code, message};
use crate::resource_policy_principal_admission::{
    create_table, document, element, emulator_with_iam, iam, read_when_revised, reported_document,
    ACTION, INVALID_PRINCIPAL, PLACEHOLDER_RESOURCE, POLICY_VERSION,
};

/// The fixed opening of the refusal the reader words for a resource that carries no policy at
/// all; the request's own `ResourceArn` follows it. Written out here rather than shared with the
/// emulator or with the suite that pins this sentence on its own, so this suite asserts AWS's
/// wording and not another copy of it.
const NOT_FOUND_PREFIX: &str = "Resource-based policy not found for the provided ResourceArn: ";

/// The clause the OTHER refusal classes of a policy document are stated behind (a document that
/// is not JSON, an action of another vendor — see `resource_policy_document_refusal_wording`).
/// An unresolvable principal is a class of its own, so its sentence must not be stated behind
/// this clause, or a caller could not tell the two apart.
const DOCUMENT_CLAUSE: &str = "Invalid policy document";

/// The IAM entity a row's principal names: what it is, and how IAM makes it once the row needs
/// it to exist.
#[derive(Debug, Clone, Copy)]
enum Entity {
    /// An IAM user of this account, made under `path` (`/` is the default path).
    User {
        name: &'static str,
        path: &'static str,
    },
    /// An IAM role of this account.
    Role { name: &'static str },
}

impl Entity {
    /// The ARN a policy document names this entity by, whether or not IAM holds it yet: the
    /// spelling IAM reports for an entity of this type, name and path.
    fn arn(&self, account: &str) -> String {
        match self {
            Self::User { name, path } => format!("arn:aws:iam::{account}:user{path}{name}"),
            Self::Role { name } => format!("arn:aws:iam::{account}:role/{name}"),
        }
    }

    /// The name IAM holds this entity under, which no refusal may carry.
    fn name(&self) -> &'static str {
        match self {
            Self::User { name, .. } | Self::Role { name } => name,
        }
    }

    /// Make this entity through IAM's own API and answer the ARN IAM reports for it.
    async fn make(&self, router: &MultiServiceRouter, case: &str) -> String {
        let (status, body) = match self {
            Self::User { name, path } => {
                iam(router, "CreateUser", &[("UserName", name), ("Path", path)]).await
            }
            Self::Role { name } => {
                let trust = json!({
                    "Version": POLICY_VERSION,
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {"Service": "dynamodb.amazonaws.com"},
                        "Action": "sts:AssumeRole",
                    }],
                })
                .to_string();
                iam(
                    router,
                    "CreateRole",
                    &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
                )
                .await
            }
        };
        assert_eq!(
            status, 200,
            "case {case:?}: IAM must make {self:?}, got {status} with {body}"
        );
        element(case, &body, "Arn")
    }
}

/// The values one row's document is written in terms of.
#[derive(Debug)]
struct Named {
    /// The ARN of the identity the row names before IAM holds it.
    principal: String,
    /// The account's own root: a principal that is no identity, so it always resolves and a
    /// document naming it beside the row's value is refused for the row's value alone.
    account_root: String,
}

/// One row: the entity its document names, and the document it attaches.
struct Row {
    case: &'static str,
    entity: Entity,
    document: fn(&Named) -> Value,
}

/// The `Resource`, the version, the action and the one-statement shape every row's document is
/// written in are the admission suite's, so the two suites cannot drift on the document they
/// attach: [`statement`] is its builder over [`PLACEHOLDER_RESOURCE`], a wildcard rather than one
/// table's ARN, so one document is the same text whichever of this suite's tables it goes to.
fn statement(effect: &str, written_under: &str, principal: Value) -> Value {
    document(PLACEHOLDER_RESOURCE, effect, written_under, principal)
}

/// The rows: one per way a document reaches the rule.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a user named as the one Principal",
            entity: Entity::User {
                name: "rbp-tracked-user",
                path: "/",
            },
            document: |named| statement("Allow", "Principal", json!({"AWS": &named.principal})),
        },
        Row {
            case: "a role named as the one Principal",
            entity: Entity::Role {
                name: "rbp-tracked-role",
            },
            document: |named| statement("Allow", "Principal", json!({"AWS": &named.principal})),
        },
        Row {
            case: "a user under a path of its own",
            entity: Entity::User {
                name: "rbp-tracked-pathed",
                path: "/team/",
            },
            document: |named| statement("Allow", "Principal", json!({"AWS": &named.principal})),
        },
        Row {
            case: "a user listed after the account root, which resolves",
            entity: Entity::User {
                name: "rbp-tracked-listed",
                path: "/",
            },
            document: |named| {
                statement(
                    "Allow",
                    "Principal",
                    json!({"AWS": [&named.account_root, &named.principal]}),
                )
            },
        },
        Row {
            case: "a role under a Deny's NotPrincipal",
            entity: Entity::Role {
                name: "rbp-tracked-excepted",
            },
            document: |named| statement("Deny", "NotPrincipal", json!({"AWS": &named.principal})),
        },
        Row {
            case: "a user in the SECOND statement of a document",
            entity: Entity::User {
                name: "rbp-tracked-second",
                path: "/",
            },
            document: |named| {
                json!({
                    "Version": POLICY_VERSION,
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Principal": {"AWS": &named.account_root},
                            "Action": ACTION,
                            "Resource": PLACEHOLDER_RESOURCE,
                        },
                        {
                            "Effect": "Allow",
                            "Principal": {"AWS": &named.principal},
                            "Action": ACTION,
                            "Resource": PLACEHOLDER_RESOURCE,
                        },
                    ],
                })
            },
        },
    ]
}

impl Row {
    /// The values this row's document is written in terms of, in `account`.
    fn named(&self, account: &str) -> Named {
        Named {
            principal: self.entity.arn(account),
            account_root: format!("arn:aws:iam::{account}:root"),
        }
    }
}

/// Every assertion a refusal of an unresolvable principal must satisfy: the status, the code,
/// AWS's fixed sentence, and that the sentence carries nothing of the request.
fn assert_refused(case: &str, status: u16, body: &Value, named: &Named, entity: &Entity) {
    assert_eq!(
        (status, error_code(body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: a document naming {} must be refused 400 ValidationException, got \
         {status} with {body}",
        named.principal
    );
    let said = message(body);
    assert_eq!(
        said, INVALID_PRINCIPAL,
        "case {case:?}: expected AWS's fixed invalid-principal sentence, got {said:?}"
    );
    for carried in [
        named.principal.as_str(),
        entity.name(),
        "\"AWS\"",
        DOCUMENT_CLAUSE,
    ] {
        assert!(
            !said.contains(carried),
            "case {case:?}: the invalid-principal sentence is fixed text and must not carry \
             {carried:?}, got {said:?}"
        );
    }
}

/// RULE: see the module docs — `PutResourcePolicy` refuses a document while a principal of it
/// names an identity the account does not hold, stores nothing, and admits the IDENTICAL
/// document once IAM holds that identity.
#[tokio::test]
async fn put_resource_policy_admits_the_document_once_the_named_identity_exists() {
    let router = emulator_with_iam();
    let account = engine::EMULATOR_ACCOUNT_ID.to_string();
    let rows = rows();

    // While the account holds none of the named identities, every row's attach is refused and
    // the resource is left carrying no policy at all.
    let mut refused = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let table_arn = create_table(&router, row.case, &format!("rbp-tracked-{index}")).await;
        let named = row.named(&account);
        let document = (row.document)(&named);
        let attach = json!({"ResourceArn": &table_arn, "Policy": document.to_string()});
        let (status, body) = call(&router, "PutResourcePolicy", attach.clone()).await;
        assert_refused(row.case, status, &body, &named, &row.entity);
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
        assert_eq!(
            message(&read),
            format!("{NOT_FOUND_PREFIX}{table_arn}"),
            "case {:?}: the reader must name the ResourceArn of the request, got {:?}",
            row.case,
            message(&read)
        );
        refused.push((row, table_arn, named, document, attach));
    }

    // The identities are made, so the same documents now name principals the account can
    // resolve and the identical attach is admitted.
    let mut admitted = Vec::new();
    for (row, table_arn, named, document, attach) in refused {
        let made = row.entity.make(&router, row.case).await;
        assert_eq!(
            made, named.principal,
            "case {:?}: the identity IAM made must carry the ARN the document names it by",
            row.case
        );
        let (status, body) = call(&router, "PutResourcePolicy", attach).await;
        assert_eq!(
            status, 200,
            "case {:?}: the identical document must be admitted once {} exists, got {status} \
             with {body}",
            row.case, named.principal
        );
        let revision = body["RevisionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: no RevisionId in {body}", row.case))
            .to_string();
        admitted.push((row, table_arn, revision, document));
    }

    // Each admitted revision comes into force and reads back as it was attached.
    for (row, table_arn, revision, document) in admitted {
        let read = read_when_revised(&router, row.case, &table_arn, &revision).await;
        assert_eq!(
            reported_document(row.case, &read),
            document,
            "case {:?}: the admitted document must read back as it was attached, got {read}",
            row.case
        );
    }
}

/// RULE: the same instant decides `CreateTable$ResourcePolicy`, the other call that attaches a
/// document — refused while the named identity is missing, leaving no table behind, and admitted
/// once IAM holds it, with the document readable from the create itself ("when you attach a
/// resource-based policy while creating a table, the policy application is strongly consistent").
#[tokio::test]
async fn create_table_attaches_the_document_once_the_named_identity_exists() {
    let router = emulator_with_iam();
    let account = engine::EMULATOR_ACCOUNT_ID.to_string();

    for (index, row) in rows().iter().enumerate() {
        let table = format!("rbp-tracked-create-{index}");
        let named = row.named(&account);
        let document = (row.document)(&named);
        let create = json!({
            "TableName": &table,
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "ResourcePolicy": document.to_string(),
        });
        let (status, body) = call(&router, "CreateTable", create.clone()).await;
        assert_refused(row.case, status, &body, &named, &row.entity);
        let (status, described) =
            call(&router, "DescribeTable", json!({"TableName": &table})).await;
        assert_eq!(
            (status, error_code(&described).as_str()),
            (400, "ResourceNotFoundException"),
            "case {:?}: a refused create must leave no table, got {status} with {described}",
            row.case
        );

        let made = row.entity.make(&router, row.case).await;
        assert_eq!(
            made, named.principal,
            "case {:?}: the identity IAM made must carry the ARN the document names it by",
            row.case
        );
        let (status, created) = call(&router, "CreateTable", create).await;
        assert_eq!(
            status, 200,
            "case {:?}: the identical create must be admitted once {} exists, got {status} with \
             {created}",
            row.case, named.principal
        );
        let table_arn = created["TableDescription"]["TableArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: no TableArn in {created}", row.case))
            .to_string();
        let (status, read) = call(
            &router,
            "GetResourcePolicy",
            json!({"ResourceArn": &table_arn}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: a document attached by the create is readable from the create itself, \
             got {status} with {read}",
            row.case
        );
        assert_eq!(
            reported_document(row.case, &read),
            document,
            "case {:?}: the attached document must read back as it was attached, got {read}",
            row.case
        );
    }
}
