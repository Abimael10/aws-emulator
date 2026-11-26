// SPDX-License-Identifier: CC0-1.0

//! Whether the IAM ROLE a resource operation request named lets this service make the calls
//! the resource type's handler has to make.
//!
//! Cloud Control API does not carry a resource operation out as whoever signed the call: it
//! invokes the type's handler with the credentials of the role the request names — "the ARN
//! of the Identity and Access Management (IAM) role for Cloud Control API to use when
//! performing this resource operation. The role specified must have the permissions required
//! for this operation" (`CreateResource$RoleArn`). So a call the policies in force on that
//! role do not allow is a handler that FAILS rather than a call that happens, which is the
//! difference between a request that was refused downstream and one that wrote. The request
//! reaches a terminal `FAILED` event and the owning service's resource is left exactly as it
//! was, because the question is asked BEFORE the provider writes anything.
//!
//! The refusal is the OWNING service's own: its API would have refused that very call the
//! same way, and the authorization answer is a fact about the call rather than about the
//! resource model the handler judged. What it is reported AS follows from that: the handler
//! made the call and read that service's refusal of it, so the failure is an error the
//! downstream service generated which the handler maps to no verdict of its own
//! ([`NativeRefusal::Rejected`]) — reported exactly as that service's other errors are, under
//! the catch-all `GeneralServiceException` and in the handler's own sentence about the call
//! it was making, which the request's terminal event carries in place of any diagnosis the
//! downstream service stated. The sentence this verdict carries is the one that service
//! refuses such a call with
//! ([`authz::not_authorized_by_identity_policy`]), named after the SESSION of the role the
//! call was made as ([`::service_common::iam_role::assumed_role_session_arn`]) and stating
//! which of IAM's rules withheld the grant, so this service and the owning service cannot
//! word one decision two ways.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **who** — the `RoleArn` the request carried, kept on the request record
//!    ([`crate::state::RequestRecord::role_arn`]) so the work is judged by the role the call
//!    that started it named;
//! 2. **what it may do** — the identity-based documents the IAM store holds for that role
//!    (its inline policies and the default version of each managed policy attached to it),
//!    read through IAM's own `principal_policies` accessor so this service sees exactly what
//!    `CreateRole`/`PutRolePolicy`/`AttachRolePolicy` wrote, plus the permissions boundary
//!    that caps them;
//! 3. **which calls the work needs** — the owning service's API actions the type's provider
//!    publishes for the operation ([`super::NativeResourceProvider::handler_calls`]) on the
//!    resource that provider names an ARN for
//!    ([`super::NativeResourceProvider::resource_arn`]);
//! 4. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation
//!    of policy documents against `(action, resource)`, so this service and every other one
//!    handed the same documents decide alike.
//!
//! Nothing is refused on evidence this emulator does not have. Each of these shows nothing
//! about the role and lets the work go ahead: a request that named no role at all (which is
//! a call made with the caller's own credentials), an engine carrying no IAM store, a
//! `RoleArn` that store holds no role for (it may belong to another account), a role carrying
//! an attachment whose document this account does not hold — an AWS-managed policy might be
//! the very one that carries the grant — a type whose provider publishes no call for the
//! operation, and a resource it names no ARN for. What the documents DO state is decided in
//! IAM's own order: an explicit `Deny` refuses, a grant allows, and policies that state
//! nothing about the call withhold it, because the documents in force on a role are the
//! authority on what it may do.

use ::service_common::iam_authorization::{self as authz, Decision};

use super::{IamState, NativeRefusal, NativeResourceProvider};

/// The name of the SESSION this service's handler calls are made under.
///
/// A role is never acted AS: a service handed one assumes it and makes its calls as a
/// session of it, and the permissions in force on that session are the role's. So the
/// identity an authorization refusal of such a call is about is
/// `arn:<partition>:sts::<account>:assumed-role/<role name>/<this name>`, which is what the
/// sentence below names rather than the role's own ARN.
const SESSION_NAME: &str = "AWSCloudControlAPI";

