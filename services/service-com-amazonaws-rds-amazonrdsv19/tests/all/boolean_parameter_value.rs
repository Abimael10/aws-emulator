// SPDX-License-Identifier: CC0-1.0

//! The value rule of a parameter whose published `DataType` is a BOOLEAN, on both
//! parameter-group levels of this one `rds` front door.
//!
//! The rule: a value a parameter's published `DataType` cannot carry is refused with HTTP
//! 400 `InvalidParameterValue` worded about that TYPE — for a boolean parameter, the fixed
//! text "Invalid boolean value: " followed by the `ParameterValue` the request carried — and
//! nothing is stored, so the parameter keeps the value the group already reported for it.
//! The two domain-shaped wordings (a numeric range, an enumeration) belong to the parameters
//! whose type the value DID carry, and `cluster_parameter_value_range` and
//! `db_parameter_metadata` beside this suite span those.
//!
//! The table is every boolean parameter of every catalog this front door publishes — both
//! levels, each cluster and instance engine vocabulary, the families chosen so none is the
//! one the recorded request used alone — crossed with the ways a value can fail the type:
//! a word of no type at all, an integer past the switch, a negative one, and the two
//! spellings another engine's own boolean parser takes (`on`, `true`). The two values the
//! type DOES carry are written first on every one of those parameters and read back from
//! state, so each refusal is asserted against a parameter the group really carries a value
//! for.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    family_parameters, published_cluster_parameters, published_db_parameters,
    published_spec_for_family, ClusterParameterSpec, DATA_TYPE_BOOLEAN, PARAMETER_SOURCE_USER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};

/// The error code Amazon RDS answers a `ParameterValue` it cannot serve with: the query
/// protocol's common client error, which this operation's own modelled list does not carry.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The values no boolean parameter can carry, spanning the ways a value fails that type: a
/// word of no type, an integer past the two the switch has, a negative integer, and the
/// spellings another engine's own boolean parser accepts but that no `AllowedValues` of this
/// API publishes.
const REFUSED_VALUES: [&str; 5] = ["not-a-valid-value-xyz", "2", "-1", "on", "true"];

/// The sentence Amazon RDS refuses a value a boolean parameter cannot carry with, written
/// out here as the LITERAL text the API answers: its fixed part, then the `ParameterValue`
/// the request carried and nothing else.
fn expected_refusal(value: &str) -> String {
    format!("Invalid boolean value: {value}")
}

/// Which parameter-group level a row drives: the two operations that write a parameter value,
/// over the two catalogs a family publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// A DB CLUSTER parameter group: `ModifyDBClusterParameterGroup`.
    Cluster,
    /// A DB (instance) parameter group: `ModifyDBParameterGroup`.
    Instance,
}

impl Level {
    /// The create, modify and describe operations of this level, and the member every one of
    /// them names the group by.
    fn operations(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Level::Cluster => (
                "CreateDBClusterParameterGroup",
                "ModifyDBClusterParameterGroup",
                "DescribeDBClusterParameters",
                "DBClusterParameterGroupName",
            ),
            Level::Instance => (
                "CreateDBParameterGroup",
                "ModifyDBParameterGroup",
                "DescribeDBParameters",
                "DBParameterGroupName",
            ),
        }
    }

    /// The catalog this level publishes for `family`.
    fn catalog(self, family: &str) -> Option<&'static [ClusterParameterSpec]> {
        match self {
            Level::Cluster => published_cluster_parameters(family),
            Level::Instance => published_db_parameters(family),
        }
    }
}

/// The boolean parameters `family` carries at `level` that a caller may set: a parameter the
/// engine protects is refused before its value is read at all, which is its own rule.
fn boolean_parameters(level: Level, family: &str) -> Vec<&'static ClusterParameterSpec> {
    let Some(catalog) = level.catalog(family) else {
        return Vec::new();
    };
    family_parameters(family, catalog)
        .into_iter()
        .filter(|spec| spec.data_type == DATA_TYPE_BOOLEAN && spec.is_modifiable)
        .collect()
}

