// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the principal that SIGNED a request perform the action it asked for —
//! the decision taken in FRONT of every operation of this service, before anything is read
//! or written.
//!
//! Every operation of this API is signed with IAM credentials (`sigv4` is the only auth
//! scheme the model declares, `smithy_query(query="service_info")`), and a signed call is
//! decided by IAM ahead of the service: a caller whose identity-based policies do not allow
//! the action on the resource the request names is told it is not authorized, and the answer
//! says nothing about whether that stack exists here. That is what makes a MISSING GRANT
//! distinguishable from a missing stack: the first is IAM's own refusal, the second this
//! service's own `ValidationError`.
//!
//! The decision is taken per request, so a statement `iam:PutRolePolicy` wrote refuses the
//! next call and one `iam:DeleteRolePolicy` removed stops granting it — which is what makes a
//! least-privilege test, and a revocation drill, mean anything here. The most common caller
//! this bounds is a RUNNING function: a handler's own SDK calls are signed as the session of
//! its execution role, so what that role's documents say about `cloudformation:<operation>`
//! is what the handler is served or refused.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who it is, what it may do, and the decision** —
//!    [`crate::state::calling_principal`], the one place this service resolves the principal
//!    an access key id speaks for, reads the identity-based documents and the permissions
//!    boundary in force on it and asks [`::service_common::iam_authorization`] about an
//!    `(action, resource)` pair. The callers it does not bound — the account's own published
//!    credential, which every in-process call another emulated service makes presents, a
//!    principal carrying a grant this emulator cannot read, and an engine with no STS or IAM
//!    store — are named there;
//! 2. **what the request asks for** — the action and the resource, read off the request
//!    alone ([`action_for`], [`judged_resource`]), so a refused caller learns nothing about
//!    what the account holds;
//! 3. **where the decision is taken** — in the operation wrapper every request of this
//!    service passes through
//!    ([`super::query_error_envelope::RequestIdEnvelopeOperation`]), ahead of the handler.
//!    Wrapping is what makes it ONE decision: every modelled operation is judged by the same
//!    rule before its handler, so no handler can forget it and none of them takes it twice.
//!
//! ## The action and the resource
//!
//! CloudFormation publishes one IAM action per API operation, named by that operation
//! (`cloudformation:DescribeStacks`, `cloudformation:GetTemplate`), under the ARN namespace
//! this service signs with — so a statement scoped to `cloudformation:*` covers every
//! operation of it and nothing outside it.
//!
//! The resource is the STACK the request names, by the ARN CloudFormation gave that stack:
//! a stack id IS that ARN, and a stack name resolves to it through this service's own store
//! (`StackStore::resolve_any`, which keeps a deleted stack addressable). A name this Region
//! holds no stack for is judged against the ARN form a stack of that name would carry, with
//! the id wildcarded — the decision precedes the lookup, so it cannot depend on whether the
//! stack exists. A request that names no stack at all — a listing, a template validated, a
//! call about a stack set or about the extension registry — is judged against every resource
//! of this service in the account and the Region the request was signed for, written in this
//! service's own ARN form with the resource wildcarded rather than as a bare `*`, which is
//! what a statement has to cover to grant such a call.
//!
//! ## The refusal
//!
//! No error shape of this model carries it — no operation of this service declares an access
//! error — because it belongs to the authorization layer ahead of the service rather than to
//! any one operation: it is answered with the wire error code CloudFormation publishes for an
//! unauthorized call ([`crate::state::service_role::ACCESS_DENIED`]) at HTTP 403, in this
//! service's own awsQuery envelope, so a client branches on the same code and reads the same
//! `<RequestId>` it reads on every other error of this endpoint.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::smithy_server_runtime::{OperationContext, ServerHttpProtocol};

use crate::state::service_role::ACCESS_DENIED;

/// The HTTP status an authorization refusal carries — the status AWS answers an unauthorized
/// request with.
const ACCESS_DENIED_STATUS: u16 = 403;

/// The namespace this service's IAM actions are published under, which is also its SigV4
/// signing name and its ARN namespace ([`crate::state::ARN_NAMESPACE`]): a policy statement
/// spells one of them `cloudformation:<OperationName>`.
const ACTION_PREFIX: &str = crate::state::ARN_NAMESPACE;

/// The input member a request names the stack it is about in, which every operation of this
/// service that is about ONE stack binds under that name
/// (`smithy_query(query="operation")` for each of them: `StackName`, taking a stack name or a
/// stack id).
const STACK_MEMBER: &str = "StackName";

