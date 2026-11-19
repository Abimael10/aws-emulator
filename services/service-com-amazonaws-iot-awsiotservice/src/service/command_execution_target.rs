// SPDX-License-Identifier: CC0-1.0

//! WHERE the ownership of a command-execution `targetArn` is decided: in FRONT of the
//! request's own shape, beside the IAM decision this service takes there.
//!
//! `GetCommandExecution` and `DeleteCommandExecution` bind `targetArn` as a query parameter,
//! and WHICH device the value names is read off the ARN
//! ([`crate::state::command_execution_target`]). A value that can name no device of this
//! account is an AUTHORIZATION answer, and AWS answers one at the layer in front of the
//! service — before the request's own members are constraint-checked — so a request that names
//! a target outside the account is refused on THAT ground whatever else about it is malformed,
//! rather than being told about a member it would also have to fix.
//!
//! This decides nothing the operation does not decide for itself (each handler reads the same
//! `targetArn` through the same rule before it reads or writes anything): what lives
//! here is the ORDER, which is the only thing the centralized deserialize -> validate ->
//! handle walk cannot express, since a handler runs only after modelled-constraint
//! enforcement.
//!
//! The other way a `targetArn` is refused — a value that is no ARN at all — stays with the
//! operation: that is a statement about the MEMBER, which is what the validation layer
//! answers, and the handler words it as the modelled `ValidationException`.

use ::aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;

/// The query parameter the two command-execution operations bind the device's ARN to —
/// `smithy_query(query="operation",operation="GetCommandExecution,DeleteCommandExecution")`:
/// `targetArn`, `@required`, `@httpQuery("targetArn")`.
const TARGET_ARN_MEMBER: &str = "targetArn";
/// The operations whose input binds [`TARGET_ARN_MEMBER`]; a value standing at that name on
/// any other request is not a target this decision is about.
const TARGET_ARN_OPERATIONS: [&str; 2] = ["GetCommandExecution", "DeleteCommandExecution"];

/// The media type this restJson1 service renders an error in.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The refusal this request is answered with when its `targetArn` can name no device of the
/// account, or `None` for every request this decision does not refuse — one naming another
/// operation, one carrying no `targetArn` at all (the framework answers the absent required
/// member), one whose value is no ARN (the operation words that itself) and one whose ARN
/// names a device of this account, held in the registry or not.
pub fn refusal(operation: &str, req: &WireRequest) -> Option<WireResponse> {
    if !TARGET_ARN_OPERATIONS.contains(&operation) {
        return None;
    }
    let target_arn = super::request_query::value(req.uri(), TARGET_ARN_MEMBER)?;
    let refused = matches!(
        crate::state::command_execution_target(&target_arn),
        Err(crate::state::CommandExecutionTargetRefusal::NotInAccount)
    );
    refused.then(refusal_response)
}

/// The wire answer to a target outside the account: HTTP 403 with the access-denied error code
/// this refusal carries and its capitalised `Message` member
/// ([`crate::scaffolded::ACCESS_DENIED_ERROR_TYPE`],
/// [`crate::state::TARGET_NOT_IN_ACCOUNT_MESSAGE`]) — the same answer the operation's own
/// error channel renders, so the layer that answers it is not something a client can tell.
fn refusal_response() -> WireResponse {
    let body = ::serde_json::json!({
        "__type": crate::scaffolded::ACCESS_DENIED_ERROR_TYPE,
        "Message": crate::state::TARGET_NOT_IN_ACCOUNT_MESSAGE,
    });
    let mut response = WireResponse::new(
        StatusCode::try_from(403u16).expect("the status is a constant of this module"),
        SdkBody::from(body.to_string()),
    );
    response
        .headers_mut()
        .insert("content-type", JSON_CONTENT_TYPE);
    response.headers_mut().insert(
        "x-amzn-errortype",
        crate::scaffolded::ACCESS_DENIED_ERROR_TYPE,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the refusal this decision answers with is the access-denied one, at 403, with the
    /// Coral-located error code in the header and in the body's discriminator and the fixed
    /// text under the capitalised `Message` member.
    #[test]
    fn the_refusal_is_the_access_denied_answer() {
        let response = refusal_response();
        assert_eq!(
            response.status().as_u16(),
            403,
            "the ownership refusal is answered at 403"
        );
        assert_eq!(
            response.headers().get("x-amzn-errortype"),
            Some(crate::scaffolded::ACCESS_DENIED_ERROR_TYPE),
            "a restJson1 client resolves the shape from this header"
        );
        let body: ::serde_json::Value = ::serde_json::from_slice(
            response.body().bytes().expect("the body is built in memory"),
        )
        .expect("the body is JSON");
        assert_eq!(
            (
                body.get("__type").and_then(::serde_json::Value::as_str),
                body.get("Message").and_then(::serde_json::Value::as_str),
            ),
            (
                Some(crate::scaffolded::ACCESS_DENIED_ERROR_TYPE),
                Some(crate::state::TARGET_NOT_IN_ACCOUNT_MESSAGE),
            ),
            "expected the access-denied shape and its capitalised Message, got {body:?}"
        );
    }
}
