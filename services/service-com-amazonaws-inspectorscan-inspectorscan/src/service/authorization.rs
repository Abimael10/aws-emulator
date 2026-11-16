// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLING principal perform this service's action — the decision
//! `ScanSbom` takes before it reads the submitted document.
//!
//! RULE: a signed call is decided against the calling principal's identity-based IAM
//! policies before the service acts on the request, so a principal no readable document
//! grants the action to is refused with the modelled `AccessDeniedException` (HTTP 403) and
//! is told nothing the call would have computed. A caller holding no identity-based policy
//! at all is in exactly that position — IAM's default deny — and the refusal names the
//! principal that signed, the action it asked for and the resource it addressed.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the SAME
//!    place `sts:GetCallerIdentity` answers from ([`sts_store::resolve_caller`]) for a
//!    session credential, and from IAM's own `access_keys` map (the map `iam:GetUser`
//!    resolves a caller through) for a long-term key;
//! 2. **what it may do** — the identity-based policy documents in force on that principal
//!    and the permissions boundary that caps them, read through the IAM service's OWN
//!    accessors (`principal_policies`, `permissions_boundary_document`), so a policy
//!    `iam:PutUserPolicy` wrote takes effect on the next call and deleting it lifts it
//!    again;
//! 3. **the decision and the sentence** — [`::service_common::iam_authorization`], the one
//!    shared evaluation of those documents against `(action, resource)` and the one wording
//!    of the refusal, so nothing of IAM's rule or its prose is spelled here.
//!
//! Only the store LOOKUPS live here. They stay in this service crate because the IAM crate
//! depends on `service-common`, so the shared crate cannot read IAM's store without a
//! dependency cycle; WHICH IAM entity a principal ARN names carries no such dependency and
//! is read through the shared [`::service_common::iam_role`].
//!
//! ## The action and the resource
//!
//! The action is `inspector-scan:<OperationName>`: `inspector-scan` is this service's
//! `arnNamespace` and its SigV4 signing name
//! (`smithy_query(query="service_info", model_path="services/service-com-amazonaws-inspectorscan-inspectorscan/smithy-model/com-amazonaws-inspectorscan-inspectorscan.json")`).
//!
//! The resource is the one the request addressed. This service models no resource shape and
//! mints no resource ARN — `ScanSbom` is bound to the HTTP path `/scan/sbom`
//! (`smithy_query(query="operation", operation="ScanSbom")`) and scans a document the
//! request carries — so what a call addresses is that path under this service's namespace,
//! in the Region the caller signed for ([`scan_resource`]). It is built from the request
//! alone, so the decision never depends on stored state and the refusal can be stated
//! before anything is read.
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

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The action namespace this service's operations are published under, which is also the
/// SigV4 signing name and `arnNamespace` its own model declares: a policy statement spells
/// one of them `inspector-scan:<OperationName>`.
pub const ACTION_PREFIX: &str = "inspector-scan";

/// The Region an ARN carries when the request's credential scope names none.
/// `::engine::emulator_arn`'s own documentation gives this as the emulator's default.
const DEFAULT_REGION: &str = "us-east-1";

