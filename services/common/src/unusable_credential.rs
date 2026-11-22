// SPDX-License-Identifier: CC0-1.0

//! The ONE refusal of a request presenting a credential the account holds and will NOT
//! accept, for every service a credential like that can be presented to.
//!
//! Two kinds of credential are refused here, and each is refused for what the credential IS
//! rather than for what any document says about the identity behind it:
//!
//! * an access key the account holds authenticates a request only while its stored `Status`
//!   is `Active`: `UpdateAccessKey$Status` documents that "Active means that the key can be
//!   used for programmatic calls to Amazon Web Services, while Inactive means that the key
//!   cannot be used", so moving a key's status off `Active` cuts the credential off while
//!   leaving the key listed — the reversible, non-destructive way to withdraw one; and
//! * a TEMPORARY credential a service does not accept at all — one of a class that may not
//!   call the service ([`crate::temporary_credential_classes`]), and one the account has
//!   withdrawn because the principal it speaks for is gone. Neither is a document, so no
//!   `Allow` anywhere lifts the refusal and it is taken before any policy is read.
//!
//! What that makes the request is an AUTHENTICATION failure, and the distinction is
//! load-bearing: the presented credential is not one this service accepts at all, so NO
//! principal is resolved from it, no document is collected for it and no action is
//! authorized — the request is refused before any operation runs. It is therefore answered
//! with the code an unresolved credential carries (which [`crate::authentication_refusal`]
//! states for each service's own vocabulary) and with the fixed sentence that module states
//! for the stage the refusal came from — the one that resolves a key id, and the one that
//! resolves the security token presented with it — never with an authorization denial and
//! never with one of a service's modelled resource errors.
//!
//! The rule is one rule, so it lives here once: the account's own records answer whether a
//! credential is usable, and every service asks them the same question and answers alike.
//! What a service supplies is its own two halves — the unknown-credential CODE its protocol
//! family names ([`UnusableCredential::stated_as`]), and the account's answer for the
//! presented key id, read from the records that own it (the identity service's own
//! `CreateAccessKey` / `UpdateAccessKey`, and the record the operation that MINTED a
//! temporary credential left). Nothing here reaches a store, so this module names no
//! service.
//!
//! The seam a service reaches it from is [`smithy_server_runtime::Service::override_operation`],
//! the one hook that sees a whole request before the modelled walk deserializes, validates
//! and dispatches it; [`UnusableCredential::refusal_for`] is the whole decision, and the same
//! value is the pre-dispatch operation that answers it.
//!
//! A READ throughout: the decision only consults the records a service hands it, so a
//! refused request changes nothing a later caller sees — and a status moved back to `Active`
//! is accepted again on the very next request, because nothing was torn down to refuse it.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::ProtocolError;
use ::smithy_server_runtime::{
    BoxFuture, ErasedOperation, OperationContext, ServerHttpProtocol, Service,
};

/// Answers a request that presented a credential the account will not accept, and decides
/// which request that is.
///
/// Held by a service and handed back from its pre-dispatch seam as a borrowed
/// [`ErasedOperation`], so nothing is built per request.
#[derive(Debug, Clone, Copy)]
pub struct UnusableCredential {
    /// The code this service answers an unresolved credential with — its own, because the
    /// name differs by protocol family (see [`crate::authentication_refusal`]).
    code: &'static str,
}

impl UnusableCredential {
    /// The refusal as the service naming `code` for an unresolved credential states it.
    ///
    /// `code` is that service's own unknown-credential code, the same one
    /// [`crate::authentication_refusal::StatedAuthenticationRefusal::unresolved_credential`]
    /// carries for it: a credential the account will not accept and one it cannot resolve at
    /// all are the same failure to the caller, so they are stated alike.
    #[must_use]
    pub const fn stated_as(code: &'static str) -> Self {
        Self { code }
    }

