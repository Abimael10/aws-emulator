// SPDX-License-Identifier: CC0-1.0

//! What `AddRoleToDBInstance` needs of the DB INSTANCE it addresses: the status it is in, the
//! number of associations it already holds, and the Region the request was sent to.
//!
//! The rules under test, each asserted over a table spanning what it generalises over rather
//! than over the one request that showed it:
//!
//! 1. "To add a role to a DB instance, the status of the DB instance must be available", so an
//!    association on an instance in any other status is the modelled `InvalidDBInstanceState`
//!    (HTTP 400) and records nothing. Amazon RDS words it by whether the instance will LEAVE
//!    the status — fixed text stating the state the call needs, which is what a caller polls
//!    on — or cannot, which names the status the instance is in. The refusal is about the
//!    STATE alone: the very same call is taken once the instance reports `available`, whether
//!    it got there by coming up or by being started again.
//! 2. A DB instance carries at most the published number of IAM role associations ("IAM roles
//!    per DB instance"). Every association up to that bound is recorded and reported `ACTIVE`;
//!    the one PAST it is the modelled `DBInstanceRoleQuotaExceeded` (HTTP 400), whose sentence
//!    names the instance and the maximum, and records nothing. The bound is on what the
//!    instance holds RIGHT NOW: a disassociation frees a slot, and the association that was
//!    refused is then taken.
//! 3. `DBInstanceIdentifier` names an instance of the REQUEST's own Region, folded the way the
//!    store keys it — the identifier as it was created, in another case, or the instance's ARN
//!    all address the one instance — and an identifier that Region holds no instance under is
//!    the modelled `DBInstanceNotFound` (HTTP 404), including one that names an instance in
//!    ANOTHER Region. The addressed instance is resolved BEFORE the role is looked up, so a
//!    request that also names a role IAM holds nothing under is still the not-found fault, and
//!    the instance that does exist elsewhere is left with no association.
//!
//! Every call goes over the wire against this service mounted beside the emulated IAM the
//! trust decision resolves against, exactly as the assembled router mounts them, and every
//! assertion reads the XML the emulator answered with. Each instance is waited for through the
//! one poll loop this test target shares (`lifecycle_poll_loops::poll_until`) — the CONDITION,
//! never a fixed span. The sentences and the quota are spelled out here rather than taken from
//! the code under test, so the table pins them.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, call_in, expect_error, expect_ok, text, the_instance, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{associated_roles, emulator, instance_associated_roles, role};

/// The canonical account id every generated emulator presents, which the instance ARN below
/// is read against.
const ACCOUNT: &str = "000000000000";

/// A second Region, for the rows that address an instance outside the one it was created in.
const OTHER_REGION: &str = "eu-west-1";

/// The service principal a role Amazon RDS acts under for a DB instance feature must trust.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// The master user every instance of its own below is created with, and its password — test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The engine, class and storage of the standalone instances below.
const ENGINE: &str = "postgres";
const CLASS: &str = "db.t3.micro";
const STORAGE: &str = "20";

/// The `FeatureName` the state rows associate a role for: one of the
/// `SupportedFeatureNames` the engine above publishes, so the only thing deciding those
/// answers is the instance's STATUS.
const FEATURE: &str = "s3Import";

/// The statuses a DB instance reports on the way through the lifecycles below.
const AVAILABLE: &str = "available";
const STOPPED: &str = "stopped";
const DELETING: &str = "deleting";

/// The status an association reports once it holds.
const ACTIVE: &str = "ACTIVE";

/// The sentence a state refusal carries for a status the instance will LEAVE: entirely fixed
/// text, naming neither the instance, nor the role, nor the status itself.
const NOT_AVAILABLE: &str = "Database instance is not in available state.";

/// The sentence a state refusal carries for a status the instance can NEVER leave: it names
/// the status the instance is in and says that waiting for it to change is futile.
fn terminal_state_sentence(status: &str) -> String {
    format!(
        "The specified database instance is currently in {status} state, which is a terminal \
         state."
    )
}

