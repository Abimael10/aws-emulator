// SPDX-License-Identifier: CC0-1.0

//! The instance-level engine-default catalog as a CLIENT discovers it: what a caller learns
//! from `DescribeEngineDefaultParameters` alone, before it has created anything.
//!
//! This front door serves three control planes (`src/service.rs`): an Amazon RDS, an Amazon
//! DocumentDB and an Amazon Neptune SDK client all sign `rds`, so every one of their DB
//! parameter group families is read here.
//!
//! The rule under test: for a `DBParameterGroupFamily` this front door publishes, the report
//! answers 200 carrying one `EngineDefaults.Parameters.Parameter` per DB-INSTANCE-level
//! parameter the family's engine publishes a record for — each sourced `engine-default` and
//! carrying that record's `ParameterName`, `ParameterValue`, `Description`, `ApplyType`,
//! `DataType`, `AllowedValues` and `IsModifiable` — so a family of an engine with a published
//! catalog never reports an EMPTY list, and the record a client reads off the wire is the one
//! the service then configures a group of that family against.
//!
//! What separates these two tables from the sibling suites: every expectation here is taken
//! from the REPORT itself rather than from a catalog constant inside the crate, which is all a
//! real client has. A discovery client reads the list, picks a parameter, sets it and
//! pre-validates the next value against the domain the list published; the second table walks
//! exactly that path over the wire.
//!
//! Both tables span the class the rule generalises over — one family per engine parameter
//! vocabulary this door serves, the graph engine's two family-name shapes among them, plus a
//! family whose engine this emulator publishes no catalog for, which is served and honestly
//! empty — rather than the one family any recorded request used.
//!
//! The harness (`emulator`, `call`, `expect_ok`, `expect_error` and the element readers) is
//! the [`crate::db_instances`] suite's, the group create/modify/describe trio is
//! [`crate::db_parameters`]'s and the `<Parameter>` reader is
//! [`crate::engine_default_parameters`]'s, reused rather than copied.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    APPLY_TYPE_DYNAMIC, APPLY_TYPE_STATIC, PARAMETER_SOURCE_ENGINE_DEFAULT, PARAMETER_SOURCE_USER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, subtrees, text, REGION};
use crate::db_parameters::{create_group, describe, modify_in};
use crate::engine_default_parameters::{reported, Reported, DATA_TYPES};

/// The report under test.
const ACTION: &str = "DescribeEngineDefaultParameters";

/// The Query API code this front door answers a parameter value it cannot serve with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The two words `Parameter$IsModifiable` is reported as on this wire — a boolean member,
/// whose text a client branches on to decide whether it may set the parameter at all.
const BOOLEAN_WORDS: [&str; 2] = ["true", "false"];

/// One family row: (case, the `DBParameterGroupFamily`, whether its engine publishes an
/// instance-level parameter catalog at all).
///
/// The rows differ along what the rule generalises over: which engine the family names (one
/// per parameter vocabulary this door serves, the two sibling control planes' engines
/// included), the SHAPE of the family name (engine plus dotted version, engine plus a version
/// with no dot, a hyphen-introduced version, a versionless engine), and whether the engine has
/// a published catalog at all — the last column separates "served with a catalog" from "served
/// and honestly empty", which is the difference the instance-level report is about.
const FAMILIES: &[(&str, &str, bool)] = &[
    ("the graph engine's current family", "neptune1.4", true),
    ("the graph engine's first family, whose name carries no dot", "neptune1", true),
    ("the document database engine's family", "docdb5.0", true),
    ("a MySQL family", "mysql8.0", true),
    ("a MariaDB family, which shares the MySQL vocabulary", "mariadb10.11", true),
    ("a PostgreSQL-compatible cluster family", "aurora-postgresql16", true),
    ("an Oracle family, whose version is hyphen-introduced", "oracle-ee-19", true),
    ("a Microsoft SQL Server family", "sqlserver-ee-15.0", true),
    // A family the member's own value list publishes whose engine this emulator holds no
    // catalog for: served, and empty rather than filled with invented parameters.
    ("a versionless family whose engine publishes no catalog", "db2-ae", false),
];

