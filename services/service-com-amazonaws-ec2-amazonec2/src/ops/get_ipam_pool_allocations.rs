// SPDX-License-Identifier: CC0-1.0

//! @op GetIpamPoolAllocations
//!
//! Reports the CIDRs allocated out of one IPAM pool, out of the store `AllocateIpamPoolCidr`
//! writes and a provision to a pool WITHIN this one adds to — so this read, the allocation
//! dependency `DeleteIpamPool` is refused for and the allocate itself cannot disagree about
//! one range.
//!
//! A pure REPORT: nothing is written beyond the clock-owed moves
//! ([`crate::state::ComAmazonawsEc2Amazonec2State::materialize_due_transitions`]), which were
//! due at their own instant whether or not anybody called.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request. The store is
//! reached through [`crate::state::require_service_state`], the way every handler beside this
//! one reaches it.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `GetIpamPoolAllocations`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")
//! ```
//!
//! Get a list of all the CIDR allocations in an IPAM pool. The Region you use should be the
//! IPAM pool locale. The locale is the Amazon Web Services Region where this IPAM pool is
//! available for allocations. If you use this action after AllocateIpamPoolCidr or
//! ReleaseIpamPoolAllocation, note that all EC2 API actions follow an eventual consistency
//! model.
//!
//! ## Input
//!
//! * `DryRun` (Boolean) — A check for whether you have the required permissions for the
//!   action without actually making the request and provides an error response. If you have
//!   the required permissions, the error response is DryRunOperation. Otherwise, it is
//!   UnauthorizedOperation.
//! * `Filters` (FilterList) — One or more filters for the request. For more information about
//!   filtering, see Filtering CLI output.
//! * `IpamPoolAllocationId` (IpamPoolAllocationId) — The ID of the allocation.
//! * `IpamPoolId` (IpamPoolId) **required** — The ID of the IPAM pool you want to see the
//!   allocations for.
//! * `MaxResults` (GetIpamPoolAllocationsMaxResults, range 1000..100000) — The maximum number
//!   of results you would like returned per page.
//! * `NextToken` (NextToken) — The token for the next page of results.
//!
//! ## Output
//!
//! * `IpamPoolAllocations` (IpamPoolAllocationSet) — The IPAM pool allocations you want
//!   information on.
//! * `NextToken` (NextToken) — The token to use to retrieve the next page of results. This
//!   value is null when there are no more results to return.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetIpamPoolAllocationsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ec2-amazonec2.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetIpamPoolAllocationsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ec2#GetIpamPoolAllocations")]
#[derive(Debug)]
pub enum GetIpamPoolAllocationsErrorModeled {
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_ipam_pool_allocations::GetIpamPoolAllocationsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetIpamPoolAllocationsError = crate::scaffolded::Scaffolded<GetIpamPoolAllocationsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetIpamPoolAllocationsErrorModeled> for GetIpamPoolAllocationsError {
    fn from(modeled: GetIpamPoolAllocationsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetIpamPoolAllocations` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetIpamPoolAllocations<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetIpamPoolAllocations<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetIpamPoolAllocations<Eng>
{
    type Input = GetIpamPoolAllocationsInput;
    type Output = GetIpamPoolAllocationsOutput;
    type Error = GetIpamPoolAllocationsError;

    const NAME: &'static str = "GetIpamPoolAllocations";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_IPAM_POOL_ALLOCATIONS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_IPAM_POOL_ALLOCATIONS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state::{
            CompiledFilter, MAX_FILTER_MATCH_STEPS, MAX_FILTER_VALUE_BYTES, MAX_FILTER_VALUES,
            MAX_FILTERS, MAX_PAGE_SIZE, MatchBudget,
        };

        // `ctx` carries request metadata only; this operation reads none of it — an allocation
        // is named by the pool it came out of, which carries its own Region. The store is
        // reached through the engine held on `self`.
        let _ = ctx;

        // "The token for the next page of results" (`$NextToken`) — anything else is refused
        // rather than read as a position, and it is refused before any other member is looked
        // at, so whether a bogus token is caught never depends on the rest of the request.
        let start_at = match input.next_token.as_deref() {
            None => None,
            Some(token) => Some(
                crate::state::decode_page_token(token)
                    .ok_or_else(crate::state::invalid_pagination_token)?,
            ),
        };

        // Matching cost is filters x values x held values x stored allocations and the model
        // bounds none of them (`Filters` is a plain `FilterList`), so the request-controlled
        // factors are capped here, before any work proportional to them is done — and their
        // PRODUCT is charged to one [`MatchBudget`] below.
        let filters: &[Filter] = input.filters.as_deref().unwrap_or_default();
        if filters.len() > MAX_FILTERS {
            return Err(crate::state::invalid_parameter_value(format!(
                "The request carries {} filters, above the maximum of {MAX_FILTERS}",
                filters.len()
            )));
        }
        let mut selectors: Vec<CompiledFilter> = Vec::with_capacity(filters.len());
        for filter in filters {
            let name = filter.name.as_deref().unwrap_or_default();
            if !IPAM_POOL_ALLOCATION_FILTERS.contains(&name) {
                return Err(crate::state::invalid_parameter_value(format!(
                    "The filter '{name}' is invalid"
                )));
            }
            let values = filter.values.as_deref().unwrap_or_default();
            if values.len() > MAX_FILTER_VALUES {
                return Err(crate::state::invalid_parameter_value(format!(
                    "The filter '{name}' carries {} values, above the maximum of \
                     {MAX_FILTER_VALUES}",
                    values.len()
                )));
            }
            // A capped NUMBER of comparisons is not capped WORK: the matcher's cost is the
            // product of the pattern's length and the value's, so each value's size is
            // bounded too, before any matching runs under the lock.
            if let Some(oversize) = values
                .iter()
                .find(|value| value.len() > MAX_FILTER_VALUE_BYTES)
            {
                return Err(crate::state::invalid_parameter_value(format!(
                    "A value of the filter '{name}' carries {} bytes, above the maximum of \
                     {MAX_FILTER_VALUE_BYTES}",
                    oversize.len()
                )));
            }
            selectors.push(CompiledFilter::new(name.to_string(), values.to_vec()));
        }

        // The FORM of each id the request names, before anything is resolved: "not an id of
        // this kind" is a fact about the request, so it is answered `.Malformed` ahead of the
        // DryRun arm below and never as a missing pool or allocation.
        let pool_id = input.ipam_pool_id.as_str();
        crate::state::require_well_formed_id(
            crate::state::IPAM_POOL_ID_PREFIX,
            pool_id,
            crate::state::INVALID_IPAM_POOL_ID_MALFORMED,
        )?;
        if let Some(allocation_id) = input.ipam_pool_allocation_id.as_deref() {
            crate::state::require_well_formed_ipam_pool_allocation_id(allocation_id)?;
        }

        // "A check for whether you have the required permissions for the action without
        // actually making the request" (`$DryRun`). Every caller of this emulator is
        // permitted, so a dry run reports that outcome and reads nothing. It sits AFTER every
        // parameter has been validated: the flag asks whether the request WOULD have
        // succeeded.
        if input.dry_run.unwrap_or(false) {
            return Err(crate::state::dry_run_operation());
        }

        let state = crate::state::require_service_state(&*self.engine)?;
        let mut store = state.lock();
        // The clock-owed moves this read is allowed to apply — the same call every other IPAM
        // read makes: a pool whose `create-complete` move has fallen due is already in that
        // state, whether or not anybody called, so applying them here changes nothing a later
        // caller would not have seen anyway.
        store.materialize_due_transitions();

        // "The ID of the IPAM pool you want to see the allocations for" (`$IpamPoolId`): a
        // pool this account holds none of is an error, not an empty page — which is what lets
        // a client tell a dangling reference from a pool nothing is allocated out of.
        if !store.ipam_pools.contains_key(pool_id) {
            return Err(crate::state::id_not_found(
                crate::state::INVALID_IPAM_POOL_ID_NOT_FOUND,
                crate::state::IPAM_POOL_ID_KIND,
                pool_id,
            ));
        }

        // The allocations made out of the pool, with every filter then holding ("If you
        // specify multiple filters, the filters are joined with an AND" — `Filter`). Every
        // comparison is charged to ONE budget, so the PRODUCT of stored allocations, filters,
        // values and value length is bounded however the caller combines them.
        let mut budget = MatchBudget::new(MAX_FILTER_MATCH_STEPS);
        let mut selected: Vec<IpamPoolAllocation> = Vec::new();
        for record in store.ipam_pool_allocation_records(pool_id) {
            if budget.exhausted() {
                break;
            }
            // "The ID of the allocation" (`$IpamPoolAllocationId`): a request naming one asks
            // about that allocation alone.
            if input
                .ipam_pool_allocation_id
                .as_deref()
                .is_some_and(|named| record.view.ipam_pool_allocation_id.as_deref() != Some(named))
            {
                continue;
            }
            if crate::state::matches_every_filter(&selectors, &mut budget, &|name| {
                filter_values(&record.view, name)
            }) {
                selected.push(record.view.clone());
            }
        }
        // A spent budget means the answer is INCOMPLETE, which is not something to report as a
        // result: the request asked for more filtering than one call will do, and is told so
        // rather than given a silently truncated page.
        if budget.exhausted() {
            return Err(crate::scaffolded::Scaffolded::request_limit_exceeded(
                format!(
                    "The request's filters would take more than {MAX_FILTER_MATCH_STEPS} \
                     comparisons to evaluate; narrow them or page with MaxResults"
                ),
            ));
        }
        // An id the pool holds no allocation for is a dangling reference rather than an empty
        // page, exactly as an unknown pool id is above.
        if let Some(named) = input
            .ipam_pool_allocation_id
            .as_deref()
            .filter(|_| selected.is_empty() && selectors.is_empty())
        {
            return Err(crate::state::id_not_found(
                crate::state::INVALID_IPAM_POOL_ALLOCATION_ID_NOT_FOUND,
                crate::state::IPAM_POOL_ALLOCATION_ID_KIND,
                named,
            ));
        }

        // Page in `IpamPoolAllocationId` order, which the cursor below names a position in —
        // the order the store already holds the records in, established here at the point of
        // use.
        selected.sort_by(|left, right| {
            left.ipam_pool_allocation_id
                .cmp(&right.ipam_pool_allocation_id)
        });
        let from = match start_at.as_deref() {
            None => 0,
            Some(cursor) => selected.partition_point(|allocation| {
                allocation
                    .ipam_pool_allocation_id
                    .as_deref()
                    .unwrap_or_default()
                    < cursor
            }),
        };
        let page = selected.get(from..).unwrap_or_default();

        // "The maximum number of results you would like returned per page" (`$MaxResults`). A
        // request that names none gets the largest page this service serves, which is also the
        // ceiling a larger `MaxResults` is held to. The model's `range 1000..100000` has
        // already refused anything outside it.
        let limit = input
            .max_results
            .and_then(|max_results| usize::try_from(max_results).ok())
            .unwrap_or(MAX_PAGE_SIZE)
            .min(MAX_PAGE_SIZE);

        // A token only when an allocation follows this page.
        let next_token = page
            .get(limit)
            .and_then(|allocation| allocation.ipam_pool_allocation_id.as_deref())
            .map(crate::state::encode_page_token);

        Ok(GetIpamPoolAllocationsOutput {
            ipam_pool_allocations: Some(page.iter().take(limit).cloned().collect()),
            next_token,
        })
    }
}

/// The filter names this operation resolves.
///
/// `GetIpamPoolAllocationsRequest$Filters` documents no list of its own — "One or more filters
/// for the request" is all of it
/// (`smithy_query(query="doc",shape="GetIpamPoolAllocationsRequest",member="Filters")`) — so
/// the names are the ones EC2's filtering convention gives: each member of the resource the
/// operation reports, spelled in kebab-case. This list is therefore derived mechanically from
/// the `IpamPoolAllocation` shape (`smithy_query(query="shape",shape="IpamPoolAllocation")`),
/// one entry per SCALAR member; `Tags` is a LIST, whose entries carry no single value to
/// compare a filter value against, so it is not one. Every entry must be answered by
/// [`filter_values`].
const IPAM_POOL_ALLOCATION_FILTERS: [&str; 7] = [
    "cidr",
    "description",
    "ipam-pool-allocation-id",
    "resource-id",
    "resource-owner",
    "resource-region",
    "resource-type",
];

/// The values the EC2 filter `name` compares against this allocation.
///
/// A filter naming an attribute this record does not carry compares against NO values, so it
/// selects nothing — the same answer AWS gives for, say, `resource-id` on an allocation held by
/// no resource.
fn filter_values<'a>(
    allocation: &'a IpamPoolAllocation,
    name: &str,
) -> Vec<::std::borrow::Cow<'a, str>> {
    use ::std::borrow::Cow;

    let text = |value: Option<&'a str>| -> Vec<Cow<'a, str>> {
        value.map(Cow::Borrowed).into_iter().collect()
    };
    match name {
        "cidr" => text(allocation.cidr.as_deref()),
        "description" => text(allocation.description.as_deref()),
        "ipam-pool-allocation-id" => text(allocation.ipam_pool_allocation_id.as_deref()),
        "resource-id" => text(allocation.resource_id.as_deref()),
        "resource-owner" => text(allocation.resource_owner.as_deref()),
        "resource-region" => text(allocation.resource_region.as_deref()),
        "resource-type" => text(allocation.resource_type.as_deref()),
        _ => Vec::new(),
    }
}