/// How many IAM roles one DB instance may have associated with it at once: the "IAM roles per
/// DB instance" quota Amazon RDS publishes (Quotas and constraints for Amazon RDS), which is
/// what the modelled quota fault reports having reached. Stated here rather than read from the
/// code under test, so the table pins the bound as well as the wording.
const ROLES_PER_INSTANCE: usize = 5;

/// The sentence the quota fault carries: the instance and the maximum it already holds.
fn quota_sentence(identifier: &str) -> String {
    format!(
        "The DB instance {identifier} already has {ROLES_PER_INSTANCE} IAM roles associated \
         with it, which is the maximum."
    )
}

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// `CreateDBInstance` for a standalone instance of [`ENGINE`] in `region`.
async fn create_instance_in(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "CreateDBInstance",
        &params![
            "DBInstanceIdentifier" => identifier,
            "Engine" => ENGINE,
            "DBInstanceClass" => CLASS,
            "AllocatedStorage" => STORAGE,
            "MasterUsername" => MASTER_USER,
            "MasterUserPassword" => MASTER_PASSWORD,
        ],
    )
    .await
}

/// The `DBInstanceStatus` the instance `identifier` reports in `region` right now.
async fn instance_status_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    identifier: &str,
) -> String {
    let described = expect_ok(
        case,
        call_in(
            router,
            region,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
    );
    text(&the_instance(case, &described), "DBInstanceStatus")
}

/// Poll `DescribeDBInstances` in `region` until the instance reports `status`: the CONDITION
/// the rows below wait for, through a REPORTING call and nothing else, rather than for a fixed
/// span.
async fn poll_instance_until_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    identifier: &str,
    status: &str,
) {
    let described = params!["DBInstanceIdentifier" => identifier];
    poll_until(
        case,
        &format!("the DB instance {identifier} reaching {status}"),
        || call_in(router, region, "DescribeDBInstances", &described),
        |http_status, body| {
            http_status == 200 && text(&the_instance(case, body), "DBInstanceStatus") == status
        },
    )
    .await;
}

/// Poll an instance of [`REGION`] until it reports `status`.
async fn poll_instance_until(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    status: &str,
) {
    poll_instance_until_in(router, REGION, case, identifier, status).await;
}

/// Create a standalone instance of [`ENGINE`] in [`REGION`] and poll until it reports
/// `available`, which is the status an association needs. Named for the engine it creates, so
/// it cannot be confused with the shared fixture's `available_instance`, which takes the engine
/// and waits out the transition span instead of polling.
async fn available_postgres_instance(router: &MultiServiceRouter, identifier: &str) {
    expect_ok(identifier, create_instance_in(router, REGION, identifier).await);
    poll_instance_until(router, identifier, identifier, AVAILABLE).await;
}

/// `AddRoleToDBInstance` for one (instance, role, feature), with the instance spelled exactly
/// as the row spells it.
async fn add_role(
    router: &MultiServiceRouter,
    named: &str,
    role_arn: &str,
    feature: &str,
) -> (u16, String) {
    call(
        router,
        "AddRoleToDBInstance",
        &params![
            "DBInstanceIdentifier" => named,
            "RoleArn" => role_arn,
            "FeatureName" => feature,
        ],
    )
    .await
}

