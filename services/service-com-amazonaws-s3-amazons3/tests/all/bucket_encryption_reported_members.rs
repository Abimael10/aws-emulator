// SPDX-License-Identifier: CC0-1.0

//! Which MEMBERS a general purpose bucket's default encryption is reported with.
//!
//! RULE: `GetBucketEncryption` reports, for every rule, the settings IN FORCE on the
//! bucket — a setting with a documented default is materialised at that default, and the
//! bucket-level blocked set is reported whether a configuration named it or not.
//!
//! The two documented defaults are the algorithm ("by default, all buckets have a default
//! encryption configuration that uses server-side encryption with Amazon S3 managed keys
//! (SSE-S3)", `GetBucketEncryption`'s own documentation) and the S3 Bucket Key ("general
//! purpose buckets - by default, S3 Bucket Key is not enabled",
//! `smithy_query(query="doc",shape="ServerSideEncryptionRule",member="BucketKeyEnabled")`),
//! so both are reported whatever a rule named. `BlockedEncryptionTypes` is "a bucket-level
//! setting for Amazon S3 general purpose buckets used to prevent the upload of new objects
//! encrypted with the specified server-side encryption type", and "currently, this parameter
//! only supports blocking or unblocking server-side encryption with customer-provided keys
//! (SSE-C)"
//! (`smithy_query(query="doc",shape="ServerSideEncryptionRule",member="BlockedEncryptionTypes")`)
//! — a setting of the BUCKET rather than of one put, so a rule that named no set is in force
//! with the set the bucket carries anyway (the customer-key type) and a rule that named one
//! is in force with that.
//!
//! Why it matters on the wire rather than only in a parsed view: a compliance reader asks
//! this operation which encryption types the bucket blocks, and a client that round-trips the
//! rules it is handed (Get → Put) must be handed the set in force.
//!
//! The rows differ along what the rule generalises over: whether the bucket was configured
//! at all, WHICH algorithm a rule names (every `ServerSideEncryption` value the model
//! publishes, including the three no recorded call sends), whether a KMS key accompanies
//! it, whether the Bucket Key is on, off or unnamed, whether a rule names a block and
//! WHICH `EncryptionType` values it blocks (each published value and both together), how
//! MANY rules the container carries and which of them block, and whether the bucket's
//! blocked set was established and then replaced or reset.

use service_com_amazonaws_s3_amazons3::state::{DEFAULT_BUCKET_KEY_ENABLED, DEFAULT_SSE_ALGORITHM};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_encryption::{
    get_encryption, put_encryption, reported_rules, Rule, ENCRYPTION_TYPE_VALUES,
    SERVER_SIDE_ENCRYPTION_VALUES,
};
use super::bucket_lifecycle::{create_bucket, router, send, Answer};
use super::object_and_tagging_roundtrip::text;

/// The element a reported rule carries its blocked set in — the container whose presence
/// this suite is about (`BlockedEncryptionTypes`, the member's own name: the model puts no
/// `xmlName` on it).
const BLOCKED_ELEMENT: &str = "BlockedEncryptionTypes";

/// Create a bucket and assert it was created, so a later assertion about what it reports is
/// about a bucket that exists.
async fn bucket(router: &MultiServiceRouter, name: &str, case: &str) {
    let created = create_bucket(router, name, None, None).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateBucket({name:?}) must answer 200, got {} with body {:?}",
        created.status,
        text(&created)
    );
}

/// `DELETE /<bucket>?encryption` — `DeleteBucketEncryption`.
async fn delete_encryption(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send(router, "DELETE", &format!("/{bucket}?encryption"), &[], "").await
}

