// SPDX-License-Identifier: CC0-1.0

//! The blocked encryption types a general purpose bucket has IN FORCE, read and enforced:
//! what `GetBucketEncryption` reports and what a write supplying its own encryption key is
//! judged by are the SAME set, row by row.
//!
//! RULE: the types a bucket's default encryption configuration blocks are BUCKET-level state —
//! `BlockedEncryptionTypes` is "a bucket-level setting for Amazon S3 general purpose buckets
//! used to prevent the upload of new objects encrypted with the specified server-side
//! encryption type", and "currently, this parameter only supports blocking or unblocking
//! server-side encryption with customer-provided keys (SSE-C)"
//! (`smithy_query(query="doc",shape="ServerSideEncryptionRule",member="BlockedEncryptionTypes")`)
//! — so every rule the report carries names the set in force on the bucket (the set a
//! configuration named, or the set the bucket carries anyway where none named one), and that
//! same set is what refuses a write presenting a customer-provided key.
//!
//! Two consequences, which are what this suite pins together:
//!
//! * a reader of the subresource can tell "this type is blocked" from "nothing is reported",
//!   because the member is always there — the whole point of a deploy-time hardening
//!   assertion over it; and
//! * the report can never disagree with what the bucket does, so a client that reads the set
//!   and a client that writes an object see one bucket rather than two.
//!
//! The suites beside this one pin each half on its own — which MEMBERS a rule is reported
//! with and where they sit in the document ([`crate::bucket_encryption_reported_members`]),
//! and what a write presenting its own key is answered
//! ([`crate::object_customer_key_encryption`]). This one drives both halves against the same
//! bucket in every row, so neither can drift from the other.
//!
//! The rows differ along what the rule generalises over: whether the bucket was configured at
//! all, WHICH algorithm its configuration names, whether a KMS key or the S3 Bucket Key
//! accompanies it, whether a rule names a blocked set and WHICH published `EncryptionType`
//! values it names, how MANY rules the configuration carries and which of them name a set,
//! whether a later configuration or a reset replaced one that named a set, and whether the
//! bucket is hardened elsewhere (versioning enabled and every published public-access switch
//! on) — none of which may change the answer either half gives.

use ::base64::Engine as _;
use ::md5::Digest as _;
use smithy_server_runtime::MultiServiceRouter;

use crate::bucket_lifecycle::{router, send, Answer};
use crate::bucket_public_access_block::{put_public_access_block, SETTINGS};
use crate::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use crate::object_versioning::set_versioning;
// The payload writer, the reported-rule reader, the rule type and the two published value
// lists of the suite this module belongs to, so a row sends what that suite sends and names
// the values the model publishes rather than a second copy of either.
// `default_rule` is the parent's own reading of the rule a bucket nothing has configured
// reports, derived from the crate's documented defaults, so this suite expects exactly what
// that suite expects rather than a second spelling of it.
use super::{
    default_rule, get_encryption, put_encryption, reported_rules, Rule, ENCRYPTION_TYPE_VALUES,
    SERVER_SIDE_ENCRYPTION_VALUES,
};

/// The two values the `EncryptionType` enum publishes, taken from the parent suite's
/// model-derived list (`smithy_query(query="shape",shape="EncryptionType")` reports
/// `[NONE, SSE-C]`): the first names the set that blocks no upload at all, the second
/// server-side encryption with customer-provided keys.
const PERMITTED_TYPE: &str = ENCRYPTION_TYPE_VALUES[0];
const CUSTOMER_KEY_TYPE: &str = ENCRYPTION_TYPE_VALUES[1];

/// SSE-S3 and SSE-KMS as `ServerSideEncryption` values, from the same parent list
/// (`smithy_query(query="shape",shape="ServerSideEncryption")` reports
/// `[AES256, aws:fsx, aws:backup, aws:kms, aws:kms:dsse]`): SSE-S3 is the algorithm name
/// itself, and a `KMSMasterKeyID` is "allowed if and only if `SSEAlgorithm` is set to
/// `aws:kms` or `aws:kms:dsse`" (`ServerSideEncryptionByDefault$KMSMasterKeyID`).
const SSE_S3: &str = SERVER_SIDE_ENCRYPTION_VALUES[0];
const SSE_KMS: &str = SERVER_SIDE_ENCRYPTION_VALUES[3];

