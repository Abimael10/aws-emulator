// SPDX-License-Identifier: CC0-1.0

//! What a refusal of a value a BOOLEAN parameter cannot carry depends on, and what it does
//! NOT depend on.
//!
//! The rule: a `ParameterValue` outside what a parameter's published `DataType` can carry is
//! refused with HTTP 400 `InvalidParameterValue` worded about that TYPE — for a boolean
//! parameter the fixed text "Invalid boolean value: " followed by the `ParameterValue` the
//! request carried, naming neither the parameter nor its `AllowedValues` — and nothing of the
//! request is applied. The parameter's own published type is the whole of what decides that
//! wording, so the SHAPE of the request cannot change it: the `ApplyMethod` the entry carries
//! (including the one the parameter's apply type forbids, which is a refusal of its own that a
//! value this parameter cannot carry never reaches), where the entry sits among the other
//! parameters of a batch, and which `DBParameterGroupFamily` the group belongs to all leave
//! the status, the code and the text alone.
//!
//! The sibling suite `boolean_parameter_value` spans the parameters and the values; this one
//! spans the REQUEST around them. Its rows are the cluster engines this front door publishes
//! a cluster parameter catalog with a boolean parameter for, each at the family the
//! engine-version catalogue publishes as that engine's DEFAULT — resolved here the way a
//! client resolves it, through `DescribeDBEngineVersions(DefaultOnly=true)`, so no family or
//! parameter name is written out below. Amazon DocumentDB is not a row: its published cluster
//! vocabulary has no boolean parameter, so it has no case to make.

use service_com_amazonaws_rds_amazonrdsv19::state::{DATA_TYPE_BOOLEAN, PARAMETER_SOURCE_USER};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_parameter_groups::{create_group, modify};
use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};

/// The error code Amazon RDS answers a `ParameterValue` it cannot serve with: the query
/// protocol's common client error, which this operation's own modelled error list does not
/// carry.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The cluster engines whose published cluster parameter vocabulary carries a boolean
/// parameter — the MySQL- and PostgreSQL-compatible cluster engines and Amazon Neptune, each
/// named as `DescribeDBEngineVersions$Engine` takes it. The family, the parameter and its
/// domain are all read off the wire per row rather than written here.
const ENGINES: [&str; 5] = [
    "aurora-postgresql",
    "aurora-mysql",
    "postgres",
    "mysql",
    "neptune",
];

/// The values no boolean parameter can carry, spanning the ways a value fails that type: a
/// word of no type at all, an integer past the two a switch has, a negative integer, and a
/// spelling another engine's own boolean parser takes.
const REFUSED_VALUES: [&str; 4] = ["not-a-boolean-xyz", "3", "-2", "yes"];

/// The `ApplyMethod` shapes one entry can take: absent, and each of the two the model's enum
/// publishes. `immediate` is the one a STATIC parameter cannot be changed under — a refusal
/// of its own — which is why it is a row: a value the parameter's type cannot carry is
/// refused about that type whatever method was asked for.
const APPLY_METHODS: [(&str, Option<&str>); 3] = [
    ("no ApplyMethod at all", None),
    ("ApplyMethod=pending-reboot", Some("pending-reboot")),
    ("ApplyMethod=immediate", Some("immediate")),
];

/// One parameter as `DescribeDBClusterParameters` reports it: the published columns this
/// suite picks its subjects by.
#[derive(Debug, Clone)]
struct Reported {
    /// `Parameter$ParameterName`.
    name: String,
    /// `Parameter$DataType`.
    data_type: String,
    /// `Parameter$AllowedValues`, or the empty string where the family publishes none.
    allowed_values: String,
    /// `Parameter$IsModifiable`.
    is_modifiable: bool,
}

/// The `DBParameterGroupFamily` this service publishes for `engine`'s DEFAULT version, read
/// the way a client reads it.
async fn default_family(router: &MultiServiceRouter, engine: &str) -> String {
    let body = expect_ok(
        engine,
        call(
            router,
            "DescribeDBEngineVersions",
            &[("Engine", engine), ("DefaultOnly", "true")],
        )
        .await,
    );
    let entry = subtrees(&body, "DBEngineVersion")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!("case {engine:?}: expected a published default version, got {body}")
        });
    text(&entry, "DBParameterGroupFamily")
}

