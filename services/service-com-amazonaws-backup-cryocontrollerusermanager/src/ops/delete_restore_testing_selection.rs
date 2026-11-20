// SPDX-License-Identifier: CC0-1.0

//! @op DeleteRestoreTestingSelection
//!
//! Removes the restore testing selection stored under the (plan name, selection
//! name) pair the request addresses, so every later call that resolves that pair
//! takes the selection's not-found path
//! ([`crate::state::restore_testing_selection_not_found`]) and the name is free
//! for a create again. The removal is the state module's one such step
//! ([`crate::state::ComAmazonawsBackupCryocontrollerusermanagerState::remove_restore_testing_selection`]),
//! so a selection cannot be taken out of the store two different ways.
//!
//! A name this account holds no plan under is the plan's own not-found condition
//! ([`crate::state::restore_testing_plan_not_found`]), resolved first, as it is
//! for every other call that addresses a selection.
//!
//! Once the plan is resolved the removal is IDEMPOTENT: the call states that the
//! named selection is not to be held any more, which is already true of a pair
//! the plan holds no selection under — one nothing created, or one a previous
//! delete already took — so the second delete of a selection answers the same
//! empty success as the first, as the backup plan's own selection delete does.
//! "All testing selections associated with a restore testing plan must be
//! deleted before the restore testing plan can be deleted" (the model doc
//! below), so what this call removes is what the plan stops holding.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `DeleteRestoreTestingSelection`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")
//! ```
//!
//! Input the Restore Testing Plan name and Restore Testing Selection name. All testing
//! selections associated with a restore testing plan must be deleted before the restore
//! testing plan can be deleted.
//!
//! ## Input
//!
//! * `RestoreTestingPlanName` (String) **required** — Required unique name of the restore
//!   testing plan that contains the restore testing selection you wish to delete.
//! * `RestoreTestingSelectionName` (String) **required** — Required unique name of the
//!   restore testing selection you wish to delete.
//!
//! ## Errors
//!
//! * `ResourceNotFoundException` (HTTP 400) — A resource that is required for the action
//!   doesn't exist.
//! * `ServiceUnavailableException` (HTTP 500) — The request failed due to a temporary failure
//!   of the server.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `DeleteRestoreTestingSelectionError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json");

#[derive(Debug, Clone, Default)]
pub struct DeleteRestoreTestingSelectionOutput;

impl DeleteRestoreTestingSelectionOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    )
    .with_http(::aws_smithy_schema::traits::HttpTrait::new(
        "DELETE",
        "/restore-testing/plans/{RestoreTestingPlanName}/selections/{RestoreTestingSelectionName}",
        Some(204u16),
    ));
}

pub static DELETE_RESTORE_TESTING_SELECTION_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &DeleteRestoreTestingSelectionOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeleteRestoreTestingSelectionOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DeleteRestoreTestingSelectionError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.backup#DeleteRestoreTestingSelection")]
#[derive(Debug)]
pub enum DeleteRestoreTestingSelectionErrorModeled {
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::delete_restore_testing_selection::DeleteRestoreTestingSelectionError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type DeleteRestoreTestingSelectionError = crate::scaffolded::Scaffolded<DeleteRestoreTestingSelectionErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<DeleteRestoreTestingSelectionErrorModeled> for DeleteRestoreTestingSelectionError {
    fn from(modeled: DeleteRestoreTestingSelectionErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DeleteRestoreTestingSelection` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct DeleteRestoreTestingSelection<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DeleteRestoreTestingSelection<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DeleteRestoreTestingSelection<Eng>
{
    type Input = DeleteRestoreTestingSelectionInput;
    type Output = DeleteRestoreTestingSelectionOutput;
    type Error = DeleteRestoreTestingSelectionError;

    const NAME: &'static str = "DeleteRestoreTestingSelection";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DELETE_RESTORE_TESTING_SELECTION_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = DELETE_RESTORE_TESTING_SELECTION_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 204u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = ctx;
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            DeleteRestoreTestingSelectionErrorModeled::ServiceUnavailableException(
                ServiceUnavailableException {
                    message: Some("the backup vault store is unavailable".to_string()),
                    ..Default::default()
                },
            )
        })?;

        let mut store = state.lock();
        // Taking the record OUT is what makes the delete observable: the pair
        // this call addressed holds no selection afterwards, so a read of it
        // resolves nothing and a create of the name is no longer a collision.
        // Whether a record was there is not the answer — the state the caller
        // asked for holds either way — so only an unresolvable PLAN is refused.
        store
            .remove_restore_testing_selection(
                &input.restore_testing_plan_name,
                &input.restore_testing_selection_name,
            )
            .map_err(DeleteRestoreTestingSelectionErrorModeled::ResourceNotFoundException)?;
        Ok(DeleteRestoreTestingSelectionOutput)
    }
}
