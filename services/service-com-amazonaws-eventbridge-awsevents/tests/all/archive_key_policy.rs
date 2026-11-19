// SPDX-License-Identifier: CC0-1.0

//! The KEY POLICY of the customer managed key an ARCHIVE is configured with decides whether
//! that configuration is accepted at all, end to end over the wire (`awsJson1_1`, `POST /`
//! with `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter` with
//! KMS mounted beside EventBridge on one engine — so the key is read where its own handlers
//! wrote it and the outcome is asserted where a client sees it: in the refusal, and in what
//! the archive reports back afterwards.
//!
//! The RULE asserted here: an archive that names a `KmsKeyIdentifier` has everything it
//! keeps encrypted under that key by EventBridge itself, as its own service principal
//! acting for that archive — "if you do not specify a customer managed key identifier,
//! EventBridge uses an Amazon Web Services owned key to encrypt the archive"
//! (`smithy_query(query="doc",shape="CreateArchiveRequest",member="KmsKeyIdentifier")`) —
//! and such a principal holds no identity-based policies, so the KEY's own policy is the
//! whole authority over the use. A key whose policy withholds the data key an archived event
//! needs is an archive that could never keep an event, so the configuration is refused with
//! HTTP 400 `ValidationException` and the archive is left exactly as it was: not created by
//! a refused `CreateArchive`, and still reporting no key after a refused `UpdateArchive`.
//! Writing the statement afterwards lets the identical call through with nothing else
//! changed.
//!
//! The rows span the class rather than the one recorded case: they differ along what the
//! key's policy states — whether a statement about this service exists at all, which
//! principal it names, which actions it covers (all of the ones an archive needs, one of
//! them, a wildcard over the KMS namespace), which `Effect` it states, and which condition
//! it is scoped by (the service the use arrives through, the bus whose events the archive
//! keeps, the account that owns it) — and along whether the archive names a key this account
//! holds at all. Every row is asserted for BOTH writers of an archive's key, which is where
//! the rule holds; a second table spans the four spellings one identifier may name a key by.
//!
//! The harness (the router with EventBridge, KMS and the target services on one engine) is
//! `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::SERVICE_PRINCIPAL;

use crate::target_dispatch::{emulator, events, rpc, Emulator, REGION};

/// The bus whose events the archive below keeps.
const BUS: &str = "archived-bus";

/// A second bus, named only as another statement's condition value — no archive below
/// keeps its events.
const OTHER_BUS: &str = "another-bus";

/// The archive every row configures. One name for both writers, so a policy scoped to the
/// archive's own source bus is the same statement in both legs.
const ARCHIVE: &str = "encrypted-archive";

/// The KMS action an archive's key is used through, and the one a refusal names: sealing
/// what the archive keeps means having a data key generated under that key.
const ARCHIVE_KEY_ACTION: &str = "GenerateDataKey";

/// The actions the EventBridge user guide's key policy for an archive lists, which is the
/// statement an account writes such a key with.
const ARCHIVE_KEY_ACTIONS: [&str; 4] = [
    "kms:DescribeKey",
    "kms:GenerateDataKey",
    "kms:Decrypt",
    "kms:ReEncrypt*",
];

/// Another service's principal, which is not the one an archive's key is used by.
const OTHER_SERVICE_PRINCIPAL: &str = "sqs.amazonaws.com";

/// The condition keys a key policy scopes a service's use of a key by: the service the use
/// arrives through, the encryption context the data is bound to, and the account the
/// resource acting belongs to.
const VIA_SERVICE_KEY: &str = "kms:ViaService";
const EVENT_BUS_CONTEXT_KEY: &str = "kms:EncryptionContext:aws:events:event-bus:arn";
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";

/// The account every ARN below belongs to.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The ARN of the bus `name` in the Region every call here is signed for.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{}:event-bus/{name}", account())
}

/// The regional endpoint name of the service a use of the key arrives through, which is
/// how a key policy's `kms:ViaService` spells one.
fn via_service(vendor: &str) -> String {
    format!("{vendor}.{REGION}.amazonaws.com")
}

/// One KMS call (`awsJson1_1`, `TrentService.<Operation>`) that must answer 200.
async fn kms(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = rpc(emulator, "kms", "TrentService", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} answered {status}: {answered}"
    );
    answered
}

