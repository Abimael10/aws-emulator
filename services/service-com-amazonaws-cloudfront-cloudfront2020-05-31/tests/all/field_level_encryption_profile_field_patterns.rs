// SPDX-License-Identifier: CC0-1.0

//! The FIELD PATTERNS one encryption entity of a field-level encryption profile may carry,
//! end to end over HTTP.
//!
//! The rule the model states on `EncryptionEntity$FieldPatterns`: "You can provide the full
//! field name, or any beginning characters followed by a wildcard (*). You can't overlap
//! field patterns. For example, you can't have both ABC* and AB*. Note that field patterns
//! are case-sensitive." So a pattern ending in the wildcard COVERS every field name beginning
//! with the characters in front of it, and an entity may not carry both that pattern and
//! another pattern it covers: such a configuration says nothing about which key encrypts a
//! field both patterns match, so the service refuses it with the `InvalidArgument` (HTTP 400)
//! the operation models and stores no profile.
//!
//! The scope is the ENTITY, which is the scope the sentence states: two entities of one
//! profile encrypt with different keys, so they are free to name the same field.
//!
//! The rows differ along what the rule generalises over: whether a pattern is a wildcard
//! pattern or a full field name, the ORDER the covering and the covered pattern stand in, how
//! many patterns the entity carries, the CASE of otherwise-overlapping patterns, and whether
//! the patterns sit in one entity or in two. Checked on both payloads that state a profile
//! configuration — the create and the update.
//!
//! The harness is the profile lifecycle suite's, reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MAX_FIELD_LEVEL_ENCRYPTION_ENCRYPTION_ENTITIES, MAX_FIELD_LEVEL_ENCRYPTION_FIELD_PATTERNS,
    OVERLAPPING_FIELD_PATTERNS_MESSAGE,
};

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::field_level_encryption_profile_lifecycle::{
    create_profile, get_profile_config, upload_key, Entity, Profile,
};

/// `PUT /2020-05-31/field-level-encryption-profile/{Id}/config` with the profile
/// configuration `body` under the version token `if_match`.
async fn update_profile(
    router: &MultiServiceRouter,
    id: &str,
    body: &str,
    if_match: &str,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/field-level-encryption-profile/{id}/config"),
        Some(body),
        Some(if_match),
    )
    .await
}

