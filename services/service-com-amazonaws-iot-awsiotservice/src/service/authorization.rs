// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the
//! authorization step taken in front of every operation, before it reads or writes anything.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what a handler
//! would do, so a caller whose identity-based policies do not grant the action — or that an
//! explicit `Deny` covers — is told it is not authorized rather than told anything about the
//! resource it named, and the call performs nothing. The refusal is `AccessDeniedException` at
//! HTTP 403: it is the AUTHORIZATION layer's answer rather than one of this service's modelled
//! errors (no operation of this model declares an access-denied shape — `CreateThing` declares
//! `InvalidRequestException`, `ResourceAlreadyExistsException`, `ResourceNotFoundException`,
//! `ThrottlingException`, `UnauthorizedException`, `InternalFailureException` and
//! `ServiceUnavailableException`, `smithy_query(query="operation", operation="CreateThing")`),
//! which is why it is rendered here in this service's own `restJson1` error envelope rather
//! than returned by a handler. The modelled `UnauthorizedException` (HTTP 401) is this
//! service's own answer about a DEVICE's policies, not IAM's about the caller's.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for (`sts_store::resolve_caller`, the same record `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for the
//!    credentials it signed with), else the IAM user that owns the long-term key, else the
//!    account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives — so a document `iam:PutRolePolicy` wrote takes effect on
//!    the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation of
//!    those documents against `(action, resource)`, so this service and every other one handed
//!    the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow` about the
//!    request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the ARN readings do. The lookups
//! stay in this crate because hoisting them into `service-common` would make every service
//! that depends on that crate link the IAM and STS service crates.
//!
//! ## The action and the resource
//!
//! The action is `<signing name>:<OperationName>` — this service's `arnNamespace`, which is
//! also its `sigv4Name` (`smithy_query(query="service_info")`) — read off the request's own
//! resolved identity rather than restated here, so one statement scoped to this service's
//! actions covers every operation of it and nothing outside it.
//!
//! The resource is the one the REQUEST addresses ([`judged_resource`]): the ARN of this
//! service its input names at the member the model BINDS one to ([`RESOURCE_ARN_MEMBER`], the
//! `resourceArn` the tag calls carry — read only for the operations whose input declares it,
//! [`RESOURCE_ARN_OPERATIONS`]), else the ARN this service mints for the resource whose
//! IDENTIFIER the request line carries ([`path_addressed_arn`] — the `thingName` of
//! `/things/{thingName}`, the `policyName` of `/policies/{policyName}`), and [`ANY_RESOURCE`] —
//! the resource IAM writes for a request that addresses no resource of its own — for everything
//! else. Taking it from the request rather than from a stored record is what keeps the decision
//! independent of whether that resource exists, which is also what keeps a refused call from
//! telling an unauthorized caller whether it does.
//!
//! A request judged against [`ANY_RESOURCE`] names no resource of this service the decision can
//! be scoped to — a listing, an account-wide setting, an operation bound as an action under a
//! collection — so for those requests the GRANT question is the weaker one `iam_authorization`
//! states for a gate that does not know the resource ARN: whether any readable `Allow` covers
//! the action at all. Refusing them on the resource element instead would deny calls AWS
//! permits. The DENY question is always decided against the resource, so a `Deny` scoped to the
//! ARN a request does carry still refuses it.
//!
//! This service's own policy documents (`CreatePolicy`, the documents attached to a certificate
//! or a Cognito identity) are not part of this decision: those grant a DEVICE its MQTT
//! connections, publications and subscriptions ([`crate::state::policy_evaluation`]), while
//! every request this gate judges is an API call signed by a credential of this account, whose
//! grants are its identity's. So the decision here is the identity-based one alone.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS nor
//!   IAM holds a record for resolves to the account's own identity, which is not a principal
//!   the IAM store holds policies for — the position AWS account root credentials are in. Such
//!   a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary the store
//!   DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The resource IAM writes for a request that addresses no resource of this service.
pub const ANY_RESOURCE: &str = "*";

