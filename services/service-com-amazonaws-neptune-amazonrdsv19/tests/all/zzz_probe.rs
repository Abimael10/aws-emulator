// SPDX-License-Identifier: CC0-1.0

//! The MODEL-WIDE probe of how this front door reads a boolean form value: every boolean
//! member of every operation of this service, derived from the Smithy model rather than from
//! a hand-written list, so a member this emulator never thought about is covered too.
//!
//! The rule under test is the one `boolean_form_values` states over a hand-written table —
//! a boolean member's value on this wire is one of the spellings in the lexical space of an
//! XML Schema boolean (`true`, `false`, `1`, `0`; W3C XML Schema Part 2, §3.2.2), and text in
//! none of those spellings is refused with HTTP 400, the Query protocol's code for a body it
//! could not read and one fixed sentence. Because the rule is the PROTOCOL's and not any
//! handler's, it must hold for every operation that carries such a member, whether or not
//! that operation is implemented: the refusal happens while the request is read, ahead of the
//! handler.
//!
//! What the model supplies: the operations bound to the service shape, each operation's input
//! shape, which of that shape's members target a boolean shape, the key each member is
//! carried under (its `@xmlName`, else its name), and which members the shape requires — the
//! last so a request can carry them and the probe measures the boolean rather than an absent
//! member. An input that requires a member no probe value can be written for (one targeting
//! anything but a string) is reported rather than measured; the count below is what keeps
//! this suite from silently measuring nothing.
//!
//! It runs beside the hand-written suites rather than in place of them: those state the rule
//! case by case, including the one case the model cannot give this probe a key for — a boolean
//! nested inside an element of a list of structures — and this one states it over the whole
//! model. The name keeps it last in the declaration list; nothing here depends on another
//! suite having run.

use std::collections::BTreeMap;

use crate::db_cluster_lifecycle::Neptune;

/// The error code the Query protocol answers a request whose body it could not read with.
const MALFORMED_INPUT: &str = "MalformedInput";

/// The whole sentence that refusal carries, read from the module that owns the rule.
const NON_BOOLEAN_VALUE_MESSAGE: &str =
    service_common::query_boolean_values::NON_BOOLEAN_VALUE_MESSAGE;

/// A value in no spelling of the lexical space, for the refusal half of the probe.
const NOT_A_BOOLEAN: &str = "yes";

/// The value every required string member is given, so the request is refused for the boolean
/// it carries rather than for a member it left out. Any constraint the model declares on such
/// a member is judged AFTER the request is read, so the value only has to be a string.
const REQUIRED_STRING_VALUE: &str = "emu-boolean-probe";

/// The fewest boolean members this probe must reach, so a derivation that silently yields
/// nothing fails instead of passing. The model carries many more; this is a floor, not a
/// count.
const AT_LEAST: usize = 20;

/// This service's Smithy model, parsed — the one reading of it in this suite.
fn model_shapes() -> serde_json::Map<String, serde_json::Value> {
    let model_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-neptune-amazonrdsv19.json"
    );
    let model: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(model_path).expect("the service's Smithy model must be readable"),
    )
    .expect("the Smithy model must parse as JSON");
    model["shapes"]
        .as_object()
        .expect("the model must carry a shapes object")
        .clone()
}

