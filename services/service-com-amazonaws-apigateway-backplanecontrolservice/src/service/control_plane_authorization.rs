// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER's own IAM policies let a CONTROL-PLANE request perform what it asks
//! for — the decision taken before any operation of this service reads or writes a record.
//!
//! A signed call is decided by IAM ahead of the service, so a principal whose policies
//! grant none of this service's actions is told it is not authorized whatever record it
//! named, and the answer says nothing about whether that record exists here. That is what
//! makes a MISSING GRANT distinguishable from a missing resource: the first is IAM's own
//! refusal (HTTP 403, `AccessDeniedException`, the answer every AWS service gives a caller
//! its policies do not allow), the second this service's own `NotFoundException`.
//!
//! That refusal is taken BEFORE the service, which is why its shape is IAM's rather than
//! one of this service's modelled errors: the decision is about the request's action and
//! resource and is made without reading a record, so it is not an outcome the operation
//! could report.
//!
//! ## The action and the resource
//!
//! API Gateway's control plane does NOT publish one action per operation. Its IAM actions
//! are the HTTP METHODS of the REST API that serves it — `apigateway:GET`,
//! `apigateway:POST`, `apigateway:PUT`, `apigateway:PATCH`, `apigateway:DELETE` — taken on
//! a resource ARN whose resource portion is the request's own URI path
//! (`arn:aws:apigateway:{region}::/restapis/{id}/…`), which is the form every ARN this
//! service reports carries ([`crate::state::service_arn`]). So the action and the resource
//! are read off the REQUEST, and a statement a developer wrote against
//! `apigateway:GET` on `arn:aws:apigateway:*::/restapis/*` decides exactly the calls AWS
//! would decide with it.
//!
//! ## Who the caller is, and what it may do
//!
//! The same three pieces the `execute-api` data plane's gate reads, and read through the
//! same module so there is ONE resolution of who a request speaks for in this crate:
//! [`crate::service::invoke::execute_api_authorization::signing_caller`] — the principal
//! the access key id names (STS's sessions, else IAM's access keys), the identity-based
//! documents in force on it, the permissions boundary that caps them and the session
//! policies its credential was minted with. The decision itself is
//! [`::service_common::iam_authorization`], the tree's one evaluation of policy documents.
//!
//! ## The two callers the decision cannot refuse
//!
//! * **The account's own published credential**, which no IAM record holds policies for —
//!   the position AWS account root credentials are in.
//! * **A principal holding an attachment this emulator cannot read** (an AWS-managed
//!   policy): refusing on a grant that cannot be seen would deny access AWS permits.
//!
//! An explicit `Deny` in a document that CAN be read still refuses either of them, and a
//! permissions boundary the store DOES hold still caps the grant.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::OperationContext;

use crate::ops::types::UnauthorizedException;
use crate::service::invoke::execute_api_authorization::{refusing_reason, signing_caller};

/// The action namespace this service's operations are published under, which is also its
/// SigV4 signing name: a policy statement spells one of them `apigateway:<HTTP method>`.
pub const ACTION_PREFIX: &str = "apigateway";

/// The error shape a refused control-plane request is answered as, as the wire names it:
/// the shape IAM's own refusal carries, which every AWS SDK unmarshals as an
/// authorization failure.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The HTTP status that refusal carries, the status AWS answers an unauthorized request
/// with.
const ACCESS_DENIED_STATUS: u16 = 403;

/// The IAM action a request using `http_method` performs: `apigateway:<METHOD>`, the
/// form API Gateway publishes its control-plane actions in.
#[must_use]
pub fn action_for(http_method: &str) -> String {
    format!("{ACTION_PREFIX}:{}", http_method.to_ascii_uppercase())
}

/// The resource ARN a control-plane request names: the api-gateway ARN whose resource
/// portion is the request's own URI path, which is the only thing a statement could name
/// this call by.
#[must_use]
pub fn judged_resource(ctx: &OperationContext<'_>, req: &RtRequest) -> String {
    crate::state::service_arn(&crate::state::request_region(ctx.parts()), judged_path(req))
}

/// The PATH of a control-plane request, which is the resource portion of the ARN the
/// decision is taken over — whichever form the request LINE carries it in.
///
/// A request this seam is handed carries its URI as a `&str`, and an AWS SDK sends the
/// ABSOLUTE form (`https://apigateway.{region}.amazonaws.com/restapis`), which is what
/// every call made by an SDK client of this emulator — a handler's own call among them —
/// puts on the wire; a client that sends the origin form (`/restapis`) addresses the same
/// resource. So the scheme and the authority are dropped, through the one reader of a whole
/// request URI's path this tree has ([`::service_common::uri_path`]), and a statement
/// naming the resource decides both forms alike.
///
/// The QUERY is dropped FIRST, and for two reasons: it carries the request's parameters
/// rather than the resource it addresses, so a statement naming the resource cannot be made
/// to depend on them; and a query VALUE may itself carry `://`, which a scheme scan over the
/// whole URI would mistake for the authority.
///
/// Borrowed from the request rather than copied out of it, so the seam allocates nothing per
/// request.
fn judged_path(req: &RtRequest) -> &str {
    let without_query = req.uri().split('?').next().unwrap_or_default();
    ::service_common::uri_path(without_query)
}