/// Whether a bucket reports the S3 Bucket Key on while nothing has named it: "general purpose
/// buckets - by default, S3 Bucket Key is not enabled"
/// (`ServerSideEncryptionRule$BucketKeyEnabled`).
const BUCKET_KEY_OFF: bool = false;

/// A KMS key in one of the three forms `KMSMasterKeyID` documents ("you can specify the key
/// ID, key alias, or the Amazon Resource Name (ARN) of the KMS key"): the fully qualified
/// ARN. Nothing about the key decides either half of the rule under test, so one form is
/// enough here — the parent suite spans all three.
const KEY_ARN: &str = "arn:aws:kms:us-east-1:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// The `BucketVersioningStatus` the hardened rows put the bucket into.
const VERSIONING_ENABLED: &str = "Enabled";

/// The three headers one customer-provided key travels in
/// (`PutObjectRequest$SSECustomerAlgorithm`, `$SSECustomerKey` and `$SSECustomerKeyMD5`).
const ALGORITHM_HEADER: &str = "x-amz-server-side-encryption-customer-algorithm";
const KEY_HEADER: &str = "x-amz-server-side-encryption-customer-key";
const KEY_MD5_HEADER: &str = "x-amz-server-side-encryption-customer-key-md5";

/// The one algorithm such a key is presented under: "`AES256` is the only supported value"
/// (`PutObjectRequest$SSECustomerAlgorithm`).
const CUSTOMER_KEY_ALGORITHM: &str = "AES256";

/// What a write refused by the bucket's blocked set is answered with: the authorization code,
/// and the fixed clause naming the setting that refused it.
///
/// Spelled out here rather than read from the crate, so the wording a client branches on has
/// something to be wrong about. The clause itself interpolates nothing — the principal and the
/// object the sentence around it names come from the request.
const ACCESS_DENIED: &str = "AccessDenied";
const BLOCKED_CLAUSE: &str = "because this bucket has blocked upload requests that specify \
                              Server Side Encryption with Customer provided keys (SSE-C). \
                              Please specify a different server-side encryption type.";

/// The code a read of a key the bucket does not hold is answered with, which is what a refused
/// write must leave behind.
const NO_SUCH_KEY: &str = "NoSuchKey";

/// The object every row writes under its own key, and the bytes it writes there.
const PROBE_KEY: &str = "blocked-types/probe.bin";
const PROBE_BODY: &[u8] = b"the bytes a customer-provided key would have sealed";

