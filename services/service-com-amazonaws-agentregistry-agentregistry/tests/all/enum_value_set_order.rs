// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enumerated type's admissible values in when it refuses
//! a value outside that type, end-to-end over the same `MultiServiceRouter::dispatch` the
//! emulator serves on its socket.
//!
//! RULE: a clause about a value outside an enumerated type lists that type's admissible
//! values in LEXICOGRAPHIC order, whatever order the model declares that type's members in
//! — the declaration order is a fact about how the model was written, not about the type,
//! so one enum reads alike in every clause that states it, at any member, any operation and
//! any depth.
//!
//! The rows differ along what the rule generalises over: which element of the filter list
//! carried the offending value (so the member path moves while the set does not) — every
//! position up to the most filters a request may carry — the FORM of the refused value
//! (empty, a declared value in another casing, a value of another enum of this model, a
//! long string), how many clauses one refusal carries (one per offending element, up to one
//! per element), and what ELSE the request carries: the other members of this read, the
//! form of the registry identifier in the path, and whether the registry named there holds
//! any record, or exists at all. The contrast tests pin the two other halves: every value
//! the stated set lists is ACCEPTED (the listing re-orders a set, it does not decide
//! membership), and nothing of the refused value reaches the sentence.
//!
//! The admissible values themselves come from the model, through this crate's own generated
//! catalogue of them (`ops::types::REGISTRY_RECORD_FILTER_NAME_VALUES` for
//! `com.amazonaws.agentregistry#RegistryRecordFilterName`,
//! `smithy_query(query="shape", shape="RegistryRecordFilterName")`); nothing here spells a
//! value set by hand. The harness (`seeded_router`, `post`, `list_path`) is the one
//! `tests/all/discovery_read_path.rs` already drives this service with.

use serde_json::{json, Value};
use service_com_amazonaws_agentregistry_agentregistry::ops::types::REGISTRY_RECORD_FILTER_NAME_VALUES;

use crate::discovery_read_path::{list_path, post, seeded_router, EMPTY_REGISTRY, REGION, REGISTRY};
use service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::state as owner;

/// The `message` member of a refusal body.
fn stated(body: &Value) -> String {
    body["message"].as_str().unwrap_or_default().to_string()
}

/// The requirement clause an enum value set states, over `values` in the order the clause
/// lists them.
fn enum_requirement(values: &[&str]) -> String {
    format!("Member must satisfy enum value set: [{}]", values.join(", "))
}

/// `RegistryRecordFilterName`'s admissible values in the order this service states them:
/// the model's own set, sorted.
fn stated_value_set() -> Vec<&'static str> {
    let mut sorted = REGISTRY_RECORD_FILTER_NAME_VALUES.to_vec();
    sorted.sort_unstable();
    sorted
}

/// A `filters` entry naming `name`, with a `values` list every element constraint of
/// `DiscoverableFilterValues` admits, so a refusal is about the filter NAME alone.
fn filter(name: &str) -> Value {
    json!({ "name": name, "values": ["MCP"] })
}

/// The list read's path and the body carrying `filters`.
async fn list_with_filters(
    router: &smithy_server_runtime::MultiServiceRouter,
    filters: Value,
) -> (u16, Value) {
    post(router, &list_path(REGISTRY), &json!({ "filters": filters })).await
}

