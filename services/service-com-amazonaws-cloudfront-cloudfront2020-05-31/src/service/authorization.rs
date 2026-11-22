// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the
//! authorization step taken in front of EVERY operation, before it reads or writes
//! anything.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what a
//! handler would do, so a caller whose identity-based policies do not grant the action on
//! the ARN the request addressed is told it is not authorized rather than told anything
//! about that resource, and the call performs nothing. The ORDER is part of the rule: a
//! caller that holds no grant and names a resource the account does not hold reads the
//! authorization refusal, not "no such resource" — otherwise a refused call reports whether
//! a resource exists to a caller that may not look.
//!
//! The decision is taken for EVERY operation of this service, in ONE place
//! ([`refusal`], called by [`super::envelope::ErrorEnvelope`], the seam every request the
//! model's routing table resolves passes through), because authorization is a property of
//! the REQUEST rather than of any one operation's error set: there is no operation a caller
//! may perform without a grant, so there is no operation left unjudged and no second copy of
//! the rule to drift.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_caller`, the same place
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one
//!    the caller reads back for the credentials it signed with), else the IAM user that
//!    owns the long-term key, else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for
//!    that principal and the permissions boundary that caps them, read through the IAM
//!    service's OWN accessors (`principal_policies`, `permissions_boundary_document`),
//!    where the collection rule itself lives — so a policy `iam:PutRolePolicy` wrote takes
//!    effect on the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared
//!    evaluation of those documents against `(action, resource)`, so this service and
//!    every other one handed the same documents answer alike: an explicit `Deny` wins,
//!    otherwise an `Allow` about the request grants it, and with neither the request falls
//!    to IAM's default deny.
//!
//! Only the store LOOKUPS live here; the decision does not. That is the split every other
//! gate in this tree makes, and the lookups stay in the service crate for the reason those
//! keep theirs there: hoisting them into `service-common` would make every service that
//! depends on that crate link the IAM and STS service crates.
//!
//! ## The action and the resource
//!
//! The ACTION is this service's namespace and the operation's own name ([`action_of`]),
//! which is how IAM publishes the actions of a service — so one statement scoped to
//! `cloudfront:*` covers every operation of it and nothing outside it.
//!
//! The RESOURCE is the one the REQUEST addresses ([`judged_resource`]). CloudFront is a
//! REST API whose routes carry the addressed resource as a path label, so the resource is
//! read off the request line: the kind is the route's own resource segment and the
//! identifier is the label beside it, and the ARN is the one THIS service mints for a
//! resource of that kind ([`ARN_OF_KIND`], each entry the crate's own minting function). An
//! identifier that is already an ARN of this service is the resource itself. The tagging
//! routes carry theirs in the `Resource` query parameter instead
//! ([`TAGGING_RESOURCE_PARAMETER`]), and that ARN is read the same way. Taking the resource
//! from the request rather than from a stored record is what keeps the decision independent
//! of whether that resource exists, which is also what keeps a refused call from telling an
//! unauthorized caller whether it does — so a policy scoped to one distribution's ARN
//! permits that distribution's calls and no other's.
//!
//! Every other request is judged against [`ANY_RESOURCE`], the resource IAM writes for a
//! request that addresses no resource of its own: a collection route (`GET
//! /2020-05-31/distribution`), a create, and an account-level call all address the whole
//! service rather than one resource of it. For those the GRANT question is also asked in
//! the weaker form `iam_authorization` states for a gate that does not know a resource ARN
//! — whether any readable `Allow` covers the action at all — because a request judged
//! against the wildcard may still be ABOUT a resource a statement is scoped to (one named
//! in the request document rather than on the route, or one of a kind whose ARN this gate
//! does not mint), and refusing it on the resource element would deny calls AWS permits.
//! The DENY question is always decided against what the request addressed, so a `Deny`
//! scoped to an ARN still refuses a call that names it.
//!
//! A CloudFront distribution carries no resource-based policy of its own, so the decision
//! here is the identity-based one alone. (The resource policy this service's own
//! `PutResourcePolicy` writes grants principals OUTSIDE this account access to a VPC
//! origin; every request this gate judges is signed by a credential of this account, whose
//! grants are its identity's.)
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root
//!   credentials are in. Such a request is allowed. An emulated service calling this one
//!   through the in-process callback signs with that credential, so this service's own
//!   internal calls are never refused for want of a grant.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed
//!   policy belongs to AWS rather than to this account, so the IAM store holds no document
//!   for it; refusing on a grant this emulator cannot see would deny access AWS permits.
//!   An explicit `Deny` in a document that CAN be read still refuses, and a permissions
//!   boundary the store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//! * **The EDGE data plane.** A viewer request to a distribution's own domain carries no
//!   AWS credential and names no modelled operation, so it is not an API call of this
//!   account at all; it is served outside this gate (`crate::service::edge`), and what
//!   admits or refuses it is the distribution's own configuration.
//!
//! An expired session credential speaks for nobody: it carries no documents and no
//! principal spelling, so every action it asks for is refused. (A session the
//! authenticating layer has already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// This service's IAM action prefix: an action is `cloudfront:<OperationName>` ("Actions
/// defined by Amazon CloudFront", IAM Service Authorization Reference), which is this
/// service's `arnNamespace` (`smithy_query(query="service_info")`) — the same string every
/// ARN this service mints carries ([`crate::state::distribution_arn`]).
pub const ACTION_PREFIX: &str = "cloudfront";

