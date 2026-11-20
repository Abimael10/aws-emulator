// SPDX-License-Identifier: CC0-1.0

//! Joining ONE batch read's answers back to the selector strings the request asked with,
//! and losing exactly the parameter a batch delete removed.
//!
//! RULE under test: every entry a batch read reports carries the parameter's own `Name`
//! beside the selector it resolved by in the documented `parameter_name:version` /
//! `parameter_name:label` form — separator included
//! (`smithy_query(query="doc",shape="Parameter",member="Selector")`: "Either the version
//! number or the label used to retrieve the parameter value. Specify selectors by using one
//! of the following formats: parameter_name:version parameter_name:label") — so
//! `Name` + `Selector` reproduces the string the caller asked that entry with and resolves
//! to the very version served, while a name the read resolves to no version is reported in
//! `InvalidParameters` under the string asked, in the order asked. A read of a name with no
//! selector carries no `Selector` at all, so the same concatenation is the bare name.
//!
//! That is what makes ONE call over a MIXED set of selectors joinable: a client that keys
//! its results by the strings it sent finds each of them exactly once. An entry that echoed
//! its selector without the separator would collapse two different asks (`<name>:1` and
//! `<name>1`) into one key and name a parameter nothing created.
//!
//! The rows differ along what the rule generalises over: the form and depth of the
//! parameter's name (a top-level name, a four-level hierarchy), how many versions it
//! carries, the selector's kind (the oldest version, a middle one, the newest, a label
//! pointing at a middle one, none at all), the size of the value each version holds, and
//! whether the selector names a version the parameter carries at all. The sibling suite
//! `all/parameter_read_payload_shape.rs` pins the same echo one selector per call; what this
//! suite adds is the WHOLE batch in one call, keyed the way a batch client keys it, and the
//! same read after a batch delete.
//!
//! Every call goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router (`OPERATIONS.md`, "Wire protocol") — through this crate's one
//! harness (`all/parameter_store.rs`), so nothing reaches into a state struct.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, router};

/// How many names ONE batch read may carry (`ParameterNameList`, `@length` 1..10, which
/// both `GetParametersRequest$Names` and `GetParametersResult$InvalidParameters` target).
const MAX_NAMES_PER_CALL: usize = 10;

/// A top-level name: one that sits in no hierarchy, so it carries no separator at all.
const PLAIN: &str = "batch-join-plain";

/// The values [`PLAIN`]'s versions are written with, in order.
const PLAIN_VALUES: &[&str] = &["p-one", "p-two"];

/// The label attached to [`PLAIN`]'s OLDEST version, so a label selector of a top-level
/// name resolves to a version that is not the newest.
const PLAIN_LABEL: &str = "Release-1";

/// A four-level hierarchy name, written with a leading separator as a hierarchical name is.
const NESTED: &str = "/batch/join/app/config";

/// The values [`NESTED`]'s versions are written with, in order — each one longer than the
/// last, so no two versions of it hold a value of the same size.
const NESTED_VALUES: &[&str] = &["rev-1", "rev-2-xx", "rev-3-xxx", "rev-4-xxxx"];

/// The label attached to [`NESTED`]'s THIRD version: a label is resolved to the version it
/// was attached to rather than to the newest one.
const NESTED_LABEL: &str = "pinned.release-3";

/// A version number no parameter here carries, so a selector naming it resolves to nothing.
const ABSENT_VERSION: &str = "9";

// The table below is ONE call's worth of names, which is what makes the whole of it joinable:
// checked where the table is written rather than where it is sent.
const _: () = assert!(ROWS.len() <= MAX_NAMES_PER_CALL);

