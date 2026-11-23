// SPDX-License-Identifier: CC0-1.0

//! The DB parameter group APPLY lifecycle a `ModifyDBInstance` that switches groups starts.
//!
//! The rule under test: `ModifyDBInstance$DBParameterGroupName` — "the parameter group name
//! itself is changed immediately, but the actual parameter changes are not applied until you
//! reboot the instance without failover. … However, if you modify dynamic parameters in the
//! newly associated DB parameter group, these changes are applied immediately without a
//! reboot". So a modification that moves an instance to ANOTHER group reports that group's
//! membership `applying` while RDS attaches it, and where it rests once attached is decided by
//! what the newly associated group carries over:
//!
//! * every value it sets is one applied immediately (a dynamic parameter the caller asked no
//!   reboot for) — nothing is left for a reboot to do, so the membership is `in-sync`;
//! * it sets a value whose change needs a reboot — a static parameter, or one the caller asked
//!   to apply `pending-reboot` — so the membership rests at `pending-reboot` until the engine
//!   comes up again, the reboot response included, because the engine restarts when the
//!   momentary outage is over and not when the reboot was accepted, and is `in-sync` once the
//!   instance is back `available`;
//! * it sets nothing at all, so the switch carries no immediately applied change either and
//!   the membership waits for the engine to read the new group when it next starts.
//!
//! A modification that names the group the instance is ALREADY in changes nothing, so its
//! membership stays in sync.
//!
//! The rows span what the rule generalises over: two engine families with their own parameter
//! group families, a group created empty, a group carrying one dynamic value, two dynamic
//! values, a static value, and a dynamic value the request asked to apply at the next reboot —
//! and the no-op re-naming of the group already in effect. Every wait is a poll for the
//! CONDITION the client waits on, through the shared poll loop.
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

/// `Parameter$Source` of a value a CALLER set, which is what the group reports for each value
/// a row writes into it.
const SOURCE_USER: &str = "user";

/// One `(ParameterName, ParameterValue, ApplyMethod)` a row writes into its group before the
/// switch; the method is omitted where the row leaves it to the parameter's apply type.
type Setting = (&'static str, &'static str, Option<&'static str>);

/// One row: the instance, the group it is switched to, what that group holds, and where the
/// membership rests once the group is attached.
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
    /// The values the group is given before the switch.
    settings: &'static [Setting],
    /// The `ParameterApplyStatus` the membership rests at once the group is attached.
    attached: &'static str,
}

