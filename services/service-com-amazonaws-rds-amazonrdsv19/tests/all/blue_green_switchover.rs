// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `SwitchoverBlueGreenDeployment`: the call that moves PRODUCTION from
//! the databases in a deployment's blue environment to the ones in its green environment.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope the
//! router keys on, and the data-plane assertions drive a real MySQL client against the engine
//! endpoints the control plane publishes. The assertions read what the emulator answered, so a
//! handler that computed nothing could not pass them.
//!
//! The rules under test:
//!
//! * a switchover EXCHANGES the two databases a deployment pairs — the blue one is renamed out
//!   of the way to `<name>-old<N>` and keeps its data, its resource id and its automated
//!   backups, while the green one takes the name production was reached by — and the
//!   deployment reports `SWITCHOVER_IN_PROGRESS` until the clock settles it on
//!   `SWITCHOVER_COMPLETED`;
//! * production is the side of the pairing that takes WRITES: the green database serves reads
//!   only until the switchover and the blue one serves reads only after it, under a session
//!   that was already open as much as under a new one;
//! * a deployment that is in no state to be switched over, and an identifier that names none,
//!   are refused as their own layer words it rather than switched over.
//!
//! The tables below span what those rules generalise over: both kinds of database a
//! deployment can pair (DB instances and DB clusters), the forms `SwitchoverTimeout` takes
//! (omitted, at the smallest value the member accepts, at the documented default), and each
//! class of refusal.

use std::time::Duration;

use service_com_amazonaws_rds_amazonrdsv19::state::{
    BLUE_GREEN_STATUS_AVAILABLE, BLUE_GREEN_STATUS_PROVISIONING,
    BLUE_GREEN_STATUS_SWITCHOVER_COMPLETED, BLUE_GREEN_STATUS_SWITCHOVER_IN_PROGRESS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{
    available_instance, endpoint_of, mysql_login, MySqlSession, Refusal, Rows, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator, expect_error, expect_ok, text, texts, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents, which the ARNs below are read
/// against.
const ACCOUNT: &str = "000000000000";

/// The `BlueGreenDeploymentName` every deployment below is created under: the name is the
/// caller's and names one deployment per Region, and each case stands up its own emulator.
const DEPLOYMENT_NAME: &str = "emuSwitchover";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The `Status` a `BlueGreenDeployment` envelope reports.
///
/// The deployment carries its own `Status` AND the `Status` of each switchover detail, and the
/// two must agree: a resource in a deployment that has been switched over has been switched
/// over itself. So this reads every one of them and asserts they do.
fn reported_status(case: &str, xml: &str) -> String {
    let found = texts(xml, "Status");
    assert!(
        !found.is_empty(),
        "case {case:?}: expected a Status in {xml}"
    );
    assert!(
        found.iter().all(|status| *status == found[0]),
        "case {case:?}: the deployment's status and its switchover details' must agree, got \
         {found:?} in {xml}"
    );
    found.into_iter().next().expect("checked just above")
}

/// Which kind of database a deployment pairs: the two `CreateBlueGreenDeployment$Source`
/// accepts, each cloned into a green database of its own kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Databases {
    Instances,
    Clusters,
}

impl Databases {
    /// The ARN of the database `identifier` names, as `Source` carries it.
    fn arn(self, identifier: &str) -> String {
        match self {
            Self::Instances => format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{identifier}"),
            Self::Clusters => format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{identifier}"),
        }
    }

    /// The report that reads one of them back.
    fn describe(self) -> &'static str {
        match self {
            Self::Instances => "DescribeDBInstances",
            Self::Clusters => "DescribeDBClusters",
        }
    }

    /// The member that names one in that report's request.
    fn identifier_member(self) -> &'static str {
        match self {
            Self::Instances => "DBInstanceIdentifier",
            Self::Clusters => "DBClusterIdentifier",
        }
    }

    /// The member carrying the status it reports.
    fn status_member(self) -> &'static str {
        match self {
            Self::Instances => "DBInstanceStatus",
            Self::Clusters => "Status",
        }
    }

    /// The member carrying the resource id Amazon RDS assigned it, which "can't be changed":
    /// it is what tells WHICH database a name resolves to after the two exchange names.
    fn resource_id_member(self) -> &'static str {
        match self {
            Self::Instances => "DbiResourceId",
            Self::Clusters => "DbClusterResourceId",
        }
    }
}

