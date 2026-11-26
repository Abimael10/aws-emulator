// SPDX-License-Identifier: CC0-1.0

//! What `CreateInvalidation` answers a distribution identifier it cannot resolve, end to end
//! over HTTP, and which of the two sentences one status and one code carry.
//!
//! The rule under test: an operation addressed by a distribution identifier resolves a value
//! WRITTEN IN the alphabet CloudFront writes an entity identifier in against the store and
//! reports the distribution ABSENT — `NoSuchDistribution` (404) under that class's fixed
//! sentence — for every such value the account holds no distribution under, whatever request
//! document accompanies it; only a value written OUTSIDE that alphabet, which was never an
//! identifier CloudFront issued, is the identifier the route reports it cannot READ
//! (`<member> format is invalid`, naming the model's own member). The two sentences are
//! different client contracts — one says the distribution is gone and the caller should create
//! or re-resolve it, the other says the caller's own identifier handling is broken — so the
//! unreadable-identifier wording may never answer a well-formed identifier that is merely
//! absent, and neither outcome is the argument class the model binds separately
//! (`InvalidArgument`, 400).
//!
//! `smithy_query(query="operation",operation="CreateInvalidation")` reports
//! `DistributionId` as a required `@httpLabel` on a plain `string` — no `@pattern`, no
//! `@length` — with `InvalidationBatch` as the required `@httpPayload`, and binds
//! `NoSuchDistribution` (404) and `InvalidArgument` (400) as distinct error classes, so the
//! form of the identifier is read no further than the alphabet and the absent distribution is
//! never reported under the argument class.
//!
//! This suite is the PAYLOAD pairing the identifier suites do not make: the identifier rows are
//! crossed with the batch documents a well-formed request can carry — one wildcard, one single
//! object, several objects including an encoded one — because what the route answers an
//! identifier it cannot resolve is decided by the identifier alone, so no batch may change the
//! sentence. Both identifier tables are the crate's own — the alphabet rows
//! ([`crate::distribution_identifier_readability::alphabet_rows`]) and the rows written outside
//! that alphabet ([`crate::distribution_identifier_reads_its_alphabet::unreadable`]) — so this
//! suite varies the same forms the suite that drives every distribution-addressed route varies,
//! rather than carrying its own spelling of them.
//!
//! The wire harness and the create helpers are the crate's own.

use crate::distribution_identifier_readability::{alphabet_rows, live, CALLER_REFERENCE};
use crate::distribution_identifier_reads_its_alphabet::unreadable;
use crate::distribution_lifecycle::{
    batch_xml, create_invalidation, emulator, get_invalidation, Answer,
};
use crate::field_level_encryption_identifier_width_and_namespace::in_the_identifier_alphabet;

/// The sentence `NoSuchDistribution` carries for a distribution the account holds nothing for,
/// spelled out here rather than taken from the crate, so a reworded refusal fails this suite.
const ABSENT_DISTRIBUTION: &str = "The specified distribution does not exist.";

/// How this route words the identifier it reports it cannot READ: the model's own member for
/// the label a resource held UNDER a distribution arrives in.
const UNREADABLE_DISTRIBUTION_ID: &str = "DistributionId format is invalid";

/// The code both refusals carry — the one class this operation's model binds for an identifier
/// it cannot resolve. Never the argument class the model binds separately (`InvalidArgument`,
/// 400), which is what asserting this code on a 404 says.
const NO_SUCH_DISTRIBUTION: &str = "NoSuchDistribution";

/// The well-formed batch documents the identifier rows are crossed with: the wildcard every
/// object of the distribution, one single object, and several objects one of which carries a
/// character the documentation requires URL-encoded. Each states the quantity it really
/// carries, so none of them is refused on the document alone.
fn batches() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("the wildcard batch", vec!["/*"]),
        ("a single object", vec!["/index.html"]),
        (
            "several objects, one of them encoded",
            vec!["/a.html", "/b/c.html", "/d%20e.html"],
        ),
    ]
}

