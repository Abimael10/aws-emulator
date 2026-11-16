// SPDX-License-Identifier: CC0-1.0

//! WHICH resource an SSE-KMS object's data key is bound to, and that both halves of the
//! envelope are judged under it.
//!
//! RULE: the encryption context S3 binds an SSE-KMS object's data key to names, under the
//! `aws:s3:arn` entry, the resource being encrypted — the BUCKET's ARN when an S3 Bucket Key
//! is in force for that object and the OBJECT's own ARN otherwise — so a KMS key policy
//! conditioned on that entry admits exactly one of the two, for the `kms:GenerateDataKey`
//! every write that stores such an object performs and for the `kms:Decrypt` every read that
//! serves its bytes performs alike, whichever way the Bucket Key setting reached the object.
//!
//! The sources:
//!
//! * Amazon S3 User Guide, *Using server-side encryption with KMS keys*, "Encryption
//!   context": S3 "uses the object or bucket Amazon Resource Name (ARN) as the encryption
//!   context pair", under the `aws:s3:arn` entry name.
//! * "Specifies whether Amazon S3 should use an S3 Bucket Key for object encryption with
//!   server-side encryption using Key Management Service (KMS) keys (SSE-KMS)"
//!   (`smithy_query(query="doc",shape="PutObjectRequest",member="BucketKeyEnabled")`) — one
//!   data key per BUCKET rather than per object, which is why the entry's value is the
//!   bucket's ARN.
//! * "You must also have the kms:GenerateDataKey and kms:Decrypt permissions in IAM
//!   identity-based policies and KMS key policies for the KMS key"
//!   (`smithy_query(query="doc",shape="HeadObject")`): the write's action and the read's,
//!   both asked of the object's key under that context.
//! * "Amazon S3 encrypts data with server-side encryption by using Amazon S3 managed keys
//!   (SSE-S3) by default. You can optionally tell Amazon S3 to encrypt data at rest by using
//!   server-side encryption with other key options"
//!   (`smithy_query(query="doc",shape="PutObjectRequest",member="ServerSideEncryption")`) —
//!   the other key options being the ones `PutBucketEncryption` configures, so a write naming
//!   no encryption member of its own inherits the bucket's Bucket Key setting too.
//!
//! What the tables span, beyond the one write the recordings carried: both
//! `ServerSideEncryption` values a KMS key belongs to; both ways the Bucket Key setting
//! reaches the object — the write's OWN header (sent explicitly `true` and explicitly
//! `false`) and the bucket's DEFAULT encryption configuration, inherited by a write naming no
//! encryption member; both resources a context-scoped statement can name; both halves of the
//! envelope, the write's data key and the read's decrypt; and the SWITCH itself — a bucket
//! whose default Bucket Key setting is changed under a fixed key policy, which flips which
//! writes that policy admits, in both directions.
//!
//! Every key, key policy, bucket, default encryption rule and object here is made with KMS's
//! and S3's own operations over one shared engine ([`super::kms_key_references`]), and the
//! keys, the documents and the refusal sentence are the ones the data-key suite beside this
//! one owns ([`super::sse_kms_data_key_authorization`]): nothing reaches into a store to
//! plant a key, a policy or a decision.

use serde_json::{json, Map, Value};

use super::bucket_encryption::{put_encryption, Rule};
use super::kms_key_references::router_with_kms;
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::sse_kms_data_key_authorization::{
    account_statement, bucket_arn, key_with, object_arn, policy, put_key_policy, Denial,
};

/// The `ServerSideEncryption` values a KMS key id belongs to
/// (`smithy_query(query="shape",shape="ServerSideEncryption")` lists both, and
/// `ApplyServerSideEncryptionByDefault$KMSMasterKeyID` is "allowed if and only if SSEAlgorithm
/// is set to aws:kms or aws:kms:dsse").
const KMS_ALGORITHMS: [&str; 2] = ["aws:kms", "aws:kms:dsse"];

/// The bytes every write of this suite carries: an encryption context is about WHICH resource
/// the data key is bound to, not about what is in the object, so one payload serves every row.
const BODY: &[u8] = b"sse-kms encryption-context payload";

/// The code S3 answers an authorization refusal with, and the status it carries (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";

/// The HTTP status that code is answered with.
const ACCESS_DENIED_STATUS: u16 = 403;

/// The action a write that stores an SSE-KMS object performs on its key.
const GENERATE_DATA_KEY_ACTION: &str = "kms:GenerateDataKey";