/// A value outside the enumerated type is refused with the type's whole admissible set
/// listed in lexicographic order, at the member path the offending element sits at.
#[tokio::test]
async fn an_enum_violation_lists_the_admissible_set_in_lexicographic_order() {
    let router = seeded_router();
    let set = enum_requirement(&stated_value_set());
    let long_value = "x".repeat(200);
    // (case, the filter list the request carries, the member path the clause is stated at).
    // The validating walk spells an element as a 1-based position and a `member` step, so
    // the path moves with the position the offending value sits at.
    let cases: [(&str, Value, &str); 6] = [
        (
            "a filter name that is no member of the type",
            json!([filter("bogus")]),
            "filters.1.member.name",
        ),
        (
            "a declared value in another casing",
            json!([filter("RECORDTYPE")]),
            "filters.1.member.name",
        ),
        (
            "the empty string",
            json!([filter("")]),
            "filters.1.member.name",
        ),
        (
            // A value of ANOTHER enum of this model (`RecordType`), which is no member of
            // this one.
            "a value of another enumerated type of this model",
            json!([filter("GATEWAY")]),
            "filters.1.member.name",
        ),
        (
            "a long value, in the second element of the list",
            json!([filter("recordType"), filter(&long_value)]),
            "filters.2.member.name",
        ),
        (
            "the third element of the list, both earlier ones admissible",
            json!([
                filter("recordType"),
                filter("descriptorType"),
                filter("descriptor_type"),
            ]),
            "filters.3.member.name",
        ),
    ];

    for (case, filters, member) in cases {
        let (status, response) = list_with_filters(&router, filters).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {response}"
        );
        let expected =
            format!("1 validation error detected: Value at '{member}' failed to satisfy constraint: {set}");
        let actual = stated(&response);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// Every clause of a refusal that carries more than one states the same set in the same
/// order: the order belongs to the enumerated type, not to the clause's position.
#[tokio::test]
async fn every_clause_of_one_refusal_states_the_set_in_that_one_order() {
    let router = seeded_router();
    let set = enum_requirement(&stated_value_set());
    let (status, response) = list_with_filters(
        &router,
        json!([filter("bogus"), filter("recordType"), filter("")]),
    )
    .await;
    assert_eq!(
        status, 400,
        "case two offending filter names: expected 400, got {status}: {response}"
    );
    let actual = stated(&response);
    let expected = format!(
        "2 validation errors detected: \
         Value at 'filters.1.member.name' failed to satisfy constraint: {set}; \
         Value at 'filters.3.member.name' failed to satisfy constraint: {set}"
    );
    assert_eq!(
        actual, expected,
        "case two offending filter names: expected {expected:?}, got {actual:?}"
    );
}

/// The stated set is the model's own set, re-ordered and nothing else: every value the
/// model declares is listed exactly once, the listing is sorted, and no value the model
/// does not declare appears.
#[tokio::test]
async fn the_stated_set_is_the_declared_set_sorted() {
    let router = seeded_router();
    let (status, response) = list_with_filters(&router, json!([filter("bogus")])).await;
    assert_eq!(
        status, 400,
        "case a refused filter name: expected 400, got {status}: {response}"
    );
    let actual = stated(&response);
    let listed: Vec<String> = actual
        .split_once("enum value set: [")
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(set, _)| set.split(", ").map(str::to_string).collect())
        .unwrap_or_else(|| panic!("case a refused filter name: no value set in {actual:?}"));

    let mut sorted = listed.clone();
    sorted.sort_unstable();
    assert_eq!(
        listed, sorted,
        "case a refused filter name: the set must be listed in lexicographic order, got {listed:?}"
    );
    let mut declared: Vec<String> = REGISTRY_RECORD_FILTER_NAME_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    declared.sort_unstable();
    assert_eq!(
        listed, declared,
        "case a refused filter name: the set must be the model's own, got {listed:?}"
    );
}

/// The contrast: every value the stated set lists is ACCEPTED, so the listing re-orders a
/// set and decides no membership — and the refusals above are about values outside the
/// type.
#[tokio::test]
async fn every_value_the_stated_set_lists_is_accepted() {
    let router = seeded_router();
    for value in stated_value_set() {
        let (status, response) = list_with_filters(&router, json!([filter(value)])).await;
        assert_eq!(
            status, 200,
            "case a filter named {value:?}: expected 200, got {status}: {response}"
        );
    }
    // And all of them together, so no admissible value is refused in the company of
    // another.
    let every: Vec<Value> = stated_value_set().into_iter().map(filter).collect();
    let (status, response) = list_with_filters(&router, json!(every)).await;
    assert_eq!(
        status, 200,
        "case every admissible filter name at once: expected 200, got {status}: {response}"
    );
}

/// Nothing of the refused value reaches the sentence: the clause names the member and the
/// admissible set, whatever the request carried.
#[tokio::test]
async fn an_enum_clause_quotes_no_value() {
    let router = seeded_router();
    for refused in ["bogus", "RECORDTYPE", "descriptor_type"] {
        let (status, response) = list_with_filters(&router, json!([filter(refused)])).await;
        assert_eq!(
            status, 400,
            "case a filter named {refused:?}: expected 400, got {status}: {response}"
        );
        let actual = stated(&response);
        assert!(
            !actual.contains(refused) && !actual.contains("Value '"),
            "case a filter named {refused:?}: no value may be quoted, got {actual:?}"
        );
    }
}

/// The most filters one request may carry, and so the widest element position a clause can
/// be stated at: `RegistryRecordFilterList` declares `@length(max: 10)`
/// (`smithy_query(query="shape", shape="RegistryRecordFilterList")`).
const MOST_FILTERS: usize = 10;

/// A filter name the enumerated type admits, for the elements of a list whose offending
/// element sits somewhere else.
fn admissible_name() -> &'static str {
    stated_value_set()[0]
}

