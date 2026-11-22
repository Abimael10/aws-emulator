// SPDX-License-Identifier: CC0-1.0

//! Which TARGET KEY an `UpdateObjectEncryption` may move an object's envelope onto.
//!
//! RULE: re-encrypting an object S3 already holds performs, on the KMS key the request names,
//! the KMS operation that re-encryption needs — the stored envelope is encrypted under the
//! target key, which is `kms:Encrypt` ("to use this operation, you must have the following
//! permissions: s3:UpdateObjectEncryption kms:Encrypt kms:Decrypt kms:GenerateDataKey
//! kms:ReEncrypt*", this operation's own documentation) — as the request's own principal and
//! through S3's service path. A caller no authority in force on that key reaches that action
//! for is refused `AccessDenied` with HTTP 403 (one of the three errors the operation models,
//! `smithy_query(query="operation",operation="UpdateObjectEncryption")`) carrying the sentence
//! KMS words for the refusal, and the refused call is a NO-OP: every reader still reports the
//! encryption, the key, the ETag and the bytes the object already had. The decision is the
//! key's authorities as they stand at the moment of the call, so it lifts and returns as that
//! document is moved, with no intervening call of any kind.
//!
//! The rows span what the rule generalises over rather than one recorded call:
//!
//! * WHICH authority refuses — an explicit deny naming every principal, the same deny naming
//!   the account's root user, a deny whose action element is a list pairing the data-key
//!   actions by pattern with the encrypt, and a document that allows the account another
//!   action only, so no resource-based policy reaches the encrypt at all;
//! * WHICH action the policy speaks about — a deny of the data-key action alone leaves the
//!   update served, because a re-encryption mints no data key;
//! * what the object was encrypted with BEFORE the call — SSE-S3, which is what an object
//!   written with no encryption headers carries, and SSE-KMS under another key of this
//!   account — since a refusal must leave either of them untouched;
//! * and the DIRECTION the key's policy moves in: a deny lifted makes the identical call
//!   succeed, and a deny put back refuses it again.
//!
//! Every key, key policy, bucket and object here is made with KMS's and S3's own operations
//! over one shared engine: nothing reaches into a store to plant a key, a policy or a
//! decision.

use serde_json::{json, Value};

use crate::kms_key_references::router_with_kms;
use crate::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use crate::sse_kms_data_key_authorization::{
    account_statement, allow_account, key_with, policy, put_key_policy, Denial,
};
// The payload writer, the dispatcher and the reader of the suite this module belongs to, so
// what a row sends and what it reads back are the ones that suite pins.
use super::{head_object, payload, update_object_encryption};

/// The action a call that re-encrypts an object S3 ALREADY holds performs on the key it is
/// re-encrypted TO: no new data key is minted and no plaintext is served, the stored envelope
/// is encrypted under that key.
const ENCRYPT_ACTION: &str = "kms:Encrypt";

/// The data-key action as this operation's own permission list names it, carrying the trailing
/// wildcard a key policy's author writes it with — the action a re-encryption does NOT perform.
const GENERATE_DATA_KEY_PATTERN: &str = "kms:GenerateDataKey*";

/// The action a READ of an SSE-KMS object performs, which no row here denies: a refused
/// update's object has to stay readable for the no-op to be observable.
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// The code S3 answers an authorization refusal with (Amazon S3 "Error responses":
/// `AccessDenied`, which this operation models with `httpError 403`).
const ACCESS_DENIED: &str = "AccessDenied";

/// The `ServerSideEncryption` value SSE-KMS carries, and the one SSE-S3 carries
/// (`smithy_query(query="shape",shape="ServerSideEncryption")` publishes both).
const SSE_KMS: &str = "aws:kms";
/// See [`SSE_KMS`].
const SSE_S3: &str = "AES256";

/// A key-policy statement over every resource naming EVERY principal, which is the form an
/// explicit deny meant to stop a key being used by anybody takes — the one construct no allow
/// elsewhere overrides.
fn everyone_statement(effect: &str, actions: Value) -> Value {
    json!({
        "Effect": effect,
        "Principal": "*",
        "Action": actions,
        "Resource": "*",
    })
}

