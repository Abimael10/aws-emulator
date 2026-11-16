// SPDX-License-Identifier: CC0-1.0

use super::*;

/// The event source types and their categories, as `DescribeEventCategories` reports
/// them.
///
/// It is the SHARED catalog of the one front door
/// ([`::service_common::rds_event_categories::EVENT_CATEGORIES`]), re-exported here rather
/// than restated: Amazon RDS, Amazon DocumentDB and Amazon Neptune are three views of that
/// one door and the report answers all three from one table, so no view can publish rows,
/// or an order, the others do not.
pub use ::service_common::rds_event_categories::{event_categories_of, EVENT_CATEGORIES};

/// The message the refusal of a `SourceType` that names no published event source type
/// carries.
///
/// The set of source types the front door accepts is wider than the ones this report
/// publishes a category map for, and the refusal names the whole of it, so it is the shared
/// `service_common::rds_event_source_types` list — one set for every service view this one
/// front door serves — that this sentence comes from.
pub fn invalid_event_source_type_message() -> String {
    ::service_common::rds_event_source_types::invalid_event_source_type_message()
}

/// One source type's categories as the modelled `EventCategoriesMap`.
pub fn event_categories_to_wire(source_type: &str, categories: &[&str]) -> EventCategoriesMap {
    EventCategoriesMap {
        source_type: Some(source_type.to_string()),
        event_categories: Some(
            categories
                .iter()
                .map(|category| (*category).to_string())
                .collect(),
        ),
    }
}

/// The source types an event notification SUBSCRIPTION may name.
///
/// The set is the one the model's `SourceType` enum carries
/// (`crate::ops::types::SOURCE_TYPE_VALUES`), which is `db-shard-group` wider than the
/// list `CreateEventSubscriptionMessage$SourceType`'s prose spells out; the ORDER is the
/// one Amazon RDS lists the valid types in when it refuses a value outside the set
/// ([`invalid_event_subscription_source_type_message`]), so the refusal names them exactly
/// as the service does.
///
/// Wider than the source types [`EVENT_CATEGORIES`] carries a category map for, which is
/// the narrower list `DescribeEventCategories`'s own member documents: a subscription may
/// be created for a source type whose published categories this emulator does not have,
/// and [`check_event_categories`] says what that means for the categories such a
/// subscription may name.
pub const EVENT_SUBSCRIPTION_SOURCE_TYPES: [&str; 11] = [
    "db-instance",
    "db-security-group",
    "db-parameter-group",
    "db-snapshot",
    "db-cluster",
    "db-cluster-snapshot",
    "custom-engine-version",
    "db-proxy",
    "blue-green-deployment",
    "db-shard-group",
    "zero-etl",
];

/// The message Amazon RDS refuses a `SourceType` no subscription can be created for with.
///
/// A statement about the PARAMETER and not about the value: the fixed sentence says the
/// source type is not valid and then names the types that ARE valid — it echoes nothing
/// from the request, so neither does this. Each type is named in single quotes, the way the
/// service writes a list of literal values inside a sentence, in the order
/// [`EVENT_SUBSCRIPTION_SOURCE_TYPES`] carries them, and the sentence ends with that list
/// (no trailing period). The sibling refusal a `DescribeEvents` source type gets is worded
/// with one more clause ([`invalid_event_source_type_message`]) and over the narrower set
/// that report covers.
pub fn invalid_event_subscription_source_type_message() -> String {
    let quoted: Vec<String> = EVENT_SUBSCRIPTION_SOURCE_TYPES
        .iter()
        .map(|source_type| format!("'{source_type}'"))
        .collect();
    format!("Invalid event source type. Valid types are {}", quoted.join(", "))
}

/// How Amazon RDS refuses a request that names event categories without the source type
/// they belong to: ONE fixed sentence, which is why it is a constant — it echoes neither
/// the categories nor the subscription, so the same text answers every such request.
pub const CATEGORIES_NEED_A_SOURCE_TYPE: &str =
    "If SourceType is null, Categories must also be null.";

/// The constraint `CreateEventSubscriptionMessage$SourceIds` states in those words: "If
/// `SourceIds` are supplied, `SourceType` must also be provided." The ids name resources of
/// one source type, so there is nothing to resolve them against without it.
pub const SOURCE_IDS_NEED_A_SOURCE_TYPE: &str =
    "If SourceIds are supplied, SourceType must also be provided.";

/// Refuse a `SourceType` a subscription cannot be created for; the caller answers it with
/// the Query API's `InvalidParameterValue`.
pub fn check_event_subscription_source_type(source_type: &str) -> Result<(), String> {
    if EVENT_SUBSCRIPTION_SOURCE_TYPES.contains(&source_type) {
        return Ok(());
    }
    Err(invalid_event_subscription_source_type_message())
}

/// The `SourceNotFoundFault` this service answers a request that names an event source
/// which does not exist, built HERE so every handler that resolves a source identifier
/// words the one fault the same way.
///
/// Its text is fixed — `Could not find source :` — and its only variable part is the
/// identifier the request supplied, carried verbatim: RDS interpolates neither the source
/// type the identifier was resolved against nor the region the call was signed for, and
/// appends no trailing period.
pub fn source_not_found(source_identifier: &str) -> SourceNotFoundFault {
    SourceNotFoundFault {
        message: Some(format!("Could not find source :{source_identifier}")),
    }
}

/// The categories of `source_type` a subscription may subscribe to, with the one that
/// is not among them, if any: `EventCategories` is "a list of event categories for a
/// particular source type (SourceType) that you want to subscribe to", so a category
/// that source type does not publish names no events at all.
///
/// A source type [`EVENT_CATEGORIES`] carries no published map for is not checked —
/// there is nothing to check it against, and refusing every category for it would refuse
/// the ones AWS accepts. Matching is exact: the published categories are all lowercase.
pub fn check_event_categories(source_type: &str, categories: &[String]) -> Result<(), String> {
    let Some(published) = event_categories_of(source_type) else {
        return Ok(());
    };
    for category in categories {
        if !published.contains(&category.as_str()) {
            return Err(category_not_found_for_source_type_message(
                category,
                source_type,
            ));
        }
    }
    Ok(())
}

/// How this front door words the refusal of a category a source type does not publish: it
/// reports that the CATEGORY could not be found FOR that source type and names both, and
/// says nothing about the categories the type does publish.
///
/// Everything but the two names is fixed text — including the space before the colon — and
/// each is carried as the request spelled it. The caller answers it with the modelled
/// `SubscriptionCategoryNotFoundFault` (HTTP 404).
pub fn category_not_found_for_source_type_message(category: &str, source_type: &str) -> String {
    format!("Category : {category} not found for source type {source_type}.")
}