/// Every parameter `group` reports, paged to the end.
async fn reported_parameters(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<Reported> {
    let mut found: Vec<Reported> = Vec::new();
    let mut marker: Option<String> = None;
    // Bounded by the longest catalog this service publishes, at 100 records a page.
    for _ in 0..12 {
        let mut params: Vec<(&str, String)> = vec![
            ("DBClusterParameterGroupName", group.to_string()),
            ("MaxRecords", "100".to_string()),
        ];
        if let Some(marker) = &marker {
            params.push(("Marker", marker.clone()));
        }
        let page = expect_ok(
            case,
            call(router, "DescribeDBClusterParameters", &params).await,
        );
        for parameter in subtrees(&page, "Parameter") {
            found.push(Reported {
                name: text(&parameter, "ParameterName"),
                data_type: text(&parameter, "DataType"),
                allowed_values: optional_text(&parameter, "AllowedValues").unwrap_or_default(),
                is_modifiable: text(&parameter, "IsModifiable") == "true",
            });
        }
        marker = optional_text(&page, "Marker");
        if marker.is_none() {
            break;
        }
    }
    assert!(
        marker.is_none(),
        "case {case:?}: the parameter report of {group} never finished paging"
    );
    found
}

/// The boolean parameters of `reported` a caller may set: a parameter the engine protects is
/// refused before its value is read at all, which is its own rule.
fn modifiable_booleans(reported: &[Reported]) -> Vec<&Reported> {
    reported
        .iter()
        .filter(|parameter| parameter.data_type == DATA_TYPE_BOOLEAN && parameter.is_modifiable)
        .collect()
}

/// A parameter of `reported` OTHER than `besides` whose published domain names a value it
/// admits, with that value: the companion entry a batch carries beside the refused one.
fn admitted_companion(reported: &[Reported], besides: &str) -> Option<(String, String)> {
    reported.iter().find_map(|parameter| {
        let admitted = parameter.allowed_values.split(',').next()?.trim();
        (parameter.is_modifiable
            && parameter.name != besides
            && parameter.allowed_values.contains(','))
        .then(|| (parameter.name.clone(), admitted.to_string()))
    })
}

/// The sentence Amazon RDS refuses a value a boolean parameter cannot carry with: its fixed
/// text, then the `ParameterValue` the request carried and nothing else.
fn expected_refusal(value: &str) -> String {
    format!("Invalid boolean value: {value}")
}

/// The value `group` reports as the CALLER's (`Source` `user`) for `name`, or `None` where it
/// carries none of its own for it.
async fn user_value(router: &MultiServiceRouter, group: &str, name: &str) -> Option<String> {
    let reported = expect_ok(
        name,
        call(
            router,
            "DescribeDBClusterParameters",
            &[
                ("DBClusterParameterGroupName", group),
                ("Source", PARAMETER_SOURCE_USER),
            ],
        )
        .await,
    );
    subtrees(&reported, "Parameter")
        .into_iter()
        .find(|parameter| text(parameter, "ParameterName") == name)
        .map(|parameter| text(&parameter, "ParameterValue"))
}

/// A value a boolean parameter cannot carry is refused in the same words under every
/// `ApplyMethod` an entry can name, at every engine's default family, and the group keeps
/// whatever it carried.
#[tokio::test]
async fn the_apply_method_does_not_change_how_a_boolean_value_is_refused() {
    let router = emulator();

    for (index, engine) in ENGINES.into_iter().enumerate() {
        let family = default_family(&router, engine).await;
        let case = format!("{engine} at its default family {family}");
        let group = create_group(&router, &case, &format!("emu-bool-shape-{index}"), &family).await;
        let reported = reported_parameters(&router, &case, &group).await;
        let booleans: Vec<Reported> = modifiable_booleans(&reported)
            .into_iter()
            .cloned()
            .collect();
        assert!(
            !booleans.is_empty(),
            "case {case:?}: expected the family to publish a modifiable boolean parameter, got \
             {reported:?}"
        );

        for parameter in &booleans {
            for value in REFUSED_VALUES {
                for (shape, apply_method) in APPLY_METHODS {
                    let case = format!("{case}: {} = {value:?} with {shape}", parameter.name);
                    let answered = expect_error(
                        &case,
                        modify(&router, &group, &parameter.name, value, apply_method).await,
                        400,
                        INVALID_PARAMETER_VALUE,
                    );
                    let message = text(&answered, "Message");
                    let expected = expected_refusal(value);
                    assert_eq!(
                        message, expected,
                        "case {case:?}: expected {expected:?}, got {message:?}"
                    );
                }
            }
            // None of those refusals wrote anything, so the group still carries no value of
            // its own for the parameter.
            let stored = user_value(&router, &group, &parameter.name).await;
            assert_eq!(
                stored, None,
                "case {case:?}: a refused value must leave {} unset, got {stored:?}",
                parameter.name
            );

            // A value the type DOES carry is stored and read back...
            let admitted = parameter
                .allowed_values
                .split(',')
                .next_back()
                .unwrap_or_default()
                .trim()
                .to_string();
            let case = format!("{case}: {} = {admitted:?}", parameter.name);
            expect_ok(
                &case,
                modify(
                    &router,
                    &group,
                    &parameter.name,
                    &admitted,
                    Some("pending-reboot"),
                )
                .await,
            );
            let stored = user_value(&router, &group, &parameter.name).await;
            assert_eq!(
                stored.as_deref(),
                Some(admitted.as_str()),
                "case {case:?}: expected the group to report the stored value, got {stored:?}"
            );

            // ...and a later refusal leaves that stored value exactly where it was, whatever
            // method it asked for.
            for (shape, apply_method) in APPLY_METHODS {
                let value = REFUSED_VALUES[0];
                let case = format!("{case}: {} = {value:?} with {shape}", parameter.name);
                let answered = expect_error(
                    &case,
                    modify(&router, &group, &parameter.name, value, apply_method).await,
                    400,
                    INVALID_PARAMETER_VALUE,
                );
                let message = text(&answered, "Message");
                let expected = expected_refusal(value);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                let stored = user_value(&router, &group, &parameter.name).await;
                assert_eq!(
                    stored.as_deref(),
                    Some(admitted.as_str()),
                    "case {case:?}: a refused value must leave the group as it was, got \
                     {stored:?}"
                );
            }
        }
    }
}

/// A boolean value the type cannot carry is refused in the same words wherever it sits in a
/// batch, and the whole batch is left unapplied; the same batch with that value brought
/// inside the type applies every entry of it.
#[tokio::test]
async fn a_refused_boolean_value_anywhere_in_a_batch_applies_none_of_it() {
    let router = emulator();

    for (index, engine) in ENGINES.into_iter().enumerate() {
        let family = default_family(&router, engine).await;
        let case = format!("{engine} at its default family {family}");
        let group = create_group(&router, &case, &format!("emu-bool-batch-{index}"), &family).await;
        let reported = reported_parameters(&router, &case, &group).await;
        let booleans = modifiable_booleans(&reported);
        let Some(boolean) = booleans.first() else {
            panic!("case {case:?}: expected a modifiable boolean parameter, got {reported:?}");
        };
        let Some((companion, companion_value)) = admitted_companion(&reported, &boolean.name)
        else {
            panic!(
                "case {case:?}: expected a second modifiable parameter with a published \
                 domain, got {reported:?}"
            );
        };

        // The batch, with the boolean entry FIRST and with it SECOND: one request in two
        // orders, each carrying the same two parameters.
        let batch = |boolean_value: &str, boolean_first: bool| {
            let boolean_entry = (boolean.name.clone(), boolean_value.to_string());
            let companion_entry = (companion.clone(), companion_value.clone());
            let (first, second) = if boolean_first {
                (boolean_entry, companion_entry)
            } else {
                (companion_entry, boolean_entry)
            };
            vec![
                ("DBClusterParameterGroupName".to_string(), group.clone()),
                ("Parameters.Parameter.1.ParameterName".to_string(), first.0),
                ("Parameters.Parameter.1.ParameterValue".to_string(), first.1),
                ("Parameters.Parameter.2.ParameterName".to_string(), second.0),
                ("Parameters.Parameter.2.ParameterValue".to_string(), second.1),
            ]
        };

        for boolean_first in [true, false] {
            for value in REFUSED_VALUES {
                let position = if boolean_first { "first" } else { "second" };
                let case =
                    format!("{case}: {} = {value:?} {position} in the batch", boolean.name);
                let answered = expect_error(
                    &case,
                    call(
                        &router,
                        "ModifyDBClusterParameterGroup",
                        &batch(value, boolean_first),
                    )
                    .await,
                    400,
                    INVALID_PARAMETER_VALUE,
                );
                let message = text(&answered, "Message");
                let expected = expected_refusal(value);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                for name in [boolean.name.as_str(), companion.as_str()] {
                    let stored = user_value(&router, &group, name).await;
                    assert_eq!(
                        stored, None,
                        "case {case:?}: the refused batch must have applied nothing, yet \
                         {name} reports {stored:?}"
                    );
                }
            }
        }

        // The same batch, with the boolean value one the type carries, applies both entries.
        let admitted = boolean
            .allowed_values
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        expect_ok(
            &case,
            call(
                &router,
                "ModifyDBClusterParameterGroup",
                &batch(&admitted, true),
            )
            .await,
        );
        for (name, expected) in [
            (boolean.name.as_str(), admitted.as_str()),
            (companion.as_str(), companion_value.as_str()),
        ] {
            let stored = user_value(&router, &group, name).await;
            assert_eq!(
                stored.as_deref(),
                Some(expected),
                "case {case:?}: expected {name} to report {expected:?}, got {stored:?}"
            );
        }
    }
}
