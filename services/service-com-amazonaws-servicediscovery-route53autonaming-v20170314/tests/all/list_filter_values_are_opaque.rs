// SPDX-License-Identifier: CC0-1.0

//! A list filter's `Values` are opaque match terms, not an enumeration.
//!
//! Every filter member this service models — `NamespaceFilter$Values`,
//! `ServiceFilter$Values` and `OperationFilter$Values` — targets `FilterValues`, an
//! unconstrained list of strings
//! (`smithy_query(query="shape", shape="FilterValues,NamespaceFilter,ServiceFilter,OperationFilter")`).
//! So a well-formed filter whose value names no namespace type, no account, no resource and
//! no operation status selects NOTHING: the listing answers HTTP 200 with an empty page,
//! where the filter `Name` (a modelled enum) and the `Name`/`Condition` pairing are still
//! refused with `InvalidInput`.
//!
//! The rows span every listing that takes filters, the enumerated and the free-text filter
//! names, and both conditions a namespace filter supports — each row asserted both ways: a
//! term that names nothing matches nothing, and the term that does name something still
//! selects it, so "matches nothing" is never the answer to every value.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloud_map_core::{dns_namespace, emulator, failure, namespace, ok, service};

/// The names in one listing's page, under the output member that carries it.
async fn listed_names(
    router: &MultiServiceRouter,
    operation: &str,
    member: &str,
    body: Value,
) -> BTreeSet<String> {
    let page = ok(router, operation, body).await;
    page[member]
        .as_array()
        .unwrap_or_else(|| panic!("{operation} answers a {member} list, got {page}"))
        .iter()
        .filter_map(|entry| entry["Name"].as_str())
        .map(str::to_string)
        .collect()
}

/// The operation ids one `ListOperations` page carries.
async fn listed_operations(router: &MultiServiceRouter, body: Value) -> BTreeSet<String> {
    let page = ok(router, "ListOperations", body).await;
    page["Operations"]
        .as_array()
        .unwrap_or_else(|| panic!("ListOperations answers an Operations list, got {page}"))
        .iter()
        .filter_map(|summary| summary["Id"].as_str())
        .map(str::to_string)
        .collect()
}

/// `ListNamespaces` answers an empty page for a filter value that names nothing, and the
/// matching page for one that names something — whichever filter name and condition.
#[tokio::test]
async fn list_namespaces_treats_filter_values_as_terms() {
    let router = emulator();
    namespace(&router, "opaque-http-ns").await;
    dns_namespace(&router, "opaque-dns-ns").await;

    // (case, the filter, the namespace names the page must carry)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "a TYPE value outside NamespaceType, which the recording shows accepted",
            json!({ "Name": "TYPE", "Values": ["NOT_A_REAL_NAMESPACE_TYPE"], "Condition": "EQ" }),
            vec![],
        ),
        (
            "a TYPE value of a single character, which names no type",
            json!({ "Name": "TYPE", "Values": ["H"], "Condition": "EQ" }),
            vec![],
        ),
        (
            "a TYPE value in the wrong case",
            json!({ "Name": "TYPE", "Values": ["http"], "Condition": "EQ" }),
            vec![],
        ),
        (
            "the TYPE value that does name a stored namespace's type",
            json!({ "Name": "TYPE", "Values": ["HTTP"], "Condition": "EQ" }),
            vec!["opaque-http-ns"],
        ),
        (
            "a TYPE prefix under BEGINS_WITH, which names both DNS types",
            json!({ "Name": "TYPE", "Values": ["DNS_"], "Condition": "BEGINS_WITH" }),
            vec!["opaque-dns-ns.example.com"],
        ),
        (
            "a TYPE prefix under BEGINS_WITH that names no type",
            json!({ "Name": "TYPE", "Values": ["TXT"], "Condition": "BEGINS_WITH" }),
            vec![],
        ),
        (
            "a RESOURCE_OWNER value that names no owner",
            json!({ "Name": "RESOURCE_OWNER", "Values": ["EVERYONE"], "Condition": "EQ" }),
            vec![],
        ),
        (
            "the other documented RESOURCE_OWNER value, which no namespace here has",
            json!({ "Name": "RESOURCE_OWNER", "Values": ["OTHER_ACCOUNTS"], "Condition": "EQ" }),
            vec![],
        ),
        (
            "the RESOURCE_OWNER value every namespace here has",
            json!({ "Name": "RESOURCE_OWNER", "Values": ["SELF"], "Condition": "EQ" }),
            vec!["opaque-http-ns", "opaque-dns-ns.example.com"],
        ),
        (
            "a NAME that names no namespace",
            json!({ "Name": "NAME", "Values": ["no-such-ns"], "Condition": "EQ" }),
            vec![],
        ),
        (
            "an HTTP_NAME that names no namespace",
            json!({ "Name": "HTTP_NAME", "Values": ["no-such-ns"], "Condition": "EQ" }),
            vec![],
        ),
    ];

    for (case, filter, expected) in cases {
        let found =
            listed_names(&router, "ListNamespaces", "Namespaces", json!({ "Filters": [filter] }))
                .await;
        let expected: BTreeSet<String> = expected.into_iter().map(String::from).collect();
        assert_eq!(
            found, expected,
            "case {case:?}: expected the page to carry {expected:?}, got {found:?}"
        );
    }

    // The filter NAME is still an enum, and the Name/Condition pairing still a matrix: an
    // opaque VALUE is not a licence to accept a filter the model does not declare.
    for (case, filter) in [
        (
            "a filter name outside NamespaceFilterName",
            json!({ "Name": "NAMESPACE", "Values": ["HTTP"] }),
        ),
        (
            "a condition a namespace filter does not support",
            json!({ "Name": "NAME", "Values": ["opaque-http-ns"], "Condition": "BETWEEN" }),
        ),
    ] {
        let (status, code) =
            failure(&router, "ListNamespaces", json!({ "Filters": [filter] })).await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidInput"),
            "case {case:?}: expected 400 InvalidInput, got {status} {code}"
        );
    }
}

