// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLER of a request to this service perform the action it asks for —
//! the decision taken in front of every operation, BEFORE anything is read or written.
//!
//! Every request to an AWS service is signed, and the credential it is signed with speaks for
//! a principal: an IAM user's long-term access key, or a temporary session the token service
//! minted for a ROLE — which is what an SDK call made from inside a compute environment
//! running under an execution role presents. AWS decides such a request at the IAM layer,
//! ahead of the service looking at what the request names: a principal whose policies do not
//! allow the action is told it is not authorized and nothing about the resource, and the call
//! has NO effect — a refused send delivers no message and a refused create stores nothing.
//! The answer is HTTP 403 under the error code `AccessDeniedException`, which is NOT one of
//! this model's own errors (`smithy_query(query="service_info")` lists `BadRequestException`
//! and `TooManyRequestsException` as its service-common ones): the refusal is the IAM layer's
//! rather than the service's, which is why a client reads a code its model does not declare.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for ([`sts_store::resolve_caller`], the same place `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for
//!    the credentials it signed with), else the IAM user that holds the long-term key, else
//!    the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts it
//!    again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation
//!    of those documents against `(action, resource)`, and the SENTENCE a refusal carries,
//!    which is that module's `not_authorized_by_identity_policy` and the reason clauses
//!    beside it, so nothing of IAM's wording is spelled here.
//!
//! Only the store LOOKUPS live here; neither the collection nor the decision does. The
//! lookups stay in this service crate because the IAM service crate depends on
//! `service-common`, so the shared crate cannot read IAM's store without a dependency cycle.
//!
//! ## The action and the resource
//!
//! The action is `ses:<OperationName>` ([`action_for`]): `ses` is this service's
//! `arnNamespace` and its `sigv4Name` alike (`smithy_query(query="service_info")`), which is
//! the namespace its actions are published under and the one
//! [`crate::state::ARN_NAMESPACE`] spells into every ARN this service mints.
//!
//! The resource is the one the request ACTS ON ([`addressed_resource`]): this model binds its
//! operations under `/v2/email/<family>/<id>/…`, so a request that names one resource of a
//! family this service mints ARNs for is decided against THAT resource's ARN, built from the
//! identifier the request carried and the Region the caller signed for rather than resolved
//! out of the store — the decision is then the same whether or not the resource exists here,
//! and a refusal tells an unauthorized caller nothing about what the account holds. A CREATE
//! carries that identifier in its INPUT rather than in a URI label, since the resource it
//! acts on is the one it is about to create ([`RESOURCE_FAMILIES`] names the member each
//! family's create spells it in), so it is judged against that resource's own ARN and not
//! against the collection it will join. A request that acts on no single resource of its own
//! — an account-level read, a send, a listing, a call whose target is an ARN in its BODY — is
//! decided on the weaker question the shared evaluator publishes for exactly that case
//! ([`iam::identity_may_allow_action`]): whether any readable document could allow the action
//! at all. KNOWN BOUND: such a request is therefore granted by a statement scoped to one
//! resource, which is deliberate — refusing it would deny calls AWS permits, and nothing in
//! the URI says which resource it addressed.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to [`sts_store::ambient_identity`], which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed, which is also what keeps every in-process call
//!   another emulated service makes with the credential the framework hands its handlers
//!   served.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary the
//!   store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no document, so every action
//! it asks for is refused, and the refusal names that state rather than the key it presented,
//! which this module never quotes back.

use ::std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::OperationContext;

/// The action namespace this service's operations are published under, which is its
/// `arnNamespace` and its SigV4 signing name alike: a policy statement spells one of them
/// `ses:<OperationName>`.
pub const ACTION_PREFIX: &str = crate::state::ARN_NAMESPACE;

/// The resource an action that addresses no single resource is decided against: every
/// resource, which is what a statement has to cover to permit such a call.
pub const EVERY_RESOURCE: &str = "*";