/// The wire name of the refusal this gate answers with, and the status an authorization
/// failure carries.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The media type this restJson1 service renders an error in.
const JSON_CONTENT_TYPE: &str = "application/json";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The input member this model binds an ARN of this service to.
///
/// Source: `smithy_query(query="operation", operation="TagResource,UntagResource,
/// ListTagsForResource")` — `resourceArn`, "The ARN of the resource", the one member of this
/// model's inputs that carries an ARN of this service; every other operation addresses its
/// resource by NAME in the request line (see the module docs for what that leaves the decision
/// resting on).
const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The operations whose input BINDS [`RESOURCE_ARN_MEMBER`], which are the only ones a value at
/// that name is read from.
///
/// Source: the same reading as above — the three tag operations are the whole of this model's
/// surface that names its resource by ARN. It matters that the list is closed: this gate runs
/// AHEAD of deserialization, so a member the input shape does not declare is still in the
/// document the codec will later discard, and reading one for another operation would let a
/// caller name the resource its own request is judged against while the handler acts on the one
/// the request line carries.
const RESOURCE_ARN_OPERATIONS: &[&str] = &["TagResource", "UntagResource", "ListTagsForResource"];

/// One refused request: the sentence IAM states the refusal with, and nothing else — the status
/// and the error shape are this module's constants.
#[derive(Debug, Clone)]
pub struct Denial {
    /// What the caller is told: who was refused, which action, on which resource when the
    /// request addressed one, and which of IAM's rules refused it.
    pub message: String,
}

/// This service's IAM action prefix: the signing name the request's own resolved identity
/// carries, which is this service's `arnNamespace` and `sigv4Name` alike.
#[must_use]
pub fn action_prefix<'ctx>(ctx: &'ctx ::smithy_server_runtime::OperationContext<'_>) -> &'ctx str {
    ctx.identity().signing_name()
}

/// The resource this request is judged against: the ARN of the resource it ADDRESSES, else
/// [`ANY_RESOURCE`].
///
/// Two ways a request names its resource, and this service's model uses both:
///
/// * the ARN itself, at the member the model BINDS one to ([`arn_in_input`]) — only for the
///   operations whose input declares it ([`RESOURCE_ARN_OPERATIONS`]), in the request DOCUMENT
///   for the two tag writes and as a query parameter for the tag read;
/// * the resource's IDENTIFIER as the first `@httpLabel` of the request line, which is how
///   every other operation addresses the resource it acts on
///   ([`path_addressed_arn`]) — the ARN is then the one this service itself mints for that
///   resource.
///
/// `vendor` is this service's ARN namespace, which an ARN the input carries must be of, and
/// `region` the one the request was signed for, which is the Region segment of an ARN this
/// service mints.
#[must_use]
pub fn judged_resource(
    vendor: &str,
    region: &str,
    operation: &str,
    req: &WireRequest,
) -> String {
    arn_in_input(
        operation,
        vendor,
        req.body().bytes().unwrap_or_default(),
        req.uri(),
    )
    .or_else(|| path_addressed_arn(region, operation, ::service_common::request_path(req.uri())))
    .unwrap_or_else(|| ANY_RESOURCE.to_string())
}

/// The ARN of `vendor` the input of `operation` NAMES at [`RESOURCE_ARN_MEMBER`], read from the
/// request document and then from the query string — the two bindings this model gives that
/// member.
///
/// `None` for an operation whose input does not declare it ([`RESOURCE_ARN_OPERATIONS`]): such a
/// request addresses its resource in the request line, and a value standing at that name in its
/// document is a member the codec discards rather than anything the operation acts on.
fn arn_in_input(operation: &str, vendor: &str, body: &[u8], uri: &str) -> Option<String> {
    if !RESOURCE_ARN_OPERATIONS.contains(&operation) {
        return None;
    }
    arn_in_document(vendor, body).or_else(|| arn_in_query(vendor, uri))
}

