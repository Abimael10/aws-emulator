// SPDX-License-Identifier: CC0-1.0

//! `TagResource`: the tag WRITE, over the wire.
//!
//! A child suite of `brand_profile_lifecycle`, which owns the tag READ and the ARN
//! resolution every tagging call shares; this file owns what a write does to the stored
//! tags and which tag sets no request may carry. Declared by its parent, because this
//! crate's one test target declares the suites directly under `tests/all/`.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a write merges its pairs into the tags the resource already carries — a key the
//!   resource holds takes the new value, a key it does not hold is added, and every
//!   other key is left alone — for every taggable resource type of this service, and
//!   `ListTagsForResource` reports the merged set afterwards;
//! * a tag key is unique per resource and case-sensitive (`TagKey`'s own
//!   documentation), so two keys differing only in case are two tags;
//! * a request naming a tag key inside the `aws:` namespace AWS reserves for its own
//!   tags, in any casing, is refused with `ValidationException` (400) and writes
//!   nothing — wherever that tag set arrived, on a write or on a create;
//! * a request naming one tag key more than once is refused the same way, since it
//!   asks for two values for one key;
//! * a write that would leave a resource carrying more tags than the per-resource
//!   ceiling is refused with `ServiceQuotaExceededException` (400) and writes nothing,
//!   while a write that only overwrites keys already stored is served at the ceiling.

use serde_json::{json, Value};

use service_com_amazonaws_endusermessaging_endusermessaging::state::{
    MAX_TAGS_PER_RESOURCE, RESERVED_TAG_KEY_PREFIX,
};

use super::{reported_tags, sorted, tag_body, Emulator, Reply};

/// `TagResource`. `resourceArn` is a non-greedy `@httpLabel`, so the ARN travels
/// percent-encoded as one path segment, the way an SDK client sends it.
async fn tag(em: &Emulator, resource_arn: &str, tags: &[(String, String)]) -> Reply {
    tag_raw(em, resource_arn, tag_body(tags)).await
}

/// `TagResource` with the `tags` member exactly as given, for a list no pair vector can
/// express (one that names a key twice).
async fn tag_raw(em: &Emulator, resource_arn: &str, tags: Value) -> Reply {
    em.call(
        "POST",
        &format!(
            "/v1/tags/{}",
            crate::brand_profile_default_attributes::path_label(resource_arn)
        ),
        Some(json!({ "tags": tags })),
    )
    .await
}

