// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER's own identity-based IAM policies let a request to this service
//! perform the action it asks for — the decision taken in FRONT of every operation of this
//! service, before anything the request carried is read.
//!
//! A signed call is decided by IAM ahead of the service, so a principal whose policies
//! grant none of this service's actions is told it is not authorized whatever reference it
//! named, and the answer says nothing about whether that reference resolves here. That is
//! what makes a MISSING GRANT distinguishable from an absent channel: the first is an
//! authorization refusal (HTTP 403, `AccessDeniedException`, carrying neither `successful`
//! nor `failed`), the second this service's own modelled `ChannelNotFound`.
//!
//! WHERE the decision is taken is part of the rule, which is why it is taken by
//! [`Authorized`] — the layer that serves this service's operations — and not inside a
//! handler: a handler runs only after the request's own values have been deserialized and
//! held to the model's constraints, so a decision taken there would be SKIPPED for every
//! request refused at that boundary. The decision therefore precedes every refusal this
//! service states, and the error class a caller meets does not depend on which of a fault's
//! spellings its request used: a value that is not a channel reference is answered the same
//! way whether the model's `@pattern` rejects it at the boundary or this service finds it
//! names no channel, and an unentitled caller is never handed a refusal restating the
//! values it submitted.
//!
//! The three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the SAME
//!    place `sts:GetCallerIdentity` answers from ([`sts_store::resolve_caller`]) for a
//!    session credential, and from IAM's own `access_keys` map (the map `iam:GetUser`
//!    resolves a caller through) for a long-term key;
//! 2. **what it may do** — the identity-based policy documents in force on that principal,
//!    the permissions boundary that caps them, read through the IAM service's OWN
//!    accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next call and deleting it lifts it
//!    again — and the SESSION POLICIES the mint of this very credential scoped it with
//!    ([`sts_store::session_scoping_documents`]), which cap those documents the same way:
//!    "the resulting session's permissions are the intersection of the role's
//!    identity-based policy and the session policies" (`AssumeRoleRequest$PolicyArns`), so
//!    a session policy takes permissions away and grants none of its own;
//! 3. **the decision** — [`::service_common::iam_authorization`], the one shared evaluation
//!    of those documents against `(action, resource)`, and the SENTENCE a refusal carries,
//!    which is that module's
//!    [`::service_common::iam_authorization::not_authorized_by_identity_policy`] and the
//!    reason clauses beside it, so nothing of IAM's wording is spelled here.
//!
//! Only the store LOOKUPS live here; neither the collection nor the decision does. The
//! lookups stay in this service crate because the IAM service crate depends on
//! `service-common`, so the shared crate cannot read IAM's store without a dependency
//! cycle. WHICH IAM entity a principal ARN names carries no such dependency, so it is read
//! through the shared [`::service_common::iam_role`] rather than parsed again here.
//!
//! ## The action and the resource
//!
//! The action is `<signing name>:<OperationName>` ([`action_for`]). This service signs as
//! `cloudtrail-data` (`sigv4Name` / `awsService.endpointPrefix`,
//! `smithy_query(query="service_info")`), which is the namespace its actions are published
//! under — a namespace DISJOINT from the CloudTrail control plane's `cloudtrail:`, so a
//! grant of `cloudtrail:*` covers no action of this service.
//!
//! The resource is the channel reference the request itself carried, read off the request
//! ([`judged_resource`]) rather than resolved out of the store, so the decision is the same
//! whether or not a channel of that name is registered here and a refusal tells an
//! unauthorized caller nothing about what the emulator holds.
//!
//! ## The two callers the decision cannot refuse
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to [`sts_store::ambient_identity`], which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to the account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An
//!   explicit `Deny` in a document that CAN be read still refuses, and a permissions
//!   boundary the store DOES hold still caps the grant.
//!
//! An expired session credential speaks for nobody: it carries no document, so every action
//! it asks for is refused, and the refusal names that state rather than the key it
//! presented, which this module never quotes back.

use std::sync::Arc;
use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::{
    BoxFuture, ErasedOperation, OperationContext, ServerHttpProtocol, Service,
};

use crate::scaffolded::{AccessDeniedException, ACCESS_DENIED_CODE, ACCESS_DENIED_STATUS};

/// The query parameter this service's operations bind the channel reference to
/// (`PutAuditEventsRequest$channelArn` is `@httpQuery("channelArn")`,
/// `smithy_query(query="operation", operation="PutAuditEvents")`), which is where the
/// channel a request names is read from before anything of the request is deserialized.
const CHANNEL_ARN_PARAMETER: &str = "channelArn";