/// Whether `region` is shaped like an AWS Region code (`us-east-1`, `ap-southeast-4`):
/// lower-case letters, digits and hyphens only, and short enough to keep an ARN carrying it
/// to a sane length.
fn is_region_shaped(region: &str) -> bool {
    !region.is_empty()
        && region.len() <= 32
        && region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The Region this request was signed for.
///
/// Read from the caller's own SigV4 credential scope, and checked against the Region grammar
/// before it is interpolated into an ARN: a scope naming something that is not a Region (or
/// no scope at all) falls back to [`DEFAULT_REGION`] rather than letting caller-chosen text
/// reshape an ARN's colon-delimited fields.
#[must_use]
pub fn request_region(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map(|scope| scope.region)
        .filter(|region| is_region_shaped(region))
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// The IAM action `operation` is performed under: `inspector-scan:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The resource a call on `path` addresses, under this service's namespace and in `region`.
///
/// Built through the shared ARN builder every ARN this emulator emits comes from. `path` is
/// the operation's own modelled HTTP path, so the ARN is a function of which operation was
/// called and nothing the caller chose.
#[must_use]
pub fn scan_resource(region: &str, path: &str) -> String {
    ::engine::emulator_arn(ACTION_PREFIX, region, path)
}

/// The resource ONE request addresses: [`scan_resource`] in the Region the caller signed for.
#[must_use]
pub fn resource_addressed(parts: &::http::request::Parts, path: &str) -> String {
    scan_resource(&request_region(parts), path)
}

/// `Some(message)` when IAM does not allow `inspector-scan:<operation>` on `resource` for
/// the calling principal — the sentence the refusal carries — and `None` when it does.
///
/// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
/// whatever else the principal carries; otherwise a grant is needed, which a caller this
/// gate does not bound has by definition (see the module docs); and a grant is capped by the
/// permissions boundary, since "a permissions boundary sets the maximum permissions that an
/// identity-based policy can grant" (IAM, permissions boundaries).
///
/// Asked with NO store guard of this service held, and before the request's own input is
/// read.
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
    // the action and the resource the request addressed.
    let request = iam::Request::new(&action, resource, &[]);
    let said = iam::identity_decision(&caller.documents, &request);
    if said == Decision::Denied {
        return Some(iam::not_authorized_by_identity_policy(
            &caller.arn,
            &action,
            resource,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    if !(iam::is_allowed(said, Decision::NotStated) || caller.identity_is_unbounded()) {
        return Some(iam::not_authorized_by_identity_policy(
            &caller.arn,
            &action,
            resource,
            &iam::no_identity_policy_allows(&action),
        ));
    }
    let boundary = caller.boundary.as_ref()?;
    let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
        Decision::Allowed => return None,
        // An explicit deny is a fact about the boundary document, so the refusal names it;
        // a boundary that merely states nothing withheld the grant the policies gave.
        Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
        Decision::NotStated => iam::no_permissions_boundary_allows(&action),
    };
    Some(iam::not_authorized_by_identity_policy(
        &caller.arn,
        &action,
        resource,
        &reason,
    ))
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
    // An expired session credential speaks for nobody: it carries no document, so every
    // action it asks for is refused — and the refusal names the credential's STATE rather
    // than the key id that was presented, which this module never quotes back.
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
        arn: identity.arn,
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
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

    /// RULE: the action asked about carries this service's own namespace, so a document
    /// scoped to its actions decides this service's operations and nothing outside them.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        let document =
            r#"{"Statement":[{"Effect":"Allow","Action":"inspector-scan:*","Resource":"*"}]}"#;
        let action = action_for("ScanSbom");
        assert_eq!(
            action, "inspector-scan:ScanSbom",
            "case \"the modelled operation\": expected this service's namespace, got {action:?}"
        );
        let reached = decision(document, &action, "*");
        assert_eq!(
            reached,
            Decision::Allowed,
            "case \"a grant over this service\": expected {action} allowed, got {reached:?}"
        );
        let outside = decision(document, "inspector2:ListFindings", "*");
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected no grant outside this service, got {outside:?}"
        );
    }

    /// RULE: the resource an ARN names is built from the request alone — this service's
    /// namespace, the Region the call was signed for, and the operation's own HTTP path.
    #[test]
    fn the_resource_is_the_operations_path_in_the_signed_region() {
        let account = ::engine::emulator_account_id();
        for (case, region, path) in [
            ("the default Region", "us-east-1", "/scan/sbom"),
            ("another Region", "eu-west-3", "/scan/sbom"),
        ] {
            let built = scan_resource(region, path);
            assert_eq!(
                built,
                format!("arn:aws:inspector-scan:{region}:{account}:{path}"),
                "case {case:?}: expected the addressed resource ARN, got {built:?}"
            );
        }
    }

    /// RULE: a credential scope naming something that is not a Region cannot reshape the
    /// ARN; the fallback is the emulator's default Region.
    #[test]
    fn a_scope_that_names_no_region_falls_back_to_the_default() {
        for (case, scope, expected) in [
            ("a Region code", Some("eu-central-1"), "eu-central-1"),
            ("no authorization header at all", None, DEFAULT_REGION),
            ("a scope carrying a colon", Some("us-east-1:x"), DEFAULT_REGION),
            ("an upper-case scope", Some("US-EAST-1"), DEFAULT_REGION),
        ] {
            let mut builder = ::http::Request::builder().method("POST").uri("/scan/sbom");
            if let Some(scope) = scope {
                builder = builder.header(
                    "authorization",
                    format!(
                        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{scope}/\
                         {ACTION_PREFIX}/aws4_request, SignedHeaders=host, Signature=00"
                    ),
                );
            }
            let (parts, ()) = builder.body(()).expect("a well-formed request").into_parts();
            let actual = request_region(&parts);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected Region {expected:?}, got {actual:?}"
            );
        }
    }
}
