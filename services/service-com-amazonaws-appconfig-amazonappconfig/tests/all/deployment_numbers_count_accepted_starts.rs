// SPDX-License-Identifier: CC0-1.0

//! The per-environment `DeploymentNumber` sequence counts the deployments AppConfig
//! CREATED.
//!
//! RULE: `DeploymentNumber` is "the sequence number of the deployment"
//! (`smithy_query(query="operation",operation="StartDeployment")`), issued per environment
//! by the `StartDeployment` calls AppConfig accepts — so a `StartDeployment` AppConfig
//! REFUSES creates nothing and issues no number, whichever class of refusal it is, and the
//! next deployment the environment accepts is numbered one past the last deployment it
//! holds. A number is an identity, not a slot: nothing reissues it, so the numbers one
//! environment holds stay the contiguous run the accepted starts built.
//!
//! The rows span what the rule generalises over: the CLASS of the refusal (the modelled
//! `ConflictException` for an environment still working on a deployment, and the
//! `ResourceNotFoundException` of each reference the body carries — the configuration
//! profile, the version of it, and the deployment strategy), and, for the conflict, the
//! phase of flight the refused start arrives in (a rollout still growing, and a final bake).
//! The recorded trajectory — a start, a conflicting start refused while the first bakes, a
//! stop that rolls it back, and then the next deployment — is one row of it.
//!
//! The half of that trajectory the sequence is read through is asserted here too, because a
//! number only means something if the deployment it names can be addressed: the stop of a
//! deployment AppConfig has not finished with is its modelled 202 leaving the deployment
//! `ROLLED_BACK`, and the reader answers that same record under that same number.
//!
//! Every wait is on the CONDITION a phase makes true, re-read until it holds; nothing here
//! asserts how long a phase took.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, deployment_in_state,
    instant_deployment_strategy, json_call, router, send, Answer,
};
use crate::deployment_wire_fidelity::{create_strategy, host_version, member, Row};

/// `StartDeployment`: `POST
/// /applications/{ApplicationId}/environments/{EnvironmentId}/deployments`.
async fn start(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    profile: &str,
    strategy: &str,
    version: &str,
) -> Answer {
    json_call(
        router,
        "POST",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version,
        }),
    )
    .await
}

/// `StopDeployment`: `DELETE
/// /applications/{ApplicationId}/environments/{EnvironmentId}/deployments/{DeploymentNumber}`,
/// asking for no revert.
async fn stop(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    number: i64,
) -> Answer {
    send(
        router,
        "DELETE",
        &format!("/applications/{application}/environments/{environment}/deployments/{number}"),
        "application/json",
        Vec::new(),
        &[],
    )
    .await
}

/// `GetDeployment` of one deployment, with the status it answered.
async fn read(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    number: i64,
) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments/{number}"),
        &Value::Null,
    )
    .await
}

/// Every `DeploymentNumber` `ListDeployments` reports for one environment, in ascending
/// order so a row compares the RUN of numbers the environment holds rather than the order
/// the listing serves them in.
async fn held_numbers(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
) -> Vec<i64> {
    let answer = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: ListDeployments must answer 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let mut numbers: Vec<i64> = answer
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.get("DeploymentNumber").and_then(Value::as_i64))
        .collect();
    numbers.sort_unstable();
    numbers
}

