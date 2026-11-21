// SPDX-License-Identifier: CC0-1.0

//! A deployment's ticks are AppConfig's OWN work: an `AT_DEPLOYMENT_TICK` extension is
//! invoked, and the directive it answers applied, for a client that never reads the
//! deployment again.
//!
//! The rule under test: every stage of a deployment after the call that started it — each
//! growth interval of the rollout, the final bake, the completion, and the rollback a tick's
//! `ROLL_BACK` directive asks for — falls due on AppConfig's own schedule, so the extension
//! actions those stages owe, and the `AT_DEPLOYMENT_TICK` action owed once per tick of a
//! deployment still in flight, are invoked when they fall due and not when somebody next
//! asks about the deployment. A client that installs a tick extension as a kill switch and
//! then watches what that extension DOES — the queue an action forwards each event to, the
//! configuration its application is served — makes no further AppConfig call, and still
//! observes the ticks, the abort its handler directed, and the terminal state the abort left
//! (`DeploymentState`'s `ROLLED_BACK`, `smithy_query(query="shape",shape="DeploymentState")`)
//! with that rollback attributed to the EXTENSION that directed it (`TriggeredBy`'s
//! `EXTENSION`, `smithy_query(query="shape",shape="DeploymentEvent")`).
//!
//! So every wait here is on an SQS queue, and between `StartDeployment` and the verdict the
//! suite makes NO AppConfig call at all: the control plane is read once at the end, to report
//! what the deployment had already become.
//!
//! The rows span what the rule generalises over: how the rollout is paced (its `GrowthType`,
//! its `GrowthFactor` and the rollout window it declares, including the published upper bound
//! of `MinutesBetween0And24Hours`, `range 0..1440` — the declared minutes pace the rollout and
//! are not what decides whether its ticks happen) and what the steering handler answers (the
//! abort directive with a reason and without one, and an answer that steers nothing, whose
//! rollout completes on its own instead).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{create_application, create_environment, create_profile};
use crate::deployment_tick_directive::given_function;
use crate::extension_action_points::{
    associate, create_version, drain, get_deployment, given_extension,
    given_growing_strategy_of_type, given_queue, start_deployment,
};
use crate::resource_tags::arn;

/// How long a row waits for the events it expects on the queue.
///
/// A ceiling on the WAIT, not a statement about how long a rollout takes: a row that spends
/// this long is one whose ticks nothing is dispatching, which is the defect this suite pins.
/// Nothing here is decided by the clock short of it.
const OBSERVED_BY: Duration = Duration::from_secs(120);

/// How often the observing queue is re-read while waiting for that condition. The queue is
/// the ONLY thing read in the wait: see this suite's own documentation.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `Type` an `AT_DEPLOYMENT_TICK` event carries: the action point in PascalCase, as the
/// event handed to every action names its own point.
const TICK_EVENT: &str = "AtDeploymentTick";

/// What one row paces its rollout by, what its steering handler answers, and what that
/// makes of the deployment.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// `GrowthType`: `EXPONENTIAL | LINEAR`
    /// (`smithy_query(query="shape",shape="GrowthType")`).
    growth_type: &'static str,
    /// The share of targets each growth interval adds (`Percentage`, `range 1..100`).
    growth_factor: f64,
    /// The rollout window the strategy declares (`MinutesBetween0And24Hours`,
    /// `range 0..1440`).
    duration_in_minutes: i32,
    /// The JSON document the row's handler answers, verbatim.
    answer: &'static str,
    /// The terminal `DeploymentState` the deployment ends in, as the document serves it.
    expected_state: &'static str,
    /// The `Type` of the lifecycle event the observing queue sees for that ending: the
    /// action point of the stage that finished the deployment.
    expected_ending: &'static str,
    /// The `Description` the handler asked AppConfig to record for the rollback, when it
    /// supplied one.
    expected_description: Option<&'static str>,
}