/// The resource a request that names no channel at all is judged against: IAM's own resource
/// wildcard, which is how it writes the resource of a request that addresses none.
const ANY_CHANNEL: &str = "*";

/// The detail the access record of a refused call carries.
const DENIED_DETAIL: &str = "the caller's identity-based policies do not allow this action";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The action namespace this service's operations are published under, which is also its
/// SigV4 signing name (`sigv4Name` `cloudtrail-data`): a policy statement spells one of
/// them `cloudtrail-data:<OperationName>`. It is NOT the CloudTrail control plane's
/// `cloudtrail:` namespace, which publishes different actions.
pub const ACTION_PREFIX: &str = "cloudtrail-data";

/// The IAM action `operation` is performed under: `cloudtrail-data:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The resource this request is judged against: the channel reference the REQUEST carried,
/// read off the request itself so the decision can be taken before anything of it is
/// deserialized or held to the model's constraints.
///
/// The reference is the value of the query parameter the model binds it to
/// ([`CHANNEL_ARN_PARAMETER`]), percent-decoded the way the protocol's own deserializer
/// decodes a query-bound member, so the gate judges the very value a handler would see —
/// whatever spelling it takes, including one the member's modelled `@pattern` rejects.
///
/// [`ANY_CHANNEL`] for a request that carries no reference at all (the parameter absent, or
/// bound with no value): such a request addresses no channel of this service, and the
/// absent `@required` member is reported by the boundary once the call is authorized.
#[must_use]
pub fn judged_resource(req: &RtRequest) -> String {
    channel_reference_named(req.uri()).unwrap_or_else(|| ANY_CHANNEL.to_string())
}

/// The channel reference `uri` names, or `None` when it names none.
///
/// The raw query string is read through the tree's one reader of a raw one
/// (`service_common::query_parameter`), which takes the FIRST spelling of a repeated
/// parameter — the one the protocol's deserializer keeps.
fn channel_reference_named(uri: &str) -> Option<String> {
    let (_, query) = uri.split_once('?')?;
    let reference = ::service_common::query_parameter(query, CHANNEL_ARN_PARAMETER)
        .map(::smithy_server_runtime::percent_decode)?;
    (!reference.is_empty()).then_some(reference)
}

/// One modelled operation of this service, served behind the decision IAM takes in front of
/// it.
///
/// It holds the shared engine, which is what the caller's identity and the documents in
/// force on it are read through, and the operation's own name, which is what the action asked
/// about is built from ([`action_for`]). Every request it does not refuse is handed to the
/// layer it wraps, which serves the one operation the generated routing table built from
/// that same engine — so there is no second instance of the operation and no second store,
/// and nothing about how a served request is deserialized, validated, handled or serialized
/// changes.
pub(super) struct Authorized<Inner, Eng> {
    /// What serves a request this gate admits.
    inner: Inner,
    /// The shared engine the decision reads the caller and its policies through.
    engine: Arc<Eng>,
    /// The operation's own name, as the model declares it.
    operation: &'static str,
}

impl<Inner, Eng> Authorized<Inner, Eng> {
    /// Put `inner` — the layer that serves the modelled operation named `operation` — behind
    /// the identity-based decision, read from `engine`.
    pub(super) fn new(engine: Arc<Eng>, operation: &'static str, inner: Inner) -> Self {
        Self {
            inner,
            engine,
            operation,
        }
    }
}

impl<Inner: ErasedOperation, Eng: Send + Sync + 'static> ErasedOperation for Authorized<Inner, Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // IAM's decision, taken before anything of the request is read: the action is
            // this operation's own, and the resource is the channel reference the request
            // carried. A refused request reaches neither the constraint boundary nor the
            // handler, so it reports nothing of what it submitted and persists nothing.
            let resource = judged_resource(req);
            if let Some(denial) = refusal(&*self.engine, ctx, self.operation, &resource) {
                crate::ops::put_audit_events::record_access(
                    ctx,
                    &resource,
                    ACCESS_DENIED_CODE,
                    DENIED_DETAIL,
                );
                // The protocol is read, used and dropped inside this one statement:
                // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the
                // await below would make this future non-`Send`.
                return denied_response(svc.definition().protocol(), &denial);
            }
            self.inner.run(svc, req, ctx).await
        })
    }

    /// The wrapped layer's modelled auth schemes: standing in front of an operation must not
    /// change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.inner.auth_schemes()
    }
}