/// The resource IAM writes for a request that addresses no resource of its own.
pub const ANY_RESOURCE: &str = "*";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The one prefix every route of this API carries: its version, which is the API version
/// this service's own resource paths are built under
/// ([`crate::state::distribution_path`]).
const ROUTE_PREFIX: &str = "/2020-05-31/";

/// The resource-kind segment whose routes interpose a literal segment between the kind and
/// the identity's own id (`/origin-access-identity/cloudfront/{Id}`, from this service's
/// own `origin_access_identity_path`), so the identifier is one segment further along.
const ORIGIN_ACCESS_IDENTITY_KIND: &str = "origin-access-identity";

/// The query parameter the tagging routes address their resource by: `TagResource`,
/// `UntagResource` and `ListTagsForResource` all bind `Resource` — "an ARN of a CloudFront
/// resource" — to `@httpQuery("Resource")` over the one `/2020-05-31/tagging` path, so the
/// resource those calls are about is never on the route.
const TAGGING_RESOURCE_PARAMETER: &str = "Resource";

/// How an ARN of one resource kind is minted from an identifier of that kind: the crate's
/// own minting function for that kind.
type MintArn = fn(&str) -> String;

/// The resource kinds a route addresses by the resource's OWN identifier, each with the
/// function THIS crate mints an ARN of that kind with — so the ARN a statement is matched
/// against is the very one the service hands a client back, and no ARN form is restated
/// here.
///
/// The segment is the route's own (`OPERATIONS.md`, rendered from the model): the
/// distribution routes `/distribution/{Id}` and the monitoring-subscription routes
/// `/distributions/{DistributionId}` both address a distribution, `/distribution-tenant`,
/// `/streaming-distribution`, `/anycast-ip-list` and `/vpc-origin` each address their own
/// kind, and `/function/{Name}` addresses a function — whose ARN carries that very name.
/// Every other route either addresses no single resource or names it in the request
/// document, and is judged against [`ANY_RESOURCE`] (see the module docs).
const ARN_OF_KIND: &[(&str, MintArn)] = &[
    ("distribution", crate::state::distribution_arn),
    ("distributions", crate::state::distribution_arn),
    ("distribution-tenant", crate::state::distribution_tenant_arn),
    (
        "streaming-distribution",
        crate::state::streaming_distribution_arn,
    ),
    ("anycast-ip-list", crate::state::anycast_ip_list_arn),
    ("vpc-origin", crate::state::vpc_origin_arn),
    ("function", crate::state::function_arn),
];

/// The IAM action `operation` performs: this service's namespace and the operation's own
/// name ([`smithy_server_runtime::Operation::NAME`]), which is how IAM publishes the
/// actions of a service.
#[must_use]
pub fn action_of(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// One refused request: the sentence IAM states the refusal with, and nothing else — the
/// status and the error code are the envelope's ([`super::envelope::access_denied`]).
#[derive(Debug, Clone)]
pub struct Denial {
    /// What the caller is told: who was refused, which action, on which resource when the
    /// request addressed one, and which of IAM's rules refused it.
    pub message: String,
}

/// The wire answer to this request when IAM refuses it, or `None` when IAM permits it.
///
/// The ONE call site of this gate: the seam every request the model's routing table
/// resolves runs through ([`super::envelope::ErrorEnvelope`]) asks this before it dispatches
/// anything, with `operation` the name the model names the request's operation by.
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    req: &RtRequest,
) -> Option<RtResponse> {
    let resource = judged_resource(req);
    authorize(engine, ctx, operation, &resource)
        .err()
        .map(|denial| super::envelope::access_denied(&denial.message))
}