/// The error code the IAM layer refuses an unauthorized request under, and the status it
/// answers.
const ACCESS_DENIED_CODE: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The media type this restJson1 service renders a body in, which the refusal below carries
/// like every other error document of this service.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The header an AWS answer names the error code of a refusal in.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The path segments this model binds every operation under
/// (`smithy_query(query="operation")`: `/v2/email/…`), so the segment that names a resource
/// FAMILY is the one after them.
const MODELLED_PATH_PREFIX: [&str; 2] = ["v2", "email"];

/// Which resource one of this model's URI families addresses, as `(the path segment the
/// family is bound under, the resource type that resource's ARN carries, the input member a
/// create over that collection names the resource by)`.
///
/// The first column is the segment after [`MODELLED_PATH_PREFIX`] in this model's own `@http`
/// URIs (`smithy_query(query="operation")`); the second is the resource form SES writes that
/// kind in the resource field of its ARN, which is stated for this service in exactly one
/// place — the resolution of a caller-supplied `ResourceArn`
/// (`crate::state::…::resource_tags`: `identity/<identity>`, `contact-list/<name>`,
/// `configuration-set/<name>`, `dedicated-ip-pool/<name>`) and
/// [`crate::state::TENANT_RESOURCE_TYPES`] (`template/<name>`). The third is the required
/// member the create bound to that collection takes the new resource's name in
/// (`smithy_query(query="operation")`: `CreateEmailIdentity$EmailIdentity`,
/// `CreateConfigurationSet$ConfigurationSetName`, `CreateContactList$ContactListName`,
/// `CreateDedicatedIpPool$PoolName`, `CreateEmailTemplate$TemplateName`). A family this
/// service mints no ARN for is not listed: a request addressing one is decided as a request
/// that names no single resource (see the module header), never against an ARN form this
/// service does not publish.
const RESOURCE_FAMILIES: [(&str, &str, &str); 5] = [
    ("identities", "identity", "EmailIdentity"),
    (
        "configuration-sets",
        "configuration-set",
        "ConfigurationSetName",
    ),
    ("contact-lists", "contact-list", "ContactListName"),
    ("dedicated-ip-pools", "dedicated-ip-pool", "PoolName"),
    ("templates", "template", "TemplateName"),
];

/// The IAM action `operation` is performed under: `ses:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The ARN the request to `uri` with the input `body` ACTS ON, by the ARN forms this service
/// publishes, and [`EVERY_RESOURCE`] for a request that acts on no single resource of its
/// own.
///
/// Read off the REQUEST: this model binds a resource-scoped operation under
/// `/v2/email/<family>/<id>…`, with the id in a `@httpLabel`, so the family picks the
/// resource type ([`RESOURCE_FAMILIES`]) and the segment after it is the id — percent-decoded
/// the way the codec that deserializes the request decodes a path label, so an identity whose
/// `@` or `+` a client encoded names the same resource a policy names. A request bound to the
/// COLLECTION of such a family carries the identifier in its input instead when it is the
/// create of that family, which is about to make exactly one resource: the name that create's
/// own required member states is the resource it acts on, so the decision is made against
/// that resource's ARN. Everything else — a family this service mints no ARN for, an
/// account-level path, a listing over a collection, a call whose target is an ARN in its body
/// — acts on no single resource.
#[must_use]
pub fn addressed_resource(uri: &str, region: &str, body: &[u8]) -> String {
    let path = ::service_common::uri_path(uri);
    for (index, expected) in MODELLED_PATH_PREFIX.iter().enumerate() {
        if ::service_common::nth_path_segment(path, index) != Some(*expected) {
            return EVERY_RESOURCE.to_string();
        }
    }
    let family = ::service_common::nth_path_segment(path, MODELLED_PATH_PREFIX.len());
    let Some((_family, resource_type, name_member)) = RESOURCE_FAMILIES
        .iter()
        .find(|(bound, _, _)| Some(*bound) == family)
    else {
        return EVERY_RESOURCE.to_string();
    };
    let id = ::service_common::nth_path_segment(path, MODELLED_PATH_PREFIX.len() + 1)
        .map(::service_common::percent_decode_path_label)
        .filter(|id| !id.is_empty())
        .or_else(|| named_in_input(body, name_member));
    match id {
        Some(id) => ::emulator_server::emulator_arn(
            crate::state::ARN_NAMESPACE,
            region,
            &format!("{resource_type}/{id}"),
        ),
        None => EVERY_RESOURCE.to_string(),
    }
}

