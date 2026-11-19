// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLER of a request to this service perform the action it asks for —
//! the decision taken ahead of EVERY operation, before any handler reaches the store.
//!
//! Every request to an AWS service is signed, and the credential it is signed with speaks for
//! a principal: an IAM user's long-term access key, or a temporary session the token service
//! minted for a ROLE — which is what an SDK call made under an assumed role presents. AWS
//! decides such a request at the IAM layer, ahead of the service looking at the resource the
//! request named: a principal whose policies do not allow the action is told it is not
//! authorized, and the call has NO effect — a refused `DeleteCacheSubnetGroup` leaves the
//! group where it was and a refused `CreateReplicationGroup` stores nothing. So the decision
//! is a fact about the REQUEST's credential and the policies in force at the instant of the
//! call rather than about the resource the request names, which is why it is taken once here
//! rather than per handler.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for ([`sts_store::resolve_request_caller`], the same place
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one the
//!    caller reads back for the credentials it signed with), else the IAM user that holds the
//!    long-term key, else the account's own published credential;
//! 2. **what it may do** — every document in force on that credential: the identity-based
//!    policy documents the IAM store holds for the principal and the permissions boundary
//!    that caps them, read through the IAM service's OWN accessors (`principal_policies`,
//!    `permissions_boundary_document`), so a document `iam:PutRolePolicy` wrote takes effect
//!    on the next request and deleting it lifts it again — beside the SESSION POLICIES the
//!    mint of this very credential scoped it with
//!    ([`sts_store::session_scoping_documents`]), which cap those documents the same way the
//!    boundary does: "the resulting session's permissions are the intersection of the role's
//!    identity-based policy and the session policies" (`AssumeRoleRequest$Policy`);
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation of
//!    those documents against `(action, resource)`, and the SENTENCE a refusal carries, so
//!    nothing of IAM's wording is spelled here: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the collection nor the decision does. They stay
//! in this service crate because the IAM and STS crates depend on `service-common`, so the
//! shared crate cannot read their stores without a dependency cycle.
//!
//! ## The action and the resource
//!
//! The action is `elasticache:<OperationName>` ([`action_for`]): `elasticache` is this
//! service's `arnNamespace` and its `sigv4Name` alike (`smithy_query(query="service_info")`),
//! which is the namespace its actions are published under.
//!
//! The resource is what the request's OWN parameters address ([`addressed_resource`]), built
//! from the identifier the request carried and the Region it was signed for rather than
//! resolved out of the store — so the decision is the same whether or not that resource exists
//! here, and a refusal tells an unauthorized caller nothing about what the emulator holds. A
//! request that addresses no resource of its own (a catalog read, an account-wide listing) is
//! decided on the weaker question the shared evaluator publishes for exactly that case:
//! whether any readable document could allow the action at all.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS nor
//!   IAM holds a record for resolves to the ambient identity, which is not a principal the IAM
//!   store holds policies for — the position AWS account root credentials are in. Such a
//!   request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary or
//!   session policy the store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no document, so every action it
//! asks for is refused, and the refusal names that state rather than the key it presented,
//! which this module never quotes back.

use ::std::borrow::Cow;
use ::std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::ProtocolError;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::{OperationContext, PreDispatchProtocol, Refused, Service};

/// The action namespace this service's operations are published under, which is its
/// `arnNamespace` and its SigV4 signing name alike (`smithy_query(query="service_info")`): a
/// policy statement spells one of them `elasticache:<OperationName>`.
pub const ACTION_PREFIX: &str = crate::state::ARN_NAMESPACE;

/// The resource an action that addresses no single resource is decided against: every
/// resource, which is what a statement has to cover to permit such a call.
pub const EVERY_RESOURCE: &str = "*";

/// The error code AWS refuses an unauthorized ElastiCache request with, answered with the 403
/// the framework's own pre-dispatch renderer gives an authorization refusal.
pub const ACCESS_DENIED: &str = "AccessDenied";

/// The `<Message>` the framework's renderer would otherwise pick for a refusal of this class;
/// replaced per request by the sentence IAM states ([`Caller::refusal`]).
const NOT_AUTHORIZED_SENTENCE: &str = "User is not authorized to perform this action";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The envelope a pre-dispatch failure of this service renders in: this service speaks
/// awsQuery (`smithy_query(query="service_info")`), whose error envelope the framework owns.
const QUERY_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Query { ec2: false };

