// SPDX-License-Identifier: CC0-1.0

//! Which class `PutTableBucketEncryption` refuses an SSE-KMS configuration in,
//! and that each class is the right one.
//!
//! RULE: an operation that SETS a table bucket's encryption serves a
//! configuration naming the KMS algorithm only when the `kmsKeyArn` it carries is
//! of the one key-ARN form the model declares AND names a key KMS resolves for
//! the Region the request was signed for — the model requires the service be
//! granted access to that key ("if you choose SSE-KMS encryption you must grant
//! the S3 Tables maintenance principal access to your KMS key",
//! `smithy_query(query="doc", shape="PutTableBucketEncryption")`), and
//! `kmsKeyArn` is `@length(1, 2048)` with
//! `@pattern(^(arn:aws[-a-z0-9]*:kms:[-a-z0-9]*:[0-9]{12}:key/.+)$)` while
//! `sseAlgorithm` is `@required` and targets `SSEAlgorithm` (`AES256`, `aws:kms`)
//! (`smithy_query(query="shape", shape="EncryptionConfiguration,SSEAlgorithm")`).
//! The two failures are DIFFERENT classes and are answered as such:
//!
//! * a value outside that form — absent where the algorithm requires one, empty,
//!   or ARN-shaped in a way the pattern does not admit — is the modelled
//!   `BadRequestException` (HTTP 400) carrying the service's single fixed
//!   invalid-key-ARN sentence, which interpolates nothing;
//! * a value inside the form that KMS answers the caller nothing about is the
//!   modelled `ForbiddenException` (HTTP 403) carrying KMS's own sentence for an
//!   authorization failure it can report nothing about the resource with, whose
//!   only variable part is the principal the request speaks for — what
//!   `sts:GetCallerIdentity` reports for the caller.
//!
//! Either refusal is effect-free: the bucket keeps the configuration it already
//! carried, and a configuration the model does allow still applies afterwards.
//!
//! The rows span what the rule generalises over rather than one recorded
//! request: WHERE the form is missed (the member itself, its length, the ARN's
//! resource type, the absence of an ARN prefix on a value KMS itself would
//! accept) and WHY the key does not resolve (another partition's Region, this
//! account and Region with an id nothing minted, another 12-digit account,
//! another Region of this partition) — plus the served rows that keep the
//! refusals honest, under keys created through KMS's OWN `CreateKey` over the
//! same engine.

use serde_json::{json, Map, Value};

use crate::harness::Emulator;
use crate::table_bucket_encryption::reported;

/// The Region the subject buckets live in.
const REGION: &str = "us-east-1";

/// Another Region of the same partition, for the rows about a key the request's
/// Region does not serve, and for the sentences read a second time.
const OTHER_REGION: &str = "eu-west-1";

/// The modelled error codes of the two refusal classes.
const BAD_REQUEST: &str = "BadRequestException";
const FORBIDDEN: &str = "ForbiddenException";

/// The service's fixed invalid-key-ARN sentence, spelled out here rather than
/// read from the crate so this suite pins the wording itself.
const INVALID_KMS_KEY_ARN: &str = "The specified KMS key ARN is invalid. Only key ARN format \
                                   'arn:<partition>:kms:<region>:<account-id>:key/<key-id>' is \
                                   supported.";

/// The format template that sentence spells out, which is fixed text rather than
/// anything taken from the request.
const KEY_ARN_TEMPLATE: &str = "'arn:<partition>:kms:<region>:<account-id>:key/<key-id>'";

/// A 12-digit account id that is not this emulator's — the form the modelled
/// `kmsKeyArn` pattern requires of that segment (`[0-9]{12}`).
const FOREIGN_ACCOUNT: &str = "444455556666";

/// KMS's sentence for an authorization failure it can report nothing about the
/// resource with, spelled out here too. `{principal}` is its only variable part,
/// and `kms:DescribeKey` is the action that resolves a key identifier.
fn access_sentence(principal: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: kms:DescribeKey on this resource \
         because the resource does not exist in this Region, no resource-based policies allow \
         access, or a resource-based policy explicitly denies access"
    )
}

