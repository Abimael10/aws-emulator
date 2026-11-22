// SPDX-License-Identifier: CC0-1.0

//! What a SUBSCRIPTION field's own resolver decides about the subscriptions on it: which
//! publications each subscriber receives, and which subscriptions a mutation ENDS.
//!
//! A subscription field may carry a resolver like any other field, and AppSync runs that
//! resolver's REQUEST half when a subscriber subscribes — before the subscription is
//! registered — so the handler decides the subscription itself rather than a value:
//!
//! * `extensions.setSubscriptionFilter(filter)` installs the filter the service then
//!   evaluates against every publication on the field, delivering only the publications it
//!   accepts ([`accepts`]);
//! * `extensions.setSubscriptionInvalidationFilter(filter)` installs the filter that says
//!   whether an INVALIDATION a mutation raises ends this subscription;
//! * a handler that refuses (`util.unauthorized`, `util.error`) refuses the subscribe, and
//!   so does code the runtime cannot read: a subscription that could not be decided is not
//!   registered unfiltered.
//!
//! `extensions.invalidateSubscriptions({subscriptionField, payload})` is stated by a
//! MUTATION's response half instead, and ends exactly those subscriptions of that field
//! whose invalidation filter accepts the payload: each is sent the `complete` frame a
//! subscription that is over carries, and is forgotten, so nothing more is delivered to it.
//!
//! A filter is a `filterGroup`: a list of filter objects, each carrying `filters` of
//! `{fieldName, operator, value}`. A publication satisfies the group when it satisfies ANY
//! of its objects, and it satisfies an object when EVERY filter of that object holds — which
//! is how the AppSync developer guide's enhanced-subscription-filtering section states it.
//! `fieldName` addresses a member of the publication, nested members with `.` between the
//! names.

use ::serde_json::{Map as JsonMap, Value as Json};

use super::PlaneContext;
use crate::ops::evaluate_code::appsync_js::{
    self, ExtensionCall, Outcome, ARGUMENTS_MEMBER, ARGUMENTS_SHORT_MEMBER,
    INVALIDATE_SUBSCRIPTIONS, SET_SUBSCRIPTION_FILTER, SET_SUBSCRIPTION_INVALIDATION_FILTER,
};

/// The member a filter states its group of filter objects in.
const FILTER_GROUP_MEMBER: &str = "filterGroup";

/// The members one filter object and one filter carry.
const FILTERS_MEMBER: &str = "filters";
const FIELD_NAME_MEMBER: &str = "fieldName";
const OPERATOR_MEMBER: &str = "operator";
const VALUE_MEMBER: &str = "value";

/// The members an `invalidateSubscriptions` call states.
const SUBSCRIPTION_FIELD_MEMBER: &str = "subscriptionField";
const PAYLOAD_MEMBER: &str = "payload";

/// What a subscribe-time handler installed on the subscription it decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Installed {
    /// The filter every publication on the field is judged by before it is delivered to this
    /// subscription, as the handler stated its `filterGroup`.
    pub filter: Option<Json>,
    /// The filter an invalidation's payload is judged by before it ends this subscription.
    pub invalidation_filter: Option<Json>,
}

/// Why a subscribe-time handler refused the subscribe: the `errorType` the refusal carries
/// and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// Whether the handler refused the SUBSCRIBER rather than the request — which
    /// `util.unauthorized` does — so the endpoint states it as an unauthorized subscription.
    pub unauthorized: bool,
    /// What the refusal says, as the handler worded it.
    pub message: String,
}

