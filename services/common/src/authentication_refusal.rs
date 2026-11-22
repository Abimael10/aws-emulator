// SPDX-License-Identifier: CC0-1.0

//! How an emulated service words the request the emulator's authentication layer refused.
//!
//! The emulator verifies every signature; a service never checks one itself. What the layer
//! hands a service is ONE structural refusal — authentication refused the caller — whichever
//! way the credential failed, and the generic code it carries (`AccessDenied`) is an
//! AUTHORIZATION denial, which is not what AWS answers here.
//!
//! AWS splits the refusal three ways, and the split is load-bearing:
//!
//! * a request that presents NO credential at all — neither of SigV4's two presentations
//!   (`crate::sigv4_presentation`) — never named a caller to look up, and is answered with
//!   the service's missing-credential code, the one code that says "you sent no
//!   credentials" and sends a client to credential DISCOVERY rather than to key validity.
//!   It is never answered with the unknown-credential code: nothing was presented, so
//!   nothing could be unknown, and a service that words that class at its own front door
//!   (naming none here) keeps the answer it already has;
//! * an access key id that resolves to NO credential — never issued, deleted, or presented
//!   to a service it was not scoped for — and a SECURITY TOKEN that is not the one issued
//!   for the credential presenting it, failed AUTHENTICATION: no principal was found, so
//!   nothing could be authorized. The answer is the service's own unknown-credential code,
//!   which is what makes an SDK re-resolve its credential chain (refresh a stale profile,
//!   re-read instance metadata, fail fast on a mistyped key id). One code, two sentences:
//!   AWS reports the two from different stages of its front door, so a token that was
//!   presented and resolved to nothing carries [`INVALID_SECURITY_TOKEN_MESSAGE`] and a key
//!   id that resolves to nothing — a temporary credential presented with no token among
//!   them — carries [`UNRESOLVED_CREDENTIAL_MESSAGE`];
//! * a key id that DOES resolve, whose signature does not verify under that credential's
//!   secret, failed the SIGNATURE: the caller is known, and the service's signature code
//!   points at the secret half of the pair rather than at the key id.
//!
//! `AccessDenied` is neither: it is the answer a KNOWN principal gets for an action its
//! policies do not permit, and it makes a client report a policy problem and stop. With the
//! codes collapsed, a wrong or stale key is diagnosed as a missing permission and no
//! credential-refresh path in a client is ever exercised.
//!
//! Which failure a refusal is, the authentication layer already decided — it is the code it
//! reports — and the one seam handed that code is the service's own definition, where
//! `refusal_response` renders it. [`AuthenticationRefusalDefinition`] wraps a generated
//! definition there and restates the refusal in that service's vocabulary: the codes the
//! service names ([`StatedAuthenticationRefusal`]) carrying the fixed sentences AWS's SigV4
//! layer produces. The codes differ by protocol family and by service, so they are the
//! service's to name and only the rule lives here.
//!
//! A service whose answer depends on the REQUEST rather than on the layer's verdict — the
//! signing diagnostics S3 reports, or a credential a service's own store refuses — words it
//! through [`smithy_server_runtime::Service::override_refusal`] instead, and this wrapper
//! defers to that for the verdict the layer leaves UNCLASSIFIED: a refusal worded there is
//! passed through untouched.
//!
//! What that seam cannot defer is a failure the layer itself NAMED. `override_refusal` is
//! handed the structural refusal alone — authentication refused the caller — so a service
//! wording it can only re-derive a class from the credential the request presented, and a
//! derivation cannot overrule a verdict it cannot see: a security token the layer resolved to
//! no issued session is an unknown credential however well the request was signed, and a
//! request that presented nothing is missing a credential and not carrying an unknown one.
//! Those two are therefore restated here, from the layer's own code and the request's
//! presentation, ahead of whatever the service chose ([`StatedAuthenticationRefusal::restated`]).
//!
//! Nothing here reads a credential, a secret or any state: one code is translated into
//! another, and the request is read only for WHICH presentation it carries (never a value in
//! it). The HTTP status and the error envelope stay with the protocol that owns them.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::ProtocolError;
use smithy_server_runtime::{
    ErasedServiceDefinition, OperationRoutingTable, Refused, ServerHttpProtocol, ServiceIdentity,
    WireProtocol,
};

/// The code the authentication layer reports when the request's access key id resolved to no
/// credential this emulator holds. It is the layer's own generic wording, which is why a
/// service restates it: no AWS service answers an unknown credential with it.
const REPORTED_UNRESOLVED_CREDENTIAL: &str = "AccessDenied";

