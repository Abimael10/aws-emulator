// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `Parameter$Description` — "Provides a description of the parameter"
//! — as every door of this service's parameter slice reports it.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on, so every assertion reads the XML the emulator itself produced.
//!
//! The rule under test: a parameter's `Description` is the text the parameter's own published
//! catalog carries for it, reported BYTE FOR BYTE — the published sentence as written, its
//! punctuation and its missing trailing period included — and the same string whichever door
//! reports that parameter, since one catalog per engine serves them all. A re-worded or
//! re-punctuated sentence is a `Parameter$Description` the service never reports, which is
//! what a console, a doc generator or a configuration-drift check surfaces to a human.
//!
//! The tables span what the rule generalises over rather than one request:
//!
//! * [`PUBLISHED_TEXT`] is one row per published sentence FORM the catalogs carry — a
//!   sentence that ends in a period and one that does not, a one-clause sentence and a
//!   multi-clause one, a sentence that opens with the unit the engine publishes the setting
//!   in, and a name written with a hyphen and one written with a dot — across both parameter
//!   vocabularies this front door serves and over several families of each, including rows
//!   and families no issue cited;
//! * every row is read through the FAMILY's engine-default door and through a GROUP of that
//!   family, and [`the_two_doors_agree_on_every_parameter_of_a_family`] widens that to EVERY
//!   parameter of each family rather than the named ones;
//! * a parameter both LEVELS of a cluster engine publish is read through the instance-level
//!   and the cluster-level engine-default door, which must report one and the same sentence.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_ok, optional_text};
use crate::db_parameters::create_group;
use crate::engine_default_parameters::reported;

/// One published sentence: the parameter it belongs to, the families that publish the
/// parameter, and the text the API reports for it.
struct PublishedText {
    case: &'static str,
    name: &'static str,
    /// Families that carry the parameter: several per row, because the sentence belongs to
    /// the PARAMETER and not to any one family of its engine.
    families: &'static [&'static str],
    description: &'static str,
}

/// The rows the rule is asserted over.
///
/// Source: the published parameter lists of the engines this front door serves — the Amazon
/// RDS and Amazon Aurora User Guides' MySQL parameter tables, and PostgreSQL's own settings
/// catalog, whose description of a setting measured in milliseconds opens with the `(ms)`
/// unit prefix. Each string here is the sentence the API reports for that parameter, written
/// exactly as published; the trailing period is part of the sentence where the catalog writes
/// one and absent where it does not, so the rows cover both.
const PUBLISHED_TEXT: &[PublishedText] = &[
    PublishedText {
        case: "a MySQL sentence with no trailing period, on a hyphenated parameter name",
        name: "allow-suspicious-udfs",
        families: &["mysql5.7", "mysql8.0", "mariadb10.11", "aurora-mysql8.0"],
        description: "Controls whether user-defined functions that have only an xxx symbol \
                      for the main function can be loaded",
    },
    PublishedText {
        case: "a MySQL sentence that ends in a period",
        name: "auto_generate_certs",
        families: &["mysql5.7", "mysql8.0", "aurora-mysql8.0"],
        description: "Controls whether the server autogenerates SSL key and certificate \
                      files in the data directory, if they do not already exist.",
    },
    PublishedText {
        case: "a MySQL sentence about a pair of parameters, with no trailing period",
        name: "auto_increment_increment",
        families: &["mysql5.7", "mysql8.0", "mariadb10.11", "aurora-mysql8.0"],
        description: "Intended for use with master-to-master replication, and can be used \
                      to control the operation of AUTO_INCREMENT columns",
    },
    PublishedText {
        case: "the second parameter of that same pair",
        name: "auto_increment_offset",
        families: &["mysql5.7", "mysql8.0", "mariadb10.11", "aurora-mysql8.0"],
        description: "Determines the starting point for the AUTO_INCREMENT column value",
    },
    PublishedText {
        case: "the shortest MySQL sentence of the head",
        name: "autocommit",
        families: &["mysql5.7", "mysql8.0", "mariadb10.11", "aurora-mysql8.0"],
        description: "Sets the autocommit mode",
    },
    PublishedText {
        case: "a multi-clause MySQL sentence that names the parameter's own default",
        name: "automatic_sp_privileges",
        families: &["mysql5.7", "mysql8.0", "mariadb10.11", "aurora-mysql8.0"],
        description: "When this variable has a value of 1 (the default), the server \
                      automatically grants the EXECUTE and ALTER ROUTINE privileges to the \
                      creator of a stored routine, if the user cannot already execute and \
                      alter or drop the routine.",
    },
    PublishedText {
        case: "a MySQL sentence that opens by naming the variable itself",
        name: "avoid_temporal_upgrade",
        families: &["mysql5.7", "mysql8.0", "mariadb10.11", "aurora-mysql8.0"],
        description: "This variable controls whether ALTER TABLE implicitly upgrades \
                      temporal columns found to be in pre-5.6.4 format.",
    },
    PublishedText {
        case: "a MySQL sentence that names the engine, with no trailing period",
        name: "back_log",
        families: &["mysql5.7", "mysql8.0", "mariadb10.11", "aurora-mysql8.0"],
        description: "The number of outstanding connection requests MySQL can have",
    },
    PublishedText {
        case: "a MySQL sentence for a parameter only the later release line carries",
        name: "activate_all_roles_on_login",
        families: &["mysql8.0", "aurora-mysql8.0"],
        description: "Automatically set all granted roles as active after the user has \
                      authenticated successfully.",
    },
    PublishedText {
        case: "a PostgreSQL sentence that opens with the unit the setting is measured in",
        name: "log_min_duration_statement",
        families: &["postgres16", "aurora-postgresql15"],
        description: "(ms) Sets the minimum execution time above which statements will be \
                      logged.",
    },
    PublishedText {
        case: "a PostgreSQL sentence on a parameter whose name carries a dot",
        name: "rds.logical_replication",
        families: &["postgres16", "aurora-postgresql15"],
        description: "Enables logical decoding.",
    },
    PublishedText {
        case: "a PostgreSQL sentence that names what the setting selects",
        name: "log_statement",
        families: &["postgres16", "aurora-postgresql15"],
        description: "Sets the type of statements logged.",
    },
];

