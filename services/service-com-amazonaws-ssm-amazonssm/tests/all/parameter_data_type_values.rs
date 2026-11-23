// SPDX-License-Identifier: CC0-1.0

//! What a parameter's `DataType` decides about a put, end to end over the awsJson1_1 wire.
//!
//! RULE under test: **`PutParameter` accepts the data type the request declares whatever it
//! names, and holds the VALUE and the TIER to what that data type states.**
//! `ParameterDataType` is a
//! plain string carrying a length (0..128) and no enum and no pattern
//! (`smithy_query(query="shape",shape="ParameterDataType")`), so every data-type string of
//! that length is a value the member admits — `PutParameter`'s documentation LISTS the data
//! types it describes ("The following data type values are supported. text aws:ec2:image
//! aws:ssm:integration", `smithy_query(query="doc",shape="PutParameterRequest",
//! member="DataType")`) rather than closing the set the wire takes. What the service refuses
//! is a value that is not in the form its data type states, and the sentence it is refused
//! with names the VALUE, because the value is what the caller has to change; a data type the
//! service serves only on an ADVANCED parameter is refused for every put that resolved to
//! another tier, with the sentence naming that data type.
//!
//! The rows differ along what the rule generalises over: WHICH data type the put declares
//! (each one the documentation lists, one whose value form the service states, one served
//! only on an advanced parameter, one no documentation names, one at the modelled length
//! limit), WHICH value it carries (a whole number, a signed one, zero, and each way the
//! stated form can be broken), which TIER it resolves to — and whether the data type came
//! from this request or from the parameter it is overwriting.
//!
//! Every case goes through the front door a client uses, and every write is read back through
//! `GetParameter` and ended with `DeleteParameter`, so no row passes on a put that stored
//! nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::parameter_store::{call, error_code, error_message, router, ACCOUNT, REGION};

/// The sentence a value that is not a whole number is refused under the `integer` data type
/// with: FIXED text, carrying nothing from the request or from the store, which states the
/// form the value has to be in. It is the form this suite's rows are built from.
const INTEGER_REFUSAL: &str = "The parameter value is not a valid integer. A value must be \
     an optional minus sign followed by digits, with no leading zeros, no plus sign, no \
     decimal point and no whitespace.";

/// The data type whose value form the service states, and whose refusal
/// [`INTEGER_REFUSAL`] is.
const INTEGER: &str = "integer";

/// A data type the service serves only on an ADVANCED parameter: a standard parameter may
/// not declare it at all.
const ADVANCED_ONLY: &str = "url";

/// The sentence a put declaring [`ADVANCED_ONLY`] outside the advanced tier is refused
/// with — fixed text around the data type the request declared.
fn advanced_only_refusal(data_type: &str) -> String {
    format!("The {data_type} data type is only supported for advanced parameters.")
}

/// The modelled length of `ParameterDataType`
/// (`smithy_query(query="shape",shape="ParameterDataType")`: min 0, max 128) — the one
/// constraint the member carries, so a data type this long is still a value it admits.
const MAX_DATA_TYPE_LENGTH: usize = 128;

/// The content size limit of the standard tier ("Standard parameters have a content size
/// limit of 4 KB", `smithy_query(query="doc",shape="PutParameterRequest",member="Tier")`), so
/// a value past it is one only an advanced parameter holds.
const STANDARD_VALUE_LIMIT: usize = 4 * 1024;

/// One accepted-data-type row: the case, the parameter name, the data type the put declares
/// and the value it carries.
struct Accepted {
    case: &'static str,
    name: &'static str,
    data_type: String,
    value: &'static str,
}

/// The class the rule covers on the accepting side: every data type the documentation lists
/// except the asynchronously validated one (whose write is gated elsewhere,
/// `all/parameter_image_data_type_validation.rs`), the data type whose value form the service
/// states, a data type no documentation names, and one at the modelled length limit.
fn accepted() -> Vec<Accepted> {
    vec![
        Accepted {
            case: "the default data type",
            name: "/data-type/text",
            data_type: "text".to_string(),
            value: "plain text",
        },
        Accepted {
            case: "the integration data type",
            name: "/data-type/integration",
            data_type: "aws:ssm:integration".to_string(),
            value: "subscription",
        },
        Accepted {
            case: "a whole-number data type",
            name: "/data-type/integer",
            data_type: INTEGER.to_string(),
            value: "443",
        },
        Accepted {
            case: "a negative whole number",
            name: "/data-type/negative",
            data_type: INTEGER.to_string(),
            value: "-17",
        },
        Accepted {
            case: "zero",
            name: "/data-type/zero",
            data_type: INTEGER.to_string(),
            value: "0",
        },
        Accepted {
            case: "a long run of digits",
            name: "/data-type/long",
            data_type: INTEGER.to_string(),
            value: "900719925474099",
        },
        Accepted {
            case: "a data type no documentation names",
            name: "/data-type/invented",
            data_type: "text/plain".to_string(),
            value: "whatever this caller means by it",
        },
        Accepted {
            case: "a data type at the modelled length limit",
            name: "/data-type/longest",
            data_type: "d".repeat(MAX_DATA_TYPE_LENGTH),
            value: "stored all the same",
        },
    ]
}

