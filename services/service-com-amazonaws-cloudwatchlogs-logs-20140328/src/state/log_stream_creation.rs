// SPDX-License-Identifier: CC0-1.0

//! Serving the CREATION of a log stream this service is asked for by another AWS service's
//! LOG DRIVER, on a ROLE's behalf.
//!
//! A compute service that delivers a workload's output to this service does not sign a
//! request of its own: its agent creates the stream the output goes to AS a role of the
//! caller's account — an Amazon ECS task execution role, say — before the workload exists.
//! That creation is still this service's to answer, because the two questions it turns on
//! are facts about this service's own resources, and they are the very two
//! `CreateLogStream` itself answers:
//!
//! 1. **may this principal create it** — `logs:CreateLogStream` ("Creates a log stream for
//!    the specified log group", `CreateLogStream`) on the stream the driver names, decided
//!    over the documents in force on the role by the same shared evaluation this service's
//!    signed requests go through ([`super::identity_authorization`]);
//! 2. **is there a group to create it in** — `CreateLogStream` answers
//!    `ResourceNotFoundException` for a group this account does not hold, in this service's
//!    own words ([`super::resource_not_found_message`]), and that is what a driver
//!    configured to deliver into a group nobody created finds.
//!
//! The order is IAM's: authorization is decided before the resource is looked up, so a
//! principal that may not create the stream is told that and nothing about what the account
//! holds.
//!
//! Nothing is withheld on evidence this emulator does not have. An engine carrying no IAM
//! store, a role that store holds no record for, and a role carrying an attachment whose
//! document this account does not hold (an entry of AWS's own catalog) each show nothing
//! about the role, so the creation is not refused on them — the same rule every other gate
//! over a role's documents in this tree keeps. A creation made with no principal at all is
//! not decided either: a caller that names no role states nothing for a policy to be read
//! against.
//!
//! A READ throughout: the decision consults the IAM store and this one and writes to
//! neither — the stream itself is created by the driver's own `CreateLogStream` call, which
//! is what a client then reads back — so deciding a creation changes nothing a later caller
//! of either service sees.
//!
//! LOCK ORDER: the IAM store's lock is taken with no guard of this service's store held, and
//! each guard hands back owned values — the contract that keeps a lock cycle between the
//! stores unreachable. A caller reaches this with no lock of its own held.

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The operation a log driver creates the stream it delivers to through, which is the action
/// IAM publishes for it ("Actions defined by Amazon CloudWatch Logs", IAM Service
/// Authorization Reference).
pub const CREATE_LOG_STREAM_ACTION: &str = "CreateLogStream";

/// Why this service did not serve a log driver's stream creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamCreationRefusal {
    /// No policy in force on the principal permits the creation: the sentence AWS refuses it
    /// with, under the class a refused call carries.
    NotAuthorized {
        /// The authorization refusal itself.
        message: String,
    },
    /// There is no log group to create the stream in — this account holds none by that name
    /// — carrying this service's own sentence about it.
    NotFound {
        /// Why nothing was created, in this service's own words.
        message: String,
    },
}

/// The principal a log driver's stream creation is made as.
#[derive(Debug, Clone, Copy)]
pub struct DeliveryPrincipal<'a> {
    /// The IAM ROLE whose identity-based documents are in force on the creation.
    pub role_arn: &'a str,
    /// What a refusal NAMES as the principal: the ARN of the SESSION the delivering service
    /// assumed that role in, which is what AWS reports for a call made under one. `None`
    /// names the role itself, for a caller that holds no session of its own.
    pub session_arn: Option<&'a str>,
}

impl DeliveryPrincipal<'_> {
    /// The ARN a refusal names as the principal (see [`Self::session_arn`]).
    fn reported_arn(&self) -> &str {
        self.session_arn.unwrap_or(self.role_arn)
    }
}

