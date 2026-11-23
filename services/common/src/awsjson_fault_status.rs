// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the HTTP status an `awsJson` service answers a modelled error at:
//! the error's FAULT, not the exact code its shape's `@httpError` trait names.
//!
//! # The rule
//!
//! `@httpError` is an HTTP-BINDING trait, and the `awsJson1_0` / `awsJson1_1` protocols
//! bind nothing of an operation to the transport — the operation travels in
//! `x-amz-target` and every member in the body. So an awsJson front end reports an
//! error's FAULT rather than a per-shape status: every modelled CLIENT error
//! (`@error: "client"`) is HTTP 400, with the body's `__type` as the only thing that
//! distinguishes one from another, and a SERVER error keeps its 5xx so a client's retry
//! classifier still sees a server fault.
//!
//! [`wire_http_status`] states that as a classification of the MODELLED status rather
//! than as a list of shape names, so an error shape a service starts answering later is
//! covered without touching it.
//!
//! # Why the status matters
//!
//! An SDK's retry classifier branches on the status, so a service answering a client
//! fault at one status where the real one answers another makes a caller retry — or give
//! up — differently than it would against AWS. Stating the classification once is what
//! keeps three services from drifting apart on it.
//!
//! # Whose rule it is
//!
//! Whether a GIVEN service answers at the fault or at the status its model declares stays
//! that service's own decision, grounded in its own recorded wire; this module is what a
//! service that has made that decision reads, instead of spelling the classification
//! again. The recordings behind today's callers are per-fault and each is a 4xx shape
//! answered as a 400:
//!
//! * Amazon EVS (`awsJson1_0`) answers `ResourceNotFoundException` — `@httpError(404)` in
//!   its published model — with HTTP 400;
//! * AWS HealthLake (`awsJson1_0`) answers its own `ResourceNotFoundException`, also
//!   `@httpError(404)`, with HTTP 400;
//! * Service Quotas (`awsJson1_1`) answers a lookup that names no quota, service or
//!   increase request with HTTP 400 carrying `NoSuchResourceException`, a shape its model
//!   annotates `@httpError(404)`. Its declared client statuses are not HTTP semantics at
//!   all but legacy error numbers — `TagPolicyViolationException` is `@httpError(401)`
//!   ("the specified tag is a reserved word"), `InvalidResourceStateException` is
//!   `@httpError(405)` ("the resource is in an invalid state") — so a front end putting
//!   them on the wire would tell a client to re-authenticate, or that the method is not
//!   allowed, for a request whose fault is simply the caller's input.
//!
//! A service whose own recording shows the MODELLED statuses on the wire is deliberately
//! not a caller: AWS CodeStar connections, for one, answers 404 for its
//! `ResourceNotFoundException` and 429 for its `LimitExceededException`, and so keeps the
//! status its shapes declare.
//!
//! Nor is every other fold in the tree a copy of this one. A service that folds only the 4xx
//! class and keeps everything else — OpenSearch Serverless is the case to compare against —
//! answers a status outside 400..500 with that status rather than with 400, which is a
//! different classification and not this rule; such a service states its own fold and is left
//! alone here. Only a service whose recording shows the FAULT on the wire for every modelled
//! error reads this module.

/// The HTTP status an `awsJson` service answers a modelled error at, from the status that
/// error's own shape declares.
///
/// A modelled status below 500 is a CLIENT fault and goes on the wire as 400; a 5xx is a
/// SERVER fault and is kept, so `@httpError(401)`/`(402)`/`(403)`/`(404)`/`(405)`/`(409)`/
/// `(429)` all render as 400 while `@httpError(500)` stays 500.
#[must_use]
pub fn wire_http_status(modeled_status: u16) -> u16 {
    if modeled_status >= 500 {
        modeled_status
    } else {
        400
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classification over the `@httpError` values the callers' models actually
    /// declare, plus each published bound of the client/server split and one past it.
    ///
    /// Each row names where its modelled status comes from, so no value here is a number
    /// written for its own sake. The shape rows are
    /// `smithy_query(query="shape", shape="...")` against the three callers' shipped
    /// models: `com.amazonaws.servicequotas#TagPolicyViolationException` (401),
    /// `com.amazonaws.evs#ServiceQuotaExceededException` (402),
    /// `com.amazonaws.healthlake#AccessDeniedException` and
    /// `com.amazonaws.servicequotas#AccessDeniedException` (403),
    /// `com.amazonaws.evs#ResourceNotFoundException`,
    /// `com.amazonaws.healthlake#ResourceNotFoundException` and
    /// `com.amazonaws.servicequotas#NoSuchResourceException` (404),
    /// `com.amazonaws.servicequotas#InvalidResourceStateException` (405),
    /// `com.amazonaws.healthlake#ConflictException` and
    /// `com.amazonaws.servicequotas#QuotaExceededException` (409),
    /// `com.amazonaws.evs#ThrottlingException` and
    /// `com.amazonaws.servicequotas#TooManyRequestsException` (429),
    /// `com.amazonaws.evs#ValidationException` (400), and
    /// `com.amazonaws.evs#InternalServerException`,
    /// `com.amazonaws.healthlake#InternalServerException` and
    /// `com.amazonaws.servicequotas#ServiceException` (500).
    const ROWS: &[(&str, u16, u16)] = &[
        ("@httpError(400), an invalid argument", 400, 400),
        ("@httpError(401), a reserved tag word", 401, 400),
        ("@httpError(402), a service quota exceeded", 402, 400),
        ("@httpError(403), access denied", 403, 400),
        ("@httpError(404), a resource that does not exist", 404, 400),
        ("@httpError(405), an invalid resource state", 405, 400),
        ("@httpError(409), a conflict", 409, 400),
        ("@httpError(429), throttling", 429, 400),
        ("@httpError(500), an internal server error", 500, 500),
        ("the lowest client status", 400, 400),
        ("the highest client status", 499, 400),
        ("the lowest server status", 500, 500),
        ("a server status past the lowest", 503, 503),
    ];

    #[test]
    fn a_modelled_client_status_renders_as_400_and_a_server_status_is_kept() {
        for (case, modeled, expected) in ROWS {
            let actual = wire_http_status(*modeled);
            assert_eq!(
                actual, *expected,
                "case {case:?}: a modelled {modeled} must render as {expected}, got {actual}"
            );
        }
    }

    /// The classification is total, and it splits at 500 and nowhere else: every status below
    /// 500 a shape could declare is answered with 400, and every 5xx is answered with itself.
    ///
    /// Each half is stated as its own expectation rather than recomputed from the body, so a
    /// fold written with a different boundary — or one that rewrote a server status — fails
    /// here.
    #[test]
    fn the_split_is_at_500_and_nowhere_else() {
        for modeled in 100_u16..500 {
            let actual = wire_http_status(modeled);
            assert_eq!(
                actual, 400,
                "case a modelled {modeled}, below the server class: expected 400, got {actual}"
            );
        }
        for modeled in 500_u16..=599 {
            let actual = wire_http_status(modeled);
            assert_eq!(
                actual, modeled,
                "case a modelled {modeled}, a server fault: expected it kept, got {actual}"
            );
        }
    }
}
