// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DBInstance$OptionGroupMemberships[].Status`.
//!
//! RULE: the status reports whether the option group a DB instance runs under has been
//! APPLIED to it (`smithy_query(query="doc",shape="OptionGroupMembership",member="Status")`:
//! "Valid values are: `in-sync`, `pending-apply`, `pending-removal`, …"). RDS applies the
//! options of a group the ACCOUNT created as part of provisioning the instance, so the call
//! that provisions it answers `pending-apply` and the attachment is owed from that same
//! instant, which makes every later read report `in-sync`; a group RDS provides itself holds
//! no option to apply, so its membership is `in-sync` from the start.
//!
//! A client that waits for an option group to leave `pending-apply` can only distinguish
//! "applied" from "never pending" if the provisioning call reports the pending membership at
//! all — and one that polls a default group must not wait for a transition that is not owed.
//!
//! The rows span what the rule generalises over: the engine families whose default option
//! group differs, a default group against a group the account created, and both calls that
//! provision a DB instance holding an option group (`CreateDBInstance` and
//! `CreateDBInstanceReadReplica`). Every call goes over the wire through the one mounting
//! helper this test target has, and every assertion reads the XML the emulator answered
//! with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_ok, subtrees, text, the_instance};
use crate::lifecycle_poll_loops::{poll_until, read_instance};

/// The class every instance below is created with: the subject here is the option-group
/// membership, which no compute class changes.
const CLASS: &str = "db.t3.medium";

/// The status a membership reports while this service still owes the attachment.
const PENDING_APPLY: &str = "pending-apply";

/// The status a membership reports once the group is attached.
const IN_SYNC: &str = "in-sync";

/// One row of the provisioning table.
struct Row {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// Everything the create names beyond the three required members.
    extra: &'static [(&'static str, &'static str)],
    /// The option group the account creates first, as
    /// `(OptionGroupName, EngineName, MajorEngineVersion)`, for a row that names one of its
    /// own rather than taking the engine version's default group.
    account_group: Option<(&'static str, &'static str, &'static str)>,
    /// The status the create's OWN answer must report: the group is still being applied
    /// where the account created it, and applied already where RDS provides it.
    created_status: &'static str,
}

/// The `(OptionGroupName, Status)` of the one option-group membership an envelope reports.
fn membership(case: &str, envelope: &str, instance: &str) -> (String, String) {
    let found = subtrees(instance, "OptionGroupMembership");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: the {envelope} envelope must report exactly one \
         OptionGroupMembership, got {found:?}"
    );
    let membership = found.into_iter().next().expect("checked just above");
    (
        text(&membership, "OptionGroupName"),
        text(&membership, "Status"),
    )
}

/// Assert the membership an envelope reports: its group and the status of the attachment.
fn assert_membership(
    case: &str,
    envelope: &str,
    instance: &str,
    expected_group: &str,
    expected_status: &str,
) {
    let (group, status) = membership(case, envelope, instance);
    assert_eq!(
        group, expected_group,
        "case {case:?}: the {envelope} envelope must report the option group \
         {expected_group:?}, got {group:?}"
    );
    assert_eq!(
        status, expected_status,
        "case {case:?}: the {envelope} envelope must report the membership as \
         {expected_status:?}, got {status:?}"
    );
}

/// `CreateOptionGroup` for a group the account owns.
async fn create_option_group(
    router: &MultiServiceRouter,
    case: &str,
    group: (&str, &str, &str),
) {
    let (name, engine_name, major_version) = group;
    expect_ok(
        case,
        call(
            router,
            "CreateOptionGroup",
            &[
                ("OptionGroupName", name.to_string()),
                ("EngineName", engine_name.to_string()),
                ("MajorEngineVersion", major_version.to_string()),
                (
                    "OptionGroupDescription",
                    "the group the instance is provisioned with".to_string(),
                ),
            ],
        )
        .await,
    );
}

/// `CreateDBInstance` with its three required members plus `extra`, as the `<DBInstance>`
/// the create envelope carries.
async fn create_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
    extra: &[(&str, &str)],
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
        ("DBInstanceClass", CLASS.to_string()),
    ];
    params.extend(
        extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    let body = expect_ok(case, call(router, "CreateDBInstance", &params).await);
    the_instance(case, &body)
}

/// `DescribeDBInstances` for one identifier, as the one `<DBInstance>` it reports.
async fn describe_instance(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let (status, body) = read_instance(router, identifier).await;
    the_instance(case, &expect_ok(case, (status, body)))
}

/// The `<DBInstance>` the instance reports once it has reached `available`, which is the
/// state a client waits for before using it.
async fn available_instance(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let (body, _polls) = poll_until(
        case,
        &format!("{identifier} reaching available"),
        || read_instance(router, identifier),
        |status, body| {
            status == 200 && text(&the_instance(case, body), "DBInstanceStatus") == "available"
        },
    )
    .await;
    the_instance(case, &body)
}