/// One row of the batch: the parameter and selector one name of the request addresses, and
/// what the read must answer for it.
struct Row {
    case: &'static str,
    /// The parameter's name, as the request spells it.
    name: &'static str,
    /// The selector the request appends after the separator, or `None` for a bare name.
    selector: Option<&'static str>,
    /// The version and value this row must resolve to, or `None` when it names no version
    /// the parameter carries — reported among the unresolvable names instead.
    resolves_to: Option<(i64, &'static str)>,
}

impl Row {
    /// The `Name` this row sends: the parameter name, with the selector after the
    /// separator the model documents when the row carries one.
    fn asked(&self) -> String {
        match self.selector {
            Some(selector) => format!("{}:{selector}", self.name),
            None => self.name.to_string(),
        }
    }
}

/// The batch: nine of the ten names one call may carry, mixing both parameters, every
/// selector kind and one selector that resolves to nothing.
const ROWS: &[Row] = &[
    Row {
        case: "the oldest version of the hierarchy name",
        name: NESTED,
        selector: Some("1"),
        resolves_to: Some((1, "rev-1")),
    },
    Row {
        case: "a middle version of the hierarchy name",
        name: NESTED,
        selector: Some("2"),
        resolves_to: Some((2, "rev-2-xx")),
    },
    Row {
        case: "the version the hierarchy name's label points at, asked by number",
        name: NESTED,
        selector: Some("3"),
        resolves_to: Some((3, "rev-3-xxx")),
    },
    Row {
        case: "the newest version of the hierarchy name, asked by number",
        name: NESTED,
        selector: Some("4"),
        resolves_to: Some((4, "rev-4-xxxx")),
    },
    Row {
        case: "the hierarchy name's label, which names its third version",
        name: NESTED,
        selector: Some(NESTED_LABEL),
        resolves_to: Some((3, "rev-3-xxx")),
    },
    Row {
        case: "the hierarchy name with no selector at all",
        name: NESTED,
        selector: None,
        resolves_to: Some((4, "rev-4-xxxx")),
    },
    Row {
        case: "the top-level name's label, which names its oldest version",
        name: PLAIN,
        selector: Some(PLAIN_LABEL),
        resolves_to: Some((1, "p-one")),
    },
    Row {
        case: "the top-level name with no selector at all",
        name: PLAIN,
        selector: None,
        resolves_to: Some((2, "p-two")),
    },
    Row {
        case: "a version number neither parameter carries",
        name: NESTED,
        selector: Some(ABSENT_VERSION),
        resolves_to: None,
    },
];

/// One operation, asserting it was served, and answering its body.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, response) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} {body} must be served, got {status} with {response}"
    );
    response
}

/// Write `values` as consecutive versions of `name`.
async fn write_versions(router: &MultiServiceRouter, name: &str, values: &[&str]) {
    for (index, value) in values.iter().enumerate() {
        let mut body = json!({ "Name": name, "Type": "String", "Value": value });
        if index > 0 {
            body["Overwrite"] = json!(true);
        }
        let written = ok(router, name, "PutParameter", &body).await;
        let expected = index as i64 + 1;
        assert_eq!(
            written["Version"].as_i64(),
            Some(expected),
            "case {name:?}: the write of {value:?} must create version {expected}, got {written}"
        );
    }
}

/// Every entry of one batch answer, keyed by its reported `Name` + `Selector` — the key a
/// batch client joins its request by — against the version and value it reports.
fn joined(case: &str, answer: &Value) -> BTreeMap<String, (Option<i64>, Option<String>)> {
    let reported = answer["Parameters"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries a Parameters list: {answer}"));
    let mut joined = BTreeMap::new();
    for parameter in reported {
        // A selector that was echoed without its separator would key this entry under a
        // name nothing was asked for, which is exactly what the join below catches.
        let key = format!(
            "{}{}",
            parameter["Name"].as_str().unwrap_or_default(),
            parameter["Selector"].as_str().unwrap_or_default()
        );
        let value = (
            parameter["Version"].as_i64(),
            parameter["Value"].as_str().map(str::to_string),
        );
        assert!(
            joined.insert(key.clone(), value).is_none(),
            "case {case:?}: expected each asked-for selector to be answered once, got {key:?} \
             twice in {answer}"
        );
    }
    joined
}

/// The `InvalidParameters` of one batch answer, in the order reported.
fn unresolvable(case: &str, answer: &Value) -> Vec<String> {
    answer["InvalidParameters"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the answer carries an InvalidParameters list: {answer}")
        })
        .iter()
        .map(|name| name.as_str().unwrap_or_default().to_string())
        .collect()
}

