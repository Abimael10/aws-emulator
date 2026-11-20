// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal of a PartiQL transaction carrying more statements than
//! the model admits.
//!
//! One rule, over a TABLE of inputs rather than one recorded request:
//!
//! > a list member past the `@length` maximum the model declares for it is refused HTTP 400
//! > `ValidationException` whose message is the `<N> validation error(s) detected: `
//! > envelope over a clause that QUOTES THE REJECTED LIST — in the object form this
//! > service's request validator renders a structure in, every value of it taken from the
//! > request as the request sent it — names the member by the path that validator builds,
//! > and states the bound that was broken. Nothing the emulator computed about the request
//! > reaches the clause: not the element count, not a summary of the collection.
//!
//! `ExecuteTransaction$TransactStatements` is `@length{min:1,max:100}`
//! (`smithy_query(query="operation", operation="ExecuteTransaction")`), and the members of
//! one element are the model's own, in its order (`smithy_query(query="shape",
//! shape="com.amazonaws.dynamodb#ParameterizedStatement")` gives `Statement Parameters
//! ReturnValuesOnConditionCheckFailure`), so a member the request left out is quoted as the
//! unset field it is.
//!
//! The rows differ along what the rule generalises over: how far past the bound the list
//! went (one past it, and half again as many), which members each element states, the
//! datatype of a parameter it carries, and the bound itself — the list AT the maximum is
//! not refused as a constraint violation at all, and the list below the member's MINIMUM
//! keeps the clause that echoes no value.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message};

/// The member path the request validator reports this list under.
const MEMBER_PATH: &str = "transactStatements";

/// The `@length` maximum the model declares for it.
const CAP: usize = 100;

/// The fields the service's own attribute-value object states, in the order it states
/// them, as the recorded refusals of a quoted list render them — six of them belong to no
/// member this model declares (`smithy_query(query="shape",
/// shape="com.amazonaws.dynamodb#AttributeValue")` gives `S N B SS NS BS M L NULL BOOL`),
/// and they are part of the frame all the same. Written out here so this suite pins the
/// rendering rather than the crate's own copy of it.
const CORAL_FIELDS: [&str; 16] = [
    "s", "n", "b", "iNT", "dECIMAL", "dOUBLE", "sS", "nS", "bS", "iNTSET", "dECIMALSET",
    "dOUBLESET", "m", "l", "nULL", "bOOL",
];

/// One attribute value in that rendering: every field of the frame, with `field` carrying
/// `value` and every other one unset.
fn coral_value(field: &str, value: &str) -> String {
    let fields: Vec<String> = CORAL_FIELDS
        .iter()
        .map(|declared| {
            if *declared == field {
                format!("{declared}={value}")
            } else {
                format!("{declared}=null")
            }
        })
        .collect();
    format!("AttributeValue({})", fields.join(", "))
}

/// One element as the quoted list renders it: the statement the request sent, the
/// parameters it listed and the return-value member it stated, each unset where the
/// request left it out.
fn coral_statement(statement: &str, parameters: Option<&[String]>, returns: Option<&str>) -> String {
    let rendered_parameters = match parameters {
        Some(values) => format!("[{}]", values.join(", ")),
        None => "null".to_string(),
    };
    format!(
        "ParameterizedStatement(statement={statement}, parameters={rendered_parameters}, \
         returnValuesOnConditionCheckFailure={})",
        returns.unwrap_or("null")
    )
}

/// The whole message a list past its cap earns: the envelope over one clause that quotes
/// the list, names the member by the validator's path and states the bound.
fn quoted_cap_message(elements: &[String]) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at '{MEMBER_PATH}' failed to satisfy \
         constraint: Member must have length less than or equal to {CAP}",
        elements.join(", ")
    )
}

/// The statement text element `which` carries.
fn statement_text(which: usize) -> String {
    format!("INSERT INTO \"Ledger\" VALUE {{'pk': 'k{which}'}}")
}

