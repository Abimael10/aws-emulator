// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE every paginated read of this service states when its `nextToken` is not a
//! cursor that read issued, driven over HTTP through the same `MultiServiceRouter` the
//! emulator serves with.
//!
//! The rule is not about one operation and not about one token: **a refusal this service
//! raises itself is worded through the crate's one refusal-sentence builder — the
//! `<N> validation error(s) detected: ` envelope over
//! `<subject> at '<member path>' failed to satisfy constraint: <requirement>` clauses — so
//! a `nextToken` outside the cursors a read issued is refused with ONE clause, about
//! `nextToken`, echoing the value the request held, from every paginated operation
//! alike.** The condition stays with the handlers (the model's constraint traits say how
//! long a `nextToken` may be, not which values are cursors); only the words are shared.
//!
//! So the rows below span the class along three axes rather than repeating one call:
//!
//! * the OPERATION — all seven paginated reads this service binds
//!   (`smithy_query(query="operation",operation="ListChecks,ListRecommendations,
//!   ListRecommendationResources,ListOrganizationRecommendations,
//!   ListOrganizationRecommendationAccounts,ListOrganizationRecommendationResources,
//!   ListRecommendationsForResource",
//!   model_path="services/service-com-amazonaws-trustedadvisor-trustedadvisor/smithy-model/com-amazonaws-trustedadvisor-trustedadvisor.json")`
//!   lists every one as `@paginated(inputToken: nextToken)`, each binding
//!   `ValidationException` at HTTP 400);
//! * the way the VALUE fails to be a cursor — outside the base64 alphabet, base64 of bytes
//!   that are not text, base64 of text that carries no cursor at all, a cursor another list
//!   issued, and a cursor the same list issued under filters it was not issued under;
//! * the SIZE of the echoed value, up to a value far longer than the clause's own skeleton,
//!   inside the `@length(4, 10000)` the model puts on `nextToken` (a longer one is the
//!   framework's refusal, not a handler's).
//!
//! Every message is read back through the SAME two readers
//! `tests/all/refusal_sentence_one_builder.rs` parses the other two refusal paths with, so
//! a handler wording its own envelope or its own clause skeleton has nothing to parse.
//!
//! WHICH values are not cursors is pinned here too, because the condition and the sentence
//! are one answer: **a paginated read resumes only from a continuation token THIS store
//! issued for THAT read, so every other value — one a caller assembled, one another store
//! minted, one of this read's own cursors with a byte changed — is refused instead of being
//! read as a position in the sequence.** The position a token names travels inside the
//! token, so the value is told apart by an authenticator over that position and not by its
//! shape: `no_value_but_a_cursor_this_store_issued_resumes_a_paginated_read` builds, for
//! each of the seven reads, values carrying that read's OWN scope and keys of its own
//! sequence, which is everything about a cursor a caller can read off the token it was
//! handed.

