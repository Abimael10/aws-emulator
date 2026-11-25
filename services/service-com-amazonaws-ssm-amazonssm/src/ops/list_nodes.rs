// SPDX-License-Identifier: CC0-1.0

//! @op ListNodes
//! Report the managed nodes of the Systems Manager INTEGRATED EXPERIENCE — the nodes this
//! account's endpoints hold — narrowed by the request's filters and served one page at a
//! time. This is the ONLY file to touch for this operation.
//!
//! The answer is computed from the node store in [`crate::state`]
//! (`ComAmazonawsSsmAmazonssmState::managed_nodes`), which a node enters only by
//! registering: an EC2 instance whose SSM Agent registers, or a hybrid node installed with
//! an activation code. It is the same store the node describes report and
//! [`crate::state::inventory`] projects as a node's `AWS:InstanceInformation` item, so a
//! node listing reports what registering persisted rather than a second copy of it, and an
//! account that has registered none is reported as holding none. The one write it makes is
//! materialising the registrations the clock already owed
//! ([`crate::state::node_registration::settle_registrations`]): an instance that has been
//! running long enough with a profile that lets its agent register was a managed node at
//! that instant whether or not anybody called.
//!
//! ## Which requests are refused, and with which modelled error
//!
//! * A `SyncName` other than that one names no sync a node listing can read — "the name of
//!   the supported resource data sync is AWS-QuickSetup-ManagedNode" — so it is
//!   `ResourceDataSyncNotFoundException`, carrying the name and the sync type the listing
//!   looked for beside the sentence this service words a sync it cannot find with
//!   ([`crate::state::resource_data_syncs::sync_not_found_message`]). A sync that does not
//!   exist is not an operation this account cannot run, so a `SyncName` the request states
//!   is resolved BEFORE the account's experience is read.
//! * An account that has not enabled the Systems Manager integrated experience is answered
//!   `UnsupportedOperationException` ([`crate::state::node_listing::NOT_ENABLED_MESSAGE`]),
//!   before anything else the request names is looked at. Enabling the experience is what
//!   puts the Amazon Web Services managed node resource data sync in the account
//!   ([`crate::state::node_listing::MANAGED_NODE_SYNC_NAME`]), which is what the decision is
//!   taken from — per Region, because a sync is a regional record.
//! * A `NextToken` this service did not hand out is `InvalidNextToken`.
//!
//! A filter whose `Key` is outside the modelled `NodeFilterKey` enum, and an operator
//! outside `NodeFilterOperatorType`, are refused by the centralized request walk before this
//! handler runs, which is what `InvalidFilter` ("the filter name isn't valid") would report;
//! a filter VALUE no node holds is not a refusal — it simply selects no node, which is how a
//! caller asks whether any node carries it.
//!
//! ## Filtering, aggregation and paging
//!
//! Filters intersect and each filter's values union: a node is reported when, for every
//! filter, one of that filter's values stands in the filter's relation to one of the node's
//! values for that key ([`crate::state::node_listing::matches_filter`]). Which Regions' nodes
//! are listed is what `SyncName` decides — the Region the request was signed for when it
//! names no sync, and the Regions the managed node sync aggregates when it does
//! ([`crate::state::ComAmazonawsSsmAmazonssmState::node_listing_regions`]) — and each node
//! reports the Region it is held under. A page holds `MaxResults` nodes (the modelled range
//! 1..50, and up to 50 for a request that names none), and the `NextToken` is the
//! `(region, node id)` position the next page starts at
//! ([`crate::state::node_listing::page_token`]), so a page resumes at a node rather than at
//! an offset into a list that may have changed. That cursor is total because the node store
//! is keyed by `(region, node id)` and therefore walked in ascending order of both.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `ListNodes::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! @generated by the emulator core scaffold op-stub generator.
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `ListNodes`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json")
//! ```
//!
//! Takes in filters and returns a list of managed nodes matching the filter criteria.
//!
//! ## Input
//!
//! * `Filters` (NodeFilterList, len 1..5) — One or more filters. Use a filter to return a
//!   more specific list of managed nodes.
//! * `MaxResults` (MaxResults, range 1..50) — The maximum number of items to return for this
//!   call. The call also returns a token that you can specify in a subsequent call to get the
//!   next set of results.
//! * `NextToken` (NextToken) — The token for the next set of items to return. (You received
//!   this token from a previous call.)
//! * `SyncName` (ResourceDataSyncName, len 1..64) — The name of the Amazon Web Services
//!   managed resource data sync to retrieve information about. For cross-account/cross-Region
//!   configurations, this parameter is required, and the name of the supported resource data
//!   sync is AWS-QuickSetup-ManagedNode. For single account/single-Region configurations, the
//!   parameter is not required.
//!
//! ## Output
//!
//! * `NextToken` (NextToken) — The token to use when requesting the next set of items. If
//!   there are no additional items to return, the string is empty.
//! * `Nodes` (NodeList, len 0..50) — A list of managed nodes that match the specified filter
//!   criteria.
//!
//! ## Errors
//!
//! * `InternalServerError` (HTTP 500) — An error occurred on the server side.
//! * `InvalidFilter` (HTTP 400) — The filter name isn't valid. Verify that you entered the
//!   correct name and try again.
//! * `InvalidNextToken` (HTTP 400) — The specified token isn't valid.
//! * `ResourceDataSyncNotFoundException` (HTTP 400) — The specified sync name wasn't found.
//! * `UnsupportedOperationException` (HTTP 400) — This operation is not supported for the
//!   current account. You must first enable the Systems Manager integrated experience in your
//!   account.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListNodesError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ssm-amazonssm.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListNodesError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ssm#ListNodes")]
#[derive(Debug)]
pub enum ListNodesErrorModeled {
    InternalServerError(InternalServerError),
    InvalidFilter(InvalidFilter),
    InvalidNextToken(InvalidNextToken),
    ResourceDataSyncNotFoundException(ResourceDataSyncNotFoundException),
    UnsupportedOperationException(UnsupportedOperationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_nodes::ListNodesError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListNodesError = crate::scaffolded::Scaffolded<ListNodesErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListNodesErrorModeled> for ListNodesError {
    fn from(modeled: ListNodesErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListNodes` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListNodes<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListNodes<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListNodes<Eng>
{
    type Input = ListNodesInput;
    type Output = ListNodesOutput;
    type Error = ListNodesError;

    const NAME: &'static str = "ListNodes";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_NODES_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_NODES_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state::node_listing;
        use crate::state::resource_data_syncs::{sync_not_found_message, SYNC_TYPE_FROM_SOURCE};
        use crate::state::{non_empty, request_region, INVALID_NEXT_TOKEN};

        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(ListNodesErrorModeled::InternalServerError(InternalServerError {
                message: Some("this service is mounted on an engine that holds no state".to_string()),
            })
            .into());
        };

        // Systems Manager is regional: whether the account has the integrated experience,
        // and which sync aggregates the nodes, are facts of the endpoint the request was
        // signed for.
        let region = request_region(ctx.parts());
        let named_sync = non_empty(input.sync_name.as_deref());
        let regions = {
            let store = state.lock();
            // "The name of the supported resource data sync is AWS-QuickSetup-ManagedNode":
            // any other name names no sync whose nodes this listing could read, so it is
            // reported as a sync that does not exist — the sync a node listing reads
            // aggregates data from its source, which is the `SyncFromSource` type. A sync
            // that does not exist and an operation this account cannot run are DISTINCT
            // conditions, so the sync the request NAMES is resolved first: a client
            // branching on the code is told which of the two it ran into, and the name it
            // asked for is echoed back on the error.
            if let Some(named) =
                named_sync.filter(|named| *named != node_listing::MANAGED_NODE_SYNC_NAME)
            {
                return Err(ListNodesErrorModeled::ResourceDataSyncNotFoundException(
                    ResourceDataSyncNotFoundException {
                        message: Some(sync_not_found_message(named, SYNC_TYPE_FROM_SOURCE)),
                        sync_name: Some(named.to_string()),
                        sync_type: Some(SYNC_TYPE_FROM_SOURCE.to_string()),
                    },
                )
                .into());
            }
            // An account that has not enabled the integrated experience has no node
            // listing at all, which is answered before anything else the request names is
            // looked at.
            if !store.has_integrated_experience(&region) {
                return Err(ListNodesErrorModeled::UnsupportedOperationException(
                    UnsupportedOperationException {
                        message: Some(node_listing::NOT_ENABLED_MESSAGE.to_string()),
                    },
                )
                .into());
            }
            store.node_listing_regions(&region, named_sync.is_some())
        };

        let page_size = node_listing::page_size(input.max_results)
            .map_err(crate::scaffolded::Scaffolded::validation)?;

        // The token this operation hands out is the `(region, node id)` position the next
        // page starts at, so a value of any other shape was not one of ours.
        let resume_from = match non_empty(input.next_token.as_deref()) {
            None => None,
            Some(token) => match node_listing::page_cursor(token) {
                Some(position) => Some(position),
                None => {
                    return Err(ListNodesErrorModeled::InvalidNextToken(InvalidNextToken {
                        message: Some(INVALID_NEXT_TOKEN.to_string()),
                    })
                    .into())
                }
            },
        };

        // The registration an instance of a listed Region had already made by now was owed
        // at the same instant whether or not anybody called, so it is materialised before
        // the store is read — with no guard of it held.
        for listed in &regions {
            crate::state::node_registration::settle_registrations(&*self.engine, listed);
        }

        let filters = input.filters.as_deref().unwrap_or_default();
        let store = state.lock();
        let mut nodes = Vec::new();
        let mut next_token = None;
        'listing: for listed in &regions {
            for (id, node) in store.managed_nodes_in_region(listed) {
                if resume_from.is_some_and(|from| (listed.as_str(), id) < from) {
                    continue;
                }
                // Every filter has to be satisfied, and each filter's values union.
                if !filters
                    .iter()
                    .all(|filter| node_listing::matches_filter(node, listed, filter))
                {
                    continue;
                }
                if nodes.len() == page_size {
                    // The page is full and a node is still waiting: hand the caller the
                    // position it resumes at, so the next request continues where this one
                    // stopped.
                    next_token = Some(node_listing::page_token(listed, id));
                    break 'listing;
                }
                nodes.push(node_listing::to_node(listed, node));
            }
        }

        Ok(ListNodesOutput {
            nodes: Some(nodes),
            next_token,
        })
    }
}