/// The statement every key policy carries beside the row's own: the account root's
/// authority over its own key, which is what `CreateKey` writes and what the lockout safety
/// check of `PutKeyPolicy` requires of a replacement.
fn root_statement() -> Value {
    json!({
        "Sid": "Enable IAM policies",
        "Effect": "Allow",
        "Principal": { "AWS": format!("arn:aws:iam::{}:root", account()) },
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// A key policy document carrying the root statement and whatever `statements` adds.
fn key_policy(statements: Vec<Value>) -> String {
    let mut all = vec![root_statement()];
    all.extend(statements);
    json!({ "Version": "2012-10-17", "Id": "key-default", "Statement": all }).to_string()
}

/// One statement about a service principal's use of the key: `effect` of `actions` for
/// `principal`, scoped by `condition` when the row states one.
fn about_service(effect: &str, principal: &str, actions: Value, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Principal": { "Service": principal },
        "Action": actions,
        "Resource": "*",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    statement
}

/// The statement an archive key needs: this service, the actions an archive uses, unscoped.
fn admitting_events() -> Value {
    about_service("Allow", SERVICE_PRINCIPAL, json!(ARCHIVE_KEY_ACTIONS), None)
}

/// A customer managed key whose whole policy is `statements` beside the root statement,
/// answered as its ARN.
async fn given_key(emulator: &Emulator, case: &str, statements: Vec<Value>) -> String {
    let created = kms(emulator, case, "CreateKey", &json!({})).await;
    let arn = created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no KeyMetadata.Arn in {created}"))
        .to_string();
    kms(
        emulator,
        case,
        "PutKeyPolicy",
        &json!({
            "KeyId": arn,
            "PolicyName": "default",
            "Policy": key_policy(statements),
        }),
    )
    .await;
    arn
}

/// The sentence a configuration whose key withholds the use is refused with: which KMS call
/// came back denied, for which key, and what that key's policy has to admit — one sentence
/// for every way a key policy can withhold a service principal's use, which is why a policy
/// that states nothing and one that denies explicitly read alike.
fn denied_message(key_arn: &str) -> String {
    format!(
        "KMS returned an access denied exception when calling the {ARCHIVE_KEY_ACTION} API for \
         the key with ARN {key_arn}. Please ensure that {SERVICE_PRINCIPAL} has permissions to \
         access {ARCHIVE_KEY_ACTION}."
    )
}

/// What key a row configures its archive with.
enum Key {
    /// A key this account holds, carrying the statements the row states beside the root's.
    Own(Vec<Value>),
    /// A key ARN this account holds no key for, which is evidence of nothing.
    Unknown,
}

/// One row of the table.
struct Row {
    case: &'static str,
    key: Key,
    /// Whether the configuration is refused.
    refused: bool,
}

/// The two writers of an archive's `KmsKeyIdentifier`, which is where the rule holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Writer {
    Create,
    Update,
}

impl Writer {
    fn operation(self) -> &'static str {
        match self {
            Self::Create => "CreateArchive",
            Self::Update => "UpdateArchive",
        }
    }
}

/// The rows, built once and used by each writer: each carries the statements its key's
/// policy holds and the verdict the configuration earns.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a key whose policy admits this service the actions an archive needs",
            key: Key::Own(vec![admitting_events()]),
            refused: false,
        },
        Row {
            case: "a key carrying only the account root's own authority",
            key: Key::Own(Vec::new()),
            refused: true,
        },
        Row {
            case: "a key whose statement names another service",
            key: Key::Own(vec![about_service(
                "Allow",
                OTHER_SERVICE_PRINCIPAL,
                json!(ARCHIVE_KEY_ACTIONS),
                None,
            )]),
            refused: true,
        },
        Row {
            case: "a key admitting this service only the decryption",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!("kms:Decrypt"),
                None,
            )]),
            refused: true,
        },
        Row {
            case: "a key admitting this service only the data key an archived event needs",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!(format!("kms:{ARCHIVE_KEY_ACTION}")),
                None,
            )]),
            refused: false,
        },
        Row {
            case: "a key admitting this service a wildcard over the KMS namespace",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!("kms:*"),
                None,
            )]),
            refused: false,
        },
        Row {
            case: "a key whose policy denies this service beside the allow",
            key: Key::Own(vec![
                admitting_events(),
                about_service("Deny", SERVICE_PRINCIPAL, json!(ARCHIVE_KEY_ACTIONS), None),
            ]),
            refused: true,
        },
        Row {
            case: "a key scoped to uses arriving through this service",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!(ARCHIVE_KEY_ACTIONS),
                Some(json!({ "StringEquals": { VIA_SERVICE_KEY: via_service("events") } })),
            )]),
            refused: false,
        },
        Row {
            case: "a key scoped to uses arriving through another service",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!(ARCHIVE_KEY_ACTIONS),
                Some(json!({ "StringEquals": { VIA_SERVICE_KEY: via_service("sqs") } })),
            )]),
            refused: true,
        },
        Row {
            case: "a key scoped by its encryption context to the bus the archive keeps",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!(ARCHIVE_KEY_ACTIONS),
                Some(json!({ "StringEquals": { EVENT_BUS_CONTEXT_KEY: bus_arn(BUS) } })),
            )]),
            refused: false,
        },
        Row {
            case: "a key scoped by its encryption context to another bus",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!(ARCHIVE_KEY_ACTIONS),
                Some(json!({ "StringEquals": { EVENT_BUS_CONTEXT_KEY: bus_arn(OTHER_BUS) } })),
            )]),
            refused: true,
        },
        Row {
            case: "a key scoped to this account's own resources",
            key: Key::Own(vec![about_service(
                "Allow",
                SERVICE_PRINCIPAL,
                json!(ARCHIVE_KEY_ACTIONS),
                Some(json!({ "StringEquals": { SOURCE_ACCOUNT_KEY: account() } })),
            )]),
            refused: false,
        },
        Row {
            case: "a key this account holds none of, which is evidence of nothing",
            key: Key::Unknown,
            refused: false,
        },
    ]
}