/// The ARN of the resource the request line addresses: the one this service mints for the
/// resource type the leading path segment names, over the identifier the next segment carries.
///
/// Source: each entry is one of this model's `@http` URIs — `/things/{thingName}`,
/// `/thing-groups/{thingGroupName}`, `/policies/{policyName}` and so on
/// (`smithy_query(query="operation", operation="DescribeThing,DescribeThingGroup,
/// DescribeThingType,GetPolicy,GetTopicRule,DescribeJob,DescribeAuthorizer,GetCommand,
/// GetPackage,GetOTAUpdate,DescribeCertificate,DescribeStream,DescribeRoleAlias,
/// DescribeFleetMetric,DescribeProvisioningTemplate,DescribeDomainConfiguration")`) — paired
/// with the builder this crate ALREADY has for that resource type's ARN, so no ARN is spelled
/// twice and the resource a decision is taken against is the one the service reports.
///
/// `None` for a request line this table does not place, which is judged against
/// [`ANY_RESOURCE`] (see the module docs): a collection no entry names, a path carrying no
/// identifier at all (`/things`, the listing), and the operations this model binds as an ACTION
/// under a collection rather than as a resource of it — `PUT /thing-groups/addThingToThingGroup`
/// — whose segment is the operation's own name rather than a resource's
/// ([`names_the_operation`]).
fn path_addressed_arn(region: &str, operation: &str, path: &str) -> Option<String> {
    let collection = ::service_common::nth_path_segment(path, 0)?;
    let addressed = ::service_common::nth_path_segment(path, 1)
        .filter(|segment| !segment.is_empty() && !names_the_operation(operation, segment))?;
    // The label as the codec that deserializes the request reads it, so an identifier an SDK
    // percent-encoded is the identifier the handler will act on.
    let identifier = ::service_common::percent_decode_path_label(addressed);
    Some(match collection {
        "things" => crate::state::thing_arn(region, &identifier),
        "thing-groups" => crate::state::thing_group_arn(region, &identifier),
        "thing-types" => crate::state::thing_type_arn(region, &identifier),
        "policies" => crate::state::policy_arn(region, &identifier),
        "rules" => crate::state::topic_rule_arn(region, &identifier),
        "jobs" => crate::state::job_arn(region, &identifier),
        "job-templates" => crate::state::job_template_arn(region, &identifier),
        "authorizer" => crate::state::authorizer_arn(region, &identifier),
        "commands" => crate::state::command_arn(region, &identifier),
        "packages" => crate::state::package_arn(region, &identifier),
        "otaUpdates" => crate::state::ota_update_arn(region, &identifier),
        "certificates" => crate::state::certificate_arn(region, &identifier),
        "streams" => crate::state::stream_arn(region, &identifier),
        "role-aliases" => crate::state::role_alias_arn(region, &identifier),
        "fleet-metric" => crate::state::fleet_metric_arn(region, &identifier),
        "provisioning-templates" => crate::state::provisioning_template_arn(region, &identifier),
        "domainConfigurations" => crate::state::domain_configuration_arn(region, &identifier),
        _ => return None,
    })
}

/// Whether `segment` is the OPERATION's own name rather than a resource's identifier.
///
/// A handful of this model's operations are bound as an action under a collection —
/// `PUT /thing-groups/addThingToThingGroup`, `PUT /billing-groups/addThingToBillingGroup`,
/// `PUT /thing-groups/updateThingGroupsForThing` — and each spells that segment as its own
/// operation name with the leading letter lower-cased, which is what tells such a segment from
/// the identifier a sibling URI binds in the same position. Those requests name their resource
/// in the request document instead, so they are judged against [`ANY_RESOURCE`].
fn names_the_operation(operation: &str, segment: &str) -> bool {
    segment.eq_ignore_ascii_case(operation)
}

/// The ARN of `vendor` the request body `body` names at [`RESOURCE_ARN_MEMBER`], or `None` for
/// a body that names none — which covers a body that is not a readable JSON object and one
/// whose bound ARN member carries something that is not an ARN of this service.
fn arn_in_document(vendor: &str, body: &[u8]) -> Option<String> {
    let document: ::serde_json::Value = ::serde_json::from_slice(body).ok()?;
    document
        .as_object()?
        .get(RESOURCE_ARN_MEMBER)
        .and_then(::serde_json::Value::as_str)
        .filter(|value| ::service_common::arn::is_resource_arn_of(value, vendor))
        .map(str::to_string)
}