/// One refused-value row: the case and the value a put under [`INTEGER`] carries.
struct Refused {
    case: &'static str,
    value: &'static str,
}

/// The class the rule covers on the refusing side: each way the form
/// [`INTEGER_REFUSAL`] states can be broken.
fn refused() -> Vec<Refused> {
    vec![
        Refused {
            case: "a decimal point",
            value: "80.5",
        },
        Refused {
            case: "a leading zero",
            value: "0443",
        },
        Refused {
            case: "a plus sign",
            value: "+443",
        },
        Refused {
            case: "leading whitespace",
            value: " 443",
        },
        Refused {
            case: "trailing whitespace",
            value: "443 ",
        },
        Refused {
            case: "whitespace between digits",
            value: "4 43",
        },
        Refused {
            case: "a minus sign alone",
            value: "-",
        },
        Refused {
            case: "words",
            value: "four hundred and forty three",
        },
        Refused {
            case: "an exponent",
            value: "4e3",
        },
        Refused {
            case: "a thousands separator",
            value: "1,000",
        },
        Refused {
            case: "hexadecimal",
            value: "0x1f",
        },
    ]
}

/// `GetParameter` of `name`, returning `(status, body)`.
async fn get(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GetParameter", &json!({ "Name": name })).await
}

#[tokio::test]
async fn a_put_takes_the_data_type_it_declares_and_the_parameter_reads_back_under_it() {
    let router = router();
    for row in accepted() {
        let (status, written) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": row.name,
                "Type": "String",
                "Value": row.value,
                "DataType": row.data_type,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: PutParameter under DataType {:?} must succeed, got {status} {written}",
            row.case, row.data_type
        );
        assert_eq!(
            written["Version"].as_i64(),
            Some(1),
            "case {:?}: the create reports the version it allocated, got {written}",
            row.case
        );
        assert_eq!(
            written["Tier"].as_str(),
            Some("Standard"),
            "case {:?}: the create reports the tier it resolved, got {written}",
            row.case
        );

        // The write is read back through the reader a client uses, so the row cannot pass on
        // a put that stored nothing.
        let (status, read) = get(&router, row.name).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetParameter must succeed, got {status} {read}",
            row.case
        );
        let parameter = &read["Parameter"];
        assert_eq!(
            parameter["DataType"].as_str(),
            Some(row.data_type.as_str()),
            "case {:?}: expected the stored DataType, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["Value"].as_str(),
            Some(row.value),
            "case {:?}: expected the stored Value, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["Type"].as_str(),
            Some("String"),
            "case {:?}: expected the stored Type, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["Version"].as_i64(),
            Some(1),
            "case {:?}: expected the stored Version, got {parameter}",
            row.case
        );
        assert_eq!(
            parameter["ARN"].as_str(),
            Some(
                format!(
                    "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{}",
                    row.name
                )
                .as_str()
            ),
            "case {:?}: expected the parameter's own ARN, got {parameter}",
            row.case
        );
        assert!(
            parameter["LastModifiedDate"].is_number(),
            "case {:?}: expected a LastModifiedDate, got {parameter}",
            row.case
        );

        // ...and the lifecycle a client built on such a parameter ends the same way.
        let (status, deleted) = call(
            &router,
            "DeleteParameter",
            &json!({ "Name": row.name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: DeleteParameter must succeed, got {status} {deleted}",
            row.case
        );
        let (status, gone) = get(&router, row.name).await;
        assert_eq!(
            status, 400,
            "case {:?}: the deleted parameter is gone, got {status} {gone}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_value_outside_the_form_its_data_type_states_is_refused_for_the_value() {
    let router = router();
    for row in refused() {
        let name = "/data-type/refused";
        let (status, response) = call(
            &router,
            "PutParameter",
            &json!({
                "Name": name,
                "Type": "String",
                "Value": row.value,
                "DataType": INTEGER,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: PutParameter of {:?} must be refused, got {status} {response}",
            row.case, row.value
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {:?}: expected ValidationException, got {response}",
            row.case
        );
        assert_eq!(
            error_message(&response),
            INTEGER_REFUSAL,
            "case {:?}: expected the value-level sentence, got {response}",
            row.case
        );
        // A refused put wrote nothing, so the name resolves to no parameter.
        let (status, gone) = get(&router, name).await;
        assert_eq!(
            status, 400,
            "case {:?}: the refused put stored nothing, got {status} {gone}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_put_with_no_value_at_all_is_refused_under_a_data_type_that_states_a_form() {
    let router = router();
    let name = "/data-type/empty-value";
    let (status, response) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Type": "String", "Value": "", "DataType": INTEGER }),
    )
    .await;
    assert_eq!(
        status, 400,
        "an empty value is no whole number, got {status} {response}"
    );
    assert_eq!(
        error_message(&response),
        INTEGER_REFUSAL,
        "expected the value-level sentence, got {response}"
    );
}

#[tokio::test]
async fn a_data_type_served_only_on_an_advanced_parameter_turns_on_the_resolved_tier() {
    let router = router();
    // (case, the `Tier` the put names, the value's size, whether the put is served)
    let rows: Vec<(&str, Option<&str>, usize, bool)> = vec![
        ("a put naming no tier", None, 32, false),
        ("a put naming the standard tier", Some("Standard"), 32, false),
        (
            "a put asking the service to choose, with a value a standard parameter holds",
            Some("Intelligent-Tiering"),
            32,
            false,
        ),
        ("a put naming the advanced tier", Some("Advanced"), 32, true),
        (
            "a put asking the service to choose, with a value only an advanced parameter holds",
            Some("Intelligent-Tiering"),
            STANDARD_VALUE_LIMIT + 1,
            true,
        ),
    ];
    for (index, (case, tier, size, served)) in rows.into_iter().enumerate() {
        let name = format!("/data-type/advanced-only/{index}");
        let value = format!("https://example.invalid/{}", "p".repeat(size));
        let mut body = json!({
            "Name": &name,
            "Type": "String",
            "Value": value,
            "DataType": ADVANCED_ONLY,
        });
        if let Some(tier) = tier {
            body["Tier"] = json!(tier);
        }
        let (status, response) = call(&router, "PutParameter", &body).await;
        if !served {
            assert_eq!(
                status, 400,
                "case {case:?}: the put must be refused, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {response}"
            );
            assert_eq!(
                error_message(&response),
                advanced_only_refusal(ADVANCED_ONLY),
                "case {case:?}: expected the tier-level sentence, got {response}"
            );
            let (status, gone) = get(&router, &name).await;
            assert_eq!(
                status, 400,
                "case {case:?}: the refused put stored nothing, got {status} {gone}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: the put must succeed, got {status} {response}"
        );
        assert_eq!(
            response["Tier"].as_str(),
            Some("Advanced"),
            "case {case:?}: the served put reports the advanced tier, got {response}"
        );
        let (status, read) = get(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetParameter must succeed, got {status} {read}"
        );
        assert_eq!(
            read["Parameter"]["DataType"].as_str(),
            Some(ADVANCED_ONLY),
            "case {case:?}: expected the stored DataType, got {read}"
        );
    }

    // An update that states no data type and no tier inherits both, so the parameter keeps
    // the data type its advanced tier admits.
    let name = "/data-type/advanced-only/0";
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({
            "Name": name,
            "Type": "String",
            "Value": "https://example.invalid/first",
            "DataType": ADVANCED_ONLY,
            "Tier": "Advanced",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the advanced create must succeed, got {status} {written}"
    );
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({
            "Name": name,
            "Value": "https://example.invalid/second",
            "Overwrite": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "an update of the advanced parameter must succeed, got {status} {written}"
    );
    let (status, read) = get(&router, name).await;
    assert_eq!(status, 200, "the update is readable, got {status} {read}");
    assert_eq!(
        (
            read["Parameter"]["Value"].as_str(),
            read["Parameter"]["DataType"].as_str()
        ),
        (Some("https://example.invalid/second"), Some(ADVANCED_ONLY)),
        "expected the updated value under the inherited data type, got {read}"
    );
}

#[tokio::test]
async fn an_overwrite_is_judged_against_the_data_type_the_parameter_already_carries() {
    let router = router();
    let name = "/data-type/inherited";
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Type": "String", "Value": "1", "DataType": INTEGER }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the create must succeed, got {status} {written}"
    );

    // An update states no data type, so the one in force is the parameter's own — and the
    // value is held to the form that data type states.
    let (status, response) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Value": "two", "Overwrite": true }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the inherited data type still judges the value, got {status} {response}"
    );
    assert_eq!(
        error_message(&response),
        INTEGER_REFUSAL,
        "expected the value-level sentence, got {response}"
    );

    // The refused overwrite left the parameter exactly as it was.
    let (status, read) = get(&router, name).await;
    assert_eq!(status, 200, "the parameter survives, got {status} {read}");
    assert_eq!(
        (
            read["Parameter"]["Value"].as_str(),
            read["Parameter"]["Version"].as_i64()
        ),
        (Some("1"), Some(1)),
        "expected the refused overwrite to change nothing, got {read}"
    );

    // ...and an update carrying a value the data type admits is served.
    let (status, written) = call(
        &router,
        "PutParameter",
        &json!({ "Name": name, "Value": "-2", "Overwrite": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "an update carrying a whole number must succeed, got {status} {written}"
    );
    assert_eq!(
        written["Version"].as_i64(),
        Some(2),
        "the update reports the version it created, got {written}"
    );
    let (status, read) = get(&router, name).await;
    assert_eq!(status, 200, "the update is readable, got {status} {read}");
    assert_eq!(
        (
            read["Parameter"]["Value"].as_str(),
            read["Parameter"]["DataType"].as_str()
        ),
        (Some("-2"), Some(INTEGER)),
        "expected the updated value under the inherited data type, got {read}"
    );
}