/// Read the bucket's configuration back and assert, on the wire and in the parsed view,
/// that it reports `expected` — including which blocked set each rule carries.
async fn assert_reports(
    router: &MultiServiceRouter,
    bucket_name: &str,
    case: &str,
    expected: &[Rule],
) {
    let read = get_encryption(router, bucket_name).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetBucketEncryption({bucket_name:?}) must answer 200, got {} with \
         body {:?}",
        read.status,
        text(&read)
    );
    let reported = reported_rules(&read);
    assert_eq!(
        reported, expected,
        "case {case:?}: {bucket_name:?} must report {expected:?}, got {reported:?} (body {:?})",
        text(&read)
    );

    // The container's presence, asserted on the bytes as well: every rule in force carries
    // the blocked set the bucket is on, so the element appears once per rule.
    let blocking_rules = expected
        .iter()
        .filter(|rule| rule.blocked_encryption_types.is_some())
        .count();
    let occurrences = text(&read).matches(&format!("<{BLOCKED_ELEMENT}>")).count();
    assert_eq!(
        occurrences, blocking_rules,
        "case {case:?}: {bucket_name:?} must carry {blocking_rules} <{BLOCKED_ELEMENT}> \
         element(s), got {occurrences} (body {:?})",
        text(&read)
    );
}

/// One row: the case, the bucket it configures, and the configuration it puts — `None` for
/// a bucket left exactly as `CreateBucket` made it.
struct Row {
    case: &'static str,
    bucket: &'static str,
    configured: Option<Vec<Rule>>,
}

/// The rule every bucket reports while no `PutBucketEncryption` has configured one: the
/// documented SSE-S3 algorithm with the Bucket Key off, and the bucket-level blocked set.
fn base_default() -> Rule {
    Rule::by_default(DEFAULT_SSE_ALGORITHM)
        .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
        .blocking(&[ENCRYPTION_TYPE_VALUES[1]])
}

/// The rules each row must report: the configured ones with `BucketKeyEnabled` materialised,
/// the bucket-level blocked set in force where a rule named none, and every other member
/// exactly as configured — or the base default.
fn expected_of(row: &Row) -> Vec<Rule> {
    match &row.configured {
        None => vec![base_default()],
        Some(rules) => rules
            .iter()
            .map(|rule| Rule {
                bucket_key_enabled: Some(rule.bucket_key_enabled.unwrap_or(false)),
                blocked_encryption_types: rule
                    .blocked_encryption_types
                    .clone()
                    .or_else(|| Some(vec![ENCRYPTION_TYPE_VALUES[1].to_string()])),
                ..rule.clone()
            })
            .collect(),
    }
}

/// A KMS key in the ARN form `KMSMasterKeyID` documents ("you can specify the key ID, key
/// alias, or the Amazon Resource Name (ARN) of the KMS key").
const KEY_ARN: &str = "arn:aws:kms:us-west-2:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab";

fn rows() -> Vec<Row> {
    // The two published `EncryptionType` values, taken from the model's own list rather
    // than spelled here: `NONE` unblocks and `SSE-C` blocks the customer-key type.
    let [unblocked, customer_key] = ENCRYPTION_TYPE_VALUES;
    // The algorithm values, likewise from the published list: index 0 is SSE-S3 and the
    // last two are the SSE-KMS pair a `KMSMasterKeyID` may accompany.
    let sse_s3 = SERVER_SIDE_ENCRYPTION_VALUES[0];
    let kms = SERVER_SIDE_ENCRYPTION_VALUES[3];
    let dual_layer_kms = SERVER_SIDE_ENCRYPTION_VALUES[4];

    vec![
        Row {
            case: "a bucket no put has configured",
            bucket: "members-unconfigured",
            configured: None,
        },
        Row {
            case: "an SSE-S3 default, named explicitly and nothing else",
            bucket: "members-sse-s3",
            configured: Some(vec![Rule::by_default(sse_s3)]),
        },
        Row {
            case: "an SSE-KMS default naming no key, as a toolkit staging bucket declares",
            bucket: "members-kms-no-key",
            configured: Some(vec![Rule::by_default(kms)]),
        },
        Row {
            case: "an SSE-KMS default naming a key ARN, with the Bucket Key on",
            bucket: "members-kms-keyed",
            configured: Some(vec![Rule::by_default(kms)
                .with_key(KEY_ARN)
                .with_bucket_key(true)]),
        },
        Row {
            case: "a dual-layer SSE-KMS default with the Bucket Key off",
            bucket: "members-dsse-kms",
            configured: Some(vec![Rule::by_default(dual_layer_kms).with_bucket_key(false)]),
        },
        Row {
            case: "a rule naming only the Bucket Key",
            bucket: "members-bucket-key-only",
            configured: Some(vec![Rule::bucket_key_only(true)]),
        },
        Row {
            case: "a rule that blocks the customer-key type",
            bucket: "members-blocking-sse-c",
            configured: Some(vec![Rule::by_default(sse_s3).blocking(&[customer_key])]),
        },
        Row {
            case: "a rule that names the unblocking value",
            bucket: "members-blocking-none",
            configured: Some(vec![Rule::by_default(kms).blocking(&[unblocked])]),
        },
        Row {
            case: "a rule that names every published encryption type",
            bucket: "members-blocking-both",
            configured: Some(vec![
                Rule::by_default(sse_s3).blocking(&ENCRYPTION_TYPE_VALUES)
            ]),
        },
        Row {
            case: "two rules, only the second of which blocks",
            bucket: "members-two-rules",
            configured: Some(vec![
                Rule::by_default(kms).with_key(KEY_ARN),
                Rule::bucket_key_only(true).blocking(&[customer_key]),
            ]),
        },
    ]
}

