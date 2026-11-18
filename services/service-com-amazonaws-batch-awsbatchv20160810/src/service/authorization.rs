// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it names — the decision
//! taken BEFORE the operation reads or writes anything, as AWS takes it.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what a
//! handler would do, so a caller whose identity-based policies do not grant the action — or
//! that an explicit `Deny` covers — is told it is not authorized rather than told anything
//! about the resource it named, and the call changes nothing: no job is submitted, no job is
//! terminated, nothing is minted. The refusal is `AccessDeniedException` at HTTP 403, the
//! AUTHORIZATION layer's own answer rather than one of this service's modelled errors (every
//! operation of this model declares exactly `ClientException` at 400 and `ServerException`
//! at 500), which is why it is rendered here in the same `restJson1` envelope rather than
//! returned by a handler.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_caller`, the same record
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one the
//!    caller reads back for the credentials it signed with), else the IAM user that owns the
//!    long-term key, else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives — so a document `iam:PutUserPolicy` wrote takes effect on
//!    the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the wording of the refusal
//! does. That is the split every other gate in this tree makes, and the lookups stay in this
//! crate for the reason those keep theirs there: hoisting them into `service-common` would
//! make every service that depends on that crate link the IAM and STS service crates.
//!
//! ## The action and the resource
//!
//! The action is `<signing name>:<OperationName>` — this service's `arnNamespace`, which is
//! also its `sigv4Name` — read off the request's own resolved identity rather than restated
//! here, so one statement scoped to this service's actions covers every operation of it and
//! nothing outside it.
//!
//! The resource is the one the REQUEST addresses ([`judged_resource`]): the ARN of this
//! service it names — the whole ARN the tagging surface binds as its one path label
//! (`/v1/tags/{resourceArn}`), else the ARN of what an input member the model binds a
//! resource identifier to names ([`ARN_MEMBERS`]), built by this service's OWN ARN builders
//! for a member that gives a NAME (every such member documents "the name or the ARN", and
//! none of this service's ARNs carries a minted suffix, so the ARN a name resolves to is
//! computable from the request) — and [`ANY_RESOURCE`], the resource IAM writes for a
//! request that addresses no single resource of its own, for everything else. Taking it from
//! the request rather than from a stored record is what keeps the decision independent of
//! whether that resource exists, which is also what keeps a refused call from telling an
//! unauthorized caller whether it does.
//!
//! A request that addresses no single resource — a collection read, a create whose input
//! names only the resource it is about to make — is judged against [`ANY_RESOURCE`], and for
//! those the GRANT question is the weaker one `iam_authorization` states for a gate that does
//! not know the resource ARN: whether any readable `Allow` covers the action at all.
//! Refusing them on the resource element instead would deny calls AWS permits. The DENY
//! question is always decided against the resource, so a `Deny` scoped to the ARN a request
//! does address still refuses it.
//!
//! This service publishes no resource-based policy of its own — the model declares no policy
//! operation and no policy member — so the decision here is the identity-based one alone.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary the
//!   store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no user can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::OperationContext;

/// The resource IAM writes for a request that addresses no resource of this service.
pub const ANY_RESOURCE: &str = "*";

/// The wire name of the refusal this gate answers with, and the status an authorization
/// failure carries.
pub const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The media type this restJson1 service renders an error in.
const JSON_CONTENT_TYPE: &str = "application/json";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The path the tagging surface is bound under, whose one label is the whole ARN of the
/// resource the call addresses (`/v1/tags/{resourceArn}` —
/// `smithy_query(query="operation",operation="TagResource")`).
const TAGS_PATH_PREFIX: &str = "/v1/tags/";

/// How the ARN of the resource an input member names is built.
///
/// Every member below documents "the name or the ARN" of what it addresses, so a value in
/// the ARN form IS the resource and a name is resolved through the builder this service
/// mints that family's ARNs with ([`crate::state::resolved_arn`]) — the same builder the
/// create of that resource uses, which is what makes the gate's resource the ARN the
/// operation would act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    /// One job queue (`crate::state::job_queue_arn`).
    JobQueue,
    /// One compute environment (`crate::state::compute_environment_arn`).
    ComputeEnvironment,
    /// One service environment (`crate::state::service_environment_arn`).
    ServiceEnvironment,
    /// One consumable resource (`crate::state::consumable_resource_arn`).
    ConsumableResource,
    /// One scheduling policy (`crate::state::scheduling_policy_arn`).
    SchedulingPolicy,
    /// One job, addressed by the id this service minted for it: a SERVICE job's ARN on the
    /// operations of the service-job plane ([`SERVICE_JOB_OPERATIONS`],
    /// `crate::state::service_job_arn`) and a container job's on every other
    /// (`crate::state::job_arn`).
    JobId,
    /// A member that only ever carries an ARN — a quota share's own ARN, and a job
    /// definition reference, whose ARN carries the revision a bare name does not name — so
    /// a value that is not one addresses no single resource.
    ArnOnly,
}

