// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCES S3 refuses a bucket configuration write with.
//!
//! The rule under test: where S3 publishes its own sentence for a refusal, the refusal carries
//! THAT sentence, with only the parts the published wording takes from the request filled in.
//! Three sentences of the id-addressed bucket configuration families are pinned here, each
//! written out in this file rather than read from the crate so that a change to the wording
//! fails this suite:
//!
//! * The per-bucket CONFIGURATION LIMIT. "You can have up to 1,000 S3 Intelligent-Tiering
//!   configurations per bucket" (`PutBucketIntelligentTieringConfiguration`'s own
//!   documentation), and the same ceiling for the analytics, S3 Inventory and metrics families
//!   — one code (`TooManyConfigurations`), one status (400) and ONE sentence for all four, with
//!   nothing of the request in it: neither the id the put asked for nor which family it
//!   addressed.
//! * The id LENGTH bound (`InvalidConfigId`, 400) on each family whose operations publish that
//!   code: every one of those sentences states the bound rather than the id that broke it, and
//!   each is the sentence THAT family answers on the wire — the analytics and S3
//!   Intelligent-Tiering sub-resources word the one rule differently, so the wording is pinned
//!   per family. The metrics and S3 Inventory families publish no such code and report the same
//!   id rule through their own class, which is their own operations' business and not these
//!   sentences'.
//! * The UNIQUENESS of an S3 Intelligent-Tiering configuration's access tiers
//!   (`InvalidAccessTier`, 400): fixed text except the tier the document repeated, which is the
//!   one part of it that comes from the request.
//!
//! The tables span what each rule generalises over rather than one recorded call: every family
//! that shares the limit sentence; several lengths past the bound, spelled across the whole id
//! character class, on every family that publishes that code; and a duplicate of each
//! `IntelligentTieringAccessTier` value, with the repeat adjacent to its twin and separated from
//! it, so the sentence is not keyed to one tier or to one ordering.

use service_com_amazonaws_s3_amazons3::state::{
    INVALID_ACCESS_TIER_CODE, INVALID_CONFIG_ID_CODE, MAX_CONFIGURATION_ID_CHARS,
    TOO_MANY_CONFIGURATIONS_CODE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, Answer};

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The sentence S3 sends beside `TooManyConfigurations`, written out here rather than read from
/// the crate so the wording has something to be wrong about. ENTIRELY fixed text: the number in
/// it is the published ceiling and nothing else of it comes from the request.
const LIMIT_MESSAGE: &str = "You have attempted to create more configurations than the 1000 allowed.";

/// The sentence S3 sends beside `InvalidConfigId` for an id longer than the bound on the
/// ANALYTICS sub-resource, likewise written out: it states the bound and echoes nothing of the
/// id.
const ANALYTICS_INVALID_ID_MESSAGE: &str =
    "Config Id should not be more than 1 char and less than 64 chars in length";

/// The same refusal's sentence on the S3 INTELLIGENT-TIERING sub-resource, which S3 words in
/// full words and ends in a full stop: one rule, one code, two published sentences.
const INTELLIGENT_TIERING_INVALID_ID_MESSAGE: &str =
    "Config Id should be more than 1 character and less than 64 characters in length.";

/// The two values `IntelligentTieringAccessTier` publishes
/// (`smithy_query(query="shape",shape="IntelligentTieringAccessTier")`), and the fewest days a
/// tiering may name for each (`Tiering$Days`: "the minimum number of days specified for Archive
/// Access tier must be at least 90 days and Deep Archive Access tier must be at least 180
/// days").
const ACCESS_TIERS: [(&str, i32); 2] = [("ARCHIVE_ACCESS", 90), ("DEEP_ARCHIVE_ACCESS", 180)];

/// The published per-bucket ceiling on configurations of ONE family, which the sentence states.
const CONFIGURATIONS_PER_BUCKET: usize = 1_000;