/// The action a read that serves such an object's bytes performs on it.
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// The encryption-context ENTRY NAME S3 binds the data key to (Amazon S3 User Guide, *Using
/// server-side encryption with KMS keys*, "Encryption context").
const ARN_CONTEXT_ENTRY: &str = "aws:s3:arn";

/// The bound header a write reports its S3 Bucket Key setting in
/// (`PutObjectOutput$BucketKeyEnabled`).
const BUCKET_KEY_HEADER: &str = "x-amz-server-side-encryption-bucket-key-enabled";

/// The object key every row of the tables writes: each row owns a bucket, so one key name
/// serves them all and the OBJECT's ARN still differs per row.
const OBJECT_KEY: &str = "payload.bin";

/// HOW the S3 Bucket Key setting reaches the object a row writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// The write's own `x-amz-server-side-encryption-bucket-key-enabled` header, beside the
    /// algorithm and the key it names.
    Header,
    /// The bucket's default encryption configuration, inherited by a write that names no
    /// encryption member at all.
    BucketDefault,
}

/// WHICH resource the key policy's `kms:EncryptionContext:aws:s3:arn` condition names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The bucket's ARN — the value a Bucket Key write's data key is bound to.
    Bucket,
    /// The object's own ARN — the value every other SSE-KMS write's data key is bound to.
    Object,
}

/// A key policy allowing the account every KMS action, but only for a call whose encryption
/// context binds `aws:s3:arn` to the resource `scope` names.
fn context_scoped_policy(scope: Scope, bucket: &str, key: &str) -> String {
    let arn = match scope {
        Scope::Bucket => bucket_arn(bucket),
        Scope::Object => object_arn(bucket, key),
    };
    let mut equals = Map::new();
    equals.insert(format!("kms:EncryptionContext:{ARN_CONTEXT_ENTRY}"), json!(arn));
    let mut condition = Map::new();
    condition.insert("StringEquals".to_string(), Value::Object(equals));
    policy(vec![account_statement(
        "Allow",
        json!("kms:*"),
        Value::Object(condition),
    )])
}

/// The headers one write sends for its encryption, for a row whose Bucket Key setting arrives
/// on the write ITSELF: the algorithm, the key it names, and the setting as the explicit
/// boolean the member takes.
fn write_headers<'a>(
    algorithm: &'a str,
    key_id: &'a str,
    bucket_key: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        ("x-amz-server-side-encryption", algorithm),
        ("x-amz-server-side-encryption-aws-kms-key-id", key_id),
        (BUCKET_KEY_HEADER, bucket_key),
    ]
}

/// One row of the context table: the algorithm, how the Bucket Key setting arrives, whether it
/// is on, and which resource the key policy's condition names.
#[derive(Debug, Clone, Copy)]
struct Row {
    algorithm: &'static str,
    arrival: Arrival,
    bucket_key: bool,
    scope: Scope,
}

impl Row {
    /// Whether the key policy admits this row's write: a context-scoped statement names ONE
    /// resource, and the resource the data key is bound to is the bucket when a Bucket Key is
    /// in force and the object otherwise — so the write is stored exactly when the two agree.
    fn stored(self) -> bool {
        matches!(self.scope, Scope::Bucket) == self.bucket_key
    }
}

/// Every combination the rule generalises over: both algorithms, both arrivals, the setting on
/// and off, and both resources a statement may name.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for algorithm in KMS_ALGORITHMS {
        for arrival in [Arrival::Header, Arrival::BucketDefault] {
            for bucket_key in [true, false] {
                for scope in [Scope::Bucket, Scope::Object] {
                    rows.push(Row {
                        algorithm,
                        arrival,
                        bucket_key,
                        scope,
                    });
                }
            }
        }
    }
    rows
}

