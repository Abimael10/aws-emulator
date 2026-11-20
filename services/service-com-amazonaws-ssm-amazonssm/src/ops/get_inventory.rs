// SPDX-License-Identifier: CC0-1.0

//! @op GetInventory
//! Report the Systems Manager Inventory of this Region's managed nodes: one entity per
//! node the request's filters select, or the counts an aggregator asks for. This is the
//! ONLY file to touch for this operation.
//!
//! The answer is computed from the inventory in [`crate::state::inventory`], which is the
//! node store itself: a node's `AWS:InstanceInformation` item is the projection of the
//! record it registered with ([`crate::state::inventory::instance_information_content`]),
//! so an account that has registered no node holds no inventory and nothing here invents
//! an item. The one write it makes is materialising the registrations the clock already
//! owed ([`crate::state::node_registration::settle_registrations`]): an EC2 instance that
//! has been running long enough with a profile that lets its agent register was a managed
//! node at that instant whether or not anybody called, and its own information is the
//! inventory it has collected.
//!
//! ## Which requests are refused, and with which modelled error
//!
//! * A `Filters` key that is not the `<TypeName>.<AttributeName>` form is not a key at all
//!   and is `InvalidFilter` quoting the key and the form one takes; a key of that form whose
//!   type-name part names no inventory type is a TYPE-NAME fault, so it is
//!   `InvalidTypeNameException` naming the type name and the key it was read from; and a key
//!   naming an attribute outside the schema of the one type whose schema this service holds
//!   is `InvalidFilter` naming that attribute
//!   ([`crate::state::inventory::filter_key_rejection`]). A key naming a type this service
//!   knows and holds no item of is NOT refused: it selects nothing, which is what has been
//!   collected for it.
//! * A `Filters` entry naming the `Exists` operator is `InvalidFilter`
//!   ([`crate::state::inventory::EXISTS_ONLY_IN_AGGREGATOR_FILTERS`]): "the `Exists` filter
//!   must be used with aggregators" (`InventoryFilter$Type`), so asking whether an attribute
//!   is reported at all is a criterion an aggregator's groups are defined by, not a way of
//!   selecting the nodes a query reports.
//! * An `Aggregators` expression that is not the `<TypeName>.<AttributeName>` form at all
//!   names no aggregation to look up, and is `InvalidAggregatorException` quoting the
//!   expression ([`crate::state::inventory::invalid_expression_message`]).
//! * An `Aggregators` expression whose type-name part names no inventory type is
//!   `InvalidTypeNameException` naming it — the type name a request carries is validated as
//!   a type name, separately from and ahead of the aggregation it asks for, so a mistyped
//!   or unsupported type is reported as the type-name fault it is rather than as an
//!   aggregator fault ([`crate::state::inventory::aggregator_expression_rejection`]).
//! * An `Aggregators` expression naming an attribute its type supports no aggregation on is
//!   `InvalidAggregatorException` stating WHICH attribute of WHICH type was rejected
//!   ([`crate::state::inventory::not_aggregatable_message`]) — "this call returns the
//!   AWS:InstanceInformation type, because it supports aggregation based on the
//!   PlatformName, PlatformType, and PlatformVersion attributes"
//!   (`GetInventorySchemaRequest$Aggregator`), so an aggregator over another attribute of
//!   that type, or over any attribute of a type this service reports no aggregatable schema
//!   for, is one the service does not support.
//! * An aggregator that names neither an expression nor groups has nothing to count over,
//!   and one that names both names two aggregations where its result holds one: both are
//!   `InvalidAggregatorException` stated by that class's own sentence. So is a nesting
//!   deeper than [`MAX_AGGREGATOR_NESTING`], the emulator's own bound on how far a request
//!   may nest aggregators.
//! * A `ResultAttributes` type name outside the types this query PROJECTS
//!   ([`crate::state::inventory::SUPPORTED_RESULT_TYPE_NAMES`]) is
//!   `InvalidResultAttributeException` naming it beside the names it may name — that set is
//!   narrower than the inventory types that exist, so a real inventory type a read of one
//!   node's entries reports is refused here too. A projected type this store holds no item
//!   of is not refused: the query reports the nothing collected for it.
//! * A `NextToken` this service did not issue is `InvalidNextToken`.
//!
//! ## Filtering, aggregating and paging
//!
//! Filters intersect and each filter's values union, under the operator the filter names
//! (`Equal` when it names none — [`crate::state::inventory::matches_filter`]; `Exists`, which
//! asks only whether the attribute is reported at all, is a group's criterion rather than a
//! query's filter). An aggregator counts the selected
//! nodes per distinct value of the attribute its expression names, a nested aggregator
//! refines each of those counts into the counts of its own attribute within them, and a
//! group counts the selected nodes its filters match beside the ones they do not ("Groups
//! return a count of resources that match and don't match the specified criteria" —
//! `InventoryAggregator$Groups`). A count belongs to no node, so the counts are reported as
//! the one entity whose `Id` is empty.
//!
//! A page of the per-node report holds `MaxResults` entities (the modelled range 1..50) and
//! the `NextToken` is the id of the node the next page starts at — the cursor every node
//! report of this service hands out — which is total because the node store is keyed by
//! `(region, node id)` and therefore walked in ascending node-id order.
//!
//! An AGGREGATION that counted nothing — a fleet reporting no value of the attribute it
//! counts over — still reports the type it is about, holding an empty content list
//! ([`empty_result_entity`]): the aggregated entity is the answer to the question the
//! aggregation asked, and "nothing was counted" is an answer to it. A ROW query that selected
//! no node reports no entity: there is no node's inventory to report.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `GetInventory::new(engine)` and drives it through the centralized request walk:
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
//! # Modeled contract for `GetInventory`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json")
//! ```
//!
//! Query inventory information. This includes managed node status, such as Stopped or
//! Terminated.
//!
//! ## Input
//!
//! * `Aggregators` (InventoryAggregatorList, len 1..10) — Returns counts of inventory types
//!   based on one or more expressions. For example, if you aggregate by using an expression
//!   that uses the AWS:InstanceInformation.PlatformType type, you can see a count of how many
//!   Windows and Linux managed nodes exist in your inventoried fleet.
//! * `Filters` (InventoryFilterList, len 1..5) — One or more filters. Use a filter to return
//!   a more specific list of results.
//! * `MaxResults` (MaxResults, range 1..50) — The maximum number of items to return for this
//!   call. The call also returns a token that you can specify in a subsequent call to get the
//!   next set of results.
//! * `NextToken` (NextToken) — The token for the next set of items to return. (You received
//!   this token from a previous call.)
//! * `ResultAttributes` (ResultAttributeList, len 1..1) — The list of inventory item types to
//!   return.
//!
//! ## Output
//!
//! * `Entities` (InventoryResultEntityList) — Collection of inventory entities such as a
//!   collection of managed node inventory.
//! * `NextToken` (NextToken) — The token to use when requesting the next set of items. If
//!   there are no additional items to return, the string is empty.
//!
//! ## Errors
//!
//! * `InternalServerError` (HTTP 500) — An error occurred on the server side.
//! * `InvalidAggregatorException` (HTTP 400) — The specified aggregator isn't valid for the
//!   group type. Verify that the aggregator you provided is supported.
//! * `InvalidFilter` (HTTP 400) — The filter name isn't valid. Verify that you entered the
//!   correct name and try again.
//! * `InvalidInventoryGroupException` (HTTP 400) — The specified inventory group isn't valid.
//! * `InvalidNextToken` (HTTP 400) — The specified token isn't valid.
//! * `InvalidResultAttributeException` (HTTP 400) — The specified inventory item result
//!   attribute isn't valid.
//! * `InvalidTypeNameException` (HTTP 400) — The parameter type name isn't valid.