/// The families whose two doors are compared parameter by parameter: one per engine
/// vocabulary, and two release lines of one engine.
const COMPARED_FAMILIES: [&str; 5] = [
    "mysql5.7",
    "mysql8.0",
    "mariadb10.11",
    "postgres16",
    "aurora-postgresql15",
];

/// A parameter both LEVELS of a cluster engine publish, with the family that publishes it:
/// the one sentence each must report, read through the instance-level and the cluster-level
/// engine-default door.
const AT_BOTH_LEVELS: [(&str, &str); 2] = [
    ("aurora-mysql8.0", "activate_all_roles_on_login"),
    ("aurora-postgresql15", "rds.logical_replication"),
];

/// The parameter-name filter this operation documents as its only supported filter, asking
/// for exactly `name`.
fn parameter_name_filter(name: &str) -> Vec<(String, String)> {
    vec![
        (
            "Filters.Filter.1.Name".to_string(),
            "parameter-name".to_string(),
        ),
        (
            "Filters.Filter.1.Values.Value.1".to_string(),
            name.to_string(),
        ),
    ]
}

/// The `Description` the FAMILY's engine-default door reports for `name` in `family`, or
/// `None` when that door reports no such parameter.
async fn described_by(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    name: &str,
) -> Option<String> {
    let mut params = vec![(
        "DBParameterGroupFamily".to_string(),
        family.to_string(),
    )];
    params.extend(parameter_name_filter(name));
    let body = expect_ok(
        case,
        call(router, "DescribeEngineDefaultParameters", &params).await,
    );
    let rows = reported(&body);
    assert!(
        rows.len() <= 1,
        "case {case:?}: a parameter-name filter selects at most the one parameter it names, \
         got {:?}",
        rows.iter().map(|row| &row.name).collect::<Vec<_>>()
    );
    let row = rows.into_iter().next()?;
    assert_eq!(
        row.name, name,
        "case {case:?}: expected the filtered parameter, got {:?}",
        row.name
    );
    row.description
}

/// The `Description` the CLUSTER-level engine-default door reports for `name` in `family`,
/// or `None` when that door publishes no such parameter.
///
/// That door's own `Filters` member is documented "This parameter isn't currently
/// supported", so the whole published page is read and the row is picked out of it.
async fn cluster_described(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    name: &str,
) -> Option<String> {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeEngineDefaultClusterParameters",
            &[
                ("DBParameterGroupFamily", family),
                // "Constraints: Minimum 20, maximum 100" — the largest page the model
                // admits, which every cluster-level catalog this door publishes fits in.
                ("MaxRecords", "100"),
            ],
        )
        .await,
    );
    assert_eq!(
        optional_text(&body, "Marker"),
        None,
        "case {case:?}: the cluster-level catalog fits one page, so no Marker is owed"
    );
    reported(&body)
        .into_iter()
        .find(|row| row.name == name)
        .and_then(|row| row.description)
}