/// What the environment is doing when the refused start arrives.
enum Context {
    /// Nothing in flight, so the refusal is a lookup of what the body named.
    Idle,
    /// A deployment of its own holds the environment: the criteria of the strategy pacing
    /// it, and the `DeploymentState` the row waits for it to report before the refused
    /// start is sent.
    InFlight { row: Row, phase: &'static str },
}

/// Which reference of the refused start resolves to nothing — or none of them, which is the
/// body a busy environment refuses anyway.
enum Body {
    /// Every reference resolves: only the environment's own state refuses this one.
    Resolvable,
    /// A configuration profile id no application holds.
    UnknownProfile,
    /// A configuration profile that holds no hosted version at all.
    VersionlessProfile,
    /// A version the named configuration profile does not hold.
    UnknownVersion,
    /// A deployment strategy id nothing holds.
    UnknownStrategy,
}

/// The fixture a row's body is spelled against: the ids this suite created once.
struct Fixture {
    profile: String,
    /// A second configuration profile, created with no hosted version.
    versionless_profile: String,
    /// The strategy a row's accepted deployments are paced by.
    strategy: String,
}

impl Body {
    /// The `(ConfigurationProfileId, ConfigurationVersion, DeploymentStrategyId)` this body
    /// sends.
    fn references<'a>(&self, fixture: &'a Fixture) -> (&'a str, &'static str, &'a str) {
        match self {
            // A profile id and a strategy id that satisfy the members' own patterns
            // (`Id` is `^[a-z0-9]{4,7}$`, and `DeploymentStrategyId` is
            // `^(^[a-z0-9]{4,7}$|^AppConfig\.[A-Za-z0-9]{9,40}$)$`) and that nothing
            // created or predefined carries.
            Body::Resolvable => (&fixture.profile, "1", &fixture.strategy),
            Body::UnknownProfile => ("nosuch", "1", &fixture.strategy),
            Body::VersionlessProfile => (&fixture.versionless_profile, "1", &fixture.strategy),
            Body::UnknownVersion => (&fixture.profile, "4096", &fixture.strategy),
            Body::UnknownStrategy => (&fixture.profile, "1", "zzzzzz"),
        }
    }
}

/// One row: the case it is called by, the state of the environment the refused start
/// arrives in, the body it sends, and the refusal AppConfig answers it with.
struct Case {
    case: &'static str,
    context: Context,
    body: Body,
    status: u16,
    code: &'static str,
}

/// The refusal classes `StartDeployment` answers, each one asserted to leave the sequence
/// untouched. `MinutesBetween0And24Hours` is `range 0..1440` and `Percentage` is
/// `range 1..100` (`smithy_query(query="shape",shape="DeploymentStrategy")`), so the
/// in-flight rows below pace their deployments inside those bounds: the longest rollout the
/// model allows grown one percent at a time stays in flight, and no rollout window with the
/// longest bake reaches the bake at once.
const CASES: &[Case] = &[
    Case {
        case: "a second deployment while the first is still rolling out",
        context: Context::InFlight {
            row: ("the longest rollout, one percent at a time", 1440, 1.0, "LINEAR", 0),
            phase: "DEPLOYING",
        },
        body: Body::Resolvable,
        status: 409,
        code: "ConflictException",
    },
    Case {
        case: "a second deployment while the first monitors its final bake",
        context: Context::InFlight {
            row: ("no rollout window, the longest bake", 0, 100.0, "LINEAR", 1440),
            phase: "BAKING",
        },
        body: Body::Resolvable,
        status: 409,
        code: "ConflictException",
    },
    Case {
        case: "a second deployment naming an unknown profile while the first bakes",
        context: Context::InFlight {
            row: ("no rollout window, a long bake", 0, 50.0, "LINEAR", 1440),
            phase: "BAKING",
        },
        body: Body::UnknownProfile,
        status: 409,
        code: "ConflictException",
    },
    Case {
        case: "a configuration profile no application holds",
        context: Context::Idle,
        body: Body::UnknownProfile,
        status: 404,
        code: "ResourceNotFoundException",
    },
    Case {
        case: "a configuration profile that holds no version at all",
        context: Context::Idle,
        body: Body::VersionlessProfile,
        status: 404,
        code: "ResourceNotFoundException",
    },
    Case {
        case: "a configuration version the profile does not hold",
        context: Context::Idle,
        body: Body::UnknownVersion,
        status: 404,
        code: "ResourceNotFoundException",
    },
    Case {
        case: "a deployment strategy nothing holds",
        context: Context::Idle,
        body: Body::UnknownStrategy,
        status: 404,
        code: "ResourceNotFoundException",
    },
];

/// The `DeploymentNumber` a Deployment document reports.
fn number_of(case: &str, document: &str, body: &Value) -> i64 {
    member(body, "DeploymentNumber")
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: {document} answered no DeploymentNumber, got {body}"))
}