/// The most form parameters this gate walks in one request: a bound on the work a request can
/// ask of it, well above the longest identifier list an ElastiCache request carries.
const MAX_PARAMETERS_READ: usize = 4_096;

/// The member every tagging operation of this model addresses its resource by: an ARN, which
/// IS the resource the decision is taken against (`smithy_query(query="operation",
/// operation="AddTagsToResource")`).
const RESOURCE_ARN_MEMBER: &str = "ResourceName";

/// How a request names the resource it acts on: the word the operation's own name carries for
/// that kind of resource, the input member the identifier arrives in, and the ARN form this
/// service mints for it.
///
/// A named record rather than a tuple, so each part of an entry says what it is at the place it
/// is read.
struct AddressedBy {
    /// The word an operation's own name carries when it acts on this kind of resource.
    kind: &'static str,
    /// The input member the identifier of this kind arrives in.
    member: &'static str,
    /// The ARN this service mints for a resource of this kind, from the Region and the
    /// identifier.
    arn: fn(&str, &str) -> String,
}

/// Every kind of resource a request of this service addresses ([`AddressedBy`]).
///
/// The members are this model's own (`smithy_query(query="operation")`) and the ARN forms this
/// crate's own ([`crate::state::subnet_group_arn`] and its siblings), so the resource a
/// statement is decided against is the very ARN the resource reports for itself. Ordered from
/// the most specific kind to the least, because one word contains another — a
/// `GlobalReplicationGroup` operation also reads as a `ReplicationGroup` one, and a
/// `ServerlessCacheSnapshot` as a `Snapshot` one — so the first entry whose word the operation
/// name carries decides.
const ADDRESSED_BY: &[AddressedBy] = &[
    AddressedBy {
        kind: "GlobalReplicationGroup",
        member: "GlobalReplicationGroupId",
        arn: crate::state::global_replication_group_arn,
    },
    AddressedBy {
        kind: "ServerlessCacheSnapshot",
        member: "ServerlessCacheSnapshotName",
        arn: crate::state::serverless_cache_snapshot_arn,
    },
    AddressedBy {
        kind: "ServerlessCache",
        member: "ServerlessCacheName",
        arn: crate::state::serverless_cache_arn,
    },
    AddressedBy {
        kind: "ReplicationGroup",
        member: "ReplicationGroupId",
        arn: crate::state::replication_group_arn,
    },
    AddressedBy {
        kind: "CacheSubnetGroup",
        member: "CacheSubnetGroupName",
        arn: crate::state::subnet_group_arn,
    },
    AddressedBy {
        kind: "CacheSecurityGroup",
        member: "CacheSecurityGroupName",
        arn: crate::state::security_group_arn,
    },
    AddressedBy {
        kind: "CacheParameterGroup",
        member: "CacheParameterGroupName",
        arn: crate::state::parameter_group_arn,
    },
    AddressedBy {
        kind: "CacheCluster",
        member: "CacheClusterId",
        arn: crate::state::cluster_arn,
    },
    AddressedBy {
        kind: "UserGroup",
        member: "UserGroupId",
        arn: crate::state::user_group_arn,
    },
    AddressedBy {
        kind: "User",
        member: "UserId",
        arn: crate::state::user_arn,
    },
    AddressedBy {
        kind: "Snapshot",
        member: "SnapshotName",
        arn: crate::state::snapshot_arn,
    },
    AddressedBy {
        kind: "ReservedCacheNode",
        member: "ReservedCacheNodeId",
        arn: crate::state::reservation_arn,
    },
];

/// The IAM action `operation` is performed under: `elasticache:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The refusal a request to `operation` on `resource` gets when the documents in force do not
/// allow it — the sentence IAM states such a refusal with — and `None` when they do.
///
/// `operation` is the operation's own shape name and `resource` the ARN the request ADDRESSES
/// ([`addressed_resource`]).
///
/// Asked BEFORE this service's store is read, and with an ARN built from the identifier the
/// request carried rather than from a stored record, so the decision does not depend on
/// whether that resource exists.
///
/// Resolving the caller goes through STS's own resolution, which first materialises the session
/// expiries the clock has already made due; that write is owed at the same instant whether or
/// not anybody called, which is why a REPORTING operation of this service passes this gate too.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    caller(engine, ctx).refusal(&action_for(operation), resource)
}