/// Create the blue database of kind `kind` and poll it to `available`, answering the resource
/// id it reports.
async fn available_blue(
    router: &MultiServiceRouter,
    case: &str,
    kind: Databases,
    identifier: &str,
) -> String {
    match kind {
        Databases::Instances => {
            available_instance(router, identifier, "mysql", &params!["EngineVersion" => "8.0.39"])
                .await;
        }
        Databases::Clusters => {
            expect_ok(
                case,
                call(
                    router,
                    "CreateDBCluster",
                    &params![
                        "DBClusterIdentifier" => identifier,
                        "Engine" => "aurora-mysql",
                        "MasterUsername" => MASTER_USER,
                        "MasterUserPassword" => MASTER_PASSWORD,
                    ],
                )
                .await,
            );
        }
    }
    text(
        &poll_database_until(router, case, kind, identifier, "available").await,
        kind.resource_id_member(),
    )
}

/// Poll the report of `kind` until the database `identifier` reports `status`, answering the
/// body it settled on.
async fn poll_database_until(
    router: &MultiServiceRouter,
    case: &str,
    kind: Databases,
    identifier: &str,
    status: &str,
) -> String {
    let request = params![kind.identifier_member() => identifier];
    let (body, _) = poll_until(
        case,
        &format!("{identifier} reaching {status}"),
        || call(router, kind.describe(), &request),
        |http_status, body| {
            http_status == 200 && text(body, kind.status_member()) == status
        },
    )
    .await;
    body
}

/// Poll `DescribeBlueGreenDeployments` until the deployment `identifier` reports `status`,
/// answering the body it settled on.
async fn poll_deployment_until(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    status: &str,
) -> String {
    let request = params!["BlueGreenDeploymentIdentifier" => identifier];
    let (body, _) = poll_until(
        case,
        &format!("deployment {identifier} reaching {status}"),
        || call(router, "DescribeBlueGreenDeployments", &request),
        |http_status, body| http_status == 200 && text(body, "Status") == status,
    )
    .await;
    body
}

/// `CreateBlueGreenDeployment` over `source_arn`, answering the identifier the service
/// assigned the deployment and the identifier of the green database it cloned.
async fn create_deployment(
    router: &MultiServiceRouter,
    case: &str,
    source_arn: &str,
) -> (String, String) {
    let created = expect_ok(
        case,
        call(
            router,
            "CreateBlueGreenDeployment",
            &params![
                "BlueGreenDeploymentName" => DEPLOYMENT_NAME,
                "Source" => source_arn,
            ],
        )
        .await,
    );
    assert_eq!(
        reported_status(case, &created),
        BLUE_GREEN_STATUS_PROVISIONING,
        "case {case:?}: resources are being created in the green environment: {created}"
    );
    let green = text(&created, "Target")
        .rsplit(':')
        .next()
        .expect("the target ARN names the green database")
        .to_string();
    (text(&created, "BlueGreenDeploymentIdentifier"), green)
}

/// `SwitchoverBlueGreenDeployment`, with `SwitchoverTimeout` where the case carries one.
async fn switch_over(
    router: &MultiServiceRouter,
    identifier: &str,
    timeout: Option<i32>,
) -> (u16, String) {
    let mut request: Vec<(&str, String)> =
        params!["BlueGreenDeploymentIdentifier" => identifier].to_vec();
    if let Some(seconds) = timeout {
        request.push(("SwitchoverTimeout", seconds.to_string()));
    }
    call(router, "SwitchoverBlueGreenDeployment", &request).await
}

/// One row of the switchover table.
struct SwitchoverCase {
    case: &'static str,
    kind: Databases,
    /// What `SwitchoverTimeout` carries, where the row sends it.
    timeout: Option<i32>,
}

/// The rows: both kinds of database a deployment pairs, and each form the timeout takes —
/// omitted (its documented default of 300 applies), at the smallest value the member accepts
/// (its modelled `range 30..`) and at that default.
const SWITCHOVER_CASES: &[SwitchoverCase] = &[
    SwitchoverCase {
        case: "DB instances, no timeout",
        kind: Databases::Instances,
        timeout: None,
    },
    SwitchoverCase {
        case: "DB instances at the smallest timeout the member accepts",
        kind: Databases::Instances,
        timeout: Some(30),
    },
    SwitchoverCase {
        case: "DB clusters at the documented default timeout",
        kind: Databases::Clusters,
        timeout: Some(300),
    },
    SwitchoverCase {
        case: "DB clusters, no timeout",
        kind: Databases::Clusters,
        timeout: None,
    },
];

