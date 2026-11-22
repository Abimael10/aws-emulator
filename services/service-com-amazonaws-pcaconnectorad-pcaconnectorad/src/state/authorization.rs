// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the
//! authorization step every operation runs BEFORE it reads or writes anything.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what the
//! request names, so a caller whose policies do not grant the action is told it is not
//! authorized rather than told anything about the resource it addressed, and the call
//! performs nothing. Every operation of this model declares `AccessDeniedException`
//! (HTTP 403) for that decision — "insufficient permissions in policies attached to your
//! Amazon Web Services Identity and Access Management (IAM) principal"
//! (`smithy_query(query="shape",shape="AccessDeniedException")`) — so the refusal is that
//! modelled error and not a synthetic one.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the ROLE SESSION the
//!    minting operation recorded that key for in the IAM store (the record IAM's own
//!    authorizer resolves a temporary credential through), else the session STS holds for
//!    it (`sts_store::resolve_request_caller`, the same place `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for
//!    the credentials it signed with), else the IAM user that owns the long-term key, else
//!    the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for
//!    that principal and the permissions boundary that caps them, read through the IAM
//!    service's OWN accessors (`principal_policies`,
//!    `permissions_boundary_document`), where the collection rule itself lives — so a
//!    policy `iam:PutRolePolicy` wrote takes effect on the next request and deleting it
//!    lifts it again;
//! 3. **what the mint scoped the credential to** — the session policies `sts:AssumeRole`
//!    was called with, read through STS's own resolver
//!    (`sts_store::session_scoping_documents`): "the resulting session's permissions are
//!    the intersection of the role's identity-based policy and the session policies"
//!    (`AssumeRoleRequest$PolicyArns`), so a scope that does not allow the action withholds
//!    it however the role's own documents read;
//! 4. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default
//!    deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor its wording does. That is the
//! split every other gate in this tree makes (AWS Private CA, AWS Secrets Manager, Amazon
//! ECS), and the lookups stay in the service crate for the reason those keep theirs there:
//! hoisting them into `service-common` would make every service that depends on that crate
//! link the IAM and STS service crates.
//!
//! ## The action and the resource
//!
//! The action is `pca-connector-ad:<OperationName>` ([`ACTION_PREFIX`] is this service's
//! `arnNamespace`, `smithy_query(query="service_info")`), so a policy decides each
//! operation separately; nothing here is keyed on which operation asked, because the action
//! is composed from [`smithy_server_runtime::Operation::NAME`] at each handler's one call
//! site.
//!
//! The resource is what the request NAMES — the ARN its own members carry — taken from the
//! request rather than resolved from a stored record, which is what keeps the decision
//! independent of whether that resource exists and keeps a refusal from disclosing whether
//! it does. An action that addresses no single resource of its own (creating one, listing
//! the account's) is judged against the COLLECTION it adds to or reads over, the ARN this
//! service already names such a request's subject by
//! ([`crate::state::connector_collection_arn`],
//! [`crate::state::directory_registration_collection_arn`]).
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! Everything else is judged, including a principal whose only attachment is an AWS-managed
//! policy whose document this emulator does not carry: such an attachment grants nothing
//! the decision can read, so it does not lift IAM's default deny. That reading cannot
//! withhold a grant a readable document gives — the account's own inline and managed
//! documents are all carried, and so are the AWS-managed policies that grant a service's
//! actions wholesale (`AdministratorAccess`, `PowerUserAccess`) — while the opposite
//! reading would serve every request of a role whose attachment is an uncarried
//! deny-everything policy, which is the shape a least-privilege caller takes.
//!
//! An expired session credential speaks for nobody: it carries no documents and no
//! principal spelling, so every action it asks for is refused. (A session the
//! authenticating layer has already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// This service's IAM action prefix: an action is `pca-connector-ad:<OperationName>`, which
/// is this service's `arnNamespace` (`smithy_query(query="service_info")`) — the same
/// string [`super::ARN_NAMESPACE`] spells into every ARN this service builds.
pub const ACTION_PREFIX: &str = super::ARN_NAMESPACE;

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The global condition keys that name the PRINCIPAL a request was signed by, which this
/// gate answers exactly: the ARN of the principal that made the request, and the account
/// that principal belongs to (IAM's global condition keys). No member of this model
/// publishes a service-specific key, so these are the whole context.
const PRINCIPAL_ARN_KEY: &str = "aws:PrincipalArn";
const PRINCIPAL_ACCOUNT_KEY: &str = "aws:PrincipalAccount";

