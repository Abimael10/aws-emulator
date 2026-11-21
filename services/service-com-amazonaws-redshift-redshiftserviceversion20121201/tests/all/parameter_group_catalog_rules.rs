// SPDX-License-Identifier: CC0-1.0

//! The cluster-parameter-group rules this service answers a request's INPUT by,
//! end to end over the awsQuery wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The suite asserts RULES over tables of inputs rather than one recorded
//! request:
//!
//! * a request the model-level walk refused — a `@required` member it did not
//!   carry as much as a value outside a declared bound — is answered under the
//!   ONE input-refusal code this service names both with, over every operation of
//!   the family;
//! * a parameter whose published `DataType` is `boolean` is a SWITCH: it takes
//!   the published literals and the other pair of words a switch is set with,
//!   read without regard to case, and then HOLDS the published literal, while a
//!   value outside that domain is refused in the vocabulary of the parameter;
//! * the `Source` filter's domain is the three sources a reported
//!   `Parameter$Source` can name, and a value outside it is refused naming the
//!   value and the domain;
//! * the catalog's own metadata — `Description` and `ApplyType` — is the engine's
//!   published text and arrives unchanged through the unfiltered read and through
//!   every `Source`-filtered one;
//! * a value of the parameter whose value is a JSON DOCUMENT that is not one is
//!   refused with that class's one fixed sentence, and nothing is stored.
//!
//! The expected sentences are written out literally here, so a change to the
//! fixed text fails a test rather than passing one computed from the code under
//! test.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};
use crate::cluster_parameters::{create_group, default_family, reported_parameters};
use crate::modify_cluster_parameter_group::{modify, reported};

/// The error code this service names every refusal of a request's input with.
const INPUT_REFUSAL_CODE: &str = "ValidationError";

/// The error code it names a value it cannot honour with.
const VALUE_REFUSAL_CODE: &str = "InvalidParameterValue";

/// Assert the status and awsQuery `<Code>` of one refusal.
fn assert_refused(case: &str, answer: &Answer, status: u16, code: &str) {
    assert_eq!(
        (answer.status, answer.error_code()),
        (status, Some(code.to_string())),
        "case {case}: expected {status} {code}, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
}

/// Assert the whole `<Message>` of one refusal.
fn assert_message(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.text("Message")
    );
}

