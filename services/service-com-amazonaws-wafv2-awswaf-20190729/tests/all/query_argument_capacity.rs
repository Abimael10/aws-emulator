// SPDX-License-Identifier: CC0-1.0

//! What a statement pays for the request component it inspects, and for the
//! transformations it applies to it, end to end over the wire.
//!
//! `CheckCapacity` "returns the web ACL capacity unit (WCU) requirements for a
//! specified scope and set of rules", and "WAF calculates capacity differently for
//! each rule type, to reflect the relative cost of each rule. Simple rules that cost
//! little to run use fewer WCUs than more complex rules that use more processing
//! power". Two RULES are asserted here, over every statement kind that carries a
//! `FieldToMatch`:
//!
//! * a component WAF has to EXTRACT out of a container costs a flat surcharge on top
//!   of the statement's base, and a component it inspects as it arrived costs nothing
//!   extra. Two kinds of component need that work and are charged alike: a query
//!   argument, which WAF parses the raw query string into arguments for — one named
//!   argument does not save that parse — and every value of a multi-valued component
//!   (`smithy_query(query="shape",shape="FieldToMatch")`: "inspect a single query
//!   argument", "inspect all query arguments", "inspect the request headers",
//!   "inspect the request cookies");
//! * a `PreParseTextTransformations` chain is transformation work like any other —
//!   "pre-parse text transformations normalize the raw query string before WAF parses
//!   it into individual query arguments. They are applied before the standard text
//!   transformations" (`smithy_query(query="shape",shape="PreParseTextTransformation")`)
//!   — so it is charged exactly what the same chain costs as a statement's
//!   `TextTransformations`, and nothing besides.
//!
//! Every row is priced through `CheckCapacity`, and one is read back through
//! `WebACL$Capacity`, because "you can use this to check the capacity requirements for
//! the rules you want to use in a RuleGroup or WebACL": the report and the stored
//! figure are one derivation.

use serde_json::{json, Value};

use crate::web_acl_lifecycle::{base64_of, create_web_acl, ordered, rule, Waf};

/// The namespace these rows are priced in. The cost of a rule list is a function of
/// the statements in it, so one namespace is enough; the other one is covered by
/// `wcu_cost_model`.
const SCOPE: &str = "REGIONAL";

/// The WCUs a component WAF has to extract out of a container adds. Written out here
/// rather than imported, so the table pins the arithmetic and not merely its reuse.
const EXTRACTED_COMPONENT_WCU: i64 = 10;

/// The WCUs each transformation a statement applies adds.
const TRANSFORMATION_WCU: i64 = 10;

/// The WCUs a `CONTAINS` byte match over one query argument costs: the statement's
/// own base for a positional constraint that scans, plus the surcharge for a
/// component parsed out of the query string. This is the figure the recorded corpus
/// pins for such a statement carrying no transformation that transforms.
const SCANNING_BYTE_MATCH_OVER_A_QUERY_ARGUMENT_WCU: i64 = 10 + EXTRACTED_COMPONENT_WCU;

/// `[{ "Priority": i, "Type": t }, …]` for the transformation types named, in the
/// order named: `TextTransformation$Priority` and `PreParseTextTransformation$Priority`
/// both "set the relative processing order", which the position in the list is.
fn chain(types: &[&str]) -> Value {
    Value::Array(
        types
            .iter()
            .enumerate()
            .map(|(position, kind)| json!({ "Priority": position, "Type": kind }))
            .collect(),
    )
}

/// One statement of `kind` inspecting `field_to_match`, applying `text` as its
/// `TextTransformations` and `pre_parse` as its `PreParseTextTransformations` when
/// that is given.
///
/// The kinds are the ones that carry a `FieldToMatch` and need no stored resource of
/// their own (`smithy_query(query="shape",shape="Statement")`); the kind that
/// references a regex pattern set is priced in `web_acl_rule_set_preconditions`.
fn statement(kind: &str, field_to_match: &Value, text: &Value, pre_parse: Option<&Value>) -> Value {
    let mut inner = match kind {
        "ByteMatchStatement" => json!({
            "SearchString": base64_of("probe"),
            "FieldToMatch": field_to_match,
            "TextTransformations": text,
            "PositionalConstraint": "CONTAINS",
        }),
        "SizeConstraintStatement" => json!({
            "FieldToMatch": field_to_match,
            "ComparisonOperator": "GT",
            "Size": 1024,
            "TextTransformations": text,
        }),
        "RegexMatchStatement" => json!({
            "RegexString": "^/admin",
            "FieldToMatch": field_to_match,
            "TextTransformations": text,
        }),
        // `SqliMatchStatement` and `XssMatchStatement` carry the component and its
        // transformations and nothing else required.
        _ => json!({
            "FieldToMatch": field_to_match,
            "TextTransformations": text,
        }),
    };
    if let Some(pre_parse) = pre_parse {
        inner["PreParseTextTransformations"] = pre_parse.clone();
    }
    json!({ kind: inner })
}