/// The ARN of `vendor` the request's query string names at [`RESOURCE_ARN_MEMBER`], or `None`
/// for a request whose query names none.
///
/// The decoding is `form_urlencoded`'s, the one reader of a query string this tree uses, so a
/// percent-encoded ARN is read exactly as the framework's own codec reads it.
fn arn_in_query(vendor: &str, uri: &str) -> Option<String> {
    let query = uri.split_once('?').map(|(_path, query)| query)?;
    ::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _value)| name == RESOURCE_ARN_MEMBER)
        .map(|(_name, value)| value.into_owned())
        .filter(|value| ::service_common::arn::is_resource_arn_of(value, vendor))
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
/// whether or not anybody called, which is why a REPORTING operation of this service takes this
/// decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
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
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read refuses
    // the request whatever else the principal carries — including a grant this emulator cannot
    // read, which only lifts the DEFAULT deny of a request no readable statement allows (see
    // the module docs).
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
            // An explicit deny is a fact about the boundary document, so the refusal names it;
            // a boundary that merely states nothing withheld the grant the identity-based
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
/// [`ANY_RESOURCE`] — one whose input names no ARN of this service, and so may still be about a
/// resource a statement is scoped to (see the module docs).
///
/// A request that DOES name an ARN is decided against that ARN alone, where the resource
/// element is what AWS matches it on.
fn may_allow(documents: &[iam::PreparedDocument], action: &str, resource: &str) -> bool {
    resource == ANY_RESOURCE && iam::identity_may_allow_action(documents, action)
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it. Every value in it comes from the request
/// and from the stored policies — the principal from the credential the request presented, the
/// action from the operation it named, the resource from the ARN it addressed.
///
/// A request that addresses no resource of this service is judged against [`ANY_RESOURCE`], and
/// the sentence states no resource clause for it: there is nothing to name. Every other refusal
/// is the shared sentence, so this service and the others that reach the same decision word it
/// alike.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> Denial {
    let message = if resource == ANY_RESOURCE {
        format!("User: {principal} is not authorized to perform: {action} {reason}")
    } else {
        iam::not_authorized_by_identity_policy(principal, action, resource, reason)
    };
    Denial { message }
}

