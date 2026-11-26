// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLER of a Data Catalog request perform the action it names, decided
//! BEFORE the operation reads or writes anything.
//!
//! The Data Catalog publishes its catalog, its databases and its tables as IAM RESOURCES, and
//! every call that acts on one is authorized against the policies in force on the principal
//! that signed it: a client that holds `glue:GetTable` on one database's tables may read there
//! and nowhere else, and a client holding no grant for `glue:CreateTable` cannot register a
//! table however much else it may do. That is what an execution role of another service is
//! scoped with — a Spark driver reaching this catalog through the metastore client is just such
//! a caller — so the refusal is a property of the (principal, action, resource) triple rather
//! than of anything the catalog holds.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for (`sts_store::resolve_caller`, the same record `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for the
//!    credentials it signed with), else the IAM user that owns the long-term key, else the
//!    account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's own
//!    accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation of
//!    those documents against `(action, resource)`, so this service and every other one handed
//!    the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow` about the
//!    request grants it, and with neither the request falls to IAM's default deny.
//!
//! The refusal's own sentence is the shared one as well
//! ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), which is the
//! sentence AWS states such a refusal with: the principal that signed, the action it asked for,
//! the resource it addressed and which of IAM's rules withheld the grant. Each operation answers
//! it as its own `AccessDeniedException` ("Access to a resource was denied"), the code this
//! model declares for exactly that.
//!
//! ## The action and the resource
//!
//! The action is `glue:<OperationName>` — this service's own IAM action namespace
//! ([`IAM_ACTION_PREFIX`]) and the operation's own shape name — so one statement scoped to this
//! service's actions covers every operation of it and nothing outside it.
//!
//! The resource is what the REQUEST addresses, built from what the request carried rather than
//! from a stored record: the table a table call names ([`table_arn`]), the database a database
//! call names ([`database_arn`]), and the catalog itself for a call that addresses the catalog
//! as a whole ([`catalog_arn`]). Taking it from the request is what keeps the decision
//! independent of whether that resource exists — which is also what keeps a refused call from
//! telling an unauthorized caller whether it does.
//!
//! The Data Catalog's own resource-based policy is a separate question, answered where it
//! already is ([`super::catalog_refusal`], which decides whether a caller may address a
//! `CatalogId` at all); this module is the identity-based half and takes nothing of that one.
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
//! * **A principal this account holds no record of at all** — a session of a role provisioned
//!   outside this emulator among them: nothing can be SHOWN about what it may do, which is the
//!   same reading every other check of a role's permissions in this crate makes
//!   ([`super::role_policy_documents`]).
//! * **An engine carrying no IAM store**, where no principal and no policy can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use super::*;

/// The resource type a Glue DATABASE is addressed by in an ARN
/// (`arn:aws:glue:<region>:<account>:database/<name>`), which is also the segment the shared
/// resource table gives that kind ([`TaggedResourceKind::Database`]).
pub const DATABASE_ARN_RESOURCE_TYPE: &str = "database";

/// The resource type a Glue TABLE is addressed by in an ARN
/// (`arn:aws:glue:<region>:<account>:table/<database>/<name>`): a table is addressed under the
/// database that holds it, which is why a grant can be written for one database's tables alone.
pub const TABLE_ARN_RESOURCE_TYPE: &str = "table";

/// The ARN of the database `database` in the catalog `catalog_id`, as addressed in `region`.
#[must_use]
pub fn database_arn(region: &str, catalog_id: &str, database: &str) -> String {
    glue_arn_for_account(
        region,
        catalog_id,
        &format!("{DATABASE_ARN_RESOURCE_TYPE}/{database}"),
    )
}

/// The ARN of the table `table` of the database `database` in the catalog `catalog_id`, as
/// addressed in `region`.
#[must_use]
pub fn table_arn(region: &str, catalog_id: &str, database: &str, table: &str) -> String {
    glue_arn_for_account(
        region,
        catalog_id,
        &format!("{TABLE_ARN_RESOURCE_TYPE}/{database}/{table}"),
    )
}

/// Why IAM refuses the caller of this request `operation` on `resource`, as the sentence an
/// `AccessDeniedException` carries — or `None` when it permits the call (and for every caller
/// this gate does not bound, see the module docs).
///
/// `operation` is the operation's own name ([`smithy_server_runtime::Operation::NAME`]) and
/// `resource` the ARN the request NAMES ([`table_arn`], [`database_arn`], [`catalog_arn`]). Call
/// it BEFORE the store is read or written, and with the ARN built from what the request carried
/// rather than from a stored record.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service takes this
/// decision too.
pub fn identity_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    let action = format!("{IAM_ACTION_PREFIX}{operation}");
    let caller = caller(engine, ctx);
    let reason = refused(&caller, &action, resource)?;
    Some(iam::not_authorized_by_identity_policy(
        &caller.arn,
        &action,
        resource,
        &reason,
    ))
}

