// SPDX-License-Identifier: CC0-1.0

//! A refusal sentence this service WORDS around a value the request carried fits the
//! `Message` member that carries it.
//!
//! Every modelled error of this service carries `Message`, and every one of those members
//! targets `ErrorMessage`, which the model constrains to `@length` 0..256
//! (`smithy_query(query="shape",shape="com.amazonaws.ebs#ErrorMessage")`). So a refusal
//! that interpolates a request value — a `BlockToken` whose own `@length` maximum is
//! itself 256, a header-bound `ChecksumAlgorithm` whose value the service names back
//! alongside the set it supports — has to be rendered so the sentence stays inside that
//! maximum: the fixed text whole, the echo elided when it would not fit, with the value's
//! full length reported so the caller still learns what was refused.
//!
//! Rows differ along what the rule generalises over: which operation words the refusal,
//! which member the echoed value arrived in and where it travelled (a query parameter, a
//! header), and how long the value is — inside the budget, exactly at it, one character
//! past it, and far past it.

use crate::snapshot_block_loop::{
    block_bytes, checksum, complete_snapshot_with_algorithm, get_block, list_blocks, listed_blocks,
    member, put_block, put_block_with_algorithm, router, start_snapshot, write_snapshot, Reply,
    BLOCK_SIZE,
};

/// The `@length` maximum of `ErrorMessage`, the shape every modelled error's `Message`
/// member targets (`smithy_query(query="shape",shape="com.amazonaws.ebs#ErrorMessage")`
/// reports `@length` 0..256). Mirrors `crate::state::ERROR_MESSAGE_MAX_LENGTH`; a Smithy
/// `@length` on a string counts characters, which is how the assertions measure.
const ERROR_MESSAGE_MAX_LENGTH: usize = 256;

/// The fixed halves of the sentence `GetSnapshotBlock` words for a well-formed token that
/// names no block, which the refusal must carry WHOLE however long the token is.
const TOKEN_PREFIX: &str = "The block token ";
const TOKEN_SUFFIX: &str = " is invalid.";

/// The fixed halves of the sentence the operations that take `ChecksumAlgorithm` off the
/// request boundary word for an algorithm this service does not support. The supported set
/// is the one value the modelled `ChecksumAlgorithm` enum declares
/// (`smithy_query(query="shape",shape="com.amazonaws.ebs#ChecksumAlgorithm")`).
const ALGORITHM_PREFIX: &str = "Unsupported Checksum Algorithm '";
const ALGORITHM_SUFFIX: &str = "', supported algorithms are SHA256";

/// The refusal this service words for `value`, as the rule requires it to be rendered: the
/// value echoed whole when the sentence around it leaves room, and otherwise a prefix plus
/// the elision `... [N bytes]` naming the value's full length, the whole echo held to the
/// budget the sentence leaves.
///
/// Computed here independently of the service — the shared elision holds an echo to a
/// BYTE budget, which for the ASCII values these rows send is the character count too.
fn expected_message(prefix: &str, value: &str, suffix: &str) -> String {
    let fixed = prefix.chars().count() + suffix.chars().count();
    let budget = ERROR_MESSAGE_MAX_LENGTH - fixed;
    if value.chars().count() <= budget {
        return format!("{prefix}{value}{suffix}");
    }
    let elision = format!("... [{} bytes]", value.len());
    let kept: String = value
        .chars()
        .take(budget - elision.chars().count())
        .collect();
    format!("{prefix}{kept}{elision}{suffix}")
}

/// Assert one refusal: a 400 `ValidationException` whose `Message` carries the sentence's
/// fixed text whole, echoes what fits of the rejected value, and stays inside the modelled
/// maximum.
fn assert_fitted(case: &str, reply: &Reply, reason: &str, expected: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: the refusal must answer 400, got {} with body {}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(
        member(reply, "Reason"),
        reason,
        "case {case:?}: expected Reason {reason:?}, got {}",
        reply.json()
    );
    let message = member(reply, "Message");
    assert!(
        message.chars().count() <= ERROR_MESSAGE_MAX_LENGTH,
        "case {case:?}: expected a Message of at most {ERROR_MESSAGE_MAX_LENGTH} characters, \
         got {} characters: {message:?}",
        message.chars().count()
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected Message {expected:?}, got {message:?}"
    );
}