/// What the object under test was encrypted with before the update, which a refused call has
/// to leave exactly as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// Encryption with Amazon S3 managed keys, which names no KMS key of this account: what an
    /// object written with no encryption headers carries.
    SseS3,
    /// Encryption under a customer-managed KMS key of this account, named by the write.
    SseKms,
}

impl Source {
    /// The part of an object key that says which source this is, so one row's object is never
    /// another's.
    fn slug(self) -> &'static str {
        match self {
            Self::SseS3 => "sse-s3",
            Self::SseKms => "sse-kms",
        }
    }

    /// The `ServerSideEncryption` a reader reports for an object stored this way.
    fn algorithm(self) -> &'static str {
        match self {
            Self::SseS3 => SSE_S3,
            Self::SseKms => SSE_KMS,
        }
    }

    /// The KMS key a reader reports for such an object, `None` for the one that names none.
    fn key(self, key_arn: &str) -> Option<String> {
        match self {
            Self::SseS3 => None,
            Self::SseKms => Some(key_arn.to_string()),
        }
    }

    /// The headers the write that stores such an object sends.
    fn headers(self, key_arn: &str) -> Vec<(&str, &str)> {
        match self {
            Self::SseS3 => Vec::new(),
            Self::SseKms => vec![
                ("x-amz-server-side-encryption", SSE_KMS),
                ("x-amz-server-side-encryption-aws-kms-key-id", key_arn),
            ],
        }
    }
}

/// Every source encryption the update is judged over, each of which a refusal must leave
/// untouched.
const SOURCES: [Source; 2] = [Source::SseS3, Source::SseKms];

/// One row of the target-policy table: the key policy the TARGET key carries, and why the
/// update is refused under it (`None` for a row that must be served).
struct PolicyRow {
    case: &'static str,
    /// The document the target key is created with; `None` for a key carrying the default key
    /// policy, which delegates the decision to the account.
    document: Option<String>,
    denied: Option<Denial>,
}

/// The target-policy table, one row per authority the decision can be taken under.
fn policy_rows() -> Vec<PolicyRow> {
    vec![
        PolicyRow {
            case: "an explicit deny of the encrypt naming every principal",
            document: Some(policy(vec![
                allow_account(),
                everyone_statement("Deny", json!(ENCRYPT_ACTION)),
            ])),
            denied: Some(Denial::ExplicitDeny),
        },
        PolicyRow {
            case: "an explicit deny naming the data-key actions by pattern beside the encrypt",
            document: Some(policy(vec![
                allow_account(),
                everyone_statement("Deny", json!([GENERATE_DATA_KEY_PATTERN, ENCRYPT_ACTION])),
            ])),
            denied: Some(Denial::ExplicitDeny),
        },
        PolicyRow {
            case: "that same deny naming the account's root user",
            document: Some(policy(vec![
                allow_account(),
                account_statement("Deny", json!(ENCRYPT_ACTION), Value::Null),
            ])),
            denied: Some(Denial::ExplicitDeny),
        },
        PolicyRow {
            case: "a policy allowing the account the read action only",
            document: Some(policy(vec![account_statement(
                "Allow",
                json!(DECRYPT_ACTION),
                Value::Null,
            )])),
            denied: Some(Denial::NoResourcePolicy),
        },
        PolicyRow {
            case: "a deny of the data-key action alone, which a re-encryption never performs",
            document: Some(policy(vec![
                allow_account(),
                everyone_statement("Deny", json!(GENERATE_DATA_KEY_PATTERN)),
            ])),
            denied: None,
        },
        PolicyRow {
            case: "a policy allowing the account every action",
            document: Some(policy(vec![allow_account()])),
            denied: None,
        },
        PolicyRow {
            case: "a key carrying its default key policy",
            document: None,
            denied: None,
        },
    ]
}