/// This service's `AccessDenied` response for a request whose caller the policies in force do
/// not permit to perform the action it named, and `None` for every request they permit.
///
/// The action asked about is the operation's own name, taken from the request's routing key and
/// only when the routing table SERVES that key: so it is always a modelled operation's name,
/// never a value the body carried, and a request naming no operation of this service still
/// reaches the routing failure rather than an authorization decision about a name nothing
/// serves.
///
/// The code is AWS's own ([`ACCESS_DENIED`]) and the sentence IAM's ([`Caller::refusal`]); the
/// status is the 403 the framework's pre-dispatch renderer answers an authorization refusal
/// with, and the envelope is the protocol's, so this refusal is rendered exactly like every
/// other pre-dispatch one rather than by a second hand-written envelope.
#[must_use]
pub fn refusal_response<Eng: Send + Sync + 'static>(
    engine: &Eng,
    svc: &dyn Service,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> Option<RtResponse> {
    let operation = svc.definition().protocol().routing_key(req).ok()?;
    svc.definition().routing_table().get(operation)?;
    let region = crate::state::request_region(ctx.parts());
    let addressed = addressed_resource(req, operation, &region);
    let message = refusal(engine, ctx, operation, &addressed)?;
    Some(::smithy_server_runtime::protocol_error_response_with(
        QUERY_ENVELOPE,
        ProtocolError::Unauthenticated {
            code: ACCESS_DENIED,
            message: NOT_AUTHORIZED_SENTENCE,
        },
        Some(Refused {
            code: ACCESS_DENIED.to_string(),
            message,
        }),
    ))
}

/// The ARN the request `req` to `operation` ADDRESSES, and [`EVERY_RESOURCE`] for a request
/// that addresses no resource of its own.
///
/// An awsQuery request carries its parameters as an `application/x-www-form-urlencoded` body,
/// so they are read by the crate that decodes that media type rather than by a split of the
/// bytes, and at most [`MAX_PARAMETERS_READ`] of them.
///
/// Three readings, in the order a request narrows what it addresses:
///
/// * [`RESOURCE_ARN_MEMBER`] — the tagging operations, which address their resource BY ARN;
///   a value that is no ARN of this service names no resource of it, so the decision falls to
///   the weaker question rather than to a string the caller chose.
/// * the identifier for the kind of resource the OPERATION's own name carries
///   ([`ADDRESSED_BY`]): `DeleteCacheSubnetGroup` acts on the cache subnet group its
///   `CacheSubnetGroupName` names, whatever other identifiers the request also carries.
/// * failing that, the first identifier of [`ADDRESSED_BY`] the request carries at all, which
///   is what an operation whose name does not spell its resource kind
///   (`IncreaseReplicaCount`, `TestFailover`) addresses.
#[must_use]
pub fn addressed_resource(req: &RtRequest, operation: &str, region: &str) -> String {
    let parameters: Vec<(Cow<'_, str>, Cow<'_, str>)> = request_parameters(req).collect();
    let text = |member: &str| {
        parameters
            .iter()
            .find(|(name, value)| name == member && !value.is_empty())
            .map(|(_, value)| value.to_string())
    };
    if let Some(named) = text(RESOURCE_ARN_MEMBER) {
        if crate::state::parse_resource_arn(&named).is_some() {
            return named;
        }
    }
    for addressed in ADDRESSED_BY {
        if operation.contains(addressed.kind) {
            if let Some(identifier) = text(addressed.member) {
                return (addressed.arn)(region, &identifier);
            }
        }
    }
    for addressed in ADDRESSED_BY {
        if let Some(identifier) = text(addressed.member) {
            return (addressed.arn)(region, &identifier);
        }
    }
    EVERY_RESOURCE.to_string()
}