/// Every statement kind these rows are asserted on.
const KINDS: [&str; 5] = [
    "ByteMatchStatement",
    "SqliMatchStatement",
    "XssMatchStatement",
    "SizeConstraintStatement",
    "RegexMatchStatement",
];

/// The `FieldToMatch` a statement inspects as it arrived, so no extraction is
/// charged for it (`smithy_query(query="shape",shape="FieldToMatch")`).
fn components_inspected_as_they_arrived() -> Vec<(&'static str, Value)> {
    vec![
        ("UriPath", json!({ "UriPath": {} })),
        ("QueryString", json!({ "QueryString": {} })),
        ("Method", json!({ "Method": {} })),
        ("UriFragment", json!({ "UriFragment": {} })),
        ("HeaderOrder", json!({ "HeaderOrder": { "OversizeHandling": "CONTINUE" } })),
        (
            "SingleHeader",
            json!({ "SingleHeader": { "Name": "x-probe" } }),
        ),
    ]
}

/// The `FieldToMatch` WAF parses the raw query string into, which is also what a
/// pre-parse chain normalizes
/// (`smithy_query(query="shape",shape="PreParseTextTransformation")`: "only supported
/// when `FieldToMatch` is `SingleQueryArgument` or `AllQueryArguments`").
fn query_argument_components() -> Vec<(&'static str, Value)> {
    vec![
        (
            "SingleQueryArgument",
            json!({ "SingleQueryArgument": { "Name": "q" } }),
        ),
        ("AllQueryArguments", json!({ "AllQueryArguments": {} })),
    ]
}

/// The `FieldToMatch` WAF has to extract out of a container: the two query-argument
/// components, which the raw query string is parsed into, and the two multi-valued
/// ones, which the match runs over every value of.
fn components_extracted_from_a_container() -> Vec<(&'static str, Value)> {
    let mut components = query_argument_components();
    components.extend([
        (
            "Headers",
            json!({
                "Headers": {
                    "MatchPattern": { "All": {} },
                    "MatchScope": "ALL",
                    "OversizeHandling": "CONTINUE",
                }
            }),
        ),
        (
            "Cookies",
            json!({
                "Cookies": {
                    "MatchPattern": { "All": {} },
                    "MatchScope": "ALL",
                    "OversizeHandling": "CONTINUE",
                }
            }),
        ),
    ]);
    components
}

/// The `Capacity` `CheckCapacity` reports for `rules`.
async fn priced(waf: &Waf, rules: Value) -> i64 {
    let answered = waf
        .ok("CheckCapacity", json!({ "Scope": SCOPE, "Rules": rules }))
        .await;
    answered["Capacity"]
        .as_i64()
        .unwrap_or_else(|| panic!("CheckCapacity must report a numeric Capacity, got {answered}"))
}

/// The `Capacity` reported for one rule carrying `statement`.
async fn priced_statement(waf: &Waf, statement: Value) -> i64 {
    priced(waf, json!([rule("prices", statement)])).await
}

/// A component WAF has to extract out of a container adds the same flat surcharge
/// whichever statement kind inspects it, and a component inspected as it arrived adds
/// nothing: one row per (statement kind × component), measured as the difference from
/// the same statement over the component the kind's own base cost is read on.
#[tokio::test]
async fn a_component_extracted_from_a_container_is_surcharged() {
    let waf = Waf::new();
    let no_transformation = chain(&["NONE"]);
    let baseline_component = json!({ "UriPath": {} });

    for kind in KINDS {
        let base = priced_statement(
            &waf,
            statement(kind, &baseline_component, &no_transformation, None),
        )
        .await;
        let rows = components_inspected_as_they_arrived()
            .into_iter()
            .map(|(component, field)| (component, field, 0))
            .chain(
                components_extracted_from_a_container()
                    .into_iter()
                    .map(|(component, field)| (component, field, EXTRACTED_COMPONENT_WCU)),
            );
        for (component, field_to_match, surcharge) in rows {
            let case = format!("{kind} over {component}");
            let actual =
                priced_statement(&waf, statement(kind, &field_to_match, &no_transformation, None))
                    .await;
            assert_eq!(
                actual,
                base + surcharge,
                "case {case:?}: expected {}, got {actual:?}",
                base + surcharge
            );
        }
    }
}

/// The recorded figure for the statement the surcharge was read on: a scanning byte
/// match over one named query argument, applying no transformation that transforms.
#[tokio::test]
async fn a_scanning_byte_match_over_one_query_argument_prices_its_recorded_figure() {
    let waf = Waf::new();
    let single = json!({ "SingleQueryArgument": { "Name": "q" } });
    let rows: Vec<(&str, Option<Value>)> = vec![
        ("no PreParseTextTransformations member", None),
        ("an empty PreParseTextTransformations", Some(json!([]))),
        (
            "a PreParseTextTransformations that transforms nothing",
            Some(chain(&["NONE"])),
        ),
    ];
    for (case, pre_parse) in rows {
        let actual = priced_statement(
            &waf,
            statement(
                "ByteMatchStatement",
                &single,
                &chain(&["NONE"]),
                pre_parse.as_ref(),
            ),
        )
        .await;
        assert_eq!(
            actual, SCANNING_BYTE_MATCH_OVER_A_QUERY_ARGUMENT_WCU,
            "case {case:?}: expected {SCANNING_BYTE_MATCH_OVER_A_QUERY_ARGUMENT_WCU}, \
             got {actual:?}"
        );
    }
}