/// RULE: the `INVALID_BLOCK_TOKEN` refusal `GetSnapshotBlock` words for a well-formed
/// token that names no block fits `ErrorMessage`'s `@length` maximum, whatever the token's
/// own length is: the sentence is carried whole and the echo is elided only when echoing
/// the token whole would push the message past that maximum.
#[tokio::test]
async fn the_invalid_block_token_refusal_fits_the_modelled_message_length() {
    let router = router();
    let readable = write_snapshot(&router, "a readable snapshot", 1, &[(0, 41)]).await;

    // The echo's budget is what the sentence leaves of the modelled maximum, so the rows
    // bracket it: inside, exactly at it, one past it, and at the token member's own
    // `@length` maximum (256, the recorded case, which is itself the whole budget of the
    // member carrying the message).
    let budget = ERROR_MESSAGE_MAX_LENGTH
        - TOKEN_PREFIX.chars().count()
        - TOKEN_SUFFIX.chars().count();
    let lengths = [1_usize, 64, budget - 1, budget, budget + 1, 256];

    for length in lengths {
        let case = format!("a blockToken of {length} characters");
        // Inside the expression the service enforces on a `blockToken`
        // (`^[A-Za-z0-9+/=_-]+$`), so the request reaches token resolution rather than the
        // constraint refusal, and inside the member's own `@length` 0..256.
        let token = "A".repeat(length);
        let reply = get_block(&router, &readable, 0, &token).await;
        assert_fitted(
            &case,
            &reply,
            "INVALID_BLOCK_TOKEN",
            &expected_message(TOKEN_PREFIX, &token, TOKEN_SUFFIX),
        );
        let message = member(&reply, "Message");
        assert!(
            message.starts_with(TOKEN_PREFIX) && message.ends_with(TOKEN_SUFFIX),
            "case {case:?}: the sentence must be carried whole, got {message:?}"
        );
        // An elided echo still reports the value's full length, so the caller can tell a
        // token that was shortened from one that really was that short.
        if length > budget {
            assert!(
                message.contains(&format!("... [{length} bytes]")),
                "case {case:?}: an elided echo must name the token's full length, \
                 got {message:?}"
            );
        } else {
            assert!(
                message.contains(&token),
                "case {case:?}: a token inside the budget must be echoed whole, \
                 got {message:?}"
            );
        }
    }

    // The token the service really issued still reads its block, so the rule above refuses
    // nothing a caller is entitled to.
    let listed = listed_blocks(&list_blocks(&router, &readable, "").await);
    let (_, real_token) = listed
        .first()
        .cloned()
        .expect("the sealed snapshot lists its block");
    let fetched = get_block(&router, &readable, 0, &real_token).await;
    assert_eq!(
        fetched.status, 200,
        "case \"the issued token\": GetSnapshotBlock must still answer 200, got {} with body {}",
        fetched.status,
        String::from_utf8_lossy(&fetched.body)
    );
}