/// The wire answer to a request this gate refused: `AccessDeniedException` at
/// [`ACCESS_DENIED_STATUS`], serialized through the SAME protocol call a handler-returned
/// error of that shape goes through, so a refusal taken here and one taken in a handler are
/// indistinguishable on the wire.
///
/// An authorization decision never degrades into success, so a response is answered in every
/// case: the hand-written fallback carries the same three things a client branches on — the
/// status, the shape name and the prose member — for the one case the serializer can fail.
fn denied_response(protocol: &dyn ServerHttpProtocol, denial: &str) -> RtResponse {
    let refused = AccessDeniedException::new(denial);
    protocol
        .serialize_error(
            &refused,
            &crate::scaffolded::ACCESS_DENIED_SCHEMA,
            ACCESS_DENIED_CODE,
            ACCESS_DENIED_STATUS,
        )
        .unwrap_or_else(|failed| {
            ::tracing::warn!(
                error = %failed,
                "the authorization refusal could not be rendered in this service's envelope"
            );
            let body = ::serde_json::json!({
                "__type": ACCESS_DENIED_CODE,
                "Message": refused.message,
            });
            let mut response = RtResponse::new(
                StatusCode::try_from(ACCESS_DENIED_STATUS)
                    .expect("the refusal's status is a constant of this crate"),
                SdkBody::from(body.to_string()),
            );
            response
                .headers_mut()
                .insert("content-type", "application/json");
            response
                .headers_mut()
                .insert("x-amzn-errortype", ACCESS_DENIED_CODE);
            response
        })
}

/// `Some(message)` when IAM does not allow `cloudtrail-data:<operation>` on `resource` for
/// the calling principal — the sentence the refusal carries — and `None` when it does.
///
/// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
/// whatever else the principal carries; otherwise a grant is needed, which a caller this
/// gate does not bound has by definition (see the module docs); and a grant is capped BOTH
/// by the session policies the credential was minted with, since "the resulting session's
/// permissions are the intersection of the role's identity-based policy and the session
/// policies" (`AssumeRoleRequest$PolicyArns`), AND by the permissions boundary, since "a
/// permissions boundary sets the maximum permissions that an identity-based policy can
/// grant" (IAM, permissions boundaries). Each cap takes permissions away and grants none of
/// its own, so a call only a cap allows is refused as if that cap had allowed nothing.
///
/// Which document withheld the grant is a different fact, and needs a different fix, from an
/// explicit deny in it — so the refusal names the one the decision rested on, in the order
/// IAM reads them.
///
/// Asked with NO store guard of this service held, and before the request's own channel
/// reference is resolved.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    let action = action_for(operation);
    let caller = caller(engine, ctx);
    // An identity-based document states no `Principal` element, so the decision rests on
    // the action and the resource the request addressed — and on the CLASS of the credential
    // presented, which is what a statement conditioned on `aws:PrincipalType` selects on.
    // That key is answered, and declared answered, for every request whose credential this
    // gate resolved, so such a clause decides rather than standing undecided.
    let context = ::service_common::temporary_credential_classes::principal_type_conditions(
        caller.principal_type,
    );
    let published =
        [::service_common::temporary_credential_classes::PRINCIPAL_TYPE_CONDITION_KEY];
    let request = iam::Request::new(&action, resource, &[])
        .with_context(&context)
        .with_published_keys(&published);
    let said = iam::identity_decision(&caller.documents, &request);
    // Whether anything the principal carries grants the call at all. A caller this gate
    // does not bound is granted by definition (see the module docs), but an explicit `Deny`
    // in a document that CAN be read still wins over every grant.
    let granted = iam::is_allowed(said, Decision::NotStated)
        || (said != Decision::Denied && caller.identity_is_unbounded());
    // What the SCOPE this credential was minted with says, through the tree's one reading of
    // a resolved scope: a credential no mint scoped is capped by nothing, which that reading
    // answers as allowed.
    let scoped = caller.scope.decision(&request);
    // The ceiling in force on the principal. A call an identity-based policy explicitly
    // DENIES is already refused on that deny, so no ceiling is read for it.
    let capped = match (said == Decision::Denied, caller.boundary.as_ref()) {
        (false, Some(boundary)) => {
            iam::identity_decision(::std::slice::from_ref(boundary), &request)
        }
        _ => Decision::Allowed,
    };
    if granted && scoped == Decision::Allowed && capped == Decision::Allowed {
        return None;
    }
    // An explicit deny is a fact about the document that carries it, so the refusal names
    // that document; otherwise the refusal names the document that WITHHELD the grant — the
    // principal's own policies where nothing granted the call, the scope where they granted
    // it and the mint's session policies did not, and the boundary where both did.
    let reason = if said == Decision::Denied {
        iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
    } else if scoped == Decision::Denied {
        iam::SESSION_POLICY_EXPLICIT_DENY.to_string()
    } else if capped == Decision::Denied {
        iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()
    } else if !granted {
        iam::no_identity_policy_allows(&action)
    } else if scoped != Decision::Allowed {
        iam::no_session_policy_allows(&action)
    } else {
        iam::no_permissions_boundary_allows(&action)
    };
    Some(iam::not_authorized_by_identity_policy(
        &caller.arn,
        &action,
        resource,
        &reason,
    ))
}

