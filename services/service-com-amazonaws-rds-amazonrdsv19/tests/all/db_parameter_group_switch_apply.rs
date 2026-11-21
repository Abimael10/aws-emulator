// SPDX-License-Identifier: CC0-1.0

//! The DB parameter group APPLY lifecycle a `ModifyDBInstance` that switches groups starts.
//!
//! The rule under test: `ModifyDBInstance$DBParameterGroupName` — "the parameter group name
//! itself is changed immediately, but the actual parameter changes are not applied until you
//! reboot the instance without failover". So a modification that moves an instance to
//! ANOTHER group reports that group's membership `applying` while RDS attaches it, then
//! `pending-reboot` once it is attached and for as long as the instance has not come up
//! again — the reboot response included, because the engine restarts when the momentary
//! outage is over and not when the call was accepted — and `in-sync` once the instance is
//! back `available`. A modification that names the group the instance is ALREADY in changes
//! nothing, so its membership stays in sync.
//!
//! The rows span what the rule generalises over: two engine families with their own parameter
//! group families, a switch to a group created empty and a switch to a group carrying a
//! static parameter change, and the no-op re-naming of the group already in effect. Every
//! wait is a poll for the CONDITION the client waits on, through the shared poll loop.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML this emulator itself answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::{poll_until, read_instance};

/// `DBParameterGroupStatus$ParameterApplyStatus` while RDS is attaching the group a
/// modification named.
const APPLYING: &str = "applying";

/// The status of a group that is attached and waiting for the reboot that applies it.
const PENDING_REBOOT: &str = "pending-reboot";

/// The status of a group whose values the engine is running.
const IN_SYNC: &str = "in-sync";

/// One row: the instance, the group it is switched to, and what that group holds.
struct SwitchCase {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    /// The engine version the instance is created with, which fixes its parameter group
    /// family.
    engine_version: &'static str,
    /// The group the modification switches the instance to, and its family.
    group: &'static str,
    family: &'static str,
    /// A `(name, value, ApplyMethod)` the group is given before the switch, for a row whose
    /// group carries a change of its own.
    parameter: Option<(&'static str, &'static str, &'static str)>,
}

const SWITCH_CASES: &[SwitchCase] = &[
    SwitchCase {
        case: "a postgres instance switched to an empty group of its family",
        identifier: "emu-pgswitch-empty",
        engine: "postgres",
        engine_version: "18.3",
        group: "emu-pgswitch-empty-group",
        family: "postgres18",
        parameter: None,
    },
    SwitchCase {
        case: "a postgres instance switched to a group carrying a static parameter",
        identifier: "emu-pgswitch-static",
        engine: "postgres",
        engine_version: "18.3",
        group: "emu-pgswitch-static-group",
        family: "postgres18",
        // A static parameter: one that "requires a reboot ... before the change can take
        // effect", which is the change this switch carries over.
        parameter: Some(("shared_buffers", "16384", "pending-reboot")),
    },
    SwitchCase {
        case: "a mysql instance switched to an empty group of its family",
        identifier: "emu-myswitch-empty",
        engine: "mysql",
        engine_version: "8.0.40",
        group: "emu-myswitch-empty-group",
        family: "mysql8.0",
        parameter: None,
    },
];

/// The compute class every instance below is ordered in.
const CLASS: &str = "db.t3.micro";

/// The `ParameterApplyStatus` of the one DB parameter group membership `instance` reports.
fn apply_status(case: &str, envelope: &str, instance: &str) -> String {
    let memberships = subtrees(instance, "DBParameterGroup");
    assert_eq!(
        memberships.len(),
        1,
        "case {case:?}: the {envelope} envelope must report one DB parameter group \
         membership, got {instance}"
    );
    let membership = memberships.into_iter().next().expect("checked just above");
    text(&membership, "ParameterApplyStatus")
}

/// The one `<DBInstance>` subtree `body` carries.
fn the_instance(case: &str, body: &str) -> String {
    let reported = subtrees(body, "DBInstance");
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: exactly one DBInstance must be reported, got {body}"
    );
    reported.into_iter().next().expect("checked just above")
}

