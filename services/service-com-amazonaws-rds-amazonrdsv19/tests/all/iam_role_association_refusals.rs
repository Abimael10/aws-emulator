// SPDX-License-Identifier: CC0-1.0

//! What `AddRoleToDBInstance` / `AddRoleToDBCluster` and their disassociations REFUSE, and
//! the code and the sentence each refusal carries.
//!
//! The rules under test, each asserted over a table spanning what it generalises over
//! rather than over the one request that showed it:
//!
//! 1. A member that names an IAM ROLE must carry an IAM role ARN
//!    (`arn:<aws partition>:iam::<account digits>:role/<name>`, the form this service's own
//!    model states as the `IAMRoleArn` pattern on the cluster-side members). A value outside
//!    it names no role at all, so it is refused `InvalidParameterValue` with
//!    `Invalid Role ARN: <value>` for its FORM — before the addressed resource is resolved,
//!    and without changing that resource's associations. The instance-side members are the
//!    ones whose model leaves the form unstated, so they are the ones driven here.
//! 2. A FEATURE of a DB instance's engine acts under ONE role: an association for a feature
//!    name that already has a role is refused `InvalidParameterValue` naming the engine (by
//!    the product name the engine catalogue publishes) and the feature.
//! 3. A DB CLUSTER associates a role ONCE: an association of a role the cluster already
//!    carries, for that feature or another or none, is the modelled
//!    `DBClusterRoleAlreadyExists`, whose sentence names the role ARN and the cluster
//!    identifier as the request spelled them. Another role for the same feature is a
//!    different association and is recorded.
//! 4. A role-association call whose `DBClusterIdentifier` names no cluster answers the
//!    modelled `DBClusterNotFoundFault` with `DBCluster not found: <identifier>` — the
//!    identifier the reference carries, which an ARN form is reduced to.
//! 5. A role whose trust policy does not let `rds.amazonaws.com` assume it is refused
//!    `InvalidParameterValue` with ONE fixed sentence, whichever resource the request
//!    addressed and whichever principal the policy names instead: the sentence carries no
//!    part of the request at all.
//! 6. A DB CLUSTER carries at most the published number of role associations ("IAM roles per
//!    DB cluster"). Every association up to that bound is recorded and reported `ACTIVE`;
//!    the one PAST it is the modelled `DBClusterRoleQuotaExceeded`, whose sentence names the
//!    cluster and the maximum, and records nothing. The bound is on what the cluster holds
//!    right now and is the cluster's own: a disassociation frees a slot, and another cluster
//!    takes the very role the full one refused.
//! 7. An association CHANGES the cluster's configuration, so a cluster that is not
//!    `available` refuses it with the modelled `InvalidDBClusterStateFault` carrying the
//!    sentence that names the STATUS the cluster is in and nothing of the request; the
//!    refusal leaves the associations as they were, and the same call is taken once the
//!    cluster is available.
//!
//! Every call goes over the wire against this service mounted beside the emulated IAM the
//! trust decision resolves against, exactly as the assembled router mounts them, and every
//! assertion reads the XML the emulator answered with. The sentences are spelled out here
//! rather than taken from the code under test, so the table pins the wording.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_rds_amazonrdsv19::state as st;

use crate::db_instances::{call, expect_error, expect_ok, subtrees, text, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{
    associated_roles, available_cluster, available_instance_of, emulator, role,
};

/// The canonical account id every generated emulator presents, so every ARN below is one
/// this emulator could have minted.
const ACCOUNT: &str = "000000000000";

/// The sentence a role Amazon RDS cannot assume is refused with. Entirely FIXED text: it
/// carries neither the role ARN the request named nor the principal its trust policy is
/// missing.
const UNTRUSTED_ROLE: &str = "IAM role ARN value is invalid or does not include the \
                              required permissions for: AWS_ROLE_INTEGRATION";

/// The query parameters of one call.
fn params(pairs: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
    pairs
        .iter()
        .map(|(name, value)| (*name, (*value).to_string()))
        .collect()
}

/// A DB instance of `engine`, created through the shared fixture with the class and
/// storage that engine is offered with, and no `DBName`: what this suite asserts is
/// decided by the ENGINE, which is why its rows span several.
async fn available_instance(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
    class: &str,
    storage: u32,
) {
    available_instance_of(router, identifier, engine, class, storage, None).await;
}

/// The `AssociatedRoles` a DB instance reports.
async fn instance_roles(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, String)> {
    let described = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &params(&[("DBInstanceIdentifier", identifier)]),
        )
        .await,
    );
    associated_roles(&described)
}