/// One row: how the bucket came to carry the set it carries, and the rules it must then
/// report. Whether the write is served is DERIVED from those rules rather than stated, which
/// is the agreement this suite is about.
struct Row {
    case: &'static str,
    /// The bucket the row stands up.
    bucket_name: &'static str,
    /// Each `PutBucketEncryption` the row makes, in order — so a row can replace one
    /// configuration with another. Empty for a bucket no put has configured.
    puts: Vec<Vec<Rule>>,
    /// Whether a `DeleteBucketEncryption` follows those puts.
    reset: bool,
    /// Whether the bucket is hardened elsewhere: versioning enabled and every published
    /// public-access switch on.
    hardened: bool,
    /// The key material the row's write presents, as a seed, so every row presents a
    /// different key.
    key_seed: u8,
    /// What `GetBucketEncryption` must report, rule by rule and member by member.
    reported: Vec<Rule>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bucket no put has configured",
            bucket_name: "in-force-unconfigured",
            puts: Vec::new(),
            reset: false,
            hardened: false,
            key_seed: 11,
            reported: vec![default_rule()],
        },
        Row {
            case: "a configuration naming the SSE-S3 algorithm and nothing else",
            bucket_name: "in-force-plain-sse-s3",
            puts: vec![vec![Rule::by_default(SSE_S3)]],
            reset: false,
            hardened: false,
            key_seed: 23,
            reported: vec![default_rule()],
        },
        Row {
            case: "a configuration naming SSE-S3 with the Bucket Key on",
            bucket_name: "in-force-bucket-key-on",
            puts: vec![vec![Rule::by_default(SSE_S3).with_bucket_key(true)]],
            reset: false,
            hardened: false,
            key_seed: 37,
            reported: vec![Rule::by_default(SSE_S3)
                .with_bucket_key(true)
                .blocking(&[CUSTOMER_KEY_TYPE])],
        },
        Row {
            case: "a configuration naming SSE-KMS with a key",
            bucket_name: "in-force-sse-kms",
            puts: vec![vec![Rule::by_default(SSE_KMS).with_key(KEY_ARN)]],
            reset: false,
            hardened: false,
            key_seed: 53,
            reported: vec![Rule::by_default(SSE_KMS)
                .with_key(KEY_ARN)
                .with_bucket_key(BUCKET_KEY_OFF)
                .blocking(&[CUSTOMER_KEY_TYPE])],
        },
        Row {
            case: "a configuration naming the set that blocks nothing",
            bucket_name: "in-force-permitting",
            puts: vec![vec![Rule::by_default(SSE_S3).blocking(&[PERMITTED_TYPE])]],
            reset: false,
            hardened: false,
            key_seed: 67,
            reported: vec![Rule::by_default(SSE_S3)
                .with_bucket_key(BUCKET_KEY_OFF)
                .blocking(&[PERMITTED_TYPE])],
        },
        Row {
            case: "a configuration naming the customer-key type itself",
            bucket_name: "in-force-blocking",
            puts: vec![vec![Rule::by_default(SSE_S3).blocking(&[CUSTOMER_KEY_TYPE])]],
            reset: false,
            hardened: false,
            key_seed: 71,
            reported: vec![default_rule()],
        },
        Row {
            case: "a configuration naming both published types, in that order",
            bucket_name: "in-force-blocking-both",
            puts: vec![vec![
                Rule::by_default(SSE_S3).blocking(&[PERMITTED_TYPE, CUSTOMER_KEY_TYPE])
            ]],
            reset: false,
            hardened: false,
            key_seed: 89,
            reported: vec![Rule::by_default(SSE_S3)
                .with_bucket_key(BUCKET_KEY_OFF)
                .blocking(&[PERMITTED_TYPE, CUSTOMER_KEY_TYPE])],
        },
        Row {
            case: "a configuration naming no set replacing one that named the permitting set",
            bucket_name: "in-force-replaced",
            puts: vec![
                vec![Rule::by_default(SSE_S3).blocking(&[PERMITTED_TYPE])],
                vec![Rule::by_default(SSE_S3)],
            ],
            reset: false,
            hardened: false,
            key_seed: 97,
            reported: vec![default_rule()],
        },
        Row {
            case: "the permitting configuration reset by a delete",
            bucket_name: "in-force-reset",
            puts: vec![vec![Rule::by_default(SSE_S3).blocking(&[PERMITTED_TYPE])]],
            reset: true,
            hardened: false,
            key_seed: 103,
            reported: vec![default_rule()],
        },
        Row {
            case: "two rules, only the second of which carries the bucket's own set",
            bucket_name: "in-force-two-rules",
            puts: vec![vec![
                Rule::by_default(SSE_S3).blocking(&[PERMITTED_TYPE]),
                Rule::bucket_key_only(true),
            ]],
            reset: false,
            hardened: false,
            key_seed: 109,
            reported: vec![
                Rule::by_default(SSE_S3)
                    .with_bucket_key(BUCKET_KEY_OFF)
                    .blocking(&[PERMITTED_TYPE]),
                Rule::bucket_key_only(true).blocking(&[CUSTOMER_KEY_TYPE]),
            ],
        },
        Row {
            case: "a hardened bucket no put has configured",
            bucket_name: "in-force-hardened",
            puts: Vec::new(),
            reset: false,
            hardened: true,
            key_seed: 127,
            reported: vec![default_rule()],
        },
        Row {
            case: "a hardened bucket configured with the set that blocks nothing",
            bucket_name: "in-force-hardened-permitting",
            puts: vec![vec![Rule::by_default(SSE_S3).blocking(&[PERMITTED_TYPE])]],
            reset: false,
            hardened: true,
            key_seed: 149,
            reported: vec![Rule::by_default(SSE_S3)
                .with_bucket_key(BUCKET_KEY_OFF)
                .blocking(&[PERMITTED_TYPE])],
        },
    ]
}

