// SPDX-License-Identifier: CC0-1.0

//! The unit tests of [`super`]: the two addressings a data-plane request resolves under,
//! and the readings this plane's own front door performs on one.

use super::*;

/// RULE: the data plane is addressed either at the host a domain publishes or under
/// the reserved path namespace naming this plane and the domain; nothing else names it.
#[test]
fn a_prefixed_path_names_this_plane_the_domain_and_the_request_path() {
    // (the case, the path, the domain it names, the request path within it)
    struct Row {
        case: &'static str,
        path: &'static str,
        names: Option<(&'static str, &'static str)>,
    }
    let rows: &[Row] = &[
        Row {
            case: "a domain name and a cluster path",
            path: "/_aws/es/search-target/_cluster/health",
            names: Some(("search-target", "/_cluster/health")),
        },
        Row {
            case: "a domain name and the root path",
            path: "/_aws/es/search-target",
            names: Some(("search-target", "/")),
        },
        Row {
            case: "another plane under the same namespace",
            path: "/_aws/execute-api/abc/stage",
            names: None,
        },
        Row {
            case: "no namespace at all",
            path: "/_cluster/health",
            names: None,
        },
        Row {
            case: "the namespace and this plane but no domain",
            path: "/_aws/es",
            names: None,
        },
    ];
    for row in rows {
        let named = prefixed_addressing(row.path);
        assert_eq!(
            named, row.names,
            "case {:?}: expected {:?}, got {named:?}",
            row.case, row.names
        );
    }
}

/// RULE: a query selects documents by the clause it states, and a clause the engine
/// registers no query for is refused rather than selecting everything.
#[test]
fn a_query_selects_by_the_clause_it_states() {
    let source = json!({ "level": "warn", "code": 7, "tags": ["a", "b"] });
    // (the case, the query, whether it selects the document)
    let rows: &[(&str, Value, bool)] = &[
        ("no query at all", json!({ "match_all": {} }), true),
        ("a term on a text field", json!({ "term": { "level": "warn" } }), true),
        ("a term on another value", json!({ "term": { "level": "info" } }), false),
        ("a term on a numeric field", json!({ "term": { "code": 7 } }), true),
        ("a term with an explicit value", json!({ "term": { "code": { "value": 7 } } }), true),
        ("a match clause", json!({ "match": { "level": "warn" } }), true),
        ("a terms clause", json!({ "terms": { "level": ["info", "warn"] } }), true),
        ("a term on one element of an array", json!({ "term": { "tags": "b" } }), true),
        ("an ids clause naming the document", json!({ "ids": { "values": ["one"] } }), true),
        ("an ids clause naming another", json!({ "ids": { "values": ["two"] } }), false),
        (
            "a bool composing a filter and a must_not",
            json!({ "bool": {
                "filter": [{ "term": { "level": "warn" } }],
                "must_not": [{ "term": { "code": 8 } }]
            } }),
            true,
        ),
        (
            "a bool whose must_not excludes it",
            json!({ "bool": { "must_not": [{ "term": { "level": "warn" } }] } }),
            false,
        ),
    ];
    for (case, query, expected) in rows {
        let select = selector(Some(query), 0, None)
            .map_err(|_| ())
            .unwrap_or_else(|()| panic!("case {case:?}: expected the query to be read"));
        // No index schema and no mounted file: the fields a `match` names are analyzed
        // by the default chain, which is what an index with no analysis settings of
        // its own uses.
        let mut analyzed = BTreeSet::new();
        select.analyzed_clauses(&mut analyzed);
        let budget = ::std::cell::Cell::new(0_usize);
        let terms = Terms::of(
            state::analysis::IndexAnalysis::default(),
            &state::analysis::PackageFiles::default(),
            &analyzed,
            &BTreeSet::new(),
            &budget,
        )
        .map_err(|_| ())
        .unwrap_or_else(|()| panic!("case {case:?}: expected the chains to resolve"));
        let matched = select.matches("one", &source, &terms);
        assert_eq!(
            matched, *expected,
            "case {case:?}: expected {expected}, got {matched}"
        );
    }
    let refused = selector(Some(&json!({ "wildcard": { "level": "w*" } })), 0, None);
    assert!(
        refused.is_err(),
        "case \"a clause the engine registers no query for\": expected a refusal"
    );
    // And a `bool` nested past the engine's own bound is refused rather than walked.
    let mut nested = json!({ "match_all": {} });
    for _ in 0..=MAX_BOOL_NESTING {
        nested = json!({ "bool": { "must": [nested] } });
    }
    assert!(
        selector(Some(&nested), 0, None).is_err(),
        "case \"a bool nested past the engine's bound\": expected a refusal"
    );
}

/// RULE: the version banner reports the distribution and the version number the
/// domain's own `EngineVersion` names.
#[test]
fn the_banner_version_comes_from_the_domains_engine_version() {
    // (the case, the stored EngineVersion, the distribution, the number)
    let rows: &[(&str, &str, &str, &str)] = &[
        ("an OpenSearch version", "OpenSearch_2.11", "opensearch", "2.11.0"),
        ("an Elasticsearch version", "Elasticsearch_7.10", "elasticsearch", "7.10.0"),
        ("a three-part version", "OpenSearch_2.11.1", "opensearch", "2.11.1"),
    ];
    for (case, stored, distribution, number) in rows {
        let answered = engine_distribution(stored);
        assert_eq!(
            answered,
            ((*distribution).to_string(), (*number).to_string()),
            "case {case:?}: expected {distribution}/{number}, got {answered:?}"
        );
    }
}