/// Run the subscribe-time half of `field`'s resolver, if the API holds one defined by
/// APPSYNC_JS code, and answer what it installed on the subscription.
///
/// `Ok(Installed::default())` is a field whose resolver installs nothing — including a field
/// that carries no resolver at all, and one defined by mapping templates, which state no
/// `extensions` call. `Err` is a handler that refused the subscribe, which is what keeps an
/// undecided subscription out of the registry.
///
/// The resolver's definition is copied out of the store under a lock of its own, and the code
/// runs with no lock held: a subscriber's own handler cannot hold up the requests of every
/// other connection to this service.
pub async fn subscribe_time(
    context: &PlaneContext,
    root_type: &str,
    field: &str,
    arguments: &JsonMap<String, Json>,
) -> Result<Installed, Refused> {
    let code = {
        let store = context.state.lock();
        store
            .graphql_apis
            .get(&context.api_id)
            .and_then(|api| api.resolver(root_type, field))
            .and_then(|resolver| resolver.code.clone())
    };
    let Some(code) = code.filter(|code| !code.trim().is_empty()) else {
        return Ok(Installed::default());
    };
    let context_document = subscribe_context(root_type, field, arguments);
    let evaluation = appsync_js::evaluate(code, REQUEST_FUNCTION.to_string(), context_document).await;
    match evaluation.outcome {
        // The value a subscribe-time request half returns configures nothing this plane
        // calls: what it DECIDED is what it installed.
        Outcome::Returned(_) | Outcome::ReturnedEarly(_) => Ok(installed(&evaluation.extensions)),
        Outcome::Raised {
            message,
            refused_the_field,
            ..
        } => Err(Refused {
            unauthorized: refused_the_field,
            message,
        }),
        Outcome::Stopped(message) => Err(Refused {
            unauthorized: false,
            message,
        }),
        Outcome::Faults(faults) => Err(Refused {
            unauthorized: false,
            message: faults
                .first()
                .map(|fault| fault.value.clone())
                .unwrap_or_else(|| NO_DEFINITION.to_string()),
        }),
    }
}

/// The name of the resolver half that runs when a subscriber subscribes.
const REQUEST_FUNCTION: &str = "request";

/// What a resolver whose code the runtime could not read at all is refused with.
const NO_DEFINITION: &str = "This subscription's resolver defines no request function.";

/// The `ctx` a subscribe-time handler is run over: the arguments the subscription states
/// (under both names an APPSYNC_JS context carries them), and the field being subscribed to.
///
/// The members are the ones a resolver's own context carries
/// (`crate::service::graphql_api::execution`), so one handler reads the same `ctx` whether it
/// runs for a subscribe or for a field being resolved. Nothing is resolved yet, so `source`,
/// `result` and `prev.result` carry nothing.
fn subscribe_context(root_type: &str, field: &str, arguments: &JsonMap<String, Json>) -> Json {
    let arguments = Json::Object(arguments.clone());
    ::serde_json::json!({
        ARGUMENTS_MEMBER: arguments,
        ARGUMENTS_SHORT_MEMBER: arguments,
        "source": Json::Null,
        "identity": Json::Null,
        "request": {"headers": {}},
        "info": {
            "fieldName": field,
            "parentTypeName": root_type,
        },
        "stash": {},
        "result": Json::Null,
        "prev": {"result": Json::Null},
        "error": Json::Null,
    })
}

/// The filters a run's `extensions` calls installed: the LAST call of each kind, because a
/// handler calling one twice states the filter it ends with.
fn installed(calls: &[ExtensionCall]) -> Installed {
    let group_of = |name: &str| {
        calls
            .iter()
            .rfind(|call| call.name == name)
            .map(|call| filter_group(&call.argument()))
    };
    Installed {
        filter: group_of(SET_SUBSCRIPTION_FILTER),
        invalidation_filter: group_of(SET_SUBSCRIPTION_INVALIDATION_FILTER),
    }
}

/// The group of filter objects a stated filter carries: the `filterGroup` member of the
/// object a handler passed, or the value itself where it passed the group as a list.
fn filter_group(stated: &Json) -> Json {
    match stated.get(FILTER_GROUP_MEMBER) {
        Some(group) => group.clone(),
        None => stated.clone(),
    }
}