/// What the service must answer for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The configuration is storable: HTTP 200, and the read reports it.
    Served,
    /// The key names no ARN of the modelled form: the 400 class.
    OutsideTheForm,
    /// The key is of that form but KMS resolves nothing for it: the 403 class.
    Unresolvable,
}

/// An `encryptionConfiguration` naming `algorithm`, and `key` when the row
/// carries one.
fn configuration(algorithm: &str, key: Option<&str>) -> Value {
    let mut members = Map::new();
    members.insert("sseAlgorithm".to_string(), json!(algorithm));
    if let Some(key) = key {
        members.insert("kmsKeyArn".to_string(), json!(key));
    }
    Value::Object(members)
}

/// A table bucket in `region`, by its ARN.
async fn bucket(emulator: &Emulator, region: &str, name: &str) -> String {
    let created = emulator
        .create_table_bucket(region, &json!({ "name": name }))
        .await;
    assert_eq!(
        created.status, 200,
        "the bucket {name:?} must be created, got {status} {body}",
        status = created.status,
        body = created.body
    );
    created.text("arn")
}

/// The configuration `arn` reports, as `(sseAlgorithm, kmsKeyArn)`.
async fn reads_as(
    emulator: &Emulator,
    region: &str,
    arn: &str,
    case: &str,
) -> (String, Option<String>) {
    let read = emulator.get_table_bucket_encryption(region, arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the encryption read must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    reported(&read.body, case)
}

#[tokio::test]
async fn the_refusal_class_follows_the_key_arns_form_and_then_kmss_answer() {
    let emulator = Emulator::new();
    let arn = bucket(&emulator, REGION, "kms-refusal-classes").await;
    let principal = emulator.caller_identity_arn(REGION).await;

    // The keys the rows name, each minted by KMS itself.
    let resident = emulator.create_kms_key(REGION).await;
    let second_resident = emulator.create_kms_key(REGION).await;
    let elsewhere = emulator.create_kms_key(OTHER_REGION).await;
    // The resident key's own key id, with the ARN stripped off: KMS's own
    // `KeyId` member accepts a bare key id, and the modelled `kmsKeyArn` pattern
    // does not — so the form is decided before KMS is asked anything.
    let bare_key_id = resident
        .rsplit_once('/')
        .map(|(_, key_id)| key_id.to_string())
        .expect("a key ARN carries a key id");

    // The bucket carries a configuration the model allows BEFORE the refusals,
    // so each refused put is shown to leave the previous one rather than an
    // absence.
    let applied = emulator
        .put_table_bucket_encryption(REGION, &arn, &configuration("aws:kms", Some(&resident)))
        .await;
    assert_eq!(
        applied.status, 200,
        "the first put must be served, got {status} {body}",
        status = applied.status,
        body = applied.body
    );

    // (case, the `kmsKeyArn` the put carries, what the service must answer)
    let rows: Vec<(&str, Option<String>, Expected)> = vec![
        // Outside the modelled form, in each place the form can be missed.
        ("no kmsKeyArn member at all", None, Expected::OutsideTheForm),
        (
            "an empty kmsKeyArn",
            Some(String::new()),
            Expected::OutsideTheForm,
        ),
        (
            "a bare key id KMS itself would accept",
            Some(bare_key_id.clone()),
            Expected::OutsideTheForm,
        ),
        (
            "an alias name KMS itself would accept",
            Some("alias/s3-tables".to_string()),
            Expected::OutsideTheForm,
        ),
        (
            "an ARN naming the alias resource type",
            Some(format!("arn:aws:kms:{REGION}:000000000000:alias/s3-tables")),
            Expected::OutsideTheForm,
        ),
        (
            "an ARN with nothing after key/",
            Some(format!("arn:aws:kms:{REGION}:000000000000:key/")),
            Expected::OutsideTheForm,
        ),
        // Inside the form, naming a key KMS resolves nothing for.
        (
            "a key ARN in another partition's Region",
            Some(
                "arn:aws-cn:kms:cn-north-1:000000000000:key/\
                 2f6b9a1c-7d84-4e35-91ab-0c5d8e7f6a2b"
                    .to_string(),
            ),
            Expected::Unresolvable,
        ),
        (
            "a key ARN in the GovCloud partition",
            Some(
                "arn:aws-us-gov:kms:us-gov-west-1:000000000000:key/\
                 8c3d5e71-24fb-4a09-b6de-1f20a4c8795e"
                    .to_string(),
            ),
            Expected::Unresolvable,
        ),
        (
            "this account and Region, with a key id nothing minted",
            Some(format!(
                "arn:aws:kms:{REGION}:000000000000:key/no-key-was-ever-minted-under-this-id"
            )),
            Expected::Unresolvable,
        ),
        (
            "another 12-digit account's key ARN",
            Some(resident.replace("000000000000", FOREIGN_ACCOUNT)),
            Expected::Unresolvable,
        ),
        (
            "a key of this account minted in another Region",
            Some(elsewhere.clone()),
            Expected::Unresolvable,
        ),
        // And the rows the model does allow, so the refusals above are about the
        // key rather than about SSE-KMS.
        (
            "a second key KMS minted in this Region",
            Some(second_resident.clone()),
            Expected::Served,
        ),
        (
            "the key the bucket already carries",
            Some(resident.clone()),
            Expected::Served,
        ),
    ];

    // The configuration the bucket carries, which only a served row changes.
    let mut carried = resident.clone();

    for (case, key, expected) in &rows {
        let put = emulator
            .put_table_bucket_encryption(REGION, &arn, &configuration("aws:kms", key.as_deref()))
            .await;
        match expected {
            Expected::Served => {
                assert_eq!(
                    put.status, 200,
                    "case {case:?}: the put must be served, got {status} {body}",
                    status = put.status,
                    body = put.body
                );
                carried = key
                    .clone()
                    .expect("a served row of this table names a key");
            }
            Expected::OutsideTheForm => {
                assert_eq!(
                    (put.status, put.error_code.as_deref()),
                    (400, Some(BAD_REQUEST)),
                    "case {case:?}: expected a 400 {BAD_REQUEST}, got {status} {body}",
                    status = put.status,
                    body = put.body
                );
                assert_eq!(
                    put.message(),
                    INVALID_KMS_KEY_ARN,
                    "case {case:?}: expected the fixed invalid-key-ARN sentence, got {actual:?}",
                    actual = put.message()
                );
                assert!(
                    put.message().contains(KEY_ARN_TEMPLATE),
                    "case {case:?}: the sentence must spell out {KEY_ARN_TEMPLATE}, got {actual:?}",
                    actual = put.message()
                );
            }
            Expected::Unresolvable => {
                assert_eq!(
                    (put.status, put.error_code.as_deref()),
                    (403, Some(FORBIDDEN)),
                    "case {case:?}: expected a 403 {FORBIDDEN}, got {status} {body}",
                    status = put.status,
                    body = put.body
                );
                assert_eq!(
                    put.message(),
                    access_sentence(&principal),
                    "case {case:?}: expected KMS's sentence about the caller's own principal, \
                     got {actual:?}",
                    actual = put.message()
                );
            }
        }

        // Neither sentence takes anything from the request.
        if let Some(sent) = key.as_deref().filter(|value| value.chars().count() >= 3) {
            assert!(
                !put.message().contains(sent),
                "case {case:?}: the answer must not echo the request's {sent:?}, got {actual:?}",
                actual = put.message()
            );
        }

        // A refused put changed nothing, and a served one stored what it named.
        let (algorithm, stored) = reads_as(&emulator, REGION, &arn, case).await;
        assert_eq!(
            (algorithm.as_str(), stored.as_deref()),
            ("aws:kms", Some(carried.as_str())),
            "case {case:?}: the read must report the last configuration a served put stored, \
             got {algorithm:?}/{stored:?}"
        );
    }
}

#[tokio::test]
async fn neither_sentence_varies_with_the_region_or_what_the_bucket_carries() {
    let emulator = Emulator::new();

    // The prior state the refused puts meet: no configuration of the bucket's
    // own, the S3-managed default set explicitly, and an SSE-KMS configuration
    // under a key of the row's own Region.
    #[derive(Debug, Clone, Copy)]
    enum Prior {
        None,
        Managed,
        Kms,
    }

    for (index, region) in [REGION, OTHER_REGION].into_iter().enumerate() {
        let principal = emulator.caller_identity_arn(region).await;
        // A key of THIS Region, so the prior SSE-KMS state is storable here.
        let held = emulator.create_kms_key(region).await;

        for (step, prior) in [Prior::None, Prior::Managed, Prior::Kms]
            .into_iter()
            .enumerate()
        {
            let case = format!("{region} with a prior state of {prior:?}");
            let arn = bucket(&emulator, region, &format!("kms-sentences-{index}-{step}")).await;

            // Whatever the bucket carries going in, and what the reads below
            // must keep reporting.
            let expected_state: (&str, Option<&str>) = match prior {
                Prior::None => ("AES256", None),
                Prior::Managed => ("AES256", None),
                Prior::Kms => ("aws:kms", Some(held.as_str())),
            };
            let configured = match prior {
                Prior::None => None,
                Prior::Managed => Some(configuration("AES256", None)),
                Prior::Kms => Some(configuration("aws:kms", Some(&held))),
            };
            if let Some(configured) = &configured {
                let applied = emulator
                    .put_table_bucket_encryption(region, &arn, configured)
                    .await;
                assert_eq!(
                    applied.status, 200,
                    "case {case:?}: the prior put must be served, got {status} {body}",
                    status = applied.status,
                    body = applied.body
                );
            }

            // The form class: the same fixed sentence, wherever the request was
            // signed and whatever the bucket carries.
            let missing = emulator
                .put_table_bucket_encryption(region, &arn, &configuration("aws:kms", None))
                .await;
            assert_eq!(
                (missing.status, missing.error_code.as_deref()),
                (400, Some(BAD_REQUEST)),
                "case {case:?}: expected a 400 {BAD_REQUEST} for a missing key, got {status} \
                 {body}",
                status = missing.status,
                body = missing.body
            );
            assert_eq!(
                missing.message(),
                INVALID_KMS_KEY_ARN,
                "case {case:?}: expected the fixed invalid-key-ARN sentence, got {actual:?}",
                actual = missing.message()
            );

            // The access class: KMS's sentence, naming the caller's own
            // principal and nothing of the key it was asked about.
            let foreign = held.replace("000000000000", FOREIGN_ACCOUNT);
            let refused = emulator
                .put_table_bucket_encryption(
                    region,
                    &arn,
                    &configuration("aws:kms", Some(&foreign)),
                )
                .await;
            assert_eq!(
                (refused.status, refused.error_code.as_deref()),
                (403, Some(FORBIDDEN)),
                "case {case:?}: expected a 403 {FORBIDDEN} for a key KMS answers nothing \
                 about, got {status} {body}",
                status = refused.status,
                body = refused.body
            );
            assert_eq!(
                refused.message(),
                access_sentence(&principal),
                "case {case:?}: expected KMS's sentence about the caller's own principal, got \
                 {actual:?}",
                actual = refused.message()
            );

            // Both refusals wrote nothing: the bucket still reports what it
            // carried going in.
            let (algorithm, key) = reads_as(&emulator, region, &arn, &case).await;
            assert_eq!(
                (algorithm.as_str(), key.as_deref()),
                expected_state,
                "case {case:?}: the refusals must leave the bucket's configuration as it was, \
                 got {algorithm:?}/{key:?}"
            );

            // And the positive control: a configuration the model allows still
            // applies to the same bucket afterwards.
            let served = emulator
                .put_table_bucket_encryption(region, &arn, &configuration("AES256", None))
                .await;
            assert_eq!(
                served.status, 200,
                "case {case:?}: the following AES256 put must be served, got {status} {body}",
                status = served.status,
                body = served.body
            );
            let (algorithm, key) = reads_as(&emulator, region, &arn, &case).await;
            assert_eq!(
                (algorithm.as_str(), key.as_deref()),
                ("AES256", None),
                "case {case:?}: the read must report what the served put stored, got \
                 {algorithm:?}/{key:?}"
            );
        }
    }
}
