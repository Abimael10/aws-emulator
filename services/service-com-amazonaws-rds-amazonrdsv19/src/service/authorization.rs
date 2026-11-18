// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this front door perform the action it names — the decision
//! taken ahead of EVERY operation, before any handler reaches the store.
//!
//! AWS decides authorization at the IAM layer, in front of the service: a caller whose
//! identity-based policies do not allow the action it asked for — or that an explicit `Deny`
//! covers — is refused rather than served, and the call reads nothing and changes nothing.
//! This model declares no error shape for that decision (no operation of it lists an access
//! error, `smithy_query(query="operation", operation="DescribeDBInstances")` reports
//! `DBInstanceNotFoundFault` alone), because the refusal belongs to the layer in front of a
//! service's own code: on this Query wire it is the error code [`ACCESS_DENIED`] at HTTP
//! 403, which is the code a client branches on and which this service already answers
//! through [`crate::scaffolded::Scaffolded::access_denied`].
//!
//! The decision is a fact about the REQUEST's credential and the policies in force at the
//! instant of the call, not about what any one operation would do with its input — which is
//! why it is taken once here, for every request this front door serves, rather than per
//! handler.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for (`sts_store::resolve_caller`, the same record `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for
//!    the credentials it signed with), else the IAM user that holds the long-term key, else
//!    the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives — so a document `iam:PutRolePolicy` wrote takes effect on
//!    the next request and deleting it lifts it again;
//! 3. **the decision and its wording** — [`::service_common::iam_authorization`], the tree's
//!    ONE evaluation of policy documents against `(action, resource)` and the one sentence
//!    every service in this tree refuses an unauthorized request with, so two services handed
//!    the same documents answer alike and word the refusal alike.
//!
//! Only the store LOOKUPS live here; neither the decision nor the sentence does.
//!
//! ## The action, and the resource it is judged over
//!
//! The action is `rds:<OperationName>` ("Actions defined by Amazon RDS", IAM Service
//! Authorization Reference), where the prefix is this service's `arnNamespace`
//! (`smithy_query(query="service_info")`) — so one statement scoped to this service's actions
//! covers every operation of it and nothing outside it.
//!
//! The resource is [`EVERY_RESOURCE`], the resource IAM writes for a request that addresses
//! no single resource of its own: this gate does not resolve WHICH resource an operation
//! acts on, because that differs per operation and a request names its resources by
//! identifier rather than by ARN. So the question asked here is the weaker one
//! `iam_authorization` states for a gate that does not know the resource ARN
//! ([`::service_common::iam_authorization::identity_may_allow_action`]): whether any readable
//! `Allow` covers the action AT ALL. A principal whose documents say nothing about the action
//! is refused — which is the decision AWS takes for an action it judges over every resource,
//! and for one judged over a resource ARN alike — while a grant scoped to particular ARNs
//! reaches the operation, because refusing it on the resource element this gate did not read
//! would deny calls AWS permits. The DENY question is decided the same way round: a `Deny`
//! about the action over every resource refuses the request here.
//!
//! A refusal therefore names no resource, which is also how AWS words the refusal of an
//! action it judges over every resource ([`Denied::message`]).
//!
//! This service's resource-based policies are not part of this decision: the documents an
//! Amazon RDS resource carries govern what ANOTHER principal (an export's role, an engine's
//! associated role) may do to the objects of another service, and every request this gate
//! judges is signed by a credential whose grants are its identity's.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed, which is also what keeps every in-process call
//!   another emulated service makes to this one served.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the account's store may hold no document
//!   for it; refusing on a grant this emulator cannot see would deny access AWS permits. An
//!   explicit `Deny` in a document that CAN be read still refuses, and a permissions boundary
//!   the store DOES hold still caps the grant.
//! * **A session of a role this account's IAM holds no record of.** An execution role is
//!   routinely provisioned outside the client that uses it, so a session may speak for a role
//!   whose policies this emulator never saw; nothing can be shown about what such a role may
//!   do, and refusing on grants it cannot read would deny access AWS permits.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)
//!
//! The decision is taken in front of everything the front door does for a request, the audit
//! leg included ([`crate::service::management_events`]): a refused call performed nothing, so
//! there is no served call to record, exactly as a call this emulator has not implemented
//! records nothing.
//!
//! LOCK ORDER: the STS registry's lock and then the IAM store's are taken, each in turn, and
//! BEFORE this service's own store lock — the gate runs in front of every handler, so no
//! handler holds this service's guard while the decision is taken. Neither of those services
//! reads this one's state.
//!
//! Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
//! session expiries the clock has already made due; that write is owed at the same instant
//! whether or not anybody called, which is why a REPORTING operation of this service takes
//! this decision too.