/// The `(name, value)` form parameters `req` carries, body first and then its query string, at
/// most [`MAX_PARAMETERS_READ`] of them.
fn request_parameters(req: &RtRequest) -> impl Iterator<Item = (Cow<'_, str>, Cow<'_, str>)> {
    let body = req.body().bytes().unwrap_or_default();
    let query = req
        .uri()
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    ::form_urlencoded::parse(body)
        .chain(::form_urlencoded::parse(query.as_bytes()))
        .take(MAX_PARAMETERS_READ)
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// Every spelling of the principal a statement may name it by: the ARN STS reports, the
    /// ARN of the IAM role behind an assumed-role session, and the account it belongs to.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose document
    /// this emulator holds.
    boundary: Option<String>,
    /// The session policies the mint of this credential scoped it with, as they read at the
    /// instant of this request. Empty for a credential no mint scoped.
    scope: sts_store::ScopingPolicies,
    /// Set when this request presents the account's own published credential, which holds no
    /// IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not hold
    /// — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
    /// The session tags the principal carries: the values `aws:PrincipalTag/<key>` holds for a
    /// request this credential signs.
    tags: Vec<(String, String)>,
    /// Set when the principal's session tags are KNOWN — a session this emulator's STS registry
    /// holds — which is what lets a clause over a tag key the principal does NOT carry be
    /// decided rather than left undecided.
    tags_known: bool,
    /// The condition keys the web identity this credential was minted by exchanging publishes,
    /// as STS recorded them on the session. Empty for a credential no such exchange minted.
    federated_claims: Vec<(String, String)>,
}

/// The condition key family a principal's session tags answer for, as IAM publishes it.
const PRINCIPAL_TAG_CONDITION_KEY_FAMILY: &str = "aws:PrincipalTag/";

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no IAM
    /// policies, or a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// The values the condition keys this caller answers for hold for this request.
    fn context(&self) -> Vec<(String, String)> {
        let mut context: Vec<(String, String)> = self
            .tags
            .iter()
            .map(|(key, value)| {
                (
                    format!("{PRINCIPAL_TAG_CONDITION_KEY_FAMILY}{key}"),
                    value.clone(),
                )
            })
            .collect();
        context.extend(self.federated_claims.iter().cloned());
        context
    }

    /// The keys this caller answers for whether or not it carries a value for each.
    fn published_keys(&self) -> Vec<&str> {
        let mut published = Vec::new();
        if self.tags_known {
            published.push(PRINCIPAL_TAG_CONDITION_KEY_FAMILY);
        }
        published.extend(sts_store::federated_claim_keys(&self.federated_claims));
        published
    }

    /// `Some(message)` when the documents in force do not let this caller perform `action` on
    /// `resource` — the sentence the refusal carries — and `None` when they do.
    ///
    /// IAM's order: the scope the credential was minted with can only refuse, so its explicit
    /// deny is taken first ([`sts_store::ScopingPolicies::decision`], the tree's one reading of
    /// a resolved scope); the account's own published credential holds no IAM document and owns
    /// its resources (see the module docs); otherwise an explicit `Deny` in a document that CAN
    /// be read refuses the request whatever else the principal carries; the call then needs a
    /// grant, which a caller this gate does not bound has by definition; and that grant is
    /// capped by the permissions boundary ("a permissions boundary sets the maximum permissions
    /// that an identity-based policy can grant", IAM, permissions boundaries) and by the scope,
    /// each of which takes permissions away and grants none of its own — the refusal naming
    /// which document withheld the grant, because they need different fixes.
    fn refusal(&self, action: &str, resource: &str) -> Option<String> {
        // An identity-based document states no `Principal` element; the spellings are carried
        // so a statement conditioned on who signed is decided on this caller.
        let context = self.context();
        let published = self.published_keys();
        let request = iam::Request::new(action, resource, &self.principals)
            .with_context(&context)
            .with_published_keys(&published);
        let refusal = |reason: &str| {
            Some(iam::not_authorized_by_identity_policy(
                &self.arn, action, resource, reason,
            ))
        };
        let scope = self.scope.prepare();
        let scoped = scope.decision(&request);
        if scoped == Decision::Denied {
            return refusal(iam::SESSION_POLICY_EXPLICIT_DENY);
        }
        if self.account_credential {
            return None;
        }
        let said = iam::identity_decision(&self.documents, &request);
        if said == Decision::Denied {
            return refusal(iam::IDENTITY_POLICY_EXPLICIT_DENY);
        }
        if !(grants(said, &self.documents, action, resource) || self.identity_is_unbounded()) {
            return refusal(&iam::no_identity_policy_allows(action));
        }
        if let Some(boundary) = &self.boundary {
            let capped = ::std::slice::from_ref(boundary);
            let said_by_boundary = iam::identity_decision(capped, &request);
            if said_by_boundary == Decision::Denied {
                return refusal(iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY);
            }
            if !grants(said_by_boundary, capped, action, resource) {
                return refusal(&iam::no_permissions_boundary_allows(action));
            }
        }
        // The scope is read for the resource the request addresses by the same rule the
        // documents above are ([`grants`]): a request that addresses none of its own is decided
        // on the weaker question the resolved scope publishes for exactly that case
        // ([`sts_store::PreparedScopingPolicies::may_allow_action`]).
        if scoped != Decision::Allowed
            && !(resource == EVERY_RESOURCE && scope.may_allow_action(action))
        {
            return refusal(&iam::no_session_policy_allows(action));
        }
        None
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's OWN
    /// accessors: the documents in force on it, whether it carries an attachment whose document
    /// this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(&mut self, store: &sts_store::IamState, identity_arn: &str) {
        if let Some(policies) = store.principal_policies(identity_arn) {
            self.documents = policies.documents;
            // An attachment the collection resolved no document for is an entry of AWS's own
            // catalog this emulator does not carry: a grant it cannot read.
            self.unreadable_attachment = !policies.uncarried.is_empty();
        }
        self.boundary = store.permissions_boundary_document(identity_arn);
    }
}

