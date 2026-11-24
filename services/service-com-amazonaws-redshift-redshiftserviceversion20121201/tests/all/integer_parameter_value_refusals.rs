// SPDX-License-Identifier: CC0-1.0

//! What `ModifyClusterParameterGroup` answers when a value does not have the
//! FORM its parameter's `Parameter$DataType` fixes, end to end over the awsQuery
//! wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The RULE this suite pins: a parameter value the published set does not admit
//! is refused with HTTP 400 and the Query API's `InvalidParameterValue`, worded
//! with the fixed text of the VALUE CLASS it falls in and nothing else — one
//! fixed text per class rather than one template with the data type substituted
//! into it. The classes this validation path has are the form of a whole number
//! (`invalid integer value:` + the value the request sent), the form of a switch
//! (`parameter value is not a valid boolean:` + the value), the form of the
//! workload-management document (`Invalid JSON for ` + the parameter) and the
//! MAGNITUDE of a whole number the published range does not contain (the
//! parameter, the value and the advertised set). The variable part of each is
//! taken from the request and from the emulator's own catalog, so the integer
//! class echoes the rejected value and names neither the parameter, nor the
//! value's position in the query serialization, nor the domain it missed.
//!
//! The rows differ along what the rule generalises over: WHICH integer parameter
//! of the published catalog is set (every one of them, read off the catalog
//! rather than listed here) and WHAT form the rejected value takes — a word, a
//! fraction, another notation for a number, digits carrying a separator or a
//! unit, a boolean literal, a bare sign. A refused request is also a read-only
//! request: each case asserts the group reports exactly what it reported before.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};
use crate::cluster_parameters::create_group;
use crate::modify_cluster_parameter_group::{modify, reported};

/// The fixed text of the integer class: AWS's own words for a value its integer
/// parser could not read, with no space after the colon and no trailing period.
const INTEGER_FORM_REFUSAL: &str = "invalid integer value:";

/// The head of the switch class's own fixed text (`parameter value is not a
/// valid boolean:<value>`), which the same validation path words differently —
/// the two classes share no template, so the integer class must not carry it.
const SWITCH_CLASS_HEAD: &str = "parameter value is not a valid";

/// The awsQuery code every value-domain refusal of this path carries.
const VALUE_REFUSAL_CODE: &str = "InvalidParameterValue";

/// One row of the batch table: the case, the name-value pairs one request
/// carries and the whole sentence the refusal must carry.
type BatchRefusal<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);

/// Assert the status, awsQuery `<Code>` and whole `<Message>` of one refusal.
fn assert_refusal(case: &str, answer: &Answer, status: u16, code: &str, message: &str) {
    assert_eq!(
        (answer.status, answer.error_code(), answer.text("Message")),
        (status, Some(code.to_string()), Some(message.to_string())),
        "case {case}: expected {status} {code} {message:?}, got {} / {:?} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.text("Message"),
        answer.body
    );
}

/// Every parameter the published catalog types as a whole number, read off the
/// same catalog the handlers do rather than listed here, so a parameter added to
/// it is covered without editing this suite.
fn integer_parameters() -> Vec<&'static state::EngineParameter> {
    state::ENGINE_DEFAULT_PARAMETERS
        .iter()
        .filter(|parameter| parameter.data_type == state::DATA_TYPE_INTEGER)
        .collect()
}

