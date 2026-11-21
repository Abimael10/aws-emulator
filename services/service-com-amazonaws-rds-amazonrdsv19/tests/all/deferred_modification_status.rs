// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `DBInstanceStatus` a `ModifyDBInstance` leaves behind, and for
//! what a client can do with the instance while a modification is still pending.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on.
//!
//! The rule under test: `DBInstanceStatus` reports what the DB instance is doing NOW, and a
//! setting staged for the next maintenance window is not being applied yet — "if this
//! parameter is disabled, changes to the DB instance are applied during the next maintenance
//! window" (`ApplyImmediately`). So a modification whose every setting is deferred leaves
//! the instance in the status it is running under, with the requested values reported in
//! `PendingModifiedValues` ("changes that will be applied during the next maintenance
//! window"), and the instance goes on accepting what that status allows — the
//! `RebootDBInstance` such a change waits for, which applies it. A modification that IS
//! being applied, because the caller asked for it as soon as possible, reports `modifying`
//! and takes no reboot until it has settled.
//!
//! The table below spans what the rule generalises over: the kinds of value
//! `PendingModifiedValues` carries — a published certificate identifier, an integer, an
//! instance class and a boolean — each staged, read back, and then applied by a reboot.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    supported_ca_certificate_identifiers, DEFAULT_CA_CERTIFICATE_IDENTIFIER,
    STATUS_TRANSITION_DELAY,
};
use smithy_server_runtime::MultiServiceRouter;
use std::time::{Duration, SystemTime};

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts,
};
use crate::pending_modified_values::window_days_ahead;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// How long a poll for a change the clock owes keeps looking before giving up: far longer
/// than the span this service owes a transition in, so a loaded machine still gets there.
/// Nothing is asserted about elapsed time — only that the condition holds in the end.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often that poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The engine, class and storage every instance below is created with.
const ENGINE: &str = "postgres";
const CLASS: &str = "db.t3.micro";
const STORAGE: &str = "20";

/// One row of the deferral table: a setting `ModifyDBInstance` takes, the value it is
/// modified to, the member the instance reports it under, and what it read before.
struct DeferralCase {
    case: &'static str,
    identifier: &'static str,
    /// The member the request carries the new value in.
    member: &'static str,
    value: &'static str,
    /// The member of `DBInstance` — and of `PendingModifiedValues` — the value is reported
    /// under.
    reported_as: &'static str,
    /// What the instance reports for `reported_as` before the modification.
    was: &'static str,
}

/// The settings driven through the rule, one per kind of value `PendingModifiedValues`
/// carries.
///
/// The certificate identifier is one the service's own published CA catalogue carries
/// (`DescribeCertificates`/`supported_ca_certificate_identifiers`) and is not the default a
/// create assigns, which [`the_fixtures_are_well_formed`] checks against that catalogue
/// rather than against a value written here.
const DEFERRED: &[DeferralCase] = &[
    DeferralCase {
        case: "the CA certificate",
        identifier: "emu-defer-ca-ecc",
        member: "CACertificateIdentifier",
        value: "rds-ca-ecc384-g1",
        reported_as: "CACertificateIdentifier",
        was: DEFAULT_CA_CERTIFICATE_IDENTIFIER,
    },
    DeferralCase {
        case: "another CA certificate",
        identifier: "emu-defer-ca-rsa4096",
        member: "CACertificateIdentifier",
        value: "rds-ca-rsa4096-g1",
        reported_as: "CACertificateIdentifier",
        was: DEFAULT_CA_CERTIFICATE_IDENTIFIER,
    },
    DeferralCase {
        case: "allocated storage",
        identifier: "emu-defer-status-storage",
        member: "AllocatedStorage",
        value: "40",
        reported_as: "AllocatedStorage",
        was: STORAGE,
    },
    DeferralCase {
        case: "the DB instance class",
        identifier: "emu-defer-status-class",
        member: "DBInstanceClass",
        value: "db.t3.small",
        reported_as: "DBInstanceClass",
        was: CLASS,
    },
    DeferralCase {
        case: "a Multi-AZ deployment",
        identifier: "emu-defer-status-multi-az",
        member: "MultiAZ",
        value: "true",
        reported_as: "MultiAZ",
        was: "false",
    },
];