/// The `AssociatedRoles` the instance `identifier` reports in `region`.
async fn instance_roles_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, String)> {
    let described = expect_ok(
        case,
        call_in(
            router,
            region,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
    );
    associated_roles(&described)
}

/// The `AssociatedRoles` an instance of [`REGION`] reports, through the one read of that list
/// the shared fixture offers its suites ([`instance_associated_roles`]); [`instance_roles_in`]
/// is the Region-parameterised reading this suite needs beside it.
async fn instance_roles(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, String)> {
    instance_associated_roles(router, case, identifier).await
}

/// Assert the `Message` of an answered error.
fn expect_message(case: &str, body: &str, expected: &str) {
    let message = text(body, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
}

/// Assert one state refusal: the modelled fault, its code, and the sentence for the status the
/// instance is in. The call is made with nothing read in between, because the status an
/// instance is in while the clock still owes it a transition is the status of a MOMENT.
async fn expect_state_refusal(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    role_arn: &str,
    expected_sentence: &str,
) {
    let body = expect_error(
        case,
        add_role(router, identifier, role_arn, FEATURE).await,
        400,
        "InvalidDBInstanceState",
    );
    expect_message(case, &body, expected_sentence);
    let message = text(&body, "Message");
    assert!(
        !message.contains(role_arn) && !message.contains(FEATURE),
        "case {case:?}: the refusal must name neither the role nor the feature, got {message:?}"
    );
}

/// Rule 1: an association needs the instance `available` — every other status is the modelled
/// state fault with the sentence that status is worded by, recording nothing, and the same call
/// is taken once the instance is available again.
#[tokio::test]
async fn an_association_needs_the_db_instance_available() {
    let (router, _engine) = emulator();
    let role_arn = role(&router, "emu-instance-state-role", RDS_PRINCIPAL, None).await;

    // A status the clock still owes a transition out of: the association arrives inside the
    // span `CreateDBInstance` owes, so the instance is still coming up.
    let coming_up = "emu-instance-state-creating";
    expect_ok(coming_up, create_instance_in(&router, REGION, coming_up).await);
    let status = instance_status_in(&router, REGION, coming_up, coming_up).await;
    assert_ne!(
        status, AVAILABLE,
        "case {coming_up:?}: the instance must still be coming up when the association arrives"
    );
    expect_state_refusal(&router, coming_up, coming_up, &role_arn, NOT_AVAILABLE).await;
    let reported = instance_roles(&router, coming_up, coming_up).await;
    assert!(
        reported.is_empty(),
        "case {coming_up:?}: a refused association records nothing, got {reported:?}"
    );

    // The refusal was about the STATE: once the clock has settled the instance, the very same
    // call is taken and the association is reported.
    poll_instance_until(&router, coming_up, coming_up, AVAILABLE).await;
    expect_ok(coming_up, add_role(&router, coming_up, &role_arn, FEATURE).await);
    let reported = instance_roles(&router, coming_up, coming_up).await;
    assert_eq!(
        reported,
        vec![(role_arn.clone(), FEATURE.to_string(), ACTIVE.to_string())],
        "case {coming_up:?}: expected the association once the instance is available, got \
         {reported:?}"
    );

    // And a SETTLED status that is not `available`: an instance whose stop has finished. It
    // will leave that status, but only when the caller starts it — so the refusal is still the
    // one that states the state the call needs.
    let stopped = "emu-instance-state-stopped";
    available_postgres_instance(&router, stopped).await;
    expect_ok(
        stopped,
        call(
            &router,
            "StopDBInstance",
            &params!["DBInstanceIdentifier" => stopped],
        )
        .await,
    );
    poll_instance_until(&router, stopped, stopped, STOPPED).await;
    expect_state_refusal(&router, stopped, stopped, &role_arn, NOT_AVAILABLE).await;
    let reported = instance_roles(&router, stopped, stopped).await;
    assert!(
        reported.is_empty(),
        "case {stopped:?}: a refused association records nothing, got {reported:?}"
    );

    // Started again, the same call is taken: nothing about the instance but its status stood
    // in the way.
    expect_ok(
        stopped,
        call(
            &router,
            "StartDBInstance",
            &params!["DBInstanceIdentifier" => stopped],
        )
        .await,
    );
    poll_instance_until(&router, stopped, stopped, AVAILABLE).await;
    expect_ok(stopped, add_role(&router, stopped, &role_arn, FEATURE).await);
    let reported = instance_roles(&router, stopped, stopped).await;
    assert_eq!(
        reported,
        vec![(role_arn.clone(), FEATURE.to_string(), ACTIVE.to_string())],
        "case {stopped:?}: expected the association once the instance is started, got \
         {reported:?}"
    );

    // A status the instance can never leave: one on its way out. No wait takes it to
    // `available`, so the sentence names the status it is in rather than the state the call
    // needs.
    let going_out = "emu-instance-state-deleting";
    available_postgres_instance(&router, going_out).await;
    expect_ok(
        going_out,
        call(
            &router,
            "DeleteDBInstance",
            &params![
                "DBInstanceIdentifier" => going_out,
                "SkipFinalSnapshot" => "true",
            ],
        )
        .await,
    );
    expect_state_refusal(
        &router,
        going_out,
        going_out,
        &role_arn,
        &terminal_state_sentence(DELETING),
    )
    .await;
}

/// The engine the quota instance below runs. Aurora PostgreSQL publishes the most
/// `SupportedFeatureNames` of any engine in this service's catalogue — `Bedrock`,
/// `Comprehend`, `Lambda`, `SageMaker`, `s3Export`, `s3Import`, the list
/// `DBEngineVersion$SupportedFeatureNames` reports for it through `DescribeDBEngineVersions`,
/// which is where a client reads them — so a table of DISTINCT feature names reaching one PAST
/// the quota fits inside it.
const QUOTA_ENGINE: &str = "aurora-postgresql";

/// The class a member of an Aurora DB cluster is created with. An instance of a cluster engine
/// has no master credential of its own and no storage of its own: both are the cluster's.
const QUOTA_CLASS: &str = "db.r5.large";

/// One row of the quota table: the role an association names and the feature it names it for.
struct QuotaRow {
    case: &'static str,
    /// The role NAME, created in IAM trusting `rds.amazonaws.com`: an association names a role
    /// Amazon RDS can take on, which is the gate it is taken past first, so every row names
    /// such a role.
    name: &'static str,
    /// The `FeatureName` the association names. Each row names a DIFFERENT one, because a
    /// feature of a DB instance's engine acts under one role and a role is associated with one
    /// instance once — so only distinct features with distinct roles reach the quota at all.
    feature: &'static str,
}

/// [`ROLES_PER_INSTANCE`] associations that FIT, then one PAST the bound. The rows differ along
/// what the rule generalises over: the published feature names of the engine, in no particular
/// order, because the quota counts the associations an instance holds whatever they name.
const QUOTA_ROWS: &[QuotaRow] = &[
    QuotaRow {
        case: "a role for the Bedrock feature",
        name: "emu-iquota-bedrock",
        feature: "Bedrock",
    },
    QuotaRow {
        case: "a role for the S3 import feature",
        name: "emu-iquota-import",
        feature: "s3Import",
    },
    QuotaRow {
        case: "a role for the Lambda feature",
        name: "emu-iquota-lambda",
        feature: "Lambda",
    },
    QuotaRow {
        case: "a role for the S3 export feature",
        name: "emu-iquota-export",
        feature: "s3Export",
    },
    QuotaRow {
        case: "a role for the SageMaker feature",
        name: "emu-iquota-sagemaker",
        feature: "SageMaker",
    },
    QuotaRow {
        case: "one association past the published quota, for the Comprehend feature",
        name: "emu-iquota-comprehend",
        feature: "Comprehend",
    },
];

/// Rule 2: a DB instance takes the published number of role associations and refuses the one
/// past it with the modelled quota fault, whose sentence names the instance and the maximum;
/// freeing a slot lets the refused association through.
#[tokio::test]
async fn a_db_instance_takes_the_published_number_of_role_associations() {
    assert_eq!(
        QUOTA_ROWS.len(),
        ROLES_PER_INSTANCE + 1,
        "the quota table must reach exactly one association past the bound, got {} rows",
        QUOTA_ROWS.len()
    );
    let (router, _engine) = emulator();

    // A member of an Aurora DB cluster, which is how an instance of that engine exists: the
    // cluster carries the master credential and the storage.
    let cluster = "emu-iquota-cluster";
    expect_ok(
        cluster,
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => cluster,
                "Engine" => QUOTA_ENGINE,
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
            ],
        )
        .await,
    );
    let identifier = "emu-iquota-instance";
    expect_ok(
        identifier,
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => QUOTA_ENGINE,
                "DBInstanceClass" => QUOTA_CLASS,
                "DBClusterIdentifier" => cluster,
            ],
        )
        .await,
    );
    poll_instance_until(&router, identifier, identifier, AVAILABLE).await;

    let mut role_arns: Vec<String> = Vec::with_capacity(QUOTA_ROWS.len());
    for row in QUOTA_ROWS {
        role_arns.push(role(&router, row.name, RDS_PRINCIPAL, None).await);
    }

    // Every association up to the bound is recorded and reported `ACTIVE`, in the order it was
    // made.
    let mut expected: Vec<(String, String, String)> = Vec::new();
    for (row, role_arn) in QUOTA_ROWS.iter().zip(&role_arns).take(ROLES_PER_INSTANCE) {
        expect_ok(
            row.case,
            add_role(&router, identifier, role_arn, row.feature).await,
        );
        expected.push((
            role_arn.clone(),
            row.feature.to_string(),
            ACTIVE.to_string(),
        ));
        let reported = instance_roles(&router, row.case, identifier).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected the associations made so far, got {reported:?}",
            row.case
        );
    }

    // The one PAST the bound is the modelled quota fault, and records nothing.
    let past = &QUOTA_ROWS[ROLES_PER_INSTANCE];
    let past_role = &role_arns[ROLES_PER_INSTANCE];
    let body = expect_error(
        past.case,
        add_role(&router, identifier, past_role, past.feature).await,
        400,
        "DBInstanceRoleQuotaExceeded",
    );
    expect_message(past.case, &body, &quota_sentence(identifier));
    let reported = instance_roles(&router, past.case, identifier).await;
    assert_eq!(
        reported, expected,
        "case {:?}: a refused association records nothing, got {reported:?}",
        past.case
    );

    // The bound is on what the instance holds RIGHT NOW: a disassociation frees a slot, and the
    // association that was refused is then taken.
    let freed = &QUOTA_ROWS[0];
    expect_ok(
        freed.case,
        call(
            &router,
            "RemoveRoleFromDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "RoleArn" => role_arns[0],
                "FeatureName" => freed.feature,
            ],
        )
        .await,
    );
    expect_ok(
        past.case,
        add_role(&router, identifier, past_role, past.feature).await,
    );
    let mut refilled = expected[1..].to_vec();
    refilled.push((
        past_role.clone(),
        past.feature.to_string(),
        ACTIVE.to_string(),
    ));
    let reported = instance_roles(&router, past.case, identifier).await;
    assert_eq!(
        reported, refilled,
        "case {:?}: expected the freed slot to take the refused association, got {reported:?}",
        past.case
    );
}