/// Whether `filter` accepts `published`: a subscription with no filter receives every
/// publication on its field, and one with a filter receives a publication that satisfies ANY
/// of the filter objects of its group.
///
/// A group with no filter object at all accepts nothing: a handler that installed an empty
/// group asked for a subscription nothing satisfies, which is not the same as asking for no
/// filter.
pub fn accepts(filter: Option<&Json>, published: &Json) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let Some(group) = filter.as_array() else {
        return false;
    };
    group.iter().any(|object| object_holds(object, published))
}

/// Whether every filter of one filter object holds for `published`. An object stating no
/// filter holds for nothing, for the reason an empty group accepts nothing.
fn object_holds(object: &Json, published: &Json) -> bool {
    let filters = object
        .get(FILTERS_MEMBER)
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    !filters.is_empty()
        && filters
            .iter()
            .all(|filter| filter_holds(filter, published))
}

/// Whether ONE filter holds: the member of the publication its `fieldName` addresses,
/// compared with its `value` under its `operator`.
fn filter_holds(filter: &Json, published: &Json) -> bool {
    let Some(field_name) = filter.get(FIELD_NAME_MEMBER).and_then(Json::as_str) else {
        return false;
    };
    let Some(operator) = filter.get(OPERATOR_MEMBER).and_then(Json::as_str) else {
        return false;
    };
    let wanted = filter.get(VALUE_MEMBER).unwrap_or(&Json::Null);
    let held = member(published, field_name);
    holds(operator, held, wanted)
}

/// The member `path` addresses of `published`: a member of the publication, nested members
/// with `.` between the names. `None` where the publication carries no such member, which no
/// comparing operator holds for.
fn member<'a>(published: &'a Json, path: &str) -> Option<&'a Json> {
    let mut value = published;
    for name in path.split('.') {
        value = value.get(name)?;
    }
    Some(value)
}

/// Whether `held` satisfies `wanted` under `operator`, by the operator table the AppSync
/// developer guide's enhanced-subscription-filtering section publishes:
/// `eq`, `ne`, `le`, `lt`, `ge`, `gt`, `contains`, `notContains`, `beginsWith`, `in`,
/// `notIn`, `between` and `containsAny`.
///
/// An operator the guide does not publish holds for nothing: a filter this service cannot
/// evaluate is not one it may ignore, because ignoring it would deliver what the subscriber
/// asked not to receive. A publication carrying no such member satisfies no operator either,
/// with the two NEGATED operators the exception they are: a member that is not there holds
/// neither the value nor the element the filter names.
fn holds(operator: &str, held: Option<&Json>, wanted: &Json) -> bool {
    let Some(held) = held else {
        return matches!(operator, "ne" | "notContains" | "notIn");
    };
    match operator {
        "eq" => held == wanted,
        "ne" => held != wanted,
        "le" => ordered(held, wanted).is_some_and(|order| order.is_le()),
        "lt" => ordered(held, wanted).is_some_and(|order| order.is_lt()),
        "ge" => ordered(held, wanted).is_some_and(|order| order.is_ge()),
        "gt" => ordered(held, wanted).is_some_and(|order| order.is_gt()),
        "contains" => contains(held, wanted),
        "notContains" => !contains(held, wanted),
        "beginsWith" => match (held.as_str(), wanted.as_str()) {
            (Some(held), Some(prefix)) => held.starts_with(prefix),
            _ => false,
        },
        "in" => within(held, wanted),
        "notIn" => !within(held, wanted),
        "between" => match wanted.as_array().map(Vec::as_slice) {
            Some([low, high]) => {
                ordered(held, low).is_some_and(|order| order.is_ge())
                    && ordered(held, high).is_some_and(|order| order.is_le())
            }
            _ => false,
        },
        "containsAny" => match (held.as_array(), wanted.as_array()) {
            (Some(held), Some(wanted)) => wanted.iter().any(|element| held.contains(element)),
            _ => false,
        },
        _ => false,
    }
}