/// Whether the published domain `allowed` admits `value`, or `None` when that domain cannot be
/// read off the published text.
///
/// A domain is published either as an enumeration of the values the parameter admits
/// (comma-separated) or as the `<low>-<high>` range `Parameter$AllowedValues` documents as
/// "the valid range of values for the parameter". Anything else — a single admitted word, a
/// bound that is not a number — is unreadable here, and so is a default published as the
/// FORMULA the managed engine computes it from (`{DBInstanceClassMemory/12582880}`), which is
/// no number at all; those answer `None` rather than a verdict this test cannot justify.
fn admits(allowed: &str, value: &str) -> Option<bool> {
    if allowed.contains(',') {
        return Some(allowed.split(',').any(|admitted| admitted == value));
    }
    let (low, high) = allowed.split_once('-')?;
    let low: f64 = low.trim().parse().ok()?;
    let high: f64 = high.trim().parse().ok()?;
    let value: f64 = value.parse().ok()?;
    Some((low..=high).contains(&value))
}

/// One step past the top of a published numeric range, or `None` where the domain is not such
/// a range: the value a client that read the catalog expects the service to refuse.
fn one_past_the_published_range(allowed: &str) -> Option<String> {
    if allowed.contains(',') {
        return None;
    }
    let (_, high) = allowed.split_once('-')?;
    let high: i64 = high.trim().parse().ok()?;
    high.checked_add(1).map(|past| past.to_string())
}

/// The instance-level engine defaults `family` publishes, asserted to answer 200.
async fn discovered(router: &MultiServiceRouter, case: &str, family: &str) -> Vec<Reported> {
    let body = expect_ok(
        case,
        call(router, ACTION, &[("DBParameterGroupFamily", family)]).await,
    );
    assert_eq!(
        text(&body, "DBParameterGroupFamily"),
        family,
        "case {case:?}: the report echoes the family the request named, got {body}"
    );
    reported(&body)
}

/// A family whose engine publishes a catalog reports that catalog, with every published
/// column of every entry; one whose engine this emulator holds no catalog for reports an
/// empty list.
#[tokio::test]
async fn every_published_family_publishes_its_instance_level_records() {
    let router = emulator();

    for (row, family, publishes) in FAMILIES {
        let case = format!("{row} ({family})");
        let catalog = discovered(&router, &case, family).await;
        assert_eq!(
            !catalog.is_empty(),
            *publishes,
            "case {case:?}: expected a published instance-level catalog == {publishes}, got {} \
             entries",
            catalog.len()
        );

        for parameter in &catalog {
            let name = parameter.name.clone();
            assert!(
                !name.is_empty(),
                "case {case:?}: every reported parameter names itself, got {parameter:?}"
            );
            assert_eq!(
                parameter.source.as_deref(),
                Some(PARAMETER_SOURCE_ENGINE_DEFAULT),
                "case {case:?}: {name} is the ENGINE's default, got {:?}",
                parameter.source
            );
            assert!(
                [Some(APPLY_TYPE_STATIC), Some(APPLY_TYPE_DYNAMIC)]
                    .contains(&parameter.apply_type.as_deref()),
                "case {case:?}: {name} must report when a change to it applies, got {:?}",
                parameter.apply_type
            );
            assert!(
                parameter
                    .data_type
                    .as_deref()
                    .is_some_and(|data_type| DATA_TYPES.contains(&data_type)),
                "case {case:?}: {name} must report a published DataType, got {:?}",
                parameter.data_type
            );
            assert!(
                parameter
                    .is_modifiable
                    .as_deref()
                    .is_some_and(|word| BOOLEAN_WORDS.contains(&word)),
                "case {case:?}: {name} must report whether a caller may set it, got {:?}",
                parameter.is_modifiable
            );
            assert!(
                parameter
                    .description
                    .as_ref()
                    .is_some_and(|description| !description.is_empty()),
                "case {case:?}: {name} must report the catalog's description, got {:?}",
                parameter.description
            );
            // The report is a discovery catalog, so the default it publishes for a parameter
            // must be a value that parameter's OWN published domain admits: a client reads
            // both members off the same entry and treats the default as a legal setting.
            let Some(allowed) = parameter.allowed_values.as_deref() else {
                continue;
            };
            let Some(value) = parameter.value.as_deref() else {
                continue;
            };
            if let Some(admitted) = admits(allowed, value) {
                assert!(
                    admitted,
                    "case {case:?}: {name} publishes the default {value:?}, which its own \
                     AllowedValues {allowed:?} does not admit"
                );
            }
        }
    }
}