/// The resource this request is judged against: the ARN of the resource it ADDRESSES, else
/// [`ANY_RESOURCE`] (see the module docs for what each covers).
#[must_use]
pub fn judged_resource(req: &RtRequest) -> String {
    addressed_resource(req.uri()).unwrap_or_else(|| ANY_RESOURCE.to_string())
}

/// The ARN of this service the request URI `uri` addresses, or `None` for a request that
/// addresses no one resource of it.
fn addressed_resource(uri: &str) -> Option<String> {
    routed_resource(::service_common::request_path(uri)).or_else(|| tagged_resource(uri))
}

/// The ARN the ROUTE `path` addresses: the kind its resource segment names and the
/// identifier beside it.
fn routed_resource(path: &str) -> Option<String> {
    let mut segments = path
        .strip_prefix(ROUTE_PREFIX)?
        .split('/')
        .filter(|segment| !segment.is_empty());
    let kind = segments.next()?;
    if kind == ORIGIN_ACCESS_IDENTITY_KIND {
        // The routes of that kind spell the literal `cloudfront` before the id.
        segments.next()?;
    }
    let identifier = ::service_common::percent_decode_path_label(segments.next()?);
    arn_of(kind, &identifier)
}

/// The ARN the `Resource` query parameter of a tagging route carries, read the way the
/// request boundary reads a query value.
fn tagged_resource(uri: &str) -> Option<String> {
    let carried = super::envelope::query_value(
        super::envelope::request_query(uri),
        TAGGING_RESOURCE_PARAMETER,
    )?;
    arn_of_this_service(&::smithy_server_runtime::percent_decode(carried))
}

/// The ARN `identifier` names for a resource of `kind`: the identifier itself when it is
/// already an ARN of this service, else the ARN this service mints for an identifier of
/// that kind — and `None` for a kind whose ARN this gate does not mint.
fn arn_of(kind: &str, identifier: &str) -> Option<String> {
    if let Some(arn) = arn_of_this_service(identifier) {
        return Some(arn);
    }
    let mint = ARN_OF_KIND
        .iter()
        .find_map(|(named, mint)| (*named == kind).then_some(mint))?;
    Some(mint(identifier))
}

/// `value` when it is an ARN of THIS service naming a resource, read through the crate's one
/// reading of such an ARN ([`crate::state::cloudfront_arn_resource`]).
fn arn_of_this_service(value: &str) -> Option<String> {
    crate::state::cloudfront_arn_resource(value).map(|_| value.to_string())
}

/// Authorize this request, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]) and `resource` the resource it ADDRESSES
/// ([`judged_resource`]).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the refusal otherwise. Taken BEFORE the operation runs, so a refused
/// request reads and writes nothing.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises
/// the session expiries the clock has already made due; that write is owed at the same
/// instant whether or not anybody called, which is why a REPORTING operation of this
/// service may take this decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), Denial> {
    let action = action_of(operation);
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, and a distribution publishes no resource-based policy (see the module
    // docs), so the decision below rests on the action and the resource.
    let request = iam::Request::new(&action, resource, &caller.principals);
    let documents = iam::prepare_documents(&caller.documents);
    let identity_said = iam::identity_decision_prepared(&documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    let granted = iam::is_allowed(identity_said, Decision::NotStated)
        || may_allow(&documents, &action, resource)
        || caller.is_unbounded();
    if !granted {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            &iam::no_identity_policy_allows(&action),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy
    // can grant" (IAM, permissions boundaries): a grant the boundary does not also allow
    // is not a permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let boundary = iam::prepare_documents(::std::slice::from_ref(boundary));
        let reason = match iam::identity_decision_prepared(&boundary, &request) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the
            // identity-based policies gave.
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => {
                if may_allow(&boundary, &action, resource) {
                    return Ok(());
                }
                iam::no_permissions_boundary_allows(&action)
            }
        };
        return Err(not_authorized(&caller.arn, &action, resource, &reason));
    }
    Ok(())
}

