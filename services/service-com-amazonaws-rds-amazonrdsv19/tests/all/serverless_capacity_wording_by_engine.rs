// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH SENTENCE a serverless capacity above the ceiling a cluster
//! scales to is refused with, over the cluster engines this one awsQuery front door serves.
//!
//! One front door serves three control planes (`src/service.rs`), and the refusal wording is
//! the control plane's, not the front door's. The rule under test, asserted over a TABLE of
//! engines and capacities rather than over one request:
//!
//! * the AURORA control plane refuses a capacity above the ceiling its engine version reaches
//!   by naming that ceiling, the version imposing it and the page where the per-version
//!   requirements are published — and NOT the value it rejected, because the ways out are a
//!   lower capacity or another version;
//! * every other cluster engine with a serverless capacity range — Amazon Neptune, whose
//!   clusters scale in that engine's own capacity units — refuses the same configuration by
//!   naming the VALUE it would not take, the range the cluster scales in and the call that
//!   changes that range, with no engine version and no Aurora documentation in it.
//!
//! The rows span what the rule generalises over: the ENGINE (two Aurora engines and the
//! Neptune engine), and HOW FAR past the ceiling the capacity reaches (one half-step past it
//! — the smallest excess the member admits — and twice the ceiling), on each of
//! `MinCapacity` and `MaxCapacity`. Each engine's ceiling is read from that engine version's
//! own `ServerlessV2FeaturesSupport` report rather than written here, and each engine's
//! configurable floor is pinned by a create AT that floor and the ceiling being served, so
//! the range the refusal states is the range the service really configures a cluster in.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML the emulator itself answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, subtrees, text};

/// The Query API's own code for a member value the service will not serve.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The increment a capacity value is specified in: "You can specify ACU values in half-step
/// increments, such as 8, 8.5, 9, and so on"
/// (`smithy_query(query="shape",shape="ServerlessV2ScalingConfiguration")`, member
/// `MinCapacity`), so every capacity below stays on the grid the member admits and the
/// refusal under test is the ceiling rather than the step.
const CAPACITY_STEP: f64 = 0.5;

/// One ACU value as a request spells it and as the refusal names it: a double always carries
/// a fractional part (`4.0`, never `4`).
fn acu(value: f64) -> String {
    format!("{value:?}")
}

/// One cluster engine the rule is spanned over.
struct EngineRow {
    /// `CreateDBCluster$Engine`.
    engine: &'static str,
    /// Whether this engine's control plane is the Aurora one, which words a capacity above
    /// the ceiling by the engine VERSION that imposes it.
    aurora_wording: bool,
    /// The lowest capacity a cluster of this engine may be CONFIGURED at, which the range
    /// sentence states as its floor. Pinned by a served create at this floor below, so no row
    /// rests on the figure alone: the Aurora engines configure a cluster from the half ACU
    /// their versions publish, while a Neptune Serverless cluster is configured from zero
    /// capacity.
    configured_floor: f64,
    /// The identifier prefix the clusters of this row are created under.
    prefix: &'static str,
}

/// The engines whose versions publish a serverless capacity range: the two Aurora engines
/// and the Neptune engine, which is the one serving this front door whose clusters scale in
/// another control plane's units.
const ENGINES: &[EngineRow] = &[
    EngineRow {
        engine: "aurora-postgresql",
        aurora_wording: true,
        configured_floor: 0.5,
        prefix: "emu-sv2-word-pg",
    },
    EngineRow {
        engine: "aurora-mysql",
        aurora_wording: true,
        configured_floor: 0.5,
        prefix: "emu-sv2-word-my",
    },
    EngineRow {
        engine: "neptune",
        aurora_wording: false,
        configured_floor: 0.0,
        prefix: "emu-sv2-word-npt",
    },
];

/// Which capacity member carries the offending value, so the rule is not read off one of
/// them: `MinCapacity` and `MaxCapacity` are the two the configuration has.
const MEMBERS: [&str; 2] = ["MinCapacity", "MaxCapacity"];

/// The DB subnet group the Neptune clusters name, created before any of them.
const SUBNET_GROUP: &str = "emu-sv2-word-subnets";

/// `CreateDBSubnetGroup` with the two subnets a group must name.
async fn create_subnet_group(router: &MultiServiceRouter) {
    expect_ok(
        "the subnet group the clusters name",
        call(
            router,
            "CreateDBSubnetGroup",
            &[
                ("DBSubnetGroupName", SUBNET_GROUP),
                ("DBSubnetGroupDescription", "serverless wording by engine"),
                ("SubnetIds.SubnetIdentifier.1", "subnet-0123456789abcdef0"),
                ("SubnetIds.SubnetIdentifier.2", "subnet-0fedcba987654321f"),
            ],
        )
        .await,
    );
}

/// The members a cluster create of `row` carries beside its scaling configuration: the
/// Aurora engines take a master user, while the Neptune engine's own API documents
/// `MasterUserPassword` as one it does not take and its clusters sit in a DB subnet group.
fn base_request(row: &EngineRow, identifier: &str) -> Vec<(&'static str, String)> {
    let mut request: Vec<(&'static str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", row.engine.to_string()),
    ];
    if row.aurora_wording {
        request.push(("MasterUsername", "emuadmin".to_string()));
        request.push(("MasterUserPassword", "a-strong-password".to_string()));
    } else {
        request.push(("DBSubnetGroupName", SUBNET_GROUP.to_string()));
    }
    request
}