/// What a client does with the catalog it discovered: it sets a parameter to the default the
/// report published, reads it back as the group's own, and pre-validates the next value
/// against the domain the report published — which the service refuses in a sentence naming
/// that parameter and that domain.
#[tokio::test]
async fn a_group_is_configured_from_the_discovered_engine_defaults() {
    let router = emulator();

    for (index, (row, family, publishes)) in FAMILIES.iter().enumerate() {
        if !publishes {
            continue;
        }
        let case = format!("{row} ({family})");
        let catalog = discovered(&router, &case, family).await;

        // The first entry a caller may set whose published default its own published domain
        // admits — picked from the report, which is all a discovery client has.
        let settable = catalog.iter().find(|parameter| {
            parameter.is_modifiable.as_deref() == Some("true")
                && match (parameter.allowed_values.as_deref(), parameter.value.as_deref()) {
                    (Some(allowed), Some(value)) => admits(allowed, value) == Some(true),
                    _ => false,
                }
        });
        let Some(parameter) = settable else {
            panic!(
                "case {case:?}: expected the discovered catalog to publish a modifiable \
                 parameter whose default its own domain admits, got {catalog:?}"
            );
        };
        let name = parameter.name.as_str();
        let value = parameter.value.as_deref().unwrap_or_default();
        let allowed = parameter.allowed_values.as_deref().unwrap_or_default();

        let group = create_group(
            &router,
            &case,
            &format!("emu-discovered-defaults-{index}"),
            family,
        )
        .await;
        expect_ok(
            &case,
            modify_in(&router, REGION, &group, &[(name, value, None)]).await,
        );

        let body = expect_ok(
            &case,
            describe(
                &router,
                &group,
                &[("Source", PARAMETER_SOURCE_USER.to_string())],
            )
            .await,
        );
        let carried = subtrees(&body, "Parameter")
            .into_iter()
            .find(|entry| text(entry, "ParameterName") == name);
        let Some(carried) = carried else {
            panic!(
                "case {case:?}: expected the group to report {name} as its own setting, got \
                 {body}"
            );
        };
        assert_eq!(
            (
                text(&carried, "ParameterValue"),
                text(&carried, "AllowedValues")
            ),
            (value.to_string(), allowed.to_string()),
            "case {case:?}: expected the group to carry the discovered default of {name} with \
             the domain the catalog published, got {carried}"
        );

        // And the domain the report published is the one the service enforces, so a client
        // that read the catalog can tell a setting the service will refuse from one it will
        // take — where the domain is a range, one step past its top.
        let Some(outside) = one_past_the_published_range(allowed) else {
            continue;
        };
        let refused = expect_error(
            &case,
            modify_in(&router, REGION, &group, &[(name, &outside, None)]).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&refused, "Message");
        assert!(
            message.contains(name) && message.contains(allowed) && message.contains(&outside),
            "case {case:?}: expected the refusal of {outside:?} to name {name}, the value and \
             the published domain {allowed:?}, got {message:?}"
        );
    }
}
