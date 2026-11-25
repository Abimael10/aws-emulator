// SPDX-License-Identifier: CC0-1.0

//! What an operation ADDRESSED by a distribution identifier answers a value written in NO
//! alphabet CloudFront issues an identifier in, end to end over HTTP.
//!
//! The rule under test: an operation addressed by a distribution identifier reads of the
//! submitted value the ALPHABET CloudFront writes an entity identifier in — decimal digits and
//! uppercase ASCII letters, around no separator — because
//! `smithy_query(query="operation",operation="GetDistribution,GetDistributionConfig,\
//! DeleteDistribution,CreateInvalidation,ListInvalidations,GetInvalidation,\
//! GetMonitoringSubscription")` reports each one's `Id`/`DistributionId` as a required
//! `@httpLabel` on a plain `string` and `NoSuchDistribution` (404) as the one error class
//! covering an identifier it cannot resolve. One status and one code therefore carry BOTH
//! conditions and the SENTENCE tells them apart: a value of that alphabet names a distribution
//! the account may hold, so one it holds none under is the distribution reported absent (the
//! sibling suite `distribution_identifier_reads_no_form`), while a value written outside that
//! alphabet was never an identifier at all and is the one the route reports it cannot READ —
//! `<member> format is invalid`, naming the MODEL's member (`Id`, or `DistributionId` on the
//! routes addressed by the distribution a resource is held under) and nothing the request
//! supplied.
//!
//! The rows span what the rule generalises over: a wholly lower-cased identifier, one with a
//! single lower-case character, a UUID, a bare word, a caller reference, a domain name and
//! characters of the alphabet around a separator. Every row is driven through all seven
//! operations addressed by a distribution identifier, with the live distribution as the
//! positive control.
//!
//! The wire harness, the create helpers and the route table are the crate's own.

use crate::distribution_identifier_reads_no_form::{live, Live, NO_SUCH_DISTRIBUTION, OPS};
use crate::distribution_lifecycle::{call, emulator, API_VERSION};

/// The caller reference the live distribution of this suite is created under, which one row
/// then submits where its identifier belongs.
const REFERENCE: &str = "id-alphabet";

/// Every value this suite submits that is written in NO alphabet CloudFront issues an
/// identifier in, so no route can read it as an identifier at all.
fn unreadable(live: &Live) -> Vec<(String, String)> {
    let held = &live.distribution_id;
    vec![
        (
            "the distribution's own identifier in lower case".to_string(),
            held.to_lowercase(),
        ),
        (
            "the distribution's own identifier with one lower-case character".to_string(),
            format!("{}z", &held[..held.len() - 1]),
        ),
        (
            "a UUID, the namespace the policy kinds are keyed by".to_string(),
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        ("a bare word".to_string(), "not-an-identifier".to_string()),
        (
            "the distribution's caller reference rather than its identifier".to_string(),
            REFERENCE.to_string(),
        ),
        (
            "a domain name rather than an identifier".to_string(),
            "d111111abcdef8.cloudfront.net".to_string(),
        ),
        (
            "characters of the alphabet around a separator".to_string(),
            "E1AAAA_AAAAAAA".to_string(),
        ),
    ]
}

/// RULE: a value written outside the alphabet CloudFront writes an identifier in was never an
/// identifier it issued, so every route addressed by a distribution identifier reports the
/// identifier it cannot READ — the not-found status and code its model binds, under the
/// sentence that names the model's own member and nothing from the request.
#[tokio::test]
async fn a_value_written_in_no_identifier_alphabet_is_the_identifier_that_cannot_be_read() {
    let router = emulator();
    let live = live(&router, REFERENCE).await;

    for op in OPS {
        let expected = format!("{} format is invalid", op.member());
        for (case, id) in unreadable(&live) {
            let case = format!("{op:?} with {case} ({id:?})");
            let answer = op.send(&router, &id).await;
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
            assert!(
                !answer.raw.contains(id.as_str()),
                "case {case}: the refusal echoed {id:?} back in {}",
                answer.raw
            );
        }
    }
}

/// RULE, as the positive control: the identifier the distribution really IS held under is
/// written in that alphabet and resolved by every one of those routes, so none of them refuses
/// a call it has to answer — and the distribution survived every refusal above.
#[tokio::test]
async fn the_identifier_the_distribution_is_held_under_is_resolved_by_every_route() {
    let router = emulator();
    let live = live(&router, "id-alphabet-live").await;

    for op in OPS {
        let case = format!("{op:?} with the identifier the distribution is held under");
        let answer = op.send(&router, &live.distribution_id).await;
        assert_ne!(
            answer.error_code(),
            NO_SUCH_DISTRIBUTION,
            "case {case}: expected the route to resolve the distribution, got {} with {}",
            answer.status,
            answer.raw
        );
    }

    // And the configuration is still readable at that identifier after every call above.
    let read = call(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution/{}/config", live.distribution_id),
        None,
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case the live distribution: expected HTTP 200, got {} with {}",
        read.status, read.raw
    );
}