#[test]
fn the_values_this_suite_names_are_the_ones_the_model_publishes() {
    // The two names picked out of each published list, so an index that drifted is a failure
    // here rather than a row that tests something else.
    assert_eq!(
        (PERMITTED_TYPE, CUSTOMER_KEY_TYPE),
        ("NONE", "SSE-C"),
        "the `EncryptionType` values this suite names must be the published ones, got {:?}",
        (PERMITTED_TYPE, CUSTOMER_KEY_TYPE)
    );
    assert_eq!(
        (SSE_S3, SSE_KMS),
        ("AES256", "aws:kms"),
        "the `ServerSideEncryption` values this suite names must be the published ones, got \
         {:?}",
        (SSE_S3, SSE_KMS)
    );
}

#[tokio::test]
async fn the_blocked_set_a_bucket_reports_is_the_set_its_writes_are_judged_by() {
    let router = router();

    for row in rows() {
        let case = row.case;
        let name = row.bucket_name;
        bucket(&router, name).await;
        if row.hardened {
            harden(&router, name, case).await;
        }
        for configuration in &row.puts {
            let put = put_encryption(&router, name, configuration).await;
            assert_eq!(
                put.status, 200,
                "case {case:?}: PutBucketEncryption({configuration:?}) must answer 200, got {} \
                 with body {:?}",
                put.status,
                text(&put)
            );
        }
        if row.reset {
            let deleted = delete_encryption(&router, name).await;
            assert_eq!(
                deleted.status, 204,
                "case {case:?}: DeleteBucketEncryption must answer the modelled 204, got {} \
                 with body {:?}",
                deleted.status,
                text(&deleted)
            );
        }

        // What the bucket REPORTS: every rule with the set in force on it, so the member is
        // never absent and a reader can tell a blocked type from an unreported one.
        let read = get_encryption(&router, name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetBucketEncryption must answer 200, got {} with body {:?}",
            read.status,
            text(&read)
        );
        let reported = reported_rules(&read);
        assert_eq!(
            reported, row.reported,
            "case {case:?}: expected the rules {:?}, got {reported:?} from body {:?}",
            row.reported,
            text(&read)
        );

        // And what it ENFORCES, decided by the set it just reported rather than by what the
        // row configured: a write presenting a customer-provided key is refused exactly when
        // that set names the customer-key type.
        assert_write_agrees(&router, name, case, row.key_seed, blocks_customer_key(&reported))
            .await;
    }
}

/// Whether `rules` — the rules as the bucket REPORTED them — name the customer-key type among
/// the types they block, which is the one type the setting supports today.
fn blocks_customer_key(rules: &[Rule]) -> bool {
    rules.iter().any(|rule| {
        rule.blocked_encryption_types
            .as_deref()
            .is_some_and(|blocked| blocked.iter().any(|kind| kind == CUSTOMER_KEY_TYPE))
    })
}