const SWITCH_CASES: &[SwitchCase] = &[
    SwitchCase {
        case: "a postgres instance switched to an empty group of its family",
        identifier: "emu-pgswitch-empty",
        engine: "postgres",
        engine_version: "18.3",
        group: "emu-pgswitch-empty-group",
        family: "postgres18",
        settings: &[],
        attached: PENDING_REBOOT,
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
        settings: &[("shared_buffers", "16384", Some(PENDING_REBOOT))],
        attached: PENDING_REBOOT,
    },
    SwitchCase {
        case: "a postgres instance switched to a group carrying one dynamic parameter",
        identifier: "emu-pgswitch-dynamic",
        engine: "postgres",
        engine_version: "18.3",
        group: "emu-pgswitch-dynamic-group",
        family: "postgres18",
        // A dynamic parameter, left to its own apply type: "changes to dynamic parameters are
        // applied immediately", so this switch leaves nothing for a reboot
        // (`smithy_query(query="doc", shape="ModifyDBInstanceMessage",
        // member="DBParameterGroupName")`; the apply type is the family's published one,
        // `DescribeEngineDefaultParameters`).
        settings: &[("log_min_duration_statement", "1000", None)],
        attached: IN_SYNC,
    },
    SwitchCase {
        case: "a mysql instance switched to an empty group of its family",
        identifier: "emu-myswitch-empty",
        engine: "mysql",
        engine_version: "8.0.40",
        group: "emu-myswitch-empty-group",
        family: "mysql8.0",
        settings: &[],
        attached: PENDING_REBOOT,
    },
    SwitchCase {
        case: "a mysql instance switched to a group carrying two dynamic parameters",
        identifier: "emu-myswitch-dynamic",
        engine: "mysql",
        engine_version: "8.0.40",
        group: "emu-myswitch-dynamic-group",
        family: "mysql8.0",
        settings: &[
            ("auto_increment_increment", "2", None),
            ("autocommit", "0", None),
        ],
        attached: IN_SYNC,
    },
    SwitchCase {
        case: "a mysql instance switched to a group carrying a dynamic and a static parameter",
        identifier: "emu-myswitch-mixed",
        engine: "mysql",
        engine_version: "8.0.40",
        group: "emu-myswitch-mixed-group",
        family: "mysql8.0",
        // `back_log` is published static, so one value of the group waits for the reboot
        // however the others apply.
        settings: &[
            ("auto_increment_increment", "2", None),
            ("back_log", "1024", None),
        ],
        attached: PENDING_REBOOT,
    },
    SwitchCase {
        case: "a mysql instance switched to a group whose dynamic value awaits a reboot",
        identifier: "emu-myswitch-deferred",
        engine: "mysql",
        engine_version: "8.0.40",
        group: "emu-myswitch-deferred-group",
        family: "mysql8.0",
        // A dynamic parameter the CALLER asked to apply at the next reboot, which is what
        // `Parameter$ApplyMethod` is for: the value waits whatever its apply type says.
        settings: &[("auto_increment_increment", "2", Some(PENDING_REBOOT))],
        attached: PENDING_REBOOT,
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
    for (name, value, apply_method) in row.settings {
        let mut request = vec![
            ("DBParameterGroupName", row.group.to_string()),
            ("Parameters.Parameter.1.ParameterName", (*name).to_string()),
            ("Parameters.Parameter.1.ParameterValue", (*value).to_string()),
        ];
        if let Some(method) = apply_method {
            request.push((
                "Parameters.Parameter.1.ApplyMethod",
                (*method).to_string(),
            ));
        }
        expect_ok(
            row.case,
            call(router, "ModifyDBParameterGroup", &request).await,
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

/// Every value a row wrote into its group, as the group's own read reports it: the value the
/// row set, under `Source` `user`, which is what a client reads to confirm the configuration
/// the switch put the instance on.
async fn assert_group_reports_its_values(router: &MultiServiceRouter, row: &SwitchCase) {
    if row.settings.is_empty() {
        return;
    }
    let reported = expect_ok(
        row.case,
        call(
            router,
            "DescribeDBParameters",
            &[
                ("DBParameterGroupName", row.group),
                ("Source", SOURCE_USER),
            ],
        )
        .await,
    );
    let rows = subtrees(&reported, "Parameter");
    for (name, value, _) in row.settings {
        let found = rows
            .iter()
            .find(|parameter| optional_text(parameter, "ParameterName").as_deref() == Some(*name))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the group to report {name}, got {reported}",
                    row.case
                )
            });
        assert_eq!(
            (
                optional_text(found, "ParameterValue").as_deref(),
                optional_text(found, "Source").as_deref(),
            ),
            (Some(*value), Some(SOURCE_USER)),
            "case {:?}: expected {name} to report the value the row set under {SOURCE_USER:?}, \
             got {found}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_parameter_group_switch_rests_where_the_values_it_carries_put_it() {
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
        let resource_id = optional_text(&modified, "DbiResourceId");
        let status = apply_status(row.case, "modify", &modified);
        assert_eq!(
            status, APPLYING,
            "case {:?}: expected the attached group to report {APPLYING:?}, got {status:?}",
            row.case
        );

        // Once attached, the membership rests where the values the group carries put it.
        let settled = await_apply_status(&router, row, row.attached).await;
        let status = apply_status(row.case, "describe", &settled);
        assert_eq!(
            status, row.attached,
            "case {:?}: expected the attached group to rest at {:?}, got {status:?}",
            row.case, row.attached
        );
        let reported = optional_text(&settled, "DBParameterGroupName");
        assert_eq!(
            reported.as_deref(),
            Some(row.group),
            "case {:?}: the attached group is the one the instance reports, got {reported:?}",
            row.case
        );
        // A switch is a modification of the instance, never a new instance: the identity it
        // was created with is untouched by it.
        let settled_resource_id = optional_text(&settled, "DbiResourceId");
        assert_eq!(
            settled_resource_id, resource_id,
            "case {:?}: the switch must leave the instance's resource id alone, got \
             {settled_resource_id:?} after {resource_id:?}",
            row.case
        );
        // And the group reports the values the switch put the instance on.
        assert_group_reports_its_values(&router, row).await;

        if row.attached != PENDING_REBOOT {
            continue;
        }

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