/// The input members this model binds a resource identifier to, in the order a request is
/// read for one, each with how the ARN of what it names is built.
///
/// Source: this model's own inputs (`smithy_query(query="operation",operation="…")`) —
/// `jobQueue` (the submits, the snapshot, a quota share's parent queue), `jobId` (the job a
/// cancel, a termination, an update or a read of one job addresses), `jobDefinition` (a
/// submit's definition), `computeEnvironment` and `serviceEnvironment` (the environment a
/// mutation addresses), `schedulingPolicyArn` and `arn` (the policy a queue is created with,
/// and the policy mutations' own identifier), `quotaShareArn` (one quota share) and
/// `consumableResource` (one consumable resource). A list-valued member addresses several
/// resources rather than one and is not read here.
///
/// Only a member the model BINDS is read: a `restJson1` request may carry members the input
/// shape does not declare, which the codec discards, so reading an identifier out of one
/// would judge the request against a resource the operation never acts on.
const ARN_MEMBERS: &[(&str, Addressed)] = &[
    ("jobQueue", Addressed::JobQueue),
    ("jobId", Addressed::JobId),
    ("jobDefinition", Addressed::ArnOnly),
    ("computeEnvironment", Addressed::ComputeEnvironment),
    ("serviceEnvironment", Addressed::ServiceEnvironment),
    ("schedulingPolicyArn", Addressed::SchedulingPolicy),
    ("arn", Addressed::SchedulingPolicy),
    ("quotaShareArn", Addressed::ArnOnly),
    ("consumableResource", Addressed::ConsumableResource),
];

/// The operations whose `jobId` names a SERVICE job rather than a container job.
///
/// Source: the inputs that carry a `jobId` in this model's service-job plane
/// (`smithy_query(query="operation",operation="DescribeServiceJob,TerminateServiceJob,UpdateServiceJob")`).
const SERVICE_JOB_OPERATIONS: &[&str] = &[
    "DescribeServiceJob",
    "TerminateServiceJob",
    "UpdateServiceJob",
];

/// One refused request: the sentence IAM states the refusal with, and nothing else — the
/// status and the error shape are this module's constants.
#[derive(Debug, Clone)]
pub struct Denial {
    /// What the caller is told: who was refused, which action, on which resource when the
    /// request addressed one, and which of IAM's rules refused it.
    pub message: String,
}

/// This service's IAM action prefix: the signing name the request's own resolved identity
/// carries, which is this service's `arnNamespace` and `sigv4Name` alike.
#[must_use]
pub fn action_prefix<'ctx>(ctx: &'ctx OperationContext<'_>) -> &'ctx str {
    ctx.identity().signing_name()
}

/// The resource this request is judged against: the ARN of what it addresses, else
/// [`ANY_RESOURCE`].
///
/// `operation` is the operation's own name, which is what decides which plane's job a
/// `jobId` names ([`SERVICE_JOB_OPERATIONS`]).
#[must_use]
pub fn judged_resource(ctx: &OperationContext<'_>, operation: &str, req: &RtRequest) -> String {
    addressed_arn(ctx, operation, req).unwrap_or_else(|| ANY_RESOURCE.to_string())
}

/// The ARN of the resource this request addresses, or `None` for one that addresses none.
///
/// The tagging surface carries it as its one path label, decoded the way the codec that
/// deserializes the request decodes it ([`::service_common::percent_decode_path_label`]), so
/// the gate reads the same ARN the handler reads; every other operation of this model carries
/// its identifiers in the body.
fn addressed_arn(
    ctx: &OperationContext<'_>,
    operation: &str,
    req: &RtRequest,
) -> Option<String> {
    addressed_arn_of(
        action_prefix(ctx),
        &crate::state::request_region(ctx),
        operation,
        ::service_common::request_path(req.uri()),
        req.body().bytes().unwrap_or_default(),
    )
}