/// One id-addressed configuration family: the subresource query parameter that names it, and a
/// document its own `PUT` accepts under any id — so a refusal of such a call is about the id or
/// the count rather than about the document.
struct Family {
    case: &'static str,
    subresource: &'static str,
    /// The document for one id, which has to carry that id as its own `Id` member.
    document: fn(&str) -> String,
    /// The sentence this family's `PUT` answers `InvalidConfigId` with for an id outside the
    /// length bound, or `None` when it publishes no such code: the metrics and S3 Inventory
    /// operations report the same rule through their own class, so neither sentence is theirs
    /// to carry.
    invalid_config_id_message: Option<&'static str>,
}

/// The four families a bucket holds per-id configurations of. Each document is the minimal one
/// that shape requires (`smithy_query(query="shape",shape="MetricsConfiguration")` and its
/// siblings).
const FAMILIES: [Family; 4] = [
    Family {
        case: "analytics",
        subresource: "analytics",
        document: analytics_document,
        invalid_config_id_message: Some(ANALYTICS_INVALID_ID_MESSAGE),
    },
    Family {
        case: "metrics",
        subresource: "metrics",
        document: metrics_document,
        invalid_config_id_message: None,
    },
    Family {
        case: "S3 Inventory",
        subresource: "inventory",
        document: inventory_document,
        invalid_config_id_message: None,
    },
    Family {
        case: "S3 Intelligent-Tiering",
        subresource: "intelligent-tiering",
        document: intelligent_tiering_document,
        invalid_config_id_message: Some(INTELLIGENT_TIERING_INVALID_ID_MESSAGE),
    },
];

fn analytics_document(id: &str) -> String {
    format!(
        "<AnalyticsConfiguration xmlns=\"{XMLNS}\"><Id>{id}</Id><StorageClassAnalysis>\
         </StorageClassAnalysis></AnalyticsConfiguration>"
    )
}

fn metrics_document(id: &str) -> String {
    format!("<MetricsConfiguration xmlns=\"{XMLNS}\"><Id>{id}</Id></MetricsConfiguration>")
}

fn inventory_document(id: &str) -> String {
    format!(
        "<InventoryConfiguration xmlns=\"{XMLNS}\"><Destination><S3BucketDestination>\
         <Bucket>arn:aws:s3:::configuration-wording-destination</Bucket><Format>CSV</Format>\
         </S3BucketDestination></Destination><IsEnabled>true</IsEnabled><Id>{id}</Id>\
         <IncludedObjectVersions>All</IncludedObjectVersions><Schedule><Frequency>Daily\
         </Frequency></Schedule></InventoryConfiguration>"
    )
}

fn intelligent_tiering_document(id: &str) -> String {
    let (tier, days) = ACCESS_TIERS[0];
    format!(
        "<IntelligentTieringConfiguration xmlns=\"{XMLNS}\"><Id>{id}</Id><Status>Enabled</Status>\
         <Tiering><Days>{days}</Days><AccessTier>{tier}</AccessTier></Tiering>\
         </IntelligentTieringConfiguration>"
    )
}

/// `PUT /<bucket>?<subresource>&id=<id>` with the family's own document for that id.
async fn put_configuration(
    router: &MultiServiceRouter,
    family: &Family,
    bucket: &str,
    id: &str,
) -> Answer {
    send(
        router,
        "PUT",
        &format!("/{bucket}?{}&id={id}", family.subresource),
        &[],
        &(family.document)(id),
    )
    .await
}