/// A value that is no whole number, refused on its FORM in the words of the
/// engine's integer parser — for EVERY integer parameter of the published
/// catalog, and whatever shape the value has.
#[tokio::test]
async fn a_value_that_is_no_whole_number_is_refused_in_the_integer_classs_own_words() {
    let emulator = Emulator::new();
    const GROUP: &str = "integer-form-refusal";
    create_group(&emulator, GROUP).await;

    // Forms a value can take that is not a whole number. Each row is one FORM,
    // not one parameter: it is sent to every integer parameter below.
    let forms = [
        ("a word", "soon"),
        ("another word", "many"),
        ("a decimal fraction", "1.5"),
        ("a negative fraction", "-2.5"),
        ("hexadecimal notation", "0x10"),
        ("exponent notation", "1e3"),
        ("digits with a thousands separator", "2,147"),
        ("digits separated by a space", "1 000"),
        ("digits carrying a unit", "100ms"),
        ("a boolean literal", "true"),
        ("a bare sign", "-"),
    ];

    let parameters = integer_parameters();
    assert!(
        !parameters.is_empty(),
        "case the catalog: expected the published catalog to type some parameter as an integer, \
         got none"
    );

    for parameter in &parameters {
        for (form, value) in &forms {
            let case = format!("{form} for {}", parameter.name);
            let before = reported(&emulator, GROUP).await;
            let answer = modify(&emulator, GROUP, &[(parameter.name, *value)]).await;

            assert_refusal(
                &case,
                &answer,
                400,
                VALUE_REFUSAL_CODE,
                &format!("{INTEGER_FORM_REFUSAL}{value}"),
            );

            let message = answer.text("Message").unwrap_or_default();
            // The one variable part is the rejected value: the sentence names
            // neither the parameter, nor where the value sat in the request, nor
            // the domain the value missed.
            assert!(
                !message.contains(parameter.name),
                "case {case}: expected the integer class to name no parameter, got {message:?}"
            );
            assert!(
                !message.contains("Parameters.Parameter"),
                "case {case}: expected no wire location in the refusal, got {message:?}"
            );
            assert!(
                !message.contains("acceptable range"),
                "case {case}: expected the form class not to describe the domain, got {message:?}"
            );
            if let Some(allowed) = parameter.allowed_values {
                assert!(
                    !message.contains(allowed),
                    "case {case}: expected the form class not to quote the published set \
                     {allowed:?}, got {message:?}"
                );
            }
            // The integer class is its own fixed text, not the switch class's
            // with the data type substituted into it.
            assert!(
                !message.contains(SWITCH_CLASS_HEAD),
                "case {case}: expected the integer class not to reuse the switch template, \
                 got {message:?}"
            );

            // A refused modify is a read-only request: the group still reports
            // the family's engine default for every parameter.
            let after = reported(&emulator, GROUP).await;
            assert_eq!(
                after, before,
                "case {case}: expected the refused modify to change nothing the group reports"
            );
            let held = after
                .iter()
                .find(|(name, _, _)| name == parameter.name)
                .map(|(_, value, source)| (value.as_str(), source.as_str()));
            assert_eq!(
                held,
                Some((
                    parameter.default_value,
                    state::PARAMETER_SOURCE_ENGINE_DEFAULT
                )),
                "case {case}: expected the engine default to survive the refusal, got {held:?}"
            );
        }
    }
}