#[tokio::test]
async fn an_update_onto_a_key_whose_authorities_refuse_the_encrypt_is_denied_and_changes_nothing() {
    let router = router_with_kms();
    let name = "object-encryption-update-target-authz";
    bucket(&router, name).await;

    // The key the SSE-KMS source objects are stored under, which permits everything: every row
    // is about the key the object is re-encrypted TO.
    let (_, source_key) =
        key_with(&router, "the key the source objects are stored under", None).await;

    for (index, row) in policy_rows().iter().enumerate() {
        for source in SOURCES {
            let case = format!("{} over an object stored with {}", row.case, source.slug());
            let object_key = format!("subject-{index}-{}.bin", source.slug());
            // Bytes of this row's own, so nothing is keyed to one payload and the ETag the
            // update has to preserve differs per row.
            let body: Vec<u8> = (0..=u8::try_from(index).unwrap_or(0))
                .cycle()
                .take(48 + index)
                .collect();
            let stored = put_object(
                &router,
                name,
                &object_key,
                &body,
                &source.headers(&source_key),
            )
            .await;
            assert_eq!(
                stored.status, 200,
                "case {case:?}: the object must be stored first, got {} with body {:?}",
                stored.status,
                text(&stored)
            );
            // The ETag the write itself minted, which the update preserves whichever way it
            // answers: the value is the stored object's own, not one this suite computes, so a
            // source encryption that words its ETag differently is compared against its own.
            let minted_etag = stored.header("etag").unwrap_or_else(|| {
                panic!("case {case:?}: the write must report the ETag it minted")
            });

            let (_, target_arn) = key_with(&router, &case, row.document.as_deref()).await;
            let answer = update_object_encryption(
                &router,
                name,
                &object_key,
                "",
                &payload(&target_arn, None),
            )
            .await;

            // What every reader must report afterwards: the object as it was for a refused
            // call, and SSE-KMS under the target key for a served one.
            let (expected_algorithm, expected_key) = match row.denied {
                Some(denial) => {
                    assert_eq!(
                        answer.status, 403,
                        "case {case:?}: a target key whose authorities refuse the encrypt is \
                         HTTP 403, got {} with body {:?}",
                        answer.status,
                        text(&answer)
                    );
                    assert_eq!(
                        answer.error_code(),
                        ACCESS_DENIED,
                        "case {case:?}: expected the {ACCESS_DENIED:?} code, got {:?} with body \
                         {:?}",
                        answer.error_code(),
                        text(&answer)
                    );
                    assert_eq!(
                        answer.error_message(),
                        denial.message(ENCRYPT_ACTION, &target_arn),
                        "case {case:?}: expected KMS's own sentence for the refusal, got {:?}",
                        answer.error_message()
                    );
                    (source.algorithm(), source.key(&source_key))
                }
                None => {
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?}: a target key whose authorities permit the encrypt must \
                         be served, got {} with body {:?}",
                        answer.status,
                        text(&answer)
                    );
                    (SSE_KMS, Some(target_arn.clone()))
                }
            };

            let head = head_object(&router, name, &object_key).await;
            assert_eq!(
                head.header("x-amz-server-side-encryption").as_deref(),
                Some(expected_algorithm),
                "case {case:?}: expected the object to report {expected_algorithm:?}, got {:?}",
                head.header("x-amz-server-side-encryption")
            );
            assert_eq!(
                head.header("x-amz-server-side-encryption-aws-kms-key-id"),
                expected_key,
                "case {case:?}: expected the object to report the key {expected_key:?}, got {:?}",
                head.header("x-amz-server-side-encryption-aws-kms-key-id")
            );

            // The object's own bytes and ETag survive either answer: a served update moves no
            // data, and a refused one writes nothing at all.
            assert_eq!(
                head.header("etag").as_deref(),
                Some(minted_etag.as_str()),
                "case {case:?}: the ETag must be the one the write minted, got {:?}",
                head.header("etag")
            );
            let read = get_object(&router, name, &object_key, "", &[]).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the object must still read back, got {} with body {:?}",
                read.status,
                text(&read)
            );
            assert_eq!(
                read.body.as_ref(),
                body.as_slice(),
                "case {case:?}: expected the stored bytes back, got {} of them",
                read.body.len()
            );
        }
    }
}

