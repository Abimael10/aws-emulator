// SPDX-License-Identifier: CC0-1.0

//! The HTTP status this API answers a modelled failure with.
//!
//! On `awsJson1_0` the CLASS of a failure travels in the body's `__type`, not in the status
//! line, so the status a service answers an error with is its own choice and need not be the
//! `@httpError` its model documents for the shape. Route 53 ARC's cluster (data-plane) API
//! takes that choice: a request it refuses because the routing control the caller named is
//! not one it serves is answered `ResourceNotFoundException` at HTTP
//! [`RESOURCE_NOT_FOUND_STATUS`] — the same status as its validation refusals — while the
//! model declares `@httpError` 404 for that shape. What the service ANSWERS is the contract a
//! client sees, so it is what this emulator answers; the modelled figure is kept for every
//! shape this API has not been observed to answer differently.
//!
//! One mapping for the whole service, read by the error channel every operation returns
//! through ([`crate::scaffolded::Scaffolded`]), so two operations cannot answer one error
//! class with two statuses.

/// The `__type` of the modelled error a request naming a routing control or control panel
/// this API does not serve is refused with.
pub const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// The status that refusal is answered with: the client-error status this API states every
/// observed refusal in, rather than the `@httpError` 404 the shape carries in the model.
///
/// A client reads the class from `__type`, which still names the not-found shape, so the
/// refusal is as distinguishable as the model makes it.
pub const RESOURCE_NOT_FOUND_STATUS: u16 = 400;

/// The status this API answers the modelled error `shape_name` with: its own where it
/// states one, and `modelled` — the status the model declares for the shape — everywhere
/// else.
#[must_use]
pub fn of(shape_name: &str, modelled: u16) -> u16 {
    match shape_name {
        RESOURCE_NOT_FOUND_EXCEPTION => RESOURCE_NOT_FOUND_STATUS,
        _ => modelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: only the not-found refusal is stated at this API's own status; every other
    /// modelled error keeps the status its shape carries in the model.
    #[test]
    fn only_the_not_found_refusal_departs_from_the_modelled_status() {
        // (case, the shape, the status the model declares for it, the status answered)
        let rows = [
            (
                "the not-found refusal",
                RESOURCE_NOT_FOUND_EXCEPTION,
                404,
                RESOURCE_NOT_FOUND_STATUS,
            ),
            ("a validation refusal", "ValidationException", 400, 400),
            ("an authorization refusal", "AccessDeniedException", 403, 403),
            (
                "a quota refusal",
                "ServiceLimitExceededException",
                402,
                402,
            ),
            ("a throttling refusal", "ThrottlingException", 429, 429),
            ("a conflict", "ConflictException", 409, 409),
            (
                "an unavailable endpoint",
                "EndpointTemporarilyUnavailableException",
                503,
                503,
            ),
            ("a server fault", "InternalServerException", 500, 500),
        ];
        for (case, shape, modelled, expected) in rows {
            let actual = of(shape, modelled);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected status {expected}, got {actual}"
            );
        }
    }
}