/// The code it reports when the key id resolved and the signature computed under that
/// credential's secret is not the one the request carried.
const REPORTED_SIGNATURE_MISMATCH: &str = "SignatureDoesNotMatch";

/// The code it reports when the request's SECURITY TOKEN is not the one issued for the
/// credential that presented it — a temporary credential presented with a token that belongs
/// to no live session, or with none at all. The layer resolved the token itself, which is a
/// verdict about the CREDENTIAL and not about the signature over the request.
const REPORTED_INVALID_SECURITY_TOKEN: &str = "InvalidClientTokenId";

/// How AWS words a credential that resolves to nothing: fixed text with no variable part, so
/// a caller presenting an unknown key learns nothing about this account.
pub const UNRESOLVED_CREDENTIAL_MESSAGE: &str =
    "The security token included in the request is invalid.";

/// How AWS words a SECURITY TOKEN that was PRESENTED and is not the one issued for the
/// credential presenting it.
///
/// The same words as the sentence above and not the same sentence: AWS reports the two
/// conditions from different stages of its front door — resolving the key id, and resolving
/// the token presented with it — and each is pinned by its own recorded answer, which differ
/// in the closing punctuation. The emulator's own verifier reports the token stage with this
/// wording too (`vendor/server/src/request_authentication.rs`), so a refusal restated from
/// that verdict keeps the sentence the stage it came from carries.
pub const INVALID_SECURITY_TOKEN_MESSAGE: &str =
    "The security token included in the request is invalid";

/// How AWS's SigV4 layer words a signature that does not verify: the fixed sentence for that
/// one condition, which names the half of the credential pair that is wrong.
pub const SIGNATURE_MISMATCH_MESSAGE: &str =
    "The request signature we calculated does not match the signature you provided. Check \
     your AWS Secret Access Key and signing method. Consult the service documentation for \
     details.";

/// The codes one service answers the authentication failures with.
///
/// Each is the service's own: the names differ by protocol family — the awsQuery identity
/// services answer `InvalidClientTokenId` / `SignatureDoesNotMatch`, an awsJson service
/// `UnrecognizedClientException` / `InvalidSignatureException` — and a single code cannot be
/// right for all of them.
#[derive(Debug, Clone, Copy)]
pub struct StatedAuthenticationRefusal {
    /// What this service calls a request whose access key id resolved to no credential, and
    /// whose security token resolved to no issued session.
    pub unresolved_credential: &'static str,
    /// What it calls a request whose signature did not verify under a credential that did
    /// resolve.
    pub signature_mismatch: &'static str,
    /// What it calls a request that presented NO credential at all, where this service's own
    /// front door words that class.
    ///
    /// `None` for a service whose own layer words it — one answering it at another status, or
    /// naming the class in a namespace of its own — so this one does not state it a second
    /// time; such a service keeps its own answer.
    pub missing_credential: Option<&'static str>,
}

impl StatedAuthenticationRefusal {
    /// How this service words the refusal the authentication layer reported as `reported`,
    /// for a request that presented `presented`
    /// ([`crate::sigv4_presentation`], the one thing about the request this reads).
    ///
    /// The layer reports its generic unknown-credential verdict for a key id that resolved to
    /// nothing AND for a request that offered nothing to resolve, so the two are told apart
    /// by WHAT was presented: a credential that WAS presented is the unknown-credential
    /// class, and a request that presented none is the missing-credential class, which
    /// [`Self::restated`] answers where this service names a class for it and which keeps the
    /// words it already has where this service names none — a caller that sent no credentials
    /// is never told the key it did not send is unknown.
    ///
    /// `None` for anything else: a code that is already a service's own (a pre-dispatch
    /// refusal a service rendered itself) keeps the words it was given rather than being
    /// mapped a second time.
    #[must_use]
    pub fn stated(
        &self,
        reported: &str,
        presented: crate::sigv4_presentation::PresentedCredential,
    ) -> Option<Refused> {
        let (code, message) = match reported {
            REPORTED_UNRESOLVED_CREDENTIAL if presented.credential => (
                self.unresolved_credential,
                UNRESOLVED_CREDENTIAL_MESSAGE,
            ),
            REPORTED_SIGNATURE_MISMATCH => (self.signature_mismatch, SIGNATURE_MISMATCH_MESSAGE),
            _ => return None,
        };
        Some(Refused {
            code: code.to_string(),
            message: message.to_string(),
        })
    }