/// ONE batch read over a mixed set of version- and label-pinned selectors answers every
/// name it was asked with under the string it was asked by, so the whole answer joins back
/// to the request; and each joined key, fed back as a `Name`, resolves to the very version
/// that key was served.
#[tokio::test]
async fn one_batch_read_joins_back_to_every_selector_it_was_asked_with() {
    let router = router();
    let names: Vec<String> = ROWS.iter().map(Row::asked).collect();

    write_versions(&router, NESTED, NESTED_VALUES).await;
    write_versions(&router, PLAIN, PLAIN_VALUES).await;
    ok(
        &router,
        "label the hierarchy name's third version",
        "LabelParameterVersion",
        &json!({ "Name": NESTED, "ParameterVersion": 3, "Labels": [NESTED_LABEL] }),
    )
    .await;
    ok(
        &router,
        "label the top-level name's oldest version",
        "LabelParameterVersion",
        &json!({ "Name": PLAIN, "ParameterVersion": 1, "Labels": [PLAIN_LABEL] }),
    )
    .await;

    let case = "a mixed batch of version- and label-pinned selectors";
    let answer = ok(&router, case, "GetParameters", &json!({ "Names": names })).await;

    // What the whole answer must join to: one entry per row that resolves, keyed by the
    // string that row asked with.
    let expected: BTreeMap<String, (Option<i64>, Option<String>)> = ROWS
        .iter()
        .filter_map(|row| {
            row.resolves_to
                .map(|(version, value)| (row.asked(), (Some(version), Some(value.to_string()))))
        })
        .collect();
    assert_eq!(
        joined(case, &answer),
        expected,
        "case {case:?}: every entry must be keyed by Name + Selector back to the string it \
         was asked with, got {answer}"
    );

    // Each entry's own echo: the selector in the documented form, separator and all, and
    // nothing echoed for a name asked without one.
    let reported = answer["Parameters"].as_array().cloned().unwrap_or_default();
    for row in ROWS.iter().filter(|row| row.resolves_to.is_some()) {
        let asked = row.asked();
        let echoed = row.selector.map(|selector| format!(":{selector}"));
        let entry = reported
            .iter()
            .find(|parameter| {
                parameter["Name"].as_str() == Some(row.name)
                    && parameter["Selector"].as_str() == echoed.as_deref()
            })
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected an entry for {asked:?} reporting Name {:?} and the \
                     selector {echoed:?}, got {answer}",
                    row.case, row.name,
                )
            });
        match row.selector {
            Some(selector) => assert_eq!(
                entry["Selector"].as_str(),
                Some(format!(":{selector}").as_str()),
                "case {:?}: the echoed selector carries its separator, got {entry}",
                row.case
            ),
            // Nothing selected, so nothing is echoed: the member is absent, which a reader
            // sees as the JSON null the sibling suites assert.
            None => assert!(
                entry["Selector"].is_null(),
                "case {:?}: a name asked with no selector reports none, got {entry}",
                row.case
            ),
        }
    }

    // The invariant behind the join: the key is itself a `Name` that resolves to the
    // version it was served.
    for (key, (version, value)) in joined(case, &answer) {
        let again = ok(&router, &key, "GetParameter", &json!({ "Name": &key })).await;
        assert_eq!(
            (
                again["Parameter"]["Version"].as_i64(),
                again["Parameter"]["Value"].as_str().map(str::to_string),
            ),
            (version, value.clone()),
            "case {key:?}: the reference rebuilt from the batch answer must serve version \
             {version:?} and {value:?}, got {again}"
        );
    }

    // A name that resolves to no version is reported as unresolvable under the string
    // asked, in the order asked — not refused, and not silently dropped.
    let expected_invalid: Vec<String> = ROWS
        .iter()
        .filter(|row| row.resolves_to.is_none())
        .map(Row::asked)
        .collect();
    assert_eq!(
        unresolvable(case, &answer),
        expected_invalid,
        "case {case:?}: expected {expected_invalid:?} reported unresolvable, got {answer}"
    );

    // "The results for GetParameters requests are listed in alphabetical order in query
    // responses" (`GetParametersRequest$Names`), whatever order the batch asked in. Several
    // selectors of one parameter report the one `Name`, so the version each resolved to
    // orders them within it — which is what makes this an ordering the six entries of the
    // hierarchy name below can violate.
    let reported_order: Vec<(&str, Option<i64>)> = reported
        .iter()
        .map(|parameter| {
            (
                parameter["Name"].as_str().unwrap_or_default(),
                parameter["Version"].as_i64(),
            )
        })
        .collect();
    let mut ordered = reported_order.clone();
    ordered.sort_by(|left, right| left.0.cmp(right.0).then(left.1.cmp(&right.1)));
    assert_eq!(
        reported_order, ordered,
        "case {case:?}: expected the entries in alphabetical order of their names and by the \
         version each selector resolved to within one name, got {answer}"
    );
}