/// One row of the structural-refusal table: the operation, the members the
/// request DOES carry, and the member it leaves out.
struct AbsentMember {
    case: &'static str,
    action: &'static str,
    params: Vec<(&'static str, String)>,
    absent: &'static str,
}

/// A request the model-level walk refuses is answered under this service's one
/// input-refusal code, whichever layer of the walk found the fault and whichever
/// operation it was addressed to — and the message names the member at fault.
///
/// The rows differ along which member is absent and which operation the request
/// named, including operations the issue did not cite (the group delete and the
/// parameter read), so the code is the service's rather than one handler's.
#[tokio::test]
async fn an_input_the_walk_refuses_is_answered_under_one_code_over_every_operation() {
    let emulator = Emulator::new();
    let family = default_family();

    let rows = vec![
        AbsentMember {
            case: "a create carrying no Description",
            action: "CreateClusterParameterGroup",
            params: vec![
                ("ParameterGroupName", "structural-create".to_string()),
                ("ParameterGroupFamily", family.clone()),
            ],
            absent: "description",
        },
        AbsentMember {
            case: "a create carrying no ParameterGroupName",
            action: "CreateClusterParameterGroup",
            params: vec![
                ("ParameterGroupFamily", family.clone()),
                ("Description", "a structural case".to_string()),
            ],
            absent: "parameterGroupName",
        },
        AbsentMember {
            case: "a create carrying no ParameterGroupFamily",
            action: "CreateClusterParameterGroup",
            params: vec![
                ("ParameterGroupName", "structural-family".to_string()),
                ("Description", "a structural case".to_string()),
            ],
            absent: "parameterGroupFamily",
        },
        AbsentMember {
            case: "a create carrying nothing at all",
            action: "CreateClusterParameterGroup",
            params: Vec::new(),
            absent: "parameterGroupName",
        },
        AbsentMember {
            case: "a modify carrying no Parameters",
            action: "ModifyClusterParameterGroup",
            params: vec![("ParameterGroupName", "structural-modify".to_string())],
            absent: "parameters",
        },
        AbsentMember {
            case: "a modify carrying no ParameterGroupName",
            action: "ModifyClusterParameterGroup",
            params: vec![
                (
                    "Parameters.Parameter.1.ParameterName",
                    "require_ssl".to_string(),
                ),
                ("Parameters.Parameter.1.ParameterValue", "true".to_string()),
            ],
            absent: "parameterGroupName",
        },
        AbsentMember {
            case: "a group delete carrying no name",
            action: "DeleteClusterParameterGroup",
            params: Vec::new(),
            absent: "parameterGroupName",
        },
        AbsentMember {
            case: "a parameter read carrying no group",
            action: "DescribeClusterParameters",
            params: Vec::new(),
            absent: "parameterGroupName",
        },
    ];

    for row in &rows {
        let params: Vec<(&str, &str)> = row
            .params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let answer = emulator.call(row.action, &params).await;
        assert_refused(row.case, &answer, 400, INPUT_REFUSAL_CODE);
        // The walk's own clause, in AWS's envelope: it names the member the
        // request left out, so a caller can fix the request from the text alone.
        let message = answer.text("Message").unwrap_or_default();
        assert!(
            message.contains(row.absent),
            "case {}: expected the message to name {:?}, got {message:?}",
            row.case,
            row.absent
        );
    }

    // Nothing was created by any of them: a refused create persists nothing.
    let listed = emulator
        .call("DescribeClusterParameterGroups", &[])
        .await;
    for name in ["structural-create", "structural-family"] {
        assert!(
            !listed.body.contains(name),
            "case {name}: expected the refused create to persist nothing, got body {}",
            listed.body
        );
    }
}

/// One row of the switch-domain table: the value a request sets a boolean
/// parameter to, and the published literal the group then holds — or `None` for
/// a value the parameter's domain does not admit at all.
struct SwitchCase {
    case: &'static str,
    parameter: &'static str,
    value: &'static str,
    holds: Option<&'static str>,
}

/// A parameter whose published `DataType` is `boolean` takes the published
/// literals and the other pair of words a switch is set with, read without
/// regard to case, and HOLDS the published literal whichever word the request
/// used; a value outside that domain is refused in the vocabulary of the
/// parameter, with the group left as it was.
///
/// Source for the parameters and their sets: the `DataType` and `AllowedValues`
/// of `state::ENGINE_DEFAULT_PARAMETERS`, which is what
/// `smithy_query(query="doc", shape="Parameter", member="AllowedValues")` ("The
/// valid range of values for the parameter") publishes per parameter. The rows
/// differ along the WORD the request used and its case, and cover parameters the
/// issue did not cite (`auto_mv`, `use_fips_ssl`, `auto_analyze`).
#[tokio::test]
async fn a_boolean_parameter_takes_either_pair_of_switch_words_and_holds_the_literal() {
    let emulator = Emulator::new();
    const GROUP: &str = "switch-domain";
    create_group(&emulator, GROUP).await;

    let rows = [
        SwitchCase {
            case: "the published literal for true",
            parameter: "require_ssl",
            value: "true",
            holds: Some("true"),
        },
        SwitchCase {
            case: "the published literal for false",
            parameter: "require_ssl",
            value: "false",
            holds: Some("false"),
        },
        SwitchCase {
            case: "the published literal in upper case",
            parameter: "enable_user_activity_logging",
            value: "TRUE",
            holds: Some("true"),
        },
        SwitchCase {
            case: "the published literal in title case",
            parameter: "auto_mv",
            value: "False",
            holds: Some("false"),
        },
        SwitchCase {
            case: "the other word for true",
            parameter: "use_fips_ssl",
            value: "on",
            holds: Some("true"),
        },
        SwitchCase {
            case: "the other word for false",
            parameter: "auto_analyze",
            value: "off",
            holds: Some("false"),
        },
        SwitchCase {
            case: "the other word for true in upper case",
            parameter: "enable_case_sensitive_identifier",
            value: "ON",
            holds: Some("true"),
        },
        SwitchCase {
            case: "a word of assent is not one of the switch words",
            parameter: "require_ssl",
            value: "yes",
            holds: None,
        },
        SwitchCase {
            case: "a word of refusal is not one of the switch words",
            parameter: "auto_mv",
            value: "no",
            holds: None,
        },
        SwitchCase {
            case: "a numeral is not one of the switch words",
            parameter: "use_fips_ssl",
            value: "1",
            holds: None,
        },
        SwitchCase {
            case: "prose is no switch word at all",
            parameter: "auto_analyze",
            value: "maybe",
            holds: None,
        },
        SwitchCase {
            case: "a switch word with a space in it",
            parameter: "require_ssl",
            value: "o n",
            holds: None,
        },
    ];

    for row in &rows {
        let before = reported(&emulator, GROUP).await;
        let answer = modify(&emulator, GROUP, &[(row.parameter, row.value)]).await;
        let after = reported(&emulator, GROUP).await;

        let Some(holds) = row.holds else {
            assert_refused(row.case, &answer, 400, VALUE_REFUSAL_CODE);
            // The parameter's domain IS its data type, so the sentence names the
            // type and the value the request carried, and nothing else.
            assert_message(
                row.case,
                &answer,
                &format!("parameter value is not a valid boolean:{}", row.value),
            );
            assert_eq!(
                after, before,
                "case {}: expected the refused modify to change nothing the group reports",
                row.case
            );
            continue;
        };

        assert_eq!(
            answer.status, 200,
            "case {}: expected 200, got {} with body {}",
            row.case, answer.status, answer.body
        );
        assert_eq!(
            answer.text("ParameterGroupStatus").as_deref(),
            Some(state::PARAMETER_GROUP_STATUS_UPDATED),
            "case {}: expected the documented ParameterGroupStatus, got {:?}",
            row.case,
            answer.text("ParameterGroupStatus")
        );

        // ROUND TRIP: what the group holds is the published literal, with the
        // `Source` a value a caller set carries — never the word the request
        // happened to spell it with.
        let held = after
            .iter()
            .find(|(name, _, _)| name == row.parameter)
            .map(|(_, value, source)| (value.as_str(), source.as_str()));
        assert_eq!(
            held,
            Some((holds, state::PARAMETER_SOURCE_USER)),
            "case {}: expected the group to hold {holds:?} with Source user, got {held:?}",
            row.case
        );
    }
}

/// The `Source` filter's domain is the three sources a reported
/// `Parameter$Source` can name: each of them selects the parameters reported
/// under it, and a value outside the domain is refused naming the value and the
/// domain.
///
/// Source for the domain: the sources `Parameter$Source` reports
/// (`smithy_query(query="doc", shape="Parameter", member="Source")` — "such as
/// \"engine-default\" or \"user\"", which leaves the set open), plus the
/// service's own `system`, which recorded behaviour admits as a filter. The rows
/// differ along which source is named and, for the refusals, along HOW the value
/// misses the domain.
#[tokio::test]
async fn the_source_filter_domain_is_the_three_reported_sources() {
    let emulator = Emulator::new();
    const GROUP: &str = "source-domain";
    create_group(&emulator, GROUP).await;

    // Two parameters a caller set, so each source has something to tell apart.
    let set = [("require_ssl", "true"), ("statement_timeout", "60000")];
    let modified = modify(&emulator, GROUP, &set).await;
    assert_eq!(
        modified.status, 200,
        "case seed two overrides: expected 200, got {} with body {}",
        modified.status, modified.body
    );

    let held: Vec<&str> = state::ENGINE_DEFAULT_PARAMETERS
        .iter()
        .map(|parameter| parameter.name)
        .filter(|name| set.iter().any(|(held, _)| held == name))
        .collect();
    let untouched: Vec<&str> = state::ENGINE_DEFAULT_PARAMETERS
        .iter()
        .map(|parameter| parameter.name)
        .filter(|name| !set.iter().any(|(held, _)| held == name))
        .collect();

    for (case, source, expected) in [
        ("the caller's own source", state::PARAMETER_SOURCE_USER, held),
        (
            "the engine's published defaults",
            state::PARAMETER_SOURCE_ENGINE_DEFAULT,
            untouched,
        ),
        // A legal filter this emulator reports no parameter under: it selects
        // nothing rather than being refused.
        (
            "the service's own source",
            state::PARAMETER_SOURCE_SYSTEM,
            Vec::new(),
        ),
    ] {
        let answer = emulator
            .call(
                "DescribeClusterParameters",
                &[("ParameterGroupName", GROUP), ("Source", source)],
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected Source={source:?} to be served, got {} with body {}",
            answer.status, answer.body
        );
        let listed: Vec<String> = reported_parameters(&answer.body)
            .into_iter()
            .filter_map(|parameter| parameter.name)
            .collect();
        assert_eq!(
            listed, expected,
            "case {case}: expected Source={source:?} to select {expected:?}, got {listed:?}"
        );
    }

    // Rows differing along how the value misses the domain: a word of its own, a
    // source name of another front door, a published source with its hyphen
    // changed, and one in the wrong case.
    for (case, source) in [
        ("a word of its own", "custom"),
        ("the filter vocabulary of another front door", "customer"),
        ("a published source with an underscore", "engine_default"),
        ("a published source in the wrong case", "User"),
    ] {
        let answer = emulator
            .call(
                "DescribeClusterParameters",
                &[("ParameterGroupName", GROUP), ("Source", source)],
            )
            .await;
        assert_refused(case, &answer, 400, VALUE_REFUSAL_CODE);
        assert_message(
            case,
            &answer,
            &format!("Invalid Source: {source}. Allowed values are: engine-default, system, user"),
        );
    }
}

/// One row of the catalog-metadata table: the parameter, and the `Description`
/// and `ApplyType` the engine publishes for it.
struct CatalogEntry {
    parameter: &'static str,
    description: &'static str,
    apply_type: &'static str,
}

/// The `Description` and `ApplyType` a read reports are the engine's own
/// published text, and they are the SAME through the unfiltered read and through
/// every `Source`-filtered one — the metadata belongs to the catalog, not to a
/// read path.
///
/// Source: what the engine publishes for the family this service offers (`aws
/// redshift describe-default-cluster-parameters --parameter-group-family
/// redshift-1.0`), which `state::ENGINE_DEFAULT_PARAMETERS` carries verbatim —
/// `Parameter$Description` ("A description of the parameter") and
/// `Parameter$ApplyType` ("Specifies how to apply the WLM configuration
/// parameter", whose published values are `static` and `dynamic`). The literal
/// rows hold the entries the issue cited and one it did not (`require_ssl`), and
/// each is read at a DIFFERENT position of the two lists; the second half holds
/// every other entry of the catalog to its own published text, so no read path
/// can report metadata of its own.
#[tokio::test]
async fn the_catalogs_metadata_is_the_engines_own_in_every_read() {
    let emulator = Emulator::new();
    const GROUP: &str = "catalog-metadata";
    create_group(&emulator, GROUP).await;

    let rows = [
        CatalogEntry {
            parameter: "enable_user_activity_logging",
            description: "parameter for audit logging purpose",
            apply_type: "static",
        },
        CatalogEntry {
            parameter: "wlm_json_configuration",
            description: "wlm json configuration",
            apply_type: "dynamic",
        },
        CatalogEntry {
            parameter: "require_ssl",
            description: "require ssl for all databaseconnections",
            apply_type: "static",
        },
    ];

    // One parameter of the table is set, so the SAME entries are read once under
    // `user` and once under `engine-default`: the filter shortens the list and
    // moves every entry's position, and changes nothing about the metadata.
    let modified = modify(
        &emulator,
        GROUP,
        &[("enable_user_activity_logging", "true")],
    )
    .await;
    assert_eq!(
        modified.status, 200,
        "case seed one override: expected 200, got {} with body {}",
        modified.status, modified.body
    );

    for (case, source) in [
        ("the unfiltered read", None),
        ("the user-filtered read", Some(state::PARAMETER_SOURCE_USER)),
        (
            "the engine-default-filtered read",
            Some(state::PARAMETER_SOURCE_ENGINE_DEFAULT),
        ),
    ] {
        let mut params = vec![("ParameterGroupName", GROUP)];
        if let Some(source) = source {
            params.push(("Source", source));
        }
        let answer = emulator.call("DescribeClusterParameters", &params).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        let listed = reported_parameters(&answer.body);

        for row in &rows {
            let Some(reported) = listed
                .iter()
                .find(|parameter| parameter.name.as_deref() == Some(row.parameter))
            else {
                // A filtered read reports only the parameters of its own source,
                // so an entry it does not carry is nothing to assert about.
                continue;
            };
            assert_eq!(
                (
                    reported.description.as_deref(),
                    reported.apply_type.as_deref()
                ),
                (Some(row.description), Some(row.apply_type)),
                "case {case}/{}: expected ({:?}, {:?}), got {reported:?}",
                row.parameter,
                row.description,
                row.apply_type
            );
        }

        // And EVERY entry the read carries reports its own catalog entry's text,
        // not just the rows above: a read path reports the catalog, so one that
        // reworded an entry — or reported another entry's metadata at the shifted
        // position a filter leaves — is caught whichever parameter it was.
        for reported in &listed {
            let name = reported.name.clone().unwrap_or_default();
            let entry = state::ENGINE_DEFAULT_PARAMETERS
                .iter()
                .find(|entry| entry.name == name)
                .unwrap_or_else(|| panic!("case {case}: {name:?} is no entry of the catalog"));
            assert_eq!(
                (
                    reported.description.as_deref(),
                    reported.apply_type.as_deref()
                ),
                (Some(entry.description), Some(entry.apply_type)),
                "case {case}/{name}: expected the catalog's own ({:?}, {:?}), got {reported:?}",
                entry.description,
                entry.apply_type
            );
        }
    }
}

/// A value of the parameter whose value is a JSON DOCUMENT that is not one is
/// refused with that class's one fixed sentence — which names the parameter and
/// echoes nothing — and nothing is stored; a document that IS one is taken
/// verbatim.
///
/// Source: `smithy_query(query="doc", shape="Parameter",
/// member="ParameterValue")`, which calls out `wlm_json_configuration`'s value as
/// the workload management configuration (and is why it alone publishes a size).
/// The rows differ along HOW the value fails to be a list of queues.
#[tokio::test]
async fn a_workload_configuration_that_is_not_a_json_list_is_refused_with_one_sentence() {
    let emulator = Emulator::new();
    const GROUP: &str = "wlm-json";
    create_group(&emulator, GROUP).await;

    // A configuration the parameter takes, so the refusals below cannot be an
    // "everything is refused" artefact and there is a stored value to protect.
    const TAKEN: &str = "[{\"query_group\":[\"etl\"],\"query_concurrency\":5}]";
    let accepted = modify(
        &emulator,
        GROUP,
        &[(state::WLM_JSON_CONFIGURATION, TAKEN)],
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case a list of queues: expected 200, got {} with body {}",
        accepted.status, accepted.body
    );

    for (case, value) in [
        ("an unterminated list", "[{\"auto_wlm\":true"),
        ("a JSON object rather than a list", "{\"auto_wlm\":true}"),
        ("a bare JSON string", "\"auto\""),
        ("a bare JSON number", "5"),
        ("JSON null", "null"),
        ("prose", "auto"),
        ("the empty value", ""),
        ("a list with a trailing comma", "[{\"auto_wlm\":true},]"),
    ] {
        let answer = modify(
            &emulator,
            GROUP,
            &[(state::WLM_JSON_CONFIGURATION, value)],
        )
        .await;
        assert_refused(case, &answer, 400, VALUE_REFUSAL_CODE);
        assert_message(case, &answer, "Invalid JSON for wlm_json_configuration");

        // Nothing was stored: the group still holds the configuration it took.
        let held = reported(&emulator, GROUP)
            .await
            .into_iter()
            .find(|(name, _, _)| name == state::WLM_JSON_CONFIGURATION)
            .map(|(_, value, source)| (value, source));
        assert_eq!(
            held,
            Some((
                TAKEN.to_string(),
                state::PARAMETER_SOURCE_USER.to_string()
            )),
            "case {case}: expected the refused value not to be stored, got {held:?}"
        );
    }
}
