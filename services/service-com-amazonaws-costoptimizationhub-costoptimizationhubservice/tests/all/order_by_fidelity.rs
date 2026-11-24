// SPDX-License-Identifier: CC0-1.0

//! What `ListRecommendations` does with the `orderBy` a request sets, driven
//! end-to-end over the wire (`awsJson1_0`) through the same multi-service router
//! the emulator boots.
//!
//! The rule under test: an ordering directive a request sets is honoured as a
//! whole or refused — the member naming WHAT the rows are ordered on is what
//! makes the directive actionable, so a directive that leaves it out carries
//! nothing the operation can apply and is refused with the operation's
//! `ValidationException` (HTTP 400) instead of being served in an order the
//! caller did not ask for. Only an absent directive asks for no ordering.
//!
//! For `ListRecommendations` that member is `orderBy.dimension`, which the model
//! declares beside `orderBy.order` with neither `@required`
//! (`smithy_query(query="shape", shape="OrderBy")`), so every combination below
//! is input the model admits and the handler has to answer for.
//!
//! Asserted over a table spanning the forms an `orderBy` can take — the member
//! absent, null or blank, with and without an `order` beside it, over both
//! values of `includeAllRecommendations` — beside the forms that ARE actionable,
//! so the refusal is shown to be about the missing dimension and not about
//! `orderBy` itself.
//!
//! The other half of the same rule is WHICH dimensions are actionable: every
//! `Recommendation` member carrying an ordered value is one, whatever kind of
//! value it carries, so the table below spans all of them (and each member's
//! casings, since the name is matched case-insensitively) and leaves refused
//! only the names that are no such member.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cost_optimization_hub::{
    assert_stated_rule_refusal, call, enrolled_router, error_code, ids, ok,
};

/// The member every `orderBy` refusal is attributed to: the one that says which
/// dimension the rows are ordered on.
const DIMENSION_MEMBER: &str = "orderBy.dimension";

/// Every `orderBy` that names no dimension to order by: the member absent,
/// empty, or blank — each with and without an `order` beside it, because an
/// `order` alone does not say what it orders.
fn order_by_without_a_dimension() -> Vec<(&'static str, Value)> {
    vec![
        ("no member at all", json!({})),
        ("order Desc alone", json!({"order": "Desc"})),
        ("order Asc alone", json!({"order": "Asc"})),
        ("dimension empty", json!({"dimension": ""})),
        ("dimension empty with order", json!({"dimension": "", "order": "Asc"})),
        ("dimension blank", json!({"dimension": "   "})),
        ("dimension blank with order", json!({"dimension": "\t ", "order": "Desc"})),
    ]
}

/// An `orderBy` that names no dimension is refused — with the operation's
/// `ValidationException`, a message stating the rule, the refusal attributed to
/// `orderBy.dimension`, and no items served.
#[tokio::test]
async fn an_order_by_naming_no_dimension_is_refused() {
    let router = enrolled_router().await;

    for (form, order_by) in order_by_without_a_dimension() {
        // De-duped and full listings take the same path through `orderBy`, so
        // both arms of `includeAllRecommendations` owe the same refusal.
        for include_all in [true, false] {
            let case = format!("{form} (includeAllRecommendations {include_all})");
            let (status, payload) = call(
                &router,
                "ListRecommendations",
                &json!({"includeAllRecommendations": include_all, "orderBy": order_by}),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {payload}"
            );
            assert_stated_rule_refusal(&payload, &case, DIMENSION_MEMBER);
            assert!(
                payload["items"].is_null(),
                "case {case:?}: a refused request serves no items, got {payload}"
            );
        }
    }

    // A `dimension` sent as JSON `null` names no dimension either, and is
    // refused while being read off the wire — a string member set to null is not
    // a string — so it is answered in the framework's own envelope rather than
    // the handler's. Either way it is a 400 `ValidationException` and nothing is
    // served.
    for order_by in [json!({"dimension": null}), json!({"dimension": null, "order": "Desc"})] {
        let case = format!("dimension null in {order_by}");
        let (status, payload) = call(
            &router,
            "ListRecommendations",
            &json!({"includeAllRecommendations": true, "orderBy": order_by}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {payload}"
        );
        let code = error_code(&payload);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} in {payload}"
        );
        assert!(
            payload["items"].is_null(),
            "case {case:?}: a refused request serves no items, got {payload}"
        );
    }
}