/// The wire answer to a request this gate refused: HTTP 403 `AccessDeniedException`, in the
/// envelope this restJson1 service renders every other pre-dispatch failure in — the `__type`
/// plus lowercase `message` body, and the `x-amzn-errortype` header restJson1 resolves an error
/// shape from.
#[must_use]
pub fn refusal_response(denial: &Denial) -> WireResponse {
    let body = ::serde_json::json!({
        "__type": ACCESS_DENIED_EXCEPTION,
        "message": denial.message,
    });
    let mut response = WireResponse::new(
        StatusCode::try_from(ACCESS_DENIED_STATUS).expect("the status is a constant of this module"),
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
    /// Set when the principal has a policy attached whose document the IAM store does not hold
    /// — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no readable
    /// document DENIES the action: the account's own credential, which holds no IAM policies, or
    /// a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's own
    /// accessors: the documents in force on it, whether it carries an attachment whose document
    /// this emulator cannot read, and the permissions boundary that caps it.
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
        // No STS store on this engine: the only identity a request can present is the account's
        // own published credential.
        None => Ok(sts_store::ambient_identity()),
    };
    let identity = match identity {
        Ok(identity) => identity,
        // The session credential has expired, so it speaks for nobody: no documents, no
        // principal spelling, and every action it asks for falls to the default deny. The key id
        // it presented is NOT quoted back — this module echoes no value of the request — and in
        // the assembled emulator the layer that authenticates a request refuses an expired
        // credential before any handler runs.
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
    // user that owns it (`access_keys`, the map IAM's own `GetUser` resolves a caller through),
    // so a request signed with one speaks for THAT user and is judged on the user's policies
    // rather than served as the account. A key the IAM store does not hold either really is the
    // account's own published credential.
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
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by the
        // ARN the account holds it under — which carries its PATH, where the session ARN does
        // not.
        if let Some(held) = store.roles.get(&iam_store::name_key(role_name)) {
            caller.principals.push(held.arn.clone());
            caller.read_policies_of(&store, &held.arn.clone());
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
    const VENDOR: &str = "iot";

    /// RULE: the refusal names the principal, the action, the resource the request addressed and
    /// which of IAM's rules refused — every part taken from the request and the stored policies
    /// — and it names no resource for a request that addresses none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = format!("arn:aws:iam::{}:user/probe", ::engine::emulator_account_id());
        let thing = format!(
            "arn:aws:iot:us-east-1:{}:thing/probed",
            ::engine::emulator_account_id()
        );
        // (case, the operation, the resource it addressed, the clause the sentence carries)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "an operation that addresses no resource of this service",
                "ListThings",
                ANY_RESOURCE,
                String::new(),
            ),
            (
                "an operation addressing a resource of this service by ARN",
                "ListTagsForResource",
                &thing,
                format!(" on resource: {thing}"),
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

    /// RULE: the resource a request is judged against is the ARN of this service its input
    /// names at the member the model BINDS one to — in the document or in the query string,
    /// which are the two bindings this model gives that member — read for the operations whose
    /// input DECLARES it and for no others, and the any-resource wildcard for a request that
    /// names none.
    #[test]
    fn the_judged_resource_is_the_arn_the_input_names() {
        let thing = format!(
            "arn:aws:iot:us-east-1:{}:thing/probed",
            ::engine::emulator_account_id()
        );
        // (case, the operation the request named, the request body, the resource it addresses)
        let documents: Vec<(&str, &str, String, String)> = vec![
            (
                "a tag write naming a thing",
                "TagResource",
                ::serde_json::json!({ "resourceArn": thing, "tags": [] }).to_string(),
                thing.clone(),
            ),
            (
                "the untag write, the other operation that binds the member",
                "UntagResource",
                ::serde_json::json!({ "resourceArn": thing, "tagKeys": ["env"] }).to_string(),
                thing.clone(),
            ),
            (
                // The gate runs ahead of deserialization, so the member is in the document the
                // codec will discard: it names nothing this operation acts on, and reading it
                // would let a caller choose the resource its own request is judged against.
                "a member the operation's input does not bind",
                "CreateThing",
                ::serde_json::json!({ "resourceArn": thing }).to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a request naming its thing by name",
                "CreateThing",
                ::serde_json::json!({ "thingTypeName": "sensor" }).to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a request naming another service's ARN",
                "TagResource",
                ::serde_json::json!({ "roleArn": "arn:aws:iam::000000000000:role/rules" })
                    .to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a request carrying no member at all",
                "TagResource",
                "{}".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a body that is not a JSON object",
                "TagResource",
                "[]".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a body that is not JSON",
                "TagResource",
                "not-json".to_string(),
                ANY_RESOURCE.to_string(),
            ),
        ];
        for (case, operation, body, expected) in documents {
            let actual = arn_in_input(operation, VENDOR, body.as_bytes(), "/tags")
                .unwrap_or_else(|| ANY_RESOURCE.to_string());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the judged resource {expected:?}, got {actual:?}"
            );
        }
        // (case, the operation the request named, the request URI, the resource it addresses)
        let queries: Vec<(&str, &str, String, String)> = vec![
            (
                "a tag read naming a thing, as an SDK client encodes the parameter",
                "ListTagsForResource",
                format!(
                    "/tags?{}",
                    ::form_urlencoded::Serializer::new(String::new())
                        .append_pair(RESOURCE_ARN_MEMBER, &thing)
                        .finish()
                ),
                thing.clone(),
            ),
            (
                "a tag read whose ARN is not encoded",
                "ListTagsForResource",
                format!("/tags?resourceArn={thing}"),
                thing.clone(),
            ),
            (
                "the same parameter on an operation that does not bind it",
                "ListThings",
                format!("/things?resourceArn={thing}"),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a listing naming its filters and no ARN",
                "ListTagsForResource",
                "/tags?nextToken=abc".to_string(),
                ANY_RESOURCE.to_string(),
            ),
            (
                "a request with no query at all",
                "ListTagsForResource",
                "/tags".to_string(),
                ANY_RESOURCE.to_string(),
            ),
        ];
        for (case, operation, uri, expected) in queries {
            let actual = arn_in_input(operation, VENDOR, b"", &uri)
                .unwrap_or_else(|| ANY_RESOURCE.to_string());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the judged resource {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a request that addresses its resource by IDENTIFIER in the request line is judged
    /// against the ARN this service mints for that resource, and a request line that addresses
    /// no resource of this service — a collection with no identifier, an operation bound as an
    /// action under a collection, a collection this service mints no ARN for — against the
    /// any-resource wildcard.
    ///
    /// The rows span the resource types the table places, one entry the issue did not cite
    /// among them, and each way a request line carries no identifier.
    #[test]
    fn the_judged_resource_is_the_arn_of_the_resource_the_request_line_addresses() {
        let region = "eu-west-1";
        let arn = |resource: &str| {
            format!(
                "arn:aws:iot:{region}:{}:{resource}",
                ::engine::emulator_account_id()
            )
        };
        // (case, the operation the request named, its path, the resource it addresses)
        let rows: Vec<(&str, &str, &str, String)> = vec![
            (
                "a thing addressed by name",
                "CreateThing",
                "/things/laser-1",
                arn("thing/laser-1"),
            ),
            (
                "a sub-resource of that thing",
                "ListThingPrincipals",
                "/things/laser-1/principals",
                arn("thing/laser-1"),
            ),
            (
                "a thing group",
                "DescribeThingGroup",
                "/thing-groups/factory",
                arn("thinggroup/factory"),
            ),
            (
                "a thing type",
                "DescribeThingType",
                "/thing-types/sensor",
                arn("thingtype/sensor"),
            ),
            (
                "a policy version of a policy",
                "CreatePolicyVersion",
                "/policies/device-policy/version",
                arn("policy/device-policy"),
            ),
            (
                "a topic rule",
                "GetTopicRule",
                "/rules/route_telemetry",
                arn("rule/route_telemetry"),
            ),
            (
                "a job",
                "DescribeJob",
                "/jobs/firmware-7",
                arn("job/firmware-7"),
            ),
            (
                "an authorizer",
                "DescribeAuthorizer",
                "/authorizer/token-checker",
                arn("authorizer/token-checker"),
            ),
            (
                "a role alias",
                "DescribeRoleAlias",
                "/role-aliases/credential-alias",
                arn("rolealias/credential-alias"),
            ),
            (
                "a stream",
                "DescribeStream",
                "/streams/firmware-stream",
                arn("stream/firmware-stream"),
            ),
            (
                "an identifier an SDK percent-encoded",
                "DescribeThing",
                "/things/site%3Alaser",
                arn("thing/site:laser"),
            ),
            (
                "a listing, which carries no identifier",
                "ListThings",
                "/things",
                ANY_RESOURCE.to_string(),
            ),
            (
                "a collection with an empty identifier",
                "ListThings",
                "/things/",
                ANY_RESOURCE.to_string(),
            ),
            (
                "an operation bound as an action under a collection",
                "AddThingToThingGroup",
                "/thing-groups/addThingToThingGroup",
                ANY_RESOURCE.to_string(),
            ),
            (
                "another such action, under another collection",
                "AddThingToBillingGroup",
                "/billing-groups/addThingToBillingGroup",
                ANY_RESOURCE.to_string(),
            ),
            (
                "a collection this service mints no ARN for",
                "DescribeAuditTask",
                "/audit/tasks/task-1",
                ANY_RESOURCE.to_string(),
            ),
            (
                "an account-wide setting, which addresses no resource",
                "DescribeAccountAuditConfiguration",
                "/audit/configuration",
                ANY_RESOURCE.to_string(),
            ),
        ];
        for (case, operation, path, expected) in rows {
            let actual = path_addressed_arn(region, operation, path)
                .unwrap_or_else(|| ANY_RESOURCE.to_string());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the judged resource {expected:?}, got {actual:?}"
            );
        }
    }
}
