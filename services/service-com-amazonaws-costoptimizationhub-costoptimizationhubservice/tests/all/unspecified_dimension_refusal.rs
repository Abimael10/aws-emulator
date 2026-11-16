// SPDX-License-Identifier: CC0-1.0

//! One rule over every member of this service that names a DIMENSION, driven
//! end-to-end over the wire (`awsJson1_0`) through the same multi-service router
//! the emulator boots.
//!
//! **A member a request SUPPLIES while naming no dimension in it is refused with
//! a message that states that rule** — it names the member and says an
//! unspecified dimension cannot be grouped or ordered by — rather than with a
//! sentence the blank value is spliced into, which names no dimension at all.
//! Such a value reaches the handler because each of these members targets a
//! plain `String` with no `@length` and no `@pattern`
//! (`smithy_query(query="operation",
//! operation="ListRecommendationSummaries,ListRecommendations,ListEfficiencyMetrics")`),
//! so presence alone satisfies the pre-dispatch constraint walk.
//!
//! The table spans the class: every dimension-naming member of the service,
//! against every form a blank JSON string takes (empty, and each run of
//! whitespace), plus — for a dimension carried inside a structure the request
//! set — the member left out altogether. Two complements from the same table
//! keep the rule from swallowing the values it is not about: a value that names
//! none of the dimensions the member accepts is still refused with that value
//! STATED, and a value that names one is served, surrounding whitespace and all.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cost_optimization_hub::{
    assert_stated_rule_refusal, call, enrolled_router, fields, ok, router,
};
use crate::efficiency_metrics::last_month_request;

/// One member of this service that names a dimension.
///
/// `member` is the member a request sets, `action` what that member asks the
/// operation to do with the dimension it names, `attributed` the member a
/// refusal is attributed to (the one actually carrying the dimension), `named` a
/// value that names a dimension the member accepts and `unknown` one that names
/// none, and `results` the output member a served request answers with.
#[derive(Debug, Clone, Copy)]
struct DimensionMember {
    operation: &'static str,
    member: &'static str,
    action: &'static str,
    attributed: &'static str,
    named: &'static str,
    unknown: &'static str,
    results: &'static str,
}

/// Whether the dimension sits inside a structure the request sets, so a request
/// can set the member while leaving the dimension out altogether — which asks
/// for the same thing a blank one does.
fn nested(member: &DimensionMember) -> bool {
    member.attributed != member.member
}

/// Every dimension-naming member of this service.
///
/// The members are the model's own
/// (`smithy_query(query="operation",
/// operation="ListRecommendationSummaries,ListRecommendations,ListEfficiencyMetrics")`:
/// `groupBy` on the two grouped reads, `orderBy.dimension` on the two ordered
/// ones). The accepted values each row names a dimension from are the
/// categorical recommendation dimensions for a grouping
/// (`state::GROUP_DIMENSIONS`) and, for `ListEfficiencyMetrics`' ordering, the
/// members its groups carry (`smithy_query(query="shape",
/// shape="EfficiencyMetricsByGroup,MetricsByTime")`: `group`, and `score`,
/// `savings`, `spend`) — each row taking a dimension no case of the suites
/// beside this one is keyed to.
const MEMBERS: &[DimensionMember] = &[
    DimensionMember {
        operation: "ListRecommendationSummaries",
        member: "groupBy",
        action: "group",
        attributed: "groupBy",
        named: "ImplementationEffort",
        unknown: "NotADimension",
        results: "items",
    },
    DimensionMember {
        operation: "ListEfficiencyMetrics",
        member: "groupBy",
        action: "group",
        attributed: "groupBy",
        named: "RollbackPossible",
        unknown: "EstimatedMonthlyCost",
        results: "efficiencyMetricsByGroup",
    },
    DimensionMember {
        operation: "ListRecommendations",
        member: "orderBy",
        action: "order",
        attributed: "orderBy.dimension",
        named: "estimatedMonthlyCost",
        unknown: "NotADimension",
        results: "items",
    },
    DimensionMember {
        operation: "ListEfficiencyMetrics",
        member: "orderBy",
        action: "order",
        attributed: "orderBy.dimension",
        named: "spend",
        unknown: "accountId",
        results: "efficiencyMetricsByGroup",
    },
];

/// Every form a blank value takes on the wire: the empty string, and a run of
/// each whitespace character a JSON string can carry.
const BLANK: &[&str] = &["", " ", "   ", "\t", "\n", "\r\n", " \t\n "];

/// A well-formed request for `member`'s operation with the dimension set to
/// `dimension`, or — where `dimension` is `None` — left out of the structure
/// that carries it.
///
/// Every other member of the request is well formed, so nothing but the
/// dimension can be what an answer is about. `ListEfficiencyMetrics` needs a
/// granularity and a period it can report over, which is the one
/// [`last_month_request`] builds from the clock.
fn request(member: &DimensionMember, dimension: Option<&str>) -> Value {
    let mut body = if member.operation == "ListEfficiencyMetrics" {
        last_month_request()
    } else {
        json!({})
    };
    if nested(member) {
        let mut order_by = json!({"order": "Asc"});
        if let Some(dimension) = dimension {
            order_by["dimension"] = json!(dimension);
        }
        body["orderBy"] = order_by;
    } else if let Some(dimension) = dimension {
        body[member.member] = json!(dimension);
    }
    body
}