    /// How this service words the failures the authentication layer NAMED ITSELF, which a
    /// service's own wording cannot overrule because the seam that words it is not handed
    /// them (see the module docs).
    ///
    /// `presented` is what the request offered to authenticate with
    /// ([`crate::sigv4_presentation`]), which is the one thing about the request this reads,
    /// and which decides two of the three answers:
    ///
    /// * nothing presented at all is the missing-credential class;
    /// * a security token PRESENTED that the layer resolved to no issued session is the
    ///   unknown-credential class under the sentence AWS's token stage carries
    ///   ([`INVALID_SECURITY_TOKEN_MESSAGE`]);
    /// * the same verdict for a request that presented NO token — a temporary credential's
    ///   key and secret alone — is the same class under the sentence AWS's
    ///   credential-resolution stage carries, because what the caller presented is a key id
    ///   that resolves to no usable credential.
    ///
    /// `None` for everything else, including the missing-credential class for a service that
    /// words that one itself: the refusal then keeps the words it already has.
    #[must_use]
    pub fn restated(
        &self,
        reported: &str,
        presented: crate::sigv4_presentation::PresentedCredential,
    ) -> Option<Refused> {
        let (code, message) = match reported {
            // Nothing was presented, so nothing could be unknown: the layer looked a
            // credential up because every signed scheme it holds declined the request.
            REPORTED_UNRESOLVED_CREDENTIAL if !presented.credential => (
                self.missing_credential?,
                crate::sigv4_presentation::MISSING_CREDENTIAL_MESSAGE,
            ),
            // The layer resolved the security token and the request's is not the issued one.
            REPORTED_INVALID_SECURITY_TOKEN if presented.security_token => (
                self.unresolved_credential,
                INVALID_SECURITY_TOKEN_MESSAGE,
            ),
            REPORTED_INVALID_SECURITY_TOKEN => {
                (self.unresolved_credential, UNRESOLVED_CREDENTIAL_MESSAGE)
            }
            _ => return None,
        };
        Some(Refused {
            code: code.to_string(),
            message: message.to_string(),
        })
    }
}

/// What `req` presented to authenticate with, read through the tree's one reading of that
/// rule ([`crate::sigv4_presentation`]) over the places SigV4 puts a presentation.
///
/// `pub`: a service's own refusal layer that states one of these failures itself asks
/// [`StatedAuthenticationRefusal::stated`] the same question about the same request, and one
/// reading of a presentation keeps the two from disagreeing about what a request offered.
#[must_use]
pub fn presented_credential(
    req: &RtRequest,
) -> crate::sigv4_presentation::PresentedCredential {
    let query = req.uri().split_once('?').map(|(_, query)| query);
    crate::sigv4_presentation::PresentedCredential::of(
        req.headers().get("authorization"),
        req.headers()
            .get(crate::sigv4_presentation::SECURITY_TOKEN_HEADER),
        query,
    )
}

/// A service's model-derived half with the authentication refusal stated in that service's
/// own vocabulary (see the module docs).
///
/// Identity, the routing table, the protocol and every other rendering come from the wrapped
/// definition untouched — nothing model-derived is restated here. Generic over that
/// definition rather than over an engine, so this crate names no service's generated type.
pub struct AuthenticationRefusalDefinition<D> {
    inner: D,
    stated: StatedAuthenticationRefusal,
}

impl<D: ErasedServiceDefinition> AuthenticationRefusalDefinition<D> {
    /// Wrap a service's definition, stating an authentication refusal with `stated`'s codes.
    pub fn new(inner: D, stated: StatedAuthenticationRefusal) -> Self {
        Self { inner, stated }
    }
}

impl<D: ErasedServiceDefinition> ErasedServiceDefinition for AuthenticationRefusalDefinition<D> {
    fn identity(&self) -> &ServiceIdentity {
        self.inner.identity()
    }

    fn protocol(&self) -> &dyn ServerHttpProtocol {
        self.inner.protocol()
    }

    fn routing_table(&self) -> &OperationRoutingTable {
        self.inner.routing_table()
    }

    fn routing_error_response(&self, req: &RtRequest) -> RtResponse {
        self.inner.routing_error_response(req)
    }

    fn framework_error_response(&self, error: ProtocolError, req: &RtRequest) -> RtResponse {
        self.inner.framework_error_response(error, req)
    }

    fn refusal_response(
        &self,
        error: ProtocolError,
        chosen: Option<Refused>,
        req: &RtRequest,
    ) -> RtResponse {
        // A failure the authentication layer NAMED is stated from that name, ahead of a
        // service's own wording, which was chosen without seeing it; its generic verdict is
        // stated only where the service worded nothing.
        let stated = match &error {
            ProtocolError::Unauthenticated { code, .. } => {
                let presented = presented_credential(req);
                let named = self.stated.restated(code, presented);
                match (named, &chosen) {
                    (named @ Some(_), _) => named,
                    (None, None) => self.stated.stated(code, presented),
                    (None, Some(_)) => None,
                }
            }
            _ => None,
        };
        self.inner.refusal_response(error, stated.or(chosen), req)
    }