/// Every parameter a DB parameter group of `family` reports, by name, with the `Description`
/// each row carried.
///
/// The whole listing is walked across its `Marker`s, so a parameter past the first page is
/// read too; the walk is bounded, because a catalog this front door publishes is finite.
async fn group_descriptions(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> BTreeMap<String, Option<String>> {
    /// More pages than any family's published catalog can fill at the largest page size the
    /// model admits, so a walk that does not end is a failure rather than a hang.
    const PAGE_LIMIT: usize = 32;
    /// "Constraints: Minimum 20, maximum 100" — the largest page the model admits.
    const PAGE: &str = "100";

    let mut described = BTreeMap::new();
    let mut marker: Option<String> = None;
    for _ in 0..PAGE_LIMIT {
        let mut params = vec![
            ("DBParameterGroupName".to_string(), group.to_string()),
            ("MaxRecords".to_string(), PAGE.to_string()),
        ];
        if let Some(marker) = &marker {
            params.push(("Marker".to_string(), marker.clone()));
        }
        let body = expect_ok(case, call(router, "DescribeDBParameters", &params).await);
        for row in reported(&body) {
            described.insert(row.name, row.description);
        }
        marker = optional_text(&body, "Marker");
        if marker.is_none() {
            return described;
        }
    }
    panic!("case {case:?}: the group's parameter listing did not end within {PAGE_LIMIT} pages");
}

/// Each published parameter reports its own published sentence, byte for byte, through the
/// family's engine-default door and through a group of that family.
#[tokio::test]
async fn each_parameter_reports_its_published_description() {
    let router = emulator();

    for (index, family) in COMPARED_FAMILIES.iter().enumerate() {
        // One group per family, so the group door is asked about the same catalog the
        // engine-default door is.
        let group = format!("emu-published-text-{index}");
        create_group(&router, &format!("family {family}"), &group, family).await;
    }

    for row in PUBLISHED_TEXT {
        for family in row.families {
            let case = format!("{} ({} of {family})", row.case, row.name);
            let reported = described_by(&router, &case, family, row.name).await;
            assert_eq!(
                reported.as_deref(),
                Some(row.description),
                "case {case:?}: expected the published sentence, got {reported:?}"
            );
        }
    }

    // The same sentence through the GROUP door: a group reports the metadata of its family's
    // parameters from the one catalog, so the two can never disagree.
    for (index, family) in COMPARED_FAMILIES.iter().enumerate() {
        let group = format!("emu-published-text-{index}");
        let described = group_descriptions(&router, &format!("family {family}"), &group).await;
        for row in PUBLISHED_TEXT {
            if !row.families.contains(family) {
                continue;
            }
            let case = format!("{} ({} in group of {family})", row.case, row.name);
            let reported = described.get(row.name).cloned().flatten();
            assert_eq!(
                reported.as_deref(),
                Some(row.description),
                "case {case:?}: a group reports its family's published sentence, got \
                 {reported:?}"
            );
        }
    }
}

/// For EVERY parameter of a family — not only the named rows — the family's engine-default
/// door and a group of that family report one and the same `Description`.
#[tokio::test]
async fn the_two_doors_agree_on_every_parameter_of_a_family() {
    let router = emulator();

    for (index, family) in COMPARED_FAMILIES.iter().enumerate() {
        let case = format!("family {family}");
        let group = format!("emu-two-doors-{index}");
        create_group(&router, &case, &group, family).await;
        let described = group_descriptions(&router, &case, &group).await;
        assert!(
            !described.is_empty(),
            "case {case:?}: the family publishes parameters, so its group reports them"
        );

        for (name, in_group) in &described {
            let at_family = described_by(&router, &case, family, name).await;
            assert_eq!(
                at_family.as_deref(),
                in_group.as_deref(),
                "case {case:?}: {name} must read the same published sentence at both doors, \
                 got {at_family:?} at the family and {in_group:?} in the group"
            );
        }
    }
}

/// A parameter both LEVELS of a cluster engine publish reports one and the same sentence at
/// each level: the description belongs to the PARAMETER, so the instance-level catalog and
/// the cluster-level one cannot word it differently.
#[tokio::test]
async fn both_levels_report_one_published_description() {
    let router = emulator();

    for (family, name) in AT_BOTH_LEVELS {
        let case = format!("{name} of {family} at both levels");
        let at_instance = described_by(&router, &case, family, name).await;
        let at_cluster = cluster_described(&router, &case, family, name).await;
        assert!(
            at_instance.as_ref().is_some_and(|text| !text.is_empty()),
            "case {case:?}: the instance level publishes this parameter, got {at_instance:?}"
        );
        assert_eq!(
            at_cluster, at_instance,
            "case {case:?}: both levels report the one published sentence, got {at_cluster:?} \
             at the cluster level and {at_instance:?} at the instance level"
        );
    }
}