/// Start a deployment that must be ACCEPTED, assert it is numbered one past the
/// deployments the environment already holds, and answer that number.
async fn accepted(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    strategy: &str,
    expected: i64,
) -> i64 {
    let answer = start(router, application, environment, profile, strategy, "1").await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: StartDeployment must answer its modelled 201, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let document = answer.json();
    let number = number_of(case, "StartDeployment", &document);
    assert_eq!(
        number, expected,
        "case {case:?}: the environment's next deployment is numbered one past the ones it \
         holds, expected {expected}, got {number}"
    );
    // The number is an address: the reader answers the same deployment under it.
    let read_back = read(router, application, environment, number).await;
    assert_eq!(
        read_back.status,
        200,
        "case {case:?}: GetDeployment must answer the deployment just numbered {number}, got \
         {} / {}",
        read_back.status,
        String::from_utf8_lossy(&read_back.body)
    );
    assert_eq!(
        number_of(case, "GetDeployment", &read_back.json()),
        number,
        "case {case:?}: GetDeployment must report the number it was addressed by, got {}",
        read_back.json()
    );
    number
}

/// RULE: every class of refused `StartDeployment` leaves the environment's deployment
/// sequence exactly where it was, so the next accepted deployment is numbered one past the
/// deployments the environment holds and the numbers it holds stay contiguous.
#[tokio::test]
async fn a_refused_start_issues_no_deployment_number() {
    let router = router();
    let application = create_application(&router, "deployment-sequence").await;
    let profile = create_profile(&router, &application, "config").await;
    host_version(&router, &application, &profile).await;
    let fixture = Fixture {
        profile: profile.clone(),
        versionless_profile: create_profile(&router, &application, "versionless").await,
        // A strategy that paces no stage: a deployment it paces is over when the start
        // answers, so a row that only needs the sequence to move does not wait on one.
        strategy: instant_deployment_strategy(&router, "sequence-instant").await,
    };
    // One environment for the whole table: the sequence under test is the environment's
    // own, so every row must leave it where the last row left it.
    let environment = create_environment(&router, &application, "production").await;
    // How many deployments this environment holds, which is the number the next accepted
    // start must issue past. Nothing in this suite reads it off the service.
    let mut held: i64 = 0;

    for (index, row) in CASES.iter().enumerate() {
        let case = row.case;

        // The deployment that puts the environment in this row's state is itself an
        // accepted start, so it consumes the next number.
        let in_flight = match &row.context {
            Context::Idle => None,
            Context::InFlight { row: criteria, phase } => {
                let paced = create_strategy(&router, 7_000 + index, criteria).await;
                held += 1;
                let number = accepted(
                    &router,
                    case,
                    &application,
                    &environment,
                    &profile,
                    &paced,
                    held,
                )
                .await;
                // The phase this row refuses in is owed by the CLOCK: re-read until the
                // deployment reports it.
                deployment_in_state(&router, case, &application, &environment, number, &[*phase])
                    .await;
                Some(number)
            }
        };

        let (named_profile, version, named_strategy) = row.body.references(&fixture);
        let refused = start(
            &router,
            &application,
            &environment,
            named_profile,
            named_strategy,
            version,
        )
        .await;
        assert_eq!(
            refused.status,
            row.status,
            "case {case:?}: expected the refusal's HTTP {}, got {} / {}",
            row.status,
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        let code = refused.error_code();
        assert!(
            code.contains(row.code),
            "case {case:?}: expected the modelled {}, got {code:?}",
            row.code
        );
        // The refusal created nothing: the environment holds exactly the deployments it
        // held before it.
        let numbers = held_numbers(&router, case, &application, &environment).await;
        assert_eq!(
            numbers,
            (1..=held).collect::<Vec<i64>>(),
            "case {case:?}: a refused start creates no deployment, so the environment holds \
             the contiguous run of the ones it accepted, got {numbers:?}"
        );

        // A deployment AppConfig has not finished with is stopped so the next row can
        // deploy into this environment — which is a SUCCESS that rolls it back, settled in
        // the call, and the reader answers that same record under that same number.
        if let Some(number) = in_flight {
            let stopped = stop(&router, &application, &environment, number).await;
            assert_eq!(
                stopped.status,
                202,
                "case {case:?}: StopDeployment of a deployment in flight must answer its \
                 modelled 202, got {} / {}",
                stopped.status,
                String::from_utf8_lossy(&stopped.body)
            );
            let settled = stopped.json();
            assert_eq!(
                member(&settled, "State").as_str(),
                Some("ROLLED_BACK"),
                "case {case:?}: a stop rolls the deployment back and settles it, got {settled}"
            );
            assert_eq!(
                number_of(case, "StopDeployment", &settled),
                number,
                "case {case:?}: the stop answers the deployment it was addressed by, got \
                 {settled}"
            );
            let read_back = read(&router, &application, &environment, number).await;
            assert_eq!(
                read_back.status,
                200,
                "case {case:?}: GetDeployment must still answer the stopped deployment, got {} \
                 / {}",
                read_back.status,
                String::from_utf8_lossy(&read_back.body)
            );
            assert_eq!(
                member(&read_back.json(), "State").as_str(),
                Some("ROLLED_BACK"),
                "case {case:?}: the reader answers the record the stop left, got {}",
                read_back.json()
            );
        }

        // The sequence moved for the accepted starts and for nothing else: the next
        // deployment is numbered one past the ones the environment holds.
        held += 1;
        accepted(
            &router,
            case,
            &application,
            &environment,
            &profile,
            &fixture.strategy,
            held,
        )
        .await;
        let numbers = held_numbers(&router, case, &application, &environment).await;
        assert_eq!(
            numbers,
            (1..=held).collect::<Vec<i64>>(),
            "case {case:?}: the numbers an environment holds are the contiguous run its \
             accepted starts built, got {numbers:?}"
        );
    }
}

/// RULE, read through the trajectory the sequence is addressed by: a start, a conflicting
/// start refused while the deployment bakes, a stop that rolls that deployment back, and
/// then the next deployment — which is numbered one past the stopped one, because the
/// refused start issued nothing and a rolled-back deployment keeps the number it was
/// issued.
#[tokio::test]
async fn a_stop_leaves_the_sequence_at_the_deployment_it_rolled_back() {
    let router = router();
    let application = create_application(&router, "stop-then-redeploy").await;
    let profile = create_profile(&router, &application, "config").await;
    host_version(&router, &application, &profile).await;
    let environment = create_environment(&router, &application, "production").await;
    // A bake long enough that the conflicting start below arrives while AppConfig is still
    // working on the deployment, whatever the host's pace.
    let baking = create_strategy(
        &router,
        7_100,
        &("no rollout window, the longest bake", 0, 100.0, "LINEAR", 1440),
    )
    .await;
    let case = "a stop between two deployments";

    let first = accepted(&router, case, &application, &environment, &profile, &baking, 1).await;
    deployment_in_state(&router, case, &application, &environment, first, &["BAKING"]).await;

    let refused = start(&router, &application, &environment, &profile, &baking, "1").await;
    assert_eq!(
        refused.status,
        409,
        "case {case:?}: a start into an environment AppConfig is still working on is refused, \
         got {} / {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );

    let stopped = stop(&router, &application, &environment, first).await;
    assert_eq!(
        stopped.status,
        202,
        "case {case:?}: the stop of the baking deployment must answer 202, got {} / {}",
        stopped.status,
        String::from_utf8_lossy(&stopped.body)
    );
    assert_eq!(
        member(&stopped.json(), "State").as_str(),
        Some("ROLLED_BACK"),
        "case {case:?}: the stop rolls the baking deployment back, got {}",
        stopped.json()
    );

    // One refusal and one rollback later, the sequence is where the accepted starts left
    // it: the next deployment is the second this environment ever held.
    let second = accepted(&router, case, &application, &environment, &profile, &baking, 2).await;
    assert_eq!(
        second,
        first + 1,
        "case {case:?}: the deployment after a stop is numbered one past the stopped one, got \
         {second} after {first}"
    );
    let numbers = held_numbers(&router, case, &application, &environment).await;
    assert_eq!(
        numbers,
        vec![first, second],
        "case {case:?}: the environment holds exactly the two deployments it accepted, got \
         {numbers:?}"
    );
    // The stopped deployment still answers under its own number: a number names one
    // deployment for good.
    let rolled_back = read(&router, &application, &environment, first).await;
    assert_eq!(
        member(&rolled_back.json(), "State").as_str(),
        Some("ROLLED_BACK"),
        "case {case:?}: deployment {first} is still the one the stop rolled back, got {}",
        rolled_back.json()
    );
}
