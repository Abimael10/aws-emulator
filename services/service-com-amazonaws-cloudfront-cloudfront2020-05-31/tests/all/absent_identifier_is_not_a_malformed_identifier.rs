// SPDX-License-Identifier: CC0-1.0

//! A well-formed identifier no resource answers to is the resource reported ABSENT — never an
//! identifier the service says it cannot read — on the two routes this suite drives, end to end
//! over HTTP.
//!
//! The rule under test: where a path identifier arrives in a member the model constrains no
//! further than `string` (`smithy_query(query="operation",operation="CreateInvalidation,\
//! GetOriginAccessControl")` reports `CreateInvalidation$DistributionId` and
//! `GetOriginAccessControl$Id` as required `@httpLabel`s with no `@pattern` and no `@length`,
//! each route carrying its own not-found error — `NoSuchDistribution` 404 and
//! `NoSuchOriginAccessControl` 404), a value WRITTEN IN the alphabet CloudFront writes an
//! identifier in is resolved against the store and, when nothing answers to it, reported as
//! that route's resource being ABSENT under that error class's fixed sentence. The width of
//! such a value and the namespace letter it opens with decide nothing: the identifiers this
//! emulator MINTS are one form CloudFront writes one in, never the only form, and WHICH
//! identifier was ever issued — and for which kind — is a property of the store alone. So the
//! malformed-identifier sentence, which tells a caller its own identifier handling is broken (a
//! non-retryable programming error rather than a resource to re-resolve or create), is never
//! the answer to such a value on either route.
//!
//! This suite is the CROSS-KIND pairing: ONE table of identifier forms
//! ([`crate::distribution_identifier_readability::alphabet_rows`]) driven through two different
//! resource kinds, each answering its own code and its own sentence, because the rule is one
//! rule rather than each route's own habit. What each kind answers a value written OUTSIDE that
//! alphabet is its own contract and belongs to the suites that own it
//! (`distribution_identifier_reads_no_form`, `origin_access_control_identifier_readability`).
//!
//! The rows span what the rule generalises over: the width this emulator mints at (never
//! issued), one character short of it, one character past it, far wider than it, a single
//! character, a namespace letter no kind of this service mints, a body of digits alone under no
//! letter, and the really-minted identifier of the OTHER kind in the table. Every row asserts
//! the status, the code, the exact sentence, that no malformed-identifier sentence was served
//! and that the submitted value was not echoed back; both live resources are exercised
//! afterwards, so no refusal here touched the store.
//!
//! The wire harness and the create helpers are the crate's own.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_identifier_readability::{alphabet_rows, live, Live};
use crate::distribution_lifecycle::{batch_xml, create_invalidation, emulator, Answer};
use crate::field_level_encryption_identifier_width_and_namespace::in_the_identifier_alphabet;
use crate::origin_access_control_lifecycle::get_control;

/// The sentences these two routes answer a resource the store holds nothing for, spelled out
/// here rather than taken from the crate, so a reworded refusal fails this suite.
const NO_SUCH_DISTRIBUTION: &str = "The specified distribution does not exist.";
const NO_SUCH_CONTROL: &str = "The specified origin access control does not exist.";

/// How an identifier the service reports it cannot READ is worded, for both member names these
/// routes could name — the answer this suite asserts is NEVER served for a value written in the
/// identifier alphabet.
const UNREADABLE_SENTENCES: [&str; 2] =
    ["DistributionId format is invalid", "Id format is invalid"];

/// One route addressed by an identifier the model constrains no further than `string`, named
/// for the resource kind it addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `POST /2020-05-31/distribution/{DistributionId}/invalidation`.
    CreateInvalidation,
    /// `GET /2020-05-31/origin-access-control/{Id}`.
    GetOriginAccessControl,
}

/// Both routes: one mutation addressed by a distribution, one read addressed by an origin
/// access control — two resource kinds, two codes, two sentences, one rule.
const ROUTES: &[Route] = &[Route::CreateInvalidation, Route::GetOriginAccessControl];