/// The IAM action an `operation` call performs: `cloudformation:<OperationName>`, the form
/// CloudFormation publishes its actions in.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// `Some(sentence)` when the documents in force on the calling principal do not let it
/// perform what this request asks for, and `None` when they do — including every caller the
/// gate does not bound (see the module docs).
///
/// Taken with NO store of this service locked, and before the request's own identifiers are
/// resolved: a refused request must read and write nothing.
#[must_use]
pub fn denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    req: &RtRequest,
    operation: &str,
) -> Option<String> {
    let action = action_for(operation);
    let resource = judged_resource(engine, ctx, req);
    crate::state::calling_principal::Caller::of(engine, ctx)
        .unauthorized_message(&action, &resource)
}

/// The resource this request is judged against: the stack it names, by that stack's own ARN,
/// and otherwise every resource of this service in the account and Region (see the module
/// docs).
#[must_use]
pub fn judged_resource<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    req: &RtRequest,
) -> String {
    let region = crate::state::request_region(ctx.parts());
    let Some(named) = stack_named(req) else {
        return ::emulator_server::emulator_arn(crate::state::ARN_NAMESPACE, &region, "*");
    };
    // The stack's own ARN where this Region holds the stack the request named — a stack id is
    // that ARN already, and a name resolves to it — read and released before anything else
    // takes a lock.
    let held = crate::state::service_state(engine).and_then(|state| {
        state
            .lock()
            .resolve_any(&region, &named)
            .map(|stack| stack.stack_id.clone())
    });
    held.unwrap_or_else(|| {
        if named.starts_with("arn:") {
            return named;
        }
        // No stack of that name here: the ARN form one would carry, with the id a create
        // would mint wildcarded, because the decision precedes the lookup.
        ::emulator_server::emulator_arn(
            crate::state::ARN_NAMESPACE,
            &region,
            &format!("stack/{named}/*"),
        )
    })
}

/// The stack `req` names: the `StackName` parameter its awsQuery request binds, decoded as a
/// form value.
///
/// An awsQuery client sends the operation's input as the parameters of a form-encoded body,
/// and a client that sends them in the query string of a `GET` addresses the same operation,
/// so both are read. `None` when the request binds no stack, and when it binds one with no
/// value — neither names a stack, and an absent required member is the framework's own
/// refusal, taken after this gate.
fn stack_named(req: &RtRequest) -> Option<String> {
    let body = req
        .body()
        .bytes()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    let query = req.uri().split_once('?').map(|(_, query)| query);
    let found = [Some(body.as_str()), query]
        .into_iter()
        .flatten()
        .find_map(|form| ::service_common::query_parameter(form, STACK_MEMBER))
        .map(form_value)
        .filter(|named| !named.is_empty());
    found
}

/// One form parameter's value as it was submitted: `+` is a space and every escape is
/// decoded, which is how a client encodes a value — a stack id is an ARN, whose `:` and `/`
/// an SDK percent-encodes.
fn form_value(value: &str) -> String {
    let spaced = value.replace('+', " ");
    ::percent_encoding::percent_decode_str(&spaced)
        .decode_utf8_lossy()
        .into_owned()
}

/// The wire answer to a request this gate refused: the code CloudFormation publishes for an
/// unauthorized call at its 403, serialized through the SAME protocol a handler-returned
/// error goes through, so the refusal carries the envelope every other error of this service
/// does — and the `<RequestId>` the wrapper around this one adds to it.
///
/// An authorization decision never degrades into success, so this answers a response in every
/// case: where the error cannot be serialized through its schema, the same status, code and
/// message are written directly rather than letting the request reach a handler the gate
/// decided not to serve.
#[must_use]
pub fn refusal_response(protocol: &dyn ServerHttpProtocol, message: &str) -> RtResponse {
    let refused = AccessDenied {
        message: message.to_string(),
    };
    protocol
        .serialize_error(
            &refused,
            &ACCESS_DENIED_SCHEMA,
            ACCESS_DENIED,
            ACCESS_DENIED_STATUS,
        )
        .unwrap_or_else(|_| {
            let body = format!(
                "<ErrorResponse><Error><Type>Sender</Type><Code>{ACCESS_DENIED}</Code>\
                 <Message>{}</Message></Error></ErrorResponse>",
                xml_text(&refused.message)
            );
            let mut response = RtResponse::new(
                ::aws_smithy_runtime_api::http::StatusCode::try_from(ACCESS_DENIED_STATUS)
                    .expect("the status is a constant of this module"),
                ::aws_smithy_types::body::SdkBody::from(body),
            );
            response.headers_mut().insert("content-type", "text/xml");
            response
        })
}