/// A brand profile carrying `tags`, by its ARN.
async fn tagged_profile(em: &Emulator, name: &str, tags: &[(String, String)]) -> String {
    let created = em
        .create(json!({ "brandProfileName": name, "tags": tag_body(tags) }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating {name} must succeed, got {} / {:?}",
        created.status, created.body
    );
    created.string("brandProfileArn")
}

/// A notify code configuration carrying `tags`, by its ARN: the other taggable
/// resource type of this service.
async fn tagged_configuration(em: &Emulator, name: &str, tags: &[(String, String)]) -> String {
    let created = em
        .create_notify_code_configuration(json!({
            "notifyCodeConfigurationName": name,
            "tags": tag_body(tags),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating {name} must succeed, got {} / {:?}",
        created.status, created.body
    );
    created.member("notifyCodeConfiguration")["notifyCodeConfigurationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reported no ARN, got {:?}", created.body))
        .to_string()
}

/// The `message` a refusal states.
fn refusal_message(reply: &Reply) -> String {
    reply.string("message")
}

/// What a request naming `key` inside the reserved namespace is refused with: the
/// sentence names the prefix and closes with the key the request gave.
fn reserved_key_message(key: &str) -> String {
    format!("Tag key cannot start with '{RESERVED_TAG_KEY_PREFIX}': {key}")
}

/// What a request naming `key` more than once is refused with.
fn duplicate_key_message(key: &str) -> String {
    format!("Duplicate tag key: '{key}'")
}

/// The tags `resource_arn` reports, as a sorted list of pairs.
async fn listed_tags(em: &Emulator, resource_arn: &str) -> Vec<(String, String)> {
    let listed = em.list_tags(resource_arn).await;
    assert_eq!(
        listed.status, 200,
        "reading the tags of {resource_arn} must succeed, got {} / {:?}",
        listed.status, listed.body
    );
    reported_tags(&listed)
}

/// One merge row: the tags the resource is created with, the tags one write asks for,
/// and the set the resource carries afterwards.
struct Merge {
    case: &'static str,
    stored: Vec<(String, String)>,
    written: Vec<(String, String)>,
    expected: Vec<(String, String)>,
}

fn pair(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// The rows span what a merge has to decide: a resource carrying nothing, a key the
/// resource does not hold, a key it does hold (whose value is replaced), a mixture of
/// both in one request, a value replaced by the empty string `TagValue`'s
/// `@length(min: 0)` allows, a key that differs from a stored one only in case — which
/// `TagKey` ("case-sensitive") makes a second tag — and a write of the same pair the
/// resource already carries, which changes nothing.
fn merges() -> Vec<Merge> {
    vec![
        Merge {
            case: "a first tag on an untagged resource",
            stored: Vec::new(),
            written: vec![pair("env", "prod")],
            expected: vec![pair("env", "prod")],
        },
        Merge {
            case: "several tags on an untagged resource",
            stored: Vec::new(),
            written: vec![pair("env", "prod"), pair("team", "messaging")],
            expected: vec![pair("env", "prod"), pair("team", "messaging")],
        },
        Merge {
            case: "a key the resource does not hold",
            stored: vec![pair("env", "prod")],
            written: vec![pair("team", "messaging")],
            expected: vec![pair("env", "prod"), pair("team", "messaging")],
        },
        Merge {
            case: "a key the resource holds, whose value is replaced",
            stored: vec![pair("env", "prod"), pair("team", "messaging")],
            written: vec![pair("env", "staging")],
            expected: vec![pair("env", "staging"), pair("team", "messaging")],
        },
        Merge {
            case: "one key replaced and one added in the same request",
            stored: vec![pair("env", "prod"), pair("team", "messaging")],
            written: vec![pair("env", "staging"), pair("cost-center", "42")],
            expected: vec![
                pair("env", "staging"),
                pair("team", "messaging"),
                pair("cost-center", "42"),
            ],
        },
        Merge {
            case: "a value replaced by the empty string",
            stored: vec![pair("env", "prod")],
            written: vec![pair("env", "")],
            expected: vec![pair("env", "")],
        },
        Merge {
            case: "a key differing from a stored key only in case",
            stored: vec![pair("env", "prod")],
            written: vec![pair("Env", "staging")],
            expected: vec![pair("env", "prod"), pair("Env", "staging")],
        },
        Merge {
            case: "the pair the resource already carries",
            stored: vec![pair("env", "prod")],
            written: vec![pair("env", "prod")],
            expected: vec![pair("env", "prod")],
        },
        Merge {
            case: "a key and a value at their maximum modelled length",
            stored: vec![pair("env", "prod")],
            written: vec![("k".repeat(128), "v".repeat(256))],
            expected: vec![pair("env", "prod"), ("k".repeat(128), "v".repeat(256))],
        },
    ]
}

#[tokio::test]
async fn a_write_merges_its_tags_into_the_resources_own_tags() {
    for merge in merges() {
        let em = Emulator::new();
        // A second, differently tagged profile stands beside each row, so a write that
        // reached the store rather than this resource would fail.
        let neighbour = tagged_profile(&em, "other-profile", &[pair("owner", "other")]).await;
        let arn = tagged_profile(&em, "tagged-profile", &merge.stored).await;

        let written = tag(&em, &arn, &merge.written).await;
        assert_eq!(
            written.status,
            200,
            "case {}: TagResource must succeed, got {} / {:?}",
            merge.case,
            written.status,
            written.body
        );

        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&merge.expected),
            "case {}: unexpected tags after the write",
            merge.case
        );
        assert_eq!(
            listed_tags(&em, &neighbour).await,
            vec![pair("owner", "other")],
            "case {}: a write must leave another resource's tags alone",
            merge.case
        );

        // A repeat of the same write is the same state: the merge is by key, so a key
        // already carrying the written value is not added a second time.
        let again = tag(&em, &arn, &merge.written).await;
        assert_eq!(
            again.status,
            200,
            "case {}: a repeated write must succeed, got {} / {:?}",
            merge.case,
            again.status,
            again.body
        );
        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&merge.expected),
            "case {}: a repeated write must leave the same tags",
            merge.case
        );
    }
}