impl Route {
    /// The modelled error code this route answers an identifier it cannot resolve.
    fn code(self) -> &'static str {
        match self {
            Self::CreateInvalidation => "NoSuchDistribution",
            Self::GetOriginAccessControl => "NoSuchOriginAccessControl",
        }
    }

    /// The sentence this route's not-found class carries.
    fn absent(self) -> &'static str {
        match self {
            Self::CreateInvalidation => NO_SUCH_DISTRIBUTION,
            Self::GetOriginAccessControl => NO_SUCH_CONTROL,
        }
    }

    /// The really-minted identifier of the kind this route does NOT address.
    fn other_kinds_identifier(self, live: &Live) -> String {
        match self {
            Self::CreateInvalidation => live.control_id.clone(),
            Self::GetOriginAccessControl => live.distribution_id.clone(),
        }
    }

    /// Address `id` through this route. The invalidation carries a well-formed batch under
    /// `reference`, so what the call is answered with is decided by the identifier in its path
    /// and nothing else.
    async fn send(self, router: &MultiServiceRouter, reference: &str, id: &str) -> Answer {
        match self {
            Self::CreateInvalidation => {
                create_invalidation(router, id, &batch_xml(reference, &["/*"])).await
            }
            Self::GetOriginAccessControl => get_control(router, id).await,
        }
    }
}

/// RULE: on both routes, every value written in the identifier alphabet that the account holds
/// no resource of the addressed kind under is that resource reported ABSENT — the route's own
/// 404 and code, under its own fixed sentence — and never the identifier the service says it
/// cannot read.
#[tokio::test]
async fn a_well_formed_absent_identifier_is_the_absent_resource_on_both_routes() {
    let router = emulator();
    let live = live(&router).await;

    for route in ROUTES {
        let other_kind = route.other_kinds_identifier(&live);
        for (row, (case, id)) in alphabet_rows(&live.distribution_id, &other_kind)
            .into_iter()
            .enumerate()
        {
            let case = format!("{route:?} with {case} ({id:?})");
            assert!(
                in_the_identifier_alphabet(&id),
                "case {case}: this row is meant to be written in the identifier alphabet"
            );
            let answer = route
                .send(&router, &format!("absent-{route:?}-{row}"), &id)
                .await;
            assert_eq!(
                answer.status, 404,
                "case {case}: expected HTTP 404, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                route.code(),
                "case {case}: expected code {}, got {:?} with {}",
                route.code(),
                answer.error_code(),
                answer.raw
            );
            let message = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                message,
                route.absent(),
                "case {case}: expected the sentence {:?}, got {message:?}",
                route.absent()
            );
            for unreadable in UNREADABLE_SENTENCES {
                assert_ne!(
                    message, unreadable,
                    "case {case}: the route answered the malformed-identifier sentence \
                     {unreadable:?} for a well-formed identifier it simply does not hold"
                );
            }
            // The sentence is fixed text, so nothing of the value submitted reaches the wire.
            // Read of a value long enough for its presence to mean something: a one-character
            // row says nothing about echoing, since a single letter of the alphabet occurs in
            // the envelope's own element names.
            if id.len() >= 8 {
                assert!(
                    !answer.raw.contains(id.as_str()),
                    "case {case}: the refusal echoed {id:?} back in {}",
                    answer.raw
                );
            }
        }
    }

    // None of those refusals touched the store: both live resources are still served at their
    // own identifiers.
    let invalidated = Route::CreateInvalidation
        .send(&router, "absent-not-malformed-control", &live.distribution_id)
        .await;
    assert_eq!(
        invalidated.status, 201,
        "case the live distribution: expected HTTP 201 from CreateInvalidation, got {} with {}",
        invalidated.status, invalidated.raw
    );
    let read = get_control(&router, &live.control_id).await;
    assert_eq!(
        read.status, 200,
        "case the live origin access control: expected HTTP 200, got {} with {}",
        read.status, read.raw
    );
}