/// `text` as the character data of an XML element: the five markup characters escaped, so a
/// sentence carrying one cannot change the document's structure.
fn xml_text(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&apos;".to_string(),
            held => held.to_string(),
        })
        .collect()
}

/// The `message` member of the synthetic error shape an authorization refusal is rendered
/// through.
static ACCESS_DENIED_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#AccessDenied$message",
        "smithy.framework",
        "message",
    ),
    ShapeType::String,
    "message",
    0,
)
.with_xml_name("Message");

/// Schema for that shape.
///
/// Deliberately in the `smithy.framework` namespace, as the scaffold channel's own synthetic
/// shapes are: the shape is in no service's model — the refusal is IAM's, taken ahead of this
/// service — so it must not collide with a modelled one. The `<Code>` on the wire is the one
/// [`refusal_response`] names, not this shape's.
static ACCESS_DENIED_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#AccessDenied",
        "smithy.framework",
        "AccessDenied",
    ),
    ShapeType::Structure,
    &[&ACCESS_DENIED_MESSAGE],
);

/// The payload of that refusal: the one sentence IAM words an authorization failure in.
#[derive(Debug, Clone)]
struct AccessDenied {
    message: String,
}

impl SerializableStruct for AccessDenied {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&ACCESS_DENIED_MESSAGE, self.message.as_str())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{action_for, form_value, stack_named, STACK_MEMBER};

    /// One awsQuery request carrying `form` as its body.
    fn request(form: &str) -> ::aws_smithy_runtime_api::http::Request {
        let mut req = ::aws_smithy_runtime_api::http::Request::new(
            ::aws_smithy_types::body::SdkBody::from(form.to_string()),
        );
        req.set_uri("https://cloudformation.us-east-1.amazonaws.com/")
            .expect("the test URI is well formed");
        req
    }

    /// RULE: the IAM action a request asks for is this service's namespace and the
    /// operation's own name, so a statement naming one covers exactly that operation.
    #[test]
    fn the_action_is_the_operations_own_name() {
        for (case, operation, expected) in [
            ("a read", "DescribeStacks", "cloudformation:DescribeStacks"),
            ("a template read", "GetTemplate", "cloudformation:GetTemplate"),
            ("a write", "CreateStack", "cloudformation:CreateStack"),
        ] {
            let action = action_for(operation);
            assert_eq!(
                action, expected,
                "case {case:?}: expected {expected:?}, got {action:?}"
            );
        }
    }

    /// RULE: the stack a request names is the `StackName` parameter it binds, decoded as the
    /// form value it was submitted as; a request binding none names none.
    ///
    /// The rows differ along the FORM the parameter arrives in: a plain name, a name among
    /// other parameters, a stack id whose ARN separators are escaped, a value carrying `+`, a
    /// request binding the member empty, and one binding no stack at all.
    #[test]
    fn the_stack_named_is_the_bound_parameter() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "the one parameter",
                "Action=DescribeStacks&Version=2010-05-15&StackName=app",
                Some("app"),
            ),
            (
                "among other parameters",
                "Action=ListStackResources&StackName=app&NextToken=abc",
                Some("app"),
            ),
            (
                "a stack id, its ARN separators escaped",
                "Action=DescribeStacks&StackName=arn%3Aaws%3Acloudformation%3Aus-east-1%3A\
                 000000000000%3Astack%2Fapp%2F0123",
                Some("arn:aws:cloudformation:us-east-1:000000000000:stack/app/0123"),
            ),
            (
                "a value carrying a space as +",
                "Action=DescribeStacks&StackName=two+words",
                Some("two words"),
            ),
            ("no stack at all", "Action=ListStacks&Version=2010-05-15", None),
            ("the member bound empty", "Action=DescribeStacks&StackName=", None),
        ];
        for (case, form, expected) in rows {
            let named = stack_named(&request(form));
            assert_eq!(
                named.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
        assert_eq!(
            STACK_MEMBER, "StackName",
            "the member read is the one the model binds a stack under"
        );
        assert_eq!(
            form_value("a%2Bb"),
            "a+b",
            "case \"an escaped plus\": expected the submitted value"
        );
    }
}