/// The `AssociatedRoles` a DB cluster reports.
async fn cluster_roles(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, String)> {
    let described = expect_ok(
        case,
        call(
            router,
            "DescribeDBClusters",
            &params(&[("DBClusterIdentifier", identifier)]),
        )
        .await,
    );
    associated_roles(&described)
}

/// Assert the `Message` of an answered error.
fn expect_message(case: &str, body: &str, expected: &str) {
    let message = text(body, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
}

/// One row of the role-ARN form table: a value for a member that names an IAM role, and
/// whether it is an IAM role ARN.
struct RoleArnRow {
    case: &'static str,
    role_arn: &'static str,
    /// Whether the value carries the form the model's `IAMRoleArn` pattern
    /// (`^arn:aws[a-z-]*:iam::[0-9]*:role/.*$`) states.
    well_formed: bool,
}

/// The forms a `RoleArn` can arrive in, spanning each field the IAM-role-ARN form fixes —
/// the ARN grammar itself, the partition, the vendor, the Region, the account and the
/// resource — on both sides of the rule.
///
/// Source: the `IAMRoleArn` pattern this service's model states for
/// `AddRoleToDBCluster$RoleArn`, read with
/// `smithy_query(query="operation",operation="AddRoleToDBCluster")`.
const ROLE_ARN_ROWS: &[RoleArnRow] = &[
    RoleArnRow {
        case: "a plain name, not an ARN at all",
        role_arn: "not-a-valid-role-arn",
        well_formed: false,
    },
    RoleArnRow {
        case: "fewer than the six fields an ARN carries",
        role_arn: "arn:aws:iam::000000000000",
        well_formed: false,
    },
    RoleArnRow {
        case: "an ARN of another vendor",
        role_arn: "arn:aws:s3:::emu-role-bucket/key",
        well_formed: false,
    },
    RoleArnRow {
        case: "an IAM user rather than a role",
        role_arn: "arn:aws:iam::000000000000:user/emu-user",
        well_formed: false,
    },
    RoleArnRow {
        case: "an IAM policy rather than a role",
        role_arn: "arn:aws:iam::000000000000:policy/emu-policy",
        well_formed: false,
    },
    RoleArnRow {
        case: "a resource that names no role",
        role_arn: "arn:aws:iam::000000000000:role",
        well_formed: false,
    },
    RoleArnRow {
        case: "an IAM ARN carrying a Region",
        role_arn: "arn:aws:iam:us-west-2:000000000000:role/emu-rds-role",
        well_formed: false,
    },
    RoleArnRow {
        case: "an account field that is not digits",
        role_arn: "arn:aws:iam::not-an-account:role/emu-rds-role",
        well_formed: false,
    },
    RoleArnRow {
        case: "a partition outside the aws family",
        role_arn: "arn:azure:iam::000000000000:role/emu-rds-role",
        well_formed: false,
    },
    RoleArnRow {
        case: "the role ARN of this account",
        role_arn: "arn:aws:iam::000000000000:role/emu-rds-role",
        well_formed: true,
    },
    RoleArnRow {
        case: "a role in the China partition",
        role_arn: "arn:aws-cn:iam::000000000000:role/emu-rds-role",
        well_formed: true,
    },
    RoleArnRow {
        case: "a role in the GovCloud partition",
        role_arn: "arn:aws-us-gov:iam::000000000000:role/emu-rds-role",
        well_formed: true,
    },
    RoleArnRow {
        case: "a role under a path",
        role_arn: "arn:aws:iam::000000000000:role/service-role/emu-rds-role",
        well_formed: true,
    },
    RoleArnRow {
        case: "a role whose account field is empty, which the pattern admits",
        role_arn: "arn:aws:iam:::role/emu-rds-role",
        well_formed: true,
    },
    RoleArnRow {
        case: "a role name in another case",
        role_arn: "arn:aws:iam::000000000000:role/EMU-RDS-Role",
        well_formed: true,
    },
];

/// Rule 1: the two instance-side members that name an IAM role refuse a value outside the
/// IAM-role-ARN form for its FORM, naming the value and nothing else, and the instance's
/// associations are untouched by the refusal.
#[tokio::test]
async fn a_role_arn_outside_the_iam_role_form_is_refused_for_its_form() {
    let (router, _engine) = emulator();
    let identifier = "emu-role-arn-form";
    available_instance(&router, identifier, "postgres", "db.t3.micro", 20).await;
    // The role the WELL-FORMED rows name, created in the IAM mounted beside this service and
    // trusting the principal Amazon RDS acts under: the role gate the association takes next
    // resolves the value against that service, so a row that is here to pass the FORM gate has
    // to name a role IAM really holds. IAM holds one role per name whatever case it is written
    // in, which is why the row naming it in another case needs no second role.
    role(&router, "emu-rds-role", st::RDS_SERVICE_PRINCIPAL, None).await;
    for row in ROLE_ARN_ROWS {
        let request = params(&[
            ("DBInstanceIdentifier", identifier),
            ("RoleArn", row.role_arn),
            ("FeatureName", "s3Import"),
        ]);
        if !row.well_formed {
            let expected = format!("Invalid Role ARN: {}", row.role_arn);
            // Both members, because the model leaves the form unstated on both and the
            // reading of "the ARN of an IAM role" is the same one.
            for action in ["AddRoleToDBInstance", "RemoveRoleFromDBInstance"] {
                let case = format!("{action} with {}", row.case);
                let body = expect_error(
                    &case,
                    call(&router, action, &request).await,
                    400,
                    "InvalidParameterValue",
                );
                expect_message(&case, &body, &expected);
            }
            let reported = instance_roles(&router, row.case, identifier).await;
            assert!(
                reported.is_empty(),
                "case {:?}: a refused association records nothing, got {reported:?}",
                row.case
            );
            continue;
        }

        // A well-formed role ARN passes the form gate: the association is recorded, and
        // the disassociation that names the same ARN takes it away again, so the next row
        // starts from the same empty list.
        expect_ok(row.case, call(&router, "AddRoleToDBInstance", &request).await);
        let reported = instance_roles(&router, row.case, identifier).await;
        assert_eq!(
            reported,
            vec![(
                row.role_arn.to_string(),
                "s3Import".to_string(),
                "ACTIVE".to_string()
            )],
            "case {:?}: expected the association to be reported, got {reported:?}",
            row.case
        );
        expect_ok(
            row.case,
            call(&router, "RemoveRoleFromDBInstance", &request).await,
        );
        let reported = instance_roles(&router, row.case, identifier).await;
        assert!(
            reported.is_empty(),
            "case {:?}: expected no association after the removal, got {reported:?}",
            row.case
        );
    }
}

/// One row of the one-ARN-per-feature table: an engine, what it takes to create an
/// instance of it, a feature name that engine publishes, and the name Amazon RDS calls the
/// engine in a sentence.
struct EngineRow {
    case: &'static str,
    engine: &'static str,
    class: &'static str,
    storage: u32,
    feature: &'static str,
    /// The product name the engine catalogue publishes for that `Engine` value, which is
    /// what the refusal names. Source: `DescribeDBEngineVersions`' `DBEngineDescription`
    /// family of names for the engine.
    product: &'static str,
}

/// Three engine families that publish a role feature, each named differently from its
/// `Engine` value, with a feature name that family publishes.
const ENGINE_ROWS: &[EngineRow] = &[
    EngineRow {
        case: "RDS for PostgreSQL imports from S3",
        engine: "postgres",
        class: "db.t3.micro",
        storage: 20,
        feature: "s3Import",
        product: "PostgreSQL",
    },
    EngineRow {
        case: "RDS for PostgreSQL invokes Lambda",
        engine: "postgres",
        class: "db.t3.micro",
        storage: 20,
        feature: "Lambda",
        product: "PostgreSQL",
    },
    EngineRow {
        case: "RDS for Oracle integrates with S3",
        engine: "oracle-se2",
        class: "db.m5.xlarge",
        storage: 40,
        feature: "S3_INTEGRATION",
        product: "Oracle Database Standard Edition Two",
    },
    EngineRow {
        case: "RDS for SQL Server restores a backup",
        engine: "sqlserver-ex",
        class: "db.t3.small",
        storage: 30,
        feature: "SQLSERVER_BACKUP_RESTORE",
        product: "Microsoft SQL Server Express Edition",
    },
];

/// Rule 2: one feature of a DB instance's engine, one role ARN — the second ARN for a
/// feature that already has one is refused for its VALUE, whichever role it names, and the
/// association already in place is the only one the instance reports.
#[tokio::test]
async fn a_db_instance_feature_takes_one_role_arn() {
    let (router, _engine) = emulator();

    for (index, row) in ENGINE_ROWS.iter().enumerate() {
        let identifier = format!("emu-one-arn-{index}");
        available_instance(&router, &identifier, row.engine, row.class, row.storage).await;
        let first = role(
            &router,
            &format!("emu-one-arn-first-{index}"),
            "rds.amazonaws.com",
            None,
        )
        .await;
        let second = role(
            &router,
            &format!("emu-one-arn-second-{index}"),
            "rds.amazonaws.com",
            None,
        )
        .await;
        expect_ok(
            row.case,
            call(
                &router,
                "AddRoleToDBInstance",
                &params(&[
                    ("DBInstanceIdentifier", &identifier),
                    ("RoleArn", &first),
                    ("FeatureName", row.feature),
                ]),
            )
            .await,
        );

        let expected = format!(
            "The engine {} supports only one ARN associated with feature name {}.",
            row.product, row.feature
        );
        // The same role again, and another role: the feature is taken either way, which is
        // what makes the rule one about the feature rather than about the pair.
        for (which, role_arn) in [("the same role again", &first), ("another role", &second)] {
            let case = format!("{} with {which}", row.case);
            let body = expect_error(
                &case,
                call(
                    &router,
                    "AddRoleToDBInstance",
                    &params(&[
                        ("DBInstanceIdentifier", &identifier),
                        ("RoleArn", role_arn),
                        ("FeatureName", row.feature),
                    ]),
                )
                .await,
                400,
                "InvalidParameterValue",
            );
            expect_message(&case, &body, &expected);
        }

        let reported = instance_roles(&router, row.case, &identifier).await;
        assert_eq!(
            reported,
            vec![(
                first.clone(),
                row.feature.to_string(),
                "ACTIVE".to_string()
            )],
            "case {:?}: expected only the first association, got {reported:?}",
            row.case
        );
    }
}

/// Rule 3: a DB cluster associates a role once. The refusal names the role ARN and the
/// cluster identifier the REQUEST spelled, in each form a member that names a cluster
/// accepts; another role for the same feature is a different association.
#[tokio::test]
async fn a_db_cluster_associates_one_role_once() {
    let (router, _engine) = emulator();
    let identifier = "emu-one-role-cluster";
    available_cluster(&router, identifier, "aurora-postgresql").await;
    let first = role(&router, "emu-cluster-first", "rds.amazonaws.com", None).await;
    let second = role(&router, "emu-cluster-second", "rds.amazonaws.com", None).await;

    expect_ok(
        "the first association",
        call(
            &router,
            "AddRoleToDBCluster",
            &params(&[
                ("DBClusterIdentifier", identifier),
                ("RoleArn", &first),
                ("FeatureName", "s3Import"),
            ]),
        )
        .await,
    );

    let cluster_arn = format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{identifier}");
    let upper_cased = identifier.to_ascii_uppercase();
    // (case, the DBClusterIdentifier the request names, the FeatureName it names)
    let rows: Vec<(&str, &str, Option<&str>)> = vec![
        ("the same role and feature", identifier, Some("s3Import")),
        ("the same role for another feature", identifier, Some("s3Export")),
        ("the same role for no feature at all", identifier, None),
        (
            "the cluster named in another case",
            upper_cased.as_str(),
            Some("s3Import"),
        ),
        (
            "the cluster named by its ARN",
            cluster_arn.as_str(),
            Some("s3Import"),
        ),
    ];
    for (case, named, feature) in rows {
        let mut request = params(&[("DBClusterIdentifier", named), ("RoleArn", &first)]);
        if let Some(feature) = feature {
            request.push(("FeatureName", feature.to_string()));
        }
        let body = expect_error(
            case,
            call(&router, "AddRoleToDBCluster", &request).await,
            400,
            "DBClusterRoleAlreadyExists",
        );
        expect_message(
            case,
            &body,
            &format!(
                "Role ARN {first} is already associated with DB Cluster: {named}. Verify your \
                 role ARN and try again."
            ),
        );
    }

    // ANOTHER role for the feature the first one serves is a different association: the
    // rule is about the role, not about the feature, and the cluster then reports both.
    expect_ok(
        "another role for the same feature",
        call(
            &router,
            "AddRoleToDBCluster",
            &params(&[
                ("DBClusterIdentifier", identifier),
                ("RoleArn", &second),
                ("FeatureName", "s3Import"),
            ]),
        )
        .await,
    );
    let reported = cluster_roles(&router, "both associations", identifier).await;
    assert_eq!(
        reported,
        vec![
            (first.clone(), "s3Import".to_string(), "ACTIVE".to_string()),
            (second.clone(), "s3Import".to_string(), "ACTIVE".to_string()),
        ],
        "expected both associations to be reported, got {reported:?}"
    );
}

/// Rule 4: the role-association calls word a `DBClusterIdentifier` that names no cluster
/// as a plain lookup miss, naming the identifier the request's reference carried.
#[tokio::test]
async fn a_role_association_call_words_a_missing_db_cluster_as_a_lookup_miss() {
    let (router, _engine) = emulator();
    // A cluster that IS stored, so none of the refusals below comes from an empty store.
    available_cluster(&router, "emu-present-cluster", "aurora-postgresql").await;
    let role_arn = role(&router, "emu-absent-cluster-role", "rds.amazonaws.com", None).await;

    let absent = "emu-absent-cluster";
    let upper_cased = absent.to_ascii_uppercase();
    let cluster_arn = format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{absent}");
    // The three forms a member that names a DB cluster accepts, all naming nothing, each
    // paired with the IDENTIFIER the fault reports for it: an ARN names its own identifier.
    let forms: Vec<(&str, &str, &str)> = vec![
        ("the identifier as written", absent, absent),
        (
            "the same identifier upper-cased",
            upper_cased.as_str(),
            upper_cased.as_str(),
        ),
        ("the cluster's ARN form", cluster_arn.as_str(), absent),
    ];
    for action in ["AddRoleToDBCluster", "RemoveRoleFromDBCluster"] {
        for (form, named, reported) in &forms {
            let case = format!("{action} with {form}");
            let body = expect_error(
                &case,
                call(
                    &router,
                    action,
                    &params(&[
                        ("DBClusterIdentifier", named),
                        ("RoleArn", &role_arn),
                        ("FeatureName", "s3Import"),
                    ]),
                )
                .await,
                404,
                "DBClusterNotFoundFault",
            );
            expect_message(&case, &body, &format!("DBCluster not found: {reported}"));
        }
    }
}

/// Rule 5: a role Amazon RDS cannot assume is refused with ONE fixed sentence — the same
/// text for every principal the trust policy names instead and for both resources — and
/// nothing is recorded.
#[tokio::test]
async fn a_role_rds_cannot_assume_is_refused_with_one_fixed_sentence() {
    let (router, _engine) = emulator();
    let instance = "emu-untrusted-instance";
    let cluster = "emu-untrusted-cluster";
    available_instance(&router, instance, "postgres", "db.t3.micro", 20).await;
    available_cluster(&router, cluster, "aurora-postgresql").await;

    // The principals a trust policy can name instead of the one RDS acts under, including
    // the EXPORT principal, which is Amazon RDS's own and still not this one.
    let principals = [
        "ec2.amazonaws.com",
        "lambda.amazonaws.com",
        "export.rds.amazonaws.com",
        "s3.amazonaws.com",
    ];
    for (index, principal) in principals.iter().enumerate() {
        let role_arn = role(
            &router,
            &format!("emu-untrusted-{index}"),
            principal,
            None,
        )
        .await;
        for (action, member, resource) in [
            ("AddRoleToDBInstance", "DBInstanceIdentifier", instance),
            ("AddRoleToDBCluster", "DBClusterIdentifier", cluster),
        ] {
            let case = format!("{action} with a role trusting {principal}");
            let body = expect_error(
                &case,
                call(
                    &router,
                    action,
                    &params(&[
                        (member, resource),
                        ("RoleArn", &role_arn),
                        ("FeatureName", "s3Import"),
                    ]),
                )
                .await,
                400,
                "InvalidParameterValue",
            );
            expect_message(&case, &body, UNTRUSTED_ROLE);
        }
    }

    let reported = instance_roles(&router, "the untrusted instance", instance).await;
    assert!(
        reported.is_empty(),
        "expected no association on the instance, got {reported:?}"
    );
    let reported = cluster_roles(&router, "the untrusted cluster", cluster).await;
    assert!(
        reported.is_empty(),
        "expected no association on the cluster, got {reported:?}"
    );
}

/// How many IAM roles one DB cluster may have associated with it at once: the "IAM roles per
/// DB cluster" quota Amazon RDS publishes (Quotas and constraints for Amazon RDS), which is
/// what the modelled quota fault reports having reached. Stated here rather than read from
/// the code under test, so the table pins the bound as well as the wording.
const ROLES_PER_CLUSTER: usize = 5;

/// The engine the clusters of rules 6 and 7 run. Aurora PostgreSQL publishes the most
/// `SupportedFeatureNames` of any engine in the catalogue — `Bedrock`, `Comprehend`,
/// `Lambda`, `SageMaker`, `s3Export`, `s3Import`, the list
/// `DBEngineVersion$SupportedFeatureNames` reports for it through
/// `DescribeDBEngineVersions` — so a table of DISTINCT feature names fits inside the quota.
const QUOTA_ENGINE: &str = "aurora-postgresql";

/// The status an association needs the cluster to be in.
const CLUSTER_AVAILABLE: &str = "available";

/// `CreateDBCluster` for one identifier, of the engine the rows below run.
async fn create_cluster(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "CreateDBCluster",
        &params(&[
            ("DBClusterIdentifier", identifier),
            ("Engine", QUOTA_ENGINE),
        ]),
    )
    .await
}

