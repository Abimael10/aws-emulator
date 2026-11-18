// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the step
//! every operation that models `AccessDeniedException` runs BEFORE it reads or writes a
//! profile, a job or the published catalog.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at the
//! request's own input, so a caller whose identity-based policies do not grant the action
//! is told it is not authorized rather than told anything about the resource it named, and
//! the call changes nothing. `AccessDeniedException` (HTTP 403, "You do not have sufficient
//! access to perform this action") is what such a caller is answered with, by every
//! operation whose model declares it.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`resolve_caller`, the same place `sts:GetCallerIdentity` answers
//!    from, so the principal a refusal blames is the one the caller reads back for the
//!    credentials it signed with), else the IAM user that owns the long-term key, else the
//!    account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives — so a document `iam:PutRolePolicy` wrote takes effect on
//!    the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the principal-ARN readings do.
//! The lookups stay in this crate because hoisting them into `service-common` would make
//! every service that depends on that crate link the IAM and STS service crates.
//!
//! The action asked about is `signer:<OperationName>` ([`ACTION_PREFIX`] is this service's
//! `arnNamespace`), and the resource is the one the request's OWN values address: the
//! signing-profile ARN for a call that names a profile — including the profile a
//! `profileVersionArn` belongs to, since a version is not a resource IAM addresses on its
//! own ([`profile_of_version_arn`]) — the signing-job ARN for one that names a job, and
//! [`ANY_RESOURCE`] for a call that addresses no resource of this service (the published
//! platform catalog, and the account-wide listings).
//!
//! A signing profile does carry a resource-based policy of its own
//! (`AddProfilePermission`), and it is NOT consulted here: that policy grants OTHER accounts
//! the cross-account use of a profile, so it cannot refuse a principal of the account the
//! profile belongs to, which is the only account this emulator presents.
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
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)
//!
//! What is NOT modelled here: a statement's `Condition` clauses. The decision is taken with
//! an empty request context, so a statement conditioned on a key this emulator does not
//! supply is read as the shared evaluator reads an undecided clause. Nothing here guesses a
//! condition value.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// This service's IAM action namespace: an action is `signer:<OperationName>`, which is this
/// service's `arnNamespace` (`smithy_query(query="service_info")`) — the same string every
/// ARN this service mints spells.
pub const ACTION_PREFIX: &str = "signer";

/// The resource an action that addresses no resource of this service is judged against, as
/// IAM writes it for such a request.
pub const ANY_RESOURCE: &str = "*";

/// The `code` an authorization refusal carries: the modelled shape's own name, the
/// discriminator a client branches on to tell this refusal from any other 4xx this service
/// answers.
pub const ERROR_CODE_ACCESS_DENIED: &str = "AccessDeniedException";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The signing-profile ARN a `profileVersionArn` belongs to: the version ARN with its
/// trailing version segment dropped.
///
/// A profile VERSION is not a resource IAM addresses — AWS Signer's resource is the signing
/// profile, which every version of it is a revision of — so a request that names a version is
/// authorized against the profile that version belongs to. Every other field is kept as the
/// REQUEST spelled it, its partition, region and account included: the resource a call is
/// judged against is the one the request ADDRESSED, not one this account was found to hold, so
/// a policy scoped to the ARN a caller passes decides that caller's call.
///
/// `None` when the value is not a well-formed signing-profile VERSION ARN of this service
/// (see [`crate::state::profile_version_in_arn`]), in which case it addresses no profile at
/// all and the caller judges the request against [`ANY_RESOURCE`].
pub fn profile_of_version_arn(profile_version_arn: &str) -> Option<String> {
    // Judged as an ARN of this service first, so only the version segment of a value that
    // really names a profile version is dropped.
    crate::state::profile_version_in_arn(profile_version_arn)?;
    let (profile_arn, _version) = profile_version_arn.rsplit_once('/')?;
    Some(profile_arn.to_string())
}