/// The ARN of `vendor` in `region` a request to `operation` addresses, read off its `path`
/// and its `body` alone.
///
/// Split from [`addressed_arn`] so the rule is decided from values rather than from a live
/// request: the request is where the vendor, the Region, the path and the body come from, and
/// nothing else about it matters here.
fn addressed_arn_of(
    vendor: &str,
    region: &str,
    operation: &str,
    path: &str,
    body: &[u8],
) -> Option<String> {
    if let Some(label) = path.strip_prefix(TAGS_PATH_PREFIX) {
        let arn = ::service_common::percent_decode_path_label(label);
        return ::service_common::arn::is_resource_arn_of(&arn, vendor).then_some(arn);
    }
    let document: ::serde_json::Value = ::serde_json::from_slice(body).ok()?;
    let members = document.as_object()?;
    ARN_MEMBERS.iter().find_map(|(member, addressed)| {
        let value = members.get(*member).and_then(::serde_json::Value::as_str)?;
        if ::service_common::arn::is_resource_arn_of(value, vendor) {
            return Some(value.to_string());
        }
        let minted = match addressed {
            Addressed::ArnOnly => return None,
            Addressed::JobQueue => crate::state::job_queue_arn(region, value),
            Addressed::ComputeEnvironment => crate::state::compute_environment_arn(region, value),
            Addressed::ServiceEnvironment => crate::state::service_environment_arn(region, value),
            Addressed::ConsumableResource => crate::state::consumable_resource_arn(region, value),
            Addressed::SchedulingPolicy => crate::state::scheduling_policy_arn(region, value),
            Addressed::JobId if SERVICE_JOB_OPERATIONS.contains(&operation) => {
                crate::state::service_job_arn(region, value)
            }
            Addressed::JobId => crate::state::job_arn(region, value),
        };
        Some(minted)
    })
}

/// Authorize this request, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]) and `resource` the ARN the request ADDRESSES
/// ([`judged_resource`]).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see the
/// module docs); the refusal otherwise. Taken BEFORE the operation runs, so a refused request
/// reads and writes nothing.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service takes
/// this decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), Denial> {
    let action = format!("{}:{operation}", action_prefix(ctx));
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no such
    // element, so the decision below rests on the action and the resource.
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
        || caller.identity_is_unbounded();
    if !granted {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            &iam::no_identity_policy_allows(&action),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let boundary = iam::prepare_documents(::std::slice::from_ref(boundary));
        let reason = match iam::identity_decision_prepared(&boundary, &request) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the identity-based
            // policies gave.
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
/// [`ANY_RESOURCE`] — one whose input names no ARN of this service, and so may still be about
/// a resource a statement is scoped to (see the module docs).
///
/// A request that DOES name an ARN is decided against that ARN alone, where the resource
/// element is what AWS matches it on.
fn may_allow(documents: &[iam::PreparedDocument], action: &str, resource: &str) -> bool {
    resource == ANY_RESOURCE && iam::identity_may_allow_action(documents, action)
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it — the one sentence every authorizing
/// service in this tree words it as ([`iam::not_authorized_by_identity_policy`]). Every value
/// in it comes from the request and from the stored policies.
///
/// A request that addresses no resource of this service is judged against [`ANY_RESOURCE`],
/// and the sentence states no resource clause for it: there is nothing to name.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> Denial {
    let message = if resource == ANY_RESOURCE {
        format!("User: {principal} is not authorized to perform: {action} {reason}")
    } else {
        iam::not_authorized_by_identity_policy(principal, action, resource, reason)
    };
    Denial { message }
}

/// The wire answer to a request this gate refused: HTTP 403 `AccessDeniedException`, in the
/// envelope this restJson1 service renders an error document in — the `__type` plus lowercase
/// `message` body, and the `x-amzn-errortype` header restJson1 resolves an error shape from.
#[must_use]
pub fn refusal_response(denial: &Denial) -> RtResponse {
    let body = ::serde_json::json!({
        "__type": ACCESS_DENIED_EXCEPTION,
        "message": denial.message,
    });
    let mut response = RtResponse::new(
        StatusCode::try_from(ACCESS_DENIED_STATUS)
            .expect("the status is a constant of this module"),
        SdkBody::from(body.to_string()),
    );
    response
        .headers_mut()
        .insert("content-type", JSON_CONTENT_TYPE);
    response
        .headers_mut()
        .insert("x-amzn-errortype", ACCESS_DENIED_EXCEPTION);
    response
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the IAM
    /// role behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose document
    /// this emulator holds.
    boundary: Option<String>,
    /// Set when this request presents the account's own published credential, which holds no
    /// IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not
    /// hold — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no IAM
    /// policies, or a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's own
    /// accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State,
        identity_arn: &str,
    ) {
        if let Some(policies) = store.principal_policies(identity_arn) {
            self.documents = policies.documents;
            // An attachment the collection resolved no document for is an entry of AWS's own
            // catalog this emulator does not carry: a grant it cannot read.
            self.unreadable_attachment = !policies.uncarried.is_empty();
        }
        self.boundary = store.permissions_boundary_document(identity_arn);
    }
}