/// The ids `ListFieldLevelEncryptionProfiles` reports, which is what says whether a refused
/// create stored anything.
async fn listed_profile_ids(router: &MultiServiceRouter) -> Vec<String> {
    let listed = call(
        router,
        "GET",
        &format!("/{API_VERSION}/field-level-encryption-profile"),
        None,
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "expected HTTP 200 from ListFieldLevelEncryptionProfiles, got {} with {}",
        listed.status, listed.raw
    );
    listed
        .root()
        .children_named("Items")
        .first()
        .map(|items| {
            items
                .children_named("FieldLevelEncryptionProfileSummary")
                .iter()
                .filter_map(|summary| summary.text_at("Id").map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The pattern sets one entity is run over: `(case, the patterns, whether the service stores
/// the profile)`.
///
/// `ABC*` with `AB*` is the example the model itself gives; the rest span the same rule at the
/// other shapes a pattern set takes.
fn pattern_rows() -> Vec<(&'static str, Vec<&'static str>, bool)> {
    vec![
        ("the covering pattern last", vec!["ABC*", "AB*"], false),
        ("the covering pattern first", vec!["AB*", "ABC*"], false),
        (
            "a full field name under a wildcard pattern",
            vec!["AB*", "ABCDE"],
            false,
        ),
        (
            "a wildcard pattern repeated",
            vec!["AB*", "AB*"],
            false,
        ),
        (
            "a covering pattern among disjoint ones",
            vec!["CD*", "ABC*", "EF*", "AB*"],
            false,
        ),
        (
            "a wildcard pattern that covers everything",
            vec!["*", "ABC"],
            false,
        ),
        ("disjoint wildcard patterns", vec!["AB*", "CD*"], true),
        (
            "full field names that share a prefix",
            vec!["ABC", "ABCDE", "AB"],
            true,
        ),
        (
            "patterns that overlap only in another case",
            vec!["AB*", "ab*", "Ab*"],
            true,
        ),
        ("a single wildcard pattern", vec!["AB*"], true),
        (
            "one pattern per field, none of them a wildcard",
            vec!["creditCardNumber", "creditCardExpiry"],
            true,
        ),
    ]
}

/// A create whose entity carries overlapping patterns is refused, and the same patterns split
/// across two entities are stored: the rule is about ONE entity's patterns.
#[tokio::test]
async fn an_entity_may_not_carry_overlapping_field_patterns() {
    let router = emulator();
    let first_key = upload_key(&router, 0).await;
    let second_key = upload_key(&router, 1).await;

    for (index, (case, patterns, stored)) in pattern_rows().into_iter().enumerate() {
        let before = listed_profile_ids(&router).await;
        let profile = Profile::new(
            case,
            &format!("overlap-{index}"),
            Some("the field pattern overlap suite"),
            vec![Entity::new(&first_key, "provider-one", &patterns)],
        );
        let answer = create_profile(&router, &profile.xml()).await;
        if stored {
            assert_eq!(
                answer.status, 201,
                "case {case}: expected HTTP 201 for patterns {patterns:?}, got {} with {}",
                answer.status, answer.raw
            );
            let id = answer
                .root()
                .find_text("Id")
                .unwrap_or_else(|| panic!("case {case}: the created profile reports an id"))
                .to_string();
            let read = get_profile_config(&router, &id).await;
            // `GetFieldLevelEncryptionProfileConfig` answers with the configuration itself as
            // the document's root.
            profile.assert_matches(read.root(), "after the create", &read.raw);
            continue;
        }

        assert_eq!(
            (answer.status, answer.error_code()),
            (400, "InvalidArgument"),
            "case {case}: expected a 400 InvalidArgument for patterns {patterns:?}, got {} / \
             {:?} with {}",
            answer.status,
            answer.error_code(),
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message"),
            Some(OVERLAPPING_FIELD_PATTERNS_MESSAGE),
            "case {case}: expected the sentence {OVERLAPPING_FIELD_PATTERNS_MESSAGE:?}, got {:?}",
            answer.root().find_text("Message")
        );
        assert_eq!(
            listed_profile_ids(&router).await,
            before,
            "case {case}: the refused create stored a profile"
        );

        // The SAME patterns, one per entity, describe which key encrypts each field — so the
        // service stores that profile, which is what makes the ENTITY the scope of the rule.
        let split = Profile::new(
            case,
            &format!("overlap-split-{index}"),
            Some("the field pattern overlap suite"),
            patterns
                .iter()
                .enumerate()
                .map(|(position, pattern)| {
                    let key = if position % 2 == 0 {
                        &first_key
                    } else {
                        &second_key
                    };
                    Entity::new(key, &format!("provider-{position}"), &[pattern])
                })
                .collect(),
        );
        let accepted = create_profile(&router, &split.xml()).await;
        assert_eq!(
            accepted.status, 201,
            "case {case}: expected HTTP 201 with one pattern per entity, got {} with {}",
            accepted.status, accepted.raw
        );
    }
}

/// The counts a profile's lists are held to are checked BEFORE the patterns are compared with
/// one another, so what admitting a profile costs is bounded: a list past its count is refused
/// with the modelled error for that list, and the count itself is admissible.
#[tokio::test]
async fn the_lists_of_a_profile_are_held_to_their_counts() {
    let router = emulator();
    let key = upload_key(&router, 3).await;

    // (case, how many entities, how many patterns in each, the error code or `None` for a
    // configuration the service stores) — each list at its count and one past it.
    let rows: Vec<(&str, usize, usize, Option<&str>)> = vec![
        ("one entity of one pattern", 1, 1, None),
        (
            "entities at the count",
            MAX_FIELD_LEVEL_ENCRYPTION_ENCRYPTION_ENTITIES,
            1,
            None,
        ),
        (
            "one entity past the count",
            MAX_FIELD_LEVEL_ENCRYPTION_ENCRYPTION_ENTITIES + 1,
            1,
            Some("TooManyFieldLevelEncryptionEncryptionEntities"),
        ),
        (
            "patterns at the count",
            1,
            MAX_FIELD_LEVEL_ENCRYPTION_FIELD_PATTERNS,
            None,
        ),
        (
            "one pattern past the count",
            1,
            MAX_FIELD_LEVEL_ENCRYPTION_FIELD_PATTERNS + 1,
            Some("TooManyFieldLevelEncryptionFieldPatterns"),
        ),
    ];

    for (index, (case, entities, patterns, refusal)) in rows.into_iter().enumerate() {
        let before = listed_profile_ids(&router).await;
        // Disjoint full field names, so nothing here is refused for overlapping instead.
        let named: Vec<Vec<String>> = (0..entities)
            .map(|entity| {
                (0..patterns)
                    .map(|pattern| format!("field-{entity}-{pattern}"))
                    .collect()
            })
            .collect();
        let profile = Profile::new(
            case,
            &format!("counts-{index}"),
            Some("the field pattern overlap suite"),
            named
                .iter()
                .enumerate()
                .map(|(entity, fields)| {
                    Entity::new(
                        &key,
                        &format!("provider-{entity}"),
                        &fields.iter().map(String::as_str).collect::<Vec<_>>(),
                    )
                })
                .collect(),
        );
        let answer = create_profile(&router, &profile.xml()).await;
        match refusal {
            Some(code) => {
                assert_eq!(
                    (answer.status, answer.error_code()),
                    (400, code),
                    "case {case}: expected a 400 {code}, got {} / {:?}",
                    answer.status,
                    answer.error_code()
                );
                assert_eq!(
                    listed_profile_ids(&router).await,
                    before,
                    "case {case}: the refused create stored a profile"
                );
            }
            None => assert_eq!(
                answer.status, 201,
                "case {case}: expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            ),
        }
    }
}

/// An UPDATE is held to the same rule, and the profile it would have moved is left exactly as
/// it was.
#[tokio::test]
async fn an_update_onto_overlapping_field_patterns_is_refused() {
    let router = emulator();
    let key = upload_key(&router, 2).await;

    let stored = Profile::new(
        "the stored profile",
        "overlap-update",
        Some("the field pattern overlap suite"),
        vec![Entity::new(&key, "provider-one", &["AB*", "CD*"])],
    );
    let created = create_profile(&router, &stored.xml()).await;
    assert_eq!(
        created.status, 201,
        "expected HTTP 201 from CreateFieldLevelEncryptionProfile, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .expect("the created profile reports an id")
        .to_string();
    let etag = created.header("etag").to_string();

    let moved = Profile {
        entities: vec![Entity::new(&key, "provider-one", &["AB*", "ABC*"])],
        entities_quantity: 1,
        ..stored.clone()
    };
    let refused = update_profile(&router, &id, &moved.xml(), &etag).await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "InvalidArgument"),
        "expected a 400 InvalidArgument from the update, got {} / {:?} with {}",
        refused.status,
        refused.error_code(),
        refused.raw
    );
    assert_eq!(
        refused.root().find_text("Message"),
        Some(OVERLAPPING_FIELD_PATTERNS_MESSAGE),
        "expected the sentence {OVERLAPPING_FIELD_PATTERNS_MESSAGE:?}, got {:?}",
        refused.root().find_text("Message")
    );

    let read = get_profile_config(&router, &id).await;
    assert_eq!(
        read.header("etag"),
        etag,
        "the refused update minted a new version: {}",
        read.raw
    );
    stored.assert_matches(read.root(), "after the refused update", &read.raw);
}