/// Whether IAM permits `operation` on `resource` for the caller of this request, and the
/// sentence that says why not when it does not.
///
/// `operation` is the operation's own name ([`smithy_server_runtime::Operation::NAME`]),
/// which names the IAM action `signer:<Operation>`; `resource` is the ARN the request
/// ADDRESSES, or [`ANY_RESOURCE`] for a call that addresses no resource of this service.
/// `None` for a request IAM permits (and for every caller this gate does not bound, see the
/// module docs), and the refusal's message when it does not — which each handler answers in
/// its own modelled `AccessDeniedException` (see [`crate::authorize`]).
///
/// Call it BEFORE this service's own store is locked, so the IAM and STS stores are never
/// reached while this service's guard is held, and with an ARN built from what the REQUEST
/// carried rather than from a stored record, so the decision does not depend on whether that
/// resource exists — which is also what keeps a refused call from telling an unauthorized
/// caller whether it does.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service may take
/// this decision too.
pub fn authorization_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no such
    // element, so the decision below rests on the action and the resource.
    let request = iam::Request::new(&action, resource, &caller.principals);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return Some(not_authorized(
            &caller.arn,
            &action,
            resource,
            "with an explicit deny in an identity-based policy",
        ));
    }
    let granted =
        iam::is_allowed(identity_said, Decision::NotStated) || caller.identity_is_unbounded();
    if !granted {
        return Some(not_authorized(
            &caller.arn,
            &action,
            resource,
            &format!("because no identity-based policy allows the {action} action"),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
            Decision::Allowed => return None,
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the
            // identity-based policies gave.
            Decision::Denied => "with an explicit deny in a permissions boundary".to_string(),
            Decision::NotStated => {
                format!("because no permissions boundary allows the {action} action")
            }
        };
        return Some(not_authorized(&caller.arn, &action, resource, &reason));
    }
    None
}

/// The sentence IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it. Every part comes from the request and
/// from the stored policies — the principal from the credential the request presented, the
/// action from the operation it named, the resource from the ARN it addressed.
///
/// An action that addresses no resource of this service is judged against [`ANY_RESOURCE`],
/// and the sentence states no resource clause for it: there is nothing to name.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    let on_resource = if resource == ANY_RESOURCE {
        String::new()
    } else {
        format!(" on resource: {resource}")
    };
    format!("User: {principal} is not authorized to perform: {action}{on_resource} {reason}")
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
        // principal spelling, and every action it asks for falls to the default deny. The key
        // id it presented is NOT quoted back — this module echoes no value of the request —
        // and in the assembled emulator the layer that authenticates a request refuses an
        // expired credential before any handler runs.
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