    /// This refusal for `req`, or `None` when the request may go on to the operation it
    /// named.
    ///
    /// `account_accepts` is the account's own answer for the access key id the request
    /// presented — a READ of the records the identity service holds — and is asked only once,
    /// for a request that both names an operation `protocol` can route and presents a
    /// credential at all:
    ///
    /// * a request naming no routable operation is left alone, because there is nothing to
    ///   serve it with and the routing failure is the answer it gets;
    /// * a request presenting NO credential presents no key whose status could refuse it;
    /// * a key id the account holds no record of is not an IAM user's key, so the account's
    ///   `Status` says nothing about it and `account_accepts` answers `true` for it.
    #[must_use]
    pub fn refusal_for(
        &self,
        req: &RtRequest,
        protocol: &dyn ServerHttpProtocol,
        account_accepts: impl FnOnce(&str) -> bool,
    ) -> Option<&dyn ErasedOperation> {
        if protocol.routing_key(req).is_err() {
            return None;
        }
        let access_key_id = presented_access_key_id(req)?;
        (!account_accepts(&access_key_id)).then_some(self as &dyn ErasedOperation)
    }
}

impl ErasedOperation for UnusableCredential {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // The refusal is reached from the credential alone: nothing of the request's body or
        // metadata is read, and nothing is written — the request is handed to the renderer
        // only so the refusal closes the envelope of the protocol it is served in, and the
        // status (403) is the one every authentication refusal of that envelope carries.
        let _ = ctx;
        Box::pin(async move {
            svc.definition().refusal_response(
                ProtocolError::Unauthenticated {
                    code: self.code,
                    message: crate::authentication_refusal::UNRESOLVED_CREDENTIAL_MESSAGE,
                },
                None,
                req,
            )
        })
    }
}

/// The access key id `req` presented, or `None` for a request carrying no SigV4 credential
/// scope at all.
///
/// Read through this crate's ONE reader of that scope ([`crate::request_access_key_id`]), so
/// a call signed under either SigV4 scheme — the `Authorization` header, or the query
/// parameters a presigned call carries — is read the same way a handler reads it. That reader
/// takes the request's `Parts`, which a pre-dispatch seam is not handed (it sees the
/// runtime-API request), so the parts are lifted from the request rather than the parse being
/// written a second time here.
#[must_use]
pub fn presented_access_key_id(req: &RtRequest) -> Option<String> {
    let mut builder = ::http::Request::builder()
        .method(req.method())
        .uri(req.uri());
    for (name, value) in req.headers().iter() {
        builder = builder.header(name, value);
    }
    let (parts, ()) = builder.body(()).ok()?.into_parts();
    let access_key_id = crate::request_access_key_id(&parts);
    (access_key_id != crate::UNSIGNED_ACCESS_KEY_ID).then_some(access_key_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request carrying `authorization`, as the pre-dispatch seam sees it.
    fn request(authorization: Option<&str>, query: &str) -> RtRequest {
        let mut request = RtRequest::new(::aws_smithy_types::body::SdkBody::empty());
        request
            .set_uri(format!("/{query}").as_str())
            .expect("a well-formed URI");
        request.set_method("POST").expect("a well-formed method");
        if let Some(authorization) = authorization {
            request
                .headers_mut()
                .insert("authorization", authorization.to_string());
        }
        request
    }

    /// A SigV4 credential scope naming `access_key_id`, as a signing client writes it.
    fn scope(access_key_id: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/us-east-1/iam/aws4_request, \
             SignedHeaders=host, Signature=00"
        )
    }

    /// The key id is read from wherever SigV4 put it, and a request presenting none reads as
    /// no credential rather than as the reader's sentinel.
    #[test]
    fn the_presented_key_id_is_read_from_either_sigv4_scheme() {
        // (case, the `Authorization` header, the query string, the key id expected)
        let rows: [(&str, Option<String>, &str, Option<&str>); 4] = [
            (
                "a header-signed request",
                Some(scope("AKIAHEADERSIGNED00001")),
                "",
                Some("AKIAHEADERSIGNED00001"),
            ),
            (
                "a presigned request",
                None,
                "?X-Amz-Credential=AKIAPRESIGNED00000001%2F20260101%2Fus-east-1%2Fiam%2Faws4_request",
                Some("AKIAPRESIGNED00000001"),
            ),
            ("an unsigned request", None, "", None),
            (
                "an unsigned request with a query string of its own",
                None,
                "?Action=GetCallerIdentity",
                None,
            ),
        ];
        for (case, authorization, query, expected) in rows {
            let request = request(authorization.as_deref(), query);
            let actual = presented_access_key_id(&request);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