/// The bus the archive keeps the events of, created before any archive is.
async fn given_bus(emulator: &Emulator, case: &str) {
    let (status, created) = events(emulator, "CreateEventBus", &json!({ "Name": BUS })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventBus answered {status}: {created}"
    );
}

/// An archive of [`BUS`] on the Amazon Web Services owned key — what the update leg has to
/// read the absence of a customer managed key off.
async fn given_keyless_archive(emulator: &Emulator, case: &str) {
    let (status, created) = events(
        emulator,
        "CreateArchive",
        &json!({ "ArchiveName": ARCHIVE, "EventSourceArn": bus_arn(BUS) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the unencrypted archive answered {status}: {created}"
    );
}

/// The key [`ARCHIVE`] reports itself on, or `None` for an archive on the Amazon Web
/// Services owned key — and for an archive a refused create never brought into being.
async fn described_key(emulator: &Emulator, case: &str) -> Option<String> {
    let (status, body) = events(
        emulator,
        "DescribeArchive",
        &json!({ "ArchiveName": ARCHIVE }),
    )
    .await;
    if status != 200 {
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: expected the archive to be missing, got {status}: {body}"
        );
        return None;
    }
    body["KmsKeyIdentifier"].as_str().map(str::to_string)
}

/// The request `writer` configures [`ARCHIVE`] with `key`.
fn request(writer: Writer, key: &str) -> Value {
    match writer {
        Writer::Create => json!({
            "ArchiveName": ARCHIVE,
            "EventSourceArn": bus_arn(BUS),
            "KmsKeyIdentifier": key,
        }),
        Writer::Update => json!({ "ArchiveName": ARCHIVE, "KmsKeyIdentifier": key }),
    }
}

/// An archive is configured on a customer managed key only where that key's own policy lets
/// this service use it — asserted for every writer of an archive's key, over a table of key
/// policies.
#[tokio::test]
async fn an_archive_is_configured_on_a_key_only_where_its_policy_admits_this_service() {
    for writer in [Writer::Create, Writer::Update] {
        for row in rows() {
            let case = format!("{} with {}", writer.operation(), row.case);
            let case = case.as_str();
            let emulator = emulator();
            given_bus(&emulator, case).await;
            let key = match row.key {
                Key::Own(statements) => given_key(&emulator, case, statements).await,
                Key::Unknown => format!(
                    "arn:aws:kms:{REGION}:{}:key/00000000-0000-0000-0000-000000000000",
                    account()
                ),
            };
            if writer == Writer::Update {
                given_keyless_archive(&emulator, case).await;
            }

            let (status, answered) = events(&emulator, writer.operation(), &request(writer, &key))
                .await;
            let reported = described_key(&emulator, case).await;
            if row.refused {
                assert_eq!(
                    status, 400,
                    "case {case:?}: the configuration must be refused, got {status}: {answered}"
                );
                assert_eq!(
                    answered["__type"],
                    json!("ValidationException"),
                    "case {case:?}: expected ValidationException, got {answered}"
                );
                let expected = denied_message(&key);
                assert_eq!(
                    answered["message"],
                    json!(expected),
                    "case {case:?}: expected the message {expected:?}, got {answered}"
                );
                assert!(
                    reported.is_none(),
                    "case {case:?}: a refused configuration must leave the archive on no \
                     customer managed key, got {reported:?}"
                );
            } else {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the configuration must be accepted, got {status}: {answered}"
                );
                assert_eq!(
                    reported.as_deref(),
                    Some(key.as_str()),
                    "case {case:?}: the archive must read back on that key, got {reported:?}"
                );
            }
        }
    }
}

