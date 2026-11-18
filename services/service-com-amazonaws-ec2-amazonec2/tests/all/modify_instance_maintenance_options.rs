// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyInstanceMaintenanceOptions`, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=ModifyInstanceMaintenanceOptions`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test is that the call WRITES EXACTLY THE SETTINGS THE REQUEST NAMES onto
//! the one instance it names, leaves every setting it does not name as it stands, and
//! answers with the settings that instance carries from then on — read back through
//! `DescribeInstances`, which reports the same stored record as `Instance$MaintenanceOptions`.
//!
//! So the tables below span the CLASS rather than one member: each of the two settings is
//! asked for on its own, both at once, each at both of the words its own catalog publishes,
//! and a request that names neither is asked too. Every row starts from an instance whose
//! two settings are both at the word that is NOT their documented default, so a setting a
//! modify must leave alone cannot pass by being re-derived from the defaults.
//!
//! Four suites:
//!
//! 1. the setting table — one modify per row, the answer it gives, and the record
//!    `DescribeInstances` reports afterwards;
//! 2. the catalogs — the words the rows ask for are members of the modelled enum of the
//!    member they are asked for;
//! 3. the lifecycle table — which instance states admit a modify and which answer
//!    `IncorrectInstanceState`;
//! 4. the refusal table — the instance ids and the `DryRun` probe it refuses, each asserted
//!    to leave the instance's settings exactly where they were. The id rows span both
//!    published id forms, the values inside them that name no instance, and the values
//!    outside them: this operation LOOKS UP every value EC2 can read as an instance id and
//!    refuses only one it cannot read.

use ec2::ops::types::{INSTANCE_AUTO_RECOVERY_STATE_VALUES, INSTANCE_REBOOT_MIGRATION_STATE_VALUES};
use ec2::state::{
    AUTO_RECOVERY_DEFAULT, ComAmazonawsEc2Amazonec2State as Ec2State, InstanceStateName,
    REBOOT_MIGRATION_DEFAULT,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query;
use super::wire::{child_text, document};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here names.
const AMI: &str = "ami-0123456789abcdef0";

/// `InstanceAutoRecoveryState`'s and `InstanceRebootMigrationState`'s `disabled` member —
/// the word both catalogs publish beside `default`
/// (`smithy_query(query="shape",shape="InstanceAutoRecoveryState,InstanceRebootMigrationState")`),
/// pinned against those catalogs by
/// [`the_words_the_rows_use_are_members_of_the_modelled_catalogs`].
const DISABLED: &str = "disabled";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// The two maintenance settings one instance carries, as every report of them spells them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    auto_recovery: String,
    reboot_migration: String,
}

impl Options {
    /// The settings every row starts from: both members at the word that is NOT their
    /// documented default.
    fn seeded() -> Self {
        Self {
            auto_recovery: DISABLED.to_string(),
            reboot_migration: DISABLED.to_string(),
        }
    }
}

/// The two settings one `<{element}>` of a response body carries.
fn options_of(case: &str, body: &str, element: &str) -> Options {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == element)
        .unwrap_or_else(|| panic!("case {case:?}: the body carries no <{element}>: {body}"));
    let text = |member: &str| {
        child_text(node, member).unwrap_or_else(|| {
            panic!("case {case:?}: <{element}> carries no <{member}>: {body}")
        })
    };
    Options {
        auto_recovery: text("autoRecovery"),
        reboot_migration: text("rebootMigration"),
    }
}

/// The two settings the `ModifyInstanceMaintenanceOptions` answer itself reports, which the
/// model binds as members of the response envelope rather than of a nested shape
/// (`smithy_query(query="operation",operation="ModifyInstanceMaintenanceOptions")`).
fn answered(case: &str, body: &str) -> Options {
    let text = |member: &str| {
        ec2_query::element(body, member).unwrap_or_else(|| {
            panic!("case {case:?}: the answer carries no <{member}>: {body}")
        })
    };
    Options {
        auto_recovery: text("autoRecovery"),
        reboot_migration: text("rebootMigration"),
    }
}

/// One `ModifyInstanceMaintenanceOptions` call for `instance_id` with `params` beside it.
async fn modify(
    router: &MultiServiceRouter,
    instance_id: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut all = vec![p("InstanceId", instance_id)];
    all.extend_from_slice(params);
    ec2_query::call(router, REGION, "ModifyInstanceMaintenanceOptions", &all).await
}

