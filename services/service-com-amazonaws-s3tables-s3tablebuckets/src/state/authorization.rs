// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for —
//! the decision taken for EVERY operation this service serves, before the
//! handler reads or writes anything.
//!
//! One question, decided from documents collected in two places and answered by
//! the one shared evaluator ([`refusal`]): do the CALLER's own identity-based
//! policies and the RESOURCE's own resource-based policy, read together in IAM's
//! order, let this principal perform this action on this resource? This
//! service's request-time gate ([`crate::service::authorization`]) asks it for
//! every operation it serves, handing in the resource that operation's own input
//! addresses and the policy in force on it.
//!
//! Every operation of this service documents the permission it takes ("You must
//! have the s3tables:GetTableEncryption permission to use this operation"), and
//! AWS decides a signed call against the policies in force before the service
//! looks at the resource: a caller holding no grant is told it is not authorized
//! rather than told whether the resource it named exists, and a refused write
//! changes nothing. S3 Tables answers such a call `AccessDeniedException` ("The
//! action cannot be performed because you do not have the required permission")
//! whether or not THAT operation's own model lists the shape, which is why the
//! refusal travels through the channel every operation of this service shares
//! ([`crate::scaffolded::Scaffolded::AccessDenied`]) rather than through one
//! handler's `#[operation_error]` enum.
//!
//! The three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from
//!    the SAME place `sts:GetCallerIdentity` answers from
//!    ([`sts_store::resolve_caller`]) for a session credential, and from IAM's
//!    own `access_keys` map (the map `iam:GetUser` resolves a caller through)
//!    for a long-term key, so the principal a refusal blames is the one the
//!    caller reads back for the credentials it signed with;
//! 2. **what it may do** — the identity-based policy documents in force on that
//!    principal and the permissions boundary that caps them, read through the
//!    IAM service's OWN accessors (`principal_policies`,
//!    `permissions_boundary_document`), where the collection rule itself lives —
//!    so a document `iam:PutRolePolicy` wrote takes effect on the next request
//!    and deleting it lifts it again; beside them, the resource-based document
//!    the gate hands in, which is the one `PutTableBucketPolicy` wrote on the
//!    table bucket the request addresses and `DeleteTableBucketPolicy` removed;
//! 3. **the decision** — [`::service_common::iam_authorization`], the one shared
//!    evaluation of those documents against `(action, resource, principal)`, so
//!    this service and every other one handed the same documents answer alike;
//!    and the SENTENCE a refusal carries, which is that module's
//!    [`::service_common::iam_authorization::not_authorized_by_identity_policy`]
//!    and the reason clauses beside it, so nothing of IAM's wording is spelled
//!    here.
//!
//! Only the store LOOKUPS live here; neither the collection nor the decision
//! does. The lookups stay in this service crate because the IAM service crate
//! depends on `service-common`, so the shared crate cannot read IAM's store
//! without a dependency cycle. WHICH IAM entity a principal ARN names carries no
//! such dependency, so it is read through the shared
//! [`::service_common::iam_role`] rather than parsed again here.
//!
//! The RESOURCE a request is decided against is the ARN of what the operation
//! ACTS ON: the table, namespace or table bucket the request addresses, spelled
//! as this service mints that resource's ARN. A table's permission is granted on
//! the table ("Actions, resources, and condition keys for Amazon S3 Tables"
//! publishes a table as a resource of its own, under its bucket's ARN), so an
//! operation hands that ARN in; what a refusal names is therefore the resource
//! the caller addressed and nothing else of the request.
//!
//! ## The two callers the decision cannot refuse
//!
//! * **The account's own published credential.** A request whose access key id
//!   neither STS nor IAM holds a record for resolves to
//!   [`sts_store::ambient_identity`], the account's own identity, which is not a
//!   principal the IAM store holds policies for — the same position AWS account
//!   root credentials are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An
//!   AWS-managed policy is owned by AWS, not by the account, so the IAM store
//!   holds no document for it. A principal with such a policy attached is
//!   allowed: this service cannot see what that document grants, and refusing on
//!   evidence it does not have would deny access AWS permits. An explicit `Deny`
//!   in a document that CAN be read still refuses.
//!
//! An expired session credential speaks for nobody: it carries no document, so
//! every action it asks for is refused, and the refusal names that state rather
//! than the key id that was presented, which this module never quotes back.
//!
//! The work one decision costs is bounded by what IAM holds for ONE principal
//! and does not grow with the request; it is taken with no guard of this
//! service's own store held, and it writes nothing to either store it reads.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// What a refusal names as the principal of a request whose session credential
/// has already expired: that credential speaks for nobody, and the key it
/// presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// This service's IAM action namespace, which is how a policy statement spells
/// one of its actions: the `arnNamespace` that is also its `sigv4Name`
/// (`smithy_query(query="service_info")`) and the vendor field of every ARN this
/// service mints, so the action and the resource a call is judged on are spelled
/// under the same namespace.
pub const ACTION_PREFIX: &str = "s3tables";

