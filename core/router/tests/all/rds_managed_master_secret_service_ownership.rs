// SPDX-License-Identifier: CC0-1.0

//! The Secrets Manager secret Amazon RDS MANAGES for a resource's master user password, read
//! back on the ASSEMBLED emulator.
//!
//! RULE: a secret an Amazon Web Services service creates and manages for one of its own
//! resources belongs to that service, not to the account that asked for the resource — so
//! every report of that secret names the service that created it
//! (`DescribeSecretResponse$OwningService` / `SecretListEntry$OwningService`, "The ID of the
//! service that created this secret"), and a caller's own change to it is refused:
//! `PutSecretValue` with `InvalidRequestException` and the service-linked-secret sentence,
//! `DeleteSecret` with `InvalidRequestException` and the sentence naming whose secret it is.
//! The refusal changes nothing — the credentials the owning service stored are still the ones
//! the secret holds afterwards — and a secret a CALLER created, which no service owns, takes
//! both of those calls.
//!
//! Only a suite beside the router can pin this end to end: the secret is MINTED by Amazon RDS
//! (`CreateDBInstance` / `CreateDBCluster` with `ManageMasterUserPassword` on, optionally
//! under a KMS key the request names) and REPORTED and GUARDED by Secrets Manager, and the
//! hop RDS makes to mint it is dispatched back through this same router — so what is asserted
//! here is the marker surviving the emulator's own claiming and routing, which a crate-local
//! suite mounting a hand-picked subset of services cannot see. The per-service tables live in
//! the RDS crate (`managed_secret_service_ownership.rs`, `managed_secret_direct_write_refusal.rs`)
//! and in the Secrets Manager crate (`secret_service_owned_writes.rs`).
//!
//! The rows differ along what the rule generalises over: the KIND of resource whose password
//! is managed (a DB instance, a DB cluster), the engine family, and whether the secret is
//! encrypted under a customer managed KMS key the request named or under the key the service
//! uses when a request names none. [`a_callers_own_secret_is_owned_by_nobody_and_takes_both_calls`]
//! is the other side of the same rule.
//!
//! Sources: `smithy_query(query="operation", operation="DescribeSecret")` on the
//! secretsmanager model for `OwningService`, and `smithy_query(query="operation",
//! operation="CreateDBInstance")` on the rds model for `ManageMasterUserPassword` and
//! `MasterUserSecretKmsKeyId`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// How often a poll looks again, and how long it keeps looking: what the poll waits for is
/// owed by the CLOCK, so it waits for the CONDITION under a generous deadline rather than
/// for a fixed span.
const LOOK: Duration = Duration::from_millis(50);
const DEADLINE: Duration = Duration::from_secs(120);

/// `MasterUserSecret$SecretStatus` once the credentials can be used.
const SECRET_ACTIVE: &str = "active";

/// The error Secrets Manager refuses a change to a secret another service manages with
/// (one of the documented causes of `InvalidRequestException`).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The sentence a direct WRITE to such a secret is refused with: fixed text naming the
/// owning service by the name AWS publishes for it and by the service id the secret reports.
const WRITE_REFUSAL: &str = "This secret is managed by Amazon RDS (service ID: rds), and you \
                             must use that service to update it. For more information, see \
                             https://docs.aws.amazon.com/secretsmanager/latest/userguide/service-linked-secrets.html.";

/// The sentence a DELETE of one is refused with: fixed text naming the owning service by the
/// id the secret reports.
const DELETE_REFUSAL: &str = "Operation is not allowed on secret owned by rds";

/// The service id a secret Amazon RDS manages reports as its `OwningService`, and the prefix
/// such a secret's name carries.
const RDS_SERVICE_ID: &str = "rds";
const RDS_SECRET_NAME_PREFIX: &str = "rds!";

/// The key a secret that names none of its own is encrypted under: the Amazon Web Services
/// managed key for this service, which `DescribeSecret` may report by that alias or not at
/// all.
const DEFAULT_SECRET_KEY_ALIAS: &str = "alias/aws/secretsmanager";