/// The name the request's input states in `member`, for a create that names the resource it
/// is about to make, or `None` when the body carries no such text.
///
/// Only that one member is read: the decision rests on the action and the resource, and
/// nothing else of the input takes part. A body no reader can make a JSON object of — an
/// empty one, a listing's own input, a document the framework's walk goes on to refuse — names
/// nothing.
fn named_in_input(body: &[u8], member: &str) -> Option<String> {
    let ::serde_json::Value::Object(input) = ::serde_json::from_slice(body).ok()? else {
        return None;
    };
    input
        .get(member)?
        .as_str()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// The refusal a request to `operation` on `resource` gets when the documents in force do not
/// let the caller perform it — the sentence IAM states such a refusal with — and `None` when
/// they do.
///
/// `operation` is the operation's own name ([`smithy_server_runtime::Operation::NAME`], which
/// is the routing key this service's protocol resolves) and `resource` the ARN the request
/// ACTS ON ([`addressed_resource`]).
///
/// Asked BEFORE this service's store is read or written, and with an ARN built from what the
/// request carried rather than from a stored record, so the decision does not depend on
/// whether that resource exists.
///
/// Resolving the caller goes through STS's own resolution, which first materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service may take
/// this decision too.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    caller(engine, ctx).refusal(&action_for(operation), resource)
}