/// The sentence this service refuses a supplied-but-unspecified dimension with.
fn unspecified(member: &DimensionMember) -> String {
    format!(
        "Invalid {} specified. Cannot {} by an unspecified dimension for this operation.",
        member.member, member.action
    )
}

/// The one refusal entry an answer carries, as `(fieldName, message)`.
fn only_field(payload: &Value, case: &str) -> (String, String) {
    let entries = fields(payload);
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected exactly one fields entry, got {payload}"
    );
    entries
        .into_iter()
        .next()
        .expect("the entry just counted is there")
}

/// The refusal of a blank dimension: HTTP 400 `ValidationException` whose
/// `message` states the rule and names the member, and whose single `fields`
/// entry states the same rule against the member carrying the dimension.
async fn assert_refused_as_unspecified(
    router: &MultiServiceRouter,
    member: &DimensionMember,
    case: &str,
    body: &Value,
) {
    let (status, payload) = call(router, member.operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {payload}"
    );
    assert_stated_rule_refusal(&payload, case, member.attributed);
    let message = payload["message"].as_str().unwrap_or_default();
    assert_eq!(
        message,
        unspecified(member),
        "case {case:?}: expected the refusal to state the rule, got {message:?} in {payload}"
    );
    let (field_name, clause) = only_field(&payload, case);
    assert_eq!(
        field_name, member.attributed,
        "case {case:?}: expected the entry to name {:?}, got {payload}",
        member.attributed
    );
    let required = format!("is required whenever {} is specified", member.member);
    assert!(
        clause.contains("dimension") && clause.ends_with(&required),
        "case {case:?}: expected an entry stating {required:?} of a dimension, got {clause:?}"
    );
    assert!(
        payload[member.results].is_null(),
        "case {case:?}: expected a refusal rather than an answer, got {payload}"
    );
}

/// Rule: a dimension-naming member a request supplies while naming no dimension
/// in it is refused with the rule stated, for every such member of this service
/// and every blank form the value takes.
///
/// The refusal is the operation's own rule about the request, which every one of
/// these readers applies before it reads any state, so the cases need no
/// enrolled account: a plain router is enough, and the answer cannot be about
/// anything the store holds.
#[tokio::test]
async fn a_supplied_but_unspecified_dimension_is_refused_with_the_rule_stated() {
    let router = router();

    for member in MEMBERS {
        for blank in BLANK {
            let case = format!("{}.{} = {blank:?}", member.operation, member.attributed);
            assert_refused_as_unspecified(&router, member, &case, &request(member, Some(blank)))
                .await;
        }

        // A dimension carried inside a structure the request set is unspecified
        // when it is left out as well: an `order` on its own does not say which
        // values it orders.
        if nested(member) {
            let case = format!("{}.{} left out", member.operation, member.attributed);
            assert_refused_as_unspecified(&router, member, &case, &request(member, None)).await;
        }
    }
}

/// The complement of the same rule: a value that names none of the dimensions
/// the member accepts is refused with THAT value stated — the blank rule does
/// not swallow it — and a value that names one is served, surrounding
/// whitespace and all, because a dimension is resolved from the value's content.
#[tokio::test]
async fn a_dimension_the_value_does_name_is_judged_on_what_it_names() {
    let router = enrolled_router().await;

    for member in MEMBERS {
        let case = format!(
            "{}.{} = {:?}",
            member.operation, member.attributed, member.unknown
        );
        let (status, payload) = call(
            &router,
            member.operation,
            &request(member, Some(member.unknown)),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {payload}"
        );
        assert_stated_rule_refusal(&payload, &case, member.attributed);
        let message = payload["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(member.unknown),
            "case {case:?}: expected the refusal to state the rejected value, got {message:?}"
        );
        assert_ne!(
            message,
            unspecified(member),
            "case {case:?}: expected the refusal of a named dimension, not of an unspecified one"
        );
        let (_, clause) = only_field(&payload, &case);
        assert!(
            clause.contains(member.unknown),
            "case {case:?}: expected the entry to state the rejected value, got {clause:?}"
        );

        // The same member naming a dimension the operation accepts, padded with
        // the whitespace the blank cases are made of, is served: what the value
        // names is read from its content.
        let padded = format!("  {}  ", member.named);
        let case = format!("{}.{} = {padded:?}", member.operation, member.attributed);
        let served = ok(&router, member.operation, &request(member, Some(&padded))).await;
        assert!(
            served[member.results].is_array(),
            "case {case:?}: expected {} in the answer, got {served}",
            member.results
        );
    }
}
