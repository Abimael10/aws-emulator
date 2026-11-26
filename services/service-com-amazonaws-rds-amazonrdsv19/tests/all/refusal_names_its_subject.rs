// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals that name the resource they are ABOUT: the protected
//! global database a delete will not remove, and the source instance a point-in-time restore
//! cannot find.
//!
//! RULE: a refusal carries the sentence Amazon RDS words that error class with ON THAT
//! OPERATION, and its variable parts come from the request. Where the operation addresses, or
//! names, more than one resource, the sentence says WHICH one it is about:
//!
//! * `DeleteGlobalCluster` on a global database whose deletion protection is on names the
//!   global cluster the request asked to delete — "Cannot delete protected Global Cluster
//!   <GlobalClusterIdentifier>, please disable deletion protection and try again." — so a
//!   teardown that deletes several of them can attribute the refusal and turn the right
//!   setting off. (The DB cluster and DB instance deletes word their own protection with the
//!   resource KIND alone, which the sibling `refusal_class_by_cause` suite pins: each
//!   operation carries ITS own recorded sentence.)
//! * `RestoreDBInstanceToPointInTime` names both a SOURCE instance and the TARGET it creates,
//!   so a source that does not exist is reported as the source — "The source instance could
//!   not be found: <id>" — and not with the not-found template the reports of an instance
//!   carry.
//!
//! Each rule is asserted over a TABLE spanning what it generalises over: several identifiers
//! of different shapes (so nothing but the request's own value can be in the sentence), both
//! members that name a PITR source, and — as the positive control of each — the same call once
//! the condition is gone.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_rds_amazonrdsv19::state as st;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};

/// The engine a global database under test runs, one of the two
/// `CreateGlobalClusterMessage$Engine` publishes.
const GLOBAL_ENGINE: &str = "aurora-postgresql";

/// The identifiers the global-database rows use, spanning the shapes the member admits: a
/// plain name, one carrying digits and hyphens, and a single letter.
const GLOBAL_IDENTIFIERS: [&str; 3] = ["emu-wording-global", "emu-wording-global-2026-7", "g"];

/// The identifiers the point-in-time rows name as a source that does not exist, in the same
/// spread of shapes.
const ABSENT_SOURCES: [&str; 3] = ["emu-wording-no-such-source", "emu-wording-src-2026-7", "s"];

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Create a global database, with deletion protection on or off.
async fn create_global_cluster(
    router: &MultiServiceRouter,
    identifier: &str,
    protected: bool,
) -> (u16, String) {
    call(
        router,
        "CreateGlobalCluster",
        &params![
            "GlobalClusterIdentifier" => identifier,
            "Engine" => GLOBAL_ENGINE,
            "DeletionProtection" => protected,
        ],
    )
    .await
}

/// Rule: the deletion-protection refusal of a global database names the global cluster the
/// request asked to delete, and the global database is still there afterwards.
#[tokio::test]
async fn the_protected_global_cluster_refusal_names_the_global_cluster() {
    let router = emulator();

    for identifier in GLOBAL_IDENTIFIERS {
        let case = format!("the protected global database {identifier}");
        expect_ok(&case, create_global_cluster(&router, identifier, true).await);

        let body = expect_error(
            &case,
            call(
                &router,
                "DeleteGlobalCluster",
                &params!["GlobalClusterIdentifier" => identifier],
            )
            .await,
            400,
            "InvalidParameterCombination",
        );
        let message = text(&body, "Message");
        let sentence = st::deletion_protected_global_cluster_message(identifier);
        assert_eq!(
            message, sentence,
            "case {case:?}: expected the sentence {sentence:?}, got {message:?}"
        );

        // The refused delete removed nothing: the global database is still reported.
        let reported = expect_ok(
            &case,
            call(
                &router,
                "DescribeGlobalClusters",
                &params!["GlobalClusterIdentifier" => identifier],
            )
            .await,
        );
        let answered = text(&reported, "GlobalClusterIdentifier");
        assert_eq!(
            answered, identifier,
            "case {case:?}: expected the refused delete to leave the global database in \
             place, got {answered:?}"
        );
    }
}

