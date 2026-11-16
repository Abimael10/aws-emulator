// SPDX-License-Identifier: CC0-1.0

//! What an OVERWRITE of a parameter leaves of the attributes the request does not carry, end
//! to end over the awsJson1_1 wire.
//!
//! RULE under test: **a `PutParameter` that overwrites an existing parameter changes the
//! attributes it STATES; an attribute it states nothing about keeps the value the parameter's
//! current version holds.** That is the inheritance "specifying a parameter type isn't
//! required when updating a parameter" states for the `Type`, and the one a `Tier` an update
//! names no value for goes through ("you can change a standard parameter to an advanced
//! parameter any time. But you can't revert an advanced parameter to a standard parameter") —
//! and it holds of the parameter's other attributes alike, so a client that overwrites a
//! value does not silently lose the description, the pattern, the data type or the key the
//! parameter was created with.
//!
//! The rows differ along what the rule generalises over: WHICH attribute the overwrite omits
//! (each one on its own, all of them at once, and an overwrite that states a new value for
//! one), read back through `DescribeParameters` — the metadata read a client checks a
//! parameter's configuration with.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::parameter_store::{call, router};

/// The attributes the create gives the parameter, which an overwrite that states none of them
/// has to leave exactly as they are.
const DESCRIPTION: &str = "created with a description";
const PATTERN: &str = "^[a-z0-9-]+$";
const DATA_TYPE: &str = "text";

/// One row: the body the overwrite carries beside the name and the value, and the
/// `(Description, AllowedPattern, DataType)` the parameter then reports.
struct Row {
    case: &'static str,
    name: &'static str,
    /// The members the overwrite states, beside `Name`, `Value` and `Overwrite`.
    stated: Value,
    description: Option<&'static str>,
    allowed_pattern: Option<&'static str>,
    data_type: &'static str,
}

/// The class the rule covers: an overwrite omitting any subset of the parameter's
/// attributes, and one that states a new value for one of them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an overwrite that states none of them",
            name: "/overwrite/bare",
            stated: json!({}),
            description: Some(DESCRIPTION),
            allowed_pattern: Some(PATTERN),
            data_type: DATA_TYPE,
        },
        Row {
            case: "an overwrite that restates the description alone",
            name: "/overwrite/described",
            stated: json!({"Description": "changed"}),
            description: Some("changed"),
            allowed_pattern: Some(PATTERN),
            data_type: DATA_TYPE,
        },
        Row {
            case: "an overwrite that restates the pattern alone",
            name: "/overwrite/patterned",
            stated: json!({"AllowedPattern": "^[a-z]+$"}),
            description: Some(DESCRIPTION),
            allowed_pattern: Some("^[a-z]+$"),
            data_type: DATA_TYPE,
        },
        Row {
            case: "an overwrite that states every one of them",
            name: "/overwrite/restated",
            stated: json!({
                "Description": "restated",
                "AllowedPattern": "^[a-z]+$",
                "DataType": "text",
            }),
            description: Some("restated"),
            allowed_pattern: Some("^[a-z]+$"),
            data_type: "text",
        },
    ]
}

/// The `(Description, AllowedPattern, DataType)` `DescribeParameters` reports for `name`.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let (status, body) = call(
        router,
        "DescribeParameters",
        &json!({"ParameterFilters": [{"Key": "Name", "Values": [name]}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeParameters must succeed, got {status} {body}"
    );
    let reported = body["Parameters"]
        .as_array()
        .and_then(|parameters| parameters.first().cloned())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the describe must report {name}, got {body}")
        });
    let text = |member: &str| {
        reported
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    (text("Description"), text("AllowedPattern"), text("DataType"))
}

#[tokio::test]
async fn an_overwrite_keeps_every_attribute_it_states_nothing_about() {
    let router = router();
    for row in rows() {
        let (status, body) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": row.name,
                "Type": "String",
                "Value": "before",
                "Description": DESCRIPTION,
                "AllowedPattern": PATTERN,
                "DataType": DATA_TYPE,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the create must succeed, got {status} {body}",
            row.case
        );

        let mut overwrite = json!({"Name": row.name, "Value": "after", "Overwrite": true});
        for (member, value) in row.stated.as_object().expect("the stated members are a map") {
            overwrite[member] = value.clone();
        }
        let (status, body) = call(&router, "PutParameter", &overwrite).await;
        assert_eq!(
            status, 200,
            "case {:?}: the overwrite {overwrite} must succeed, got {status} {body}",
            row.case
        );

        let reported = described(&router, row.case, row.name).await;
        let expected = (
            row.description.map(str::to_string),
            row.allowed_pattern.map(str::to_string),
            Some(row.data_type.to_string()),
        );
        assert_eq!(
            reported, expected,
            "case {:?}: expected the overwritten parameter to report {expected:?}, got \
             {reported:?}",
            row.case
        );

        // The value the overwrite really wrote, so no row passes on a put nothing applied.
        let (status, body) = call(&router, "GetParameter", &json!({"Name": row.name})).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetParameter must succeed, got {status} {body}",
            row.case
        );
        assert_eq!(
            body["Parameter"]["Value"].as_str(),
            Some("after"),
            "case {:?}: expected the overwrite to write the new value, got {body}",
            row.case
        );
    }
}