use serde_json::json;
use service_com_amazonaws_trustedadvisor_trustedadvisor::state::{
    service_state, PageTokenKey, SupportPlan,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::organization_recommendation_lifecycle::organization_recommendation_arn;
use crate::recommendation_loop::{
    call, emulator_on_plan, error_code, label, recommendation_arn, resource_arn,
    PRIORITIZED_CHECK_IDS,
};
use crate::refusal_sentence_one_builder::{clause_parts, clauses_of, Clause};

/// The member path every page-token clause names — `nextToken` is the `inputToken` of the
/// `@paginated` trait on all seven operations.
const TOKEN_MEMBER: &str = "nextToken";

/// What that member had to satisfy, in the requirement form this service states every
/// clause's rule half in.
const TOKEN_REQUIREMENT: &str =
    "Member must be a pagination token this service issued for this list";

/// The clause a refused `nextToken` earns: the value the request held, the member it was
/// at, and the requirement it failed.
fn token_clause(token: &str) -> Clause {
    (
        format!("Value '{token}'"),
        TOKEN_MEMBER.to_string(),
        TOKEN_REQUIREMENT.to_string(),
    )
}

/// A mounted emulator on a support plan that includes the Trusted Advisor API, so every
/// request below reaches its handler (the gate itself is pinned in
/// `tests/all/support_level_gate.rs`).
fn emulator() -> MultiServiceRouter {
    emulator_on_plan(SupportPlan::Business).0
}

/// The seven paginated reads, as (operation, the URI with no query string). The path
/// labels name SEEDED resources, so each row reaches the pagination step rather than a
/// not-found.
fn paginated_reads() -> Vec<(&'static str, String)> {
    let recommendation = recommendation_arn("amazon-ebs-snapshots");
    // Only a prioritized check has an organization recommendation (pinned in
    // `tests/all/organization_prioritized_only.rs`).
    let organization = organization_recommendation_arn(PRIORITIZED_CHECK_IDS[0]);
    vec![
        ("ListChecks", "/v1/checks".to_string()),
        ("ListRecommendations", "/v1/recommendations".to_string()),
        (
            "ListRecommendationResources",
            format!("/v1/recommendations/{}/resources", label(&recommendation)),
        ),
        (
            "ListOrganizationRecommendations",
            "/v1/organization-recommendations".to_string(),
        ),
        (
            "ListOrganizationRecommendationAccounts",
            format!(
                "/v1/organization-recommendations/{}/accounts",
                label(&organization)
            ),
        ),
        (
            "ListOrganizationRecommendationResources",
            format!(
                "/v1/organization-recommendations/{}/resources",
                label(&organization)
            ),
        ),
        (
            "ListRecommendationsForResource",
            format!("/v1/recommendations-for-resource/{}", label(&seeded_ec2())),
        ),
    ]
}

/// An `awsResourceArn` the modelled pattern admits
/// (`^arn:aws(-\w+)*:[\w\d-]+:([\w\d-]*)?:[\w\d_-]*([:/].+)*$`, len 20..2048), in the
/// emulator's own account.
fn seeded_ec2() -> String {
    format!(
        "arn:aws:ec2:us-east-1:{}:instance/i-0abc1de23f4567890",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// Drive one refused read and read the clauses its message carries back through the shared
/// readers: the envelope first, then each clause's three parts.
async fn refused_clauses(
    router: &MultiServiceRouter,
    case: &str,
    path_and_query: &str,
) -> Vec<Clause> {
    let (status, body) = call(router, "GET", path_and_query, None).await;
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case {case}: expected ValidationException, got {body}"
    );
    let message = body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: the refusal carries no message: {body}"))
        .to_string();
    clauses_of(case, &message)
        .iter()
        .map(|clause| clause_parts(case, clause))
        .collect()
}

/// A token every read will reject: base64 of `bytes`, which carries no cursor.
fn base64_of(bytes: &[u8]) -> String {
    aws_smithy_types::base64::encode(bytes)
}

#[tokio::test]
async fn every_paginated_read_words_the_page_token_refusal_the_same_way() {
    let router = emulator();

    // A cursor each of two lists really issued, so a row can replay one list's cursor on
    // another — the only failure mode that needs a token this service DID issue.
    let (_, checks_page) = call(&router, "GET", "/v1/checks?maxResults=1", None).await;
    let checks_cursor = checks_page["nextToken"]
        .as_str()
        .expect("a nextToken on a truncated ListChecks page")
        .to_string();
    let (_, recommendations_page) =
        call(&router, "GET", "/v1/recommendations?maxResults=1", None).await;
    let recommendations_cursor = recommendations_page["nextToken"]
        .as_str()
        .expect("a nextToken on a truncated ListRecommendations page")
        .to_string();

    // Each row: what is wrong with the value, and the value. Every one satisfies the
    // modelled `@length(4, 10000)`, so the refusal reached is the handler's own.
    let tokens: Vec<(&str, String)> = vec![
        (
            "four characters outside the base64 alphabet",
            "....".to_string(),
        ),
        (
            "base64 of bytes that are not text",
            base64_of(&[0xff, 0xfe, 0xfd]),
        ),
        (
            "base64 of text carrying no cursor",
            base64_of(b"not-a-cursor"),
        ),
        (
            "a value far longer than the clause that echoes it",
            "A".repeat(1_000),
        ),
        ("a cursor ListChecks issued", checks_cursor.clone()),
        (
            "a cursor ListRecommendations issued",
            recommendations_cursor.clone(),
        ),
    ];

    for (operation, base) in paginated_reads() {
        for (what, token) in &tokens {
            // A list's OWN cursor is a cursor, not a refusal: the two replay rows are
            // skipped on the list that issued the token, which
            // `tests/all/recommendation_loop.rs` pins is accepted there.
            if (operation == "ListChecks" && token == &checks_cursor)
                || (operation == "ListRecommendations" && token == &recommendations_cursor)
            {
                continue;
            }
            let case = format!("{operation} with {what}");
            let path = format!("{base}?nextToken={}", label(token));
            let clauses = refused_clauses(&router, &case, &path).await;
            assert_eq!(
                clauses,
                vec![token_clause(token)],
                "case {case}: expected one clause about {TOKEN_MEMBER}, got {clauses:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_cursor_from_another_filter_set_is_refused_with_the_same_sentence() {
    let router = emulator();

    // Each row: the read that ISSUES the cursor, and the read it is replayed on — the same
    // operation and the same parent, under a filter set it was not issued under, which is a
    // different sequence and so not a cursor of the read it reaches.
    let recommendation = recommendation_arn("amazon-ebs-snapshots");
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a filtered ListChecks cursor on the unfiltered list",
            "/v1/checks?pillar=cost_optimizing&maxResults=1".to_string(),
            "/v1/checks".to_string(),
        ),
        (
            "an unfiltered ListRecommendations cursor on a filtered list",
            "/v1/recommendations?maxResults=1".to_string(),
            "/v1/recommendations?pillar=cost_optimizing".to_string(),
        ),
        (
            "an unfiltered resources cursor on a Region-filtered list",
            format!(
                "/v1/recommendations/{}/resources?maxResults=1",
                label(&recommendation)
            ),
            format!(
                "/v1/recommendations/{}/resources?regionCode=us-east-1",
                label(&recommendation)
            ),
        ),
    ];

    for (name, issuing, replay) in rows {
        let (status, page) = call(&router, "GET", &issuing, None).await;
        assert_eq!(
            status, 200,
            "case {name}: expected the issuing read to answer 200, got {status} with {page}"
        );
        let cursor = page["nextToken"].as_str().unwrap_or_else(|| {
            panic!("case {name}: expected a nextToken on the truncated page, got {page}")
        });
        let separator = if replay.contains('?') { '&' } else { '?' };
        let path = format!("{replay}{separator}nextToken={}", label(cursor));
        let clauses = refused_clauses(&router, name, &path).await;
        assert_eq!(
            clauses,
            vec![token_clause(cursor)],
            "case {name}: expected one clause about {TOKEN_MEMBER}, got {clauses:?}"
        );
    }
}

#[tokio::test]
async fn both_conditions_this_service_raises_itself_state_one_clause_skeleton() {
    let router = emulator();

    // The page-token condition, and the repeated-ARN condition of
    // `BatchUpdateRecommendationResourceExclusion` — the two refusals no constraint trait
    // of the model can express. Each is read back through the shared readers, so the
    // subject form, the member path and the requirement are all that may differ.
    let token_clauses = refused_clauses(
        &router,
        "a page token that is not a cursor",
        "/v1/checks?nextToken=....",
    )
    .await;

    let arn = format!(
        "arn:aws:trustedadvisor::{}:recommendation-resource/amazon-ebs-snapshots-recommendation/res-ebs-0001",
        engine::EMULATOR_ACCOUNT_ID
    );
    let (status, body) = call(
        &router,
        "PUT",
        "/v1/batch-update-recommendation-resource-exclusion",
        Some(json!({
            "recommendationResourceExclusions": [
                {"arn": arn, "isExcluded": true},
                {"arn": arn, "isExcluded": false},
            ],
        })),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a repeated ARN: expected HTTP 400, got {status} with {body}"
    );
    let batch_message = body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case a repeated ARN: the refusal carries no message: {body}"));
    let batch_clauses: Vec<Clause> = clauses_of("a repeated ARN", batch_message)
        .iter()
        .map(|clause| clause_parts("a repeated ARN", clause))
        .collect();

    // Both state exactly one clause, and both echo the offending value the same way, which
    // is the half a handler could have worded for itself.
    for (case, clauses, expected_value) in [
        ("a page token that is not a cursor", &token_clauses, "...."),
        ("a repeated ARN", &batch_clauses, arn.as_str()),
    ] {
        assert_eq!(
            clauses.len(),
            1,
            "case {case}: expected one clause, got {clauses:?}"
        );
        assert_eq!(
            clauses[0].0,
            format!("Value '{expected_value}'"),
            "case {case}: expected the offending value echoed, got {:?}",
            clauses[0].0
        );
    }
    assert_eq!(
        token_clauses[0].1, TOKEN_MEMBER,
        "case a page token that is not a cursor: expected the clause at {TOKEN_MEMBER}, got {:?}",
        token_clauses[0].1
    );
    assert_eq!(
        batch_clauses[0].1, "recommendationResourceExclusions",
        "case a repeated ARN: expected the clause at recommendationResourceExclusions, got {:?}",
        batch_clauses[0].1
    );
}

#[tokio::test]
async fn a_token_outside_the_modelled_length_is_the_frameworks_refusal_not_this_one() {
    let router = emulator();

    // The boundary between the two paths: `nextToken` is `@length(4, 10000)`, so a value
    // shorter than four characters is collected by the framework's own constraint walk and
    // carries that requirement, while a four-character one reaches the handler. Both are
    // worded by the one builder, which is what lets this row read them with the same
    // reader.
    let cases: &[(&str, &str, &str)] = &[
        (
            "three characters",
            "...",
            "Member must have length greater than or equal to 4",
        ),
        ("four characters", "....", TOKEN_REQUIREMENT),
    ];

    for (case, token, requirement) in cases {
        let clauses =
            refused_clauses(&router, case, &format!("/v1/checks?nextToken={token}")).await;
        assert_eq!(
            clauses.len(),
            1,
            "case {case}: expected one clause, got {clauses:?}"
        );
        assert_eq!(
            clauses[0].1, TOKEN_MEMBER,
            "case {case}: expected the clause at {TOKEN_MEMBER}, got {:?}",
            clauses[0].1
        );
        assert_eq!(
            clauses[0].2, *requirement,
            "case {case}: expected the requirement {requirement:?}, got {:?}",
            clauses[0].2
        );
    }
}

/// What separates a cursor's scope from the key it points at. Written out here rather than
/// shared with `src/state.rs`: this suite is a caller assembling values out of the halves a
/// token it was handed reveals, not a second copy of the crate's encoding.
const CURSOR_SEPARATOR: char = '\u{1f}';

/// How many bytes of authenticator a minted token carries ahead of its cursor
/// (`service_common::page_tokens::AUTHENTICATOR_BYTES`), so a row below can build a value
/// with the SHAPE of a minted token without restating the width.
const AUTHENTICATOR_BYTES: usize = ::service_common::page_tokens::AUTHENTICATOR_BYTES;

/// A mounted emulator holding a SECOND entry everywhere a seeded read would otherwise hold
/// one, so each of the seven paginated reads truncates at `maxResults=1` and hands back a
/// cursor of its own.
///
/// Written straight into the store: Trusted Advisor models no operation that creates a
/// check result, so a second finding or a second member account can only arrive from
/// outside the API, exactly as the sibling suites write one.
fn emulator_with_two_entries_in_every_read() -> MultiServiceRouter {
    let (router, engine) = emulator_on_plan(SupportPlan::Business);
    let state = service_state(&*engine).expect("the engine carries this service's state");
    let mut state = state.lock();
    let check = PRIORITIZED_CHECK_IDS[0];

    // A second finding on the SAME AWS resource as the seeded one, so
    // `ListRecommendationsForResource` on that resource's ARN has two rows to page: the
    // flagged resource's ARN is derived from the stored resource's own Region, id and
    // owner, so a copy carrying those unchanged answers to the same ARN.
    let seeded = state
        .recommendation_resources
        .get(&resource_arn(check, "res-ec2-0001"))
        .cloned()
        .expect("the seeded store holds this check's first resource");
    let mut second_finding = seeded.clone();
    second_finding.id = "res-ec2-0009".to_string();
    second_finding.arn = resource_arn(check, "res-ec2-0009");
    state
        .recommendation_resources
        .insert(second_finding.arn.clone(), second_finding);

    // A second member account recommendation under that check's organization
    // recommendation, so `ListOrganizationRecommendationAccounts` has two rows to page.
    let member = state
        .recommendations
        .get(&recommendation_arn(check))
        .cloned()
        .expect("the seeded store holds this check's recommendation");
    let mut second_member = member.clone();
    second_member.id = format!("{}-second", member.id);
    second_member.arn = format!("{}-second", member.arn);
    let second_member_arn = second_member.arn.clone();
    state
        .recommendations
        .insert(second_member_arn.clone(), second_member);
    state
        .organization_recommendations
        .get_mut(&organization_recommendation_arn(check))
        .expect("the seeded store holds this check's organization recommendation")
        .member_recommendation_arns
        .push(second_member_arn);

    drop(state);
    router
}

/// The bytes a `nextToken` carries, base64-decoded.
fn token_bytes(case: &str, token: &str) -> Vec<u8> {
    aws_smithy_types::base64::decode(token).unwrap_or_else(|_| {
        panic!("case {case}: expected a cursor this read issued to be base64, got {token:?}")
    })
}

/// The scope and the key the cursor inside an issued token names.
///
/// A minted token is the authenticator followed by the cursor itself — a continuation token
/// is opaque, not confidential — so both halves are readable here. That is the point: every
/// value forged below carries the read's own scope and a key of its own sequence, so the
/// only thing it lacks is the authenticator, which is exactly what a caller cannot produce.
fn cursor_of(case: &str, token: &str) -> (String, String) {
    let bytes = token_bytes(case, token);
    assert!(
        bytes.len() > AUTHENTICATOR_BYTES,
        "case {case}: expected a cursor behind an authenticator of {AUTHENTICATOR_BYTES} \
         bytes, got {} bytes",
        bytes.len()
    );
    let cursor = String::from_utf8(bytes[AUTHENTICATOR_BYTES..].to_vec())
        .unwrap_or_else(|_| panic!("case {case}: the cursor behind the authenticator is not text"));
    let (scope, key) = cursor.split_once(CURSOR_SEPARATOR).unwrap_or_else(|| {
        panic!("case {case}: expected a cursor of a scope and a key, got {cursor:?}")
    });
    (scope.to_string(), key.to_string())
}

/// A `nextToken` a caller assembles out of `authenticator`, a scope and a key: the whole
/// encoding of a cursor except the one half it cannot compute.
fn assembled(authenticator: &[u8], scope: &str, key: &str) -> String {
    let cursor = format!("{scope}{CURSOR_SEPARATOR}{key}");
    aws_smithy_types::base64::encode([authenticator, cursor.as_bytes()].concat())
}

#[tokio::test]
async fn no_value_but_a_cursor_this_store_issued_resumes_a_paginated_read() {
    let router = emulator_with_two_entries_in_every_read();
    // A key of another store — another emulator process, a restarted one — whose cursors
    // are not cursors this store issued either.
    let another_store = PageTokenKey::new();

    for (operation, base) in paginated_reads() {
        // This read's own first page, truncated to one entry so it hands back a cursor,
        // and the scope and key that cursor names.
        let (status, page) = call(&router, "GET", &format!("{base}?maxResults=1"), None).await;
        assert_eq!(
            status, 200,
            "case {operation}: expected 200 from the issuing read, got {status} with {page}"
        );
        let issued = page["nextToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {operation}: expected a nextToken on a truncated page, got {page}")
            })
            .to_string();
        let (scope, key) = cursor_of(operation, &issued);

        // One of this read's own cursors with a single byte changed, in the authenticator
        // and in the cursor alike.
        let mut authenticator_changed = token_bytes(operation, &issued);
        authenticator_changed[0] ^= 0x01;
        let mut cursor_changed = token_bytes(operation, &issued);
        let last = cursor_changed.len() - 1;
        cursor_changed[last] ^= 0x01;

        // Each row: how the value fails to be a cursor this store issued for this read, and
        // the value. Rows differ along the KEY named (the one the read just offered, one
        // before every entry, one between two of them, one past every entry), along the
        // AUTHENTICATOR carried (none at all, all-zero, all-ones, another store's, one of
        // this store's with a byte changed) and along where a changed byte sits.
        let rows: Vec<(&str, String)> = vec![
            (
                "the read's own scope and the key it just offered, with no authenticator",
                assembled(&[], &scope, &key),
            ),
            (
                "the read's own scope and the empty key, with no authenticator",
                assembled(&[], &scope, ""),
            ),
            (
                "the read's own scope and a key between two entries, with no authenticator",
                assembled(&[], &scope, &format!("{key}0")),
            ),
            (
                "the read's own scope and a key past every entry, with no authenticator",
                assembled(&[], &scope, "zzzzzzzz"),
            ),
            (
                "the shape of a minted token, all-zero authenticator",
                assembled(&[0_u8; AUTHENTICATOR_BYTES], &scope, &key),
            ),
            (
                "the shape of a minted token, all-ones authenticator",
                assembled(&[0xff_u8; AUTHENTICATOR_BYTES], &scope, &key),
            ),
            (
                "a cursor another store minted for this very read",
                aws_smithy_types::base64::encode(
                    another_store.mint(&format!("{scope}{CURSOR_SEPARATOR}{key}")),
                ),
            ),
            (
                "this read's own cursor with one byte of its authenticator changed",
                aws_smithy_types::base64::encode(&authenticator_changed),
            ),
            (
                "this read's own cursor with one byte of its cursor changed",
                aws_smithy_types::base64::encode(&cursor_changed),
            ),
        ];

        for (what, token) in &rows {
            let case = format!("{operation} with {what}");
            // Inside the modelled `@length(4, 10000)`, so the refusal reached is the
            // handler's own and not the framework's constraint walk.
            let length = token.chars().count();
            assert!(
                (4..=10_000).contains(&length),
                "case {case}: expected a value the model admits, got one of {length} \
                 characters"
            );
            let clauses = refused_clauses(
                &router,
                &case,
                &format!("{base}?maxResults=1&nextToken={}", label(token)),
            )
            .await;
            assert_eq!(
                clauses,
                vec![token_clause(token)],
                "case {case}: expected one clause about {TOKEN_MEMBER}, got {clauses:?}"
            );
        }

        // And the cursor this read DID issue still resumes it: authenticating a token
        // refuses none of the positions the service itself offered.
        let (status, resumed) = call(
            &router,
            "GET",
            &format!("{base}?maxResults=1&nextToken={}", label(&issued)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {operation}: expected the issued cursor to resume the read, got {status} \
             with {resumed}"
        );
    }
}