#[allow(unused_imports)]
use super::types::*;

use crate::state::inventory::{AggregatorRejection, FilterKeyRejection};

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetInventoryError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ssm-amazonssm.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetInventoryError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ssm#GetInventory")]
#[derive(Debug)]
pub enum GetInventoryErrorModeled {
    InternalServerError(InternalServerError),
    InvalidAggregatorException(InvalidAggregatorException),
    InvalidFilter(InvalidFilter),
    InvalidInventoryGroupException(InvalidInventoryGroupException),
    InvalidNextToken(InvalidNextToken),
    InvalidResultAttributeException(InvalidResultAttributeException),
    InvalidTypeNameException(InvalidTypeNameException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_inventory::GetInventoryError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetInventoryError = crate::scaffolded::Scaffolded<GetInventoryErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetInventoryErrorModeled> for GetInventoryError {
    fn from(modeled: GetInventoryErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetInventory` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetInventory<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetInventory<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetInventory<Eng>
{
    type Input = GetInventoryInput;
    type Output = GetInventoryOutput;
    type Error = GetInventoryError;

    const NAME: &'static str = "GetInventory";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_INVENTORY_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_INVENTORY_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state::inventory::{
            filter_key_rejection, inventory_page_size, is_supported_result_type, matches_filter,
            unsupported_result_type_message, EXISTS_ONLY_IN_AGGREGATOR_FILTERS,
            INVENTORY_TYPE_INSTANCE_INFORMATION, OPERATOR_EXISTS,
        };
        use crate::state::{managed_node_id_is_well_formed, request_region, INVALID_NEXT_TOKEN};

        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(GetInventoryErrorModeled::InternalServerError(InternalServerError {
                message: Some("this service is mounted on an engine that holds no state".to_string()),
            })
            .into());
        };

        // A key that names no attribute of an inventory type cannot be served at all, so
        // the keys are validated — and the request refused — before anything is asked
        // about the nodes.
        let filters = input.filters.as_deref().unwrap_or_default();
        for filter in filters {
            if let Some(rejection) = filter_key_rejection(&filter.key) {
                return Err(filter_key_rejected(rejection));
            }
            // "The `Exists` filter must be used with aggregators" (`InventoryFilter$Type`):
            // asking whether an attribute is reported at all is a criterion an aggregator's
            // groups are defined by rather than a way of selecting the nodes a query
            // reports, so a query's own filters may not name it.
            if filter.r#type.as_deref() == Some(OPERATOR_EXISTS) {
                return Err(invalid_filter(EXISTS_ONLY_IN_AGGREGATOR_FILTERS));
            }
        }

        let aggregators = input.aggregators.as_deref().unwrap_or_default();
        check_aggregators(aggregators, 1)?;

        // "The list of inventory item types to return" is one type (the modelled list is
        // `len 1..1`), and a request naming none reads the default: "Valid value:
        // AWS:InstanceInformation. Default Value: AWS:InstanceInformation"
        // (`ResultAttribute$TypeName`).
        let reported_type = input
            .result_attributes
            .as_deref()
            .unwrap_or_default()
            .first()
            .map_or(INVENTORY_TYPE_INSTANCE_INFORMATION, |attribute| {
                attribute.type_name.as_str()
            });
        if !is_supported_result_type(reported_type) {
            return Err(GetInventoryErrorModeled::InvalidResultAttributeException(
                InvalidResultAttributeException {
                    message: Some(unsupported_result_type_message(reported_type)),
                },
            )
            .into());
        }

        let page_size =
            inventory_page_size(input.max_results).map_err(crate::scaffolded::Scaffolded::validation)?;

        // The token this query hands out is the id of the node the next page starts at, so
        // a token that is not shaped like a node id was not one of ours.
        let resume_from = match input.next_token.as_deref() {
            None | Some("") => None,
            Some(token) if managed_node_id_is_well_formed(token) => Some(token),
            Some(_) => {
                return Err(GetInventoryErrorModeled::InvalidNextToken(InvalidNextToken {
                    message: Some(INVALID_NEXT_TOKEN.to_string()),
                })
                .into())
            }
        };

        // Systems Manager is regional: a node's inventory is read through the endpoint it
        // registered with, which is the Region this request was signed for. The store
        // yields that Region's nodes in ascending node-id order, which is what makes the
        // resume cursor total.
        //
        // The nodes the filters select are SNAPSHOT here and the store released again
        // before anything is counted: an aggregation walks the selected nodes once per
        // aggregation it was asked for, and it must not hold the one store while it does.
        let region = request_region(ctx.parts());
        // The registration an instance of this Region had already made by now was owed at
        // the same instant whether or not anybody called, so it is materialised before the
        // store is read — with no guard of it held
        // ([`crate::state::node_registration::settle_registrations`]).
        crate::state::node_registration::settle_registrations(&*self.engine, &region);
        let snapshot: Vec<(String, crate::state::ManagedNodeRecord)> = {
            let store = state.lock();
            store
                .managed_nodes_in_region(&region)
                .filter(|(_, node)| {
                    filters.iter().all(|filter| {
                        matches_filter(node, &filter.key, &filter.values, filter.r#type.as_deref())
                    })
                })
                .map(|(id, node)| (id.to_string(), node.clone()))
                .collect()
        };
        let selected: Vec<(&str, &crate::state::ManagedNodeRecord)> = snapshot
            .iter()
            .map(|(id, node)| (id.as_str(), node))
            .collect();

        if !aggregators.is_empty() {
            // An aggregated count is about the fleet rather than about one node, so the
            // counts of every aggregator the request named are reported together — and a
            // fleet that reports no value of the counted attribute counts nothing, which is
            // the same no-row answer a row query that selected nothing gives, for the type
            // the aggregation is about.
            let mut entities = aggregated_entities(&selected, aggregators)?;
            if entities.is_empty() {
                entities.push(empty_result_entity(aggregated_type(aggregators)));
            }
            return Ok(GetInventoryOutput {
                entities: Some(entities),
                next_token: None,
            });
        }

        let mut entities = Vec::new();
        let mut next_token = None;
        // Only the node's own information is collected for a node here; another type this
        // query projects has had nothing reported for it, so it is reported as holding
        // nothing rather than refused.
        if reported_type == INVENTORY_TYPE_INSTANCE_INFORMATION {
            for (id, node) in selected {
                if resume_from.is_some_and(|from| id < from) {
                    continue;
                }
                if entities.len() == page_size {
                    // The page is full and a node is still waiting: hand the caller the id
                    // it resumes at, so the next request continues where this one stopped.
                    next_token = Some(id.to_string());
                    break;
                }
                entities.push(instance_information_entity(id, node));
            }
        }

        Ok(GetInventoryOutput {
            entities: Some(entities),
            next_token,
        })
    }
}

/// How far a request may nest aggregators inside aggregators. The model bounds the length
/// of an aggregator list (`len 1..10`) but not the depth of the nesting, and the depth is
/// what the aggregation walk below recurses on, so this emulator bounds it itself and
/// refuses a request past it rather than walking a depth a caller chose.
pub const MAX_AGGREGATOR_NESTING: usize = 10;

/// The modelled refusal of a filter key this operation cannot serve.
fn invalid_filter(why: &str) -> GetInventoryError {
    GetInventoryErrorModeled::InvalidFilter(InvalidFilter {
        message: Some(why.to_string()),
    })
    .into()
}

/// The modelled refusal of an aggregator this operation cannot count over.
fn invalid_aggregator(why: &str) -> GetInventoryError {
    GetInventoryErrorModeled::InvalidAggregatorException(InvalidAggregatorException {
        message: Some(why.to_string()),
    })
    .into()
}

/// The modelled refusal of a filter key this operation cannot serve, which is one of two
/// error classes: a key whose type-name part names no inventory type is a TYPE-NAME fault
/// (the modelled type-name refusal, naming the type name and the key it was read from), while
/// a key that is not a key at all, or that names an attribute outside the schema of the type
/// it names, is the filter refusal.
fn filter_key_rejected(rejection: FilterKeyRejection) -> GetInventoryError {
    use crate::state::inventory::{invalid_filter_key_message, undefined_attribute_message};
    use crate::state::inventory_catalog::invalid_filter_key_type_name_message;

    match rejection {
        FilterKeyRejection::NotAKey(key) => invalid_filter(&invalid_filter_key_message(&key)),
        FilterKeyRejection::UnknownTypeName { type_name, key } => {
            GetInventoryErrorModeled::InvalidTypeNameException(InvalidTypeNameException {
                message: Some(invalid_filter_key_type_name_message(&type_name, &key)),
            })
            .into()
        }
        FilterKeyRejection::UnknownAttribute(attribute) => {
            invalid_filter(&undefined_attribute_message(&attribute))
        }
    }
}

/// The modelled refusal of an aggregator expression this operation cannot count over, which
/// is one of three distinct refusals: the type name an expression carries and the attribute
/// it names are two different things to get wrong, so a name that names no inventory type is
/// the modelled type-name refusal (naming it, and the rules a name has to satisfy) while an
/// attribute its type supports no aggregation on is the aggregator refusal naming that
/// attribute and type.
fn aggregator_rejection(rejection: AggregatorRejection) -> GetInventoryError {
    use crate::state::inventory::{invalid_expression_message, not_aggregatable_message};
    use crate::state::inventory_catalog::invalid_type_name_message;

    match rejection {
        AggregatorRejection::UnknownTypeName(type_name) => {
            GetInventoryErrorModeled::InvalidTypeNameException(InvalidTypeNameException {
                message: Some(invalid_type_name_message(&type_name)),
            })
            .into()
        }
        AggregatorRejection::AttributeNotAggregatable {
            type_name,
            attribute,
        } => invalid_aggregator(&not_aggregatable_message(&type_name, &attribute)),
        AggregatorRejection::NotAnExpression(expression) => {
            invalid_aggregator(&invalid_expression_message(&expression))
        }
    }
}

/// The inventory type an aggregation is ABOUT: the type its first aggregator names, which is
/// the type the entries it counts are reported under — the expression's type name, or the type
/// the first group's criteria are about, and the default type when it names neither (the same
/// type the aggregation walk counts a group under).
fn aggregated_type(aggregators: &[InventoryAggregator]) -> &str {
    use crate::state::inventory::{split_qualified_attribute, INVENTORY_TYPE_INSTANCE_INFORMATION};

    aggregators
        .first()
        .and_then(|aggregator| {
            let expression = crate::state::non_empty(aggregator.expression.as_deref());
            let group_key = aggregator
                .groups
                .as_deref()
                .unwrap_or_default()
                .first()
                .and_then(|group| group.filters.first())
                .map(|filter| filter.key.as_str());
            expression.or(group_key)
        })
        .and_then(split_qualified_attribute)
        .map_or(INVENTORY_TYPE_INSTANCE_INFORMATION, |(type_name, _)| {
            type_name
        })
}

/// The one entity a valid query that selected no row of the type it asked for reports: the
/// queried type carrying an empty content list.
///
/// It belongs to no node — "for managed node inventory the result will be the managed node
/// ID" (`InventoryResultEntity$Id`), and an empty row set is the inventory of no node — so it
/// carries no `Id`, and the schema version is the one the type publishes
/// ([`crate::state::inventory_catalog`]) since no content of it was collected to report the
/// version of.
fn empty_result_entity(type_name: &str) -> InventoryResultEntity {
    use crate::state::inventory::INSTANCE_INFORMATION_SCHEMA_VERSION;
    use crate::state::inventory_catalog::published_type;

    let schema_version = published_type(type_name)
        .map_or(INSTANCE_INFORMATION_SCHEMA_VERSION, |published| {
            published.version
        });
    let item = InventoryResultItem {
        type_name: type_name.to_string(),
        schema_version: schema_version.to_string(),
        capture_time: None,
        content_hash: None,
        content: Vec::new(),
    };
    InventoryResultEntity {
        id: None,
        data: Some(std::collections::HashMap::from([(
            type_name.to_string(),
            item,
        )])),
    }
}

/// Refuse every aggregator of `aggregators`, and of the aggregators nested inside them,
/// that this service cannot count over, answering how many aggregated entries the list
/// asks for. `depth` is the nesting level the list sits at, counted from one.
///
/// An aggregator aggregates EITHER over the values of the attribute its expression names
/// or over the named groups its filters define, so one that names neither has nothing to
/// count over and one that names both names two aggregations where its result holds one.
/// A group's filter keys are filter keys and are held to the same rule the request's own
/// are.
///
/// HOW MUCH a list asks for is counted here, not discovered while counting: each group of
/// an aggregator is an entry of the result, and the entries of a nested list are asked for
/// once per group of the aggregator that nests it, so the entries MULTIPLY with the nesting
/// while the request that asks for them grows by one line. A list asking for more entries
/// than an inventory item may carry ([`crate::state::inventory::MAX_AGGREGATED_ENTRIES`])
/// is refused here, before a single node is read, so the work the walk below does is
/// bounded by that one number rather than by the depth and width a caller chose.
fn check_aggregators(
    aggregators: &[InventoryAggregator],
    depth: usize,
) -> Result<usize, GetInventoryError> {
    use crate::state::inventory::{
        aggregator_expression_rejection, filter_key_rejection, INVALID_AGGREGATOR,
        MAX_AGGREGATED_ENTRIES,
    };


    if depth > MAX_AGGREGATOR_NESTING {
        return Err(invalid_aggregator(INVALID_AGGREGATOR));
    }
    let mut entries = 0usize;
    for aggregator in aggregators {
        let expression = crate::state::non_empty(aggregator.expression.as_deref());
        let groups = aggregator.groups.as_deref().unwrap_or_default();
        match (expression, groups.is_empty()) {
            (Some(expression), true) => {
                if let Some(rejection) = aggregator_expression_rejection(expression) {
                    return Err(aggregator_rejection(rejection));
                }
            }
            (None, false) => {
                for group in groups {
                    for filter in &group.filters {
                        if let Some(rejection) = filter_key_rejection(&filter.key) {
                            return Err(filter_key_rejected(rejection));
                        }
                    }
                }
            }
            _ => return Err(invalid_aggregator(INVALID_AGGREGATOR)),
        }
        let nested = check_aggregators(aggregator.aggregators.as_deref().unwrap_or_default(), depth + 1)?;
        // One entry per group, or per value of the aggregated attribute — of which a node
        // holds one, so an expression asks for as many entries as the fleet has values
        // rather than for a count of its own. A nested list is asked for once per entry of
        // the aggregator that nests it.
        let asked_for = groups.len().max(1).saturating_mul(nested.max(1));
        entries = entries.saturating_add(asked_for);
    }
    if entries > MAX_AGGREGATED_ENTRIES {
        return Err(invalid_aggregator(INVALID_AGGREGATOR));
    }
    Ok(entries)
}

/// One node's inventory: the `AWS:InstanceInformation` item projected from the record it
/// registered with, under the node's own id — "for managed node inventory the result will
/// be the managed node ID" (`InventoryResultEntity$Id`).
fn instance_information_entity(
    id: &str,
    node: &crate::state::ManagedNodeRecord,
) -> InventoryResultEntity {
    use crate::state::inventory::{
        capture_time, instance_information_content, INSTANCE_INFORMATION_SCHEMA_VERSION,
        INVENTORY_TYPE_INSTANCE_INFORMATION,
    };

    let item = InventoryResultItem {
        type_name: INVENTORY_TYPE_INSTANCE_INFORMATION.to_string(),
        schema_version: INSTANCE_INFORMATION_SCHEMA_VERSION.to_string(),
        capture_time: capture_time(node),
        content_hash: None,
        content: vec![instance_information_content(node)],
    };
    InventoryResultEntity {
        id: Some(id.to_string()),
        data: Some(std::collections::HashMap::from([(
            INVENTORY_TYPE_INSTANCE_INFORMATION.to_string(),
            item,
        )])),
    }
}

/// The counts `aggregators` asks for over the nodes `selected`, as the one entity that
/// carries them — or no entity at all when the aggregation counted nothing, which is what
/// a fleet that reports none of the aggregated attribute holds.
///
/// The walk counts no more entries than an inventory item may carry
/// ([`crate::state::inventory::MAX_AGGREGATED_ENTRIES`]) and refuses the request when the
/// fleet turns the aggregation the request asked for into more than that, so what is
/// counted is bounded by that one number whatever the fleet holds.
fn aggregated_entities(
    selected: &[(&str, &crate::state::ManagedNodeRecord)],
    aggregators: &[InventoryAggregator],
) -> Result<Vec<InventoryResultEntity>, GetInventoryError> {
    use crate::state::inventory::{
        AGGREGATED_ENTITY_ID, INSTANCE_INFORMATION_SCHEMA_VERSION, MAX_AGGREGATED_ENTRIES,
    };

    let mut counted: std::collections::BTreeMap<String, Vec<AggregatedEntry>> =
        std::collections::BTreeMap::new();
    let mut remaining = MAX_AGGREGATED_ENTRIES;
    aggregate(
        selected,
        aggregators,
        &Vec::new(),
        &mut counted,
        &mut remaining,
    )?;
    if counted.is_empty() {
        return Ok(Vec::new());
    }
    let data = counted
        .into_iter()
        .map(|(type_name, entries)| {
            let item = InventoryResultItem {
                type_name: type_name.clone(),
                schema_version: INSTANCE_INFORMATION_SCHEMA_VERSION.to_string(),
                capture_time: None,
                content_hash: None,
                content: entries
                    .into_iter()
                    .map(|entry| entry.into_iter().collect())
                    .collect(),
            };
            (type_name, item)
        })
        .collect();
    Ok(vec![InventoryResultEntity {
        id: Some(AGGREGATED_ENTITY_ID.to_string()),
        data: Some(data),
    }])
}

/// One aggregated entry under construction: the attribute names and values it has
/// accumulated, in the order the aggregation added them, so a refined count reports the
/// value it was refined from beside its own.
type AggregatedEntry = Vec<(String, String)>;

/// Count the nodes of `nodes` as `aggregators` asks, appending each entry to the content of
/// the inventory type the aggregator is about. `carried` is what the enclosing aggregation
/// already put in every entry this one produces, and `remaining` is how many more entries
/// the whole query may still count.
///
/// An aggregator with nested aggregators reports the REFINED counts — "nested aggregators
/// to further refine aggregation for an inventory type" (`InventoryAggregator$Aggregators`)
/// — so each NON-EMPTY subset it isolates is handed to them instead of being counted
/// itself; a subset holding no node has nothing left to refine and is reported as the count
/// it is. Every entry counted spends one of `remaining`, and a query whose fleet turns its
/// aggregation into more entries than that is refused rather than half-counted.
fn aggregate(
    nodes: &[(&str, &crate::state::ManagedNodeRecord)],
    aggregators: &[InventoryAggregator],
    carried: &AggregatedEntry,
    counted: &mut std::collections::BTreeMap<String, Vec<AggregatedEntry>>,
    remaining: &mut usize,
) -> Result<(), GetInventoryError> {
    use crate::state::inventory::{
        instance_information_attribute, matches_filter, split_qualified_attribute,
        AGGREGATION_COUNT_KEY, GROUP_COMPLEMENT_PREFIX, INVALID_AGGREGATOR,
        INVENTORY_TYPE_INSTANCE_INFORMATION,
    };

    // One entry counted, or the refusal of a query asking for more than this service
    // counts.
    let count = |type_name: &str,
                     entry: AggregatedEntry,
                     counted: &mut std::collections::BTreeMap<String, Vec<AggregatedEntry>>,
                     remaining: &mut usize|
     -> Result<(), GetInventoryError> {
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| invalid_aggregator(INVALID_AGGREGATOR))?;
        counted.entry(type_name.to_string()).or_default().push(entry);
        Ok(())
    };

    for aggregator in aggregators {
        let nested = aggregator.aggregators.as_deref().unwrap_or_default();
        if let Some(expression) = crate::state::non_empty(aggregator.expression.as_deref()) {
            let Some((type_name, attribute)) = split_qualified_attribute(expression) else {
                continue;
            };
            // The nodes that report each distinct value of the aggregated attribute, in
            // ascending value order; a node reporting no value for it is counted under no
            // value. Only the node's own information is collected here, so an expression
            // over another type counts the nothing that type holds.
            let mut groups: std::collections::BTreeMap<&str, Vec<(&str, &crate::state::ManagedNodeRecord)>> =
                std::collections::BTreeMap::new();
            if type_name == INVENTORY_TYPE_INSTANCE_INFORMATION {
                for (id, node) in nodes {
                    if let Some(value) = instance_information_attribute(node, attribute) {
                        groups.entry(value).or_default().push((id, node));
                    }
                }
            }
            for (value, subset) in groups {
                let mut entry = carried.clone();
                entry.push((attribute.to_string(), value.to_string()));
                if nested.is_empty() {
                    entry.push((AGGREGATION_COUNT_KEY.to_string(), subset.len().to_string()));
                    count(type_name, entry, counted, remaining)?;
                } else {
                    aggregate(&subset, nested, &entry, counted, remaining)?;
                }
            }
            continue;
        }
        for group in aggregator.groups.as_deref().unwrap_or_default() {
            let matched: Vec<(&str, &crate::state::ManagedNodeRecord)> = nodes
                .iter()
                .filter(|(_, node)| {
                    group.filters.iter().all(|filter| {
                        matches_filter(node, &filter.key, &filter.values, filter.r#type.as_deref())
                    })
                })
                .copied()
                .collect();
            let mut entry = carried.clone();
            entry.push((group.name.clone(), matched.len().to_string()));
            entry.push((
                format!("{GROUP_COMPLEMENT_PREFIX}{}", group.name),
                (nodes.len() - matched.len()).to_string(),
            ));
            if nested.is_empty() || matched.is_empty() {
                // The type the group's criteria are about: the one its filters name.
                let type_name = group
                    .filters
                    .first()
                    .and_then(|filter| split_qualified_attribute(&filter.key))
                    .map_or(INVENTORY_TYPE_INSTANCE_INFORMATION, |(type_name, _)| type_name);
                count(type_name, entry, counted, remaining)?;
            } else {
                aggregate(&matched, nested, &entry, counted, remaining)?;
            }
        }
    }
    Ok(())
}