/// The one `<DBCluster>` subtree a report carries for `identifier`.
fn the_cluster(case: &str, body: &str) -> String {
    let reported = subtrees(body, "DBCluster");
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: exactly one DBCluster must be reported, got {body}"
    );
    reported.into_iter().next().expect("checked just above")
}

/// Poll `DescribeDBClusters` until the cluster reports `status`: the CONDITION the rows below
/// wait for, through a REPORTING call and nothing else, rather than for a fixed span.
async fn poll_cluster_until(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    status: &str,
) {
    let described = params(&[("DBClusterIdentifier", identifier)]);
    poll_until(
        case,
        &format!("the DB cluster {identifier} reaching {status}"),
        || call(router, "DescribeDBClusters", &described),
        |http_status, body| {
            http_status == 200 && text(&the_cluster(case, body), "Status") == status
        },
    )
    .await;
}

/// Create a DB cluster and poll until it reports `available`, which is the status an
/// association needs.
async fn polled_available_cluster(router: &MultiServiceRouter, identifier: &str) {
    expect_ok(identifier, create_cluster(router, identifier).await);
    poll_cluster_until(router, identifier, identifier, CLUSTER_AVAILABLE).await;
}

/// The `Status` a cluster reports.
async fn cluster_status(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let described = expect_ok(
        case,
        call(
            router,
            "DescribeDBClusters",
            &params(&[("DBClusterIdentifier", identifier)]),
        )
        .await,
    );
    text(&the_cluster(case, &described), "Status")
}