/// A batch delete takes the parameter it named away WHOLE, so the same batch read loses
/// every version and every label of that parameter — each reported unresolvable under the
/// string asked — while the parameter the delete did not name keeps being joined exactly as
/// before.
#[tokio::test]
async fn a_batch_delete_loses_every_version_and_label_of_the_name_it_removed() {
    let router = router();
    write_versions(&router, NESTED, NESTED_VALUES).await;
    write_versions(&router, PLAIN, PLAIN_VALUES).await;
    ok(
        &router,
        "label the hierarchy name's third version",
        "LabelParameterVersion",
        &json!({ "Name": NESTED, "ParameterVersion": 3, "Labels": [NESTED_LABEL] }),
    )
    .await;
    ok(
        &router,
        "label the top-level name's oldest version",
        "LabelParameterVersion",
        &json!({ "Name": PLAIN, "ParameterVersion": 1, "Labels": [PLAIN_LABEL] }),
    )
    .await;

    let names: Vec<String> = ROWS.iter().map(Row::asked).collect();
    let before = ok(
        &router,
        "before the delete",
        "GetParameters",
        &json!({ "Names": &names }),
    )
    .await;
    assert_eq!(
        joined("before the delete", &before).len(),
        ROWS.iter().filter(|row| row.resolves_to.is_some()).count(),
        "before the delete every resolving row must be served, got {before}"
    );

    let deleted = ok(
        &router,
        "the batch delete",
        "DeleteParameters",
        &json!({ "Names": [NESTED] }),
    )
    .await;
    assert_eq!(
        deleted["DeletedParameters"],
        json!([NESTED]),
        "the delete must report the one name it removed, got {deleted}"
    );

    let case = "after the delete";
    let after = ok(&router, case, "GetParameters", &json!({ "Names": &names })).await;

    // What survives: only the rows naming the parameter the delete left alone, still keyed
    // by the strings they were asked with.
    let expected: BTreeMap<String, (Option<i64>, Option<String>)> = ROWS
        .iter()
        .filter(|row| row.name == PLAIN)
        .filter_map(|row| {
            row.resolves_to
                .map(|(version, value)| (row.asked(), (Some(version), Some(value.to_string()))))
        })
        .collect();
    assert_eq!(
        joined(case, &after),
        expected,
        "case {case:?}: only the parameter the delete left alone is still joined, got {after}"
    );

    // Every version and every label of the removed parameter is now unresolvable, under the
    // string asked and in the order asked.
    let expected_invalid: Vec<String> = ROWS
        .iter()
        .filter(|row| row.name == NESTED || row.resolves_to.is_none())
        .map(Row::asked)
        .collect();
    assert_eq!(
        unresolvable(case, &after),
        expected_invalid,
        "case {case:?}: expected {expected_invalid:?} reported unresolvable, got {after}"
    );

    // The single read of the same names answers the same way a batch read's partition does:
    // the removed parameter misses, through its label as much as through a version number.
    for row in ROWS.iter().filter(|row| row.name == NESTED) {
        let asked = row.asked();
        let (status, missed) = call(&router, "GetParameter", &json!({ "Name": &asked })).await;
        assert_eq!(
            (status, error_code(&missed).as_str()),
            (400, "ParameterNotFound"),
            "case {:?}: expected {asked:?} to miss after the delete, got {status} with {missed}",
            row.case
        );
    }
}
