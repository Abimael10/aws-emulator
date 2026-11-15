// SPDX-License-Identifier: CC0-1.0

//! @op DescribeInstanceAssociationsStatus
//! Report the status of the State Manager associations that apply to ONE managed node of
//! this Region, served one page at a time. This is the ONLY file to touch for this
//! operation.
//!
//! The answer is computed from the two stores that own the facts it reports, both reached
//! through [`crate::state`]: the association store a `CreateAssociation` wrote
//! ([`crate::state::associations`]) and the node store its targets are resolved against,
//! which a node enters only by registering. An association applies to a node when it was
//! created FOR that node, when State Manager dispatched it there, or when its targets select
//! it (`ComAmazonawsSsmAmazonssmState::association_applies_to_node`), and what it reports
//! for that node is the run that reached it
//! (`associations::AssociationRecord::to_instance_association_status_info`) — derived from
//! the instant the run was dispatched, so two callers reading at one instant are told the
//! same thing whether or not either of them called.
//!
//! The one write this report makes is materialising the managed-node registrations the
//! clock already owed ([`crate::state::node_registration::settle_registrations`]): an
//! instance that has been running long enough with a profile that lets its agent register
//! was a managed node at that instant whether or not anybody called.
//!
//! ## Which requests are refused, and with which modelled error
//!
//! * A node id outside the modelled `InstanceId` pattern is refused as a request validation
//!   before this handler runs. A node id that IS well formed but names no registered node is
//!   NOT a refusal: it simply has no association that applies to it, the same reading the
//!   node describes of this service take of such an id — which is how a caller asks what a
//!   node is applying without first having to know whether it is managed.
//! * A `NextToken` this service did not issue is `InvalidNextToken`: the token this
//!   operation hands out is the id of the association the next page starts at, so a token
//!   shaped like no association id was not one of ours.
//!
//! ## Paging
//!
//! A page holds at most `MaxResults` entries (the modelled range 1..50, the same bounds the
//! association listing carries), walked in ascending association-id order — the order the
//! association store is keyed in — and the `NextToken` is the id the next page starts at,
//! so a page resumes at an id rather than at an offset into a list that may have changed
//! between two pages.
//!
//! ---
//!
//! # Modeled contract for `DescribeInstanceAssociationsStatus`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json")
//! ```
//!
//! The status of the associations for the managed nodes.
//!
//! ## Input
//!
//! * `InstanceId` (InstanceId, pattern ^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$) **required** —
//!   The managed node IDs for which you want association status information.
//! * `MaxResults` (MaxResults, range 1..50) — The maximum number of items to return for this
//!   call. The call also returns a token that you can specify in a subsequent call to get the
//!   next set of results.
//! * `NextToken` (NextToken) — The token for the next set of items to return. (You received
//!   this token from a previous call.)
//!
//! ## Output
//!
//! * `InstanceAssociationStatusInfos` (InstanceAssociationStatusInfos) — Status information
//!   about the association.
//! * `NextToken` (NextToken) — The token to use when requesting the next set of items. If
//!   there are no additional items to return, the string is empty.
//!
//! ## Errors
//!
//! * `InternalServerError` (HTTP 500) — An error occurred on the server side.
//! * `InvalidInstanceId` (HTTP 400) — The following problems can cause this exception: You
//!   don't have permission to access the managed node. Amazon Web Services Systems Manager
//!   Agent (SSM Agent) isn't running. Verify that SSM Agent is running. SSM Agent isn't
//!   registered with the SSM endpoint. Try reinstalling SSM Agent. The managed node isn't in
//!   a valid state. Valid states are: Running, Pending, Stopped, and Stopping. Invalid states
//!   are: Shutting-down and Terminated.
//! * `InvalidNextToken` (HTTP 400) — The specified token isn't valid.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `DescribeInstanceAssociationsStatusError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ssm-amazonssm.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DescribeInstanceAssociationsStatusError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ssm#DescribeInstanceAssociationsStatus")]
#[derive(Debug)]
pub enum DescribeInstanceAssociationsStatusErrorModeled {
    InternalServerError(InternalServerError),
    InvalidInstanceId(InvalidInstanceId),
    InvalidNextToken(InvalidNextToken),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::describe_instance_associations_status::DescribeInstanceAssociationsStatusError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type DescribeInstanceAssociationsStatusError = crate::scaffolded::Scaffolded<DescribeInstanceAssociationsStatusErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<DescribeInstanceAssociationsStatusErrorModeled> for DescribeInstanceAssociationsStatusError {
    fn from(modeled: DescribeInstanceAssociationsStatusErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DescribeInstanceAssociationsStatus` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct DescribeInstanceAssociationsStatus<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DescribeInstanceAssociationsStatus<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DescribeInstanceAssociationsStatus<Eng>
{
    type Input = DescribeInstanceAssociationsStatusInput;
    type Output = DescribeInstanceAssociationsStatusOutput;
    type Error = DescribeInstanceAssociationsStatusError;

    const NAME: &'static str = "DescribeInstanceAssociationsStatus";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DESCRIBE_INSTANCE_ASSOCIATIONS_STATUS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DESCRIBE_INSTANCE_ASSOCIATIONS_STATUS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // `InstanceId`'s pattern and `MaxResults`'s range are MODELLED constraints, so the
        // centralized walk has already refused anything outside them: what is resolved here
        // is which node the request names, which associations apply to it and where its page
        // starts.
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            DescribeInstanceAssociationsStatusErrorModeled::InternalServerError(
                InternalServerError {
                    message: Some("the association store is unavailable".to_string()),
                },
            )
        })?;

        let region = crate::state::request_region(ctx.parts());
        // The one write this report makes, taken with no guard of this store held: the
        // managed-node registrations the clock already owed.
        crate::state::node_registration::settle_registrations(&*self.engine, &region);

        // The bounds are the modelled `MaxResults` (range 1..50), the shape the association
        // listing is bounded by as well.
        let page_size = crate::state::associations::association_page_size(input.max_results)
            .map_err(crate::scaffolded::Scaffolded::validation)?;

        // The token this operation hands out is the id of the association the next page
        // starts at, so a token shaped like no association id was not one of ours.
        let resume_from = match input.next_token.as_deref() {
            None | Some("") => None,
            Some(token) if crate::state::associations::is_association_id(token) => Some(token),
            Some(_) => {
                return Err(
                    DescribeInstanceAssociationsStatusErrorModeled::InvalidNextToken(
                        InvalidNextToken {
                            message: Some(crate::state::INVALID_NEXT_TOKEN.to_string()),
                        },
                    )
                    .into(),
                )
            }
        };

        let now = ::std::time::SystemTime::now();
        let store = state.lock();

        // An association is created through, and reached through, the Systems Manager
        // endpoint of ONE Region, so what this report walks is the associations of the
        // Region the request was signed for and nothing another Region holds.
        let mut reported = Vec::new();
        let mut next_token = None;
        for (id, record) in store.associations_in_region(&region) {
            if resume_from.is_some_and(|from| id < from) {
                continue;
            }
            if !store.association_applies_to_node(record, &input.instance_id) {
                continue;
            }
            if reported.len() == page_size {
                // The page is full and an association is still waiting: hand the caller the
                // id it resumes at, so the next request continues where this one stopped.
                next_token = Some(id.to_string());
                break;
            }
            reported.push(record.to_instance_association_status_info(&input.instance_id, now));
        }

        Ok(DescribeInstanceAssociationsStatusOutput {
            instance_association_status_infos: Some(reported),
            next_token,
        })
    }
}