/// `AddRoleToDBCluster` / `RemoveRoleFromDBCluster` for one (cluster, role, feature), with
/// `FeatureName` left OUT of the request where the row names none — the member is optional
/// on both cluster-side operations.
async fn cluster_role_call(
    router: &MultiServiceRouter,
    action: &str,
    identifier: &str,
    role_arn: &str,
    feature: Option<&str>,
) -> (u16, String) {
    let mut request = params(&[
        ("DBClusterIdentifier", identifier),
        ("RoleArn", role_arn),
    ]);
    if let Some(feature) = feature {
        request.push(("FeatureName", feature.to_string()));
    }
    call(router, action, &request).await
}

/// One row of the quota table: the role an association names and the feature it names it for.
struct QuotaRow {
    case: &'static str,
    /// The role NAME, created in IAM trusting `rds.amazonaws.com`: an association names a
    /// role Amazon RDS can take on, which is the gate the association is taken past first
    /// (rule 5), so every row of the quota table names such a role.
    name: &'static str,
    /// The `FeatureName` the association names, or `None` for the cluster-wide association
    /// that names no feature.
    feature: Option<&'static str>,
}

/// The quota table: [`ROLES_PER_CLUSTER`] associations that FIT, then one PAST the bound.
///
/// The rows differ along what the rule generalises over — five of the engine's published
/// feature names and an association that names none — because the quota counts the
/// associations a cluster holds whatever they name.
const QUOTA_ROWS: &[QuotaRow] = &[
    QuotaRow {
        case: "a role IAM holds, for the Bedrock feature",
        name: "emu-quota-bedrock",
        feature: Some("Bedrock"),
    },
    QuotaRow {
        case: "a role IAM holds, associated with the cluster itself",
        name: "emu-quota-cluster-wide",
        feature: None,
    },
    QuotaRow {
        case: "a role IAM holds, for the S3 import feature",
        name: "emu-quota-import",
        feature: Some("s3Import"),
    },
    QuotaRow {
        case: "a role IAM holds, for the S3 export feature",
        name: "emu-quota-export",
        feature: Some("s3Export"),
    },
    QuotaRow {
        case: "a role IAM holds, for the SageMaker feature",
        name: "emu-quota-sagemaker",
        feature: Some("SageMaker"),
    },
    QuotaRow {
        case: "one association past the published quota, for the Comprehend feature",
        name: "emu-quota-comprehend",
        feature: Some("Comprehend"),
    },
];

