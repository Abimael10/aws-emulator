// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DB CLUSTER parameter groups Amazon RDS provides ITSELF in every
//! account and region, and for the `Source` a parameter report may be asked in.
//!
//! The rules under test:
//!
//! * every account holds one default DB cluster parameter group per `DBParameterGroupFamily`
//!   whose engine has a CLUSTER form, created by no request: it describes, it carries the
//!   members its wire shape publishes, its parameters read back (the family's own published
//!   defaults, and nothing of the caller's), and a request that would CHANGE it — the modify,
//!   and the reset, which is a change too — is refused rather than answered as a group that
//!   does not exist. A family of an engine with no cluster form has no such group, and is
//!   answered as missing;
//! * a parameter report's `Source` is a closed value list — "Valid Values: engine-default |
//!   system | user" — so a value outside it is refused with `InvalidParameterValue` naming
//!   the value and the list, while a value inside it selects, whatever case it is written in.
//!
//! Both rules are asserted over TABLES spanning what they generalise over: the cluster
//! families of SIX engines of three different control planes beside three families of
//! instance-only engines, and the `Source` vocabulary in every published value, each in
//! another case, beside the values an older vocabulary used.
//!
//! Every call goes over the wire: the service is mounted on a `MultiServiceRouter` exactly as
//! the production router mounts it, each request is a real awsQuery form POST carrying the
//! SigV4 credential scope the router keys on, and every assertion reads the XML the emulator
//! answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, subtrees, text, texts, REGION,
};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The error code Amazon RDS answers a parameter value its member does not accept with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The error code Amazon RDS answers a parameter group that is not there with.
const NOT_FOUND: &str = "DBParameterGroupNotFound";

/// A `DBParameterGroupFamily` this service publishes an available engine version of for
/// `engine`, read out of the report that publishes it — so the catalogue itself decides which
/// families, and therefore which default groups, exist.
///
/// The FIRST family the engine reports is taken rather than the family of its default
/// version: an engine of a sibling control plane this front door serves publishes no
/// engine-level default version at all, and every family of the engine has the same answer
/// under test.
async fn published_family(router: &MultiServiceRouter, engine: &str) -> String {
    let body = expect_ok(
        engine,
        call(
            router,
            "DescribeDBEngineVersions",
            &[("Engine", engine), ("MaxRecords", "100")],
        )
        .await,
    );
    let entry = subtrees(&body, "DBEngineVersion")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!("case {engine:?}: the engine must publish a version, got {body}")
        });
    text(&entry, "DBParameterGroupFamily")
}

/// The name RDS gives the default parameter group of a family: `default.` followed by the
/// family, at the cluster level exactly as at the instance level.
fn default_group_name(family: &str) -> String {
    format!("default.{family}")
}

/// `DescribeDBClusterParameterGroups` naming one group.
async fn describe_group(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBClusterParameterGroups",
        &[("DBClusterParameterGroupName", name)],
    )
    .await
}

/// `DescribeDBClusterParameters` on one group, with an optional `Source`.
async fn describe_parameters(
    router: &MultiServiceRouter,
    name: &str,
    source: Option<&str>,
) -> (u16, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("DBClusterParameterGroupName", name),
        ("MaxRecords", "100"),
    ];
    if let Some(source) = source {
        params.push(("Source", source));
    }
    call(router, "DescribeDBClusterParameters", &params).await
}

/// One engine the table runs over, and whether a DB CLUSTER can run it.
///
/// Source: the value list `CreateDBClusterMessage$Engine` publishes — the engines with a
/// cluster form — beside the engines of this front door that run only as a single DB
/// instance. Both halves are rows, because the rule is "RDS provides a default DB cluster
/// parameter group for exactly the families of a cluster engine".
const ENGINES: [(&str, bool); 9] = [
    // The Aurora engines, whose every DB instance is a cluster member.
    ("aurora-mysql", true),
    ("aurora-postgresql", true),
    // The two RDS engines that also run as a Multi-AZ DB CLUSTER.
    ("mysql", true),
    ("postgres", true),
    // The engines of the two sibling control planes this one front door serves.
    ("docdb", true),
    ("neptune", true),
    // Instance-only engines: no cluster, so no cluster parameter group.
    ("mariadb", false),
    ("oracle-se2", false),
    ("sqlserver-ex", false),
];