/// `ListServices` answers an empty page for a filter value that names nothing.
#[tokio::test]
async fn list_services_treats_filter_values_as_terms() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "opaque-svc-ns").await;
    service(&router, &namespace_id, "opaque-svc").await;

    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "a RESOURCE_OWNER value that names no owner",
            json!({ "Name": "RESOURCE_OWNER", "Values": ["EVERYONE"] }),
            vec![],
        ),
        (
            "the documented RESOURCE_OWNER value no service here has",
            json!({ "Name": "RESOURCE_OWNER", "Values": ["OTHER_ACCOUNTS"] }),
            vec![],
        ),
        (
            "the RESOURCE_OWNER value every service here has",
            json!({ "Name": "RESOURCE_OWNER", "Values": ["SELF"] }),
            vec!["opaque-svc"],
        ),
        (
            "a NAMESPACE_ID that names no namespace",
            json!({ "Name": "NAMESPACE_ID", "Values": ["ns-nosuchnamespace"] }),
            vec![],
        ),
        (
            "the NAMESPACE_ID of the namespace that holds the service",
            json!({ "Name": "NAMESPACE_ID", "Values": [namespace_id.clone()] }),
            vec!["opaque-svc"],
        ),
    ];

    for (case, filter, expected) in cases {
        let found =
            listed_names(&router, "ListServices", "Services", json!({ "Filters": [filter] })).await;
        let expected: BTreeSet<String> = expected.into_iter().map(String::from).collect();
        assert_eq!(
            found, expected,
            "case {case:?}: expected the page to carry {expected:?}, got {found:?}"
        );
    }

    let (status, code) = failure(
        &router,
        "ListServices",
        json!({ "Filters": [{ "Name": "NAME", "Values": ["opaque-svc"] }] }),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidInput"),
        "case \"a filter name outside ServiceFilterName\": expected 400 InvalidInput, got \
         {status} {code}"
    );
}

/// `ListOperations` answers an empty page for a `STATUS` or `TYPE` term outside the
/// model's enums, and still selects the records a term that names something matches.
#[tokio::test]
async fn list_operations_treats_filter_values_as_terms() {
    let router = emulator();
    let (create_operation, namespace_id) = namespace(&router, "opaque-ops-ns").await;
    service(&router, &namespace_id, "opaque-ops-svc").await;

    let cases: Vec<(&str, Value, bool)> = vec![
        (
            "a STATUS value outside OperationStatus",
            json!({ "Name": "STATUS", "Values": ["SUCCEED"] }),
            false,
        ),
        (
            "a STATUS value in the wrong case",
            json!({ "Name": "STATUS", "Values": ["submitted"] }),
            false,
        ),
        (
            "a TYPE value outside OperationType",
            json!({ "Name": "TYPE", "Values": ["CREATE_SERVICE"] }),
            false,
        ),
        (
            "the TYPE value the create really filed",
            json!({ "Name": "TYPE", "Values": ["CREATE_NAMESPACE"] }),
            true,
        ),
        (
            "two terms under IN, one of them outside the enum",
            json!({ "Name": "TYPE", "Values": ["CREATE_NAMESPACE", "CREATE_SERVICE"],
                    "Condition": "IN" }),
            true,
        ),
        (
            "a NAMESPACE_ID that names no namespace",
            json!({ "Name": "NAMESPACE_ID", "Values": ["ns-nosuchnamespace"] }),
            false,
        ),
    ];

    for (case, filter, selects_the_create) in cases {
        let found = listed_operations(&router, json!({ "Filters": [filter] })).await;
        assert_eq!(
            found.contains(&create_operation),
            selects_the_create,
            "case {case:?}: expected the create {create_operation} to be selected = \
             {selects_the_create}, got {found:?}"
        );
    }

    let (status, code) = failure(
        &router,
        "ListOperations",
        json!({ "Filters": [{ "Name": "NAME", "Values": ["anything"] }] }),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidInput"),
        "case \"a filter name outside OperationFilterName\": expected 400 InvalidInput, got \
         {status} {code}"
    );
}