/// Create a parameter group of `family` at `level` and answer the name the store keys it
/// under (every group name "is stored as a lowercase string").
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    level: Level,
    family: &str,
) -> String {
    let (create, _, _, member) = level.operations();
    expect_ok(
        case,
        call(
            router,
            create,
            &[
                (member, name),
                ("DBParameterGroupFamily", family),
                ("Description", "a boolean parameter probe"),
            ],
        )
        .await,
    );
    name.to_ascii_lowercase()
}

/// Set one parameter on `group` at `level`.
async fn modify(
    router: &MultiServiceRouter,
    level: Level,
    group: &str,
    name: &str,
    value: &str,
) -> (u16, String) {
    let (_, modify, _, member) = level.operations();
    call(
        router,
        modify,
        &[
            (member, group),
            ("Parameters.Parameter.1.ParameterName", name),
            ("Parameters.Parameter.1.ParameterValue", value),
        ],
    )
    .await
}

/// The value `group` reports as the CALLER's (`Source` `user`) for `name`, or `None` where it
/// carries none of its own for it.
async fn user_value(
    router: &MultiServiceRouter,
    level: Level,
    group: &str,
    name: &str,
) -> Option<String> {
    let (_, _, describe, member) = level.operations();
    let reported = expect_ok(
        name,
        call(
            router,
            describe,
            &[(member, group), ("Source", PARAMETER_SOURCE_USER)],
        )
        .await,
    );
    subtrees(&reported, "Parameter")
        .into_iter()
        .find(|parameter| text(parameter, "ParameterName") == name)
        .map(|parameter| text(&parameter, "ParameterValue"))
}

/// The rows: both levels, every cluster and instance engine vocabulary this front door
/// publishes boolean parameters for, at families the recorded request never named.
const ROWS: [(&str, Level, &str); 7] = [
    ("an Aurora PostgreSQL cluster family", Level::Cluster, "aurora-postgresql16"),
    (
        "an RDS for PostgreSQL Multi-AZ cluster family",
        Level::Cluster,
        "postgres16",
    ),
    ("an Aurora MySQL cluster family", Level::Cluster, "aurora-mysql8.0"),
    ("an Amazon Neptune cluster family", Level::Cluster, "neptune1.4"),
    ("an RDS for PostgreSQL instance family", Level::Instance, "postgres17"),
    ("an RDS for MySQL instance family", Level::Instance, "mysql8.0"),
    ("an RDS for MariaDB instance family", Level::Instance, "mariadb10.11"),
];

/// Every boolean parameter of every published catalog takes the two values its type has and
/// refuses everything else as an invalid boolean value, leaving the group as it was.
#[tokio::test]
async fn a_boolean_parameter_refuses_a_value_of_no_boolean_type() {
    let router = emulator();

    for (index, (case, level, family)) in ROWS.into_iter().enumerate() {
        let specs = boolean_parameters(level, family);
        assert!(
            !specs.is_empty(),
            "case {case:?}: expected {family} to publish a modifiable boolean parameter at \
             {level:?} level"
        );
        let group = create_group(
            &router,
            case,
            &format!("emu-boolean-{index}"),
            level,
            family,
        )
        .await;

        for spec in specs {
            // Every boolean parameter publishes the two values of the switch as its own
            // `AllowedValues`, so the type-worded refusal always has a published domain
            // behind it.
            assert_eq!(
                spec.allowed_values, "0,1",
                "case {case:?}: expected {} to publish the two values of a switch, got {:?}",
                spec.name, spec.allowed_values
            );

            // The two values the type carries are taken and read back from state.
            for value in spec.allowed_values.split(',') {
                let case = format!("{case}: {family} {} = {value:?} (admitted)", spec.name);
                expect_ok(
                    &case,
                    modify(&router, level, &group, spec.name, value).await,
                );
                let stored = user_value(&router, level, &group, spec.name).await;
                assert_eq!(
                    stored.as_deref(),
                    Some(value),
                    "case {case:?}: expected the group to report the stored value, got \
                     {stored:?}"
                );
            }
            // The value the group carries when each refusal is made: the last one written.
            let carried = spec.allowed_values.split(',').next_back().map(str::to_string);

            for refused in REFUSED_VALUES {
                let case = format!("{case}: {family} {} = {refused:?} (refused)", spec.name);
                let answered = expect_error(
                    &case,
                    modify(&router, level, &group, spec.name, refused).await,
                    400,
                    INVALID_PARAMETER_VALUE,
                );
                let message = text(&answered, "Message");
                let expected = expected_refusal(refused);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                let stored = user_value(&router, level, &group, spec.name).await;
                assert_eq!(
                    stored, carried,
                    "case {case:?}: a refused value must leave the group as it was, got \
                     {stored:?}"
                );
            }
        }
    }
}