/// Each value CLASS of this validation path carries its OWN fixed text, no two
/// of them alike; a batch carrying any of them applies nothing; and a batch the
/// service takes is reported and read back.
#[tokio::test]
async fn each_value_class_is_worded_on_its_own_and_a_refused_batch_applies_nothing() {
    let emulator = Emulator::new();
    const GROUP: &str = "integer-class-siblings";
    create_group(&emulator, GROUP).await;

    // One row per class of this path: the value sent and the whole sentence it
    // is refused with. The texts are written out literally so a change to a
    // fixed text fails a test rather than passing one computed from the code
    // under test.
    let classes = [
        (
            "the form of a whole number",
            "statement_timeout",
            "soon",
            "invalid integer value:soon".to_string(),
        ),
        (
            "the form of a switch",
            "require_ssl",
            "maybe",
            "parameter value is not a valid boolean:maybe".to_string(),
        ),
        (
            "the form of the workload-management document",
            "wlm_json_configuration",
            "{not-valid-json",
            "Invalid JSON for wlm_json_configuration".to_string(),
        ),
        (
            "the magnitude of a whole number",
            "statement_timeout",
            "50",
            "The statement_timeout of 50 is outside of the acceptable range of 0,100-2147483647"
                .to_string(),
        ),
    ];

    let mut texts: Vec<String> = Vec::with_capacity(classes.len());
    for (case, parameter, value, message) in &classes {
        let answer = modify(&emulator, GROUP, &[(*parameter, *value)]).await;
        assert_refusal(case, &answer, 400, VALUE_REFUSAL_CODE, message);
        texts.push(message.clone());
    }
    // No class borrows another's wording: one fixed text per class.
    for (case, _, _, message) in &classes {
        let shared = texts.iter().filter(|other| *other == message).count();
        assert_eq!(
            shared, 1,
            "case {case}: expected this class's own fixed text, but {message:?} words another \
             class too"
        );
    }

    // A batch carrying one value of either integer class applies NEITHER of its
    // values, in both orders.
    let batches: [BatchRefusal; 4] = [
        (
            "a non-numeric value last",
            vec![("auto_mv", "false"), ("statement_timeout", "soon")],
            "invalid integer value:soon",
        ),
        (
            "a non-numeric value first",
            vec![("statement_timeout", "soon"), ("auto_mv", "false")],
            "invalid integer value:soon",
        ),
        (
            "an out-of-range value last",
            vec![("auto_mv", "false"), ("statement_timeout", "50")],
            "The statement_timeout of 50 is outside of the acceptable range of 0,100-2147483647",
        ),
        (
            "an out-of-range value first",
            vec![("statement_timeout", "50"), ("auto_mv", "false")],
            "The statement_timeout of 50 is outside of the acceptable range of 0,100-2147483647",
        ),
    ];
    for (case, sent, message) in &batches {
        let answer = modify(&emulator, GROUP, sent).await;
        assert_refusal(case, &answer, 400, VALUE_REFUSAL_CODE, message);

        let overridden: Vec<(String, String)> = reported(&emulator, GROUP)
            .await
            .into_iter()
            .filter(|(_, _, source)| source != state::PARAMETER_SOURCE_ENGINE_DEFAULT)
            .map(|(name, value, _)| (name, value))
            .collect();
        assert!(
            overridden.is_empty(),
            "case {case}: expected the refused batch to apply nothing, got {overridden:?}"
        );
    }

    // A batch every value of which the published sets admit IS applied: the
    // answer reports the group's status and both values are read back as the
    // caller's own.
    let accepted = modify(
        &emulator,
        GROUP,
        &[("statement_timeout", "120"), ("extra_float_digits", "2")],
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case a legal multi-parameter modify: expected 200, got {} with body {}",
        accepted.status, accepted.body
    );
    assert_eq!(
        accepted.text("ParameterGroupStatus").as_deref(),
        Some(state::PARAMETER_GROUP_STATUS_UPDATED),
        "case a legal multi-parameter modify: expected the documented status, got {:?}",
        accepted.text("ParameterGroupStatus")
    );
    let held: Vec<(String, String, String)> = reported(&emulator, GROUP)
        .await
        .into_iter()
        .filter(|(_, _, source)| source == state::PARAMETER_SOURCE_USER)
        .collect();
    assert_eq!(
        held,
        vec![
            (
                "extra_float_digits".to_string(),
                "2".to_string(),
                state::PARAMETER_SOURCE_USER.to_string()
            ),
            (
                "statement_timeout".to_string(),
                "120".to_string(),
                state::PARAMETER_SOURCE_USER.to_string()
            ),
        ],
        "case a legal multi-parameter modify: expected both values held as the caller's, \
         got {held:?}"
    );

    // Resetting everything puts the family's engine defaults back, so the group
    // a refusal could not change is also one a reset returns to its start.
    let reset = emulator
        .call(
            "ResetClusterParameterGroup",
            &[("ParameterGroupName", GROUP), ("ResetAllParameters", "true")],
        )
        .await;
    assert_eq!(
        reset.status, 200,
        "case reset everything: expected 200, got {} with body {}",
        reset.status, reset.body
    );
    let defaults: Vec<(String, String, String)> = state::ENGINE_DEFAULT_PARAMETERS
        .iter()
        .map(|parameter| {
            (
                parameter.name.to_string(),
                parameter.default_value.to_string(),
                state::PARAMETER_SOURCE_ENGINE_DEFAULT.to_string(),
            )
        })
        .collect();
    let after_reset = reported(&emulator, GROUP).await;
    assert_eq!(
        after_reset, defaults,
        "case reset everything: expected every parameter back at its engine default, \
         got {after_reset:?}"
    );
}
