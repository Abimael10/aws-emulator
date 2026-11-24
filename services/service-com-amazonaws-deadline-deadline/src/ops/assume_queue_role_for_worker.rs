// SPDX-License-Identifier: CC0-1.0

//! @op AssumeQueueRoleForWorker
//!
//! "Allows a worker to assume a queue role" — the credentials a worker signs as while it
//! runs that queue's jobs. Two facts decide the answer, both read from stored state:
//!
//! * the worker has to be WORKING ON THAT QUEUE. The queue role is for the work the worker
//!   is running for the queue, so a worker the scheduler holds no open session of that
//!   queue for is refused the 409 this error class carries
//!   ([`crate::state::worker_not_working_on_queue`]); a session of another queue grants
//!   nothing here.
//! * what the credentials ARE is a session of the queue's own `roleArn`, minted through the
//!   token service's role-session mint ([`crate::state::vend_role_session`]), so a request
//!   signed with them is resolved as that role everywhere. A queue created without a role
//!   has none to vend, and the response's `credentials` member is OPTIONAL for exactly that
//!   case (it is `@required` on the sibling user- and read-role operations) — so such a
//!   worker is answered 200 carrying no credentials rather than refused.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `AssumeQueueRoleForWorker`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-deadline-deadline/smithy-model/com-amazonaws-deadline-deadline.json")
//! ```
//!
//! Allows a worker to assume a queue role.
//!
//! ## Input
//!
//! * `farmId` (FarmId, pattern ^farm-[0-9a-f]{32}$) **required** — The farm ID of the worker
//!   assuming the queue role.
//! * `fleetId` (FleetId, pattern ^fleet-[0-9a-f]{32}$) **required** — The fleet ID of the
//!   worker assuming the queue role.
//! * `queueId` (QueueId, pattern ^queue-[0-9a-f]{32}$) **required** — The queue ID of the
//!   worker assuming the queue role.
//! * `workerId` (WorkerId, pattern ^worker-[0-9a-f]{32}$) **required** — The worker ID of the
//!   worker assuming the queue role.
//!
//! ## Output
//!
//! * `credentials` (AwsCredentials) — The Amazon Web Services credentials for the role that
//!   the worker is assuming.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — You don't have permission to perform the action.
//! * `ConflictException` (HTTP 409) — Your request has conflicting operations. This can occur
//!   if you're trying to perform more than one operation on the same resource at the same
//!   time.
//! * `InternalServerErrorException` (HTTP 500) — Deadline Cloud can't process your request
//!   right now. Try again later.
//! * `ResourceNotFoundException` (HTTP 404) — The requested resource can't be found.
//! * `ThrottlingException` (HTTP 429) — Your request exceeded a request rate quota.
//! * `ValidationException` (HTTP 400) — The request isn't valid. This can occur if your
//!   request contains malformed JSON or unsupported characters.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `AssumeQueueRoleForWorkerError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-deadline-deadline.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `AssumeQueueRoleForWorkerError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.deadline#AssumeQueueRoleForWorker")]
#[derive(Debug)]
pub enum AssumeQueueRoleForWorkerErrorModeled {
    AccessDeniedException(AccessDeniedException),
    ConflictException(ConflictException),
    InternalServerErrorException(InternalServerErrorException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::assume_queue_role_for_worker::AssumeQueueRoleForWorkerError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type AssumeQueueRoleForWorkerError = crate::scaffolded::Scaffolded<AssumeQueueRoleForWorkerErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<AssumeQueueRoleForWorkerErrorModeled> for AssumeQueueRoleForWorkerError {
    fn from(modeled: AssumeQueueRoleForWorkerErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `AssumeQueueRoleForWorker` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct AssumeQueueRoleForWorker<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> AssumeQueueRoleForWorker<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for AssumeQueueRoleForWorker<Eng>
{
    type Input = AssumeQueueRoleForWorkerInput;
    type Output = AssumeQueueRoleForWorkerOutput;
    type Error = AssumeQueueRoleForWorkerError;

    const NAME: &'static str = "AssumeQueueRoleForWorker";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::ASSUME_QUEUE_ROLE_FOR_WORKER_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::ASSUME_QUEUE_ROLE_FOR_WORKER_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(
                AssumeQueueRoleForWorkerErrorModeled::InternalServerErrorException(
                    crate::state::state_unavailable(),
                )
                .into(),
            );
        };

        // Which role is to be vended, and whether this worker may hold it, is the whole of
        // what this call reads: the store guard is given up before the token service's own
        // stores are reached (see [`crate::state::vend_role_session`]).
        let role_arn = {
            let store = state.lock();
            // The worker is reached only through its own farm and fleet, so every level the
            // path names is reported as itself; the queue the query parameter names is
            // resolved under the same farm.
            if store.farm(&input.farm_id).is_none() {
                return Err(AssumeQueueRoleForWorkerErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::RESOURCE_TYPE_FARM, &input.farm_id),
                )
                .into());
            }
            if store.fleet(&input.farm_id, &input.fleet_id).is_none() {
                return Err(AssumeQueueRoleForWorkerErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::RESOURCE_TYPE_FLEET, &input.fleet_id),
                )
                .into());
            }
            if store
                .worker(&input.farm_id, &input.fleet_id, &input.worker_id)
                .is_none()
            {
                return Err(AssumeQueueRoleForWorkerErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::RESOURCE_TYPE_WORKER, &input.worker_id),
                )
                .into());
            }
            let Some(queue) = store.queue(&input.farm_id, &input.queue_id) else {
                return Err(AssumeQueueRoleForWorkerErrorModeled::ResourceNotFoundException(
                    crate::state::not_found(crate::state::RESOURCE_TYPE_QUEUE, &input.queue_id),
                )
                .into());
            };
            let role_arn = queue.role_arn.clone();
            if !store.worker_works_on_queue(&input.farm_id, &input.queue_id, &input.worker_id) {
                return Err(AssumeQueueRoleForWorkerErrorModeled::ConflictException(
                    crate::state::worker_not_working_on_queue(&input.worker_id, &input.queue_id),
                )
                .into());
            }
            role_arn
        };

        // A queue with no role has no credentials to vend: `credentials` is the one member
        // of this response and it is not `@required`, so the worker is told there is nothing
        // to assume rather than refused.
        let Some(role_arn) = role_arn else {
            return Ok(AssumeQueueRoleForWorkerOutput { credentials: None });
        };

        // The session is named after the worker that holds it, so the principal a call made
        // with these credentials reads as names the worker the queue role was vended to.
        let credentials = crate::state::vend_role_session(
            &*self.engine,
            ctx.credential_store(),
            ::std::time::SystemTime::now(),
            &role_arn,
            &input.worker_id,
        )
        .ok_or_else(|| {
            AssumeQueueRoleForWorkerError::from(
                AssumeQueueRoleForWorkerErrorModeled::InternalServerErrorException(
                    crate::state::state_unavailable(),
                ),
            )
        })?;

        Ok(AssumeQueueRoleForWorkerOutput {
            credentials: Some(credentials),
        })
    }
}