/// How one addressing row spells the instance it addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// The identifier exactly as the instance was created.
    AsCreated,
    /// The same identifier in upper case.
    UpperCase,
    /// The ARN of the instance.
    Arn,
}

/// One row of the addressing table: how the request spells the instance, and the association it
/// makes on it.
struct AddressRow {
    case: &'static str,
    spelling: Spelling,
    /// The `FeatureName` the row associates its role for — one per row, because a feature of
    /// the engine acts under one role.
    feature: &'static str,
    /// The role NAME created for the row.
    role_name: &'static str,
}

/// The spellings `DBInstanceIdentifier` takes for one stored instance: the identifier as it was
/// created, the same identifier in another case (an identifier is stored folded, so the case of
/// the request cannot address another instance), and the instance's ARN — the form the members
/// that name a DB instance document as "the customer identifier or the ARN of your DB
/// instance". Each names a `FeatureName` of the `SupportedFeatureNames` RDS for PostgreSQL
/// publishes.
const ADDRESS_ROWS: &[AddressRow] = &[
    AddressRow {
        case: "the identifier the instance was created under",
        spelling: Spelling::AsCreated,
        feature: "s3Import",
        role_name: "emu-address-import",
    },
    AddressRow {
        case: "the same identifier in upper case",
        spelling: Spelling::UpperCase,
        feature: "s3Export",
        role_name: "emu-address-export",
    },
    AddressRow {
        case: "the ARN of the instance",
        spelling: Spelling::Arn,
        feature: "Lambda",
        role_name: "emu-address-lambda",
    },
];