/// How `held` orders against `wanted`, for the four comparing operators: two numbers compare
/// as numbers and two strings as text. Values of two different kinds do not compare at all,
/// so no ordering operator holds for them.
fn ordered(held: &Json, wanted: &Json) -> Option<::std::cmp::Ordering> {
    match (held, wanted) {
        (Json::Number(held), Json::Number(wanted)) => held
            .as_f64()
            .zip(wanted.as_f64())
            .and_then(|(held, wanted)| held.partial_cmp(&wanted)),
        (Json::String(held), Json::String(wanted)) => Some(held.cmp(wanted)),
        _ => None,
    }
}

/// Whether `held` contains `wanted`: a substring of a string, or an element of a list.
fn contains(held: &Json, wanted: &Json) -> bool {
    match (held, wanted) {
        (Json::String(held), Json::String(wanted)) => held.contains(wanted.as_str()),
        (Json::Array(held), wanted) => held.contains(wanted),
        _ => false,
    }
}

/// Whether `held` is one of the values the filter listed.
fn within(held: &Json, wanted: &Json) -> bool {
    wanted
        .as_array()
        .is_some_and(|listed| listed.contains(held))
}

/// End every subscription an executed operation's `extensions.invalidateSubscriptions` calls
/// name: those of the field each call names whose own invalidation filter accepts that call's
/// payload.
///
/// A subscription that installed NO invalidation filter is left alone: invalidation reaches
/// the subscriptions whose subscribe-time handler said which payloads end them. Each
/// invalidated subscription is forgotten BEFORE its `complete` frame is queued, so no
/// publication of the same operation can still be delivered to a subscription this call
/// ended, and the frame is written through the queue its connection's own task owns — the
/// same path a delivery takes (`super::subscriptions`).
pub async fn invalidated(context: &PlaneContext, calls: &[ExtensionCall]) {
    let mut ended = false;
    for call in calls
        .iter()
        .filter(|call| call.name == INVALIDATE_SUBSCRIPTIONS)
    {
        let stated = call.argument();
        let Some(field) = stated
            .get(SUBSCRIPTION_FIELD_MEMBER)
            .and_then(Json::as_str)
        else {
            continue;
        };
        let payload = stated.get(PAYLOAD_MEMBER).cloned().unwrap_or(Json::Null);
        let subscriptions = context
            .state
            .lock()
            .graphql_data_plane
            .deliveries(&context.api_id, field);
        for (subscription_id, frames, subscription) in subscriptions {
            // A subscription with no invalidation filter is not invalidated, so the absence
            // of one is not read as a filter that accepts everything.
            let invalidated = subscription
                .invalidation_filter
                .as_ref()
                .is_some_and(|filter| accepts(Some(filter), &payload));
            if !invalidated {
                continue;
            }
            context
                .state
                .lock()
                .graphql_data_plane
                .end_subscription(&context.api_id, &subscription_id);
            ended = true;
            if frames
                .try_send(super::realtime::complete_frame(&subscription_id))
                .is_err()
            {
                ::tracing::warn!(
                    api_id = %context.api_id,
                    %subscription_id,
                    "a subscriber is not reading its socket; an invalidation was not written to it"
                );
            }
            ::tokio::task::yield_now().await;
        }
    }
    // An invalidation that ended subscriptions changed what the subscription GAUGE reads, so
    // the API reports the reading it now holds rather than waiting for the next sample
    // ([`super::metrics`]). It is not an unsubscribe: no subscriber asked for it, so nothing
    // of `UnsubscribeSuccess` is counted for one.
    if ended {
        context.metrics.sampled(context.connection_counts());
    }
}

#[cfg(test)]
mod tests {
    //! The pure rules this module owns: the operator table, and how a group of filter
    //! objects composes. The subscribe-time run and the invalidation are pinned end to end
    //! over a real socket in `tests/all/graphql_api_subscription_filtering.rs`.

