// SPDX-License-Identifier: CC0-1.0

//! The `Assertion` side of this service's store.
//!
//! An assertion exists only WITHIN a service — `CreateAssertion` "creates a resilience
//! assertion for a service" and every operation that reaches one names the service's ARN —
//! so the records live inside the parent service's record
//! ([`super::ServiceRecord::assertions`]), the way its service functions do: removing the
//! service removes its assertions with it, and no lookup can reach an assertion whose
//! service is gone.
//!
//! Every `Assertion` body this crate returns is projected from a record here through
//! [`AssertionRecord::to_assertion`], so the writer and the reader cannot drift from one
//! another or from what was stored.

use std::time::SystemTime;

use crate::ops::types::Assertion;

use super::{to_date_time, ServiceRecord};

/// `AssertionSource`'s value for an assertion a CALLER created, as opposed to the
/// `AI_GENERATED` one the assessment pipeline produces — the two values
/// `ASSERTION_SOURCE_VALUES` holds. `Assertion$source` "indicat[es] whether it was
/// AI-generated or created by a user", and a `CreateAssertion` call is a user creating one.
pub const ASSERTION_SOURCE_USER: &str = "USER";

/// The ceiling on how many assertions one service will hold, so an unauthenticated caller
/// cannot grow the process's memory without limit by repeating `CreateAssertion`.
///
/// CHOSEN, not documented: the model publishes no per-service assertion quota, so this
/// mirrors the per-service ceiling the `ServiceFunction` side of this store uses
/// ([`super::MAX_SERVICE_FUNCTIONS_PER_SERVICE`]). `CreateAssertion` refuses a create that
/// would pass it with the `ServiceQuotaExceededException` (HTTP 402) its model declares.
/// The per-service idempotency index is bounded by the same ceiling: a token already in it
/// either replays the stored assertion or is refused, never inserting a second entry.
pub const MAX_ASSERTIONS_PER_SERVICE: usize = super::MAX_SERVICE_FUNCTIONS_PER_SERVICE;

/// One assertion created on a service, and the sole source of every `Assertion` body this
/// crate returns.
#[derive(Debug, Clone)]
pub struct AssertionRecord {
    /// The minted `assertionId`; also this record's key in
    /// [`super::ServiceRecord::assertions`].
    pub assertion_id: String,
    pub text: String,
    /// Which side produced this assertion — see [`ASSERTION_SOURCE_USER`].
    pub source: String,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// Creation order, from the store's monotonic counter. A listing scans by this and its
    /// `nextToken` names a position in it, so a page resumes the same scan even after an
    /// assertion is deleted.
    pub sequence: u64,
}

impl AssertionRecord {
    /// This record as the `Assertion` shape `CreateAssertion` and `ListAssertions` return.
    ///
    /// `serviceArn` is the ARN of the service this assertion is stored under — the
    /// assertion is nested in that service's record, so it is passed in rather than
    /// duplicated in every record.
    pub fn to_assertion(&self, service_arn: &str) -> Assertion {
        Assertion {
            service_arn: service_arn.to_string(),
            assertion_id: self.assertion_id.clone(),
            text: self.text.clone(),
            source: self.source.clone(),
            created_at: Some(to_date_time(self.created_at)),
            updated_at: Some(to_date_time(self.updated_at)),
        }
    }
}

/// What a consumed `CreateAssertion` `clientToken` remembers: which assertion it created,
/// and the text it created it for.
///
/// `text` is the only thing a `CreateAssertion` request says about the assertion to create
/// beyond the service it belongs to (the one the token is remembered under), so two
/// requests are the SAME create exactly when their text matches — which is what decides
/// whether a repeated token replays the first call or is refused.
#[derive(Debug, Clone)]
pub struct AssertionIdempotencyRecord {
    pub assertion_id: String,
    pub text: String,
}

impl ServiceRecord {
    /// This service's assertions in creation order — what a listing enumerates before it
    /// projects them.
    pub fn assertions_in_creation_order(&self) -> Vec<&AssertionRecord> {
        let mut assertions: Vec<&AssertionRecord> = self.assertions.values().collect();
        assertions.sort_by_key(|assertion| assertion.sequence);
        assertions
    }
}

/// Mint a fresh `assertionId`.
///
/// `Assertion$assertionId` is typed `Uuid` (`len 36..36` with the version and variant
/// nibbles the pattern pins), which the version this crate's one minting path stamps
/// satisfies; none of its bits comes from the request, so two
/// assertions carrying the same text never collide.
pub fn mint_assertion_id(region: &str) -> String {
    super::mint_uuid(region)
}