/// The account's roles, as this service reads them: a handle to the IAM store in the SAME
/// shared engine, so a request is judged against the records IAM's own handlers wrote.
///
/// `None` means the engine carries no such store — a deployment with no IAM to ask, which is
/// evidence of nothing.
pub struct RolePermissions {
    /// The IAM store in the shared engine, where the account's roles and their policies live.
    iam: Option<::engine::ServiceState<IamState>>,
}

impl RolePermissions {
    /// The roles of the account THIS engine carries: call it with a handler's `&*self.engine`.
    pub fn for_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Self {
        Self {
            iam: ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(
                engine,
            ),
        }
    }

    /// Why the role `role_arn` may not be used to carry `operation` out on `identifier`, a
    /// resource of the type `provider` stands behind in `region` — or `None` when nothing of
    /// the kind can be shown (see the module header for the cases that show nothing).
    ///
    /// The calls the work needs are the provider's own
    /// ([`NativeResourceProvider::handler_calls`]), and every one of them has to be allowed:
    /// the handler makes them all, so a role without one of them cannot carry the operation
    /// out. The refusal is about the FIRST such call, which is the one the handler would have
    /// been refused on.
    ///
    /// It is the owning service's own refusal of the call ([`NativeRefusal::Rejected`]),
    /// stated in the sentence that service refuses such a call with: the session the call was
    /// made as, the action
    /// it asked for as that service words an action in its own refusals
    /// ([`authz::reported_action`]), the resource it was taken on, and which of IAM's rules
    /// withheld the grant — an explicit `Deny`, no statement at all, or the ceiling the
    /// permissions boundary sets.
    pub fn refusal(
        &self,
        role_arn: Option<&str>,
        provider: &dyn NativeResourceProvider,
        operation: &str,
        region: &str,
        identifier: &str,
    ) -> Option<NativeRefusal> {
        // A request that named no role is carried out with the caller's own credentials,
        // which this service never judges: the emulator verifies the signature of every
        // request, and what the SIGNING principal may do is not this decision.
        let role_arn = role_arn?;
        let calls = provider.handler_calls(operation);
        if calls.is_empty() {
            return None;
        }
        let resource = provider.resource_arn(region, identifier)?;
        let held = self.iam.as_ref()?.lock();
        // A role this account does not hold shows nothing: it may belong to another account.
        held.roles.values().find(|role| role.arn == role_arn)?;
        // The documents in force on the role. The collection is bounded by IAM's own quotas
        // on how many policies one role carries, so this walk is bounded by that store
        // rather than by a number of its own.
        let policies = held.principal_policies(role_arn)?;
        if !policies.uncarried.is_empty() {
            return None;
        }
        let documents = authz::prepare_documents(&policies.documents);
        // The ceiling attached to the role, read once for every call below. A boundary whose
        // document this account does not hold caps nothing, which is how IAM's own authorizer
        // reads one it cannot measure a call against.
        let boundary = held
            .permissions_boundary_document(role_arn)
            .map(|document| authz::PreparedDocument::read(&document));
        for call in calls {
            let action = authz::reported_action(call);
            let request = authz::Request::new(call, &resource, &[]);
            let reason = match authz::identity_decision_prepared(&documents, &request) {
                Decision::Denied => authz::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
                Decision::NotStated => authz::no_identity_policy_allows(&action),
                // The ceiling attached to the role caps what its own policies grant, so a
                // call the boundary does not allow is withheld however widely the role is
                // granted it.
                Decision::Allowed => {
                    let Some(boundary) = boundary.as_ref() else {
                        continue;
                    };
                    match authz::identity_decision_prepared(
                        ::std::slice::from_ref(boundary),
                        &request,
                    ) {
                        Decision::Allowed => continue,
                        Decision::Denied => authz::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
                        Decision::NotStated => authz::no_permissions_boundary_allows(&action),
                    }
                }
            };
            // The identity the call was made as is what its refusal is about.
            let principal =
                ::service_common::iam_role::assumed_role_session_arn(role_arn, SESSION_NAME);
            return Some(NativeRefusal::Rejected(
                authz::not_authorized_by_identity_policy(&principal, &action, &resource, &reason),
            ));
        }
        None
    }
}