/// Each cluster engine's default version names a family, and the account holds the default DB
/// cluster parameter group of that family: it describes with the members its shape publishes,
/// its parameters are the family's published defaults, it holds none of the caller's, and it
/// cannot be changed. A family of an instance-only engine has no such group.
#[tokio::test]
async fn the_account_holds_the_default_cluster_group_of_every_cluster_family() {
    let router = emulator();

    for (engine, has_a_cluster_form) in ENGINES {
        let family = published_family(&router, engine).await;
        let name = default_group_name(&family);
        let case = format!("{engine} {family}");

        if !has_a_cluster_form {
            // No cluster runs this engine, so RDS provides no cluster parameter group for
            // its families — both reads answer the modelled 404.
            expect_error(
                &case,
                describe_group(&router, &name).await,
                404,
                NOT_FOUND,
            );
            expect_error(
                &case,
                describe_parameters(&router, &name, None).await,
                404,
                NOT_FOUND,
            );
            continue;
        }

        let described = expect_ok(&case, describe_group(&router, &name).await);
        assert_eq!(
            (
                text(&described, "DBClusterParameterGroupName"),
                text(&described, "DBParameterGroupFamily"),
                text(&described, "Description"),
                text(&described, "DBClusterParameterGroupArn"),
            ),
            (
                name.clone(),
                family.clone(),
                format!("Default cluster parameter group for {family}"),
                format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster-pg:{name}"),
            ),
            "case {case:?}: the default group must report the family it belongs to, got \
             {described}"
        );

        // Its parameters are the FAMILY's: the group itself holds none of the caller's, so a
        // report of the caller's own parameters is empty while the unfiltered report carries
        // the family's published list, each entry sourced `engine-default`.
        let own = expect_ok(&case, describe_parameters(&router, &name, Some("user")).await);
        assert_eq!(
            texts(&own, "ParameterName"),
            Vec::<String>::new(),
            "case {case:?}: a default group holds no parameter of the caller's, got {own}"
        );
        let inherited = expect_ok(&case, describe_parameters(&router, &name, None).await);
        let names = texts(&inherited, "ParameterName");
        let sources = texts(&inherited, "Source");
        assert_eq!(
            sources.len(),
            names.len(),
            "case {case:?}: every reported parameter names its source, got {inherited}"
        );
        assert!(
            sources.iter().all(|source| source == "engine-default"),
            "case {case:?}: a default group reports the engine's own values, got {sources:?}"
        );
        // The same list the FAMILY publishes as its engine defaults, read from the report
        // that owns that catalogue.
        let published = expect_ok(
            &case,
            call(
                &router,
                "DescribeEngineDefaultClusterParameters",
                &[("DBParameterGroupFamily", family.as_str()), ("MaxRecords", "100")],
            )
            .await,
        );
        assert_eq!(
            names,
            texts(&published, "ParameterName"),
            "case {case:?}: the default group's parameters must be its family's published \
             defaults, got {inherited}"
        );

        // "Cannot modify a default parameter group": the group is RDS's, so both calls that
        // would CHANGE it — the modify, and the reset, which is a change too — report the
        // restriction rather than a group that does not exist, and the group is still there
        // afterwards.
        for (change, params) in [
            (
                "ModifyDBClusterParameterGroup",
                vec![
                    ("DBClusterParameterGroupName", name.as_str()),
                    ("Parameters.Parameter.1.ParameterName", "tls"),
                    ("Parameters.Parameter.1.ParameterValue", "disabled"),
                ],
            ),
            (
                "ResetDBClusterParameterGroup",
                vec![
                    ("DBClusterParameterGroupName", name.as_str()),
                    ("ResetAllParameters", "true"),
                ],
            ),
        ] {
            let case = format!("{case}: {change}");
            let refused = expect_error(
                &case,
                call(&router, change, &params).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            assert_eq!(
                text(&refused, "Message"),
                "Cannot modify a default parameter group.",
                "case {case:?}: expected the fixed default-group refusal, got {refused}"
            );
            expect_ok(&case, describe_group(&router, &name).await);
        }
    }
}

/// An unfiltered `DescribeDBClusterParameterGroups` reports the account's default groups
/// BESIDE the groups a request created — which is what a caller enumerating the catalog, or
/// resolving a template that names a default group, reads.
#[tokio::test]
async fn an_unfiltered_describe_reports_the_defaults_beside_the_accounts_own_group() {
    let router = emulator();
    let family = published_family(&router, "aurora-postgresql").await;
    expect_ok(
        "create a cluster parameter group",
        call(
            &router,
            "CreateDBClusterParameterGroup",
            &[
                ("DBClusterParameterGroupName", "catalog-probe-cpg"),
                ("DBParameterGroupFamily", family.as_str()),
                ("Description", "a group of the account's own"),
            ],
        )
        .await,
    );

    let listed = expect_ok(
        "every group",
        call(
            &router,
            "DescribeDBClusterParameterGroups",
            &[("MaxRecords", "100")],
        )
        .await,
    );
    let names = texts(&listed, "DBClusterParameterGroupName");
    for expected in ["catalog-probe-cpg".to_string(), default_group_name(&family)] {
        assert!(
            names.contains(&expected),
            "the unfiltered listing must report {expected:?}, got {names:?}"
        );
    }
}

/// `Source` names one of the three sources the member publishes, in any case; any other
/// value is refused with the sentence naming the value and the values it could have named.
#[tokio::test]
async fn a_source_outside_the_published_values_is_refused() {
    let router = emulator();
    let family = published_family(&router, "aurora-mysql").await;
    let group = "source-probe-cpg";
    expect_ok(
        "create a cluster parameter group",
        call(
            &router,
            "CreateDBClusterParameterGroup",
            &[
                ("DBClusterParameterGroupName", group),
                ("DBParameterGroupFamily", family.as_str()),
                ("Description", "a group under test"),
            ],
        )
        .await,
    );

    // Every published value, each also in another case: the value list is resolved without
    // case, the way every other published name this service resolves is.
    for source in [
        "user",
        "User",
        "USER",
        "engine-default",
        "Engine-Default",
        "system",
        "SYSTEM",
    ] {
        let case = format!("Source {source:?}");
        expect_ok(&case, describe_parameters(&router, group, Some(source)).await);
    }

    // And every value outside it — including the names an older vocabulary of the member
    // used, which select nothing here — is refused, on the account's own group and on one of
    // RDS's alike.
    for group in [group, default_group_name(&family).as_str()] {
        for source in ["customer", "engine", "service", "not-a-source", "user "] {
            let case = format!("Source {source:?} on {group:?}");
            let refused = expect_error(
                &case,
                describe_parameters(&router, group, Some(source)).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            assert_eq!(
                text(&refused, "Message"),
                format!(
                    "Invalid Source: {source}. Allowed values are: engine-default, system, user"
                ),
                "case {case:?}: expected the refusal naming the value and the allowed ones, \
                 got {refused}"
            );
        }
    }

    // The same rule holds for the instance-level report, whose `Source` publishes the same
    // three values.
    let instance_group = "source-probe-pg";
    expect_ok(
        "create a DB parameter group",
        call(
            &router,
            "CreateDBParameterGroup",
            &[
                ("DBParameterGroupName", instance_group),
                ("DBParameterGroupFamily", family.as_str()),
                ("Description", "a group under test"),
            ],
        )
        .await,
    );
    expect_ok(
        "the instance report, Source User",
        call(
            &router,
            "DescribeDBParameters",
            &[
                ("DBParameterGroupName", instance_group),
                ("Source", "User"),
            ],
        )
        .await,
    );
    let refused = expect_error(
        "the instance report, Source customer",
        call(
            &router,
            "DescribeDBParameters",
            &[
                ("DBParameterGroupName", instance_group),
                ("Source", "customer"),
            ],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    assert_eq!(
        text(&refused, "Message"),
        "Invalid Source: customer. Allowed values are: engine-default, system, user",
        "the instance-level report must refuse the same values, got {refused}"
    );
}