/// The IAM action `operation` is performed under: `s3tables:<OperationName>`,
/// which is the permission each operation's own documentation names.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// `Some(message)` when the policies in force do not let the calling principal
/// perform `s3tables:<operation>` on `resource` — the sentence the refusal
/// carries — and `None` when they do.
///
/// `resource` is the ARN of what the operation acts on and `resource_policy` the
/// resource-based document in force on it (the policy of the table bucket that
/// holds it), both as the gate resolved them off the request's own input;
/// `None` is a resource carrying no such document, which withholds nothing.
///
/// IAM's order, in the order it is decided here:
///
/// * "an explicit deny in any policy overrides any allow", so a `Deny` about the
///   request in a document that CAN be read refuses it whatever else allows it —
///   and the reason clause names WHICH document the deny is a fact about, since
///   AWS distinguishes a resource-based deny from an identity-based one and both
///   from IAM's default deny;
/// * otherwise a grant is needed, and within one account a grant from EITHER
///   side is enough ("the resource-based policy and the identity-based policy are
///   evaluated together, and access is granted if either allows it"); a caller
///   this gate does not bound has one by definition (see the module docs);
/// * a grant is capped by the permissions boundary, since "a permissions boundary
///   sets the maximum permissions that an identity-based policy can grant" (IAM,
///   permissions boundaries).
///
/// Asked with NO store guard held, and before the operation reads or writes
/// anything.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
    resource_policy: Option<&str>,
) -> Option<String> {
    let action = action_for(operation);
    let caller = caller(engine, ctx);
    // An identity-based document states no `Principal` element, so that half of
    // the decision rests on the action and the resource the request addressed.
    let request = iam::Request::new(&action, resource, &[]);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    // A resource-based statement is about the request only when its `Principal`
    // element names the calling principal. IAM reads one that ALLOWS an ACCOUNT
    // as delegating the decision to that account's own identity-based policies
    // rather than as a grant of its own, while one that DENIES the account
    // refuses each of its principals — so the DENY is decided with the account's
    // spellings beside the principal's own, and the GRANT with the principal's
    // alone.
    let said_about = |named: &[String]| {
        resource_policy.map_or(Decision::NotStated, |policy| {
            iam::resource_decision(policy, &iam::Request::new(&action, resource, named))
        })
    };
    let mut denied_by_name = caller.principals.clone();
    denied_by_name.extend(iam::account_principals(&caller.arn));
    let resource_denied = said_about(&denied_by_name);
    let resource_granted = said_about(&caller.principals);
    let refused =
        |reason: &str| iam::not_authorized_by_identity_policy(&caller.arn, &action, resource, reason);
    if resource_denied == Decision::Denied {
        return Some(refused(iam::RESOURCE_POLICY_EXPLICIT_DENY));
    }
    if identity_said == Decision::Denied {
        return Some(refused(iam::IDENTITY_POLICY_EXPLICIT_DENY));
    }
    if !(iam::is_allowed(identity_said, resource_granted) || caller.identity_is_unbounded()) {
        return Some(refused(&iam::no_identity_policy_allows(&action)));
    }
    let boundary = caller.boundary.as_ref()?;
    let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
        Decision::Allowed => return None,
        // An explicit deny is a fact about the boundary document, so the refusal
        // names it; a boundary that merely states nothing withheld the grant the
        // policies gave.
        Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
        Decision::NotStated => iam::no_permissions_boundary_allows(&action),
    };
    Some(refused(&reason))
}

/// The ARN of the principal this request speaks for, as `sts:GetCallerIdentity`
/// reports it.
///
/// It is the SAME resolution [`refusal`] blames a refused request by ([`caller`]
/// below), so a refusal this service words itself and a refusal it relays from
/// another service name one principal. A request whose credential neither STS
/// nor IAM holds a record for speaks for the account's own published identity,
/// which is what that operation answers for it too.
///
/// Reach for it where a refusal is about something OTHER than this service's own
/// authorization decision — a resource of another service the caller cannot be
/// answered about — since the decision itself already carries the principal.
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
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a
    /// refusal names.
    arn: String,
    /// Every spelling of the PRINCIPAL ITSELF that a resource-based policy's
    /// `Principal` element can name it by: the ARN STS reports for the
    /// credential, and the ARN of the IAM role behind an assumed-role session.
    /// The account the principal belongs to is not one of them — naming the
    /// account is not naming a principal — so [`refusal`] adds those spellings
    /// for the DENY alone.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one
    /// whose document this emulator holds.
    boundary: Option<String>,
    /// Set when this request presents the account's own published credential,
    /// which holds no IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store
    /// does not hold — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller
    /// once no readable document DENIES the action: the account's own
    /// credential, which holds no IAM policies, or a principal carrying a grant
    /// this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM
    /// service's OWN accessors: the documents in force on it, whether it carries
    /// an attachment whose document this emulator cannot read, and the
    /// permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &iam_store::ComAmazonawsIamAwsidentitymanagementv20100508State,
        identity_arn: &str,
    ) {
        if let Some(policies) = store.principal_policies(identity_arn) {
            self.documents = policies.documents;
            // An attachment the collection resolved no document for is an entry
            // of AWS's own catalog this emulator does not carry: a grant it
            // cannot read.
            self.unreadable_attachment = !policies.uncarried.is_empty();
        }
        self.boundary = store.permissions_boundary_document(identity_arn);
    }
}