/// The capacity ceiling and the engine version `engine`'s default version publishes, read off
/// that version's own `ServerlessV2FeaturesSupport` report.
async fn published_ceiling(router: &MultiServiceRouter, engine: &str) -> (String, f64) {
    let body = expect_ok(
        engine,
        call(
            router,
            "DescribeDBEngineVersions",
            &[("Engine", engine), ("DefaultOnly", "true")],
        )
        .await,
    );
    let version = text(&body, "EngineVersion");
    let support = subtrees(&body, "ServerlessV2FeaturesSupport")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!(
                "case {engine:?}: the default version must publish its serverless capacity \
                 range, got {body}"
            )
        });
    let ceiling = text(&support, "MaxCapacity");
    let ceiling = ceiling.parse::<f64>().unwrap_or_else(|_| {
        panic!("case {engine:?}: MaxCapacity must be a number, got {ceiling:?}")
    });
    (version, ceiling)
}

/// The whole sentence the AURORA control plane refuses a capacity above `ceiling` with,
/// spelled out HERE rather than taken from the code under test. Its only variable parts are
/// the ceiling and the version; the rejected value is not in it.
fn aurora_sentence(ceiling: f64, engine_version: &str) -> String {
    format!(
        "Invalid scaling configuration. The capacity values greater than {} ACUs are not \
         supported by the current engine version {engine_version}. Please refer to this doc \
         https://docs.aws.amazon.com/AmazonRDS/latest/AuroraUserGuide/\
         aurora-serverless-v2.requirements.html for instructions on fixing the issue.",
        acu(ceiling)
    )
}

/// The whole sentence every other cluster engine refuses a capacity outside its range with,
/// spelled out here as well: the value it would not take, the range it will and the call that
/// changes that range.
fn range_sentence(value: f64, floor: f64, ceiling: f64) -> String {
    format!(
        "Serverless v2 capacity value {} is not valid. The valid scaling range for this \
         cluster is {} to {}. To change the cluster's scaling range, use the ModifyDBCluster \
         call.",
        acu(value),
        acu(floor),
        acu(ceiling)
    )
}

#[tokio::test]
async fn a_capacity_above_the_ceiling_is_worded_by_the_control_plane_the_engine_belongs_to() {
    let router = emulator();
    create_subnet_group(&router).await;

    for row in ENGINES {
        let (version, ceiling) = published_ceiling(&router, row.engine).await;

        // The range the refusals below state is the range the service really configures a
        // cluster in: its floor and its ceiling together are served.
        let mut at_the_bounds: Vec<(String, String)> =
            base_request(row, &format!("{}-bounds", row.prefix))
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect();
        at_the_bounds.push((
            "ServerlessV2ScalingConfiguration.MinCapacity".to_string(),
            acu(row.configured_floor),
        ));
        at_the_bounds.push((
            "ServerlessV2ScalingConfiguration.MaxCapacity".to_string(),
            acu(ceiling),
        ));
        expect_ok(
            &format!("{}: the configurable range itself", row.engine),
            call(&router, "CreateDBCluster", &at_the_bounds).await,
        );

        for (index, (why, value)) in [
            ("one half-step past the ceiling", ceiling + CAPACITY_STEP),
            ("twice the ceiling", ceiling * 2.0),
        ]
        .into_iter()
        .enumerate()
        {
            for member in MEMBERS {
                let case = format!("{}: {why} on {member}", row.engine);
                let identifier = format!("{}-{index}-{}", row.prefix, member.to_lowercase());
                let mut request: Vec<(String, String)> = base_request(row, &identifier)
                    .into_iter()
                    .map(|(name, value)| (name.to_string(), value))
                    .collect();
                // The other member stays inside the range, so the value under test is the
                // only thing the configuration breaks; a floor above the ceiling is its own
                // refusal and is not what this suite is about.
                for carried in MEMBERS {
                    let asked = if carried == member {
                        value
                    } else if carried == "MinCapacity" {
                        row.configured_floor
                    } else {
                        value
                    };
                    request.push((
                        format!("ServerlessV2ScalingConfiguration.{carried}"),
                        acu(asked),
                    ));
                }

                let body = expect_error(
                    &case,
                    call(&router, "CreateDBCluster", &request).await,
                    400,
                    INVALID_PARAMETER_VALUE,
                );
                let message = text(&body, "Message");
                let expected = if row.aurora_wording {
                    aurora_sentence(ceiling, &version)
                } else {
                    range_sentence(value, row.configured_floor, ceiling)
                };
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected the refusal {expected:?}, got {message:?} in \
                     {body}"
                );

                // And the refused create provisioned nothing under that identifier.
                expect_error(
                    &case,
                    call(
                        &router,
                        "DescribeDBClusters",
                        &[("DBClusterIdentifier", identifier.as_str())],
                    )
                    .await,
                    404,
                    "DBClusterNotFoundFault",
                );
            }
        }
    }
}