/// Assert one refusal: the status, the code (which is also what says the argument class was not
/// used), the exact sentence, and that nothing of the submitted identifier reached the wire.
fn assert_refusal(answer: &Answer, case: &str, id: &str, expected: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case}: expected HTTP 404, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        NO_SUCH_DISTRIBUTION,
        "case {case}: expected code {NO_SUCH_DISTRIBUTION}, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, expected,
        "case {case}: expected the sentence {expected:?}, got {message:?}"
    );
    // Both sentences are fixed text, so nothing of the submitted value reaches the wire. Read
    // of a value long enough for its presence to mean something: a single character of the
    // alphabet occurs in the envelope's own element names.
    if id.len() >= 8 {
        assert!(
            !answer.raw.contains(id),
            "case {case}: the refusal echoed {id:?} back in {}",
            answer.raw
        );
    }
}

/// RULE: every value written in the identifier alphabet that the account holds no distribution
/// under is the distribution reported ABSENT under `NoSuchDistribution`'s fixed sentence — for
/// every well-formed batch the request may carry, since the batch decides nothing about an
/// identifier that resolves to nothing.
#[tokio::test]
async fn an_absent_distribution_is_reported_absent_whatever_batch_the_request_carries() {
    let router = emulator();
    let live = live(&router).await;

    for (row, (case, id)) in alphabet_rows(&live.distribution_id, &live.control_id)
        .into_iter()
        .enumerate()
    {
        assert!(
            in_the_identifier_alphabet(&id),
            "case {case}: this row is meant to be written in the identifier alphabet, got {id:?}"
        );
        for (batch, (batch_case, paths)) in batches().into_iter().enumerate() {
            let case = format!("{case} ({id:?}) with {batch_case}");
            let reference = format!("absent-{row}-{batch}");
            let answer = create_invalidation(&router, &id, &batch_xml(&reference, &paths)).await;
            assert_refusal(&answer, &case, &id, ABSENT_DISTRIBUTION);
        }
    }
}

/// RULE, the other side of the split: a value written outside that alphabet was never an
/// identifier CloudFront issued, so the route reports the identifier it cannot READ — naming
/// the model's own member and nothing the request supplied — rather than a distribution that is
/// merely absent.
#[tokio::test]
async fn a_value_in_no_identifier_alphabet_is_the_identifier_that_cannot_be_read() {
    let router = emulator();
    let live = live(&router).await;

    for (row, (case, id)) in unreadable(&live.distribution_id, CALLER_REFERENCE)
        .into_iter()
        .enumerate()
    {
        assert!(
            !in_the_identifier_alphabet(&id),
            "case {case}: this row is meant to be written outside the identifier alphabet, got \
             {id:?}"
        );
        for (batch, (batch_case, paths)) in batches().into_iter().enumerate() {
            let case = format!("{case} ({id:?}) with {batch_case}");
            let reference = format!("unreadable-{row}-{batch}");
            let answer = create_invalidation(&router, &id, &batch_xml(&reference, &paths)).await;
            assert_refusal(&answer, &case, &id, UNREADABLE_DISTRIBUTION_ID);
        }
    }
}

/// RULE, as the positive control: the identifier the distribution really IS held under resolves,
/// so every batch above is served and the invalidation it created is readable back at the
/// identifier the create handed out — none of the refusals above touched the store.
#[tokio::test]
async fn the_identifier_the_distribution_is_held_under_serves_every_batch() {
    let router = emulator();
    let live = live(&router).await;

    for (batch, (batch_case, paths)) in batches().into_iter().enumerate() {
        let case = format!("the live distribution with {batch_case}");
        let reference = format!("live-{batch}");
        let created = create_invalidation(
            &router,
            &live.distribution_id,
            &batch_xml(&reference, &paths),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        assert!(
            !id.is_empty(),
            "case {case}: the create reported no invalidation identifier in {}",
            created.raw
        );
        let read = get_invalidation(&router, &live.distribution_id, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 reading the invalidation back, got {} with {}",
            read.status, read.raw
        );
        let read_id = read.root().text_at("Id").unwrap_or_default();
        assert_eq!(
            read_id, id,
            "case {case}: expected the invalidation {id:?} to be read back, got {read_id:?}"
        );
    }
}