/// RULE: the create answers with the attachment of a group the account created still
/// outstanding and with a group RDS provides itself already applied, and every read after it
/// reports the group applied.
#[tokio::test]
async fn a_create_reports_whether_its_option_group_has_been_applied_yet() {
    let router = emulator();

    let rows: &[Row] = &[
        Row {
            case: "mysql, the default option group of the version it resolved",
            identifier: "emu-optattach-mysql",
            engine: "mysql",
            extra: &[],
            account_group: None,
            created_status: IN_SYNC,
        },
        Row {
            case: "postgres, a major version named, so a default option group again",
            identifier: "emu-optattach-postgres",
            engine: "postgres",
            extra: &[("EngineVersion", "16")],
            account_group: None,
            created_status: IN_SYNC,
        },
        Row {
            case: "mysql, an option group the account created",
            identifier: "emu-optattach-named-mysql",
            engine: "mysql",
            extra: &[
                ("EngineVersion", "8.0.40"),
                ("OptionGroupName", "emu-optattach-mysql-group"),
            ],
            account_group: Some(("emu-optattach-mysql-group", "mysql", "8.0")),
            created_status: PENDING_APPLY,
        },
        Row {
            case: "oracle-se2, an option group the account created",
            identifier: "emu-optattach-named-oracle",
            engine: "oracle-se2",
            extra: &[("OptionGroupName", "emu-optattach-oracle-group")],
            account_group: Some(("emu-optattach-oracle-group", "oracle-se2", "19")),
            created_status: PENDING_APPLY,
        },
    ];

    for row in rows {
        if let Some(group) = row.account_group {
            create_option_group(&router, row.case, group).await;
        }

        let created =
            create_instance(&router, row.case, row.identifier, row.engine, row.extra).await;
        // The group the instance is provisioned with, as its own envelope names it: the one
        // the request named, else the default group of the version it resolved.
        let (group, status) = membership(row.case, "create", &created);
        assert_eq!(
            status, row.created_status,
            "case {:?}: the create envelope must report the membership of {group:?} as \
             {:?}, got {status:?}",
            row.case, row.created_status
        );
        if let Some((named, ..)) = row.account_group {
            assert_eq!(
                group, named,
                "case {:?}: an instance reports the option group its request named, got \
                 {group:?}",
                row.case
            );
        }

        // Read back straight after the create, with no other call in between: an
        // attachment that was outstanding was owed from the instant the create was
        // accepted, so it is applied by the time anything reads the instance.
        let described = describe_instance(&router, row.case, row.identifier).await;
        assert_membership(row.case, "describe", &described, &group, IN_SYNC);

        // And it stays applied through the rest of the instance's life: once it is up, and
        // in the envelope the delete answers with.
        let available = available_instance(&router, row.case, row.identifier).await;
        assert_membership(row.case, "available describe", &available, &group, IN_SYNC);

        let deleted = expect_ok(
            row.case,
            call(
                &router,
                "DeleteDBInstance",
                &[
                    ("DBInstanceIdentifier", row.identifier.to_string()),
                    ("SkipFinalSnapshot", "true".to_string()),
                ],
            )
            .await,
        );
        assert_membership(
            row.case,
            "delete",
            &the_instance(row.case, &deleted),
            &group,
            IN_SYNC,
        );
    }
}

/// RULE, on the other call that provisions an instance holding an option group: a read
/// replica is provisioned the same way, so its own create answers with the attachment of the
/// group it inherits outstanding and the read back reports it applied.
#[tokio::test]
async fn a_read_replica_reports_its_option_group_attachment_the_same_way() {
    let router = emulator();
    let case = "a read replica of a mysql instance in an option group the account created";
    let source = "emu-optattach-source";
    let replica = "emu-optattach-replica";
    let group = "emu-optattach-source-group";

    create_option_group(&router, case, (group, "mysql", "8.0")).await;
    create_instance(
        &router,
        case,
        source,
        "mysql",
        &[("EngineVersion", "8.0.40"), ("OptionGroupName", group)],
    )
    .await;
    // A replica can only be created from an instance that is up, so the source's own
    // provisioning is waited out through a poll loop on a reporting call.
    let available_source = available_instance(&router, case, source).await;
    // The source's own attachment settled long before this replica existed.
    assert_membership(case, "available source", &available_source, group, IN_SYNC);

    let created = expect_ok(
        case,
        call(
            &router,
            "CreateDBInstanceReadReplica",
            &[
                ("DBInstanceIdentifier", replica.to_string()),
                ("SourceDBInstanceIdentifier", source.to_string()),
            ],
        )
        .await,
    );
    assert_membership(
        case,
        "create replica",
        &the_instance(case, &created),
        group,
        PENDING_APPLY,
    );

    let described = describe_instance(&router, case, replica).await;
    assert_membership(case, "describe replica", &described, group, IN_SYNC);
    // And nothing the replica's creation did moved the source's membership back.
    let source_read = describe_instance(&router, case, source).await;
    assert_membership(case, "describe source", &source_read, group, IN_SYNC);
}