use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::ProtocolError;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::{Refused, Service};

/// This service's IAM action prefix: an action is `rds:<OperationName>`, the prefix being
/// this service's `arnNamespace` (`smithy_query(query="service_info")`), which is also its
/// `sigv4Name` and endpoint prefix.
pub const ACTION_PREFIX: &str = "rds";

/// The resource a request this gate does not resolve a resource ARN for is judged over, as
/// IAM writes it for such a request: every resource.
pub const EVERY_RESOURCE: &str = "*";

/// The error code this service refuses an unauthorized caller with, which is what a client
/// branches on. A CLIENT fault, so the Query envelope reports it as `<Type>Sender</Type>`
/// with HTTP 403 — the same code [`crate::scaffolded::Scaffolded::AccessDenied`] renders.
pub const ACCESS_DENIED: &str = "AccessDenied";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// One refused request: who was refused, which action, and which of IAM's rules withheld the
/// grant.
#[derive(Debug, Clone)]
pub struct Denied {
    /// The principal the request speaks for, as `sts:GetCallerIdentity` reports it.
    principal: String,
    /// The IAM action the request asked for.
    action: String,
    /// Why the grant was withheld, as the refusal's closing clause words it — one of the
    /// clauses `services/common` states.
    reason: String,
}

impl Denied {
    /// The refusal's message: the principal the credential speaks for, the action the request
    /// asked for, and which of IAM's rules withheld the grant.
    ///
    /// No resource is named, because the decision was not taken on one (see the module docs);
    /// that is why this is not
    /// [`::service_common::iam_authorization::not_authorized_by_identity_policy`], whose own
    /// documentation says a call addressing no single resource is outside that sentence.
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "User: {} is not authorized to perform: {} {}",
            self.principal, self.action, self.reason
        )
    }
}

/// This service's [`ACCESS_DENIED`] response for a request whose caller the IAM policies in
/// force do not permit to perform the action it named, or `None` when they do.
///
/// The action asked about is the operation's own name, taken from the request's routing key
/// and only when the routing table SERVES that key: so it is always a modelled operation's
/// name, never an arbitrary value the body carried, and a request naming no operation of this
/// service still reaches the routing failure rather than an authorization decision about a
/// name nothing serves.
///
/// The code and the sentence are this service's ([`ACCESS_DENIED`], [`Denied::message`]); the
/// status is the 403 the framework's own pre-dispatch renderer answers an authorization
/// refusal with, and the envelope is the protocol's, closed by this service's own
/// `<RequestId>` writer — so this refusal is rendered exactly like every other error of this
/// service.
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    svc: &dyn Service,
    req: &RtRequest,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Option<RtResponse> {
    let operation = svc.definition().protocol().routing_key(req).ok()?;
    svc.definition().routing_table().get(operation)?;
    let refusal = authorize(engine, ctx, operation).err()?;
    Some(super::query_error_envelope::with_request_id(
        ::smithy_server_runtime::protocol_error_response_with(
            ::smithy_server_runtime::PreDispatchProtocol::Query { ec2: false },
            ProtocolError::Unauthenticated {
                code: ACCESS_DENIED,
                message: ACCESS_DENIED,
            },
            Some(Refused {
                code: ACCESS_DENIED.to_string(),
                message: refusal.message(),
            }),
        ),
    ))
}

/// Whether IAM lets this request perform the action of `operation` — the operation's own
/// name, which is the routing key the request named.
///
/// `Ok(())` for a request IAM permits and for every caller this gate does not bound (see the
/// module docs); `Err` carries the refusal this service answers as [`ACCESS_DENIED`].
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
) -> Result<(), Denied> {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let caller = caller(engine, ctx);
    let documents = iam::prepare_documents(&caller.documents);
    let boundary = caller
        .boundary
        .as_ref()
        .map(|boundary| iam::prepare_documents(::std::slice::from_ref(boundary)));
    decide(&caller, &documents, boundary.as_deref(), &action)
}