/// `Some(denial)` when IAM does not allow this request's action on the resource it names,
/// and `None` when it does.
///
/// IAM's order: an explicit `Deny` in a document that CAN be read refuses the call
/// whatever else the principal carries; otherwise a grant is needed, which a caller this
/// gate does not bound has by definition (see the module docs); and a grant is capped both
/// by the SESSION POLICIES the credential was minted with — "the resulting session's
/// permissions are the intersection of the role's identity-based policy and the session
/// policies" (`AssumeRoleRequest$PolicyArns`) — and by the PERMISSIONS BOUNDARY, since "a
/// permissions boundary sets the maximum permissions that an identity-based policy can
/// grant". Each cap takes permissions away and grants none of its own.
///
/// Asked with NO store guard of this service held, and before the request's own
/// identifiers are resolved.
#[must_use]
pub fn denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    req: &RtRequest,
) -> Option<UnauthorizedException> {
    let action = action_for(req.method());
    let resource = judged_resource(ctx, req);
    let caller = signing_caller(
        engine,
        &crate::state::request_caller(ctx.parts()),
        ctx.credential_store(),
        ::std::time::SystemTime::now(),
    );
    // A credential the clock has already run out on speaks for nobody, so nothing it asks
    // for is authorized. In the assembled emulator the layer that authenticates a request
    // refuses such a credential before this is reached.
    if caller.expired() {
        return Some(unauthorized(&caller.arn, &action, &resource, EXPIRED));
    }
    // The decision rests on the action, the resource the request addressed, the principal
    // spellings — and the CLASS of the credential presented, which the caller answers for
    // (`SigningCaller::conditions`), so a statement conditioned on `aws:PrincipalType`
    // decides here exactly as it does on the data plane beside it.
    let context = caller.conditions();
    let published =
        [::service_common::temporary_credential_classes::PRINCIPAL_TYPE_CONDITION_KEY];
    let request = iam::Request::new(&action, &resource, caller.spellings())
        .with_context(&context)
        .with_published_keys(&published);
    // IAM's own evaluation ORDER — explicit denies, then the grant, then each cap — is the
    // one this crate already applies to the `execute-api` data plane, asked here rather
    // than restated: this control plane publishes no resource-based policy of its own, so
    // there is no resource-based decision to hand it ([`Decision::NotStated`]). What this
    // plane owns is the SENTENCE its caller reads.
    let reason = refusing_reason(&caller, &request, &action, Decision::NotStated)?;
    Some(unauthorized(&caller.arn, &action, &resource, &reason))
}

/// The clause a refusal of a credential the clock has already run out on closes with: such
/// a credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED: &str = "because the credential it presented has expired";

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it — the one sentence every
/// authorizing service in this tree words it as
/// ([`iam::not_authorized_by_identity_policy`]). Every value in it comes from the request
/// and from the stored policies.
fn unauthorized(
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) -> UnauthorizedException {
    UnauthorizedException {
        message: Some(iam::not_authorized_by_identity_policy(
            principal, action, resource, reason,
        )),
    }
}

/// The wire answer to a request this gate refused: IAM's own `AccessDeniedException` at
/// its 403, serialized through the SAME protocol a handler-returned error goes through, so
/// a refusal taken in this layer carries the envelope every other error of this service
/// does — the status, the shape in `x-amzn-errortype` and the `__type` body a client
/// branches on.
///
/// The BODY is the one member an authorization refusal carries, a `message`, which is what
/// the schema below describes; the shape NAME on the wire is IAM's, because the refusal is
/// IAM's (see the module docs).
///
/// The hand-built fallback carries the same three things for the one case the serializer
/// can fail: a value it cannot render.
#[must_use]
pub fn refusal_response(
    protocol: &dyn ::smithy_server_runtime::ServerHttpProtocol,
    denial: &UnauthorizedException,
) -> RtResponse {
    protocol
        .serialize_error(
            denial,
            &crate::ops::types::SCHEMA_UNAUTHORIZED_EXCEPTION,
            ACCESS_DENIED_EXCEPTION,
            ACCESS_DENIED_STATUS,
        )
        .unwrap_or_else(|_| {
            let body = ::serde_json::json!({
                "__type": ACCESS_DENIED_EXCEPTION,
                "message": denial.message.clone().unwrap_or_default(),
            });
            let mut response = RtResponse::new(
                ::aws_smithy_runtime_api::http::StatusCode::try_from(ACCESS_DENIED_STATUS)
                    .expect("the status is a constant of this module"),
                ::aws_smithy_types::body::SdkBody::from(body.to_string()),
            );
            response
                .headers_mut()
                .insert("content-type", "application/json");
            response
                .headers_mut()
                .insert("x-amzn-errortype", ACCESS_DENIED_EXCEPTION);
            response
        })
}