/// The rows: see this suite's own documentation for what they span.
const ROWS: &[Row] = &[
    Row {
        case: "a linear rollout aborted at a tick, with a reason",
        growth_type: "LINEAR",
        growth_factor: 25.0,
        duration_in_minutes: 1,
        answer: r#"{"Directive": "ROLL_BACK", "Description": "the canary's error budget is spent"}"#,
        expected_state: "ROLLED_BACK",
        expected_ending: "OnDeploymentRolledBack",
        expected_description: Some("the canary's error budget is spent"),
    },
    Row {
        case: "an exponential rollout aborted at a tick",
        growth_type: "EXPONENTIAL",
        growth_factor: 10.0,
        duration_in_minutes: 20,
        answer: r#"{"Directive": "ROLL_BACK"}"#,
        expected_state: "ROLLED_BACK",
        expected_ending: "OnDeploymentRolledBack",
        expected_description: None,
    },
    Row {
        case: "a rollout window at the published upper bound, aborted at a tick",
        growth_type: "LINEAR",
        growth_factor: 50.0,
        duration_in_minutes: 1440,
        answer: r#"{"Directive": "ROLL_BACK", "Description": "the health signal is bad"}"#,
        expected_state: "ROLLED_BACK",
        expected_ending: "OnDeploymentRolledBack",
        expected_description: Some("the health signal is bad"),
    },
    Row {
        case: "a rollout nothing aborts",
        growth_type: "LINEAR",
        growth_factor: 100.0,
        duration_in_minutes: 2,
        answer: r#"{"Directive": "CONTINUE", "Description": "the canary is healthy"}"#,
        expected_state: "COMPLETE",
        expected_ending: "OnDeploymentComplete",
        expected_description: None,
    },
];

/// AppConfig, Lambda and Amazon SQS on ONE engine: the tick's steering target is a function
/// this emulator runs, and the queue is where a client observes what the action points did.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_appconfig_amazonappconfig::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// The handler of a steering function: whatever the row asked it to answer, for every event
/// it is handed. The answer is parsed from JSON text inside the handler, so a row states
/// exactly the document a tick target replies with.
fn steering_source(answer: &str) -> String {
    format!(
        "import json\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   return json.loads({answer:?})\n"
    )
}