#[tokio::test]
async fn a_write_reaches_every_taggable_resource_type() {
    // The rule is about the resource an ARN names, not about one resource type, so the
    // notify code configuration answers it exactly as a brand profile does.
    for merge in merges() {
        let em = Emulator::new();
        let arn = tagged_configuration(&em, "tagged-configuration", &merge.stored).await;

        let written = tag(&em, &arn, &merge.written).await;
        assert_eq!(
            written.status,
            200,
            "case {}: TagResource must succeed, got {} / {:?}",
            merge.case,
            written.status,
            written.body
        );
        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&merge.expected),
            "case {}: unexpected tags after the write",
            merge.case
        );
    }
}

/// The keys inside the namespace AWS reserves for the tags it creates itself, which no
/// request may name. The rows vary the casing of the prefix and what follows it: the
/// prefix alone, a name under it, and a name under it in each casing the prefix can
/// take.
fn reserved_keys() -> Vec<String> {
    vec![
        "aws:".to_string(),
        "aws:cost-center".to_string(),
        "AWS:cost-center".to_string(),
        "Aws:CostCenter".to_string(),
        "aWs:a".to_string(),
        format!("aws:{}", "k".repeat(124)),
    ]
}

/// The keys that only RESEMBLE that namespace, and so are ordinary tags: the letters
/// without the colon that makes the prefix, a longer word starting with them, the
/// prefix away from the start of the key, and a key that merely ends with it.
fn keys_outside_the_reserved_namespace() -> Vec<String> {
    vec![
        "aws".to_string(),
        "awsome".to_string(),
        "aws-cost-center".to_string(),
        "my-aws:thing".to_string(),
        "paws:thing".to_string(),
        ":aws".to_string(),
    ]
}

#[tokio::test]
async fn a_write_naming_a_reserved_tag_key_is_refused_and_writes_nothing() {
    let em = Emulator::new();
    let stored = vec![pair("env", "prod")];
    let arn = tagged_profile(&em, "reserved-key-target", &stored).await;

    for key in reserved_keys() {
        // The reserved key alone, and the reserved key beside a key that would
        // otherwise be written: the whole request is refused, so neither lands.
        for (case, tags) in [
            (
                format!("the reserved key {key} alone"),
                vec![(key.clone(), "1".to_string())],
            ),
            (
                format!("the reserved key {key} beside an ordinary key"),
                vec![pair("team", "messaging"), (key.clone(), "1".to_string())],
            ),
        ] {
            let refused = tag(&em, &arn, &tags).await;
            assert_eq!(
                refused.status, 400,
                "case {case}: expected 400, got {} / {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code, got {:?}",
                refused.error
            );
            assert_eq!(
                refusal_message(&refused),
                reserved_key_message(&key),
                "case {case}: unexpected message, got {:?}",
                refused.body
            );
            assert_eq!(
                listed_tags(&em, &arn).await,
                sorted(&stored),
                "case {case}: a refused write must leave the stored tags alone"
            );
        }
    }

    // A key that only resembles the reserved namespace is an ordinary key, and a
    // reserved VALUE is ordinary too: the rule is about keys.
    for key in keys_outside_the_reserved_namespace() {
        let written = tag(&em, &arn, &[(key.clone(), "aws:kms".to_string())]).await;
        assert_eq!(
            written.status, 200,
            "case {key}: a key outside the reserved namespace must be written, got {} / {:?}",
            written.status, written.body
        );
        assert!(
            listed_tags(&em, &arn)
                .await
                .contains(&(key.clone(), "aws:kms".to_string())),
            "case {key}: the written pair must be reported back"
        );
    }
}

#[tokio::test]
async fn a_create_naming_a_reserved_tag_key_is_refused_wherever_tags_arrive() {
    // The rule is about a tag set, not about one operation: every operation of this
    // service that takes `tags` judges it the same way. The brand-profile half also
    // asserts that nothing was stored; the configuration half cannot, because this
    // service's configuration listing is still a scaffold, so the store has no reader
    // that would show a record a refused create had left behind.
    for key in reserved_keys() {
        let em = Emulator::new();
        let tags = tag_body(&[(key.clone(), "1".to_string())]);

        let profile = em
            .create(json!({ "brandProfileName": "reserved-key-create", "tags": tags.clone() }))
            .await;
        assert_eq!(
            profile.status, 400,
            "case {key}: CreateBrandProfile must refuse it, got {} / {:?}",
            profile.status, profile.body
        );
        assert_eq!(
            profile.error.as_deref(),
            Some("ValidationException"),
            "case {key}: unexpected error code, got {:?}",
            profile.error
        );
        assert_eq!(
            refusal_message(&profile),
            reserved_key_message(&key),
            "case {key}: unexpected message, got {:?}",
            profile.body
        );
        let listed = em.list("").await;
        assert_eq!(
            listed.member("brandProfiles"),
            &json!([]),
            "case {key}: a refused create must store no profile, got {:?}",
            listed.body
        );

        let configuration = em
            .create_notify_code_configuration(json!({
                "notifyCodeConfigurationName": "reserved-key-create",
                "tags": tags,
            }))
            .await;
        assert_eq!(
            configuration.status, 400,
            "case {key}: CreateNotifyCodeConfiguration must refuse it, got {} / {:?}",
            configuration.status, configuration.body
        );
        assert_eq!(
            configuration.error.as_deref(),
            Some("ValidationException"),
            "case {key}: unexpected error code, got {:?}",
            configuration.error
        );
        assert_eq!(
            refusal_message(&configuration),
            reserved_key_message(&key),
            "case {key}: unexpected message, got {:?}",
            configuration.body
        );
    }
}