/// The principal this request speaks for, with the identity-based policies in
/// force on it.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which
/// materialises the session expiries the clock has already made due before it
/// answers: that write is owed at the same instant whether or not anybody
/// called, which is why a REPORTING operation of this service takes this
/// decision too.
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
        // No STS store on this engine: the only identity a request can present
        // is the account's own published credential, which holds no identity
        // policy.
        None => Ok(sts_store::ambient_identity()),
    };
    // An expired session credential speaks for nobody: it carries no document,
    // so every action it asks for is refused — and the refusal names the
    // credential's STATE rather than the key id that was presented, which this
    // module never quotes back.
    let identity = match identity {
        Ok(identity) => identity,
        Err(expired) => {
            let _ = expired;
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
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a
    // key it never minted is the account's ambient identity — but IAM's store
    // maps an access key id to the user that owns it (the map `iam:GetUser`
    // resolves a caller through), so a request signed with one speaks for THAT
    // user and is judged on the user's policies. A key the IAM store does not
    // hold either really is the account's own published credential.
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
    // Which IAM identity the principal's policies are held under is read through
    // the tree's shared readings of a principal ARN.
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is
        // addressed by the ARN the account holds it under — which carries its
        // PATH, where the session ARN does not.
        if let Some(role_arn) = store
            .roles
            .get(&iam_store::name_key(role_name))
            .map(|held| held.arn.clone())
        {
            caller.principals.push(role_arn.clone());
            caller.read_policies_of(&store, &role_arn);
        }
    } else if ::service_common::iam_role::user_name(&caller.arn).is_some() {
        // An IAM user's permissions are its own, under the ARN it was resolved
        // by.
        let user_arn = caller.arn.clone();
        caller.read_policies_of(&store, &user_arn);
    }
    caller
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What one stored document says about `action` on `resource`, through the
    /// shared evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: the action this gate asks about is `s3tables:<OperationName>`, so a
    /// document scoped to this service's actions decides every operation of it
    /// and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"s3tables:*","Resource":"*"}]}"#;
        for operation in [
            "GetTableEncryption",
            "GetTableBucketEncryption",
            "CreateTable",
        ] {
            let action = action_for(operation);
            let reached = decision(document, &action, "*");
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected s3tables:* to allow {action}, got {reached:?}"
            );
        }
        let outside = decision(document, "s3:GetObject", "*");
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected s3tables:* to state nothing outside this \
             service, got {outside:?}"
        );
    }

    /// RULE: a grant is read for the action AND the resource it names, so a
    /// statement about another table bucket allows nothing here.
    #[test]
    fn a_grant_is_read_for_the_action_and_the_resource_it_names() {
        let granted = crate::state::table_bucket_arn("us-east-1", "granted");
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
            "Action":"s3tables:GetTableEncryption","Resource":"{granted}"}}]}}"#
        );
        // (case, operation, resource, allowed)
        let rows: &[(&str, &str, String, bool)] = &[
            (
                "the granted action on the granted table bucket",
                "GetTableEncryption",
                granted.clone(),
                true,
            ),
            (
                "another action on the same table bucket",
                "GetTableStorageClass",
                granted.clone(),
                false,
            ),
            (
                "the granted action on another table bucket",
                "GetTableEncryption",
                crate::state::table_bucket_arn("us-east-1", "other"),
                false,
            ),
            (
                "the granted action on the same name in another Region",
                "GetTableEncryption",
                crate::state::table_bucket_arn("eu-west-1", "granted"),
                false,
            ),
        ];
        for (case, operation, resource, expected) in rows {
            let said = decision(&document, &action_for(operation), resource);
            let actual = iam::is_allowed(said, Decision::NotStated);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected allowed == {expected}, got {actual} ({said:?})"
            );
        }
    }

    /// RULE: an explicit `Deny` is final, whatever else the principal carries.
    #[test]
    fn an_explicit_deny_wins_over_a_wildcard_grant() {
        let document = r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Action":"s3tables:*","Resource":"*"},
            {"Effect":"Deny","Action":"s3tables:GetTableEncryption","Resource":"*"}]}"#;
        let resource = crate::state::table_bucket_arn("us-east-1", "denied");
        let denied = decision(document, &action_for("GetTableEncryption"), &resource);
        assert_eq!(
            denied,
            Decision::Denied,
            "case \"the denied action\": expected Denied, got {denied:?}"
        );
        let allowed = decision(document, &action_for("GetTableStorageClass"), &resource);
        assert_eq!(
            allowed,
            Decision::Allowed,
            "case \"an action the deny does not name\": expected Allowed, got {allowed:?}"
        );
    }
}