/// Whether `documents`, which answered `decision` about the request, GRANT it.
///
/// A request that addresses a resource is granted by a statement covering that resource, which
/// is what `decision` answers. A request that addresses no resource of its own
/// ([`EVERY_RESOURCE`]) is decided on the shared evaluator's weaker question
/// ([`iam::identity_may_allow_action`]), published for exactly that case: refusing on the
/// decision over `*` alone would refuse calls a resource-scoped statement permits.
fn grants(decision: Decision, documents: &[String], action: &str, resource: &str) -> bool {
    decision == Decision::Allowed
        || (resource == EVERY_RESOURCE
            && iam::identity_may_allow_action(&iam::prepare_documents(documents), action))
}

/// What a credential resolved to: the facts a decision needs, copied out of whichever registry
/// answered for the access key id.
struct Resolved {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it.
    arn: String,
    /// The ARN of the role an assumed-role session is a session of, which is also a spelling a
    /// statement may name the principal by.
    role_arn: Option<String>,
    /// WHOSE identity-based documents are in force for this credential, which is not always the
    /// principal it speaks for ([`sts_store::Caller::identity_policies_arn`]).
    policies_arn: String,
    tags: Vec<(String, String)>,
    /// Whether those tags are the whole set the principal carries (see [`Caller::tags_known`]).
    tags_known: bool,
    federated_claims: Vec<(String, String)>,
    session_policies: sts_store::SessionPolicies,
}

impl From<sts_store::Caller> for Resolved {
    fn from(caller: sts_store::Caller) -> Self {
        Self {
            policies_arn: caller.identity_policies_arn().to_string(),
            arn: caller.identity.arn,
            role_arn: caller.role_arn,
            tags: caller.tags,
            // The tags are the STS registry's record of the session, so they are known exactly
            // for a credential it minted.
            tags_known: caller.kind.is_some(),
            federated_claims: caller.federated_claims,
            session_policies: caller.session_policies,
        }
    }
}