/// IAM's decision about `action`, over every resource.
fn decide(
    caller: &Caller,
    documents: &[iam::PreparedDocument],
    boundary: Option<&[iam::PreparedDocument]>,
    action: &str,
) -> Result<(), Denied> {
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no such
    // element, so the decision below rests on the action and the resource.
    let request = iam::Request::new(action, EVERY_RESOURCE, &caller.principals);
    let identity_said = iam::identity_decision_prepared(documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return Err(caller.refused(action, iam::IDENTITY_POLICY_EXPLICIT_DENY));
    }
    let granted = iam::is_allowed(identity_said, Decision::NotStated)
        || iam::identity_may_allow_action(documents, action)
        || caller.identity_is_unbounded();
    if !granted {
        return Err(caller.refused(action, &iam::no_identity_policy_allows(action)));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    if let Some(boundary) = boundary {
        let reason = match iam::identity_decision_prepared(boundary, &request) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the identity-based
            // policies gave.
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => {
                if iam::identity_may_allow_action(boundary, action) {
                    return Ok(());
                }
                iam::no_permissions_boundary_allows(action)
            }
        };
        return Err(caller.refused(action, &reason));
    }
    Ok(())
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
    /// Set when the principal is a session of a role this account's IAM holds no record of,
    /// so nothing can be read about what it may do (see the module docs).
    unheld_role: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no IAM
    /// policies, a principal carrying a grant this emulator cannot read, or a session of a
    /// role this emulator holds no record of.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment || self.unheld_role
    }

    /// This caller's refusal of `action`, closed by `reason`.
    fn refused(&self, action: &str, reason: &str) -> Denied {
        Denied {
            principal: self.arn.clone(),
            action: action.to_string(),
            reason: reason.to_string(),
        }
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
                unheld_role: false,
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
        unheld_role: false,
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
        match store.roles.get(&iam_store::name_key(role_name)) {
            Some(held) => {
                caller.principals.push(held.arn.clone());
                caller.read_policies_of(&store, &held.arn.clone());
            }
            // A role this account's IAM holds no record of: its documents are not readable, so
            // the grant question cannot be answered about it at all (see the module docs).
            None => caller.unheld_role = true,
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

    /// A principal holding `documents` and nothing else — no boundary, not the account's own
    /// credential, no unreadable attachment.
    fn principal(documents: &[&str]) -> Caller {
        let arn = format!(
            "arn:aws:sts::{}:assumed-role/probe/session",
            ::engine::emulator_account_id()
        );
        Caller {
            principals: vec![arn.clone()],
            arn,
            documents: documents.iter().map(|held| (*held).to_string()).collect(),
            boundary: None,
            account_credential: false,
            unreadable_attachment: false,
            unheld_role: false,
        }
    }

    /// The decision over `documents` about `action`, as the gate takes it.
    fn decided(documents: &[&str], action: &str) -> Result<(), Denied> {
        let caller = principal(documents);
        let prepared = iam::prepare_documents(&caller.documents);
        decide(&caller, &prepared, None, action)
    }

    /// RULE: a caller nothing readable bounds — the account's own published credential, a
    /// principal carrying an attachment whose document this emulator does not hold, and a
    /// session of a role this account's IAM holds no record of — is served, because refusing
    /// on grants that cannot be read would deny access AWS permits; an explicit `Deny` in a
    /// document that CAN be read still refuses every one of them.
    #[test]
    fn a_caller_nothing_readable_bounds_is_served_unless_a_readable_document_denies() {
        let action = "rds:DescribeDBInstances";
        let deny = format!(
            r#"{{"Statement":[{{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
        );
        // (case, how the caller comes to be unbounded)
        type Unbind = fn(&mut Caller);
        let rows: [(&str, Unbind); 3] = [
            ("the account's own published credential", |caller| {
                caller.account_credential = true;
            }),
            ("an attachment whose document is not held", |caller| {
                caller.unreadable_attachment = true;
            }),
            ("a session of a role IAM holds no record of", |caller| {
                caller.unheld_role = true;
            }),
        ];
        for (case, unbind) in rows {
            for (documents, permitted) in [(Vec::new(), true), (vec![deny.as_str()], false)] {
                let mut caller = principal(&documents);
                unbind(&mut caller);
                let prepared = iam::prepare_documents(&caller.documents);
                let answered = decide(&caller, &prepared, None, action);
                assert_eq!(
                    answered.is_ok(),
                    permitted,
                    "case {case:?} with {} document(s): expected permitted={permitted}, got \
                     {answered:?}",
                    documents.len()
                );
            }
        }
    }

    /// RULE: the action asked about is `rds:<OperationName>`, so a statement scoped to this
    /// service's actions covers the operations of it and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"rds:*","Resource":"*"}]}"#;
        for operation in [
            "DescribeDBInstances",
            "DescribeDBClusters",
            "CreateDBInstance",
            "ListTagsForResource",
        ] {
            let action = format!("{ACTION_PREFIX}:{operation}");
            assert!(
                decided(&[document], &action).is_ok(),
                "case {operation:?}: expected rds:* to allow {action}"
            );
        }
        // Another service's actions are not this service's: a document granting them alone
        // allows nothing here.
        let outside = r#"{"Statement":[{"Effect":"Allow","Action":"redshift:*","Resource":"*"}]}"#;
        assert!(
            decided(&[outside], "rds:DescribeDBInstances").is_err(),
            "case \"another service\": expected a grant outside this service to allow nothing"
        );
    }

    /// RULE: an explicit `Deny` about the request WINS over every `Allow` beside it, whatever
    /// width either is written at and in whichever order they are written — and the sentence
    /// names that document as the rule that refused.
    #[test]
    fn an_explicit_deny_refuses_whatever_a_grant_beside_it_says() {
        let action = "rds:DescribeDBInstances";
        // (case, the documents in force, whether the request is permitted)
        let rows: &[(&str, Vec<String>, bool)] = &[
            (
                "a wildcard allow alone",
                vec![r#"{"Statement":[{"Effect":"Allow","Action":"rds:*","Resource":"*"}]}"#
                    .to_string()],
                true,
            ),
            (
                "a wildcard allow with the action denied after it",
                vec![format!(
                    r#"{{"Statement":[{{"Effect":"Allow","Action":"rds:*","Resource":"*"}},
                        {{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
                )],
                false,
            ),
            (
                "the action denied BEFORE a wildcard allow",
                vec![format!(
                    r#"{{"Statement":[{{"Effect":"Deny","Action":"{action}","Resource":"*"}},
                        {{"Effect":"Allow","Action":"rds:*","Resource":"*"}}]}}"#
                )],
                false,
            ),
            (
                "the deny in a SECOND document beside an allowing one",
                vec![
                    r#"{"Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#.to_string(),
                    format!(
                        r#"{{"Statement":[{{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
                    ),
                ],
                false,
            ),
            (
                "no document at all",
                Vec::new(),
                false,
            ),
        ];
        for (case, documents, permitted) in rows {
            let held: Vec<&str> = documents.iter().map(String::as_str).collect();
            let answered = decided(&held, action);
            match (permitted, answered) {
                (true, Ok(())) => {}
                (false, Err(denial)) => {
                    let message = denial.message();
                    assert!(
                        message.contains(action) && !message.contains("on resource"),
                        "case {case:?}: expected the action named and no resource, got \
                         {message:?}"
                    );
                }
                (expected, answered) => {
                    panic!("case {case:?}: expected permitted={expected}, got {answered:?}")
                }
            }
        }
    }

    /// RULE: the refusal names the principal, the action and which of IAM's rules withheld
    /// the grant — every part taken from the request and the stored policies — and names no
    /// resource, the decision having been taken over every resource.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_rule() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/restricted/reporter",
            ::engine::emulator_account_id()
        );
        let action = format!("{ACTION_PREFIX}:DescribeDBInstances");
        // (case, the clause the decision closed with)
        let rows: &[(&str, String)] = &[
            (
                "an explicit deny",
                iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
            ),
            (
                "no statement at all",
                iam::no_identity_policy_allows(&action),
            ),
            (
                "a permissions boundary that allows nothing",
                iam::no_permissions_boundary_allows(&action),
            ),
        ];
        for (case, reason) in rows {
            let message = Denied {
                principal: principal.clone(),
                action: action.clone(),
                reason: reason.clone(),
            }
            .message();
            assert_eq!(
                message,
                format!("User: {principal} is not authorized to perform: {action} {reason}"),
                "case {case:?}: expected IAM's wording, got {message:?}"
            );
        }
    }

    /// RULE: a grant scoped to particular resource ARNs reaches the operation — this gate
    /// resolves no resource ARN, so refusing such a grant would deny a call AWS permits —
    /// while a document that says nothing about the action still refuses.
    #[test]
    fn a_resource_scoped_grant_of_the_action_reaches_the_operation() {
        let instance = format!(
            "arn:aws:rds:us-east-1:{}:db:orders",
            ::engine::emulator_account_id()
        );
        let scoped = format!(
            r#"{{"Statement":[{{"Effect":"Allow","Action":"rds:DescribeDBInstances","Resource":"{instance}"}}]}}"#
        );
        assert!(
            decided(&[scoped.as_str()], "rds:DescribeDBInstances").is_ok(),
            "case \"a grant scoped to one instance\": expected the read to reach the operation"
        );
        assert!(
            decided(&[scoped.as_str()], "rds:DescribeDBClusters").is_err(),
            "case \"another action of the service\": expected no grant of an action the \
             document says nothing about"
        );
    }
}