/// The wire answer to a request this gate refused: HTTP 403 under the error code
/// `AccessDeniedException`, carried where a restJson1 client reads a refusal's code and
/// message — the `x-amzn-errortype` header and the body's `message` member — so an SDK raises
/// it exactly as it raises the IAM layer's own refusal.
///
/// Hand-rendered rather than serialized through a modelled error shape, because this refusal
/// is NOT one of this model's errors (see the module header): the IAM layer answers it in
/// front of the service.
#[must_use]
pub fn refusal_response(message: &str) -> RtResponse {
    let body = ::serde_json::json!({ "message": message });
    let mut response = RtResponse::new(
        StatusCode::try_from(ACCESS_DENIED_STATUS).expect("the status is a constant of this module"),
        SdkBody::from(body.to_string()),
    );
    response
        .headers_mut()
        .insert("content-type", JSON_CONTENT_TYPE);
    response
        .headers_mut()
        .insert(ERROR_TYPE_HEADER, ACCESS_DENIED_CODE);
    response
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
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
    /// IAM policies (see the module header).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not
    /// hold — an AWS-managed policy (see the module header).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no IAM
    /// policies, or a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// `Some(message)` when the documents in force do not let this caller perform `action` on
    /// `resource` — the sentence the refusal carries — and `None` when they do.
    ///
    /// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
    /// whatever else the principal carries; otherwise a grant is needed, which a caller this
    /// gate does not bound has by definition (see the module header); and a grant is capped by
    /// the permissions boundary, since "a permissions boundary sets the maximum permissions
    /// that an identity-based policy can grant" (IAM, permissions boundaries), which takes
    /// permissions away and grants none of its own.
    ///
    /// A request that addresses no resource of its own is decided on the shared evaluator's
    /// weaker question ([`iam::identity_may_allow_action`]), which is published for exactly
    /// that case: refusing on the decision over [`EVERY_RESOURCE`] alone would refuse calls a
    /// resource-scoped statement permits.
    fn refusal(&self, action: &str, resource: &str) -> Option<String> {
        // An identity-based document states no `Principal` element; the spellings travel with
        // the request because that is what one would be matched against, so the decision below
        // rests on the action and the resource.
        let request = iam::Request::new(action, resource, &self.principals);
        let said = iam::identity_decision(&self.documents, &request);
        let granted = if resource == EVERY_RESOURCE {
            said != Decision::Denied
                && (iam::identity_may_allow_action(&iam::prepare_documents(&self.documents), action)
                    || self.identity_is_unbounded())
        } else {
            iam::is_allowed(said, Decision::NotStated)
                || (said != Decision::Denied && self.identity_is_unbounded())
        };
        // The ceiling in force on the principal. A call a document explicitly DENIES is already
        // refused on that deny, so no ceiling is read for it.
        let capped = match (said, self.boundary.as_ref()) {
            (Decision::Denied, _) | (_, None) => Decision::Allowed,
            (_, Some(boundary)) => {
                iam::identity_decision(::std::slice::from_ref(boundary), &request)
            }
        };
        if granted && capped == Decision::Allowed {
            return None;
        }
        // WHICH document withheld the grant is a fact the clause names, so the caller is told
        // which policy to go and change.
        let reason = if said == Decision::Denied {
            iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
        } else if !granted {
            iam::no_identity_policy_allows(action)
        } else if capped == Decision::Denied {
            iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()
        } else {
            iam::no_permissions_boundary_allows(action)
        };
        Some(iam::not_authorized_by_identity_policy(
            &self.arn, action, resource, &reason,
        ))
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
        // account's own published credential, which holds no identity policy.
        None => Ok(sts_store::ambient_identity()),
    };
    // An expired session credential speaks for nobody: it carries no document, so every action
    // it asks for is refused — and the refusal names the credential's STATE rather than the key
    // id that was presented, which this module never quotes back.
    let identity = match identity {
        Ok(identity) => identity,
        Err(_) => {
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                ..Caller::default()
            };
        }
    };
    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a key it never
    // minted is the account's ambient identity — but IAM's store maps an access key id to the
    // user that owns it (the map IAM's own `GetUser` resolves a caller through), so a request
    // signed with one speaks for THAT user and is judged on the user's policies. A key the IAM
    // store does not hold either really is the account's own published credential.
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
    // Which IAM identity the principal's policies are held under is read through the tree's
    // shared readings of a principal ARN.
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by
        // the ARN the account holds it under — which carries its PATH, where the session ARN
        // does not.
        if let Some(role_arn) = store
            .roles
            .get(&iam_store::name_key(role_name))
            .map(|held| held.arn.clone())
        {
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

    /// The Region the rows below sign for, which is the one the ARNs they are decided against
    /// carry.
    const REGION: &str = "us-east-1";

    /// RULE: the resource a request acts on is read off the request — the ARN of the resource
    /// its family and id name, the ARN of the resource a create's own input names, and IAM's
    /// any-resource wildcard for a request that names no single resource.
    ///
    /// The rows differ along WHERE the request states the resource and WHICH family it
    /// belongs to: every resource family this service mints ARNs for addressed by a URI label
    /// (including `templates`, which no recorded case here drives), each family's create
    /// naming it in the input instead, a percent-encoded label, a deeper path under one
    /// resource, a collection a create's member is absent from, a body that is no JSON object,
    /// a family this service mints no ARN for, an account-level path and a send.
    #[test]
    fn the_resource_a_request_acts_on_is_read_off_the_request() {
        let arn = |resource: &str| {
            ::emulator_server::emulator_arn(crate::state::ARN_NAMESPACE, REGION, resource)
        };
        // (case, the request's URI, the request's body, the resource the decision is made
        // against)
        let rows: [(&str, &str, &str, String); 17] = [
            (
                "an email identity",
                "/v2/email/identities/sender@example.com",
                "",
                arn("identity/sender@example.com"),
            ),
            (
                "a configuration set",
                "/v2/email/configuration-sets/probe",
                "",
                arn("configuration-set/probe"),
            ),
            (
                "a contact list",
                "/v2/email/contact-lists/announcements",
                "",
                arn("contact-list/announcements"),
            ),
            (
                "a dedicated IP pool",
                "/v2/email/dedicated-ip-pools/warm",
                "",
                arn("dedicated-ip-pool/warm"),
            ),
            (
                "an email template",
                "/v2/email/templates/receipt",
                "",
                arn("template/receipt"),
            ),
            (
                "a percent-encoded id",
                "/v2/email/identities/sender%2Btag%40example.com",
                "",
                arn("identity/sender+tag@example.com"),
            ),
            (
                "a deeper path under one resource",
                "/v2/email/configuration-sets/probe/event-destinations",
                "",
                arn("configuration-set/probe"),
            ),
            (
                "the create of an email identity",
                "/v2/email/identities",
                r#"{"EmailIdentity":"minted.example.com"}"#,
                arn("identity/minted.example.com"),
            ),
            (
                "the create of a configuration set",
                "/v2/email/configuration-sets",
                r#"{"ConfigurationSetName":"minted-set","Tags":[]}"#,
                arn("configuration-set/minted-set"),
            ),
            (
                "the create of a contact list",
                "/v2/email/contact-lists",
                r#"{"ContactListName":"minted-list"}"#,
                arn("contact-list/minted-list"),
            ),
            (
                "the create of a dedicated IP pool",
                "/v2/email/dedicated-ip-pools",
                r#"{"PoolName":"minted-pool"}"#,
                arn("dedicated-ip-pool/minted-pool"),
            ),
            (
                "the create of an email template",
                "/v2/email/templates",
                r#"{"TemplateName":"minted-template"}"#,
                arn("template/minted-template"),
            ),
            (
                "a collection whose create member the input does not carry",
                "/v2/email/identities",
                r#"{"PageSize":10}"#,
                EVERY_RESOURCE.to_string(),
            ),
            (
                "a collection with no body at all",
                "/v2/email/identities",
                "",
                EVERY_RESOURCE.to_string(),
            ),
            (
                "a collection whose body is no JSON object",
                "/v2/email/contact-lists",
                "not a document",
                EVERY_RESOURCE.to_string(),
            ),
            (
                "a family this service mints no ARN for",
                "/v2/email/suppression/addresses/blocked@example.com",
                "",
                EVERY_RESOURCE.to_string(),
            ),
            (
                "a send, which names no single resource",
                "/v2/email/outbound-emails",
                r#"{"FromEmailAddress":"sender@example.com"}"#,
                EVERY_RESOURCE.to_string(),
            ),
        ];
        for (case, uri, body, expected) in rows {
            let actual = addressed_resource(uri, REGION, body.as_bytes());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a request that addresses no single resource is granted by a document that could
    /// allow the action at all, and refused by one that could not — with IAM's own sentence.
    #[test]
    fn a_caller_without_the_action_is_refused_with_iams_sentence() {
        let session = "arn:aws:sts::000000000000:assumed-role/ses-probe/probe";
        let caller = |document: &str| Caller {
            arn: session.to_string(),
            principals: vec![session.to_string()],
            documents: vec![document.to_string()],
            ..Caller::default()
        };
        let action = action_for("SendEmail");
        let logs_only = r#"{"Statement":[{"Effect":"Allow","Action":"logs:*","Resource":"*"}]}"#;
        let sending = r#"{"Statement":[{"Effect":"Allow","Action":"ses:SendEmail","Resource":"*"}]}"#;
        let refused = caller(logs_only).refusal(&action, EVERY_RESOURCE);
        assert_eq!(
            refused.as_deref(),
            Some(
                format!(
                    "User: {session} is not authorized to perform: {action} on resource: \
                     {EVERY_RESOURCE} because no identity-based policy allows the {action} action"
                )
                .as_str()
            ),
            "case \"a role granting another service's actions\": expected IAM's sentence, got \
             {refused:?}"
        );
        let allowed = caller(sending).refusal(&action, EVERY_RESOURCE);
        assert_eq!(
            allowed, None,
            "case \"a role granting this action\": expected no refusal, got {allowed:?}"
        );
    }
}