/// The principal ARN a refusal of THIS request names: the identity the credentials it
/// presented speak for, as `sts:GetCallerIdentity` reports it.
///
/// The same reading [`refusal`] blames, exposed so a refusal decided from the request's own
/// resource field (a resource ARN naming another account, which no document of this account
/// can grant over) names the caller the same way a policy decision does.
#[must_use]
pub fn caller_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    caller(engine, ctx).arn
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
    /// The session policy documents the mint of this request's credential scoped it with, as
    /// they read at the instant of this request, and whether a managed one among them names a
    /// document this emulator does not hold. Empty for a credential no mint scoped — every
    /// long-term access key, and every session minted without session policies — which is a
    /// credential capped by nothing.
    scope: sts_store::ScopingPolicies,
    /// The CLASS of the credential the request was signed with, as STS reports it for that
    /// credential (`sts_store::Caller::principal_type`): the value the global condition key
    /// naming the calling principal's type answers with. `None` for a request this gate
    /// resolved no credential for, which carries no value for the key at all.
    principal_type: Option<&'static str>,
    /// Set when this request presents the account's own published credential, which holds
    /// no IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not
    /// hold — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no
    /// IAM policies, or a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's OWN
    /// accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &iam_store::ComAmazonawsIamAwsidentitymanagementv20100508State,
        identity_arn: &str,
    ) {
        if let Some(policies) = store.principal_policies(identity_arn) {
            self.documents = policies.documents;
            // An attachment the collection resolved no document for is an entry of AWS's
            // own catalog this emulator does not carry: a grant it cannot read.
            self.unreadable_attachment = !policies.uncarried.is_empty();
        }
        self.boundary = store.permissions_boundary_document(identity_arn);
    }
}