/// A refused update leaves every OTHER member of the archive as it was, too: the decision is
/// taken before any part of the request is applied.
#[tokio::test]
async fn a_refused_update_applies_none_of_the_request() {
    let case = "an update carrying a description and a key the policy withholds";
    let emulator = emulator();
    given_bus(&emulator, case).await;
    let key = given_key(&emulator, case, Vec::new()).await;
    let (status, created) = events(
        &emulator,
        "CreateArchive",
        &json!({
            "ArchiveName": ARCHIVE,
            "EventSourceArn": bus_arn(BUS),
            "Description": "as created",
            "RetentionDays": 7,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the unencrypted archive answered {status}: {created}"
    );

    let (status, refused) = events(
        &emulator,
        "UpdateArchive",
        &json!({
            "ArchiveName": ARCHIVE,
            "Description": "as updated",
            "RetentionDays": 30,
            "EventPattern": json!({ "source": ["com.demo.orders"] }).to_string(),
            "KmsKeyIdentifier": key,
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the update must be refused, got {status}: {refused}"
    );

    let (status, described) = events(
        &emulator,
        "DescribeArchive",
        &json!({ "ArchiveName": ARCHIVE }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeArchive answered {status}: {described}"
    );
    for (member, expected) in [
        ("Description", json!("as created")),
        ("RetentionDays", json!(7)),
        ("EventPattern", Value::Null),
        ("KmsKeyIdentifier", Value::Null),
    ] {
        assert_eq!(
            described[member], expected,
            "case {case:?}: {member} must be as the create left it, got {described}"
        );
    }
}

/// The four spellings one identifier may name a key by — "the key Amazon Resource Name
/// (ARN), KeyId, key alias, or key alias ARN"
/// (`smithy_query(query="doc",shape="CreateArchiveRequest",member="KmsKeyIdentifier")`) —
/// are one key: each is refused while the policy withholds the use, each naming the ARN of
/// the key it resolved to, and each accepted once the statement is written.
#[tokio::test]
async fn every_spelling_of_one_identifier_is_decided_by_that_key_s_policy() {
    let case = "an identifier spelled four ways";
    let alias = "alias/archive-key";
    let emulator = emulator();
    given_bus(&emulator, case).await;
    given_keyless_archive(&emulator, case).await;
    let key_arn = given_key(&emulator, case, Vec::new()).await;
    let key_id = key_arn
        .rsplit_once("key/")
        .unwrap_or_else(|| panic!("case {case:?}: {key_arn} is no key ARN"))
        .1
        .to_string();
    kms(
        &emulator,
        case,
        "CreateAlias",
        &json!({ "AliasName": alias, "TargetKeyId": key_arn }),
    )
    .await;
    let alias_arn = format!("arn:aws:kms:{REGION}:{}:{alias}", account());
    let spellings = [
        ("the key ARN", key_arn.clone()),
        ("the key id", key_id),
        ("the key alias", alias.to_string()),
        ("the key alias ARN", alias_arn),
    ];

    for (spelling, identifier) in &spellings {
        let case = format!("{case}, refused for {spelling}");
        let case = case.as_str();
        let (status, answered) = events(
            &emulator,
            "UpdateArchive",
            &json!({ "ArchiveName": ARCHIVE, "KmsKeyIdentifier": identifier }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the configuration must be refused, got {status}: {answered}"
        );
        let expected = denied_message(&key_arn);
        assert_eq!(
            answered["message"],
            json!(expected),
            "case {case:?}: expected the message {expected:?}, got {answered}"
        );
        let reported = described_key(&emulator, case).await;
        assert!(
            reported.is_none(),
            "case {case:?}: a refused configuration must leave the archive on no customer \
             managed key, got {reported:?}"
        );
    }

    // Granting the use afterwards lets the IDENTICAL calls through, with nothing about the
    // archive or the requests changed in between — so the decision is a fact of the key's
    // policy and not of the call.
    kms(
        &emulator,
        case,
        "PutKeyPolicy",
        &json!({
            "KeyId": key_arn,
            "PolicyName": "default",
            "Policy": key_policy(vec![admitting_events()]),
        }),
    )
    .await;
    for (spelling, identifier) in &spellings {
        let case = format!("{case}, accepted for {spelling}");
        let case = case.as_str();
        let (status, answered) = events(
            &emulator,
            "UpdateArchive",
            &json!({ "ArchiveName": ARCHIVE, "KmsKeyIdentifier": identifier }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the configuration must be accepted once the key admits this \
             service, got {status}: {answered}"
        );
        let reported = described_key(&emulator, case).await;
        assert!(
            reported.is_some(),
            "case {case:?}: the archive must read back on a customer managed key, got \
             {reported:?}"
        );
    }
}