#[tokio::test]
async fn a_switchover_exchanges_the_names_of_the_two_environments() {
    for SwitchoverCase {
        case,
        kind,
        timeout,
    } in SWITCHOVER_CASES
    {
        let router = emulator();
        let blue = "emu-switchover-blue";
        let retired = format!("{blue}-old1");
        let source_arn = kind.arn(blue);
        let blue_resource_id = available_blue(&router, case, *kind, blue).await;

        let (identifier, green) = create_deployment(&router, case, &source_arn).await;
        let green_resource_id = text(
            &poll_database_until(&router, case, *kind, &green, "available").await,
            kind.resource_id_member(),
        );
        poll_deployment_until(&router, case, &identifier, BLUE_GREEN_STATUS_AVAILABLE).await;

        // The accepted switchover reports the exchange it performed: production is reached by
        // the name the blue database had, and the blue database is now the `-old1` one.
        let switched = expect_ok(case, switch_over(&router, &identifier, *timeout).await);
        assert_eq!(
            reported_status(case, &switched),
            BLUE_GREEN_STATUS_SWITCHOVER_IN_PROGRESS,
            "case {case:?}: the accepted switchover reports it in progress: {switched}"
        );
        for (member, expected) in [
            ("Source", kind.arn(&retired)),
            ("Target", source_arn.clone()),
            ("SourceMember", kind.arn(&retired)),
            ("TargetMember", source_arn.clone()),
        ] {
            let reported = text(&switched, member);
            assert_eq!(
                reported, expected,
                "case {case:?}: {member} must name the database the switchover left it on, got \
                 {reported:?} in {switched}"
            );
        }

        // It settles on SWITCHOVER_COMPLETED on its own, and the pairing it reports then is
        // the one the call answered with.
        let completed =
            poll_deployment_until(&router, case, &identifier, BLUE_GREEN_STATUS_SWITCHOVER_COMPLETED)
                .await;
        assert_eq!(
            text(&completed, "Source"),
            kind.arn(&retired),
            "case {case:?}: the deployment's source is the retired blue database: {completed}"
        );
        assert_eq!(
            text(&completed, "Target"),
            source_arn,
            "case {case:?}: the deployment's target is production: {completed}"
        );

        // WHICH database each name resolves to now, read by the resource id neither of them
        // can change: production is the database the green environment was cloned into, and
        // the retired name is the one that was production.
        let production = poll_database_until(&router, case, *kind, blue, "available").await;
        assert_eq!(
            text(&production, kind.resource_id_member()),
            green_resource_id,
            "case {case:?}: production must be the database the green environment held: \
             {production}"
        );
        let retired_database =
            poll_database_until(&router, case, *kind, &retired, "available").await;
        assert_eq!(
            text(&retired_database, kind.resource_id_member()),
            blue_resource_id,
            "case {case:?}: the retired name must be the database production was served from: \
             {retired_database}"
        );

        // And the AUTOMATED BACKUPS of the database left behind are retained: it was not
        // deleted, so the backups taken of it are still reported for it, under the name it
        // carries now.
        if *kind == Databases::Instances {
            let backups = expect_ok(
                case,
                call(
                    &router,
                    "DescribeDBInstanceAutomatedBackups",
                    &params!["DbiResourceId" => blue_resource_id.clone()],
                )
                .await,
            );
            assert_eq!(
                text(&backups, "DBInstanceIdentifier"),
                retired,
                "case {case:?}: the retired database's automated backup is reported under the \
                 name it carries now: {backups}"
            );
            assert_eq!(
                text(&backups, "Status"),
                "active",
                "case {case:?}: an automated backup of a database that still exists is \
                 active: {backups}"
            );
        }
    }
}