/// Re-read the observing queue until it has delivered a tick event and the lifecycle event
/// of the stage that ended the deployment, and answer every event it delivered.
///
/// The wait is on the CONDITION, and ONLY the queue is read: no AppConfig call happens
/// between the start of the deployment and this answer, so what arrives here arrives because
/// the service dispatched it on its own schedule.
async fn observed(router: &MultiServiceRouter, row: &Row, queue_url: &str) -> Vec<Value> {
    let case = row.case;
    let deadline = Instant::now() + OBSERVED_BY;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(router, queue_url).await);
        let types: Vec<&str> = collected
            .iter()
            .map(|event| event["Type"].as_str().unwrap_or_default())
            .collect();
        if types.contains(&TICK_EVENT) && types.contains(&row.expected_ending) {
            return collected;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a {TICK_EVENT} event and a {} event within {}s without \
             reading the deployment, got {types:?}",
            row.expected_ending,
            OBSERVED_BY.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The newest event log entry of `event_type`, which is the first one: "the most recent
/// events are displayed first" (`smithy_query(query="shape",shape="Deployment")`).
fn newest_event(document: &Value, event_type: &str) -> Option<Value> {
    document["EventLog"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|event| event["EventType"] == json!(event_type))
}

/// The whole chain a tick drives for a client that only watches the extension's target,
/// asserted per row.
#[tokio::test]
async fn an_unattended_deployment_is_ticked_and_its_directive_applied() {
    let (router, engine) = emulator();
    let application = create_application(&router, "unattended-app").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = index + 1;
        // One environment per row: an environment works on one deployment at a time.
        let environment =
            create_environment(&router, &application, &format!("unattended-env-{suffix}")).await;
        let profile =
            create_profile(&router, &application, &format!("unattended-profile-{suffix}")).await;
        let content = format!(r#"{{"release":"candidate","row":{suffix}}}"#);
        let written = create_version(
            &router,
            &application,
            &profile,
            "application/json",
            content.as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: CreateHostedConfigurationVersion answered {} / {}",
            written.status,
            String::from_utf8_lossy(&written.body)
        );

        // The steering target: the one kind of target that can ANSWER a tick.
        let function = given_function(
            &router,
            &engine,
            case,
            &format!("unattended-steer-{suffix}"),
            &steering_source(row.answer),
        )
        .await;
        let (steering, _) = given_extension(
            &router,
            &format!("unattended-steer-ext-{suffix}"),
            &["AT_DEPLOYMENT_TICK"],
            &function,
            &[],
        )
        .await;
        // The observation target: a queue cannot answer, so it reports what the action
        // points were handed without steering anything. It watches the tick and both
        // endings, because those are what a client that never calls AppConfig again has to
        // be able to see.
        let (queue_url, queue_arn) = given_queue(&router, &format!("unattended-queue-{suffix}")).await;
        let (watching, _) = given_extension(
            &router,
            &format!("unattended-watch-ext-{suffix}"),
            &[
                "AT_DEPLOYMENT_TICK",
                "ON_DEPLOYMENT_COMPLETE",
                "ON_DEPLOYMENT_ROLLED_BACK",
            ],
            &queue_arn,
            &[],
        )
        .await;
        let environment_arn = arn(&format!("application/{application}/environment/{environment}"));
        for extension in [&steering, &watching] {
            associate(&router, case, extension, &environment_arn, &Value::Null).await;
        }

        let strategy = given_growing_strategy_of_type(
            &router,
            &format!("unattended-{suffix}"),
            row.growth_type,
            row.duration_in_minutes,
            // No final bake time: the bake keeps the minutes a caller declares, and this row
            // is about the ticks of the rollout rather than about that window.
            0,
            row.growth_factor,
        )
        .await;
        let started = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "1",
            }),
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment answered {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let start_document = started.json();
        assert_eq!(
            start_document["State"],
            json!("DEPLOYING"),
            "case {case:?}: a paced deployment is still rolling out when StartDeployment \
             answers, got {start_document}"
        );
        let number = start_document["DeploymentNumber"]
            .as_i64()
            .unwrap_or_else(|| {
                panic!("case {case:?}: StartDeployment answered no DeploymentNumber")
            }) as i32;

        // From here to the verdict, the ONLY thing read is the queue.
        let events = observed(&router, row, &queue_url).await;
        let ticks: Vec<&Value> = events
            .iter()
            .filter(|event| event["Type"] == json!(TICK_EVENT))
            .collect();
        let tick = ticks.first().unwrap_or_else(|| {
            panic!("case {case:?}: no {TICK_EVENT} event among {events:?}")
        });
        assert_eq!(
            tick["DeploymentNumber"],
            json!(number),
            "case {case:?}: a tick names the deployment it paces, got {tick}"
        );
        assert_eq!(
            tick["Environment"]["Id"],
            json!(environment),
            "case {case:?}: a tick names the environment under deployment, got {tick}"
        );
        // The share of targets the tick is asking about: a share of the rollout, never more
        // than every target.
        let position = tick["PercentageComplete"].as_f64().unwrap_or_else(|| {
            panic!(
                "case {case:?}: a tick reports the rollout position it is asking about, got \
                 {tick}"
            )
        });
        assert!(
            (0.0..=100.0).contains(&position),
            "case {case:?}: expected a rollout position within the rollout, got {position}"
        );

        // The deployment is read ONCE, after the fact: it reports what the ticks had already
        // made of it.
        let read = get_deployment(&router, &application, &environment, number).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment answered {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let document = read.json();
        assert_eq!(
            document["State"],
            json!(row.expected_state),
            "case {case:?}: expected the deployment to have ended {}, got {document}",
            row.expected_state
        );

        if row.expected_state == "ROLLED_BACK" {
            let rollback = newest_event(&document, "ROLLBACK_STARTED").unwrap_or_else(|| {
                panic!("case {case:?}: the rollback's own entry must be on the event log: {document}")
            });
            assert_eq!(
                rollback["TriggeredBy"],
                json!("EXTENSION"),
                "case {case:?}: the rollback was directed by the extension, so the entry \
                 attributes it to the extension, got {rollback}"
            );
            let description = rollback["Description"].as_str().unwrap_or_default();
            match row.expected_description {
                Some(expected) => assert_eq!(
                    description, expected,
                    "case {case:?}: the rollback entry carries the reason the handler \
                     answered, got {description:?}"
                ),
                // The handler named no reason, so AppConfig's own description stands —
                // whatever it says, the entry is not left without one.
                None => assert!(
                    !description.is_empty(),
                    "case {case:?}: the rollback entry must describe the rollback, got \
                     {description:?}"
                ),
            }
            assert!(
                newest_event(&document, "ROLLBACK_COMPLETED").is_some(),
                "case {case:?}: an aborted deployment logs the rollback it finished, got \
                 {document}"
            );
        } else {
            assert!(
                newest_event(&document, "DEPLOYMENT_COMPLETED").is_some(),
                "case {case:?}: a rollout nothing aborted completes, got {document}"
            );
            assert!(
                newest_event(&document, "ROLLBACK_STARTED").is_none(),
                "case {case:?}: only an abort directive rolls a rollout back, got {document}"
            );
        }
    }
}

/// The action points of ONE stage fire once, however many deployments and reads happen
/// around them: a client that watches the queue and a client that reads the deployment see
/// the same single invocation of each.
#[tokio::test]
async fn a_stage_the_clock_dispatched_is_not_dispatched_again_by_a_read() {
    let (router, engine) = emulator();
    let application = create_application(&router, "once-app").await;
    let environment = create_environment(&router, &application, "once-env").await;
    let profile = create_profile(&router, &application, "once-profile").await;
    let written = create_version(
        &router,
        &application,
        &profile,
        "application/json",
        br#"{"release":"candidate"}"#,
    )
    .await;
    assert_eq!(
        written.status, 201,
        "seeding a configuration version answered {}",
        written.status
    );
    // A handler that steers nothing, so the rollout runs to its end and every lifecycle
    // stage of it happens.
    let function = given_function(
        &router,
        &engine,
        "once",
        "once-steer",
        &steering_source(r#"{"Directive": "CONTINUE"}"#),
    )
    .await;
    let (steering, _) = given_extension(
        &router,
        "once-steer-ext",
        &["AT_DEPLOYMENT_TICK"],
        &function,
        &[],
    )
    .await;
    let (queue_url, queue_arn) = given_queue(&router, "once-queue").await;
    let (watching, _) = given_extension(
        &router,
        "once-watch-ext",
        &["ON_DEPLOYMENT_START", "ON_DEPLOYMENT_COMPLETE"],
        &queue_arn,
        &[],
    )
    .await;
    let environment_arn = arn(&format!("application/{application}/environment/{environment}"));
    for extension in [&steering, &watching] {
        associate(&router, "once", extension, &environment_arn, &Value::Null).await;
    }
    let strategy =
        given_growing_strategy_of_type(&router, "once-strategy", "LINEAR", 1, 0, 100.0).await;
    let started = start_deployment(
        &router,
        &application,
        &environment,
        &json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": "1",
        }),
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "StartDeployment answered {} / {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    let number = started.json()["DeploymentNumber"]
        .as_i64()
        .expect("StartDeployment answers a DeploymentNumber") as i32;

    // Wait, on the queue alone, for the stage that ends the deployment.
    let ending = "OnDeploymentComplete";
    let mut collected: Vec<Value> = Vec::new();
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        collected.extend(drain(&router, &queue_url).await);
        let types: Vec<&str> = collected
            .iter()
            .map(|event| event["Type"].as_str().unwrap_or_default())
            .collect();
        if types.contains(&ending) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case \"a stage dispatched once\": expected a {ending} event within {}s, got \
             {types:?}",
            OBSERVED_BY.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Reading the deployment afterwards owes nothing: each stage happened once, so the
    // reader sees no second invocation of a stage the clock had already dispatched.
    let read = get_deployment(&router, &application, &environment, number).await;
    assert_eq!(read.status, 200, "GetDeployment answered {}", read.status);
    let again = drain(&router, &queue_url).await;
    assert!(
        again.is_empty(),
        "case \"a stage dispatched once\": a stage must not be dispatched twice, got {again:?}"
    );

    // And each stage was delivered exactly once in the first place.
    let counts = collected.iter().fold(BTreeMap::new(), |mut counts, event| {
        let event_type = event["Type"].as_str().unwrap_or_default().to_string();
        *counts.entry(event_type).or_insert(0_usize) += 1;
        counts
    });
    for stage in ["OnDeploymentStart", ending] {
        assert_eq!(
            counts.get(stage),
            Some(&1),
            "case \"a stage dispatched once\": expected the stage {stage} to be dispatched \
             once, got {counts:?}"
        );
    }
}
