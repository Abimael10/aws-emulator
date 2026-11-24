// SPDX-License-Identifier: CC0-1.0

//! @op UpdateRestoreTestingPlan
//!
//! Rewrites the restore testing plan `CreateRestoreTestingPlan` stored under the
//! name the request names, in the SAME record `GetRestoreTestingPlan` reports
//! ([`crate::state::RestoreTestingPlanRecord::apply_update`]), so every later
//! read of that plan reports the body this update submitted.
//!
//! "RestoreTestingPlanName cannot be updated after it is created" (the model's
//! doc below), so the name addresses the plan to rewrite rather than being part
//! of what is rewritten, and the identifiers minted from it — the ARN and the
//! creation instant — are what this answer and every later read still report.
//! A member the submitted body states nothing for keeps the value the plan
//! already held; a member it states replaces it, with the members the model
//! gives a default for filled in exactly as the create fills them
//! ([`crate::state::stored_restore_testing_recovery_point_selection`]).
//!
//! A name this account holds no plan under is the one not-found condition
//! ([`crate::state::restore_testing_plan_not_found`]), which every call that
//! addresses a plan by name shares, and the documented rules on the body are
//! `crate::state`'s ([`crate::state::rejected_restore_testing_plan_body`]),
//! shared with the create, so the two cannot drift.
//!
//! The updates of ONE plan are SERIALIZED. Rewriting a plan is not
//! instantaneous: a call takes the plan for the span this service spends
//! applying its update ([`crate::state::RESTORE_TESTING_PLAN_UPDATE_SPAN`]),
//! which is the window every update a caller sent without waiting for an answer
//! arrives in, and an update that finds the plan held is refused as a concurrent
//! one ([`crate::state::restore_testing_plan_concurrent_update`]). So of however
//! many updates of one plan are in flight together, one wins and rewrites it,
//! and what a later read reports is that winner's body — never a refused
//! racer's. The plan is taken and released under the store's lock, never held
//! across the span, so the racers are served beside one another rather than one
//! after another, and the guard at the foot of this file releases the plan
//! however the request ends.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `UpdateRestoreTestingPlan`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")
//! ```
//!
//! This request will send changes to your specified restore testing plan.
//! RestoreTestingPlanName cannot be updated after it is created. RecoveryPointSelection can
//! contain: Algorithm ExcludeVaults IncludeVaults RecoveryPointTypes SelectionWindowDays
//!
//! ## Input
//!
//! * `RestoreTestingPlan` (RestoreTestingPlanForUpdate) **required** — Specifies the body of
//!   a restore testing plan.
//! * `RestoreTestingPlanName` (String) **required** — The name of the restore testing plan
//!   name.
//!
//! ## Output
//!
//! * `CreationTime` (Timestamp) **required** — The time the resource testing plan was
//!   created.
//! * `RestoreTestingPlanArn` (String) **required** — Unique ARN (Amazon Resource Name) of the
//!   restore testing plan.
//! * `RestoreTestingPlanName` (String) **required** — The name cannot be changed after
//!   creation. The name consists of only alphanumeric characters and underscores. Maximum
//!   length is 50.
//! * `UpdateTime` (Timestamp) **required** — The time the update completed for the restore
//!   testing plan.
//!
//! ## Errors
//!
//! * `ConflictException` (HTTP 409) — Backup can't perform the action that you requested
//!   until it finishes performing a previous action. Try again later.
//! * `InvalidParameterValueException` (HTTP 400) — Indicates that something is wrong with a
//!   parameter's value. For example, the value is out of range.
//! * `MissingParameterValueException` (HTTP 400) — Indicates that a required parameter is
//!   missing.
//! * `ResourceNotFoundException` (HTTP 400) — A resource that is required for the action
//!   doesn't exist.
//! * `ServiceUnavailableException` (HTTP 500) — The request failed due to a temporary failure
//!   of the server.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `UpdateRestoreTestingPlanError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `UpdateRestoreTestingPlanError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.backup#UpdateRestoreTestingPlan")]
#[derive(Debug)]
pub enum UpdateRestoreTestingPlanErrorModeled {
    ConflictException(ConflictException),
    InvalidParameterValueException(InvalidParameterValueException),
    MissingParameterValueException(MissingParameterValueException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::update_restore_testing_plan::UpdateRestoreTestingPlanError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type UpdateRestoreTestingPlanError = crate::scaffolded::Scaffolded<UpdateRestoreTestingPlanErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<UpdateRestoreTestingPlanErrorModeled> for UpdateRestoreTestingPlanError {
    fn from(modeled: UpdateRestoreTestingPlanErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `UpdateRestoreTestingPlan` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct UpdateRestoreTestingPlan<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> UpdateRestoreTestingPlan<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for UpdateRestoreTestingPlan<Eng>
{
    type Input = UpdateRestoreTestingPlanInput;
    type Output = UpdateRestoreTestingPlanOutput;
    type Error = UpdateRestoreTestingPlanError;

    const NAME: &'static str = "UpdateRestoreTestingPlan";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_RESTORE_TESTING_PLAN_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_RESTORE_TESTING_PLAN_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // The plan's ARN was minted by the create and is stored with it, so this
        // write reports identifiers rather than recomputing any: nothing here is
        // read off the request's region.
        let _ = ctx;
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            UpdateRestoreTestingPlanErrorModeled::ServiceUnavailableException(
                ServiceUnavailableException {
                    message: Some("the backup vault store is unavailable".to_string()),
                    ..Default::default()
                },
            )
        })?;
        let name = input.restore_testing_plan_name;
        let body = input.restore_testing_plan;

        // The documented rules on a plan body are `crate::state`'s, shared with
        // the create, so the two cannot drift: the members this body states are
        // read by the same rules that admitted the create's. The name is not one
        // of them — it addresses the plan rather than being rewritten — so it is
        // not re-read here. Applied before the store is reached, so a refused
        // update rewrites nothing and never contends with anybody.
        if let Some(refusal) = crate::state::rejected_restore_testing_plan_body(
            None,
            body.schedule_expression.as_deref(),
            body.start_window_hours,
            body.recovery_point_selection
                .as_ref()
                .map_or(0, |selection| selection.selection_window_days),
        ) {
            return Err(
                UpdateRestoreTestingPlanErrorModeled::InvalidParameterValueException(refusal)
                    .into(),
            );
        }

        // Resolving the plan and taking it for this update are ONE critical
        // section, so of the updates of one plan in flight together exactly one
        // takes it and every other sees it held. The name addresses the plan to
        // rewrite; one the account holds no plan under is the not-found
        // condition every read of a plan answers.
        {
            let mut store = state.lock();
            store
                .stored_restore_testing_plan(&name)
                .map_err(UpdateRestoreTestingPlanErrorModeled::ResourceNotFoundException)?;
            if !store.claim_restore_testing_plan_update(&name) {
                return Err(UpdateRestoreTestingPlanErrorModeled::ConflictException(
                    crate::state::restore_testing_plan_concurrent_update(&name),
                )
                .into());
            }
        }
        let _applying = ApplyingUpdate {
            state: state.clone(),
            name: name.clone(),
        };

        // The span this service spends applying the update, spent with the
        // store's lock released so the updates racing for this plan are served
        // beside one another: each of them reaches the resolution above while
        // this one still holds the plan, instead of a write landing before
        // another caller's request can be taken in at all.
        ::tokio::time::sleep(crate::state::RESTORE_TESTING_PLAN_UPDATE_SPAN).await;

        // The rewrite, under the lock again, with the clock read there too: the
        // plan is stamped with the instant its new body landed rather than one
        // read before the span above. A plan deleted while this update was being
        // applied is no longer one this account holds, so the same not-found
        // refusal answers.
        let mut store = state.lock();
        let now = ::std::time::SystemTime::now();
        let record = store
            .stored_restore_testing_plan_mut(&name)
            .map_err(UpdateRestoreTestingPlanErrorModeled::ResourceNotFoundException)?;
        record.apply_update(body, now);
        Ok(UpdateRestoreTestingPlanOutput {
            restore_testing_plan_name: record.name.clone(),
            restore_testing_plan_arn: record.arn.clone(),
            // The create's instant, which an update leaves alone, beside the
            // instant this update stored the new body — the same instant a read
            // of the plan now reports as its last update.
            creation_time: crate::state::modelled_timestamp(record.creation_time),
            update_time: crate::state::modelled_timestamp(record.last_update_time),
        })
    }
}

/// Holds the plan an update took for as long as its request is being served, and
/// releases it however that request ends — answered, refused, or dropped because
/// its caller went away. Without that release a plan one update took would
/// refuse every later update of it, so the claim is tied to the request's
/// lifetime rather than to any one exit path.
struct ApplyingUpdate {
    state: ::engine::ServiceState<crate::state::ComAmazonawsBackupCryocontrollerusermanagerState>,
    name: String,
}

impl Drop for ApplyingUpdate {
    fn drop(&mut self) {
        self.state
            .lock()
            .release_restore_testing_plan_update(&self.name);
    }
}