/// The `RoleArn` each quota row names, created in the IAM mounted beside this service.
async fn quota_role_arns(router: &MultiServiceRouter) -> Vec<String> {
    let mut arns = Vec::with_capacity(QUOTA_ROWS.len());
    for row in QUOTA_ROWS {
        arns.push(role(router, row.name, st::RDS_SERVICE_PRINCIPAL, None).await);
    }
    arns
}

/// Rule 6: a DB cluster takes the published number of role associations and refuses the one
/// past it with the modelled quota fault, whose sentence names the cluster and the maximum.
#[tokio::test]
async fn a_db_cluster_takes_the_published_number_of_role_associations() {
    assert_eq!(
        QUOTA_ROWS.len(),
        ROLES_PER_CLUSTER + 1,
        "the table must fill the quota and then name one association past it, got {} row(s)",
        QUOTA_ROWS.len()
    );
    let (router, _engine) = emulator();
    // Both clusters are created before either is waited for, so the spans they owe run
    // together; the second one is what shows the quota is the CLUSTER's own.
    let full = "emu-cluster-role-quota";
    let other = "emu-cluster-role-quota-second";
    expect_ok(full, create_cluster(&router, full).await);
    expect_ok(other, create_cluster(&router, other).await);
    poll_cluster_until(&router, full, full, CLUSTER_AVAILABLE).await;
    poll_cluster_until(&router, other, other, CLUSTER_AVAILABLE).await;
    let arns = quota_role_arns(&router).await;

    // Every association up to the published bound is taken and reported ACTIVE, the last of
    // them AT the bound.
    for (index, row) in QUOTA_ROWS.iter().take(ROLES_PER_CLUSTER).enumerate() {
        expect_ok(
            row.case,
            cluster_role_call(&router, "AddRoleToDBCluster", full, &arns[index], row.feature).await,
        );
        let reported = cluster_roles(&router, row.case, full).await;
        assert_eq!(
            reported.len(),
            index + 1,
            "case {:?}: expected {} association(s) after the add, got {reported:?}",
            row.case,
            index + 1
        );
        let expected = (
            arns[index].clone(),
            row.feature.unwrap_or("").to_string(),
            "ACTIVE".to_string(),
        );
        assert!(
            reported.contains(&expected),
            "case {:?}: expected the association {expected:?} to be reported, got {reported:?}",
            row.case
        );
    }

    // One past the bound: the modelled quota fault, naming the cluster and the maximum.
    let past = &QUOTA_ROWS[ROLES_PER_CLUSTER];
    let past_arn = &arns[ROLES_PER_CLUSTER];
    let body = expect_error(
        past.case,
        cluster_role_call(&router, "AddRoleToDBCluster", full, past_arn, past.feature).await,
        400,
        "DBClusterRoleQuotaExceeded",
    );
    expect_message(
        past.case,
        &body,
        &format!(
            "The DB cluster {full} already has {ROLES_PER_CLUSTER} IAM roles associated with \
             it, which is the maximum."
        ),
    );

    // The refusal recorded nothing: the cluster still reports exactly the associations it
    // held, and the role it refused is none of them.
    let after = cluster_roles(&router, past.case, full).await;
    assert_eq!(
        after.len(),
        ROLES_PER_CLUSTER,
        "case {:?}: expected the refused association to record nothing, got {after:?}",
        past.case
    );
    assert!(
        !after.iter().any(|(arn, _, _)| arn == past_arn),
        "case {:?}: the refused role {past_arn:?} must not be reported, got {after:?}",
        past.case
    );

    // The bound is on what the cluster holds RIGHT NOW: dropping one association frees the
    // slot, and the very role the quota refused is then taken.
    let freed = &QUOTA_ROWS[2];
    expect_ok(
        freed.case,
        cluster_role_call(
            &router,
            "RemoveRoleFromDBCluster",
            full,
            &arns[2],
            freed.feature,
        )
        .await,
    );
    let remaining = cluster_roles(&router, freed.case, full).await;
    assert_eq!(
        remaining.len(),
        ROLES_PER_CLUSTER - 1,
        "case {:?}: expected the disassociation to free a slot, got {remaining:?}",
        freed.case
    );
    expect_ok(
        past.case,
        cluster_role_call(&router, "AddRoleToDBCluster", full, past_arn, past.feature).await,
    );
    let refilled = cluster_roles(&router, past.case, full).await;
    assert_eq!(
        refilled.len(),
        ROLES_PER_CLUSTER,
        "case {:?}: expected the freed slot to be filled, got {refilled:?}",
        past.case
    );
    assert!(
        refilled.iter().any(|(arn, _, _)| arn == past_arn),
        "case {:?}: expected the role {past_arn:?} to be reported now a slot is free, got \
         {refilled:?}",
        past.case
    );

    // And the quota is the CLUSTER's own: another cluster takes a role the full one is
    // already at its maximum with, and the full cluster is untouched by it.
    let second_case = "another cluster while the first one is full";
    expect_ok(
        second_case,
        cluster_role_call(
            &router,
            "AddRoleToDBCluster",
            other,
            &arns[0],
            QUOTA_ROWS[0].feature,
        )
        .await,
    );
    let second_roles = cluster_roles(&router, second_case, other).await;
    assert_eq!(
        second_roles.len(),
        1,
        "case {second_case:?}: expected the second cluster to report its one association, got \
         {second_roles:?}"
    );
    let unchanged = cluster_roles(&router, second_case, full).await;
    assert_eq!(
        unchanged, refilled,
        "case {second_case:?}: expected the first cluster's associations to be untouched, got \
         {unchanged:?}"
    );
}