/// Every connector of this account in the Region the request was signed for, which is what
/// a call that addresses no single connector of its own — creating one, listing them — is
/// judged against.
///
/// IAM writes such a resource as the resource TYPE's ARN with a wildcard resource id
/// (`arn:aws:pca-connector-ad:<region>:<account>:connector/*`), which is also the form a
/// policy has to name to admit the call, so a refusal quotes the ARN a caller would have to
/// grant.
pub fn any_connector(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    every_resource_of(&super::connector_collection_arn(&super::request_region(ctx)))
}

/// Every directory registration of this account in that Region. The mirror of
/// [`any_connector`].
pub fn any_directory_registration(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    every_resource_of(&super::directory_registration_collection_arn(
        &super::request_region(ctx),
    ))
}

/// The wildcard form of one of this service's collection ARNs: every resource it holds of
/// that kind.
fn every_resource_of(collection_arn: &str) -> String {
    format!("{collection_arn}/*")
}

/// The sentence to refuse this request with, or `None` when IAM permits it — where
/// `operation` is the operation's own name ([`smithy_server_runtime::Operation::NAME`]) and
/// `resource` the ARN the request NAMES.
///
/// Call it FIRST in a handler, before the store is read or written and before the request's
/// own values are judged against stored state, so a refused call performs nothing and tells
/// an unauthorized caller nothing about what the request named. Each handler returns the
/// sentence as its own modelled `AccessDeniedException`, which carries this service's 403.
///
/// The sentence is the shared one ([`iam::not_authorized_by_identity_policy`]): the principal
/// that signed, the action it asked for, the resource it asked for, and the clause naming
/// WHICH of IAM's rules withheld the grant — an explicit `Deny` in one of the principal's own
/// documents, in the scope the credential was minted with or in its permissions boundary, or
/// the absence of any grant in one of those three. The clauses are the shared ones
/// ([`iam::IDENTITY_POLICY_EXPLICIT_DENY`] and its siblings), so this service cannot word a
/// decision differently from another service that reaches the same one.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises
/// the session expiries the clock has already made due; that write is owed at the same
/// instant whether or not anybody called, which is why a REPORTING operation of this
/// service takes this decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    let action = iam::iam_action(ACTION_PREFIX, operation);
    let caller = caller(engine, ctx);
    let refusal = |reason: &str| {
        Some(iam::not_authorized_by_identity_policy(
            &caller.arn,
            &action,
            resource,
            reason,
        ))
    };
    // Every key the decision may read: the ones that name the principal the request was
    // signed by, and the CLASS of the credential it was signed with, which is what a
    // statement conditioned on `aws:PrincipalType` selects on. A credential that speaks for
    // nobody populates none of them.
    let mut context: Vec<(String, String)> = Vec::new();
    if !caller.principals.is_empty() {
        context.push((PRINCIPAL_ARN_KEY.to_string(), caller.arn.clone()));
        if let Some(parsed) = ::service_common::arn::parse(&caller.arn) {
            if !parsed.account.is_empty() {
                context.push((PRINCIPAL_ACCOUNT_KEY.to_string(), parsed.account.to_string()));
            }
        }
    }
    context.extend(
        ::service_common::temporary_credential_classes::principal_type_conditions(
            caller.principal_type,
        ),
    );
    // The keys the request DECLARES it models, which is what gives IAM's empty-set reading
    // something to decide: every key it carries a value for.
    let published: Vec<&str> = context.iter().map(|(key, _)| key.as_str()).collect();
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, so the decision rests on the action, the resource and the conditions.
    let request = iam::Request::new(&action, resource, &caller.principals)
        .with_context(&context)
        .with_published_keys(&published);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    // Taken before any grant is read, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries.
    if identity_said == Decision::Denied {
        return refusal(iam::IDENTITY_POLICY_EXPLICIT_DENY);
    }
    // What the SCOPE this credential was minted with says, through the tree's one reading of
    // a resolved scope (`sts_store::ScopingPolicies::decision`): a credential no mint scoped
    // is capped by nothing, which that reading answers as allowed. A session policy takes
    // permissions away and grants none of its own, so a call it does not allow is refused
    // even where the role's own documents grant it.
    match caller.scope.decision(&request) {
        Decision::Allowed => {}
        // An explicit deny is a fact about the SCOPE's document, so the refusal names it; a
        // scope that merely states nothing about the action withheld it all the same.
        Decision::Denied => return refusal(iam::SESSION_POLICY_EXPLICIT_DENY),
        Decision::NotStated => return refusal(&iam::no_session_policy_allows(&action)),
    }
    let granted =
        iam::is_allowed(identity_said, Decision::NotStated) || caller.identity_is_unbounded();
    if !granted {
        return refusal(&iam::no_identity_policy_allows(&action));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy
    // can grant" (IAM, permissions boundaries): a grant the boundary does not also allow is
    // not a permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
            Decision::Allowed => return None,
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => iam::no_permissions_boundary_allows(&action),
        };
        return refusal(&reason);
    }
    None
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
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
    /// What the mint scoped this credential to: the session policies `sts:AssumeRole` was
    /// called with, resolved to the documents in force now. A credential no mint scoped is
    /// capped by nothing.
    scope: sts_store::ScopingPolicies,
    /// The CLASS of the credential the request was signed with, as STS reports it for that
    /// credential (`sts_store::Caller::principal_type`): the value the global condition key
    /// naming the calling principal's type answers with. `None` for a request this gate
    /// resolved no credential for, which carries no value for the key at all.
    principal_type: Option<&'static str>,
    /// Set when this request presents the account's own published credential, which holds
    /// no IAM policies (see the module docs).
    account_credential: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no
    /// IAM policies at all.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's
    /// own accessors: the documents in force on it and the permissions boundary that caps
    /// it.
    ///
    /// `principal_policies` is where the collection rule itself lives — a role's or user's
    /// inline policies, the default version of each managed policy attached to it, and for
    /// a user the policies of the groups it belongs to — so this service reads an
    /// identity's permissions exactly as IAM's own authorizer does.
    fn read_policies_of(
        &mut self,
        store: &::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State,
        identity_arn: &str,
    ) {
        if let Some(policies) = store.principal_policies(identity_arn) {
            // An attachment the collection resolved no document for is an entry of AWS's
            // own catalog this emulator does not carry; it states nothing this decision can
            // read, so it is not among the documents and grants nothing (see the module
            // docs).
            self.documents = policies.documents;
        }
        self.boundary = store.permissions_boundary_document(identity_arn);
    }
}