/// The principal this request speaks for, with the identity-based policies in force on it.
fn caller<Eng: Send + Sync + 'static>(engine: &Eng, ctx: &OperationContext<'_>) -> Caller {
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
        // principal spelling, and every action it asks for falls to the default deny. The key
        // id it presented is NOT quoted back — this module echoes no value of the request —
        // and in the assembled emulator the layer that authenticates a request refuses an
        // expired credential before any handler runs.
        Err(_expired) => {
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
    // minted is the account's ambient identity — but IAM's store maps an access key id to the
    // user that owns it (`access_keys`, the map IAM's own `GetUser` resolves a caller
    // through), so a request signed with one speaks for THAT user and is judged on the user's
    // policies rather than served as the account. A key the IAM store does not hold either
    // really is the account's own published credential.
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
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by
        // the ARN the account holds it under — which carries its PATH, where the session ARN
        // does not.
        if let Some(held) = store.roles.get(&iam_store::name_key(role_name)) {
            let role_arn = held.arn.clone();
            caller.principals.push(role_arn.clone());
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

    /// This service's ARN namespace, which is the vendor every ARN it mints carries.
    const VENDOR: &str = "batch";

    /// RULE: the refusal names the principal, the action, the resource the request addressed
    /// and which of IAM's rules refused — every part taken from the request and the stored
    /// policies — and it names no resource for a request that addresses none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = format!("arn:aws:iam::{}:user/probe", ::engine::emulator_account_id());
        let queue = format!(
            "arn:aws:batch:us-east-1:{}:job-queue/probed",
            ::engine::emulator_account_id()
        );
        // (case, the operation, the resource it addressed, the clause the sentence carries)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "an operation that addresses no resource of this service",
                "TerminateServiceJob",
                ANY_RESOURCE,
                String::new(),
            ),
            (
                "an operation addressing a queue of this service",
                "SubmitServiceJob",
                &queue,
                format!(" on resource: {queue}"),
            ),
        ];
        for (case, operation, resource, clause) in rows {
            let action = format!("{VENDOR}:{operation}");
            for reason in [
                iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
                iam::no_identity_policy_allows(&action),
            ] {
                let refused = not_authorized(&principal, &action, resource, &reason);
                assert_eq!(
                    refused.message,
                    format!(
                        "User: {principal} is not authorized to perform: {action}{clause} {reason}"
                    ),
                    "case {case:?}: expected IAM's wording, got {:?}",
                    refused.message
                );
            }
        }
    }

    /// RULE: the resource a request is judged against is the ARN of the resource it
    /// addresses — the tagging surface's path label, the ARN an input member carries, or the
    /// ARN this service's own builders give the NAME or job id such a member names — and the
    /// any-resource wildcard for a request that addresses none, including one carrying an ARN
    /// at a member the input shape does not declare.
    #[test]
    fn the_judged_resource_is_the_arn_the_request_addresses() {
        let account = ::engine::emulator_account_id();
        let region = "us-east-1";
        let queue = format!("arn:aws:batch:{region}:{account}:job-queue/probed");
        let definition = format!("arn:aws:batch:{region}:{account}:job-definition/probed:1");
        let share = format!("arn:aws:batch:{region}:{account}:job-queue/probed/quota-share/qs");
        let job_id = "7f1d2c3b-4a59-4e6f-8a0b-1c2d3e4f5a6b";
        // Another queue of this account, which no row's request addresses: a decoy an
        // unmodelled member carries.
        let elsewhere = format!("arn:aws:batch:{region}:{account}:job-queue/elsewhere");
        // (case, the operation, the path, the body, the resource it addresses)
        let rows: Vec<(&str, &str, String, String, String)> = vec![
            (
                "a submit naming its queue by ARN",
                "SubmitServiceJob",
                "/v1/submitservicejob".to_string(),
                ::serde_json::json!({"jobQueue": queue, "jobName": "j"}).to_string(),
                queue.clone(),
            ),
            (
                "a submit naming its queue by name",
                "SubmitServiceJob",
                "/v1/submitservicejob".to_string(),
                ::serde_json::json!({"jobQueue": "probed", "jobName": "j"}).to_string(),
                queue.clone(),
            ),
            (
                "a submit naming only its definition by ARN",
                "SubmitJob",
                "/v1/submitjob".to_string(),
                ::serde_json::json!({"jobDefinition": definition}).to_string(),
                definition.clone(),
            ),
            (
                "a quota share addressed by ARN",
                "DescribeQuotaShare",
                "/v1/describequotashare".to_string(),
                ::serde_json::json!({"quotaShareArn": share}).to_string(),
                share.clone(),
            ),
            (
                "a service-job termination, addressing its job by id",
                "TerminateServiceJob",
                "/v1/terminateservicejob".to_string(),
                ::serde_json::json!({"jobId": job_id, "reason": "r"}).to_string(),
                crate::state::service_job_arn(region, job_id),
            ),
            (
                "a container-job termination, addressing its job by id",
                "TerminateJob",
                "/v1/terminatejob".to_string(),
                ::serde_json::json!({"jobId": job_id, "reason": "r"}).to_string(),
                crate::state::job_arn(region, job_id),
            ),
            (
                "a compute environment addressed by name",
                "UpdateComputeEnvironment",
                "/v1/updatecomputeenvironment".to_string(),
                ::serde_json::json!({"computeEnvironment": "ce-probed"}).to_string(),
                crate::state::compute_environment_arn(region, "ce-probed"),
            ),
            (
                "a scheduling policy addressed by its own member",
                "DeleteSchedulingPolicy",
                "/v1/deleteschedulingpolicy".to_string(),
                ::serde_json::json!({"arn": "sp-probed"}).to_string(),
                crate::state::scheduling_policy_arn(region, "sp-probed"),
            ),
            (
                "a tagging read, whose label is the whole ARN",
                "ListTagsForResource",
                format!("/v1/tags/{}", queue.replace(':', "%3A").replace('/', "%2F")),
                String::new(),
                queue.clone(),
            ),
            (
                "a tagging read of another vendor's ARN",
                "ListTagsForResource",
                "/v1/tags/arn%3Aaws%3Asqs%3Aus-east-1%3A000000000000%3Aq".to_string(),
                String::new(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "an unmodelled member carrying an ARN of this service",
                "ListJobs",
                "/v1/listjobs".to_string(),
                ::serde_json::json!({"AAA": elsewhere}).to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "an unmodelled member beside the bound one",
                "ListJobs",
                "/v1/listjobs".to_string(),
                ::serde_json::json!({"AAA": elsewhere, "jobQueue": queue}).to_string(),
                queue.clone(),
            ),
            (
                "a request carrying no member at all",
                "DescribeJobQueues",
                "/v1/describejobqueues".to_string(),
                "{}".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a request naming only a collection",
                "DescribeJobQueues",
                "/v1/describejobqueues".to_string(),
                ::serde_json::json!({"jobQueues": [queue]}).to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a body that is not a JSON object",
                "DescribeJobQueues",
                "/v1/describejobqueues".to_string(),
                "[]".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a body that is not JSON",
                "DescribeJobQueues",
                "/v1/describejobqueues".to_string(),
                "not-json".to_string(),
                ANY_RESOURCE.to_string(),
            ),
        ];
        for (case, operation, path, body, expected) in rows {
            let actual = addressed_arn_of(VENDOR, region, operation, &path, body.as_bytes())
                .unwrap_or_else(|| ANY_RESOURCE.to_string());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the judged resource {expected:?}, got {actual:?}"
            );
        }
    }
}