    fn wire_protocol(&self, req: &RtRequest) -> WireProtocol<'_> {
        self.inner.wire_protocol(req)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StatedAuthenticationRefusal, INVALID_SECURITY_TOKEN_MESSAGE, SIGNATURE_MISMATCH_MESSAGE,
        UNRESOLVED_CREDENTIAL_MESSAGE,
    };
    use crate::sigv4_presentation::{MISSING_CREDENTIAL_MESSAGE, PresentedCredential};

    /// The vocabulary of an awsQuery identity service, and of an awsJson one: two services
    /// whose codes for the same failures disagree. The awsJson one words the
    /// missing-credential class at its own front door, so it names none here.
    const QUERY: StatedAuthenticationRefusal = StatedAuthenticationRefusal {
        unresolved_credential: "InvalidClientTokenId",
        signature_mismatch: "SignatureDoesNotMatch",
        missing_credential: Some("MissingAuthenticationToken"),
    };
    const JSON: StatedAuthenticationRefusal = StatedAuthenticationRefusal {
        unresolved_credential: "UnrecognizedClientException",
        signature_mismatch: "InvalidSignatureException",
        missing_credential: None,
    };

    /// What a request of each shape presented.
    const NOTHING: PresentedCredential = PresentedCredential {
        credential: false,
        security_token: false,
    };
    const LONG_TERM: PresentedCredential = PresentedCredential {
        credential: true,
        security_token: false,
    };
    const TEMPORARY: PresentedCredential = PresentedCredential {
        credential: true,
        security_token: true,
    };

    /// One row: the service's vocabulary, the code the authentication layer reported, what the
    /// request presented, and the code and message that service answers for it (`None` where
    /// it answers with the words it was given).
    struct Row {
        case: &'static str,
        vocabulary: StatedAuthenticationRefusal,
        reported: &'static str,
        presented: PresentedCredential,
        stated: Option<(&'static str, &'static str)>,
    }

    /// Each failure the authentication layer reports is stated in the service's OWN code with
    /// AWS's fixed sentence for that failure, and a code the layer does not report is left
    /// exactly as it arrived.
    #[test]
    fn each_reported_failure_is_stated_in_the_services_own_code() {
        let rows = vec![
            Row {
                case: "a query service, a credential that resolved to nothing",
                vocabulary: QUERY,
                reported: "AccessDenied",
                presented: LONG_TERM,
                stated: Some(("InvalidClientTokenId", UNRESOLVED_CREDENTIAL_MESSAGE)),
            },
            Row {
                case: "a query service, a signature that did not verify",
                vocabulary: QUERY,
                reported: "SignatureDoesNotMatch",
                presented: LONG_TERM,
                stated: Some(("SignatureDoesNotMatch", SIGNATURE_MISMATCH_MESSAGE)),
            },
            Row {
                case: "a json service, a credential that resolved to nothing",
                vocabulary: JSON,
                reported: "AccessDenied",
                presented: LONG_TERM,
                stated: Some(("UnrecognizedClientException", UNRESOLVED_CREDENTIAL_MESSAGE)),
            },
            Row {
                // The same verdict for a TEMPORARY credential: what resolved to nothing is
                // the key id the request presented, whichever form of credential it is.
                case: "a json service, a temporary credential that resolved to nothing",
                vocabulary: JSON,
                reported: "AccessDenied",
                presented: TEMPORARY,
                stated: Some(("UnrecognizedClientException", UNRESOLVED_CREDENTIAL_MESSAGE)),
            },
            Row {
                case: "a json service, a signature that did not verify",
                vocabulary: JSON,
                reported: "SignatureDoesNotMatch",
                presented: LONG_TERM,
                stated: Some(("InvalidSignatureException", SIGNATURE_MISMATCH_MESSAGE)),
            },
            Row {
                // Nothing was presented, so nothing could be unknown: a service naming a
                // missing-credential class has it restated (the table below), and one naming
                // none keeps the answer it already has rather than being told its key is
                // unknown.
                case: "a json service wording the missing class itself, nothing presented",
                vocabulary: JSON,
                reported: "AccessDenied",
                presented: NOTHING,
                stated: None,
            },
            Row {
                case: "a query service, nothing presented",
                vocabulary: QUERY,
                reported: "AccessDenied",
                presented: NOTHING,
                stated: None,
            },
            Row {
                // The stage that resolves the SECURITY TOKEN is restated from the layer's own
                // verdict rather than from this generic mapping, which answers for the key
                // id: see the table below.
                case: "a query service, the layer's security-token verdict",
                vocabulary: QUERY,
                reported: "InvalidClientTokenId",
                presented: TEMPORARY,
                stated: None,
            },
            Row {
                // Already a service's own code: a pre-dispatch refusal it rendered itself.
                case: "a json service, a refusal already worded by the service",
                vocabulary: JSON,
                reported: "UnrecognizedClientException",
                presented: LONG_TERM,
                stated: None,
            },
            Row {
                case: "a verdict the layer does not report for a credential",
                vocabulary: QUERY,
                reported: "RequestTimeTooSkewed",
                presented: LONG_TERM,
                stated: None,
            },
        ];

        for Row {
            case,
            vocabulary,
            reported,
            presented,
            stated: expected,
        } in rows
        {
            let stated = vocabulary.stated(reported, presented);
            let actual = stated
                .as_ref()
                .map(|refused| (refused.code.as_str(), refused.message.as_str()));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} for the reported code {reported:?}, got \
                 {actual:?}"
            );
        }
    }

    /// The failures the layer NAMED are restated whatever the service worded: a request that
    /// presented no credential at all is the missing-credential class, and a security token
    /// the layer resolved to no session is the unknown-credential one — under the sentence of
    /// the stage the verdict came from.
    ///
    /// The rows differ along what the rule generalises over: whether a credential was
    /// presented, whether a security token travelled with it, which layer verdict was
    /// reported, and whether the service names a missing-credential class of its own at all.
    #[test]
    fn the_failures_the_layer_named_are_restated_from_its_own_verdict() {
        /// One row: the vocabulary, the reported code, what the request presented, and what
        /// the layer's own verdict is restated as.
        struct Named {
            case: &'static str,
            vocabulary: StatedAuthenticationRefusal,
            reported: &'static str,
            presented: PresentedCredential,
            restated: Option<(&'static str, &'static str)>,
        }
        let rows = vec![
            Named {
                case: "a query service, nothing presented",
                vocabulary: QUERY,
                reported: "AccessDenied",
                presented: NOTHING,
                restated: Some(("MissingAuthenticationToken", MISSING_CREDENTIAL_MESSAGE)),
            },
            Named {
                case: "a query service, a credential presented and unknown",
                vocabulary: QUERY,
                reported: "AccessDenied",
                presented: LONG_TERM,
                restated: None,
            },
            Named {
                case: "a service wording the missing class itself, nothing presented",
                vocabulary: JSON,
                reported: "AccessDenied",
                presented: NOTHING,
                restated: None,
            },
            Named {
                case: "a query service, a security token that resolved to no session",
                vocabulary: QUERY,
                reported: "InvalidClientTokenId",
                presented: TEMPORARY,
                restated: Some(("InvalidClientTokenId", INVALID_SECURITY_TOKEN_MESSAGE)),
            },
            Named {
                case: "a json service, a security token that resolved to no session",
                vocabulary: JSON,
                reported: "InvalidClientTokenId",
                presented: TEMPORARY,
                restated: Some(("UnrecognizedClientException", INVALID_SECURITY_TOKEN_MESSAGE)),
            },
            Named {
                // The same verdict for a request that presented NO token: a temporary
                // credential's key and secret alone, which is a key id resolving to no
                // usable credential, and carries that stage's sentence.
                case: "a query service, a temporary credential presented without its token",
                vocabulary: QUERY,
                reported: "InvalidClientTokenId",
                presented: LONG_TERM,
                restated: Some(("InvalidClientTokenId", UNRESOLVED_CREDENTIAL_MESSAGE)),
            },
            Named {
                // The signature failure is the layer's generic verdict about a credential it
                // resolved: the service's own wording stands for it.
                case: "a query service, a signature that did not verify",
                vocabulary: QUERY,
                reported: "SignatureDoesNotMatch",
                presented: TEMPORARY,
                restated: None,
            },
            Named {
                case: "a query service, a verdict about the request's instant",
                vocabulary: QUERY,
                reported: "RequestTimeTooSkewed",
                presented: LONG_TERM,
                restated: None,
            },
        ];

        for Named {
            case,
            vocabulary,
            reported,
            presented,
            restated: expected,
        } in rows
        {
            let restated = vocabulary.restated(reported, presented);
            let actual = restated
                .as_ref()
                .map(|refused| (refused.code.as_str(), refused.message.as_str()));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} for the reported code {reported:?}, got \
                 {actual:?}"
            );
        }
    }
}