/// One step of the moved-policy table: the case, which document is attached to the target key
/// before the call (`None` to leave the one already on it), and whether the identical call is
/// served under it.
type MovedStep = (&'static str, Option<usize>, bool);

#[tokio::test]
async fn the_refusal_follows_the_target_keys_policy_as_it_is_moved_either_way() {
    let router = router_with_kms();
    let name = "object-encryption-update-policy-moved";
    bucket(&router, name).await;
    let object_key = "subject.bin";
    let body: Vec<u8> = (0..=u8::MAX).cycle().take(300).collect();
    let stored = put_object(&router, name, object_key, &body, &[]).await;
    assert_eq!(
        stored.status, 200,
        "the object must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );
    let minted_etag = stored
        .header("etag")
        .expect("the write must report the ETag it minted");

    // The two documents the key's policy is moved between: one denying the encrypt to every
    // principal, one allowing the account everything.
    let documents: [String; 2] = [
        policy(vec![
            allow_account(),
            everyone_statement("Deny", json!([GENERATE_DATA_KEY_PATTERN, ENCRYPT_ACTION])),
        ]),
        policy(vec![allow_account()]),
    ];
    let (key_id, key_arn) =
        key_with(&router, "the target key its policy is moved on", Some(&documents[0])).await;

    // ONE call, issued identically at every step: what changes between them is the target key's
    // policy and nothing else — so the answer is that policy's at the moment of the call, in
    // both directions, and no answer is carried over from the step before.
    let steps: [MovedStep; 4] = [
        ("the key as created, denying the encrypt", None, false),
        ("the deny lifted", Some(1), true),
        ("the identical call again under that same policy", None, true),
        ("the deny put back", Some(0), false),
    ];

    let mut expected_algorithm = SSE_S3;
    let mut expected_key: Option<String> = None;
    for (case, document, served) in steps {
        if let Some(index) = document {
            put_key_policy(&router, case, &key_id, &documents[index]).await;
        }
        let answer =
            update_object_encryption(&router, name, object_key, "", &payload(&key_arn, None)).await;
        if served {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: the identical call must be served, got {} with body {:?}",
                answer.status,
                text(&answer)
            );
            expected_algorithm = SSE_KMS;
            expected_key = Some(key_arn.clone());
        } else {
            assert_eq!(
                answer.status, 403,
                "case {case:?}: the identical call must be refused with HTTP 403, got {} with \
                 body {:?}",
                answer.status,
                text(&answer)
            );
            assert_eq!(
                answer.error_code(),
                ACCESS_DENIED,
                "case {case:?}: expected the {ACCESS_DENIED:?} code, got {:?} with body {:?}",
                answer.error_code(),
                text(&answer)
            );
            assert_eq!(
                answer.error_message(),
                Denial::ExplicitDeny.message(ENCRYPT_ACTION, &key_arn),
                "case {case:?}: expected KMS's own sentence for the refusal, got {:?}",
                answer.error_message()
            );
        }

        let head = head_object(&router, name, object_key).await;
        assert_eq!(
            head.header("x-amz-server-side-encryption").as_deref(),
            Some(expected_algorithm),
            "case {case:?}: expected the object to report {expected_algorithm:?}, got {:?}",
            head.header("x-amz-server-side-encryption")
        );
        assert_eq!(
            head.header("x-amz-server-side-encryption-aws-kms-key-id"),
            expected_key,
            "case {case:?}: expected the object to report the key {expected_key:?}, got {:?}",
            head.header("x-amz-server-side-encryption-aws-kms-key-id")
        );
        assert_eq!(
            head.header("etag").as_deref(),
            Some(minted_etag.as_str()),
            "case {case:?}: the ETag must be the one the write minted, got {:?}",
            head.header("etag")
        );
        let read = get_object(&router, name, object_key, "", &[]).await;
        assert_eq!(
            read.body.as_ref(),
            body.as_slice(),
            "case {case:?}: expected the stored bytes back, got {} of them",
            read.body.len()
        );
    }
}