/// Whether `documents` could allow `action` at all, asked only for a request judged against
/// [`ANY_RESOURCE`] — one that addresses no resource on its route, and so may still be about
/// a resource a statement is scoped to (see the module docs).
///
/// A request that DOES address a resource is decided against that resource alone, where the
/// resource element is what AWS matches it on.
fn may_allow(documents: &[iam::PreparedDocument], action: &str, resource: &str) -> bool {
    resource == ANY_RESOURCE && iam::identity_may_allow_action(documents, action)
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it — the ONE wording of that sentence
/// ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), so this
/// service's refusal reads as every other service's does. Every value in it comes from the
/// request and from the stored policies.
///
/// A request that addresses no resource of its own is judged against [`ANY_RESOURCE`], and
/// the sentence states no resource clause for it: there is nothing to name.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> Denial {
    let message = if resource == ANY_RESOURCE {
        format!("User: {principal} is not authorized to perform: {action} {reason}")
    } else {
        iam::not_authorized_by_identity_policy(principal, action, resource, reason)
    };
    Denial { message }
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the
    /// IAM role behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
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
    fn is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's
    /// own accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    ///
    /// `principal_policies` is where the collection rule itself lives — a role's or user's
    /// inline policies, the default version of each managed policy attached to it, and for
    /// a user the policies of the groups it belongs to — so this service reads an
    /// identity's permissions exactly as IAM's own authorizer does.
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
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let identity = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
        ),
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential.
        None => Ok(sts_store::ambient_identity()),
    };
    let identity = match identity {
        Ok(identity) => identity,
        // The session credential has expired, so it speaks for nobody: no documents, no
        // principal spelling, and every action it asks for falls to the default deny. The
        // key id it presented is NOT quoted back — this module echoes no value of the
        // request — and in the assembled emulator the layer that authenticates a request
        // refuses an expired credential before any handler runs.
        Err(expired) => {
            let _ = expired;
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                principals: Vec::new(),
                documents: Vec::new(),
                boundary: None,
                account_credential: false,
                unreadable_attachment: false,
            };
        }
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        documents: Vec::new(),
        boundary: None,
        unreadable_attachment: false,
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a key it never
    // minted is the account's ambient identity — but IAM's store maps an access key id to
    // the user that owns it (`access_keys`, the map IAM's own `GetUser` resolves a caller
    // through), so a request signed with one speaks for THAT user and is judged on the
    // user's policies rather than served as the account. A key the IAM store does not hold
    // either really is the account's own published credential.
    if caller.account_credential {
        if let Some(user) = store
            .access_keys
            .get(&access_key_id)
            .and_then(|record| store.users.get(&record.user_key))
        {
            caller.account_credential = false;
            caller.arn = user.arn.clone();
            caller.principals = vec![user.arn.clone()];
        }
    }
    if let Some(role_name) =
        ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed
        // by the ARN the account holds it under — which carries its PATH, where the
        // session ARN does not.
        if let Some(held) = store.roles.get(&iam_store::name_key(role_name)) {
            caller.principals.push(held.arn.clone());
            let role_arn = held.arn.clone();
            caller.read_policies_of(&store, &role_arn);
        }
    } else if ::service_common::iam_role::user_name(&caller.arn).is_some() {
        // An IAM user's permissions are its own, under the ARN it was resolved by.
        let user_arn = caller.arn.clone();
        caller.read_policies_of(&store, &user_arn);
    }
    caller
}

