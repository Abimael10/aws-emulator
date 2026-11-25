// SPDX-License-Identifier: CC0-1.0

//! @op GetOpsSummary
//! Report a summary of this account's operations metadata (OpsData): one entity per OpsData
//! item the request's filters select, or the counts its aggregators ask for. This is the
//! ONLY file to touch for this operation.
//!
//! The answer is computed from the resources this service already holds: OpsData is a
//! PROJECTION of them under the data type each reports as ([`crate::state::ops_data`]), so
//! an `AWS:OpsItem` item is the OpsItem OpsCenter persisted and an
//! `AWS:EC2InstanceInformation` item is the instance information the node registered with.
//! An account that has opened no OpsItem and registered no node holds no OpsData, and
//! nothing here invents an item. The only writes are the transitions the clock already owed
//! — the OpsItem deletes that had settled
//! ([`crate::state::ComAmazonawsSsmAmazonssmState::materialize_due`]) and the node
//! registrations that were due
//! ([`crate::state::node_registration::settle_registrations`]) — each owed at the same
//! instant whether or not anybody called.
//!
//! ## Which requests are refused, and with which modelled error
//!
//! * A `ResultAttributes` type name outside the published OpsData types
//!   ([`crate::state::ops_data::OPS_DATA_TYPES`]) is `InvalidTypeNameException`. A published
//!   type this emulator has collected no OpsData for is NOT refused: it reports the nothing
//!   collected for it.
//! * A `Filters` key that names no attribute of a published OpsData type — not the dotted
//!   `<TypeName>.<AttributeName>` form, a type outside that list, or an attribute outside
//!   the schema of a type whose attributes this service knows — is `InvalidFilter`
//!   ([`crate::state::ops_data::filter_key_rejection`]), which states the form a key must
//!   take and the offending key for a key that is not that form.
//! * An `Aggregators` entry that names no OpsData type, a type outside the published list,
//!   no attribute to count by, an attribute its type does not carry, or an aggregation other
//!   than the count this service computes
//!   ([`crate::state::ops_data::AGGREGATOR_TYPE_COUNT`]) is `InvalidAggregatorException`. So
//!   is a nesting deeper than [`MAX_AGGREGATOR_NESTING`], and a request whose OpsData turns
//!   its aggregation into more entries than an item may carry
//!   ([`crate::state::ops_data::MAX_AGGREGATED_ENTRIES`]).
//! * A `SyncName` the Region holds no resource data sync of that aggregates OpsData is
//!   `ResourceDataSyncNotFoundException`, carrying the name and the sync type it was
//!   addressed by.
//! * A `NextToken` that names no item of this result is `InvalidNextToken`.
//!
//! ## Filtering, aggregating and paging
//!
//! Filters intersect and each filter's values union, under the operator the filter names
//! (`Equal` when it names none, and `Exists` asking only whether the attribute is reported
//! at all — [`crate::state::ops_data::OpsDataItem::satisfies`]). An aggregator counts the
//! selected items per distinct value of the attribute it names, and a nested aggregator
//! refines each of those counts into the counts of its own attribute within them ("a nested
//! aggregator for viewing counts of OpsData", `OpsAggregator$Aggregators`). A count belongs
//! to no resource, so the counts are reported as the one entity that names none, and that
//! entity reports an item for EVERY OpsData type the aggregators name — with an empty
//! `Content` for a type no selected item was counted under, so an aggregation that matched
//! nothing is an empty list rather than a missing element.
//!
//! A page of the per-item report holds `MaxResults` entities (the modelled range 1..50) and
//! the `NextToken` is the id of the resource the next page starts at, which is total because
//! the items are walked in ascending id order.
//!
//! A request naming a `SyncName` is served the OpsData of the Regions that sync's source
//! names ([`crate::state::ops_data::sync_source_regions`]); one naming none is served the
//! OpsData of the Region it was signed for.
//!
//! ---
//!
//! # Modeled contract for `GetOpsSummary`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json")
//! ```
//!
//! View a summary of operations metadata (OpsData) based on specified filters and
//! aggregators. OpsData can include information about Amazon Web Services Systems Manager
//! OpsCenter operational workitems (OpsItems) as well as information about any Amazon Web
//! Services resource or service configured to report OpsData to Amazon Web Services Systems
//! Manager Explorer.
//!
//! ## Input
//!
//! * `Aggregators` (OpsAggregatorList, len 1..12) — Optional aggregators that return counts
//!   of OpsData based on one or more expressions.
//! * `Filters` (OpsFilterList, len 1..5) — Optional filters used to scope down the returned
//!   OpsData.
//! * `MaxResults` (MaxResults, range 1..50) — The maximum number of items to return for this
//!   call. The call also returns a token that you can specify in a subsequent call to get the
//!   next set of results.
//! * `NextToken` (NextToken) — A token to start the list. Use this token to get the next set
//!   of results.
//! * `ResultAttributes` (OpsResultAttributeList, len 1..) — The OpsData data type to return.
//! * `SyncName` (ResourceDataSyncName, len 1..64) — Specify the name of a resource data sync
//!   to get.
//!
//! ## Output
//!
//! * `Entities` (OpsEntityList) — The list of aggregated details and filtered OpsData.
//! * `NextToken` (NextToken) — The token for the next set of items to return. Use this token
//!   to get the next set of results.
//!
//! ## Errors
//!
//! * `InternalServerError` (HTTP 500) — An error occurred on the server side.
//! * `InvalidAggregatorException` (HTTP 400) — The specified aggregator isn't valid for the
//!   group type. Verify that the aggregator you provided is supported.
//! * `InvalidFilter` (HTTP 400) — The filter name isn't valid. Verify that you entered the
//!   correct name and try again.
//! * `InvalidNextToken` (HTTP 400) — The specified token isn't valid.
//! * `InvalidTypeNameException` (HTTP 400) — The parameter type name isn't valid.
//! * `ResourceDataSyncNotFoundException` (HTTP 400) — The specified sync name wasn't found.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetOpsSummaryError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ssm-amazonssm.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetOpsSummaryError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ssm#GetOpsSummary")]
#[derive(Debug)]
pub enum GetOpsSummaryErrorModeled {
    InternalServerError(InternalServerError),
    InvalidAggregatorException(InvalidAggregatorException),
    InvalidFilter(InvalidFilter),
    InvalidNextToken(InvalidNextToken),
    InvalidTypeNameException(InvalidTypeNameException),
    ResourceDataSyncNotFoundException(ResourceDataSyncNotFoundException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_ops_summary::GetOpsSummaryError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetOpsSummaryError = crate::scaffolded::Scaffolded<GetOpsSummaryErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetOpsSummaryErrorModeled> for GetOpsSummaryError {
    fn from(modeled: GetOpsSummaryErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetOpsSummary` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetOpsSummary<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetOpsSummary<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetOpsSummary<Eng>
{
    type Input = GetOpsSummaryInput;
    type Output = GetOpsSummaryOutput;
    type Error = GetOpsSummaryError;

    const NAME: &'static str = "GetOpsSummary";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_OPS_SUMMARY_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_OPS_SUMMARY_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state::ops_data::{
            filter_key_rejection, is_ops_data_type, ops_summary_page_size, sync_source_regions,
            INVALID_TYPE_NAME, OPS_DATA_TYPE_EC2_INSTANCE_INFORMATION, OPS_DATA_TYPE_OPS_ITEM,
        };
        use crate::state::resource_data_syncs::{sync_not_found_message, SYNC_TYPE_FROM_SOURCE};
        use crate::state::{non_empty, request_region, INVALID_NEXT_TOKEN};

        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(GetOpsSummaryErrorModeled::InternalServerError(InternalServerError {
                message: Some("this service is mounted on an engine that holds no state".to_string()),
            })
            .into());
        };

        // A key or an aggregator this service cannot serve cannot be served whatever the
        // account holds, so the request is refused before anything is read.
        let filters = input.filters.as_deref().unwrap_or_default();
        for filter in filters {
            if let Some(why) = filter_key_rejection(&filter.key) {
                return Err(invalid_filter(why));
            }
        }
        let aggregators = input.aggregators.as_deref().unwrap_or_default();
        check_aggregators(aggregators, 1)?;

        // "The OpsData data type to return" (`$ResultAttributes`): the types the per-item
        // report is of, or the one a request naming none reports
        // ([`OPS_DATA_TYPE_OPS_ITEM`]).
        let reported_types: Vec<&str> = match input.result_attributes.as_deref() {
            None | Some([]) => vec![OPS_DATA_TYPE_OPS_ITEM],
            Some(attributes) => attributes
                .iter()
                .map(|attribute| attribute.type_name.as_str())
                .collect(),
        };
        for type_name in &reported_types {
            if !is_ops_data_type(type_name) {
                return Err(GetOpsSummaryErrorModeled::InvalidTypeNameException(
                    InvalidTypeNameException {
                        message: Some(INVALID_TYPE_NAME.to_string()),
                    },
                )
                .into());
            }
        }

        let page_size = ops_summary_page_size(input.max_results)
            .map_err(crate::scaffolded::Scaffolded::validation)?;

        // Systems Manager is regional: the OpsData a summary reports is the OpsData of the
        // Region this request was signed for, or — for a summary read through a resource
        // data sync — of the Regions that sync's source names.
        let region = request_region(ctx.parts());
        let regions = match non_empty(input.sync_name.as_deref()) {
            None => vec![region.clone()],
            Some(sync_name) => {
                let store = state.lock();
                // A sync that aggregates OpsData is one FROM a source: "this type can
                // synchronize OpsItems and OpsData from multiple Amazon Web Services
                // Regions" (`CreateResourceDataSync`), which is the `SyncFromSource` type,
                // so a name the Region holds no such sync under names no sync to get.
                let held = store
                    .resource_data_sync(&region, sync_name)
                    .filter(|record| record.sync_type == SYNC_TYPE_FROM_SOURCE);
                let Some(record) = held else {
                    return Err(
                        GetOpsSummaryErrorModeled::ResourceDataSyncNotFoundException(
                            ResourceDataSyncNotFoundException {
                                sync_name: Some(sync_name.to_string()),
                                sync_type: Some(SYNC_TYPE_FROM_SOURCE.to_string()),
                                message: Some(sync_not_found_message(
                                    sync_name,
                                    SYNC_TYPE_FROM_SOURCE,
                                )),
                            },
                        )
                        .into(),
                    );
                };
                let named = sync_source_regions(record);
                // A sync naming no Region of its own aggregates the Region it was created
                // in, which is the one OpsData it can reach.
                if named.is_empty() {
                    vec![region.clone()]
                } else {
                    named
                }
            }
        };

        // The registrations each of those Regions already owed were owed at the same instant
        // whether or not anybody called, so they are materialised — with no guard of the
        // store held — before the OpsData is read.
        for settled in &regions {
            crate::state::node_registration::settle_registrations(&*self.engine, settled);
        }

        // The OpsData is SNAPSHOT here and the store released again before anything is
        // filtered, counted or paged: an aggregation walks the selected items once per
        // aggregation it was asked for, and it must not hold the one store while it does.
        //
        // An aggregator names the OpsData type it counts over itself, so an aggregated
        // summary snapshots the OpsData of every type this service holds items of rather
        // than only the types the request asked to be reported.
        let snapshot_types: Vec<&str> = if aggregators.is_empty() {
            reported_types.clone()
        } else {
            vec![
                OPS_DATA_TYPE_OPS_ITEM,
                OPS_DATA_TYPE_EC2_INSTANCE_INFORMATION,
            ]
        };
        let now = ::std::time::SystemTime::now();
        let selected: Vec<crate::state::ops_data::OpsDataItem> = {
            let mut store = state.lock();
            // An OpsItem whose delete had settled by now is gone before this summary
            // resolves anything.
            store.materialize_due(now);
            snapshot_types
                .iter()
                .flat_map(|type_name| store.ops_data(&regions, type_name))
                .filter(|item| item.satisfies_all(filters))
                .collect()
        };

        if !aggregators.is_empty() {
            // A count is about the OpsData as a whole rather than about one resource, so the
            // counts of every aggregator the request named are reported together.
            let items: Vec<&crate::state::ops_data::OpsDataItem> = selected.iter().collect();
            return Ok(GetOpsSummaryOutput {
                entities: Some(aggregated_entities(&items, aggregators)?),
                next_token: None,
            });
        }

        // The token this summary hands out is the id of the resource the next page starts
        // at, so a token naming no item of this result is not a page of it to resume.
        let start = match non_empty(input.next_token.as_deref()) {
            None => 0,
            Some(token) => {
                let Some(at) = selected.iter().position(|item| item.id == token) else {
                    return Err(GetOpsSummaryErrorModeled::InvalidNextToken(InvalidNextToken {
                        message: Some(INVALID_NEXT_TOKEN.to_string()),
                    })
                    .into());
                };
                at
            }
        };
        // `start` is a position in `selected`, so the page is a suffix of it; `get` keeps
        // that in-bounds without indexing on a value the request steers.
        let page = selected.get(start..).unwrap_or_default();
        let entities = page.iter().take(page_size).map(reported_entity).collect();
        // An item is still waiting past this page: hand the caller the id it resumes at.
        let next_token = page.get(page_size).map(|item| item.id.clone());

        Ok(GetOpsSummaryOutput {
            entities: Some(entities),
            next_token,
        })
    }
}

/// How far a request may nest aggregators inside aggregators. The model bounds the length of
/// an aggregator list (`len 1..12`) but not the depth of the nesting, and the depth is what
/// the aggregation walk below recurses on, so this emulator bounds it itself and refuses a
/// request past it rather than walking a depth a caller chose.
pub const MAX_AGGREGATOR_NESTING: usize = 10;

/// The modelled refusal of a filter key this operation cannot serve.
fn invalid_filter(why: String) -> GetOpsSummaryError {
    GetOpsSummaryErrorModeled::InvalidFilter(InvalidFilter { message: Some(why) }).into()
}

/// The modelled refusal of an aggregator this operation cannot count over.
fn invalid_aggregator(why: &str) -> GetOpsSummaryError {
    GetOpsSummaryErrorModeled::InvalidAggregatorException(InvalidAggregatorException {
        message: Some(why.to_string()),
    })
    .into()
}

/// Refuse every aggregator of `aggregators`, and of the aggregators nested inside them, that
/// this service cannot count over. `depth` is the nesting level the list sits at, counted
/// from one.
///
/// An aggregator counts the OpsData of ONE type per distinct value of ONE attribute of it, so
/// it has to name both, the type has to be one this service publishes and the attribute one
/// that type carries. The aggregation itself has to be the count this service computes
/// ([`crate::state::ops_data::AGGREGATOR_TYPE_COUNT`]); an aggregator naming another
/// aggregation asks for one that is not supported here. A nested aggregator is held to every
/// one of those rules, and an aggregator's own filters to the rule the request's filters are.
fn check_aggregators(
    aggregators: &[OpsAggregator],
    depth: usize,
) -> Result<(), GetOpsSummaryError> {
    use crate::state::inventory::INVALID_AGGREGATOR;
    use crate::state::ops_data::{
        filter_key_rejection, is_ops_data_type, names_attribute, AGGREGATOR_TYPE_COUNT,
    };
    use crate::state::non_empty;

    if aggregators.is_empty() {
        // Nothing is asked for at this level, so no depth is reached by it: an aggregator
        // that nests none sits at the deepest level the request names.
        return Ok(());
    }
    if depth > MAX_AGGREGATOR_NESTING {
        return Err(invalid_aggregator(INVALID_AGGREGATOR));
    }
    for aggregator in aggregators {
        // An aggregator that names no aggregation asks for the one this service computes;
        // one that names another asks for an aggregation it does not.
        if non_empty(aggregator.aggregator_type.as_deref())
            .is_some_and(|named| named != AGGREGATOR_TYPE_COUNT)
        {
            return Err(invalid_aggregator(INVALID_AGGREGATOR));
        }
        let Some(type_name) = non_empty(aggregator.type_name.as_deref()) else {
            return Err(invalid_aggregator(INVALID_AGGREGATOR));
        };
        if !is_ops_data_type(type_name) {
            return Err(invalid_aggregator(INVALID_AGGREGATOR));
        }
        let Some(attribute) = non_empty(aggregator.attribute_name.as_deref()) else {
            return Err(invalid_aggregator(INVALID_AGGREGATOR));
        };
        if !names_attribute(type_name, attribute) {
            return Err(invalid_aggregator(INVALID_AGGREGATOR));
        }
        for filter in aggregator.filters.as_deref().unwrap_or_default() {
            if let Some(why) = filter_key_rejection(&filter.key) {
                return Err(invalid_filter(why));
            }
        }
        check_aggregators(aggregator.aggregators.as_deref().unwrap_or_default(), depth + 1)?;
    }
    Ok(())
}

/// One item of OpsData as the summary reports it: the attributes it holds, captured when the
/// resource they are about last changed, under the id of that resource.
fn reported_entity(item: &crate::state::ops_data::OpsDataItem) -> OpsEntity {
    let reported = OpsEntityItem {
        capture_time: item.capture_time.clone(),
        content: Some(vec![item
            .content
            .iter()
            .map(|(attribute, value)| (attribute.clone(), value.clone()))
            .collect()]),
    };
    OpsEntity {
        id: Some(item.id.clone()),
        data: Some(std::collections::HashMap::from([(
            item.type_name.clone(),
            reported,
        )])),
    }
}

/// The counts `aggregators` asks for over the OpsData `items`, as the one entity that carries
/// them: an entity item per OpsData type the aggregators name, whose `Content` holds that
/// type's counted entries — EMPTY when no selected item was counted under it, the aggregation
/// having run and matched nothing.
///
/// The walk counts no more entries than an item may carry
/// ([`crate::state::ops_data::MAX_AGGREGATED_ENTRIES`]) and refuses the request when the
/// OpsData turns the aggregation it asked for into more than that, so what is counted is
/// bounded by that one number whatever the account holds.
fn aggregated_entities(
    items: &[&crate::state::ops_data::OpsDataItem],
    aggregators: &[OpsAggregator],
) -> Result<Vec<OpsEntity>, GetOpsSummaryError> {
    use crate::state::ops_data::MAX_AGGREGATED_ENTRIES;

    let mut counted: std::collections::BTreeMap<String, Vec<AggregatedEntry>> =
        std::collections::BTreeMap::new();
    // Every type the request asked to be counted over is reported, whether or not the
    // account holds OpsData of it: the entity item says the aggregation of that type ran and
    // its `Content` says what it counted, so a client reading the counts of the type it
    // asked about finds the empty list rather than nothing at all.
    seed_counted_types(aggregators, &mut counted);
    let mut remaining = MAX_AGGREGATED_ENTRIES;
    aggregate(items, aggregators, &Vec::new(), &mut counted, &mut remaining)?;
    if counted.is_empty() {
        return Ok(Vec::new());
    }
    let data = counted
        .into_iter()
        .map(|(type_name, entries)| {
            let reported = OpsEntityItem {
                // A count is about no one capture of the OpsData, so it carries no capture
                // time.
                capture_time: None,
                content: Some(
                    entries
                        .into_iter()
                        .map(|entry| entry.into_iter().collect())
                        .collect(),
                ),
            };
            (type_name, reported)
        })
        .collect();
    Ok(vec![OpsEntity {
        // "ID of the OpsItem entity" (`OpsEntity$Id`) is the resource the reported OpsData is
        // about, and a count is the OpsData of no single resource — so the entity carrying
        // the counts names none.
        id: None,
        data: Some(data),
    }])
}

/// Record an empty count for every OpsData type `aggregators` names, the nested aggregators
/// included.
///
/// Each aggregator counts over the type IT names ("a nested aggregator for viewing counts of
/// OpsData", `OpsAggregator$Aggregators`), so each of those types is one the summary reports
/// an entity item for, and one whose `Content` is empty when nothing was counted under it.
///
/// The nesting this walks is the nesting [`check_aggregators`] has already refused anything
/// past ([`MAX_AGGREGATOR_NESTING`]), and that runs before a single item of OpsData is read,
/// so the depth here is bounded by that check rather than by what the request asked for —
/// the same bound the counting walk below stands on.
fn seed_counted_types(
    aggregators: &[OpsAggregator],
    counted: &mut std::collections::BTreeMap<String, Vec<AggregatedEntry>>,
) {
    for aggregator in aggregators {
        if let Some(type_name) = crate::state::non_empty(aggregator.type_name.as_deref()) {
            counted.entry(type_name.to_string()).or_default();
        }
        seed_counted_types(aggregator.aggregators.as_deref().unwrap_or_default(), counted);
    }
}

/// One aggregated entry under construction: the attribute names and values it has
/// accumulated, in the order the aggregation added them, so a refined count reports the value
/// it was refined from beside its own.
type AggregatedEntry = Vec<(String, String)>;

/// Count the OpsData of `items` as `aggregators` asks, appending each entry to the content of
/// the OpsData type the aggregator is about. `carried` is what the enclosing aggregation
/// already put in every entry this one produces, and `remaining` is how many more entries the
/// whole summary may still count.
///
/// An aggregator with nested aggregators reports the REFINED counts — "a nested aggregator for
/// viewing counts of OpsData" (`OpsAggregator$Aggregators`) — so each NON-EMPTY subset it
/// isolates is handed to them instead of being counted itself; a subset holding no item has
/// nothing left to refine and is reported as the count it is. An item that reports no value
/// for the counted attribute is in no bucket: there is no value of it to count the item
/// under. Every entry counted spends one of `remaining`, and a summary whose OpsData turns its
/// aggregation into more entries than that is refused rather than half-counted.
fn aggregate(
    items: &[&crate::state::ops_data::OpsDataItem],
    aggregators: &[OpsAggregator],
    carried: &AggregatedEntry,
    counted: &mut std::collections::BTreeMap<String, Vec<AggregatedEntry>>,
    remaining: &mut usize,
) -> Result<(), GetOpsSummaryError> {
    use crate::state::inventory::{AGGREGATION_COUNT_KEY, INVALID_AGGREGATOR};

    for aggregator in aggregators {
        // Every aggregator was held to these two before a single item was read
        // ([`check_aggregators`]), so an aggregator that reaches here names both.
        let Some(type_name) = crate::state::non_empty(aggregator.type_name.as_deref()) else {
            continue;
        };
        let Some(attribute) = crate::state::non_empty(aggregator.attribute_name.as_deref()) else {
            continue;
        };
        let nested = aggregator.aggregators.as_deref().unwrap_or_default();
        let narrowed = aggregator.filters.as_deref().unwrap_or_default();

        // The items this aggregator counts: the OpsData of its own type that its own filters
        // select, bucketed by the value each of them reports for the counted attribute.
        let mut buckets: std::collections::BTreeMap<&str, Vec<&crate::state::ops_data::OpsDataItem>> =
            std::collections::BTreeMap::new();
        for item in items {
            if item.type_name != type_name || !item.satisfies_all(narrowed) {
                continue;
            }
            if let Some(value) = item.attribute(attribute) {
                buckets.entry(value).or_default().push(item);
            }
        }

        for (value, bucket) in buckets {
            let mut entry = carried.clone();
            entry.push((attribute.to_string(), value.to_string()));
            if !nested.is_empty() && !bucket.is_empty() {
                aggregate(&bucket, nested, &entry, counted, remaining)?;
                continue;
            }
            *remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| invalid_aggregator(INVALID_AGGREGATOR))?;
            entry.push((
                AGGREGATION_COUNT_KEY.to_string(),
                bucket.len().to_string(),
            ));
            counted
                .entry(type_name.to_string())
                .or_default()
                .push(entry);
        }
    }
    Ok(())
}