/// Write one object under a customer-provided key and assert the answer agrees with `blocked`:
/// a refusal that stores nothing where the reported set blocks the type, and a served write
/// that reports the key's digest back where it does not.
async fn assert_write_agrees(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    key_seed: u8,
    blocked: bool,
) {
    let key = customer_key(key_seed);
    let digest = key_digest(&key);
    let headers = [
        (ALGORITHM_HEADER, CUSTOMER_KEY_ALGORITHM),
        (KEY_HEADER, key.as_str()),
        (KEY_MD5_HEADER, digest.as_str()),
    ];
    let written = put_object(router, name, PROBE_KEY, PROBE_BODY, &headers).await;

    if blocked {
        assert_eq!(
            (written.status, written.error_code()),
            (403, ACCESS_DENIED.to_string()),
            "case {case:?}: the reported set blocks the customer-key type, so the write must \
             be refused 403 {ACCESS_DENIED:?}, got {} with body {:?}",
            written.status,
            text(&written)
        );
        assert!(
            written.error_message().ends_with(BLOCKED_CLAUSE),
            "case {case:?}: the refusal must name the bucket setting that refused it, got {:?}",
            written.error_message()
        );
        // Refused means nothing was written: the object the write addressed is not there.
        let absent = get_object(router, name, PROBE_KEY, "", &[]).await;
        assert_eq!(
            (absent.status, absent.error_code()),
            (404, NO_SUCH_KEY.to_string()),
            "case {case:?}: the refused write must have stored nothing, got {} with body {:?}",
            absent.status,
            text(&absent)
        );
        return;
    }

    assert_eq!(
        written.status, 200,
        "case {case:?}: the reported set does not block the customer-key type, so the write \
         must be served, got {} with body {:?}",
        written.status,
        text(&written)
    );
    assert_eq!(
        written.header(KEY_MD5_HEADER).as_deref(),
        Some(digest.as_str()),
        "case {case:?}: the served write must report the digest of the key it was given, got \
         {:?}",
        written.header(KEY_MD5_HEADER)
    );
    // And the object is really there, under that key.
    let served = get_object(router, name, PROBE_KEY, "", &headers).await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: a read presenting the same key must be served, got {} with body {:?}",
        served.status,
        text(&served)
    );
    assert_eq!(
        served.body.as_ref(),
        PROBE_BODY,
        "case {case:?}: the read must serve the stored bytes, got {} bytes",
        served.body.len()
    );
}

/// Harden `name` elsewhere: versioning enabled and every published public-access switch on —
/// state that has nothing to do with the bucket's encryption, which is the point of the rows
/// that set it.
async fn harden(router: &MultiServiceRouter, name: &str, case: &str) {
    set_versioning(router, name, VERSIONING_ENABLED).await;
    let switches: Vec<(&str, bool)> = SETTINGS.iter().map(|setting| (*setting, true)).collect();
    let blocked_public = put_public_access_block(router, name, &switches, &[]).await;
    assert_eq!(
        blocked_public.status, 200,
        "case {case:?}: PutPublicAccessBlock must answer 200, got {} with body {:?}",
        blocked_public.status,
        text(&blocked_public)
    );
}

/// `DELETE /<bucket>?encryption` — `DeleteBucketEncryption`, which resets the configuration.
async fn delete_encryption(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send(router, "DELETE", &format!("/{bucket}?encryption"), &[], "").await
}

/// A 256-bit key, base64 encoded as the `-key` header carries it, derived from `seed` so that
/// every row presents a DIFFERENT key.
fn customer_key(seed: u8) -> String {
    let material: Vec<u8> = (0..32_u8)
        .map(|index| index.wrapping_mul(31).wrapping_add(seed))
        .collect();
    ::base64::engine::general_purpose::STANDARD.encode(&material)
}

/// The base64 MD5 of the key `key` carries, as the `-key-MD5` header states it: "the 128-bit
/// MD5 digest of the encryption key according to RFC 1321"
/// (`PutObjectRequest$SSECustomerKeyMD5`).
fn key_digest(key: &str) -> String {
    let material = ::base64::engine::general_purpose::STANDARD
        .decode(key)
        .expect("the test key must be base64");
    ::base64::engine::general_purpose::STANDARD.encode(::md5::Md5::digest(&material))
}