/// Authorize this request, in the error channel of the operation that makes it: `Ok(())` for
/// a request IAM permits, and that operation's own modelled `AccessDeniedException` (HTTP
/// 403) for one it refuses, carrying the sentence [`authorization_refusal`] built.
///
/// Every operation whose model declares `AccessDeniedException` runs this as its FIRST step,
/// with the action's own name and the ARN the request addresses, so one rule decides every
/// call of this service and one sentence words every refusal of one.
///
/// Each of those operations has its OWN generated error enum, so the lift cannot be a
/// function over one type; it is this macro instead, expanded once per handler with
/// `$modeled` naming that handler's `{Op}ErrorModeled` — the same reason
/// [`crate::tagging_arn_refusal`] is a macro. The decision, the sentence and the `code` all
/// come from this module, so every expansion answers alike.
#[macro_export]
macro_rules! authorize {
    ($modeled:ident, $engine:expr, $ctx:expr, $operation:expr, $resource:expr) => {
        match $crate::state::authorization::authorization_refusal(
            $engine,
            $ctx,
            $operation,
            $resource,
        ) {
            ::std::option::Option::None => ::std::result::Result::Ok(()),
            ::std::option::Option::Some(message) => ::std::result::Result::Err(
                $modeled::AccessDeniedException($crate::ops::types::AccessDeniedException {
                    message: ::std::option::Option::Some(message),
                    code: ::std::option::Option::Some(
                        $crate::state::authorization::ERROR_CODE_ACCESS_DENIED.to_string(),
                    ),
                }),
            ),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision one stored document reaches about an action on a resource, through the
    /// shared evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: the action this gate asks about is `signer:<OperationName>`, so a policy scoped
    /// to this service's actions covers every operation of it and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"signer:*","Resource":"*"}]}"#;
        for operation in [
            "AddProfilePermission",
            "CancelSigningProfile",
            "DescribeSigningJob",
            "GetRevocationStatus",
            "GetSigningPlatform",
            "GetSigningProfile",
            "ListProfilePermissions",
            "ListSigningJobs",
            "ListSigningPlatforms",
            "ListSigningProfiles",
            "PutSigningProfile",
            "RemoveProfilePermission",
            "RevokeSignature",
            "RevokeSigningProfile",
            "SignPayload",
            "StartSigningJob",
        ] {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let reached = decision(document, &action, ANY_RESOURCE);
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected signer:* to allow {action}, got {reached:?}"
            );
        }
        let outside = decision(document, "s3:GetObject", ANY_RESOURCE);
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected signer:* to grant nothing outside this \
             service, got {outside:?}"
        );
    }

    /// RULE: a request that names a profile VERSION is authorized against the signing profile
    /// that version belongs to — the version ARN with its trailing segment dropped and every
    /// other field, the account included, as the request spelled it — and a value that is no
    /// version ARN of this service addresses no profile.
    #[test]
    fn a_version_arn_is_authorized_against_its_profile() {
        let account = ::engine::emulator_account_id();
        // The rows span the forms the member carries: a version ARN of either region, one of
        // ANOTHER account, one whose profile name is not a value the `profileName` member
        // itself would admit, the profile ARN without a version, an ARN of another resource of
        // this service, one of another service, and a value that is no ARN at all.
        let rows: &[(&str, String, Option<String>)] = &[
            (
                "a version ARN",
                format!("arn:aws:signer:us-east-1:{account}:/signing-profiles/n/aaaaaaaaaa"),
                Some(format!(
                    "arn:aws:signer:us-east-1:{account}:/signing-profiles/n"
                )),
            ),
            (
                "a version ARN of another region",
                format!("arn:aws:signer:eu-west-3:{account}:/signing-profiles/deploys/0123456789"),
                Some(format!(
                    "arn:aws:signer:eu-west-3:{account}:/signing-profiles/deploys"
                )),
            ),
            (
                "a version ARN of another account",
                "arn:aws:signer:us-west-2:111122223333:/signing-profiles/n/aaaaaaaaaa".to_string(),
                Some("arn:aws:signer:us-west-2:111122223333:/signing-profiles/n".to_string()),
            ),
            (
                "a version ARN of another partition",
                format!("arn:aws-cn:signer:cn-north-1:{account}:/signing-profiles/n/v1"),
                Some(format!(
                    "arn:aws-cn:signer:cn-north-1:{account}:/signing-profiles/n"
                )),
            ),
            (
                "a version ARN whose profile name the member's own bounds would refuse",
                format!("arn:aws:signer:us-east-1:{account}:/signing-profiles/a/v"),
                Some(format!(
                    "arn:aws:signer:us-east-1:{account}:/signing-profiles/a"
                )),
            ),
            (
                "a profile ARN carrying no version",
                format!("arn:aws:signer:us-east-1:{account}:/signing-profiles/n"),
                None,
            ),
            (
                "a signing-job ARN",
                format!("arn:aws:signer:us-east-1:{account}:/signing-jobs/aaaaaaaaaaaaaaaaaaaa"),
                None,
            ),
            (
                "an ARN of another service",
                format!("arn:aws:s3:::{account}/signing-profiles/n/aaaaaaaaaa"),
                None,
            ),
            ("not an ARN at all", "/signing-profiles/n/v".to_string(), None),
        ];
        for (case, value, expected) in rows {
            let reached = profile_of_version_arn(value);
            assert_eq!(
                reached.as_deref(),
                expected.as_deref(),
                "case {case:?}: expected {expected:?} for {value:?}, got {reached:?}"
            );
        }
    }

    /// RULE: the refusal names the principal, the action, the resource the request addressed
    /// and which of IAM's rules refused — every part taken from the request and the stored
    /// policies — and it names no resource for an action that addresses none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = "arn:aws:sts::000000000000:assumed-role/release/session-1";
        let profile = "arn:aws:signer:us-west-2:000000000000:/signing-profiles/release";
        // (case, the operation, the resource it addressed, the clause the sentence carries)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "a resource-scoped action",
                "GetRevocationStatus",
                profile,
                format!(" on resource: {profile}"),
            ),
            (
                "an action that addresses no resource",
                "ListSigningProfiles",
                ANY_RESOURCE,
                String::new(),
            ),
        ];
        for (case, operation, resource, clause) in rows {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let reason = "with an explicit deny in an identity-based policy";
            let message = not_authorized(principal, &action, resource, reason);
            assert_eq!(
                message,
                format!(
                    "User: {principal} is not authorized to perform: {action}{clause} {reason}"
                ),
                "case {case:?}: expected IAM's wording, got {message:?}"
            );
        }
    }
}
