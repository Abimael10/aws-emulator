// SPDX-License-Identifier: CC0-1.0

//! @op DisassociateOpsItemRelatedItem
//! Deletes the association between an OpsItem and one of its related items: it removes the
//! record `AssociateOpsItemRelatedItem` persisted in
//! [`crate::state::ops_item_related_items`], so a later `ListOpsItemRelatedItems` no longer
//! reports it while every other related item of that OpsItem is left where it was.
//!
//! An association is reached through the OpsItem it belongs to, so the OpsItem has to be one
//! the Region holds and the association one that OpsItem carries; both refusals are worded
//! beside the stores they resolve against.
//!
//! ---
//!
//! # Modeled contract for `DisassociateOpsItemRelatedItem`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json")
//! ```
//!
//! Deletes the association between an OpsItem and a related item. For example, this API
//! operation can delete an Incident Manager incident from an OpsItem. Incident Manager is a
//! tool in Amazon Web Services Systems Manager.
//!
//! ## Input
//!
//! * `AssociationId` (OpsItemRelatedItemAssociationId) **required** — The ID of the
//!   association for which you want to delete an association between the OpsItem and a
//!   related item.
//! * `OpsItemId` (OpsItemId, pattern ^(oi)-[0-9a-f]{12}$) **required** — The ID of the
//!   OpsItem for which you want to delete an association between the OpsItem and a related
//!   item.
//!
//! ## Errors
//!
//! * `InternalServerError` (HTTP 500) — An error occurred on the server side.
//! * `OpsItemConflictException` (HTTP 400) — The specified OpsItem is in the process of being
//!   deleted.
//! * `OpsItemInvalidParameterException` (HTTP 400) — A specified parameter argument isn't
//!   valid. Verify the available arguments and try again.
//! * `OpsItemNotFoundException` (HTTP 400) — The specified OpsItem ID doesn't exist. Verify
//!   the ID and try again.
//! * `OpsItemRelatedItemAssociationNotFoundException` (HTTP 400) — The association wasn't
//!   found using the parameters you specified in the call. Verify the information and try
//!   again.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `DisassociateOpsItemRelatedItemError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ssm-amazonssm.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DisassociateOpsItemRelatedItemError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ssm#DisassociateOpsItemRelatedItem")]
#[derive(Debug)]
pub enum DisassociateOpsItemRelatedItemErrorModeled {
    InternalServerError(InternalServerError),
    OpsItemConflictException(OpsItemConflictException),
    OpsItemInvalidParameterException(OpsItemInvalidParameterException),
    OpsItemNotFoundException(OpsItemNotFoundException),
    OpsItemRelatedItemAssociationNotFoundException(OpsItemRelatedItemAssociationNotFoundException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::disassociate_ops_item_related_item::DisassociateOpsItemRelatedItemError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type DisassociateOpsItemRelatedItemError = crate::scaffolded::Scaffolded<DisassociateOpsItemRelatedItemErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<DisassociateOpsItemRelatedItemErrorModeled> for DisassociateOpsItemRelatedItemError {
    fn from(modeled: DisassociateOpsItemRelatedItemErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DisassociateOpsItemRelatedItem` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct DisassociateOpsItemRelatedItem<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DisassociateOpsItemRelatedItem<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DisassociateOpsItemRelatedItem<Eng>
{
    type Input = DisassociateOpsItemRelatedItemInput;
    type Output = DisassociateOpsItemRelatedItemOutput;
    type Error = DisassociateOpsItemRelatedItemError;

    const NAME: &'static str = "DisassociateOpsItemRelatedItem";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DISASSOCIATE_OPS_ITEM_RELATED_ITEM_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DISASSOCIATE_OPS_ITEM_RELATED_ITEM_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state::{ops_item_related_items as related_items, ops_items};

        // `OpsItemId`'s pattern is a MODELLED constraint, so the centralized walk has
        // already refused an id outside it. What is resolved here is what the model cannot
        // state: the OpsItem the association hangs off, the state that OpsItem is in, and
        // whether it carries the association the request names.
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            DisassociateOpsItemRelatedItemErrorModeled::InternalServerError(InternalServerError {
                message: Some("the OpsItem store is unavailable".to_string()),
            })
        })?;

        let region = crate::state::request_region(ctx.parts());
        let now = ::std::time::SystemTime::now();
        let mut store = state.lock();
        // A delete that had settled by now was owed whether or not anybody called, so an
        // OpsItem it removed — and the associations that went with it — is gone before this
        // call resolves anything.
        store.materialize_due(now);

        // An association is reached through the OpsItem it belongs to, so an id this Region
        // holds no OpsItem under is refused the way every operation that cannot resolve an
        // OpsItem id refuses it (`crate::state::ops_items::ops_item_not_found_message`).
        let Some(held) = store.ops_item(&region, &input.ops_item_id) else {
            return Err(
                DisassociateOpsItemRelatedItemErrorModeled::OpsItemNotFoundException(
                    OpsItemNotFoundException {
                        message: Some(ops_items::ops_item_not_found_message()),
                    },
                )
                .into(),
            );
        };
        // An OpsItem whose delete has been asked for and has not settled yet is still
        // reported, but there is nothing left to edit about it: the deletion of it is
        // already in progress, which is what `OpsItemConflictException` reports.
        if held.is_deleting() {
            return Err(
                DisassociateOpsItemRelatedItemErrorModeled::OpsItemConflictException(
                    OpsItemConflictException {
                        message: Some(ops_items::ops_item_deleting_message()),
                    },
                )
                .into(),
            );
        }

        // What a call deletes is ONE association of THIS OpsItem: an id the OpsItem carries
        // no association under — one nothing minted, one another OpsItem's, or one an
        // earlier call already deleted — is what
        // `OpsItemRelatedItemAssociationNotFoundException` reports nothing was found for,
        // so a repeated delete is refused rather than served as if it had deleted something.
        if store
            .remove_related_item(&region, &input.ops_item_id, &input.association_id)
            .is_none()
        {
            return Err(
                DisassociateOpsItemRelatedItemErrorModeled::OpsItemRelatedItemAssociationNotFoundException(
                    OpsItemRelatedItemAssociationNotFoundException {
                        message: Some(related_items::association_not_found_message()),
                    },
                )
                .into(),
            );
        }

        // The modelled response carries no members: what the call did is read back through
        // `ListOpsItemRelatedItems`, which no longer reports the association it deleted.
        Ok(DisassociateOpsItemRelatedItemOutput {})
    }
}