/// The principal this request speaks for, with the policies in force on it.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let resolved = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            sts_store::ambient_identity(),
        )
        .map(|caller| {
            (
                caller.principal_type(),
                caller.identity,
                caller.session_policies,
            )
        }),
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential, which no mint scoped.
        None => Ok((
            ::service_common::temporary_credential_classes::PRINCIPAL_TYPE_USER,
            sts_store::ambient_identity(),
            sts_store::SessionPolicies::default(),
        )),
    };
    let (principal_type, identity, session_policies) = match resolved {
        Ok(resolved) => resolved,
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
                scope: sts_store::ScopingPolicies::default(),
                principal_type: None,
                account_credential: false,
            };
        }
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        documents: Vec::new(),
        boundary: None,
        // The inline document the mint passed scopes the credential whether or not an IAM
        // store is there; where there is one, the scope is re-read below so a managed
        // session policy counts at the version in force NOW.
        scope: sts_store::session_scoping_documents(None, &session_policies),
        // WHICH CLASS of credential signed this request, read through STS's one answer for it
        // (`sts_store::Caller::principal_type`).
        principal_type: Some(principal_type),
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // What the mint scoped this credential to, as the documents read at the instant of this
    // request: a managed session policy stands for its DEFAULT version, so one edited since
    // the mint scopes by what it says now.
    caller.scope = sts_store::session_scoping_documents(Some(&store), &session_policies);
    // A TEMPORARY credential of a ROLE SESSION: the record the minting operation left in
    // IAM's own store is what says which principal that access key id speaks for and which
    // role's policies the call is judged on — the same record IAM's own authorizer resolves
    // such a credential through — so it is read FIRST, before this request is treated as
    // anything else. It also carries the scope the mint passed, which is read here for the
    // same reason: a credential whose session this store holds is judged on what that record
    // says, whatever another store holds for the same key.
    let now = ::aws_smithy_types::DateTime::from(SystemTime::now());
    if let Some(session) = store.role_session(&access_key_id, now) {
        caller.account_credential = false;
        caller.arn = session.session_arn.clone();
        caller.principals = vec![session.session_arn.clone(), session.role_arn.clone()];
        caller.scope = sts_store::session_scoping_documents(Some(&store), &session.scope);
        let role_arn = session.role_arn.clone();
        caller.read_policies_of(&store, &role_arn);
        return caller;
    }
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
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed
        // by the ARN the account holds it under — which carries its PATH, where the session
        // ARN does not.
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