#[tokio::test]
async fn a_request_naming_one_tag_key_twice_is_refused_and_writes_nothing() {
    let em = Emulator::new();
    let stored = vec![pair("env", "prod")];
    let arn = tagged_profile(&em, "duplicate-key-target", &stored).await;

    // The rows vary WHERE the repeat sits and how often the key is repeated: adjacent
    // entries, the first and last entry of a longer list, three entries naming one key,
    // and a key the resource already carries repeated inside one request. Each row
    // carries the key the refusal names.
    let duplicates: Vec<(&str, &str, Value)> = vec![
        (
            "two adjacent entries naming one key",
            "team",
            json!([{ "key": "team", "value": "a" }, { "key": "team", "value": "b" }]),
        ),
        (
            "the first and last entry of a longer list",
            "team",
            json!([
                { "key": "team", "value": "a" },
                { "key": "cost-center", "value": "42" },
                { "key": "team", "value": "b" },
            ]),
        ),
        (
            "three entries naming one key",
            "team",
            json!([
                { "key": "team", "value": "a" },
                { "key": "team", "value": "b" },
                { "key": "team", "value": "c" },
            ]),
        ),
        (
            "a key the resource already carries, repeated",
            "env",
            json!([{ "key": "env", "value": "a" }, { "key": "env", "value": "b" }]),
        ),
        (
            "a repeated pair carrying the very same value",
            "team",
            json!([{ "key": "team", "value": "a" }, { "key": "team", "value": "a" }]),
        ),
    ];
    for (case, repeated, tags) in duplicates {
        let refused = tag_raw(&em, &arn, tags).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected 400, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
        assert_eq!(
            refusal_message(&refused),
            duplicate_key_message(repeated),
            "case {case}: unexpected message, got {:?}",
            refused.body
        );
        assert_eq!(
            listed_tags(&em, &arn).await,
            sorted(&stored),
            "case {case}: a refused write must leave the stored tags alone"
        );
    }

    // Keys that differ in case are different keys, so a request naming both is not a
    // repeat: `TagKey` is documented case-sensitive.
    let distinct = vec![pair("Team", "a"), pair("team", "b"), pair("TEAM", "c")];
    let written = tag(&em, &arn, &distinct).await;
    assert_eq!(
        written.status, 200,
        "keys differing only in case are distinct keys, got {} / {:?}",
        written.status, written.body
    );
    let mut expected = stored.clone();
    expected.extend(distinct);
    assert_eq!(
        listed_tags(&em, &arn).await,
        sorted(&expected),
        "each key differing in case must be stored as its own tag"
    );

    // A create judges a repeated key the same way, and stores nothing.
    let refused = em
        .create(json!({
            "brandProfileName": "duplicate-key-create",
            "tags": json!([{ "key": "team", "value": "a" }, { "key": "team", "value": "b" }]),
        }))
        .await;
    assert_eq!(
        refused.status, 400,
        "a create naming one key twice must be refused, got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error.as_deref(),
        Some("ValidationException"),
        "unexpected error code, got {:?}",
        refused.error
    );
    assert_eq!(
        refusal_message(&refused),
        duplicate_key_message("team"),
        "unexpected message, got {:?}",
        refused.body
    );
}