/// A boolean parameter the request never got past keeps the engine's own default: a refusal
/// stores nothing, so the parameter is still reported from the engine's catalog with a
/// `Source` other than the caller's, and the group carries no value of its own at all.
#[tokio::test]
async fn a_refused_boolean_value_leaves_the_parameter_at_the_engine_default() {
    let router = emulator();

    for (index, (case, level, family)) in ROWS.into_iter().enumerate() {
        let specs = boolean_parameters(level, family);
        let Some(spec) = specs.first() else {
            panic!("case {case:?}: expected {family} to publish a modifiable boolean parameter");
        };
        let group = create_group(
            &router,
            case,
            &format!("emu-boolean-default-{index}"),
            level,
            family,
        )
        .await;

        for refused in REFUSED_VALUES {
            let case = format!("{case}: {family} {} = {refused:?}", spec.name);
            let answered = expect_error(
                &case,
                modify(&router, level, &group, spec.name, refused).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            let message = text(&answered, "Message");
            let expected = expected_refusal(refused);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }

        // Nothing was stored: the group reports no parameter of its own, and the engine's
        // default for the parameter is still the only value reported for it.
        let (_, _, describe, member) = level.operations();
        let own = expect_ok(
            case,
            call(
                &router,
                describe,
                &[(member, group.as_str()), ("Source", PARAMETER_SOURCE_USER)],
            )
            .await,
        );
        assert!(
            subtrees(&own, "Parameter").is_empty(),
            "case {case:?}: a refused write must leave the group carrying nothing of its \
             own, got {own}"
        );
        // ...and the parameter is still reported from the ENGINE's catalog, at the default
        // its family publishes. The report is paged to the end, so the parameter is found
        // wherever its page falls rather than only when it lands on the first one.
        let (_, _, describe, member) = level.operations();
        let mut defaults: Vec<String> = Vec::new();
        let mut marker: Option<String> = None;
        // Bounded by the longest catalog this service publishes, at 100 records a page.
        for _ in 0..12 {
            let mut params: Vec<(&str, String)> = vec![
                (member, group.clone()),
                ("Source", "engine-default".to_string()),
                ("MaxRecords", "100".to_string()),
            ];
            if let Some(marker) = &marker {
                params.push(("Marker", marker.clone()));
            }
            let page = expect_ok(case, call(&router, describe, &params).await);
            defaults.extend(subtrees(&page, "Parameter"));
            marker = optional_text(&page, "Marker");
            if marker.is_none() {
                break;
            }
        }
        assert!(
            marker.is_none(),
            "case {case:?}: the engine-default report of {group} never finished paging"
        );
        let reported = defaults
            .into_iter()
            .find(|parameter| text(parameter, "ParameterName") == spec.name)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected {} to still be reported among the family's \
                     engine defaults",
                    spec.name
                )
            });
        // The default is the one the parameter's record carries FOR this family: a record
        // the engine moved between releases reads as that family's release has it.
        let published = published_spec_for_family(family, spec);
        let value = text(&reported, "ParameterValue");
        assert_eq!(
            value, published.default_value,
            "case {case:?}: expected {} to still report the engine's own default, got \
             {reported}",
            spec.name
        );
        assert_eq!(
            text(&reported, "Source"),
            "engine-default",
            "case {case:?}: expected {} to still be reported from the engine's catalog, got \
             {reported}",
            spec.name
        );
    }
}