#[tokio::test]
async fn a_deployment_that_cannot_be_switched_over_is_refused_rather_than_switched() {
    let router = emulator();
    let blue = "emu-switchover-refused";
    let source_arn = Databases::Instances.arn(blue);
    available_blue(&router, "the blue database", Databases::Instances, blue).await;
    let (identifier, green) =
        create_deployment(&router, "the deployment", &source_arn).await;

    // `BlueGreenDeploymentIdentifier` is system-assigned: an arbitrary value is one the
    // parameter does not accept, while one of the assigned form naming no deployment is the
    // modelled not-found fault. Both layers hold here exactly as they do for the delete.
    for (case, named, status, code) in [
        (
            "an arbitrary identifier",
            "emu-not-a-deployment",
            400,
            "InvalidParameterValue",
        ),
        (
            "an assigned identifier naming no deployment",
            "bgd-0123456789abcdef",
            404,
            "BlueGreenDeploymentNotFoundFault",
        ),
    ] {
        expect_error(case, switch_over(&router, named, None).await, status, code);
    }

    // A deployment still PROVISIONING its green environment is in no state to be switched
    // over: the resources production would move to are not there yet.
    let refused = expect_error(
        "a deployment still provisioning",
        switch_over(&router, &identifier, None).await,
        400,
        "InvalidBlueGreenDeploymentStateFault",
    );
    let message = text(&refused, "Message");
    assert!(
        message.contains(&identifier) && message.contains(BLUE_GREEN_STATUS_PROVISIONING),
        "the refusal must name the deployment and the state it is in, got {message:?}"
    );

    // `SwitchoverTimeout` is modelled `range 30..`, so a shorter one is refused at the
    // boundary rather than served; the code that renders a modelled constraint is the
    // framework's, so only the status is read here.
    poll_deployment_until(&router, "the deployment", &identifier, BLUE_GREEN_STATUS_AVAILABLE)
        .await;
    let (status, body) = switch_over(&router, &identifier, Some(29)).await;
    assert_eq!(
        status, 400,
        "a timeout below the modelled bound must be refused, got {body}"
    );

    // The deployment switches over once, and the second call finds it in a state it cannot be
    // performed in — production has already moved.
    expect_ok(
        "the switchover",
        switch_over(&router, &identifier, None).await,
    );
    for (case, expected_state) in [
        ("a switchover already running", BLUE_GREEN_STATUS_SWITCHOVER_IN_PROGRESS),
        ("a deployment already switched over", BLUE_GREEN_STATUS_SWITCHOVER_COMPLETED),
    ] {
        if expected_state == BLUE_GREEN_STATUS_SWITCHOVER_COMPLETED {
            poll_deployment_until(&router, case, &identifier, expected_state).await;
        }
        let refused = expect_error(
            case,
            switch_over(&router, &identifier, None).await,
            400,
            "InvalidBlueGreenDeploymentStateFault",
        );
        let message = text(&refused, "Message");
        assert!(
            message.contains(expected_state),
            "case {case:?}: the refusal must name the state the deployment is in, got \
             {message:?}"
        );
    }

    // And `DeleteTarget` "can't be specified if the blue/green deployment status is
    // SWITCHOVER_COMPLETED": the resources in the green environment ARE production now, so
    // the member that deletes them is refused.
    expect_error(
        "DeleteTarget after the switchover",
        call(
            &router,
            "DeleteBlueGreenDeployment",
            &params![
                "BlueGreenDeploymentIdentifier" => identifier.clone(),
                "DeleteTarget" => "true",
            ],
        )
        .await,
        400,
        "InvalidParameterCombination",
    );
    // The green database production moved to is still there, under the name production is
    // reached by, because nothing deleted it.
    let production = poll_database_until(
        &router,
        "production after the refused delete",
        Databases::Instances,
        blue,
        "available",
    )
    .await;
    assert!(
        !production.contains(&green),
        "the green database took production's name, so its own is gone: {production}"
    );
    // The delete itself is still served, and takes neither database with it.
    expect_ok(
        "the delete",
        call(
            &router,
            "DeleteBlueGreenDeployment",
            &params!["BlueGreenDeploymentIdentifier" => identifier],
        )
        .await,
    );
}

/// The rows a statement answered with, on a session that must serve it.
async fn rows(session: &mut MySqlSession, case: &str, sql: &str) -> Rows {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"))
}

/// The refusal a statement a session must NOT serve answered with.
async fn refusal(session: &mut MySqlSession, case: &str, sql: &str) -> Refusal {
    match session.query(sql).await {
        Err(refusal) => refusal,
        Ok(answered) => panic!("case {case:?}: {sql:?} must be refused, got {answered:?}"),
    }
}

/// What `SELECT @@read_only` answers on a session: `1` on an endpoint that serves no writes.
async fn read_only(session: &mut MySqlSession, case: &str) -> Option<String> {
    rows(session, case, "SELECT @@read_only")
        .await
        .only()
        .map(str::to_string)
}

/// MySQL's own code for a write on a server running read-only
/// (`ER_OPTION_PREVENTS_STATEMENT`), which is what a client branches on.
const MYSQL_READ_ONLY_CODE: &str = "1290";

/// How long the switchover's own transition is given before this suite calls it unobservable.
/// Generous next to the span this service owes it in, so a loaded machine cannot fail it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(20);