#[tokio::test]
async fn an_sse_kms_data_key_is_bound_to_the_bucket_arn_only_where_a_bucket_key_is_in_force() {
    let router = router_with_kms();

    for (index, row) in rows().iter().enumerate() {
        let case = format!("{row:?}");
        let name = format!("sse-kms-context-{index}");
        bucket(&router, &name).await;
        let (key_id, key_arn) = key_with(
            &router,
            &case,
            Some(&context_scoped_policy(row.scope, &name, OBJECT_KEY)),
        )
        .await;

        // The Bucket Key setting reaches the write either on the write itself or through the
        // bucket's default encryption configuration, which a write naming no encryption
        // member inherits in full.
        let bucket_key = if row.bucket_key { "true" } else { "false" };
        let headers = match row.arrival {
            Arrival::Header => write_headers(row.algorithm, &key_id, bucket_key),
            Arrival::BucketDefault => {
                let configured = put_encryption(
                    &router,
                    &name,
                    &[Rule::by_default(row.algorithm)
                        .with_key(&key_id)
                        .with_bucket_key(row.bucket_key)],
                )
                .await;
                assert_eq!(
                    configured.status, 200,
                    "case {case:?}: the bucket's default must be configured first, got {} with \
                     body {:?}",
                    configured.status,
                    text(&configured)
                );
                Vec::new()
            }
        };

        let written = put_object(&router, &name, OBJECT_KEY, BODY, &headers).await;
        if row.stored() {
            assert_eq!(
                written.status, 200,
                "case {case:?}: the write must be stored, got {} with body {:?}",
                written.status,
                text(&written)
            );
            assert_eq!(
                written.header(BUCKET_KEY_HEADER).as_deref(),
                row.bucket_key.then_some("true"),
                "case {case:?}: expected the Bucket Key setting to be reported as {:?}, got {:?}",
                row.bucket_key.then_some("true"),
                written.header(BUCKET_KEY_HEADER)
            );
            // The READ half is judged under the same entry the write bound, so the object the
            // policy admitted the write of reads back as the bytes it carried.
            let read = get_object(&router, &name, OBJECT_KEY, "", &[]).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the stored object must read back, got {} with body {:?}",
                read.status,
                text(&read)
            );
            assert_eq!(
                read.body.as_ref(),
                BODY,
                "case {case:?}: expected the stored bytes, got {:?}",
                text(&read)
            );
            assert_eq!(
                read.header("x-amz-server-side-encryption").as_deref(),
                Some(row.algorithm),
                "case {case:?}: expected the algorithm {:?}, got {:?}",
                row.algorithm,
                read.header("x-amz-server-side-encryption")
            );
        } else {
            assert_eq!(
                written.status, ACCESS_DENIED_STATUS,
                "case {case:?}: the write must be refused with HTTP {ACCESS_DENIED_STATUS}, got \
                 {} with body {:?}",
                written.status,
                text(&written)
            );
            assert_eq!(
                written.error_code(),
                ACCESS_DENIED,
                "case {case:?}: expected the {ACCESS_DENIED:?} code, got {:?}",
                text(&written)
            );
            assert_eq!(
                written.error_message(),
                Denial::NoResourcePolicy.message(GENERATE_DATA_KEY_ACTION, &key_arn),
                "case {case:?}: expected KMS's denial sentence for the data-key action, got {:?}",
                written.error_message()
            );
            // "Amazon S3 never adds partial objects" (`PutObject`'s own documentation): a
            // refused write leaves the key holding nothing at all.
            let missing = get_object(&router, &name, OBJECT_KEY, "", &[]).await;
            assert_eq!(
                missing.status, 404,
                "case {case:?}: the refused write must store nothing, got {} with body {:?}",
                missing.status,
                text(&missing)
            );
        }
    }
}

#[tokio::test]
async fn a_read_of_an_sse_kms_object_is_judged_under_the_entry_its_write_bound() {
    let router = router_with_kms();
    let name = "sse-kms-context-read";
    bucket(&router, name).await;

    // Both values of the setting, each written under a policy that permits every action
    // unconditionally — so what the rows below vary is only the entry the READ is judged
    // under.
    for bucket_key in [true, false] {
        let case = format!("an object written with a Bucket Key of {bucket_key}");
        let object_key = format!("read-{bucket_key}.bin");
        let (key_id, key_arn) = key_with(&router, &case, None).await;
        let value = if bucket_key { "true" } else { "false" };
        let written = put_object(
            &router,
            name,
            &object_key,
            BODY,
            &write_headers(KMS_ALGORITHMS[0], &key_id, value),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the object must be stored first, got {} with body {:?}",
            written.status,
            text(&written)
        );

        // The entry the write bound is the bucket's ARN for a Bucket Key object and the
        // object's own otherwise, so the OTHER resource is what a policy has to name for the
        // read to be refused.
        let bound = if bucket_key { Scope::Bucket } else { Scope::Object };
        let other = if bucket_key { Scope::Object } else { Scope::Bucket };

        put_key_policy(
            &router,
            &case,
            &key_id,
            &context_scoped_policy(other, name, &object_key),
        )
        .await;
        let refused = get_object(&router, name, &object_key, "", &[]).await;
        assert_eq!(
            refused.status, ACCESS_DENIED_STATUS,
            "case {case:?}: a read under a policy naming {other:?} must be refused with HTTP \
             {ACCESS_DENIED_STATUS}, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            ACCESS_DENIED,
            "case {case:?}: expected the {ACCESS_DENIED:?} code, got {:?}",
            text(&refused)
        );
        assert_eq!(
            refused.error_message(),
            Denial::NoResourcePolicy.message(DECRYPT_ACTION, &key_arn),
            "case {case:?}: expected KMS's denial sentence for the decrypt, got {:?}",
            refused.error_message()
        );

        // And the same read is served the moment the policy names the entry the write really
        // bound — nothing about the object changed in between.
        put_key_policy(
            &router,
            &case,
            &key_id,
            &context_scoped_policy(bound, name, &object_key),
        )
        .await;
        let served = get_object(&router, name, &object_key, "", &[]).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: a read under a policy naming {bound:?} must be served, got {} with \
             body {:?}",
            served.status,
            text(&served)
        );
        assert_eq!(
            served.body.as_ref(),
            BODY,
            "case {case:?}: expected the stored bytes, got {:?}",
            text(&served)
        );
    }
}