/// Launch one instance and seed both of its maintenance settings to [`Options::seeded`]:
/// `AutoRecovery` through the launch's own `MaintenanceOptions` (the only member that shape
/// declares), `RebootMigration` through a modify, which is the only way to write it.
async fn launch_seeded(router: &MultiServiceRouter, case: &str) -> String {
    let params = vec![
        p("ImageId", AMI),
        p("MinCount", "1"),
        p("MaxCount", "1"),
        p("MaintenanceOptions.AutoRecovery", DISABLED),
    ];
    let (status, body) = ec2_query::call(router, REGION, "RunInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from RunInstances, got {status} with body {body}"
    );
    let instance_id = ec2_query::element(&body, "instanceId").unwrap_or_else(|| {
        panic!("case {case:?}: RunInstances must report the instance it minted: {body}")
    });
    let (status, body) = modify(router, &instance_id, &[p("RebootMigration", DISABLED)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the seeding modify, got {status} with body {body}"
    );
    instance_id
}

/// The settings `DescribeInstances` reports for one instance.
async fn described(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Options {
    let (status, body) = ec2_query::call(
        router,
        REGION,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeInstances, got {status} with body {body}"
    );
    options_of(case, &body, "maintenanceOptions")
}

/// One row of the setting table: what the request names, and what the instance's settings
/// must be afterwards.
struct Row {
    /// What this row is a case of, named in every assertion of it.
    case: &'static str,
    /// The `ModifyInstanceMaintenanceOptions` parameters beside `InstanceId`.
    names: &'static [(&'static str, &'static str)],
    /// The settings the instance must carry once the modify is served, as a mutation of
    /// [`Options::seeded`].
    expected: fn(Options) -> Options,
}

/// Each setting on its own at each word of its catalog, both at once, and a request that
/// names neither.
const ROWS: &[Row] = &[
    Row {
        case: "AutoRecovery back to default",
        names: &[("AutoRecovery", AUTO_RECOVERY_DEFAULT)],
        expected: |seeded| Options {
            auto_recovery: AUTO_RECOVERY_DEFAULT.to_string(),
            ..seeded
        },
    },
    Row {
        case: "AutoRecovery disabled again",
        names: &[("AutoRecovery", DISABLED)],
        expected: |seeded| seeded,
    },
    Row {
        case: "RebootMigration back to default",
        names: &[("RebootMigration", REBOOT_MIGRATION_DEFAULT)],
        expected: |seeded| Options {
            reboot_migration: REBOOT_MIGRATION_DEFAULT.to_string(),
            ..seeded
        },
    },
    Row {
        case: "RebootMigration disabled again",
        names: &[("RebootMigration", DISABLED)],
        expected: |seeded| seeded,
    },
    Row {
        case: "both settings back to default at once",
        names: &[
            ("AutoRecovery", AUTO_RECOVERY_DEFAULT),
            ("RebootMigration", REBOOT_MIGRATION_DEFAULT),
        ],
        expected: |_seeded| Options {
            auto_recovery: AUTO_RECOVERY_DEFAULT.to_string(),
            reboot_migration: REBOOT_MIGRATION_DEFAULT.to_string(),
        },
    },
    Row {
        case: "one setting to default and the other left disabled, in one call",
        names: &[
            ("AutoRecovery", AUTO_RECOVERY_DEFAULT),
            ("RebootMigration", DISABLED),
        ],
        expected: |_seeded| Options {
            auto_recovery: AUTO_RECOVERY_DEFAULT.to_string(),
            reboot_migration: DISABLED.to_string(),
        },
    },
    Row {
        case: "no setting at all",
        names: &[],
        expected: |seeded| seeded,
    },
];

/// The parameters one row names.
fn named(names: &[(&str, &str)]) -> Vec<(String, String)> {
    names.iter().map(|(key, value)| p(key, *value)).collect()
}

/// Rule 1: the call writes exactly the settings the request names, leaves the other one as
/// it stands, answers with the instance's own id and both settings, and `DescribeInstances`
/// reports the same record.
#[tokio::test]
async fn writes_the_settings_the_request_names_and_leaves_the_rest() {
    let (router, _engine) = ec2_query::emulator();

    for row in ROWS {
        let case = row.case;
        let instance_id = launch_seeded(&router, case).await;
        let expected = (row.expected)(Options::seeded());

        let (status, body) = modify(&router, &instance_id, &named(row.names)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the modify, got {status} with body {body}"
        );
        assert_eq!(
            ec2_query::element(&body, "instanceId").as_deref(),
            Some(instance_id.as_str()),
            "case {case:?}: the answer must name the instance it modified: {body}"
        );
        let reported = answered(case, &body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the answer to report {expected:?}, got {reported:?}"
        );

        let described = described(&router, case, &instance_id).await;
        assert_eq!(
            described, expected,
            "case {case:?}: expected DescribeInstances to report {expected:?}, got {described:?}"
        );
    }
}

/// Rule 2: the words the rows above ask for are members of the catalog of the member they
/// are asked for, so no row can pin a value the model does not publish.
#[test]
fn the_words_the_rows_use_are_members_of_the_modelled_catalogs() {
    for (member, catalog, words) in [
        (
            "AutoRecovery",
            INSTANCE_AUTO_RECOVERY_STATE_VALUES,
            [AUTO_RECOVERY_DEFAULT, DISABLED],
        ),
        (
            "RebootMigration",
            INSTANCE_REBOOT_MIGRATION_STATE_VALUES,
            [REBOOT_MIGRATION_DEFAULT, DISABLED],
        ),
    ] {
        for word in words {
            assert!(
                catalog.contains(&word),
                "case {member:?}: expected the catalog {catalog:?} to publish {word:?}, it does \
                 not"
            );
        }
    }
}

/// Rule 3: an instance on its way to `terminated`, or already there, has no maintenance or
/// recovery behaviour left to configure and answers `IncorrectInstanceState` while keeping
/// its settings; every other state admits the modify.
#[tokio::test]
async fn only_an_instance_that_is_not_terminating_admits_a_modify() {
    let states = [
        (InstanceStateName::Pending, true),
        (InstanceStateName::Running, true),
        (InstanceStateName::Stopping, true),
        (InstanceStateName::Stopped, true),
        (InstanceStateName::ShuttingDown, false),
        (InstanceStateName::Terminated, false),
    ];
    for (state, admitted) in states {
        let case = format!("a modify of an instance in {}", state.as_str());
        let (router, engine) = ec2_query::emulator();
        let instance_id = launch_seeded(&router, &case).await;
        {
            let store = engine.state::<Ec2State>();
            let mut guard = store.lock();
            let instance = guard.instances.get_mut(&instance_id).unwrap_or_else(|| {
                panic!("case {case:?}: the store must hold the launched instance {instance_id}")
            });
            instance.state = state;
            instance.pending_transition = None;
        }

        let (status, body) = modify(
            &router,
            &instance_id,
            &[p("AutoRecovery", AUTO_RECOVERY_DEFAULT)],
        )
        .await;
        if admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            let reported = answered(&case, &body);
            assert_eq!(
                reported.auto_recovery, AUTO_RECOVERY_DEFAULT,
                "case {case:?}: expected autoRecovery {AUTO_RECOVERY_DEFAULT:?}, got {reported:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            ec2_query::error_code(&body),
            "IncorrectInstanceState",
            "case {case:?}: expected the state refusal, got {body}"
        );
        assert_eq!(
            ec2_query::error_message(&body),
            format!(
                "The instance '{instance_id}' is not in a state from which it can be modified."
            ),
            "case {case:?}: expected the refusal to name the instance and the action: {body}"
        );
        let reported = described(&router, &case, &instance_id).await;
        assert_eq!(
            reported,
            Options::seeded(),
            "case {case:?}: a refused modify must leave the settings as they were, got \
             {reported:?}"
        );
    }
}

/// One row of the refusal table.
struct Refused {
    case: &'static str,
    /// The `InstanceId` the request names, or `None` for the instance this suite launched.
    instance_id: Option<&'static str>,
    /// The parameters beside it.
    names: &'static [(&'static str, &'static str)],
    status: u16,
    code: &'static str,
    /// The whole sentence the refusal must carry, as a template of the id the request named.
    message: fn(&str) -> String,
}

/// The ids and the probe a modify refuses.
const REFUSALS: &[Refused] = &[
    Refused {
        case: "an instance id outside both forms EC2 reads",
        instance_id: Some("not-an-instance"),
        names: &[("AutoRecovery", AUTO_RECOVERY_DEFAULT)],
        status: 400,
        code: "InvalidInstanceID.Malformed",
        message: |named| format!("The instance ID '{named}' is malformed"),
    },
    Refused {
        case: "an instance id with too few digits for either form",
        instance_id: Some("i-0123"),
        names: &[("AutoRecovery", AUTO_RECOVERY_DEFAULT)],
        status: 400,
        code: "InvalidInstanceID.Malformed",
        message: |named| format!("The instance ID '{named}' is malformed"),
    },
    Refused {
        case: "another resource kind's id in place of an instance id",
        instance_id: Some("vpc-0123456789abcdef0"),
        names: &[("AutoRecovery", AUTO_RECOVERY_DEFAULT)],
        status: 400,
        code: "InvalidInstanceID.Malformed",
        message: |named| format!("The instance ID '{named}' is malformed"),
    },
    Refused {
        case: "a long-form instance id this account holds no instance for",
        instance_id: Some("i-0fedcba9876543210"),
        names: &[("RebootMigration", REBOOT_MIGRATION_DEFAULT)],
        status: 400,
        code: "InvalidInstanceID.NotFound",
        message: |named| format!("The instance ID '{named}' does not exist"),
    },
    Refused {
        // The retired SHORT form is one EC2 still reads, so an account holding no instance
        // for it is told the instance is absent rather than that the id is unreadable.
        case: "a short-form instance id this account holds no instance for",
        instance_id: Some("i-1a2b3c4d"),
        names: &[("RebootMigration", REBOOT_MIGRATION_DEFAULT)],
        status: 400,
        code: "InvalidInstanceID.NotFound",
        message: |named| format!("The instance ID '{named}' does not exist"),
    },
    Refused {
        // Inside the long form EC2 reads, above the identifiers it has issued: this
        // operation looks such a value up rather than refusing to read it.
        case: "a long-form instance id whose leading digit is above zero",
        instance_id: Some("i-1234567890abcdef0"),
        names: &[("AutoRecovery", AUTO_RECOVERY_DEFAULT)],
        status: 400,
        code: "InvalidInstanceID.NotFound",
        message: |named| format!("The instance ID '{named}' does not exist"),
    },
    Refused {
        case: "the all-zero instance id every client reaches for as a placeholder",
        instance_id: Some("i-00000000000000000"),
        names: &[("AutoRecovery", AUTO_RECOVERY_DEFAULT)],
        status: 400,
        code: "InvalidInstanceID.NotFound",
        message: |named| format!("The instance ID '{named}' does not exist"),
    },
    Refused {
        case: "a dry run of a request that would have succeeded",
        instance_id: None,
        names: &[
            ("AutoRecovery", AUTO_RECOVERY_DEFAULT),
            ("RebootMigration", REBOOT_MIGRATION_DEFAULT),
            ("DryRun", "true"),
        ],
        status: 412,
        code: "DryRunOperation",
        message: |_named| "Request would have succeeded, but DryRun flag is set.".to_string(),
    },
];

/// Rule 4: every refusal answers its own status, code and sentence, and leaves the
/// instance's settings exactly where they were.
#[tokio::test]
async fn a_refused_modify_writes_nothing() {
    let (router, _engine) = ec2_query::emulator();
    let instance_id = launch_seeded(&router, "the instance every refusal is asked about").await;

    for row in REFUSALS {
        let case = row.case;
        let named_id = row.instance_id.unwrap_or(instance_id.as_str());
        let (status, body) = modify(&router, named_id, &named(row.names)).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {}, got {status} with body {body}",
            row.status
        );
        assert_eq!(
            ec2_query::error_code(&body),
            row.code,
            "case {case:?}: expected the code {:?}, got {body}",
            row.code
        );
        assert_eq!(
            ec2_query::error_message(&body),
            (row.message)(named_id),
            "case {case:?}: expected the sentence {:?}, got {body}",
            (row.message)(named_id)
        );

        let reported = described(&router, case, &instance_id).await;
        assert_eq!(
            reported,
            Options::seeded(),
            "case {case:?}: a refused modify must leave the settings as they were, got \
             {reported:?}"
        );
    }
}

/// Rule 5: nothing but the one instance the request names is touched — a second instance of
/// the same account keeps the settings it carried.
#[tokio::test]
async fn a_modify_touches_only_the_instance_it_names() {
    let case = "two instances, one modified";
    let (router, _engine) = ec2_query::emulator();
    let modified = launch_seeded(&router, case).await;
    let untouched = launch_seeded(&router, case).await;

    let (status, body) = modify(
        &router,
        &modified,
        &[
            p("AutoRecovery", AUTO_RECOVERY_DEFAULT),
            p("RebootMigration", REBOOT_MIGRATION_DEFAULT),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the modify, got {status} with body {body}"
    );

    let reported = described(&router, case, &untouched).await;
    assert_eq!(
        reported,
        Options::seeded(),
        "case {case:?}: the instance the request did not name keeps its own settings, got \
         {reported:?}"
    );
    let changed = described(&router, case, &modified).await;
    assert_eq!(
        changed,
        Options {
            auto_recovery: AUTO_RECOVERY_DEFAULT.to_string(),
            reboot_migration: REBOOT_MIGRATION_DEFAULT.to_string(),
        },
        "case {case:?}: the instance the request named carries the modified settings, got \
         {changed:?}"
    );
}