    use super::*;
    use ::serde_json::json;

    /// A filter group of ONE object carrying one filter.
    fn one(field_name: &str, operator: &str, value: Json) -> Json {
        json!([{"filters": [
            {"fieldName": field_name, "operator": operator, "value": value},
        ]}])
    }

    /// RULE: every operator the guide publishes decides a filter from the publication's own
    /// member and the filter's value, and an operator it does not publish holds for nothing.
    #[test]
    fn the_operator_table_decides_one_filter() {
        // One row per published operator, each tested on a value it holds for and one it
        // does not, plus the kinds it cannot compare and an operator off the table.
        let rows: &[(&str, &str, Json, Json, bool)] = &[
            ("eq on an equal string", "eq", json!("alpha"), json!("alpha"), true),
            ("eq on a different string", "eq", json!("alpha"), json!("beta"), false),
            ("eq on an equal number", "eq", json!(3), json!(3), true),
            ("ne on a different string", "ne", json!("alpha"), json!("beta"), true),
            ("ne on an equal string", "ne", json!("alpha"), json!("alpha"), false),
            ("lt below the value", "lt", json!(2), json!(3), true),
            ("lt at the value", "lt", json!(3), json!(3), false),
            ("le at the value", "le", json!(3), json!(3), true),
            ("gt above the value", "gt", json!(4), json!(3), true),
            ("gt at the value", "gt", json!(3), json!(3), false),
            ("ge at the value", "ge", json!(3), json!(3), true),
            ("lt across kinds", "lt", json!("3"), json!(3), false),
            ("contains a substring", "contains", json!("alphabet"), json!("pha"), true),
            ("contains a substring it lacks", "contains", json!("alphabet"), json!("zip"), false),
            ("contains a list element", "contains", json!(["a", "b"]), json!("b"), true),
            ("notContains a substring it lacks", "notContains", json!("alphabet"), json!("zip"), true),
            ("notContains a substring it has", "notContains", json!("alphabet"), json!("pha"), false),
            ("beginsWith the prefix", "beginsWith", json!("alpha"), json!("alp"), true),
            ("beginsWith another prefix", "beginsWith", json!("alpha"), json!("bet"), false),
            ("in the listed values", "in", json!("b"), json!(["a", "b"]), true),
            ("in values it is not among", "in", json!("c"), json!(["a", "b"]), false),
            ("notIn values it is not among", "notIn", json!("c"), json!(["a", "b"]), true),
            ("notIn the listed values", "notIn", json!("b"), json!(["a", "b"]), false),
            ("between the two bounds", "between", json!(5), json!([1, 10]), true),
            ("between, at the lower bound", "between", json!(1), json!([1, 10]), true),
            ("between, at the upper bound", "between", json!(10), json!([1, 10]), true),
            ("between, past the upper bound", "between", json!(11), json!([1, 10]), false),
            ("containsAny one shared element", "containsAny", json!(["a", "b"]), json!(["b", "c"]), true),
            ("containsAny no shared element", "containsAny", json!(["a", "b"]), json!(["c"]), false),
            ("an operator off the table", "matches", json!("alpha"), json!("alpha"), false),
        ];
        for (case, operator, held, wanted, expected) in rows {
            let filter = one("channel", operator, wanted.clone());
            let published = json!({"channel": held.clone()});
            let actual = accepts(Some(&filter), &published);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual} for {published} under {filter}"
            );
        }
    }

    /// RULE: a publication satisfies a group when it satisfies ANY of its objects, and an
    /// object when EVERY filter of it holds; no filter at all accepts everything, and an
    /// empty group accepts nothing.
    #[test]
    fn a_group_composes_its_objects_and_their_filters() {
        let two_filters = json!([{"filters": [
            {"fieldName": "channel", "operator": "eq", "value": "alpha"},
            {"fieldName": "level", "operator": "gt", "value": 2},
        ]}]);
        let two_objects = json!([
            {"filters": [{"fieldName": "channel", "operator": "eq", "value": "alpha"}]},
            {"filters": [{"fieldName": "channel", "operator": "eq", "value": "beta"}]},
        ]);
        let nested = json!([{"filters": [
            {"fieldName": "post.channel", "operator": "eq", "value": "alpha"},
        ]}]);
        // The rows differ along how many objects the group has, how many filters an object
        // has, whether the member is nested, and whether a filter is installed at all.
        let rows: &[(&str, Option<Json>, Json, bool)] = &[
            ("no filter at all", None, json!({"channel": "beta"}), true),
            ("an empty group", Some(json!([])), json!({"channel": "alpha"}), false),
            (
                "both filters of one object hold",
                Some(two_filters.clone()),
                json!({"channel": "alpha", "level": 3}),
                true,
            ),
            (
                "one filter of the object fails",
                Some(two_filters.clone()),
                json!({"channel": "alpha", "level": 1}),
                false,
            ),
            (
                "a member the publication lacks",
                Some(two_filters),
                json!({"channel": "alpha"}),
                false,
            ),
            (
                "the first of two objects holds",
                Some(two_objects.clone()),
                json!({"channel": "alpha"}),
                true,
            ),
            (
                "the second of two objects holds",
                Some(two_objects.clone()),
                json!({"channel": "beta"}),
                true,
            ),
            (
                "neither object holds",
                Some(two_objects),
                json!({"channel": "gamma"}),
                false,
            ),
            (
                "a nested member the filter addresses",
                Some(nested.clone()),
                json!({"post": {"channel": "alpha"}}),
                true,
            ),
            (
                "a nested member with another value",
                Some(nested),
                json!({"post": {"channel": "beta"}}),
                false,
            ),
        ];
        for (case, filter, published, expected) in rows {
            let actual = accepts(filter.as_ref(), published);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual} for {published}"
            );
        }
    }

    /// RULE: a handler's stated filter is read from its `filterGroup`, and the LAST call of
    /// each kind is the one installed.
    #[test]
    fn the_last_stated_filter_of_each_kind_is_installed() {
        let calls = vec![
            ExtensionCall {
                name: SET_SUBSCRIPTION_FILTER.to_string(),
                argument: json!({"filterGroup": [{"filters": [
                    {"fieldName": "channel", "operator": "eq", "value": "first"},
                ]}]})
                .to_string(),
            },
            ExtensionCall {
                name: SET_SUBSCRIPTION_FILTER.to_string(),
                argument: json!({"filterGroup": [{"filters": [
                    {"fieldName": "channel", "operator": "eq", "value": "last"},
                ]}]})
                .to_string(),
            },
            ExtensionCall {
                name: SET_SUBSCRIPTION_INVALIDATION_FILTER.to_string(),
                argument: json!({"filterGroup": [{"filters": [
                    {"fieldName": "group", "operator": "eq", "value": "A"},
                ]}]})
                .to_string(),
            },
        ];
        let installed = installed(&calls);
        assert!(
            accepts(installed.filter.as_ref(), &json!({"channel": "last"})),
            "case the last stated filter: expected the publication of the last call to be \
             accepted, got {:?}",
            installed.filter
        );
        assert!(
            !accepts(installed.filter.as_ref(), &json!({"channel": "first"})),
            "case the superseded filter: expected the first call's publication to be \
             rejected, got {:?}",
            installed.filter
        );
        assert!(
            accepts(installed.invalidation_filter.as_ref(), &json!({"group": "A"}))
                && !accepts(installed.invalidation_filter.as_ref(), &json!({"group": "B"})),
            "case the invalidation filter: expected only group A to be accepted, got {:?}",
            installed.invalidation_filter
        );
    }
}