/// Rule 3: the association addresses an instance of the request's own Region by the identifier
/// the store folded, and an identifier that Region holds no instance under is the modelled
/// not-found fault — the instance decides before the role is looked up.
#[tokio::test]
async fn an_association_addresses_the_instance_of_the_requests_region() {
    let (router, _engine) = emulator();
    let identifier = "emu-role-address";
    available_postgres_instance(&router, identifier).await;

    let mut expected: Vec<(String, String, String)> = Vec::new();
    for row in ADDRESS_ROWS {
        let named = match row.spelling {
            Spelling::AsCreated => identifier.to_string(),
            Spelling::UpperCase => identifier.to_ascii_uppercase(),
            Spelling::Arn => format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{identifier}"),
        };
        let role_arn = role(&router, row.role_name, RDS_PRINCIPAL, None).await;
        expect_ok(
            row.case,
            add_role(&router, &named, &role_arn, row.feature).await,
        );
        expected.push((
            role_arn.clone(),
            row.feature.to_string(),
            ACTIVE.to_string(),
        ));
        // Every spelling addressed the SAME instance, so the one instance reports every
        // association made so far.
        let reported = instance_roles(&router, row.case, identifier).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected every spelling to address one instance, got {reported:?}",
            row.case
        );
    }

    // An instance of ANOTHER Region: RDS is regional, so its identifier names no instance of
    // the Region the request was sent to.
    let elsewhere = "emu-role-elsewhere";
    expect_ok(
        elsewhere,
        create_instance_in(&router, OTHER_REGION, elsewhere).await,
    );
    poll_instance_until_in(&router, OTHER_REGION, elsewhere, elsewhere, AVAILABLE).await;

    // (case, the identifier the request names, the role it names)
    let held_role = role(&router, "emu-address-notfound", RDS_PRINCIPAL, None).await;
    let absent_role = format!("arn:aws:iam::{ACCOUNT}:role/emu-address-nowhere");
    let not_found: &[(&str, &str, &str)] = &[
        (
            "an identifier no Region holds an instance under",
            "emu-role-no-such-instance",
            held_role.as_str(),
        ),
        (
            "an instance of another Region",
            elsewhere,
            held_role.as_str(),
        ),
        (
            // The instance is resolved BEFORE the role is looked up in IAM, so this is the
            // not-found fault of the instance and not a refusal of the role.
            "an absent instance named with a role IAM holds nothing under",
            "emu-role-no-such-instance-either",
            absent_role.as_str(),
        ),
    ];
    for (case, named, role_arn) in not_found {
        let body = expect_error(
            case,
            add_role(&router, named, role_arn, FEATURE).await,
            404,
            "DBInstanceNotFound",
        );
        expect_message(case, &body, &format!("DBInstance not found: {named}"));
    }

    // Neither refused call recorded anything: the instance of the other Region still reports no
    // association in the Region that does hold it, and the one here reports exactly the three
    // the spellings above made.
    let reported = instance_roles_in(&router, OTHER_REGION, elsewhere, elsewhere).await;
    assert!(
        reported.is_empty(),
        "case {elsewhere:?}: expected the instance of the other Region to hold no association, \
         got {reported:?}"
    );
    let reported = instance_roles(&router, "every spelling", identifier).await;
    assert_eq!(
        reported, expected,
        "case {identifier:?}: expected the three associations the spellings made, got \
         {reported:?}"
    );
}