/// RULE, on the other member and the other two operations: the refusal naming an
/// unsupported `ChecksumAlgorithm` fits `ErrorMessage`'s `@length` maximum too.
///
/// This sentence is reached only by a value the member's own modelled `@length` 0..32 and
/// `@pattern` admit — a value outside them is refused by those constraints instead, in the
/// framework's constraint report (pinned by
/// `crate::judged_header_members_keep_their_modelled_form`) — so the rows run from the
/// shortest name up to that maximum, and the fit is asserted on each.
#[tokio::test]
async fn the_unsupported_algorithm_refusal_fits_the_modelled_message_length() {
    let router = router();

    // The member's modelled `@length` maximum
    // (`smithy_query(query="shape",shape="com.amazonaws.ebs#ChecksumAlgorithm")`: 0..32),
    // which the rows bracket from below and reach exactly.
    let lengths = [1_usize, 3, 31, 32];

    for length in lengths {
        // Inside the member's modelled `@pattern` `^[A-Za-z0-9]+$` and never `SHA256`, so
        // the refusal is about an algorithm the service does not support.
        let algorithm = "M".repeat(length);
        let expected = expected_message(ALGORITHM_PREFIX, &algorithm, ALGORITHM_SUFFIX);

        let started = start_snapshot(&router, &serde_json::json!({ "VolumeSize": 1 })).await;
        assert_eq!(
            started.status, 201,
            "a pending snapshot must start, got {} with body {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let pending = member(&started, "SnapshotId");

        let data = block_bytes(7);
        let digest = checksum(&data);
        let written = put_block_with_algorithm(
            &router,
            &pending,
            0,
            data,
            BLOCK_SIZE,
            &digest,
            &algorithm,
        )
        .await;
        assert_fitted(
            &format!("PutSnapshotBlock with an algorithm of {length} characters"),
            &written,
            "INVALID_PARAMETER_VALUE",
            &expected,
        );

        let completed = complete_snapshot_with_algorithm(&router, &pending, 0, &algorithm).await;
        assert_fitted(
            &format!("CompleteSnapshot with an algorithm of {length} characters"),
            &completed,
            "INVALID_PARAMETER_VALUE",
            &expected,
        );

        // Both refusals carry the service's whole sentence, whichever operation worded it.
        for (case, reply) in [("PutSnapshotBlock", &written), ("CompleteSnapshot", &completed)] {
            let message = member(reply, "Message");
            assert!(
                message.starts_with(ALGORITHM_PREFIX) && message.ends_with(ALGORITHM_SUFFIX),
                "case {case:?} with an algorithm of {length} characters: the sentence must be \
                 carried whole, got {message:?}"
            );
        }

        // And the supported algorithm is still accepted on the same snapshot, so the rule
        // refuses only the value it is about.
        let data = block_bytes(8);
        let digest = checksum(&data);
        let accepted = put_block(&router, &pending, 0, data, BLOCK_SIZE, &digest).await;
        assert_eq!(
            accepted.status, 201,
            "an algorithm of {length} characters must not affect the supported one, got {} \
             with body {}",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
    }
}

/// RULE, asserted on the rendering itself rather than through one member's wire path: the
/// sentence is held to `ErrorMessage`'s `@length` maximum for a value of ANY length — the
/// fixed text whole, the echo elided with the value's full length named.
///
/// Pinned directly because the member whose refusal words this sentence now has its
/// modelled `@length` 0..32 judged before the sentence is reached, so no request can carry
/// a value past the budget TO it; the rendering is shared by every refusal this service
/// words around a request value, and a value past the budget is exactly what it exists to
/// hold. Rows bracket the budget the echo has: one inside, exactly at it, one past it, and
/// far past it.
#[test]
fn the_shared_refusal_rendering_holds_an_over_long_value_to_the_modelled_message_length() {
    let budget = ERROR_MESSAGE_MAX_LENGTH
        - ALGORITHM_PREFIX.chars().count()
        - ALGORITHM_SUFFIX.chars().count();

    for length in [budget - 1, budget, budget + 1, 1_024] {
        let value = "M".repeat(length);
        let message =
            service_com_amazonaws_ebs_ebs::state::unsupported_checksum_algorithm_message(&value);
        let case = format!("a value of {length} characters");
        assert!(
            message.chars().count() <= ERROR_MESSAGE_MAX_LENGTH,
            "case {case:?}: expected a message of at most {ERROR_MESSAGE_MAX_LENGTH} \
             characters, got {} characters: {message:?}",
            message.chars().count()
        );
        assert_eq!(
            message,
            expected_message(ALGORITHM_PREFIX, &value, ALGORITHM_SUFFIX),
            "case {case:?}: expected the fixed text whole around the echo the budget leaves, \
             got {message:?}"
        );
        if length > budget {
            assert!(
                message.contains(&format!("... [{length} bytes]")),
                "case {case:?}: an elided echo must name the value's full length, got \
                 {message:?}"
            );
        }
    }
}