/// A bucket this emulator holds, created through `CreateBucket`.
async fn bucket(router: &MultiServiceRouter, name: &str) {
    let created = create_bucket(router, name, None, None).await;
    assert!(
        (200..300).contains(&created.status),
        "the bucket {name:?} must be created first, got {} ({})",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
}

/// RULE: the refusal of a put that would take a bucket past the per-family configuration
/// ceiling carries ONE published sentence, whichever family it addressed and whatever id it
/// asked for.
///
/// The rows are the four families that share the ceiling; each fills its own bucket to the
/// published count and then asks for one more NEW id, which creates a configuration and so is
/// what the ceiling refuses. A REPLACEMENT at the ceiling creates none and is still recorded,
/// which is the counter-case that keeps the refusal about the COUNT.
#[tokio::test]
async fn the_configuration_limit_refusal_carries_the_published_sentence_on_every_family() {
    let router = router();

    for family in &FAMILIES {
        let case = family.case;
        let name = format!("configuration-limit-{}", family.subresource);
        bucket(&router, &name).await;

        for index in 0..CONFIGURATIONS_PER_BUCKET {
            let id = format!("configuration-{index}");
            let answer = put_configuration(&router, family, &name, &id).await;
            assert!(
                (200..300).contains(&answer.status),
                "case {case:?}/{id}: a configuration within the ceiling must be recorded, got \
                 {} ({})",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
        }

        for asked in ["one-too-many", "another-new-id"] {
            let past = put_configuration(&router, family, &name, asked).await;
            assert_eq!(
                past.status, 400,
                "case {case:?}/{asked}: expected HTTP 400, got {} ({})",
                past.status,
                String::from_utf8_lossy(&past.body)
            );
            assert_eq!(
                past.error_code(),
                TOO_MANY_CONFIGURATIONS_CODE,
                "case {case:?}/{asked}: expected {TOO_MANY_CONFIGURATIONS_CODE}, got {}",
                past.error_code()
            );
            assert_eq!(
                past.error_message(),
                LIMIT_MESSAGE,
                "case {case:?}/{asked}: expected {LIMIT_MESSAGE:?}, got {:?}",
                past.error_message()
            );
        }

        // A put under an id the bucket already carries replaces that configuration: it creates
        // none, so the ceiling does not refuse it.
        let replaced = put_configuration(&router, family, &name, "configuration-0").await;
        assert!(
            (200..300).contains(&replaced.status),
            "case {case:?}: a replacement at the ceiling must be recorded, got {} ({})",
            replaced.status,
            String::from_utf8_lossy(&replaced.body)
        );
    }
}

/// RULE: the refusal of a configuration id longer than the published bound carries the
/// sentence that family's own sub-resource publishes, which states the bound and nothing of
/// the id — on every family whose operation publishes that code, and whatever the id's length
/// or spelling.
///
/// The lengths are one past the bound, far past it, and one past it spelled across the whole id
/// character class. An id AT the bound is recorded, which is what keeps the refusal about the
/// length rather than about long ids generally.
#[tokio::test]
async fn an_over_long_configuration_id_is_refused_with_the_published_sentence() {
    let router = router();

    for (family, message) in FAMILIES
        .iter()
        .filter_map(|family| family.invalid_config_id_message.map(|text| (family, text)))
    {
        let case = family.case;
        let name = format!("configuration-id-length-{}", family.subresource);
        bucket(&router, &name).await;

        // Exactly at the bound: recorded, so the refusals below are about being PAST it.
        let at_the_bound = "i".repeat(MAX_CONFIGURATION_ID_CHARS);
        let accepted = put_configuration(&router, family, &name, &at_the_bound).await;
        assert!(
            (200..300).contains(&accepted.status),
            "case {case:?}: an id at the published bound must be recorded, got {} ({})",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );

        for (length_case, id) in [
            (
                "one character past the bound",
                "i".repeat(MAX_CONFIGURATION_ID_CHARS + 1),
            ),
            (
                "far past the bound",
                "i".repeat(MAX_CONFIGURATION_ID_CHARS * 3),
            ),
            (
                "one character past the bound, spelled across the whole id character class",
                // Every character a configuration id may carry (letters, digits, dashes,
                // underscores and periods), so the refusal is about the LENGTH rather than
                // about the spelling — an id of the same characters at the bound is recorded
                // above.
                "aA0-_."
                    .chars()
                    .cycle()
                    .take(MAX_CONFIGURATION_ID_CHARS + 1)
                    .collect::<String>(),
            ),
        ] {
            let refused = put_configuration(&router, family, &name, &id).await;
            assert_eq!(
                refused.status, 400,
                "case {case:?}/{length_case}: expected HTTP 400, got {} ({})",
                refused.status,
                String::from_utf8_lossy(&refused.body)
            );
            assert_eq!(
                refused.error_code(),
                INVALID_CONFIG_ID_CODE,
                "case {case:?}/{length_case}: expected {INVALID_CONFIG_ID_CODE}, got {}",
                refused.error_code()
            );
            assert_eq!(
                refused.error_message(),
                message,
                "case {case:?}/{length_case}: expected {message:?}, got {:?}",
                refused.error_message()
            );
        }
    }
}

/// One tiering of an S3 Intelligent-Tiering configuration, as a request spells it.
fn tiering(tier: &str, days: i32) -> String {
    format!("<Tiering><Days>{days}</Days><AccessTier>{tier}</AccessTier></Tiering>")
}

/// A whole `IntelligentTieringConfiguration` carrying `tierings` under `id`.
fn tiered_document(id: &str, tierings: &str) -> String {
    format!(
        "<IntelligentTieringConfiguration xmlns=\"{XMLNS}\"><Id>{id}</Id><Status>Enabled</Status>\
         {tierings}</IntelligentTieringConfiguration>"
    )
}

/// RULE: two tierings naming the same access tier describe no plan the service can carry out,
/// and the refusal states that rule in S3's own prose while naming the ONE tier the document
/// repeated — the only part of the sentence that comes from the request.
///
/// The rows differ along which tier is repeated (each of the two the enum publishes), how many
/// tierings the document carries, and whether the repeat sits beside its twin or is separated
/// from it by another tiering — so neither the sentence nor the decision depends on the order
/// the document listed them in.
#[tokio::test]
async fn a_duplicate_access_tier_is_refused_with_the_published_sentence() {
    let router = router();
    let name = "intelligent-tiering-duplicate-tiers";
    bucket(&router, name).await;

    let (archive, archive_days) = ACCESS_TIERS[0];
    let (deep, deep_days) = ACCESS_TIERS[1];

    let rows: [(&str, String, &str); 4] = [
        (
            "the warmer tier twice, adjacent",
            format!(
                "{}{}",
                tiering(archive, archive_days),
                tiering(archive, archive_days + 10)
            ),
            archive,
        ),
        (
            "the deeper tier twice, adjacent",
            format!(
                "{}{}",
                tiering(deep, deep_days),
                tiering(deep, deep_days + 10)
            ),
            deep,
        ),
        (
            "the warmer tier repeated with the deeper one between",
            format!(
                "{}{}{}",
                tiering(archive, archive_days),
                tiering(deep, deep_days),
                tiering(archive, archive_days + 20)
            ),
            archive,
        ),
        (
            "the deeper tier repeated after the warmer one",
            format!(
                "{}{}{}",
                tiering(deep, deep_days),
                tiering(archive, archive_days),
                tiering(deep, deep_days + 20)
            ),
            deep,
        ),
    ];

    for (case, tierings, repeated) in &rows {
        let id = "duplicate-tiers";
        let refused = send(
            &router,
            "PUT",
            &format!("/{name}?intelligent-tiering&id={id}"),
            &[],
            &tiered_document(id, tierings),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {} ({})",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_ACCESS_TIER_CODE,
            "case {case:?}: expected {INVALID_ACCESS_TIER_CODE}, got {}",
            refused.error_code()
        );
        let expected = format!(
            "All Access Tiers in a configuration must be unique. Found duplicate entry for \
             {repeated}."
        );
        assert_eq!(
            refused.error_message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            refused.error_message()
        );
        // Nothing was recorded: the configuration the refused put named is not on the bucket.
        let read = send(
            &router,
            "GET",
            &format!("/{name}?intelligent-tiering&id={id}"),
            &[],
            "",
        )
        .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: the refused configuration must not be recorded, got {} ({})",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }

    // The counter-case: one tiering per tier is a plan the service can carry out, so it is
    // recorded and the sentence above is not reached.
    let id = "unique-tiers";
    let stored = send(
        &router,
        "PUT",
        &format!("/{name}?intelligent-tiering&id={id}"),
        &[],
        &tiered_document(
            id,
            &format!(
                "{}{}",
                tiering(archive, archive_days),
                tiering(deep, deep_days)
            ),
        ),
    )
    .await;
    assert!(
        (200..300).contains(&stored.status),
        "case \"one tiering per tier\": the configuration must be recorded, got {} ({})",
        stored.status,
        String::from_utf8_lossy(&stored.body)
    );
}