/// The principal this request speaks for, with every document in force on it.
fn caller<Eng: Send + Sync + 'static>(engine: &Eng, ctx: &OperationContext<'_>) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let iam = sts_store::iam_state(engine);
    // A long-term access key speaks for the IAM user that holds it, which is the identity STS
    // itself reports for it; a key no store holds is the account's own published credential.
    let long_term = match &iam {
        Some(iam) => sts_store::long_term_identity(Some(&iam.lock()), &access_key_id),
        None => sts_store::ambient_identity(),
    };
    let resolved = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            long_term.clone(),
        )
        .map(Resolved::from),
        // No STS store on this engine: the credential names no session, so it speaks for the
        // long-term identity it was resolved to and is judged on that identity's documents.
        None => Ok(Resolved {
            arn: long_term.arn.clone(),
            role_arn: None,
            policies_arn: long_term.arn.clone(),
            tags: Vec::new(),
            tags_known: false,
            federated_claims: Vec::new(),
            session_policies: sts_store::SessionPolicies::default(),
        }),
    };
    // An expired session credential speaks for nobody: it carries no document, so every action
    // it asks for is refused — and the refusal names the credential's STATE rather than the key
    // id that was presented, which this module never quotes back.
    let Ok(resolved) = resolved else {
        return Caller {
            arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
            ..Caller::default()
        };
    };
    let mut caller = Caller {
        account_credential: sts_store::identity_is_account_credential(&resolved.policies_arn),
        principals: vec![resolved.arn.clone()],
        arn: resolved.arn,
        tags: resolved.tags,
        tags_known: resolved.tags_known,
        federated_claims: resolved.federated_claims,
        ..Caller::default()
    };
    // An assumed-role session is also named by the ARN of the role it is a session of, which is
    // the spelling a statement uses to name the role itself.
    if let Some(role_arn) = &resolved.role_arn {
        if !caller.principals.contains(role_arn) {
            caller.principals.push(role_arn.clone());
        }
    }
    // The account the principal belongs to, which is how a statement names a whole account
    // rather than one of its principals.
    caller
        .principals
        .extend(iam::account_principals(&caller.arn));
    // The IAM store of the same engine, read ONCE for both halves of what the caller may do:
    // the documents in force on its identity, and the documents its credential was scoped with.
    // An engine carrying no IAM store holds neither.
    let held = iam.as_ref().map(|iam| iam.lock());
    if let Some(store) = held.as_deref() {
        let policies_arn = resolved.policies_arn.clone();
        caller.read_policies_of(store, &policies_arn);
    }
    // What the credential itself was scoped to, read at the instant of the request so a managed
    // scoping policy edited since the mint scopes by what it says NOW.
    caller.scope = sts_store::session_scoping_documents(held.as_deref(), &resolved.session_policies);
    caller
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller carrying exactly `documents` as the identity-based policies in force on it, and
    /// nothing else: no account credential, no boundary, no scope.
    fn principal(documents: &[&str]) -> Caller {
        let arn = "arn:aws:sts::000000000000:assumed-role/agent/session";
        Caller {
            arn: arn.to_string(),
            principals: vec![arn.to_string()],
            documents: documents.iter().map(|text| (*text).to_string()).collect(),
            ..Caller::default()
        }
    }

    /// An `Allow` of `actions` on every resource.
    fn allow(actions: &str) -> String {
        format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":{actions},"Resource":"*"}}]}}"#
        )
    }

    /// RULE: the action asked about carries this service's OWN namespace, so a document scoped
    /// to another service's actions states nothing about it.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        for operation in ["DeleteCacheSubnetGroup", "CreateReplicationGroup"] {
            let action = action_for(operation);
            assert_eq!(
                action,
                format!("elasticache:{operation}"),
                "case {operation:?}: expected this service's namespace, got {action:?}"
            );
        }
        let caller = principal(&[allow("\"ec2:*\"").as_str()]);
        let refused = caller.refusal(&action_for("DeleteCacheSubnetGroup"), EVERY_RESOURCE);
        assert!(
            refused.is_some(),
            "case \"another service's namespace\": expected no grant, got {refused:?}"
        );
    }

    /// RULE: a principal whose documents allow the action is served; one whose documents allow
    /// only a DIFFERENT action of this service is refused, and the sentence names IAM's rule.
    #[test]
    fn a_grant_is_read_for_the_action_the_request_performs() {
        let subnet_group = crate::state::subnet_group_arn("us-east-1", "vvd-cache-subnets");
        let action = action_for("DeleteCacheSubnetGroup");
        // (case, the document in force, whether it grants the delete)
        let rows: &[(&str, String, bool)] = &[
            (
                "no document at all",
                String::new(),
                false,
            ),
            (
                "only the creates this role was given",
                allow("[\"elasticache:CreateCacheSubnetGroup\",\"elasticache:Describe*\"]"),
                false,
            ),
            (
                "the delete itself",
                allow("\"elasticache:DeleteCacheSubnetGroup\""),
                true,
            ),
            ("the whole namespace", allow("\"elasticache:*\""), true),
        ];
        for (case, document, granted) in rows {
            let caller = if document.is_empty() {
                principal(&[])
            } else {
                principal(&[document.as_str()])
            };
            let refused = caller.refusal(&action, &subnet_group);
            assert_eq!(
                refused.is_none(),
                *granted,
                "case {case:?}: expected the delete to be {}, got {refused:?}",
                if *granted { "allowed" } else { "refused" }
            );
            if let Some(stated) = refused {
                assert_eq!(
                    stated,
                    format!(
                        "User: {} is not authorized to perform: {action} on resource: \
                         {subnet_group} because no identity-based policy allows the {action} \
                         action",
                        caller.arn
                    ),
                    "case {case:?}: expected IAM's wording, got {stated:?}"
                );
            }
        }
    }

    /// RULE: an explicit `Deny` wins over an `Allow` of everything, and the sentence names the
    /// document it stands in.
    #[test]
    fn an_explicit_deny_refuses_whatever_a_grant_says() {
        let document = r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Action":"elasticache:*","Resource":"*"},
            {"Effect":"Deny","Action":"elasticache:DeleteCacheSubnetGroup","Resource":"*"}]}"#;
        let caller = principal(&[document]);
        let action = action_for("DeleteCacheSubnetGroup");
        let stated = caller
            .refusal(&action, EVERY_RESOURCE)
            .expect("an explicit deny refuses");
        assert!(
            stated.ends_with(iam::IDENTITY_POLICY_EXPLICIT_DENY),
            "expected the refusal to name the identity-based deny, got {stated:?}"
        );
        // The same document says nothing about another action, which stays allowed.
        let allowed = caller.refusal(&action_for("CreateCacheSubnetGroup"), EVERY_RESOURCE);
        assert!(
            allowed.is_none(),
            "expected the deny to cover only its own action, got {allowed:?}"
        );
    }

    /// RULE: the resource a request is judged against is the one its own parameters address,
    /// read for the kind of resource the operation acts on.
    #[test]
    fn the_resource_is_the_one_the_request_addresses() {
        let region = "us-west-2";
        // (case, operation, the form-encoded body, the resource the decision is taken against)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "the subnet group a delete names",
                "DeleteCacheSubnetGroup",
                "CacheSubnetGroupName=vvd-subnets",
                crate::state::subnet_group_arn(region, "vvd-subnets"),
            ),
            (
                "the replication group a create names, beside the group it is placed in",
                "CreateReplicationGroup",
                "ReplicationGroupId=vvd-valkey&CacheSubnetGroupName=vvd-subnets",
                crate::state::replication_group_arn(region, "vvd-valkey"),
            ),
            (
                "the global group a delete names, not the replication group its name contains",
                "DeleteGlobalReplicationGroup",
                "GlobalReplicationGroupId=vvd-global",
                crate::state::global_replication_group_arn(region, "vvd-global"),
            ),
            (
                "the replication group an operation whose name spells no kind acts on",
                "IncreaseReplicaCount",
                "ReplicationGroupId=vvd-valkey",
                crate::state::replication_group_arn(region, "vvd-valkey"),
            ),
            (
                "the ARN a tagging call addresses",
                "AddTagsToResource",
                "ResourceName=arn%3Aaws%3Aelasticache%3Aus-west-2%3A000000000000%3Acluster%3A\
                 vvd-001",
                "arn:aws:elasticache:us-west-2:000000000000:cluster:vvd-001".to_string(),
            ),
            (
                "a catalog read that addresses no resource of its own",
                "DescribeCacheEngineVersions",
                "Engine=valkey",
                EVERY_RESOURCE.to_string(),
            ),
            (
                "a tagging call whose ResourceName names no resource of this service",
                "AddTagsToResource",
                "ResourceName=not-an-arn",
                EVERY_RESOURCE.to_string(),
            ),
        ];
        for (case, operation, body, expected) in rows {
            let request = RtRequest::try_from(
                ::http::Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(::aws_smithy_types::body::SdkBody::from(*body))
                    .expect("build the request"),
            )
            .expect("a runtime-api request");
            let addressed = addressed_resource(&request, operation, region);
            assert_eq!(
                &addressed, expected,
                "case {case:?}: expected the addressed resource, got {addressed:?}"
            );
        }
    }
}