/// `count` statements, each a distinct insert, with the members `extra` adds to every one
/// of them.
fn statements(count: usize, extra: &Value) -> Value {
    Value::Array(
        (0..count)
            .map(|which| {
                let mut element = json!({"Statement": statement_text(which)});
                for (member, value) in extra.as_object().cloned().unwrap_or_default() {
                    element[member] = value;
                }
                element
            })
            .collect(),
    )
}

/// One row of the table below: how far past the bound the list goes, the members every
/// element states beside its statement, and how one element renders in the quoted list.
struct CapCase {
    case: &'static str,
    count: usize,
    extra: Value,
    rendered: fn(usize) -> String,
}

#[tokio::test]
async fn a_statement_list_past_its_cap_is_quoted_back_at_the_validator_s_member_path() {
    let router = emulator();

    let rows: Vec<CapCase> = vec![
        CapCase {
            case: "a transaction one past the cap",
            count: CAP + 1,
            extra: json!({}),
            rendered: |which| coral_statement(&statement_text(which), None, None),
        },
        CapCase {
            case: "a transaction half again past the cap",
            count: CAP + CAP / 2,
            extra: json!({}),
            rendered: |which| coral_statement(&statement_text(which), None, None),
        },
        CapCase {
            case: "a transaction past the cap whose elements carry a string parameter",
            count: CAP + 1,
            extra: json!({"Parameters": [{"S": "held"}]}),
            rendered: |which| {
                coral_statement(
                    &statement_text(which),
                    Some(&[coral_value("s", "held")]),
                    None,
                )
            },
        },
        CapCase {
            case: "a transaction past the cap whose elements carry a number parameter and a \
                   return-value member",
            count: CAP + 1,
            extra: json!({
                "Parameters": [{"N": "7"}],
                "ReturnValuesOnConditionCheckFailure": "ALL_OLD",
            }),
            rendered: |which| {
                coral_statement(
                    &statement_text(which),
                    Some(&[coral_value("n", "7")]),
                    Some("ALL_OLD"),
                )
            },
        },
    ];

    for row in rows {
        let CapCase {
            case,
            count,
            extra,
            rendered,
        } = row;
        let (status, body) = call(
            &router,
            "ExecuteTransaction",
            json!({"TransactStatements": statements(count, &extra)}),
        )
        .await;
        let (code, stated) = (error_code(&body), message(&body));
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} {code} with {stated:?}"
        );
        let expected = quoted_cap_message(&(0..count).map(rendered).collect::<Vec<String>>());
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the rejected list quoted back, got {stated:?}"
        );
        // Nothing the emulator computed about the request may reach a caller through the
        // clause — the walk's own summary of the collection above all.
        for leaked in [" items", " entries", "emulator", "ExecuteTransaction"] {
            assert!(
                !stated.contains(leaked),
                "case {case:?}: the clause must not state {leaked:?}, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_statement_list_at_or_below_its_bounds_is_not_the_quoted_cap_refusal() {
    let router = emulator();

    // AT the maximum the list is not refused as a constraint violation at all, which is
    // what makes the rows above about the bound and not an off-by-one.
    let (status, body) = call(
        &router,
        "ExecuteTransaction",
        json!({"TransactStatements": statements(CAP, &json!({}))}),
    )
    .await;
    let stated = message(&body);
    assert!(
        !stated.contains("failed to satisfy constraint"),
        "case a list AT the cap: expected no constraint clause, got {status} with {stated:?}"
    );

    // One below the member's MINIMUM is the empty list, whose clause echoes the empty list
    // and names the bound it broke: the quoting rule is the MAXIMUM's, so the two refusals
    // are pinned side by side.
    let (status, body) = call(
        &router,
        "ExecuteTransaction",
        json!({"TransactStatements": []}),
    )
    .await;
    let (code, stated) = (error_code(&body), message(&body));
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case an empty transaction: expected 400 ValidationException, got {status} {code} with \
         {stated:?}"
    );
    assert_eq!(
        stated,
        format!(
            "1 validation error detected: Value '[]' at '{MEMBER_PATH}' failed to satisfy \
             constraint: Member must have length greater than or equal to 1"
        ),
        "case an empty transaction: expected the minimum's own clause, got {stated:?}"
    );
}