/// Create an instance whose maintenance window is days away — so a staged change is not
/// applied by the window while the test runs — and wait until it reports `available`.
async fn an_available_instance(router: &MultiServiceRouter, identifier: &str) {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => ENGINE,
                "DBInstanceClass" => CLASS,
                "AllocatedStorage" => STORAGE,
                "MasterUsername" => "pgadmin",
                "MasterUserPassword" => "a-strong-password-1",
                "PreferredMaintenanceWindow" => window_days_ahead(),
            ],
        )
        .await,
    );
    poll_instance(router, identifier, identifier, "available", |instance| {
        optional_text(instance, "DBInstanceStatus").as_deref() == Some("available")
    })
    .await;
}

/// The one `<DBInstance>` `DescribeDBInstances` reports for `identifier`.
async fn describe(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
    );
    let found = subtrees(&body, "DBInstance");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one DBInstance, got {found:?}"
    );
    found.into_iter().next().expect("checked just above")
}

/// Read the instance until `holds` is true of it, and hand that read back.
///
/// The CONDITION is what is waited for, looked for again until a deadline far longer than
/// the span this service owes a transition in; nothing is asserted about how long it took.
async fn poll_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    looking_for: &str,
    holds: impl Fn(&str) -> bool,
) -> String {
    let deadline = SystemTime::now() + POLL_DEADLINE;
    loop {
        let instance = describe(router, case, identifier).await;
        if holds(&instance) {
            return instance;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: expected {looking_for}, last read {instance}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What `instance` reports as pending, or `None` when it reports no pending change.
///
/// The element itself is reported on every instance envelope — "specific changes are
/// identified by subelements", and an instance with nothing pending carries it with no
/// subelement inside — so an EMPTY element is what "nothing is pending" reads as here.
fn pending(instance: &str) -> Option<String> {
    subtrees(instance, "PendingModifiedValues")
        .into_iter()
        .zip(texts(instance, "PendingModifiedValues"))
        .find(|(_, inside)| !inside.trim().is_empty())
        .map(|(staged, _)| staged)
}

#[tokio::test]
async fn a_modification_left_for_the_window_keeps_the_instance_in_service() {
    let router = emulator();
    for case in DEFERRED {
        an_available_instance(&router, case.identifier).await;
        let modified = expect_ok(
            case.case,
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => case.identifier,
                    "ApplyImmediately" => false,
                    case.member => case.value,
                ],
            )
            .await,
        );
        let answered = subtrees(&modified, "DBInstance")
            .into_iter()
            .next()
            .expect("the modification answers with the instance");
        // Nothing is being applied, so the instance is doing what it was doing: serving.
        let status = optional_text(&answered, "DBInstanceStatus");
        assert_eq!(
            status.as_deref(),
            Some("available"),
            "case {:?}: an instance with a change only STAGED keeps the status it is \
             running under, got {status:?}",
            case.case
        );
        // The live attribute still reads what the instance is running under, and the
        // requested value is reported as pending.
        let live = optional_text(&answered, case.reported_as);
        assert_eq!(
            live.as_deref(),
            Some(case.was),
            "case {:?}: expected the live {} to stay {:?}, got {live:?}",
            case.case,
            case.reported_as,
            case.was
        );
        let staged = pending(&answered).unwrap_or_else(|| {
            panic!(
                "case {:?}: a deferred modification must report PendingModifiedValues, got \
                 {answered}",
                case.case
            )
        });
        let reported = optional_text(&staged, case.reported_as);
        assert_eq!(
            reported.as_deref(),
            Some(case.value),
            "case {:?}: expected {} staged as {:?}, got {reported:?}",
            case.case,
            case.reported_as,
            case.value
        );
        // A read of the instance says the same: the deferral is in state, not in the one
        // answer.
        let read_back = describe(&router, case.case, case.identifier).await;
        let status = optional_text(&read_back, "DBInstanceStatus");
        assert_eq!(
            status.as_deref(),
            Some("available"),
            "case {:?}: a later read of an instance with a change staged must report it \
             available, got {read_back}",
            case.case
        );

        // And because it is available, the reboot the staged change waits for is ACCEPTED,
        // which is what applies it: "some parameter changes ... are applied on the next call
        // to RebootDBInstance".
        let rebooted = expect_ok(
            case.case,
            call(
                &router,
                "RebootDBInstance",
                &params!["DBInstanceIdentifier" => case.identifier],
            )
            .await,
        );
        let rebooting = subtrees(&rebooted, "DBInstance")
            .into_iter()
            .next()
            .expect("the reboot answers with the instance");
        let status = optional_text(&rebooting, "DBInstanceStatus");
        assert_eq!(
            status.as_deref(),
            Some("rebooting"),
            "case {:?}: an accepted reboot reports the momentary outage, got {status:?}",
            case.case
        );
        let settled = poll_instance(
            &router,
            case.case,
            case.identifier,
            "the instance available again with nothing pending",
            |instance| {
                optional_text(instance, "DBInstanceStatus").as_deref() == Some("available")
                    && pending(instance).is_none()
            },
        )
        .await;
        let live = optional_text(&settled, case.reported_as);
        assert_eq!(
            live.as_deref(),
            Some(case.value),
            "case {:?}: expected the reboot to apply {} as {:?}, got {live:?}",
            case.case,
            case.reported_as,
            case.value
        );
    }
}

#[tokio::test]
async fn a_modification_being_applied_reports_modifying_and_takes_no_reboot() {
    let router = emulator();
    let identifier = "emu-apply-now-status";
    an_available_instance(&router, identifier).await;
    // The same deferrable setting as a row above, asked for AS SOON AS POSSIBLE this time:
    // the instance IS applying it, so it is out of service for it.
    let modified = expect_ok(
        "applied at once",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "ApplyImmediately" => true,
                "AllocatedStorage" => 40,
            ],
        )
        .await,
    );
    let answered = subtrees(&modified, "DBInstance")
        .into_iter()
        .next()
        .expect("the modification answers with the instance");
    let status = optional_text(&answered, "DBInstanceStatus");
    assert_eq!(
        status.as_deref(),
        Some("modifying"),
        "a modification being applied reports modifying, got {status:?}"
    );
    // A reboot is accepted only from a status an engine is serving in, and this instance is
    // not: "can only reboot db instances with state in: available, ...".
    let refused = expect_error(
        "reboot while the change is being applied",
        call(
            &router,
            "RebootDBInstance",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
        400,
        "InvalidDBInstanceState",
    );
    let message = text(&refused, "Message");
    assert!(
        message.contains(identifier) && message.contains("available"),
        "the refusal must name the instance and the statuses a reboot is accepted from, got \
         {message:?}"
    );
    // It settles on its own, carrying the change.
    let settled = poll_instance(
        &router,
        "applied at once",
        identifier,
        "the instance available again with the change applied",
        |instance| {
            optional_text(instance, "DBInstanceStatus").as_deref() == Some("available")
                && pending(instance).is_none()
        },
    )
    .await;
    let live = optional_text(&settled, "AllocatedStorage");
    assert_eq!(
        live.as_deref(),
        Some("40"),
        "expected the storage asked for at once to be applied, got {live:?}"
    );
}

/// The values the table above names are the service's own: each staged certificate
/// identifier is one the published CA catalogue carries and none of them is the default a
/// create assigns, so every row really changes the instance's certificate.
#[test]
fn the_fixtures_are_well_formed() {
    assert!(
        STATUS_TRANSITION_DELAY < POLL_DEADLINE,
        "the poll deadline must be generous beside the span a transition is owed in"
    );
    let published = supported_ca_certificate_identifiers();
    for case in DEFERRED {
        if case.member != "CACertificateIdentifier" {
            continue;
        }
        assert!(
            published.iter().any(|identifier| identifier == case.value),
            "case {:?}: {:?} must be a certificate this service publishes, which are \
             {published:?}",
            case.case,
            case.value
        );
        assert_ne!(
            case.value, DEFAULT_CA_CERTIFICATE_IDENTIFIER,
            "case {:?}: a staged certificate must differ from the default a create assigns",
            case.case
        );
    }
}