#[tokio::test]
async fn switching_a_buckets_default_bucket_key_switches_which_writes_its_key_policy_admits() {
    let router = router_with_kms();

    // Both directions of the switch: a policy naming the BUCKET admits the writes made while
    // the default asks for a Bucket Key and withholds the rest, and a policy naming the
    // OBJECT admits exactly the opposite set.
    let directions: [(Scope, bool); 2] = [(Scope::Bucket, true), (Scope::Object, false)];

    for (index, (scope, admitted_with_bucket_key)) in directions.into_iter().enumerate() {
        let case = format!("a key policy scoped to {scope:?}");
        let name = format!("sse-kms-context-switch-{index}");
        bucket(&router, &name).await;
        let (key_id, key_arn) = key_with(
            &router,
            &case,
            Some(&context_scoped_policy(scope, &name, OBJECT_KEY)),
        )
        .await;

        // Each step configures the bucket's default and writes the SAME object key with no
        // encryption member of its own, so the only thing that changes between the steps is
        // the setting the write inherits — and with it the entry its data key is bound to.
        let steps = [
            admitted_with_bucket_key,
            !admitted_with_bucket_key,
            admitted_with_bucket_key,
        ];
        for (step, bucket_key) in steps.into_iter().enumerate() {
            let step_case = format!("{case}, step {step} with a default Bucket Key of {bucket_key}");
            let configured = put_encryption(
                &router,
                &name,
                &[Rule::by_default(KMS_ALGORITHMS[0])
                    .with_key(&key_id)
                    .with_bucket_key(bucket_key)],
            )
            .await;
            assert_eq!(
                configured.status, 200,
                "case {step_case:?}: the bucket's default must be configured, got {} with body \
                 {:?}",
                configured.status,
                text(&configured)
            );

            let written = put_object(&router, &name, OBJECT_KEY, BODY, &[]).await;
            if bucket_key == admitted_with_bucket_key {
                assert_eq!(
                    written.status, 200,
                    "case {step_case:?}: the write must be stored, got {} with body {:?}",
                    written.status,
                    text(&written)
                );
                let read = get_object(&router, &name, OBJECT_KEY, "", &[]).await;
                assert_eq!(
                    read.status, 200,
                    "case {step_case:?}: the stored object must read back, got {} with body {:?}",
                    read.status,
                    text(&read)
                );
                assert_eq!(
                    read.body.as_ref(),
                    BODY,
                    "case {step_case:?}: expected the stored bytes, got {:?}",
                    text(&read)
                );
            } else {
                assert_eq!(
                    written.status, ACCESS_DENIED_STATUS,
                    "case {step_case:?}: the write must be refused with HTTP \
                     {ACCESS_DENIED_STATUS}, got {} with body {:?}",
                    written.status,
                    text(&written)
                );
                assert_eq!(
                    written.error_code(),
                    ACCESS_DENIED,
                    "case {step_case:?}: expected the {ACCESS_DENIED:?} code, got {:?}",
                    text(&written)
                );
                assert_eq!(
                    written.error_message(),
                    Denial::NoResourcePolicy.message(GENERATE_DATA_KEY_ACTION, &key_arn),
                    "case {step_case:?}: expected KMS's denial sentence, got {:?}",
                    written.error_message()
                );
                // The object the admitted step stored is untouched by the refused write, and
                // its own data key was bound to the entry the policy names — so it stays
                // readable while writes of it are being refused.
                let still_readable = get_object(&router, &name, OBJECT_KEY, "", &[]).await;
                assert_eq!(
                    still_readable.status, 200,
                    "case {step_case:?}: the object written earlier must stay readable, got {} \
                     with body {:?}",
                    still_readable.status,
                    text(&still_readable)
                );
            }
        }
    }
}