/// The whole sentence a refusal states for offending filter names at `positions`: the
/// count of clauses, then one clause per offending element at its own path, each listing
/// the enumerated type's admissible set in the one order.
fn expected_sentence(positions: &[usize]) -> String {
    let set = enum_requirement(&stated_value_set());
    let clauses: Vec<String> = positions
        .iter()
        .map(|position| {
            format!("Value at 'filters.{position}.member.name' failed to satisfy constraint: {set}")
        })
        .collect();
    let detected = if positions.len() == 1 {
        "1 validation error detected".to_string()
    } else {
        format!("{} validation errors detected", positions.len())
    };
    format!("{detected}: {}", clauses.join("; "))
}

/// The list read of `registry` carrying `body`.
async fn list_of(
    router: &smithy_server_runtime::MultiServiceRouter,
    registry: &str,
    body: &Value,
) -> (u16, Value) {
    post(router, &list_path(registry), body).await
}

/// The set reads alike at EVERY element position the filter list admits, so the order
/// belongs to the enumerated type and not to where the offending element sits: one
/// offender walked across a full-length list, every other element admissible.
#[tokio::test]
async fn the_set_reads_alike_at_every_position_the_filter_list_admits() {
    let router = seeded_router();
    let admissible = admissible_name();
    for position in 1..=MOST_FILTERS {
        let filters: Vec<Value> = (1..=MOST_FILTERS)
            .map(|at| {
                if at == position {
                    filter("no-such-filter-name")
                } else {
                    filter(admissible)
                }
            })
            .collect();
        let case = format!("the offending filter name at position {position} of {MOST_FILTERS}");
        let (status, response) = list_with_filters(&router, json!(filters)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {response}"
        );
        let expected = expected_sentence(&[position]);
        let actual = stated(&response);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// However MANY elements offend — one, every one, or any number between — the refusal
/// states one clause per offending element, in element order, and every clause lists the
/// same set in the same order.
#[tokio::test]
async fn every_clause_lists_the_same_set_however_many_elements_offend() {
    let router = seeded_router();
    let admissible = admissible_name();
    for offenders in 1..=MOST_FILTERS {
        // The offending elements lead, so the clause count and the paths vary together.
        let filters: Vec<Value> = (1..=MOST_FILTERS)
            .map(|at| {
                if at <= offenders {
                    // A different form at each position, so one shape of value cannot be
                    // what the order depends on.
                    filter(&format!("not-a-filter-name-{at}"))
                } else {
                    filter(admissible)
                }
            })
            .collect();
        let case = format!("{offenders} of {MOST_FILTERS} filter names offending");
        let (status, response) = list_with_filters(&router, json!(filters)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {response}"
        );
        let positions: Vec<usize> = (1..=offenders).collect();
        let expected = expected_sentence(&positions);
        let actual = stated(&response);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The sentence is a function of the request's offending member and of the enumerated type
/// alone: whatever ELSE the read carries — the page members at their published bounds, a
/// resumption token, the registry identifier in either admitted form — and whatever the
/// store holds for the registry named there, including a registry it holds no record for
/// and one it does not hold at all, the clause and its set do not move.
#[tokio::test]
async fn the_set_does_not_move_with_the_rest_of_the_request_or_with_the_store() {
    let router = seeded_router();
    let offending = json!([filter("no-such-filter-name")]);
    // A registry identifier the model admits (12..16 alphanumerics) that the store does
    // not hold, so the refusal cannot be read off what was seeded.
    let absent_registry = "norecordsreg01";
    // The identifier's other admitted form: the registry's ARN.
    let arn_form = owner::registry_arn(REGION, REGISTRY);
    // (case, the registry the path names, the body).
    let cases: [(&str, &str, Value); 6] = [
        (
            "the offending filter alone",
            REGISTRY,
            json!({ "filters": offending }),
        ),
        (
            "beside maxResults at its published minimum",
            REGISTRY,
            json!({ "filters": offending, "maxResults": 1 }),
        ),
        (
            "beside maxResults at its published maximum",
            REGISTRY,
            json!({ "filters": offending, "maxResults": 100 }),
        ),
        (
            "beside a resumption token and a page size",
            REGISTRY,
            json!({ "filters": offending, "maxResults": 20, "nextToken": "cGFnZQ" }),
        ),
        (
            "a registry that holds no discoverable record",
            EMPTY_REGISTRY,
            json!({ "filters": offending }),
        ),
        (
            "a registry the store does not hold",
            absent_registry,
            json!({ "filters": offending }),
        ),
    ];

    let expected = expected_sentence(&[1]);
    for (case, registry, body) in cases {
        let (status, response) = list_of(&router, registry, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {response}"
        );
        let actual = stated(&response);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // The same read addressed by the identifier's ARN form.
    let case = "the registry named by its ARN";
    let (status, response) = list_of(&router, &arn_form, &json!({ "filters": offending })).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status}: {response}"
    );
    let actual = stated(&response);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {expected:?}, got {actual:?}"
    );
}