/// The emulator as it is ASSEMBLED, on one engine: the hop Amazon RDS makes to Secrets
/// Manager to mint the secret it manages is dispatched back into this same router.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and names the Region its ARNs are built in.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon RDS awsQuery call, answered with the HTTP status and the XML body.
async fn rds(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2014-10-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("rds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("rds"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One awsJson1_1 call to `signing_name` at its own host, answered with the HTTP status and
/// the parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Secrets Manager call.
async fn secrets(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(
        router,
        "secretsmanager",
        &format!("secretsmanager.{operation}"),
        body,
    )
    .await
}

/// One KMS call.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(router, "kms", &format!("TrentService.{operation}"), body).await
}

/// The wire error code of a refusal, without the namespace a `__type` may qualify it with.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message of a refusal, whichever casing the envelope spells the member with.
fn error_message(body: &Value) -> String {
    for member in ["Message", "message"] {
        if let Some(text) = body.get(member).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    String::new()
}

/// A string member of an answered document, or the empty string.
fn text_of(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A customer managed key for the rows that name one, reported by the ARN KMS allocated.
async fn create_key(router: &MultiServiceRouter) -> String {
    let (status, created) = kms(
        router,
        "CreateKey",
        &json!({ "Description": "the key a managed master user secret is encrypted under" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateKey must succeed, got {status} with {created}"
    );
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: a minted key reports its ARN: {created}"))
        .to_string()
}

/// The ARN of the key `identifier` names, so a reported key is compared as the key KMS
/// resolves it to rather than as one permitted spelling of it.
async fn resolved_key_arn(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let (status, described) = kms(router, "DescribeKey", &json!({ "KeyId": identifier })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeKey({identifier:?}) must succeed, got {status} with {described}"
    );
    described["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a key reports its ARN: {described}"))
        .to_string()
}

/// The credentials document a secret holds, as the master user and the password in it.
async fn held_credentials(
    router: &MultiServiceRouter,
    case: &str,
    secret_arn: &str,
) -> (String, String) {
    let (status, read) = secrets(router, "GetSecretValue", &json!({ "SecretId": secret_arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSecretValue({secret_arn:?}) must be served, got {status} with {read}"
    );
    let document: Value = serde_json::from_str(&text_of(&read, "SecretString"))
        .unwrap_or_else(|_| panic!("case {case:?}: the secret holds a credentials document: {read}"));
    (
        text_of(&document, "username"),
        text_of(&document, "password"),
    )
}

/// Which resource a row manages the master user password of, and how it is addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Instance,
    Cluster,
}

impl Kind {
    /// The operation that creates it with the management turned on.
    fn create(self) -> &'static str {
        match self {
            Self::Instance => "CreateDBInstance",
            Self::Cluster => "CreateDBCluster",
        }
    }

    /// The operation that reads it back.
    fn describe(self) -> &'static str {
        match self {
            Self::Instance => "DescribeDBInstances",
            Self::Cluster => "DescribeDBClusters",
        }
    }

    /// The member its identifier travels in.
    fn identifier_member(self) -> &'static str {
        match self {
            Self::Instance => "DBInstanceIdentifier",
            Self::Cluster => "DBClusterIdentifier",
        }
    }
}

/// One row: the resource whose managed secret is read back and written to, and whether the
/// create names a customer managed key for it.
struct Row {
    case: &'static str,
    kind: Kind,
    engine: &'static str,
    master_username: &'static str,
    extra: &'static [(&'static str, &'static str)],
    /// Whether the create carries `MasterUserSecretKmsKeyId`.
    names_kms_key: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an RDS for PostgreSQL DB instance under a key the request named",
        kind: Kind::Instance,
        engine: "postgres",
        master_username: "sim2real",
        extra: &[("DBInstanceClass", "db.t4g.micro"), ("AllocatedStorage", "20")],
        names_kms_key: true,
    },
    Row {
        case: "an RDS for MySQL DB instance under the key the service uses by default",
        kind: Kind::Instance,
        engine: "mysql",
        master_username: "admin",
        extra: &[("DBInstanceClass", "db.t3.micro"), ("AllocatedStorage", "20")],
        names_kms_key: false,
    },
    Row {
        case: "an Aurora PostgreSQL DB cluster under a key the request named",
        kind: Kind::Cluster,
        engine: "aurora-postgresql",
        master_username: "clusteradmin",
        extra: &[],
        names_kms_key: true,
    },
];

/// Poll the resource until the secret RDS minted for it reports `active`, answering that
/// secret's ARN: the credentials are being created when the call that asked for them answers,
/// and only the clock settles them.
async fn await_active_secret(
    router: &MultiServiceRouter,
    case: &str,
    kind: Kind,
    identifier: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, read) = rds(
            router,
            kind.describe(),
            &[(kind.identifier_member(), identifier)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {}({identifier:?}) must succeed, got {status} with {read}",
            kind.describe()
        );
        let reported = block_of(&read, "MasterUserSecret").unwrap_or_else(|| {
            panic!("case {case:?}: a managed-password resource reports its secret: {read}")
        });
        if opt_element(reported, "SecretStatus").unwrap_or_default() == SECRET_ACTIVE {
            return element(reported, "SecretArn");
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the managed secret of {identifier:?} must become usable, last read \
             {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

#[tokio::test]
async fn an_rds_managed_master_user_secret_is_owned_by_rds_and_closed_to_direct_writes() {
    let router = build_router();
    let customer_key = create_key(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let identifier = format!("emu-owned-secret-{index}");
        let mut params: Vec<(&str, String)> = vec![
            (row.kind.identifier_member(), identifier.clone()),
            ("Engine", row.engine.to_string()),
            ("MasterUsername", row.master_username.to_string()),
            ("ManageMasterUserPassword", "true".to_string()),
        ];
        params.extend(
            row.extra
                .iter()
                .map(|(name, value)| (*name, (*value).to_string())),
        );
        if row.names_kms_key {
            params.push(("MasterUserSecretKmsKeyId", customer_key.clone()));
        }
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let (status, created) = rds(&router, row.kind.create(), &borrowed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {}({identifier:?}) must be served, got {status} with {created}",
            row.kind.create()
        );
        let secret_arn = await_active_secret(&router, case, row.kind, &identifier).await;

        // 1. The secret is reported as the OWNING SERVICE's, under the name AWS reserves for
        //    that service, and it is not a deleted secret.
        let (status, described) =
            secrets(&router, "DescribeSecret", &json!({ "SecretId": secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeSecret({secret_arn:?}) must be served, got {status} with \
             {described}"
        );
        assert_eq!(
            described.get("OwningService").and_then(Value::as_str),
            Some(RDS_SERVICE_ID),
            "case {case:?}: expected OwningService {RDS_SERVICE_ID:?} on the secret the service \
             manages, got {described}"
        );
        let name = text_of(&described, "Name");
        assert!(
            name.starts_with(RDS_SECRET_NAME_PREFIX),
            "case {case:?}: expected the secret's name under {RDS_SECRET_NAME_PREFIX:?}, got \
             {name:?}"
        );
        assert_eq!(
            text_of(&described, "ARN"),
            secret_arn,
            "case {case:?}: expected the described secret to be the one the resource reports, \
             got {described}"
        );
        assert!(
            described.get("DeletedDate").is_none(),
            "case {case:?}: expected a secret in service to carry no DeletedDate, got {described}"
        );

        // And it is held under the key the create asked for: the one the request named, else
        // the key the service uses when a request names none.
        let reported_key = {
            let member = text_of(&described, "KmsKeyId");
            if member.is_empty() {
                DEFAULT_SECRET_KEY_ALIAS.to_string()
            } else {
                member
            }
        };
        let held_key = resolved_key_arn(&router, case, &reported_key).await;
        if row.names_kms_key {
            assert_eq!(
                held_key, customer_key,
                "case {case:?}: expected the secret to be held under the key the request named, \
                 got {reported_key:?}"
            );
        } else {
            assert_ne!(
                held_key, customer_key,
                "case {case:?}: expected a secret that named no key to be held under another \
                 key, got {reported_key:?}"
            );
        }

        // 2. The same marker wherever the account's secrets are REPORTED: a client scanning
        //    them tells the managed credential from one a caller created.
        let (status, listed) = secrets(&router, "ListSecrets", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListSecrets must be served, got {status} with {listed}"
        );
        let entry = listed["SecretList"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .find(|entry| text_of(entry, "ARN") == secret_arn)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected {secret_arn:?} among the listed secrets: {listed}")
            });
        assert_eq!(
            entry.get("OwningService").and_then(Value::as_str),
            Some(RDS_SERVICE_ID),
            "case {case:?}: expected the listed secret to report OwningService \
             {RDS_SERVICE_ID:?}, got {entry}"
        );

        let minted = held_credentials(&router, case, &secret_arn).await;
        assert_eq!(
            minted.0, row.master_username,
            "case {case:?}: expected the secret to hold the master user the request named, got \
             {:?}",
            minted.0
        );
        assert!(
            !minted.1.is_empty(),
            "case {case:?}: expected the secret to hold a generated password"
        );

        // 3. A caller's own WRITE of it is refused, with the sentence AWS words for that call.
        let (status, refused) = secrets(
            &router,
            "PutSecretValue",
            &json!({
                "SecretId": secret_arn,
                "ClientRequestToken": "11111111-2222-3333-4444-555555555555",
                "SecretString": "{\"username\":\"someone\",\"password\":\"a-callers-own\"}",
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected PutSecretValue on the managed secret to be refused, got \
             {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST} from PutSecretValue, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            WRITE_REFUSAL,
            "case {case:?}: expected the service-linked-secret sentence, got {refused}"
        );

        // 4. And a caller's own DELETE of it, with the sentence AWS words for that one.
        let (status, refused) = secrets(
            &router,
            "DeleteSecret",
            &json!({ "SecretId": secret_arn, "ForceDeleteWithoutRecovery": true }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected DeleteSecret on the managed secret to be refused, got \
             {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST} from DeleteSecret, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            DELETE_REFUSAL,
            "case {case:?}: expected the owned-secret sentence, got {refused}"
        );

        // Both refusals were inert: the secret still holds what the owning service stored,
        // and is still a secret in service.
        let after = held_credentials(&router, case, &secret_arn).await;
        assert_eq!(
            after, minted,
            "case {case:?}: a refused direct write must leave the managed credentials alone"
        );
        let (status, still) =
            secrets(&router, "DescribeSecret", &json!({ "SecretId": secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the secret must still be described, got {status} with {still}"
        );
        assert!(
            still.get("DeletedDate").is_none(),
            "case {case:?}: a refused delete must schedule nothing, got {still}"
        );
    }
}

/// The other side of the rule: a secret a CALLER created is owned by nobody — it reports no
/// `OwningService` — so the very calls the managed secret refuses are served on it. That is
/// what shows the refusals above are keyed on the secret's OWNERSHIP and not on the
/// operation.
#[tokio::test]
async fn a_callers_own_secret_is_owned_by_nobody_and_takes_both_calls() {
    let router = build_router();
    let case = "a secret the caller created for itself";
    let (status, created) = secrets(
        &router,
        "CreateSecret",
        &json!({
            "Name": "emu-callers-own-credential",
            "ClientRequestToken": "99999999-8888-7777-6666-555555555555",
            "SecretString": "{\"username\":\"someone\",\"password\":\"theirs\"}",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSecret must be served, got {status} with {created}"
    );
    let secret_arn = text_of(&created, "ARN");
    assert!(
        !secret_arn.is_empty(),
        "case {case:?}: expected the create to report the secret's ARN, got {created}"
    );

    let (status, described) =
        secrets(&router, "DescribeSecret", &json!({ "SecretId": secret_arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSecret must be served, got {status} with {described}"
    );
    assert!(
        described.get("OwningService").is_none(),
        "case {case:?}: expected no OwningService on a caller's own secret, got {described}"
    );

    let (status, written) = secrets(
        &router,
        "PutSecretValue",
        &json!({
            "SecretId": secret_arn,
            "ClientRequestToken": "77777777-6666-5555-4444-333333333333",
            "SecretString": "{\"username\":\"someone\",\"password\":\"the-next-one\"}",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutSecretValue on an unowned secret must be served, got {status} with \
         {written}"
    );
    let held = held_credentials(&router, case, &secret_arn).await;
    assert_eq!(
        held.1, "the-next-one",
        "case {case:?}: expected the served write to be the value the secret now holds, got {:?}",
        held.1
    );

    let (status, deleted) = secrets(
        &router,
        "DeleteSecret",
        &json!({ "SecretId": secret_arn, "ForceDeleteWithoutRecovery": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteSecret on an unowned secret must be served, got {status} with \
         {deleted}"
    );
}