/// Whether the documents in force on `caller` refuse `action` on `resource`, and the clause
/// that says which of IAM's rules withheld it.
///
/// The one decision this gate takes: an explicit `Deny` wins, otherwise an `Allow` about the
/// request grants it, with neither the request falls to IAM's default deny, and a permissions
/// boundary caps whatever was granted.
fn refused(caller: &Caller, action: &str, resource: &str) -> Option<String> {
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no such
    // element, so the decision below rests on the action and the resource.
    let request = iam::Request::new(action, resource, &caller.principals);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    // Taken FIRST, so an explicit `Deny` in a document that CAN be read refuses the request
    // whatever else the principal carries — including a grant this emulator cannot read, which
    // only lifts the DEFAULT deny of a request no readable statement allows.
    if identity_said == Decision::Denied {
        return Some(iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string());
    }
    let granted =
        iam::is_allowed(identity_said, Decision::NotStated) || caller.identity_is_unbounded();
    if !granted {
        return Some(iam::no_identity_policy_allows(action));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has.
    if let Some(boundary) = &caller.boundary {
        if iam::identity_decision(::std::slice::from_ref(boundary), &request) != Decision::Allowed {
            return Some(iam::no_permissions_boundary_allows(action));
        }
    }
    None
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — which IAM entity's policies
    /// are the ones in force is read off it.
    arn: String,
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the IAM role
    /// behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose document
    /// this emulator holds.
    boundary: Option<String>,
    /// Set when this request presents the account's own published credential, which holds no IAM
    /// policies (see the module docs).
    account_credential: bool,
    /// Set when nothing can be SHOWN about what the principal may do: it carries a policy whose
    /// document the IAM store does not hold (an AWS-managed policy), or this account holds no
    /// record of the principal at all (see the module docs).
    nothing_shown: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no readable
    /// document DENIES the action.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.nothing_shown
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
            self.nothing_shown = !policies.uncarried.is_empty();
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
        // it presented is NOT quoted back, and in the assembled emulator the layer that
        // authenticates a request refuses an expired credential before any handler runs.
        Err(_) => {
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                principals: Vec::new(),
                documents: Vec::new(),
                boundary: None,
                account_credential: false,
                nothing_shown: false,
            };
        }
    };
    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        documents: Vec::new(),
        boundary: None,
        nothing_shown: false,
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
        // not. A role this account holds no record of is one nothing can be shown about.
        match store.roles.get(&iam_store::name_key(role_name)) {
            Some(held) => {
                caller.principals.push(held.arn.clone());
                caller.read_policies_of(&store, &held.arn.clone());
            }
            None => caller.nothing_shown = true,
        }
    } else if ::service_common::iam_role::user_name(&caller.arn).is_some() {
        // An IAM user's permissions are its own, under the ARN it was resolved by.
        let user_arn = caller.arn.clone();
        caller.read_policies_of(&store, &user_arn);
    }
    caller
}

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the resource a decision is taken on is the one the request addresses, spelled as
    /// AWS addresses a Data Catalog resource — the catalog itself, a database under it, and a
    /// table under its database.
    #[test]
    fn a_catalog_resource_is_addressed_as_aws_addresses_it() {
        let account = ::engine::emulator_account_id();
        let region = "eu-west-1";
        // (case, the ARN this module builds, the ARN AWS publishes for that resource)
        let rows = [
            (
                "the catalog itself",
                catalog_arn(region, account),
                format!("arn:aws:glue:{region}:{account}:catalog"),
            ),
            (
                "a database",
                database_arn(region, account, "sales"),
                format!("arn:aws:glue:{region}:{account}:database/sales"),
            ),
            (
                "a table under it",
                table_arn(region, account, "sales", "orders"),
                format!("arn:aws:glue:{region}:{account}:table/sales/orders"),
            ),
            (
                "a table of another account's catalog",
                table_arn(region, "111122223333", "shared", "events"),
                format!("arn:aws:glue:{region}:111122223333:table/shared/events"),
            ),
        ];
        for (case, actual, expected) in rows {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: one statement scoped to this service's actions covers every operation of it and
    /// nothing outside it, and a statement written for one database's tables grants nothing
    /// about another's.
    #[test]
    fn the_decision_is_taken_on_the_action_and_the_resource_the_request_names() {
        let account = ::engine::emulator_account_id();
        let region = "us-east-1";
        let documents = vec![format!(
            r#"{{"Statement":[{{"Effect":"Allow","Action":["glue:GetTable","glue:GetDatabase"],
               "Resource":"arn:aws:glue:{region}:{account}:table/sales/*"}}]}}"#
        )];
        let decision = |action: &str, resource: &str| {
            iam::identity_decision(&documents, &iam::Request::new(action, resource, &[]))
        };
        // (case, the action, the resource, whether the statement allows it)
        let rows = [
            (
                "a granted action on a covered table",
                "glue:GetTable",
                table_arn(region, account, "sales", "orders"),
                true,
            ),
            (
                "the same action on a table of another database",
                "glue:GetTable",
                table_arn(region, account, "finance", "orders"),
                false,
            ),
            (
                "an action the statement does not name",
                "glue:CreateTable",
                table_arn(region, account, "sales", "orders"),
                false,
            ),
            (
                "an action of another service",
                "s3:GetObject",
                table_arn(region, account, "sales", "orders"),
                false,
            ),
        ];
        for (case, action, resource, allowed) in rows {
            let reached = decision(action, &resource);
            assert_eq!(
                reached == Decision::Allowed,
                allowed,
                "case {case:?}: expected allowed={allowed} for {action} on {resource}, got \
                 {reached:?}"
            );
        }
    }
}