/// Create one row's instance and its group, and wait until the instance is `available`.
async fn provision(router: &MultiServiceRouter, row: &SwitchCase) {
    expect_ok(
        row.case,
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", row.identifier),
                ("Engine", row.engine),
                ("EngineVersion", row.engine_version),
                ("DBInstanceClass", CLASS),
                ("AllocatedStorage", "20"),
                ("MasterUsername", "emuadmin"),
                ("MasterUserPassword", "a-strong-password-1"),
            ],
        )
        .await,
    );
    expect_ok(
        row.case,
        call(
            router,
            "CreateDBParameterGroup",
            &[
                ("DBParameterGroupName", row.group),
                ("DBParameterGroupFamily", row.family),
                ("Description", "a group this suite switches an instance to"),
            ],
        )
        .await,
    );
    if let Some((name, value, apply_method)) = row.parameter {
        expect_ok(
            row.case,
            call(
                router,
                "ModifyDBParameterGroup",
                &[
                    ("DBParameterGroupName", row.group),
                    ("Parameters.Parameter.1.ParameterName", name),
                    ("Parameters.Parameter.1.ParameterValue", value),
                    ("Parameters.Parameter.1.ApplyMethod", apply_method),
                ],
            )
            .await,
        );
    }
    await_apply_status(router, row, IN_SYNC).await;
}

/// Poll the instance until it is `available` with the apply status `expected`, and hand back
/// the instance it settled on.
async fn await_apply_status(
    router: &MultiServiceRouter,
    row: &SwitchCase,
    expected: &str,
) -> String {
    let (body, _) = poll_until(
        row.case,
        &format!("the group membership reaching {expected}"),
        || read_instance(router, row.identifier),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
                && optional_text(body, "ParameterApplyStatus").as_deref() == Some(expected)
        },
    )
    .await;
    the_instance(row.case, &body)
}

#[tokio::test]
async fn a_parameter_group_switch_is_applying_then_waits_for_a_reboot() {
    let router = emulator();

    for row in SWITCH_CASES {
        provision(&router, row).await;

        // The modification attaches the group: the name changes at once, and the membership
        // reports that RDS is applying it.
        let modified = the_instance(
            row.case,
            &expect_ok(
                row.case,
                call(
                    &router,
                    "ModifyDBInstance",
                    &[
                        ("DBInstanceIdentifier", row.identifier),
                        ("DBParameterGroupName", row.group),
                    ],
                )
                .await,
            ),
        );
        let reported = optional_text(&modified, "DBParameterGroupName");
        assert_eq!(
            reported.as_deref(),
            Some(row.group),
            "case {:?}: the modification must report the group it attached, got {reported:?}",
            row.case
        );
        let status = apply_status(row.case, "modify", &modified);
        assert_eq!(
            status, APPLYING,
            "case {:?}: expected the attached group to report {APPLYING:?}, got {status:?}",
            row.case
        );

        // Once attached, the membership rests at `pending-reboot`: the instance is back in
        // service with a change only a reboot can apply.
        let settled = await_apply_status(&router, row, PENDING_REBOOT).await;
        let status = apply_status(row.case, "describe", &settled);
        assert_eq!(
            status, PENDING_REBOOT,
            "case {:?}: expected the attached group to rest at {PENDING_REBOOT:?}, got \
             {status:?}",
            row.case
        );

        // The reboot answers with the instance going out of service, still carrying the
        // change it is the reboot that applies.
        let rebooted = the_instance(
            row.case,
            &expect_ok(
                row.case,
                call(
                    &router,
                    "RebootDBInstance",
                    &[("DBInstanceIdentifier", row.identifier)],
                )
                .await,
            ),
        );
        let reported = optional_text(&rebooted, "DBInstanceStatus");
        assert_eq!(
            reported.as_deref(),
            Some("rebooting"),
            "case {:?}: the reboot must report the instance out of service, got {reported:?}",
            row.case
        );
        let status = apply_status(row.case, "reboot", &rebooted);
        assert_eq!(
            status, PENDING_REBOOT,
            "case {:?}: the reboot response must still report {PENDING_REBOOT:?}, got \
             {status:?}",
            row.case
        );

        // And once the engine is up again the group's values are the ones it is running.
        let back = await_apply_status(&router, row, IN_SYNC).await;
        let status = apply_status(row.case, "post-reboot describe", &back);
        assert_eq!(
            status, IN_SYNC,
            "case {:?}: the reboot applies the change, so the membership is in sync again, \
             got {status:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn naming_the_group_an_instance_is_already_in_changes_nothing() {
    let router = emulator();
    let row = &SWITCH_CASES[0];
    let case = "the group already in effect, named again";
    provision(&router, row).await;
    let in_effect = optional_text(
        &await_apply_status(&router, row, IN_SYNC).await,
        "DBParameterGroupName",
    )
    .unwrap_or_else(|| panic!("case {case:?}: the instance must report its own group"));

    let modified = the_instance(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "ModifyDBInstance",
                &[
                    ("DBInstanceIdentifier", row.identifier),
                    ("DBParameterGroupName", in_effect.as_str()),
                ],
            )
            .await,
        ),
    );
    let status = apply_status(case, "modify", &modified);
    assert_eq!(
        status, IN_SYNC,
        "case {case:?}: naming the group the instance is already in attaches nothing, so the \
         membership stays {IN_SYNC:?}, got {status:?}"
    );
}