/// Assert one state refusal: the modelled fault, the sentence that names the STATUS and
/// nothing of the request, and that the cluster's associations are left as they were.
async fn expect_state_refusal(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    role_arn: &str,
    status: &str,
    held: &[(String, String, String)],
) {
    // The refused call is made FIRST, with nothing read in between, because the status a
    // cluster is in while the clock still owes it a transition is the status of a MOMENT.
    let body = expect_error(
        case,
        cluster_role_call(
            router,
            "AddRoleToDBCluster",
            identifier,
            role_arn,
            Some("Lambda"),
        )
        .await,
        400,
        "InvalidDBClusterStateFault",
    );
    expect_message(
        case,
        &body,
        &format!("DB cluster isn't available for modification with status {status}."),
    );
    let message = text(&body, "Message");
    assert!(
        !message.contains(identifier) && !message.contains(role_arn),
        "case {case:?}: the refusal must name neither the cluster nor the role, got {message:?}"
    );
    let after = cluster_roles(router, case, identifier).await;
    assert_eq!(
        after, held,
        "case {case:?}: expected the refused association to record nothing, got {after:?}"
    );
}

/// Rule 7: a cluster that is not `available` refuses an association with the modelled state
/// fault naming its status, records nothing, and takes the same call once it is available.
#[tokio::test]
async fn an_association_is_refused_while_the_cluster_is_not_available() {
    let (router, _engine) = emulator();
    let role_arn = role(&router, "emu-cluster-state-role", "rds.amazonaws.com", None).await;

    // A status the clock still owes a transition out of: the association arrives inside the
    // span `CreateDBCluster` owes, so the cluster is still coming up.
    let coming_up = "emu-cluster-state-creating";
    expect_ok(coming_up, create_cluster(&router, coming_up).await);
    let status = cluster_status(&router, coming_up, coming_up).await;
    assert_ne!(
        status, CLUSTER_AVAILABLE,
        "case {coming_up:?}: the cluster must still be coming up when the association arrives"
    );
    expect_state_refusal(&router, coming_up, coming_up, &role_arn, &status, &[]).await;

    // The refusal was about the STATE: once the clock has settled the cluster, the very same
    // call is taken and the association is reported.
    poll_cluster_until(&router, coming_up, coming_up, CLUSTER_AVAILABLE).await;
    expect_ok(
        coming_up,
        cluster_role_call(
            &router,
            "AddRoleToDBCluster",
            coming_up,
            &role_arn,
            Some("Lambda"),
        )
        .await,
    );
    let reported = cluster_roles(&router, coming_up, coming_up).await;
    assert_eq!(
        reported,
        vec![(
            role_arn.clone(),
            "Lambda".to_string(),
            "ACTIVE".to_string()
        )],
        "case {coming_up:?}: expected the association to be recorded once the cluster is \
         available, got {reported:?}"
    );

    // And a SETTLED status that is not `available`: a cluster whose stop has finished.
    let stopped = "emu-cluster-state-stopped";
    polled_available_cluster(&router, stopped).await;
    expect_ok(
        stopped,
        call(
            &router,
            "StopDBCluster",
            &params(&[("DBClusterIdentifier", stopped)]),
        )
        .await,
    );
    poll_cluster_until(&router, stopped, stopped, "stopped").await;
    expect_state_refusal(&router, stopped, stopped, &role_arn, "stopped", &[]).await;
}