/// The principal a request speaks for is read through the tree's ONE reading of an identity
/// ARN ([`::service_common::iam_role`]): `assumed_role_session_role_name` for the role behind
/// an STS session and `user_name` for an IAM user, both of which also settle that the ARN is
/// partition-global and of THIS account, so a value naming another account's identity
/// resolves to no principal of this one.
#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the action this gate asks about is `cloudfront:<OperationName>`, so a policy
    /// scoped to this service's actions covers every operation of it and nothing outside
    /// it, and a policy naming one operation covers that operation alone.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document =
            r#"{"Statement":[{"Effect":"Allow","Action":"cloudfront:*","Resource":"*"}]}"#;
        for operation in [
            "CreateInvalidation",
            "GetInvalidation",
            "ListInvalidations",
            "GetDistributionConfig",
            "CreateInvalidationForDistributionTenant",
        ] {
            let action = action_of(operation);
            let reached = iam::identity_decision(
                &[document.to_string()],
                &iam::Request::new(&action, "*", &[]),
            );
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected cloudfront:* to allow {action}, got {reached:?}"
            );
        }
        let outside = iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new("s3:GetObject", "*", &[]),
        );
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected cloudfront:* to grant nothing outside this \
             service, got {outside:?}"
        );
    }

    /// RULE: the refusal names the principal, the action, the resource the request
    /// addressed and which of IAM's rules refused — every part taken from the request and
    /// the stored policies, in IAM's own wording (spelled out here as the LITERAL AWS
    /// states rather than through the shared builder, so a change to that wording fails
    /// this) — and it names no resource for a request that addresses none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = "arn:aws:sts::000000000000:assumed-role/invalidator/session-1";
        let distribution = crate::state::distribution_arn("E1EXAMPLE");
        // (case, the operation, the resource it addressed, the clause the sentence carries)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "a refused invalidation",
                "CreateInvalidation",
                &distribution,
                format!(" on resource: {distribution}"),
            ),
            (
                "a refused read of a distribution's configuration",
                "GetDistributionConfig",
                &distribution,
                format!(" on resource: {distribution}"),
            ),
            (
                "a refused call that addresses no resource of its own",
                "ListDistributions",
                ANY_RESOURCE,
                String::new(),
            ),
        ];
        for (case, operation, resource, clause) in rows {
            let action = action_of(operation);
            for reason in [
                iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
                iam::no_identity_policy_allows(&action),
            ] {
                let refused = not_authorized(principal, &action, resource, &reason);
                assert_eq!(
                    refused.message,
                    format!(
                        "User: {principal} is not authorized to perform: {action}{clause} \
                         {reason}"
                    ),
                    "case {case:?}: expected IAM's wording, got {:?}",
                    refused.message
                );
            }
        }
    }

    /// RULE: the resource a request is judged against is the ARN of the resource its own
    /// route (or, for the tagging routes, its `Resource` parameter) addresses, and the
    /// any-resource wildcard for a request that addresses none.
    #[test]
    fn the_judged_resource_is_the_one_the_request_addresses() {
        let distribution = crate::state::distribution_arn("E1EXAMPLE");
        let tenant = crate::state::distribution_tenant_arn("dt_1EXAMPLE");
        let streaming = crate::state::streaming_distribution_arn("E2EXAMPLE");
        let anycast = crate::state::anycast_ip_list_arn("aip_3EXAMPLE");
        let vpc_origin = crate::state::vpc_origin_arn("vo_4EXAMPLE");
        let function = crate::state::function_arn("shaper");
        let key_value_store = crate::state::key_value_store_arn("kvs-5");
        // (case, the request URI, the resource it is judged against)
        let rows: Vec<(&str, String, String)> = vec![
            (
                "a distribution read",
                "/2020-05-31/distribution/E1EXAMPLE".to_string(),
                distribution.clone(),
            ),
            (
                "the configuration half of that read",
                "/2020-05-31/distribution/E1EXAMPLE/config".to_string(),
                distribution.clone(),
            ),
            (
                "an invalidation of that distribution",
                "/2020-05-31/distribution/E1EXAMPLE/invalidation/I9EXAMPLE".to_string(),
                distribution.clone(),
            ),
            (
                "the monitoring subscription of that distribution",
                "/2020-05-31/distributions/E1EXAMPLE/monitoring-subscription".to_string(),
                distribution.clone(),
            ),
            (
                "a distribution tenant's invalidations",
                "/2020-05-31/distribution-tenant/dt_1EXAMPLE/invalidation".to_string(),
                tenant.clone(),
            ),
            (
                "a streaming distribution",
                "/2020-05-31/streaming-distribution/E2EXAMPLE/config".to_string(),
                streaming.clone(),
            ),
            (
                "an Anycast static IP list",
                "/2020-05-31/anycast-ip-list/aip_3EXAMPLE".to_string(),
                anycast.clone(),
            ),
            (
                "a VPC origin",
                "/2020-05-31/vpc-origin/vo_4EXAMPLE".to_string(),
                vpc_origin.clone(),
            ),
            (
                "a function, whose ARN carries its name",
                "/2020-05-31/function/shaper/publish".to_string(),
                function.clone(),
            ),
            (
                "a label that is already an ARN of this service",
                format!(
                    "/2020-05-31/key-value-store/{}",
                    key_value_store.replace(':', "%3A").replace('/', "%2F")
                ),
                key_value_store.clone(),
            ),
            (
                "a tagging call, which names its resource in a parameter",
                format!(
                    "/2020-05-31/tagging?Operation=Tag&Resource={}",
                    distribution.replace(':', "%3A").replace('/', "%2F")
                ),
                distribution.clone(),
            ),
            (
                "the listing of a kind, which addresses no one resource",
                "/2020-05-31/distribution?MaxItems=2".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a create, which addresses no resource yet",
                "/2020-05-31/cache-policy".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a kind whose ARN this gate does not mint",
                "/2020-05-31/cache-policy/C7EXAMPLE/config".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "an origin access identity, under the literal its routes interpose",
                "/2020-05-31/origin-access-identity/cloudfront/E8EXAMPLE".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "an account-level call",
                "/2020-05-31/verify-dns-configuration".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a path outside this API's version prefix",
                "/".to_string(),
                ANY_RESOURCE.to_string(),
            ),
        ];
        for (case, uri, expected) in rows {
            let actual = addressed_resource(&uri).unwrap_or_else(|| ANY_RESOURCE.to_string());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the judged resource {expected:?}, got {actual:?}"
            );
        }
    }
}