#[tokio::test]
async fn the_request_is_judged_before_the_resource_is_looked_up() {
    // A tag set no request may carry is refused whatever resource its ARN names, so the
    // refusal does not depend on the store: the rows are an ARN that names nothing of
    // this service at all, one that names a resource this store does not hold, and the
    // ARN of a live resource.
    let em = Emulator::new();
    let live = tagged_profile(&em, "judged-first", &[]).await;
    let unknown = format!(
        "arn:aws:end-user-messaging:us-east-1:{}:brand-profile/bp-00000000000000000",
        engine::EMULATOR_ACCOUNT_ID
    );
    let not_this_service = "arn:aws:s3:::some-bucket".to_string();

    for arn in [live, unknown, not_this_service] {
        for (case, expected, tags) in [
            (
                "a reserved key",
                reserved_key_message("aws:cost-center"),
                json!([{ "key": "aws:cost-center", "value": "1" }]),
            ),
            (
                "a repeated key",
                duplicate_key_message("team"),
                json!([{ "key": "team", "value": "a" }, { "key": "team", "value": "b" }]),
            ),
        ] {
            let refused = tag_raw(&em, &arn, tags).await;
            assert_eq!(
                refused.status, 400,
                "case {case} on {arn}: expected 400, got {} / {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error.as_deref(),
                Some("ValidationException"),
                "case {case} on {arn}: unexpected error code, got {:?}",
                refused.error
            );
            // The refusal is the request's own, so it does not depend on which
            // resource — if any — the ARN names.
            assert_eq!(
                refusal_message(&refused),
                expected,
                "case {case} on {arn}: unexpected message, got {:?}",
                refused.body
            );
        }
    }
}

#[tokio::test]
async fn a_write_past_the_per_resource_tag_ceiling_is_refused_and_writes_nothing() {
    let em = Emulator::new();
    // The resource starts one tag short of the ceiling, so one new key fits and two do
    // not: the bound is on what the resource would CARRY, not on what one request names.
    let stored: Vec<(String, String)> = (0..MAX_TAGS_PER_RESOURCE - 1)
        .map(|index| (format!("key-{index}"), format!("value-{index}")))
        .collect();
    let arn = tagged_profile(&em, "tag-ceiling", &stored).await;

    let past = vec![pair("extra-a", "1"), pair("extra-b", "2")];
    let refused = tag(&em, &arn, &past).await;
    assert_eq!(
        refused.status, 400,
        "a write past the ceiling is refused, got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error.as_deref(),
        Some("ServiceQuotaExceededException"),
        "unexpected error code, got {:?}",
        refused.error
    );
    assert_eq!(
        listed_tags(&em, &arn).await,
        sorted(&stored),
        "a refused write must leave the stored tags alone"
    );

    // The last free slot is served.
    let fits = tag(&em, &arn, &[pair("extra-a", "1")]).await;
    assert_eq!(
        fits.status, 200,
        "a write into the last free slot must succeed, got {} / {:?}",
        fits.status, fits.body
    );
    let mut at_ceiling = stored.clone();
    at_ceiling.push(pair("extra-a", "1"));
    assert_eq!(
        listed_tags(&em, &arn).await,
        sorted(&at_ceiling),
        "the resource must carry exactly the ceiling's worth of tags"
    );

    // At the ceiling, a write that only replaces values is still served — it adds no
    // key — while one naming any new key is refused.
    let replaced = tag(&em, &arn, &[pair("extra-a", "2"), pair("key-0", "fresh")]).await;
    assert_eq!(
        replaced.status, 200,
        "a write that only replaces stored keys must be served at the ceiling, got {} / {:?}",
        replaced.status, replaced.body
    );
    let mut after = at_ceiling.clone();
    for (key, value) in [("extra-a", "2"), ("key-0", "fresh")] {
        let slot = after
            .iter_mut()
            .find(|(stored_key, _)| stored_key == key)
            .unwrap_or_else(|| panic!("{key} must already be stored"));
        slot.1 = value.to_string();
    }
    assert_eq!(
        listed_tags(&em, &arn).await,
        sorted(&after),
        "replacing a stored key's value must leave the key set unchanged"
    );

    let one_too_many = tag(&em, &arn, &[pair("extra-b", "2")]).await;
    assert_eq!(
        one_too_many.status, 400,
        "a new key at the ceiling is refused, got {} / {:?}",
        one_too_many.status, one_too_many.body
    );
    assert_eq!(
        one_too_many.error.as_deref(),
        Some("ServiceQuotaExceededException"),
        "unexpected error code, got {:?}",
        one_too_many.error
    );
    assert_eq!(
        listed_tags(&em, &arn).await,
        sorted(&after),
        "a refused write must leave the stored tags alone"
    );
}
