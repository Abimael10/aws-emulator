// SPDX-License-Identifier: CC0-1.0

//! @op DeleteRestoreTestingPlan
//!
//! Takes the restore testing plan the request names OUT of [`crate::state`], so
//! every later call that resolves that name takes the plan's not-found path
//! ([`crate::state::restore_testing_plan_not_found`]) and the name is free for a
//! create again — which mints another plan, with its own ARN. The removal is the
//! state module's one such step
//! ([`crate::state::ComAmazonawsBackupCryocontrollerusermanagerState::remove_restore_testing_plan`]),
//! so a plan cannot be taken out of the store two different ways.
//!
//! "Deletion can only successfully occur if all associated restore testing
//! selections are deleted first" (the model doc below), so a plan that still
//! holds one is left exactly as it was and the call is refused the condition the
//! plan's STATE makes — the modelled `InvalidRequestException`, as the backup
//! plan's own delete answers for the selections that bar it
//! ([`crate::state::restore_testing_plan_selections_exist`]).
//!
//! Otherwise the removal is IDEMPOTENT (the model marks the operation
//! `@idempotent` and gives it no not-found error to state one with): the call
//! states that the named plan is not to be held any more, which is already true
//! of a name this account holds no plan under — one nothing created, or one a
//! previous delete already took — so the second delete of a plan answers the same
//! empty success as the first.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `DeleteRestoreTestingPlan`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")
//! ```
//!
//! This request deletes the specified restore testing plan. Deletion can only successfully
//! occur if all associated restore testing selections are deleted first.
//!
//! ## Input
//!
//! * `RestoreTestingPlanName` (String) **required** — Required unique name of the restore
//!   testing plan you wish to delete.
//!
//! ## Errors
//!
//! * `InvalidRequestException` (HTTP 400) — Indicates that something is wrong with the input
//!   to the request. For example, a parameter is of the wrong type.
//! * `ServiceUnavailableException` (HTTP 500) — The request failed due to a temporary failure
//!   of the server.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `DeleteRestoreTestingPlanError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json");

#[derive(Debug, Clone, Default)]
pub struct DeleteRestoreTestingPlanOutput;

impl DeleteRestoreTestingPlanOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    )
    .with_http(::aws_smithy_schema::traits::HttpTrait::new(
        "DELETE",
        "/restore-testing/plans/{RestoreTestingPlanName}",
        Some(204u16),
    ));
}

pub static DELETE_RESTORE_TESTING_PLAN_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &DeleteRestoreTestingPlanOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeleteRestoreTestingPlanOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DeleteRestoreTestingPlanError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.backup#DeleteRestoreTestingPlan")]
#[derive(Debug)]
pub enum DeleteRestoreTestingPlanErrorModeled {
    InvalidRequestException(InvalidRequestException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::delete_restore_testing_plan::DeleteRestoreTestingPlanError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type DeleteRestoreTestingPlanError = crate::scaffolded::Scaffolded<DeleteRestoreTestingPlanErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<DeleteRestoreTestingPlanErrorModeled> for DeleteRestoreTestingPlanError {
    fn from(modeled: DeleteRestoreTestingPlanErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DeleteRestoreTestingPlan` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct DeleteRestoreTestingPlan<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DeleteRestoreTestingPlan<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DeleteRestoreTestingPlan<Eng>
{
    type Input = DeleteRestoreTestingPlanInput;
    type Output = DeleteRestoreTestingPlanOutput;
    type Error = DeleteRestoreTestingPlanError;

    const NAME: &'static str = "DeleteRestoreTestingPlan";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DELETE_RESTORE_TESTING_PLAN_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = DELETE_RESTORE_TESTING_PLAN_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 204u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = ctx;
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            DeleteRestoreTestingPlanErrorModeled::ServiceUnavailableException(
                ServiceUnavailableException {
                    message: Some("the backup vault store is unavailable".to_string()),
                    ..Default::default()
                },
            )
        })?;

        let mut store = state.lock();
        // Taking the record OUT is what makes the delete observable: the name this
        // call addressed holds no plan afterwards, so a read of it resolves nothing
        // and a create of the name is no longer a collision. Whether a record was
        // there is not the answer — the state the caller asked for holds either way
        // — so the only refusal is the one the plan's own selections make.
        store
            .remove_restore_testing_plan(&input.restore_testing_plan_name)
            .map_err(DeleteRestoreTestingPlanErrorModeled::InvalidRequestException)?;
        Ok(DeleteRestoreTestingPlanOutput)
    }
}