/// The principal this request speaks for, with the identity-based policies in force on it.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which materialises the
/// session expiries the clock has already made due before it answers: that write is owed at
/// the same instant whether or not anybody called.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    // Who the credential speaks for AND what its mint scoped it to, read together from the
    // session registry: the scope travels with the credential, so the only thing a later
    // request carries is where it has to be read from.
    let resolved = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            sts_store::ambient_identity(),
        ),
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential, which holds no identity policy and which no
        // mint scoped.
        None => Ok(sts_store::Caller {
            identity: sts_store::ambient_identity(),
            kind: None,
            source_identity: None,
            tags: Vec::new(),
            transitive_tag_keys: Vec::new(),
            role_arn: None,
            role_id: None,
            issued_at: None,
            federating_user_arn: None,
            federated_claims: Vec::new(),
            session_policies: sts_store::SessionPolicies::default(),
            mfa_authenticated: false,
        }),
    };
    // An expired session credential speaks for nobody: it carries no document, so every
    // action it asks for is refused — and the refusal names the credential's STATE rather
    // than the key id that was presented, which this module never quotes back.
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(_) => {
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                ..Caller::default()
            };
        }
    };
    // WHOSE identity-based documents are in force for this credential, read through STS's one
    // answer for it: the role behind an assumed-role session (resolved below to the ARN the
    // account holds the role under, which carries its path), the IAM user behind a
    // federated-user session, and the principal itself otherwise. Taken before the resolved
    // caller is broken up below.
    let identity_policies_arn = resolved.identity_policies_arn().to_string();
    let federated_session = resolved.federating_user_arn.is_some();
    let mut caller = Caller {
        account_credential: resolved.identity.arn == sts_store::ambient_identity().arn,
        // WHICH CLASS of credential signed this request, read through STS's one answer for it
        // (`sts_store::Caller::principal_type`).
        principal_type: Some(resolved.principal_type()),
        arn: resolved.identity.arn,
        // The inline document the mint passed scopes the credential whether or not an IAM
        // store is there; where there is one, the scope is re-read below so a managed session
        // policy counts at the version in force NOW.
        scope: sts_store::session_scoping_documents(None, &resolved.session_policies),
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // What the mint scoped this credential to, as the documents read at the instant of this
    // request: a managed session policy stands for its DEFAULT version, so one edited since
    // the mint scopes by what it says now.
    caller.scope = sts_store::session_scoping_documents(Some(&store), &resolved.session_policies);
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a key it never
    // minted is the account's ambient identity — but IAM's store maps an access key id to
    // the user that owns it (the map `iam:GetUser` resolves a caller through), so a request
    // signed with one speaks for THAT user and is judged on the user's policies. A key the
    // IAM store does not hold either really is the account's own published credential.
    if caller.account_credential {
        if let Some(user) = store
            .access_keys
            .get(&access_key_id)
            .and_then(|record| store.users.get(&record.user_key))
        {
            caller.account_credential = false;
            caller.arn = user.arn.clone();
        }
    }
    // Which IAM identity the principal's policies are held under is read through the tree's
    // shared readings of a principal ARN.
    if federated_session {
        // A FEDERATED-user session is judged on the documents of the IAM user whose long-term
        // credentials minted it — "the session permissions are the intersection of the IAM
        // user policies and the session policies that you pass" (`GetFederationToken`) —
        // because the federated user is no IAM identity and carries no document of its own.
        caller.read_policies_of(&store, &identity_policies_arn);
    } else if let Some(role_name) =
        ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by
        // the ARN the account holds it under — which carries its PATH, where the session ARN
        // does not.
        if let Some(role_arn) = store
            .roles
            .get(&iam_store::name_key(role_name))
            .map(|held| held.arn.clone())
        {
            caller.read_policies_of(&store, &role_arn);
        }
    } else if ::service_common::iam_role::user_name(&caller.arn).is_some() {
        // An IAM user's permissions are its own, under the ARN it was resolved by.
        let user_arn = caller.arn.clone();
        caller.read_policies_of(&store, &user_arn);
    }
    caller
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What one stored document says about `action` on `resource`, through the shared
    /// evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// A channel ARN of this emulator's own account, as the CloudTrail control plane mints
    /// one: `arn:aws:cloudtrail:<region>:<account>:channel/<channel id>`.
    fn channel_arn(region: &str, channel_id: &str) -> String {
        ::engine::emulator_arn("cloudtrail", region, &format!("channel/{channel_id}"))
    }

    /// RULE: the resource a request is judged against is the channel reference its query
    /// string binds, whatever else the URI carries and whatever spelling the value takes;
    /// a request that binds none addresses no channel.
    #[test]
    fn the_resource_judged_is_the_bound_channel_reference() {
        let arn = channel_arn("us-west-2", "7d9c2f1e-3b45-4a6d-8e90-1f2a3b4c5d6e");
        let encoded: String = arn.replace(':', "%3A").replace('/', "%2F");
        // (case, the request URI, the resource the decision is taken against)
        let rows: Vec<(&str, String, String)> = vec![
            (
                "the one parameter",
                format!("/PutAuditEvents?channelArn={arn}"),
                arn.clone(),
            ),
            (
                "percent-encoded, as an SDK writes a query value",
                format!("/PutAuditEvents?channelArn={encoded}"),
                arn.clone(),
            ),
            (
                "among other parameters",
                format!("/PutAuditEvents?externalId=x&channelArn={arn}"),
                arn.clone(),
            ),
            (
                "an absolute URI",
                format!("https://cloudtrail-data.us-west-2.amazonaws.com/PutAuditEvents?channelArn={arn}"),
                arn.clone(),
            ),
            (
                "the first of two spellings, as the deserializer reads it",
                format!("/PutAuditEvents?channelArn={arn}&channelArn=other"),
                arn.clone(),
            ),
            (
                // Outside the member's modelled `@pattern`: the boundary refuses such a
                // value, and this gate still judges the call on the value it carried.
                "a value that is no ARN at all",
                "/PutAuditEvents?channelArn=not-an-arn".to_string(),
                "not-an-arn".to_string(),
            ),
            (
                "a bare channel id (the ARN suffix the doc also names)",
                "/PutAuditEvents?channelArn=7d9c2f1e-3b45-4a6d-8e90-1f2a3b4c5d6e".to_string(),
                "7d9c2f1e-3b45-4a6d-8e90-1f2a3b4c5d6e".to_string(),
            ),
            (
                "no query string at all",
                "/PutAuditEvents".to_string(),
                ANY_CHANNEL.to_string(),
            ),
            (
                "another parameter only",
                "/PutAuditEvents?externalId=x".to_string(),
                ANY_CHANNEL.to_string(),
            ),
            (
                "bound with no value",
                "/PutAuditEvents?channelArn=".to_string(),
                ANY_CHANNEL.to_string(),
            ),
            (
                "bound as a bare key",
                "/PutAuditEvents?channelArn".to_string(),
                ANY_CHANNEL.to_string(),
            ),
        ];
        for (case, uri, expected) in rows {
            let mut req = ::aws_smithy_runtime_api::http::Request::new(SdkBody::empty());
            req.set_uri(uri.clone()).expect("a request URI");
            let judged = judged_resource(&req);
            assert_eq!(
                judged, expected,
                "case {case:?}: expected the resource {expected:?} for {uri:?}, got {judged:?}"
            );
        }
    }

    /// RULE: the action this gate asks about carries this service's OWN namespace, so a
    /// document scoped to the CloudTrail control plane's actions — under any spelling —
    /// states nothing about it.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        let resource = channel_arn("us-west-2", "7d9c2f1e-3b45-4a6d-8e90-1f2a3b4c5d6e");
        let action = action_for("PutAuditEvents");
        assert_eq!(
            action, "cloudtrail-data:PutAuditEvents",
            "case \"the action spelling\": expected this service's namespace, got {action:?}"
        );
        // (case, the document in force, what it says about this action)
        let rows: &[(&str, &str, Decision)] = &[
            (
                "every action of this service",
                r#"{"Statement":[{"Effect":"Allow","Action":"cloudtrail-data:*","Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "exactly this action",
                r#"{"Statement":[{"Effect":"Allow","Action":"cloudtrail-data:PutAuditEvents",
                 "Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "every action of the CloudTrail control plane",
                r#"{"Statement":[{"Effect":"Allow","Action":"cloudtrail:*","Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "this operation's name under the control plane's namespace",
                r#"{"Statement":[{"Effect":"Allow","Action":"cloudtrail:PutAuditEvents",
                 "Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "an explicit deny of this service beside a control-plane allow",
                r#"{"Statement":[
                 {"Effect":"Allow","Action":"cloudtrail:*","Resource":"*"},
                 {"Effect":"Deny","Action":"cloudtrail-data:*","Resource":"*"}]}"#,
                Decision::Denied,
            ),
        ];
        for (case, document, expected) in rows {
            let reached = decision(document, &action, &resource);
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {expected:?} for {action}, got {reached:?}"
            );
        }
    }

    /// RULE: the decision is taken against the channel the request named, so a grant scoped
    /// to one channel ARN covers that channel and no other — not another channel of the same
    /// account and Region, and not the same channel id in another Region.
    #[test]
    fn a_grant_is_read_for_the_channel_the_request_named() {
        let granted = channel_arn("us-west-2", "7d9c2f1e-3b45-4a6d-8e90-1f2a3b4c5d6e");
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
             "Action":"cloudtrail-data:PutAuditEvents","Resource":"{granted}"}}]}}"#
        );
        // (case, the channel reference the request carried, whether the grant covers it)
        let rows: &[(&str, String, bool)] = &[
            ("the granted channel", granted.clone(), true),
            (
                "another channel of the same account and Region",
                channel_arn("us-west-2", "0f1e2d3c-4b5a-4968-8776-65544332211a"),
                false,
            ),
            (
                "the same channel id in another Region",
                channel_arn("eu-west-3", "7d9c2f1e-3b45-4a6d-8e90-1f2a3b4c5d6e"),
                false,
            ),
            (
                "a channel ARN of another account",
                "arn:aws:cloudtrail:us-west-2:111122223333:channel/\
                 7d9c2f1e-3b45-4a6d-8e90-1f2a3b4c5d6e"
                    .to_string(),
                false,
            ),
        ];
        for (case, resource, covered) in rows {
            let said = decision(&document, &action_for("PutAuditEvents"), resource);
            let actual = iam::is_allowed(said, Decision::NotStated);
            assert_eq!(
                actual, *covered,
                "case {case:?}: expected the grant to {}cover {resource}, got {said:?}",
                if *covered { "" } else { "not " }
            );
        }
    }
}