#[tokio::test]
async fn a_bucket_reports_the_encryption_members_its_configuration_established() {
    let router = router();
    for row in rows() {
        bucket(&router, row.bucket, row.case).await;
        if let Some(rules) = &row.configured {
            let put = put_encryption(&router, row.bucket, rules).await;
            assert_eq!(
                put.status, 200,
                "case {:?}: PutBucketEncryption must answer 200, got {} with body {:?}",
                row.case,
                put.status,
                text(&put)
            );
        }
        assert_reports(&router, row.bucket, row.case, &expected_of(&row)).await;

        // Reading reports, it does not configure: the second read answers exactly what the
        // first did, so no member appeared because the bucket was read.
        assert_reports(&router, row.bucket, row.case, &expected_of(&row)).await;
    }
}

#[tokio::test]
async fn a_blocked_set_a_put_named_is_replaced_by_the_one_in_force() {
    let router = router();
    let [unblocked, customer_key] = ENCRYPTION_TYPE_VALUES;
    let name = "members-block-withdrawn";
    let case = "a blocked set named, then named otherwise and reset";
    bucket(&router, name, case).await;

    // Established: the put names the block, so the bucket reports it.
    let blocking = vec![Rule::by_default(DEFAULT_SSE_ALGORITHM).blocking(&[customer_key])];
    let put = put_encryption(&router, name, &blocking).await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: the blocking put must answer 200, got {} with body {:?}",
        put.status,
        text(&put)
    );
    assert_reports(
        &router,
        name,
        "the blocking put is reported",
        &[Rule::by_default(DEFAULT_SSE_ALGORITHM)
            .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
            .blocking(&[customer_key])],
    )
    .await;

    // A put is a REPLACEMENT of the rules, and a rule that names NO block is in force with
    // the set the bucket carries anyway — the bucket-level setting is not withdrawn by a put
    // that says nothing about it, while a put that names the unblocking value IS the bucket's
    // set.
    let replacement = vec![Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3])];
    let replaced = put_encryption(&router, name, &replacement).await;
    assert_eq!(
        replaced.status, 200,
        "case {case:?}: the replacing put must answer 200, got {} with body {:?}",
        replaced.status,
        text(&replaced)
    );
    assert_reports(
        &router,
        name,
        "a replacement that names no block",
        &[Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3])
            .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
            .blocking(&[customer_key])],
    )
    .await;

    let unblocking = vec![Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3]).blocking(&[unblocked])];
    let unblocked_put = put_encryption(&router, name, &unblocking).await;
    assert_eq!(
        unblocked_put.status, 200,
        "case {case:?}: the unblocking put must answer 200, got {} with body {:?}",
        unblocked_put.status,
        text(&unblocked_put)
    );
    assert_reports(
        &router,
        name,
        "a put that names the unblocking value",
        &[Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3])
            .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
            .blocking(&[unblocked])],
    )
    .await;

    // And a reset puts the bucket back where `CreateBucket` left it.
    let reset = delete_encryption(&router, name).await;
    assert_eq!(
        reset.status, 204,
        "case {case:?}: DeleteBucketEncryption must answer the modelled 204, got {} with \
         body {:?}",
        reset.status,
        text(&reset)
    );
    assert_reports(&router, name, "the bucket after a reset", &[base_default()]).await;
}