/// An `orderBy.dimension` that names no `Recommendation` member keeps its own
/// refusal — same status, same envelope, same member — and the message states
/// the value that was rejected. The values below are spellings the issue did not
/// cite, including one that is a member of another shape of this service
/// (`Filter.resourceTypes`) and one that differs from a real dimension only in
/// case-insensitive spelling.
#[tokio::test]
async fn an_order_by_dimension_naming_no_member_is_refused() {
    let router = enrolled_router().await;

    for dimension in [
        "resourceTypes",
        "savingsPercentage",
        "estimated_monthly_savings",
        "tags",
        "order",
    ] {
        let case = format!("dimension {dimension:?}");
        let (status, payload) = call(
            &router,
            "ListRecommendations",
            &json!({
                "includeAllRecommendations": true,
                "orderBy": {"dimension": dimension, "order": "Desc"},
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {payload}"
        );
        assert_stated_rule_refusal(&payload, &case, DIMENSION_MEMBER);
        let message = payload["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(dimension),
            "case {case:?}: expected the message to state the rejected value, got {message:?}"
        );
    }
}

/// An `orderBy` that DOES name a dimension is served, and its `order` is
/// applied: the sequence of that dimension's values is sorted, and the
/// descending sequence is the ascending one reversed (which holds whether or not
/// two rows share a value, since both are the same multiset in order). A request
/// with no `orderBy` at all is served too. This is the other side of the refusal
/// above — what the operation refuses is the missing dimension, not an
/// `orderBy`.
#[tokio::test]
async fn an_order_by_naming_a_dimension_is_served_in_the_order_it_asks_for() {
    let router = enrolled_router().await;

    // One `Recommendation` member of each ordered kind the dimension set holds —
    // an identifier, a categorical value, a count and a money figure
    // (`smithy_query(query="shape", shape="Recommendation")` names these
    // members).
    for dimension in [
        "recommendationId",
        "actionType",
        "recommendationLookbackPeriodInDays",
        "estimatedMonthlySavings",
    ] {
        let ascending = served(&router, &json!({"dimension": dimension, "order": "Asc"}), dimension).await;
        let descending =
            served(&router, &json!({"dimension": dimension, "order": "Desc"}), dimension).await;
        let unordered = served(&router, &json!({"dimension": dimension}), dimension).await;
        assert!(
            ascending.len() > 1,
            "case {dimension:?}: expected several rows to order, got {ascending:?}"
        );
        assert!(
            ascending.windows(2).all(|pair| ordered(&pair[0], &pair[1])),
            "case {dimension:?}: expected the Asc values non-decreasing, got {ascending:?}"
        );
        let mut reversed = descending.clone();
        reversed.reverse();
        assert_eq!(
            ascending, reversed,
            "case {dimension:?}: expected the Desc values to be the Asc values reversed, got \
             {descending:?} against {ascending:?}"
        );
        assert_eq!(
            unordered, ascending,
            "case {dimension:?}: expected an absent order to leave the dimension's own order, \
             got {unordered:?} against {ascending:?}"
        );
    }

    // No `orderBy` at all asks for no ordering, and is served.
    let payload = ok(
        &router,
        "ListRecommendations",
        &json!({"includeAllRecommendations": true}),
    )
    .await;
    assert!(
        ids(&payload).len() > 1,
        "case no orderBy: expected the catalog served unordered, got {payload}"
    );
}

/// An `orderBy.order` outside the modelled `Order` enum
/// (`smithy_query(query="shape", shape="Order")` gives `[Asc, Desc]`) is refused
/// rather than served in some other order: the same class as a missing
/// dimension — a directive the operation cannot honour is not silently dropped.
#[tokio::test]
async fn an_order_outside_the_modelled_enum_is_refused() {
    let router = enrolled_router().await;

    for order in ["asc", "DESC", "Descending", "Sideways", ""] {
        let case = format!("order {order:?}");
        let (status, payload) = call(
            &router,
            "ListRecommendations",
            &json!({
                "includeAllRecommendations": true,
                "orderBy": {"dimension": "accountId", "order": order},
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {payload}"
        );
        let code = error_code(&payload);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} in {payload}"
        );
    }
}

/// Every `Recommendation` member that carries an ordered value, named as the
/// model names it (`smithy_query(query="shape", shape="Recommendation")`, which
/// declares these twenty beside `tags`, the one member carrying a list). The
/// sortable set is defined as exactly these members, so each one is a dimension
/// the rows can be put in order of — the identifiers, the categorical values,
/// the two resource descriptions, the booleans, the count, the timestamp and the
/// three money figures alike.
const ORDERED_MEMBERS: &[&str] = &[
    "recommendationId",
    "accountId",
    "region",
    "resourceId",
    "resourceArn",
    "currentResourceType",
    "recommendedResourceType",
    "currentResourceSummary",
    "recommendedResourceSummary",
    "actionType",
    "implementationEffort",
    "source",
    "currencyCode",
    "restartNeeded",
    "rollbackPossible",
    "recommendationLookbackPeriodInDays",
    "lastRefreshTimestamp",
    "estimatedMonthlyCost",
    "estimatedMonthlySavings",
    "estimatedSavingsPercentage",
];

/// Ordering by ANY member of that set is served and applied: the values come
/// back non-decreasing under `Asc`, the `Desc` sequence is that one reversed, an
/// absent `order` leaves the dimension's own order, and the answer still carries
/// every row the unordered listing does — ordering re-arranges the rows, it
/// neither drops nor duplicates one.
#[tokio::test]
async fn every_ordered_recommendation_member_can_be_ordered_by() {
    let router = enrolled_router().await;

    let catalog = rows(&router, None, true).await;
    let catalog_ids = sorted_ids(&catalog);
    assert!(
        catalog_ids.len() > 1,
        "case catalog: expected several rows to order, got {catalog_ids:?}"
    );

    for member in ORDERED_MEMBERS {
        let ascending = rows(&router, Some(&json!({"dimension": member, "order": "Asc"})), true).await;
        let descending =
            rows(&router, Some(&json!({"dimension": member, "order": "Desc"})), true).await;
        let unordered = rows(&router, Some(&json!({"dimension": member})), true).await;

        assert_eq!(
            sorted_ids(&ascending),
            catalog_ids,
            "case {member:?}: expected the ordering to serve every row of the catalog and no \
             other, got {:?}",
            sorted_ids(&ascending)
        );
        let ascending = values(&ascending, member);
        let descending = values(&descending, member);
        let unordered = values(&unordered, member);
        assert!(
            ascending.iter().all(|value| !value.is_null()),
            "case {member:?}: expected every row to carry the dimension's value, got {ascending:?}"
        );
        assert!(
            ascending.windows(2).all(|pair| ordered(&pair[0], &pair[1])),
            "case {member:?}: expected the Asc values non-decreasing, got {ascending:?}"
        );
        let mut reversed = descending.clone();
        reversed.reverse();
        assert_eq!(
            ascending, reversed,
            "case {member:?}: expected the Desc values to be the Asc values reversed, got \
             {descending:?} against {ascending:?}"
        );
        assert_eq!(
            unordered, ascending,
            "case {member:?}: expected an absent order to leave the dimension's own order, got \
             {unordered:?} against {ascending:?}"
        );
    }
}

/// A dimension is resolved by the member it names and not by its spelling: the
/// name is compared case-insensitively, so every casing of a member of the
/// sortable set puts the rows in the same order. Asserted over each member,
/// against the sequence its own spelling serves.
#[tokio::test]
async fn a_dimension_names_its_member_whatever_its_casing() {
    let router = enrolled_router().await;

    for member in ORDERED_MEMBERS {
        let expected = values(
            &rows(&router, Some(&json!({"dimension": member, "order": "Asc"})), true).await,
            member,
        );
        for spelling in [
            member.to_ascii_lowercase(),
            member.to_ascii_uppercase(),
            capitalized(member),
        ] {
            let case = format!("dimension {spelling:?} for member {member:?}");
            let served = values(
                &rows(&router, Some(&json!({"dimension": spelling, "order": "Asc"})), true).await,
                member,
            );
            assert_eq!(
                served, expected,
                "case {case:?}: expected the same order as the member's own spelling, got \
                 {served:?} against {expected:?}"
            );
        }
    }
}

/// The two members that describe the resource — `currentResourceSummary` and
/// `recommendedResourceSummary`, plain strings the list answer already carries
/// for every row — order the rows by that description, on the de-duped listing
/// as well as the full one. Each carries more than one distinct value across the
/// catalog, so the order is observable rather than vacuous.
#[tokio::test]
async fn the_resource_summaries_order_the_rows_by_their_description() {
    let router = enrolled_router().await;

    for member in ["currentResourceSummary", "recommendedResourceSummary"] {
        for include_all in [true, false] {
            let case = format!("dimension {member:?} (includeAllRecommendations {include_all})");
            let served = values(
                &rows(
                    &router,
                    Some(&json!({"dimension": member, "order": "Asc"})),
                    include_all,
                )
                .await,
                member,
            );
            let mut distinct: Vec<&Value> = Vec::new();
            for value in &served {
                if !distinct.contains(&value) {
                    distinct.push(value);
                }
            }
            assert!(
                distinct.len() > 1,
                "case {case:?}: expected more than one distinct description to order, got \
                 {served:?}"
            );
            assert!(
                served.windows(2).all(|pair| ordered(&pair[0], &pair[1])),
                "case {case:?}: expected the descriptions non-decreasing, got {served:?}"
            );
            let mut sorted = served.clone();
            sorted.sort_by(|left, right| {
                left.as_str()
                    .unwrap_or_default()
                    .cmp(right.as_str().unwrap_or_default())
            });
            assert_eq!(
                served, sorted,
                "case {case:?}: expected the served descriptions in the order the strings take, \
                 got {served:?} against {sorted:?}"
            );
        }
    }
}

/// `member` with its first character upper-cased, one more spelling of the same
/// member name.
fn capitalized(member: &str) -> String {
    let mut characters = member.chars();
    match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => String::new(),
    }
}

/// The `items` a `ListRecommendations` answer carries for one `orderBy`, in the
/// order the answer carries them.
async fn rows(router: &MultiServiceRouter, order_by: Option<&Value>, include_all: bool) -> Vec<Value> {
    let mut request = json!({"includeAllRecommendations": include_all});
    if let Some(order_by) = order_by {
        request["orderBy"] = order_by.clone();
    }
    let payload = ok(router, "ListRecommendations", &request).await;
    payload["items"]
        .as_array()
        .expect("items is a list")
        .clone()
}

/// The value of `member` in each row, in the rows' order.
fn values(rows: &[Value], member: &str) -> Vec<Value> {
    rows.iter().map(|row| row[member].clone()).collect()
}

/// The ids the rows carry, sorted — the set of rows an answer serves, with the
/// order it served them in set aside.
fn sorted_ids(rows: &[Value]) -> Vec<String> {
    let mut sorted: Vec<String> = rows
        .iter()
        .map(|row| {
            row["recommendationId"]
                .as_str()
                .expect("every recommendation carries an id")
                .to_string()
        })
        .collect();
    sorted.sort();
    sorted
}

/// Two values of one dimension in order: `true` when `left` sorts at or before
/// `right`. Values of one dimension are always of one JSON kind, so a pair of
/// different kinds is no order at all.
fn ordered(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => {
            a.as_f64().unwrap_or_default() <= b.as_f64().unwrap_or_default()
        }
        (Value::String(a), Value::String(b)) => a <= b,
        (Value::Bool(a), Value::Bool(b)) => a <= b,
        _ => false,
    }
}

/// The values of `member` that `ListRecommendations` serves for one `orderBy`,
/// in the order the answer carries them.
async fn served(router: &MultiServiceRouter, order_by: &Value, member: &str) -> Vec<Value> {
    let payload = ok(
        router,
        "ListRecommendations",
        &json!({"includeAllRecommendations": true, "orderBy": order_by}),
    )
    .await;
    payload["items"]
        .as_array()
        .expect("items is a list")
        .iter()
        .map(|item| item[member].clone())
        .collect()
}