/// The operations bound to the service shape, as shape ids.
fn bound_operations(shapes: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    shapes
        .iter()
        .filter(|(_, shape)| shape["type"] == "service")
        .flat_map(|(_, shape)| {
            shape["operations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
        .filter_map(|operation| operation["target"].as_str().map(str::to_string))
        .collect()
}

/// The key a member is carried under on this wire: its `@xmlName`, else its name (awsQuery
/// takes either verbatim).
fn member_key(name: &str, member: &serde_json::Value) -> String {
    member["traits"]["smithy.api#xmlName"]
        .as_str()
        .unwrap_or(name)
        .to_string()
}

/// The type of the shape a member targets, as the model names it.
fn target_type<'m>(
    shapes: &'m serde_json::Map<String, serde_json::Value>,
    member: &serde_json::Value,
) -> &'m str {
    member["target"]
        .as_str()
        .and_then(|target| shapes.get(target))
        .and_then(|shape| shape["type"].as_str())
        .unwrap_or_default()
}

/// What one operation's input offers this probe: the keys of its boolean members, the request
/// its required members have to carry, and whether a required member could not be written for.
struct InputShape {
    boolean_keys: Vec<String>,
    required: Vec<(String, String)>,
    unwritable_required: bool,
}

/// Read one operation's input shape.
fn input_shape(
    shapes: &serde_json::Map<String, serde_json::Value>,
    operation: &serde_json::Value,
) -> InputShape {
    let mut shape = InputShape {
        boolean_keys: Vec::new(),
        required: Vec::new(),
        unwritable_required: false,
    };
    let Some(input) = operation["input"]["target"]
        .as_str()
        .and_then(|target| shapes.get(target))
    else {
        return shape;
    };
    let Some(members) = input["members"].as_object() else {
        return shape;
    };
    for (name, member) in members {
        let key = member_key(name, member);
        let kind = target_type(shapes, member);
        if kind == "boolean" {
            shape.boolean_keys.push(key.clone());
        }
        if member["traits"]["smithy.api#required"].is_null() {
            continue;
        }
        match kind {
            // A boolean the shape REQUIRES is one of the members under test, so it is left to
            // the probe to fill with the spelling that case is about.
            "boolean" => {}
            "string" | "enum" => shape
                .required
                .push((key, REQUIRED_STRING_VALUE.to_string())),
            _ => shape.unwritable_required = true,
        }
    }
    shape
}

/// The probe's view of the model: every operation that carries a boolean member, with the
/// request each one needs, and the operations whose required members no probe value fits.
fn boolean_members() -> (BTreeMap<String, InputShape>, Vec<String>) {
    let shapes = model_shapes();
    let mut measured: BTreeMap<String, InputShape> = BTreeMap::new();
    let mut skipped: Vec<String> = Vec::new();
    for id in bound_operations(&shapes) {
        let Some(operation) = shapes.get(&id) else {
            continue;
        };
        let name = id.split('#').next_back().unwrap_or(&id).to_string();
        let shape = input_shape(&shapes, operation);
        if shape.boolean_keys.is_empty() {
            continue;
        }
        if shape.unwritable_required {
            skipped.push(name);
            continue;
        }
        measured.insert(name, shape);
    }
    (measured, skipped)
}

#[tokio::test]
async fn every_boolean_member_the_model_declares_is_read_by_the_one_rule() {
    let neptune = Neptune::new();
    let (measured, skipped) = boolean_members();
    let mut probed = 0usize;

    for (operation, shape) in &measured {
        for key in &shape.boolean_keys {
            // The refusal half: text in no spelling of the lexical space, on an operation
            // whose required members the request carries, so what is measured is the boolean.
            for spelling in [NOT_A_BOOLEAN, "TRUE", "2", ""] {
                let mut params: Vec<(&str, &str)> = shape
                    .required
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str()))
                    .collect();
                params.push((key.as_str(), spelling));
                let reply = neptune.call(operation, &params).await;
                let case = format!("{operation}${key} = {spelling:?}");
                assert_eq!(
                    reply.status, 400,
                    "case {case:?}: expected HTTP 400, got {} with body {}",
                    reply.status, reply.body
                );
                assert_eq!(
                    reply.error_code(),
                    MALFORMED_INPUT,
                    "case {case:?}: expected the code {MALFORMED_INPUT:?}, got body {}",
                    reply.body
                );
                assert_eq!(
                    reply.error_message(),
                    NON_BOOLEAN_VALUE_MESSAGE,
                    "case {case:?}: expected {NON_BOOLEAN_VALUE_MESSAGE:?}, got {:?}",
                    reply.error_message()
                );
            }

            // The reading half: every spelling of the space is READ, so the request goes on
            // to be served or refused for something else — never for the value's spelling.
            // What the operation then answers is its own business (an implemented one serves
            // or raises a modelled fault; one still scaffolded answers the 501), so only the
            // one sentence is ruled out here.
            for spelling in ["true", "false", "1", "0"] {
                let mut params: Vec<(&str, &str)> = shape
                    .required
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str()))
                    .collect();
                params.push((key.as_str(), spelling));
                let reply = neptune.call(operation, &params).await;
                let case = format!("{operation}${key} = {spelling:?}");
                assert_ne!(
                    reply.error_message(),
                    NON_BOOLEAN_VALUE_MESSAGE,
                    "case {case:?}: a spelling of the lexical space must be read, got body {}",
                    reply.body
                );
            }
            probed += 1;
        }
    }

    assert!(
        probed >= AT_LEAST,
        "expected at least {AT_LEAST} boolean members derived from the model, probed {probed} \
         (operations whose required members no probe value fits: {skipped:?})"
    );
}