#[tokio::test]
async fn production_takes_the_writes_and_the_retired_environment_serves_reads_only() {
    let router = emulator();
    let case = "a MySQL blue/green deployment";
    let blue = "emu-switchover-mysql";
    let retired = format!("{blue}-old1");
    let source_arn = Databases::Instances.arn(blue);
    let (blue_address, blue_port) =
        available_instance(&router, blue, "mysql", &params!["EngineVersion" => "8.0.39"]).await;

    // A row production holds before the environment is cloned, which the clone has to carry.
    let mut was_production = mysql_login(
        &blue_address,
        blue_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted on the blue endpoint");
    rows(
        &mut was_production,
        case,
        "CREATE TABLE orders (id int PRIMARY KEY, memo varchar(32))",
    )
    .await;
    rows(
        &mut was_production,
        case,
        "INSERT INTO orders (id, memo) VALUES (1, 'placed')",
    )
    .await;

    let (identifier, green) = create_deployment(&router, case, &source_arn).await;
    let green_described =
        poll_database_until(&router, case, Databases::Instances, &green, "available").await;
    let (green_address, green_port) = endpoint_of(&green_described);
    let mut becomes_production = mysql_login(
        &green_address,
        green_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted on the green endpoint");
    poll_deployment_until(&router, case, &identifier, BLUE_GREEN_STATUS_AVAILABLE).await;

    // Before the switchover production is the blue environment: it takes the writes and the
    // green one serves reads only.
    assert_eq!(
        read_only(&mut was_production, "the blue environment before the switchover").await,
        Some("0".to_string()),
        "production must take writes before the switchover"
    );
    assert_eq!(
        read_only(&mut becomes_production, "the green environment before the switchover").await,
        Some("1".to_string()),
        "the green environment must serve reads only before the switchover"
    );

    expect_ok(case, switch_over(&router, &identifier, Some(300)).await);
    poll_deployment_until(
        &router,
        case,
        &identifier,
        BLUE_GREEN_STATUS_SWITCHOVER_COMPLETED,
    )
    .await;

    // The roles have exchanged under the sessions that were already open: the write role is
    // read for every statement, so neither session had to be reopened to see the switchover.
    // Each is polled for the CONDITION, because the two environments exchange roles as the
    // deployment's own transition settles.
    for (what, session, expected) in [
        ("the retired environment", &mut was_production, "1"),
        ("production", &mut becomes_production, "0"),
    ] {
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let reported = read_only(session, what).await;
            if reported.as_deref() == Some(expected) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {what:?}: expected @@read_only {expected:?} after the switchover, got \
                 {reported:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    // The database left behind refuses every write in the engine's own words, and still holds
    // the rows it held: it was renamed and retired, not emptied.
    let refused = refusal(
        &mut was_production,
        "the retired environment",
        "INSERT INTO orders (id, memo) VALUES (2, 'late')",
    )
    .await;
    assert_eq!(
        refused.code.as_str(),
        MYSQL_READ_ONLY_CODE,
        "the retired environment must refuse a write with MySQL's own read-only code, got \
         {refused:?}"
    );
    let held = rows(
        &mut was_production,
        "the retired environment",
        "SELECT memo FROM orders WHERE id = 1",
    )
    .await;
    assert_eq!(
        held.only(),
        Some("placed"),
        "the retired environment keeps its data, got {held:?}"
    );

    // Production takes the write, over the copy of the data it was cloned from.
    rows(
        &mut becomes_production,
        "production",
        "INSERT INTO orders (id, memo) VALUES (2, 'switched')",
    )
    .await;
    let served = rows(
        &mut becomes_production,
        "production",
        "SELECT memo FROM orders WHERE id = 2",
    )
    .await;
    assert_eq!(
        served.only(),
        Some("switched"),
        "production must serve the row it just took, got {served:?}"
    );
    let cloned = rows(
        &mut becomes_production,
        "production",
        "SELECT memo FROM orders WHERE id = 1",
    )
    .await;
    assert_eq!(
        cloned.only(),
        Some("placed"),
        "production holds the rows the environment was cloned from, got {cloned:?}"
    );

    // And each database publishes the endpoint it serves on under the name it carries now, so
    // a client that resolves production by name reaches the environment that took it over.
    let production =
        poll_database_until(&router, case, Databases::Instances, blue, "available").await;
    assert_eq!(
        endpoint_of(&production),
        (green_address, green_port),
        "production publishes the endpoint the green environment serves: {production}"
    );
    let retired_database =
        poll_database_until(&router, case, Databases::Instances, &retired, "available").await;
    assert_eq!(
        endpoint_of(&retired_database),
        (blue_address, blue_port),
        "the retired database publishes the endpoint it has always served: {retired_database}"
    );
}
