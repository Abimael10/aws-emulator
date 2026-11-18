// SPDX-License-Identifier: CC0-1.0

//! The HTTP STATUS this service puts a modelled error on the wire at.
//!
//! Amazon Keyspaces's stream APIs speak one RPC protocol, `awsJson1_0`, whose envelope
//! discriminates a failure by the error TYPE in the body (`__type`, plus
//! `ValidationException`'s own `errorCode`) rather than by the transport status: the status
//! a CLIENT-side failure travels with is the protocol's own 400. That is what the recorded
//! responses of the real service carry — a stream ARN no stream is held under answers HTTP
//! 400 with `ResourceNotFoundException`, a value of the wrong form answers HTTP 400 with
//! `ValidationException` — and it is what the framework's own pre-dispatch refusal already
//! answers with on this protocol, so a handler-returned client error and a boundary refusal
//! agree. Amazon Keyspaces itself, the control plane these streams follow and the other
//! service signing as `cassandra`, states the same rule for the same reason in
//! `services/service-com-amazonaws-keyspaces-keyspacesservice/src/scaffolded.rs`.
//!
//! So the status is decided HERE, once, for every operation of this service: all four reach
//! it through the one error channel ([`crate::scaffolded::Scaffolded`]), which cannot
//! disagree with itself the way four handlers could.

/// The status this service puts on the wire for ANY client-side failure.
pub const CLIENT_FAULT_STATUS: u16 = 400;

/// The status a modelled error travels with on this service's wire, given the `@httpError`
/// status its shape is annotated with.
///
/// Every CLIENT error (any 4xx: `ResourceNotFoundException`'s 404, `AccessDeniedException`'s
/// 403, `ThrottlingException`'s 429, `ValidationException`'s own 400) collapses onto
/// [`CLIENT_FAULT_STATUS`], because the per-shape annotation is not what this RPC service's
/// wire carries and a client that classifies by HTTP status would otherwise read a
/// transport/routing failure (404) where the service reported a modelled API error. Every
/// other status — a server fault's 5xx, the scaffold channel's 501 — is left alone, so a
/// retry classifier still tells a server fault from a client one.
#[must_use]
pub fn wire_status(modelled_status: u16) -> u16 {
    if (400..500).contains(&modelled_status) {
        CLIENT_FAULT_STATUS
    } else {
        modelled_status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every client fault of this service is served at 400, and every other status is
    /// the one the modelled shape declares.
    ///
    /// The rows span every error shape this service's model declares, each with the
    /// `@httpError` status the model gives it, plus the scaffold channel's 501 and the two
    /// edges of the 4xx band.
    #[test]
    fn every_client_fault_is_served_at_one_status() {
        // (the class, the model's @httpError for it, the status served)
        let rows = [
            ("ValidationException", 400, 400),
            ("AccessDeniedException", 403, 400),
            ("ResourceNotFoundException", 404, 400),
            ("ThrottlingException", 429, 400),
            ("the lowest 4xx", 400, 400),
            ("the highest 4xx", 499, 400),
            ("InternalServerException", 500, 500),
            ("the scaffold channel", 501, 501),
        ];
        for (case, modelled, served) in rows {
            let answered = wire_status(modelled);
            assert_eq!(
                answered, served,
                "case {case}: expected HTTP {served} for a modelled {modelled}, got {answered}"
            );
        }
    }
}