/// Serve the creation of the stream `log_stream` of the group `log_group` in `region`, made
/// on behalf of `principal`.
///
/// `Ok(())` for a creation this service would serve — and for every caller this gate does
/// not bound (see the module docs) — and the refusal otherwise, which the delivering service
/// reports in its own vocabulary. The stream is not created here: the driver creates it
/// through `CreateLogStream`, so what this answers is whether that call would be served at
/// all, which is what decides whether the workload the delivery is for can start.
pub fn serve_log_stream_creation<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    log_group: &str,
    log_stream: &str,
    principal: Option<DeliveryPrincipal<'_>>,
) -> Result<(), StreamCreationRefusal> {
    if let Some(refusal) = withheld_creation(engine, region, log_group, log_stream, principal) {
        return Err(refusal);
    }
    let Some(state) = crate::state::service_state(engine) else {
        // No store of this service to ask: nothing is known about the group, so the
        // creation is not refused on it.
        return Ok(());
    };
    let store = state.lock();
    if !store.log_groups.contains_key(log_group) {
        // The sentence `CreateLogStream` itself refuses a group this account does not hold
        // with, so the driver is told exactly what its own call would have been told.
        return Err(StreamCreationRefusal::NotFound {
            message: super::resource_not_found_message("log group"),
        });
    }
    Ok(())
}

/// Why no policy in force on `principal` permits the creation, or `None` when one does — or
/// when nothing of the kind can be shown (see the module docs).
fn withheld_creation<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    log_group: &str,
    log_stream: &str,
    principal: Option<DeliveryPrincipal<'_>>,
) -> Option<StreamCreationRefusal> {
    let principal = principal?;
    let role_arn = principal.role_arn;
    let iam = iam_store::service_state(engine)?;
    let (documents, boundary) = {
        let store = iam.lock();
        // A role this account's IAM holds no record for may belong to another account, so
        // its documents are not this store's to read.
        store.roles.values().find(|role| role.arn == role_arn)?;
        let policies = store.principal_policies(role_arn)?;
        // An attachment whose document this account does not hold — an entry of AWS's own
        // catalog, such as the managed policy AWS publishes for this very delivery — may be
        // the policy that carries the grant, so nothing is withheld on it.
        if !policies.uncarried.is_empty() {
            return None;
        }
        (
            policies.documents,
            store.permissions_boundary_document(role_arn),
        )
    };
    // The RESOURCE the creation addresses: the stream itself, which is what a grant written
    // for a log driver names (the group's ARN with the trailing wildcard covers it).
    let resource = super::log_stream_arn(region, log_group, log_stream);
    let action = super::identity_authorization::action_for(CREATE_LOG_STREAM_ACTION);
    // An identity-based document states no `Principal` element, so the decision rests on
    // the action and the resource the creation addressed.
    let request = iam::Request::new(&action, &resource, &[]);
    let identity = iam::identity_decision(&documents, &request);
    // IAM's own evaluation order, the order this service's gate for a signed request takes
    // too ([`super::identity_authorization`]), so one group and one role are decided alike
    // however the creation is asked for.
    let reason = if identity == Decision::Denied {
        iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
    } else if identity != Decision::Allowed {
        iam::no_identity_policy_allows(&action)
    } else {
        match boundary.as_ref().map_or(Decision::Allowed, |document| {
            iam::identity_decision(::std::slice::from_ref(document), &request)
        }) {
            Decision::Allowed => return None,
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => iam::no_permissions_boundary_allows(&action),
        }
    };
    // The refusal is the security-relevant event of this creation, so it is attributable in
    // the emulator's audit log the way this service's own gate logs a refused request: who
    // the creation was made as, what it asked for, and the outcome. No policy document is
    // logged, and no log event is read.
    ::tracing::info!(
        target: "emulator::audit",
        audit = true,
        operation = "ServeLogStreamCreation",
        outcome = "refused_access_denied",
        principal = %principal.reported_arn(),
        action = %action,
        resource = %resource,
        explicit_deny = identity == Decision::Denied,
        "no policy in force permits this role to create this log stream"
    );
    Some(StreamCreationRefusal::NotAuthorized {
        message: iam::not_authorized_by_identity_policy(
            principal.reported_arn(),
            &action,
            &resource,
            &reason,
        ),
    })
}