/// A pre-parse chain costs exactly what the same chain costs as the statement's
/// standard `TextTransformations`: one row per (statement kind × query-argument
/// component × chain), over the transformation types both enums declare
/// (`smithy_query(query="shape",shape="PreParseTextTransformationType")` and
/// `…shape="TextTransformationType"`), from a chain that transforms nothing to one of
/// two transformations.
#[tokio::test]
async fn a_pre_parse_chain_costs_what_the_same_standard_chain_costs() {
    let waf = Waf::new();
    let no_transformation = chain(&["NONE"]);
    let chains = [
        ("a chain of nothing but the no-op", chain(&["NONE"])),
        ("one transformation", chain(&["URL_DECODE"])),
        (
            "two transformations",
            chain(&["URL_DECODE", "URL_DECODE_UNI"]),
        ),
    ];

    for kind in KINDS {
        for (component, field_to_match) in query_argument_components() {
            for (shape, applied) in &chains {
                let case = format!("{kind} over {component} applying {shape}");
                let as_standard =
                    priced_statement(&waf, statement(kind, &field_to_match, applied, None)).await;
                let as_pre_parse = priced_statement(
                    &waf,
                    statement(kind, &field_to_match, &no_transformation, Some(applied)),
                )
                .await;
                assert_eq!(
                    as_pre_parse, as_standard,
                    "case {case:?}: expected {as_standard}, got {as_pre_parse:?}"
                );

                // The two chains transform different content — the raw query string
                // and the argument parsed out of it — so a statement that applies
                // both pays for both.
                let both = priced_statement(
                    &waf,
                    statement(kind, &field_to_match, applied, Some(applied)),
                )
                .await;
                let expected = as_standard
                    + (as_standard - priced_statement(
                        &waf,
                        statement(kind, &field_to_match, &no_transformation, None),
                    )
                    .await);
                assert_eq!(
                    both, expected,
                    "case {case:?} applied twice: expected {expected}, got {both:?}"
                );
            }
        }
    }
}

/// The transformed content is computed once however many statements inspect it, so
/// two statements normalizing the raw query string with the SAME pre-parse chain pay
/// that chain once, while two different chains are two charges. The figure is the one
/// a stored web ACL reports as well.
#[tokio::test]
async fn one_pre_parse_chain_is_charged_once_for_the_whole_rule_list() {
    let waf = Waf::new();
    let no_transformation = chain(&["NONE"]);
    let first = json!({ "SingleQueryArgument": { "Name": "q" } });
    let second = json!({ "SingleQueryArgument": { "Name": "r" } });
    let one = chain(&["URL_DECODE"]);
    let other = chain(&["URL_DECODE_UNI"]);
    let byte_match = |field_to_match: &Value, pre_parse: &Value| {
        statement(
            "ByteMatchStatement",
            field_to_match,
            &no_transformation,
            Some(pre_parse),
        )
    };
    let two_statements = SCANNING_BYTE_MATCH_OVER_A_QUERY_ARGUMENT_WCU * 2;
    let rows = [
        (
            "two statements sharing one pre-parse chain",
            ordered(json!([
                rule("a", byte_match(&first, &one)),
                rule("b", byte_match(&second, &one)),
            ])),
            two_statements + TRANSFORMATION_WCU,
        ),
        (
            "two statements with a pre-parse chain each",
            ordered(json!([
                rule("a", byte_match(&first, &one)),
                rule("b", byte_match(&second, &other)),
            ])),
            two_statements + 2 * TRANSFORMATION_WCU,
        ),
    ];

    for (index, (case, rules, expected)) in rows.iter().enumerate() {
        let actual = priced(&waf, rules.clone()).await;
        assert_eq!(
            actual, *expected,
            "case {case:?}: expected {expected}, got {actual:?}"
        );

        // The web ACL that stores those rules reports the price it was quoted.
        let name = format!("query-argument-capacity-{index}");
        let (id, _arn, _lock) = create_web_acl(&waf, SCOPE, &name, rules.clone()).await;
        let got = waf
            .ok("GetWebACL", json!({ "Name": &name, "Scope": SCOPE, "Id": id }))
            .await;
        let stored = got["WebACL"]["Capacity"].as_i64();
        assert_eq!(
            stored,
            Some(*expected),
            "case {case:?}: the stored web ACL must report {expected}, got {stored:?}"
        );
    }
}