/// Rule, the positive control: the sentence is about the SETTING, so a global database whose
/// deletion protection is off is deleted, and one whose protection is turned off by
/// `ModifyGlobalCluster` is deleted afterwards too.
#[tokio::test]
async fn a_global_cluster_that_is_not_protected_is_deleted() {
    const CASE: &str = "an unprotected global database";
    let router = emulator();

    expect_ok(
        CASE,
        create_global_cluster(&router, "emu-wording-unprotected", false).await,
    );
    expect_ok(
        CASE,
        call(
            &router,
            "DeleteGlobalCluster",
            &params!["GlobalClusterIdentifier" => "emu-wording-unprotected"],
        )
        .await,
    );

    const TURNED_OFF: &str = "a global database whose protection is turned off";
    expect_ok(
        TURNED_OFF,
        create_global_cluster(&router, "emu-wording-relaxed", true).await,
    );
    expect_ok(
        TURNED_OFF,
        call(
            &router,
            "ModifyGlobalCluster",
            &params![
                "GlobalClusterIdentifier" => "emu-wording-relaxed",
                "DeletionProtection" => "false",
            ],
        )
        .await,
    );
    expect_ok(
        TURNED_OFF,
        call(
            &router,
            "DeleteGlobalCluster",
            &params!["GlobalClusterIdentifier" => "emu-wording-relaxed"],
        )
        .await,
    );
}

/// Rule: a point-in-time restore whose SOURCE does not exist reports the source, whichever of
/// the two members named it, and creates no instance.
#[tokio::test]
async fn the_point_in_time_restore_reports_the_source_it_could_not_find() {
    let router = emulator();

    for (index, named) in ABSENT_SOURCES.into_iter().enumerate() {
        // The two members that name a source: the identifier of an instance and the resource
        // id of one. Exactly one of them may be given.
        for (member, target) in [
            ("SourceDBInstanceIdentifier", format!("emu-wording-pit-id-{index}")),
            ("SourceDbiResourceId", format!("emu-wording-pit-res-{index}")),
        ] {
            let case = format!("{member} naming {named}");
            let body = expect_error(
                &case,
                call(
                    &router,
                    "RestoreDBInstanceToPointInTime",
                    &params![
                        member => named,
                        "TargetDBInstanceIdentifier" => target,
                        "UseLatestRestorableTime" => "true",
                    ],
                )
                .await,
                404,
                "DBInstanceNotFound",
            );
            let message = text(&body, "Message");
            let sentence = st::restore_source_db_instance_not_found_message(named);
            assert_eq!(
                message, sentence,
                "case {case:?}: expected the sentence {sentence:?}, got {message:?}"
            );

            // Nothing was created for a restore that found no source.
            expect_error(
                &case,
                call(
                    &router,
                    "DescribeDBInstances",
                    &params!["DBInstanceIdentifier" => target],
                )
                .await,
                404,
                "DBInstanceNotFound",
            );
        }
    }
}

/// Rule, the positive control: the sentence is about the SOURCE being absent, so a restore
/// naming a source this Region DOES hold is answered by that instance's own state instead —
/// here the modelled fault for a source with no restorable window — and never by the
/// source-not-found sentence.
#[tokio::test]
async fn a_source_this_region_holds_is_not_reported_as_absent() {
    const CASE: &str = "a source instance with automated backups off";
    let router = emulator();

    expect_ok(
        CASE,
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-wording-pit-source",
                "DBInstanceClass" => "db.t3.micro",
                "Engine" => "postgres",
                "MasterUsername" => "emuadmin",
                "MasterUserPassword" => "emu-Secret-99",
                "AllocatedStorage" => "20",
                "BackupRetentionPeriod" => "0",
            ],
        )
        .await,
    );

    let body = call(
        &router,
        "RestoreDBInstanceToPointInTime",
        &params![
            "SourceDBInstanceIdentifier" => "emu-wording-pit-source",
            "TargetDBInstanceIdentifier" => "emu-wording-pit-target",
            "UseLatestRestorableTime" => "true",
        ],
    )
    .await;
    let refused = expect_error(CASE, body, 400, "PointInTimeRestoreNotEnabled");
    let message = text(&refused, "Message");
    assert!(
        !message.contains("could not be found"),
        "case {CASE:?}: expected the refusal to be about the restorable window rather than \
         the source being absent, got {message:?}"
    );
}
