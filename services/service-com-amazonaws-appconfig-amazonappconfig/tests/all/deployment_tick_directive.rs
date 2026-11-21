// SPDX-License-Identifier: CC0-1.0

//! An `AT_DEPLOYMENT_TICK` extension STEERS the rollout it is invoked for: the directive
//! the action's target answers decides whether the deployment carries on or is aborted.
//!
//! The rule under test, which the table below is one slice of: while a deployment paced by
//! a non-instant strategy is in flight, every action an associated extension binds at
//! `AT_DEPLOYMENT_TICK` is invoked once per tick with the event of that deployment —
//! naming the deployment, the environment under rollout and the rollout position the tick
//! is asking about — and the
//! answer is read as a directive — the one directive that aborts a rollout
//! (`ROLL_BACK`, `state::extension_actions::DIRECTIVE_ROLL_BACK`) drives the deployment to
//! the terminal `ROLLED_BACK` state, logs `ROLLBACK_STARTED` and `ROLLBACK_COMPLETED`
//! instead of `DEPLOYMENT_COMPLETED`, carries the `Description` the target answered on the
//! `ROLLBACK_STARTED` entry and attributes that entry to the EXTENSION that directed it,
//! withdraws the configuration from every target so the data plane serves the previously
//! deployed version's exact bytes again, and leaves a deployment a stop can no longer
//! apply to; every other answer steers nothing, so the rollout completes.
//!
//! Only a Lambda function can ANSWER an action point, so the steering target of every row
//! is a function this emulator runs, and the rows differ along what its handler answers:
//! the abort directive with and without a reason, the directive that lets the rollout
//! stand, no directive at all, and a directive AppConfig defines no meaning for. Each row
//! also binds a SECOND extension whose tick action targets an Amazon SQS queue — the
//! target kind that cannot answer — because that is where the event handed to the action
//! point is observable as a client sees it.
//!
//! The second suite reads the same rule where a running APPLICATION reads it: an in-flight
//! rollout that is directed to roll back converges on the terminal `ROLLED_BACK` state
//! available to no target, and from the instant that rollback starts the configuration
//! AppConfig serves is the version that was in force before it — for every consumer of the
//! environment, whatever share of the targets the rollout had reached — read through the
//! data plane an application polls (AppConfigData's `StartConfigurationSession` and
//! `GetLatestConfiguration`) rather than through the control plane's own read. Its rows
//! differ along how the rollout is paced (its `GrowthType`, its `GrowthFactor`, its rollout
//! window and its final bake time) and along whether the tick's answer aborts it at all,
//! because the version in force afterwards follows from the rollback and not from the
//! pacing.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, json_call, send,
};
use crate::configuration_profile_validators::{activate_functions, lambda};
use crate::extension_action_points::{
    associate, create_version, drain, get_deployment, given_growing_strategy,
    given_growing_strategy_of_type, given_queue, given_strategy, start_deployment, stop_deployment,
};
use crate::extensions::create_extension;
use crate::resource_tags::arn;

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape",shape="DeploymentState")`, whose members carry no
/// `@enumValue` trait, so each goes on the wire as its own name: the rollout ends in one of
/// these, and the suite waits for whichever one this row's directive leads to.
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// The `DeploymentState` values of a deployment the service is still rolling out, which is
/// when the position a tick reports is the position the deployment itself reports.
const IN_FLIGHT_STATES: &[&str] = &["DEPLOYING", "BAKING"];

/// The share of targets this suite's paced rollout grows by per interval.
///
/// A factor that takes SEVERAL intervals to reach every target, so the ticks of one
/// deployment are asked about more than one position: "linear … AppConfig deploys the
/// configuration to the number of targets specified by the growth factor per interval"
/// (`smithy_query(query="shape",shape="GrowthType")`).
const GROWTH_FACTOR: f64 = 25.0;

/// Every share of targets a rollout growing by [`GROWTH_FACTOR`] per interval stands at:
/// the share it starts from, each cumulative step, and every target.
const GROWTH_SHARES: &[f64] = &[0.0, 25.0, 50.0, 75.0, 100.0];

/// How long a row waits for its deployment to reach a terminal state.
///
/// A ceiling on the WAIT, not a statement about how long a rollout takes: a deployment
/// still in flight here is one nothing is advancing, which is the defect this suite pins.
const SETTLED_BY: Duration = Duration::from_secs(120);

/// How often the deployment under observation is read while waiting for that condition.
///
/// Reading it is also what gives the ticks the clock has made due somewhere to run, the
/// way a client watching its own rollout does.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// AppConfig, Lambda and Amazon SQS on ONE engine: the steering target and the observation
/// target of an extension action both live in another service, and the invocation is
/// asserted where a client sees it.
///
/// `pub(crate)` because more than one suite of this target binds an extension action to a
/// FUNCTION it runs and observes the event on a QUEUE, which is these three services.
pub(crate) fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
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

/// A .zip deployment package holding `source` as the handler's module, written in memory
/// the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries it.
///
/// `pub(crate)` because more than one suite's extension target is a FUNCTION it has to
/// upload the code of.
pub(crate) fn package(source: &str) -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the archive entry starts");
        std::io::Write::write_all(&mut writer, source.as_bytes()).expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        buffer.into_inner(),
    )
}

/// The handler of a steering function: whatever the row asked it to answer, for every
/// event it is handed.
///
/// The answer is parsed from JSON text inside the handler, so a row states exactly the
/// document a tick target replies with and nothing of it is assembled here.
fn steering_source(answer: &str) -> String {
    format!(
        "import json\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   return json.loads({answer:?})\n"
    )
}

/// Create the function `name` running `source`, bring the activation Lambda's own clock
/// owes it forward so no row waits for that window, and answer the function's ARN.
///
/// `pub(crate)` because more than one suite's extension target is a function it has to
/// create this way.
pub(crate) async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    source: &str,
) -> String {
    let created = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": name,
            "Role": format!("arn:aws:iam::{}:role/appconfig-tick", engine::emulator_account_id()),
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 20,
            "Code": { "ZipFile": package(source) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction answered {} / {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    activate_functions(engine);
    created
        .string("FunctionArn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateFunction answered no FunctionArn"))
}

/// An extension binding one action at `AT_DEPLOYMENT_TICK` pointing at `uri`. Answers its
/// minted id.
async fn given_tick_extension(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    uri: &str,
) -> String {
    let actions: BTreeMap<String, Value> = BTreeMap::from([(
        "AT_DEPLOYMENT_TICK".to_string(),
        json!([{ "Name": format!("{name}-action"), "Uri": uri }]),
    )]);
    let created = create_extension(router, &json!({ "Name": name, "Actions": actions }), None).await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateExtension answered {} / {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    created
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: CreateExtension answered no Id"))
}

/// One `AT_DEPLOYMENT_TICK` event the observation queue delivered, with the position the
/// last read taken BEFORE it could have been dispatched reported.
struct ObservedTick {
    /// The event as the action's target received it.
    event: Value,
    /// The lower bound of where the rollout stood when the tick was dispatched, and `None`
    /// when that read found a deployment that was no longer rolling out — the position of a
    /// deployment a rollback has withdrawn from its targets bounds nothing.
    at_least: Option<f64>,
}

/// Read the deployment until it reaches a state it can no longer leave, and answer the
/// document it was last reported with together with every `AT_DEPLOYMENT_TICK` event the
/// ticks of this deployment delivered to the observation queue.
///
/// The wait is on the CONDITION: the loop looks again until the state is terminal or
/// [`SETTLED_BY`] has passed, so nothing here depends on how fast the host runs. Reading
/// the deployment is also what runs the ticks the clock has made due, so the events a read
/// produced are drained right after it.
///
/// Each tick's reported position is checked against the rollout the deployment's own reads
/// report, BRACKETED rather than compared with one of them: a tick is dispatched at an
/// instant of the service's own — AppConfig paces them, and the actions bound at the point
/// are invoked one after another — so the position it reports is the position the rollout
/// stood at somewhere between the read before it and the read after it. A rollout only ever
/// grows while it stands, which makes those two reads its bounds; demanding that it equal
/// one single read's position would instead be asking whether a growth interval elapsed
/// between two independently timed observations, which is a statement about how fast the
/// host runs rather than about the rule.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i32,
    queue_url: &str,
) -> (Value, Vec<Value>) {
    let deadline = Instant::now() + SETTLED_BY;
    let mut ticks: Vec<Value> = Vec::new();
    // The ticks drained since the previous read, each waiting for the read that bounds it
    // from above: every one of them was dispatched before this drain, so the next read of
    // the deployment is the first one taken after it.
    let mut observed: Vec<ObservedTick> = Vec::new();
    // The position the previous read reported, which no tick drained after it can be
    // behind. A deployment nothing has rolled out yet stands at none of its targets.
    let mut previously_reported: Option<f64> = Some(0.0);
    loop {
        let read = get_deployment(router, application, environment, number).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment answered {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let body = read.json();
        let state = body["State"].as_str().unwrap_or_default().to_string();
        let reported = body["PercentageComplete"].as_f64().unwrap_or_default();
        let rolling_out = IN_FLIGHT_STATES.contains(&state.as_str());
        for tick in observed.drain(..) {
            let position = tick.event["PercentageComplete"].as_f64().unwrap_or_default();
            // Only while the rollout stands at both ends of the bracket: a tick that asked
            // for the abort is answered by withdrawing the configuration from every
            // target, so the position a read reports afterwards is the one the ROLLBACK
            // left, not one the rollout passed through.
            if let (true, Some(at_least)) = (rolling_out, tick.at_least) {
                assert!(
                    position >= at_least && position <= reported,
                    "case {case:?}: a tick reports the position the deployment stood at \
                     when it was dispatched, got {position} where the reads around it \
                     reported {at_least} and {reported}"
                );
            }
            ticks.push(tick.event);
        }
        for event in drain(router, queue_url).await {
            if event["Type"] != json!("AtDeploymentTick") {
                continue;
            }
            assert!(
                event["PercentageComplete"].as_f64().is_some(),
                "case {case:?}: a tick must report the rollout position it is asking \
                 about, got {event}"
            );
            observed.push(ObservedTick {
                event,
                at_least: previously_reported,
            });
        }
        previously_reported = rolling_out.then_some(reported);
        if TERMINAL_STATES.contains(&state.as_str()) {
            ticks.extend(observed.into_iter().map(|tick| tick.event));
            return (body, ticks);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deployment never reached a terminal state, it is still \
             {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `EventType` of every entry of a deployment document's event log.
fn event_types(document: &Value) -> Vec<String> {
    document["EventLog"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| event["EventType"].as_str().unwrap_or_default().to_string())
        .collect()
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

/// The configuration AppConfig serves one client for a triple, as `(bytes, version)`.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
) -> (Vec<u8>, Option<String>) {
    let read = send(
        router,
        "GET",
        &format!(
            "/applications/{application}/environments/{environment}/configurations/{profile}\
             ?client_id=tick-observer"
        ),
        "application/json",
        Vec::new(),
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetConfiguration answered {} / {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    (read.body.to_vec(), read.header("Configuration-Version"))
}

/// What one row's tick target answers and what that makes of the rollout.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The JSON document the row's handler answers, verbatim.
    answer: &'static str,
    /// The terminal `DeploymentState` the rollout ends in, as the document serves it.
    expected_state: &'static str,
    /// The MEMBER NAME of [`Row::expected_state`], which is the form the refusal of a stop
    /// of the terminal deployment spells it in.
    expected_member_name: &'static str,
    /// The `Description` the answer asked AppConfig to record for the rollback, when it
    /// supplied one.
    expected_description: Option<&'static str>,
}

/// The rows: every shape of answer a tick target can give, and what each makes of the
/// rollout. Only `ROLL_BACK` aborts one.
const ROWS: &[Row] = &[
    Row {
        case: "an abort directive with a reason",
        answer: r#"{"Directive": "ROLL_BACK", "Description": "error rate above the canary limit"}"#,
        expected_state: "ROLLED_BACK",
        expected_member_name: "ROLLED_BACK",
        expected_description: Some("error rate above the canary limit"),
    },
    Row {
        case: "an abort directive with no reason",
        answer: r#"{"Directive": "ROLL_BACK"}"#,
        expected_state: "ROLLED_BACK",
        expected_member_name: "ROLLED_BACK",
        expected_description: None,
    },
    Row {
        case: "a directive to carry on",
        answer: r#"{"Directive": "CONTINUE", "Description": "the canary is healthy"}"#,
        expected_state: "COMPLETE",
        expected_member_name: "COMPLETE",
        expected_description: None,
    },
    Row {
        case: "an answer carrying no directive",
        answer: "{}",
        expected_state: "COMPLETE",
        expected_member_name: "COMPLETE",
        expected_description: None,
    },
    Row {
        case: "a directive AppConfig defines no meaning for",
        answer: r#"{"Directive": "PAUSE", "Description": "not a directive of the API"}"#,
        expected_state: "COMPLETE",
        expected_member_name: "COMPLETE",
        expected_description: None,
    },
];

/// The whole chain a tick's directive drives, asserted per row through the front door.
#[tokio::test]
async fn an_at_deployment_tick_directive_steers_the_deployment_it_is_invoked_for() {
    let (router, engine) = emulator();
    // One instant strategy to put the PREVIOUS version in service, and one paced strategy
    // for the deployment under tick: an action point fires for a deployment that is in
    // FLIGHT, and a strategy that paces no stage leaves none in flight.
    let instant = given_strategy(&router, "tick-instant", 0, 0).await;
    let paced = given_growing_strategy(&router, "tick-paced", 1, 0, GROWTH_FACTOR).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = index + 1;
        let application = create_application(&router, &format!("tick-app-{suffix}")).await;
        let environment =
            create_environment(&router, &application, &format!("tick-env-{suffix}")).await;
        let profile = create_profile(&router, &application, &format!("tick-profile-{suffix}")).await;
        let first = format!(r#"{{"release":"first","row":{suffix}}}"#);
        let second = format!(r#"{{"release":"second","row":{suffix}}}"#);
        for content in [&first, &second] {
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
        }

        // Version 1 in service first, with no extension associated yet: it is the
        // configuration a rolled-back deployment of version 2 has to leave behind.
        let seeding = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": instant,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "1",
            }),
        )
        .await;
        assert_eq!(
            seeding.json()["State"],
            json!("COMPLETE"),
            "case {case:?}: the instant deployment of version 1 must be complete, got {}",
            seeding.json()
        );
        let (seeded, seeded_version) =
            served(&router, case, &application, &environment, &profile).await;
        assert_eq!(
            String::from_utf8_lossy(&seeded),
            first,
            "case {case:?}: version 1 must be the configuration in service before the \
             rollout, got {seeded_version:?}"
        );

        // The steering extension, and beside it an observation extension on a queue: a
        // queue cannot answer, so it reports the event without steering anything.
        let function = given_function(
            &router,
            &engine,
            case,
            &format!("tick-steer-{suffix}"),
            &steering_source(row.answer),
        )
        .await;
        let (queue_url, queue_arn) = given_queue(&router, &format!("tick-observations-{suffix}")).await;
        let steering =
            given_tick_extension(&router, case, &format!("tick-steer-ext-{suffix}"), &function)
                .await;
        let watching = given_tick_extension(
            &router,
            case,
            &format!("tick-watch-ext-{suffix}"),
            &queue_arn,
        )
        .await;
        let environment_arn = arn(&format!("application/{application}/environment/{environment}"));
        for extension in [&steering, &watching] {
            associate(&router, case, extension, &environment_arn, &Value::Null).await;
        }

        let started = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": paced,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "2",
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
        assert_eq!(
            started.json()["State"],
            json!("DEPLOYING"),
            "case {case:?}: a paced deployment is still rolling out when StartDeployment \
             answers, got {}",
            started.json()
        );

        let (document, ticks) =
            settled(&router, case, &application, &environment, 2, &queue_url).await;
        let state = document["State"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            state, row.expected_state,
            "case {case:?}: expected the rollout to end {}, got {state:?} with event log {:?}",
            row.expected_state,
            event_types(&document)
        );

        // The action point FIRED: the observation target was handed the tick events of this
        // deployment, each naming the deployment, the environment under rollout and the
        // rollout position the tick is asking about — which `settled` already bracketed
        // between the positions the reads around each tick reported.
        let tick = ticks.first().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the AT_DEPLOYMENT_TICK action point must be invoked while \
                 the deployment is in flight, got no tick event"
            )
        });
        assert_eq!(
            tick["DeploymentNumber"],
            json!(2),
            "case {case:?}: a tick names the deployment it paces, got {tick}"
        );
        assert_eq!(
            tick["Environment"]["Id"],
            json!(environment),
            "case {case:?}: a tick names the environment under deployment, got {tick}"
        );
        // Every position is one of the shares this strategy's growth factor reaches, and a
        // rollout only ever grows, so the positions consecutive ticks of one deployment
        // report never go backwards.
        let mut reached = 0.0_f64;
        for tick in &ticks {
            let position = tick["PercentageComplete"].as_f64().unwrap_or_default();
            assert!(
                GROWTH_SHARES.contains(&position) && position >= reached,
                "case {case:?}: expected a rollout position among {GROWTH_SHARES:?} that \
                 does not go back from {reached}, got {position}"
            );
            reached = position;
        }
        // A rollout nothing aborted is ticked right through its growth steps, so its ticks
        // report a position that GREW: a tick reporting the share it started at for a
        // deployment that reached every target is reporting a stale position.
        if row.expected_state != "ROLLED_BACK" {
            assert!(
                reached > 0.0,
                "case {case:?}: the ticks of a rollout that reached every target must \
                 report the shares it grew through, the last reported {reached}"
            );
        }

        let types = event_types(&document);
        if row.expected_state == "ROLLED_BACK" {
            for owed in ["ROLLBACK_STARTED", "ROLLBACK_COMPLETED"] {
                assert!(
                    types.iter().any(|event| event == owed),
                    "case {case:?}: an aborted rollout logs {owed}, got {types:?}"
                );
            }
            assert!(
                !types.iter().any(|event| event == "DEPLOYMENT_COMPLETED"),
                "case {case:?}: an aborted rollout never completed, got {types:?}"
            );
            let rollback = newest_event(&document, "ROLLBACK_STARTED").unwrap_or_else(|| {
                panic!("case {case:?}: the rollback's own entry must be on the event log")
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
                    "case {case:?}: the rollback entry carries the description the extension \
                     answered, got {description:?}"
                ),
                // The target named no reason, so AppConfig's own description stands —
                // whatever it says, the entry is not left without one.
                None => assert!(
                    !description.is_empty(),
                    "case {case:?}: the rollback entry must describe the rollback, got \
                     {description:?}"
                ),
            }
        } else {
            assert!(
                types.iter().any(|event| event == "DEPLOYMENT_COMPLETED"),
                "case {case:?}: a rollout nothing aborted completes, got {types:?}"
            );
            assert!(
                !types.iter().any(|event| event == "ROLLBACK_STARTED"),
                "case {case:?}: only a ROLL_BACK directive aborts a rollout, got {types:?}"
            );
        }

        // The data plane follows the deployment: an aborted rollout puts the previous
        // version's exact bytes back in service, and one that completed serves the new
        // ones.
        let (in_service, version) = served(&router, case, &application, &environment, &profile).await;
        let (expected_bytes, expected_version) = if row.expected_state == "ROLLED_BACK" {
            (&first, "1")
        } else {
            (&second, "2")
        };
        assert_eq!(
            String::from_utf8_lossy(&in_service),
            **expected_bytes,
            "case {case:?}: expected the bytes of version {expected_version} in service, the \
             served version was {version:?}"
        );
        assert_eq!(
            version.as_deref(),
            Some(expected_version),
            "case {case:?}: expected version {expected_version} in service, got {version:?}"
        );

        // A deployment the service has finished with is no longer stoppable, whichever
        // terminal state it reached.
        let stopped = stop_deployment(&router, &application, &environment, 2).await;
        assert_eq!(
            stopped.status,
            400,
            "case {case:?}: a terminal deployment cannot be stopped, got {} / {}",
            stopped.status,
            String::from_utf8_lossy(&stopped.body)
        );
        assert_eq!(
            stopped.error_code().split('#').next_back(),
            Some("BadRequestException"),
            "case {case:?}: expected BadRequestException, got {}",
            stopped.error_code()
        );
        let message = stopped.string("Message").unwrap_or_default();
        let expected = format!(
            "Deployment 2 for App {application} and Environment {environment} cannot be \
             stopped, it has a status of {}.",
            row.expected_member_name
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal {expected:?}, got {message:?}"
        );
    }
}

/// The `DeploymentState` values of a deployment whose rollback has begun: the one the
/// service is still working the rollback through and the terminal one it ends in. From the
/// first of them on, the version under rollout is available to no target
/// (`smithy_query(query="shape",shape="Deployment")` for `PercentageComplete`: "the
/// percentage of targets for which the deployment is available"), so nothing is served it.
const WITHDRAWN_STATES: &[&str] = &["ROLLING_BACK", "ROLLED_BACK"];

/// How many independently opened configuration sessions the sample taken after the rollout
/// settled holds.
///
/// Each session's position in a rollout is minted by the service and carried by every poll
/// of it, so a sample of several spans the positions a fleet holds rather than one of them.
const SESSION_SAMPLE: usize = 8;

/// The `FinalBakeTimeInMinutes` an aborted row's strategy declares
/// (`MinutesBetween0And24Hours`, range 0..1440).
///
/// A window for the DIRECTIVE to arrive in, not a limit any row tests: AppConfig monitors a
/// deployment over its final bake time, so a row declaring one keeps its deployment in
/// flight — and therefore ticking — for that long. Generous, because a row whose directive
/// arrived after its deployment had already finished would be reporting on how fast the
/// host runs rather than on the rule. No row ever waits it out: each is aborted at its first
/// tick, which the interval the ticks are paced by makes due at once.
const ABORT_WINDOW_IN_MINUTES: i32 = 5;

/// How long a row of this suite waits for its deployment to reach a terminal state.
///
/// A ceiling on the WAIT, not a statement about how long a rollback takes: the rollback a
/// directive asks for is owed within one tick interval plus the span the service works it
/// through, so a row that spends this long is one whose directive never arrived — the defect
/// this suite pins — and nothing here is decided by the clock short of it.
const REVERTED_BY: Duration = Duration::from_secs(180);

/// AppConfig, Lambda and the AppConfigData data plane on ONE engine: the steering target of
/// the tick lives in Lambda, and what the rollback leaves in service is what an application
/// polling its own configuration is served.
fn emulator_with_data_plane() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_appconfig_amazonappconfig::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_appconfigdata_appconfigdata::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// A deployment strategy of this account's own, paced exactly as a row asks.
///
/// The create itself is the sibling helper every suite of this target shares; this one only
/// orders a row's own members into it. Answers the strategy's minted id.
async fn given_paced_strategy(router: &MultiServiceRouter, name: &str, row: &ServedRow) -> String {
    given_growing_strategy_of_type(
        router,
        name,
        row.growth_type,
        row.duration_in_minutes,
        row.bake_in_minutes,
        row.growth_factor,
    )
    .await
}

/// Open ONE fresh configuration session over the triple through the AppConfigData data
/// plane, poll it once, and assert the exact bytes it was served.
///
/// A fresh session each time because a poll serves the configuration only "when it differs
/// from the version the session was last served": a client that has just started is the one
/// whose poll always carries the content in force.
async fn assert_served_to_a_session(
    router: &MultiServiceRouter,
    case: &str,
    when: &str,
    triple: (&str, &str, &str),
    expected: &[u8],
) {
    let (application, environment, profile) = triple;
    let started = json_call(
        router,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": application,
            "EnvironmentIdentifier": environment,
            "ConfigurationProfileIdentifier": profile,
        }),
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "case {case:?}: {when}: StartConfigurationSession answered {} / {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    let token = started.string("InitialConfigurationToken").unwrap_or_else(|| {
        panic!("case {case:?}: {when}: StartConfigurationSession answered no token")
    });
    let polled = send(
        router,
        "GET",
        &format!("/configuration?configuration_token={token}"),
        "application/json",
        Vec::new(),
        &[],
    )
    .await;
    assert_eq!(
        polled.status,
        200,
        "case {case:?}: {when}: GetLatestConfiguration answered {} / {}",
        polled.status,
        String::from_utf8_lossy(&polled.body)
    );
    assert_eq!(
        polled.body.as_ref(),
        expected,
        "case {case:?}: {when}: expected the bytes of {:?} in service, got {:?}",
        String::from_utf8_lossy(expected),
        String::from_utf8_lossy(&polled.body)
    );
}

/// One row of the data-plane suite: how the rollout under tick is paced, what its tick
/// target answers, and what the environment serves once the deployment is terminal.
struct ServedRow {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// A `GrowthType` the model declares
    /// (`smithy_query(query="shape",shape="GrowthType")`: `EXPONENTIAL | LINEAR`).
    growth_type: &'static str,
    /// `CreateDeploymentStrategy$GrowthFactor` (`Percentage`, range 1..100).
    growth_factor: f64,
    /// `CreateDeploymentStrategy$DeploymentDurationInMinutes`
    /// (`MinutesBetween0And24Hours`, range 0..1440).
    duration_in_minutes: i32,
    /// `CreateDeploymentStrategy$FinalBakeTimeInMinutes`, the same range.
    bake_in_minutes: i32,
    /// The JSON document the row's tick target answers, verbatim.
    answer: &'static str,
    /// The terminal `DeploymentState` the rollout ends in.
    expected_state: &'static str,
}

/// The rows: an aborted rollout paced four different ways, and one nothing aborted. The
/// growth factor spans the model's own range — the smallest share a rollout can grow by,
/// shares that take several intervals, and the one that reaches every target in a single
/// interval — because the version left in service follows from the rollback rather than
/// from how far the rollout had got.
///
/// Every aborted row is paced so that the deployment stays in flight for far longer than
/// the directive needs to arrive — a rollout window of many growth steps, or a final bake
/// window to be monitored over — so no row's outcome depends on how fast the host gets to
/// its first read: a tight window is not what any of them is about.
const SERVED_ROWS: &[ServedRow] = &[
    ServedRow {
        case: "a linear rollout aborted at a tick",
        growth_type: "LINEAR",
        growth_factor: 25.0,
        duration_in_minutes: 1,
        bake_in_minutes: ABORT_WINDOW_IN_MINUTES,
        answer: r#"{"Directive": "ROLL_BACK", "Description": "the canary is failing"}"#,
        expected_state: "ROLLED_BACK",
    },
    ServedRow {
        case: "an exponential rollout aborted at a tick",
        growth_type: "EXPONENTIAL",
        growth_factor: 5.0,
        duration_in_minutes: 2,
        bake_in_minutes: ABORT_WINDOW_IN_MINUTES,
        answer: r#"{"Directive": "ROLL_BACK"}"#,
        expected_state: "ROLLED_BACK",
    },
    ServedRow {
        case: "a rollout with no bake window, growing by the smallest share the model \
               admits, aborted at a tick",
        growth_type: "LINEAR",
        growth_factor: 1.0,
        duration_in_minutes: 1,
        bake_in_minutes: 0,
        answer: r#"{"Directive": "ROLL_BACK", "Description": "aborted one percent in"}"#,
        expected_state: "ROLLED_BACK",
    },
    ServedRow {
        case: "a rollout reaching every target in one interval, aborted at a tick",
        growth_type: "LINEAR",
        growth_factor: 100.0,
        duration_in_minutes: 1,
        bake_in_minutes: ABORT_WINDOW_IN_MINUTES,
        answer: r#"{"Directive": "ROLL_BACK"}"#,
        expected_state: "ROLLED_BACK",
    },
    ServedRow {
        case: "a rollout no directive aborted",
        growth_type: "LINEAR",
        growth_factor: 50.0,
        duration_in_minutes: 1,
        bake_in_minutes: 0,
        answer: r#"{"Directive": "CONTINUE", "Description": "the canary is healthy"}"#,
        expected_state: "COMPLETE",
    },
];

/// RULE: a rollout directed to roll back converges on the terminal `ROLLED_BACK` state
/// available to no target, and from the instant the rollback starts every session of the
/// environment is served the exact bytes of the version that was in force before it; a
/// rollout nothing aborted completes and its own version is what is served.
#[tokio::test]
async fn an_aborted_rollout_serves_the_previous_version_to_every_session() {
    let (router, engine) = emulator_with_data_plane();
    // An instant strategy to put the PREVIOUS version in service: a strategy that paces no
    // stage leaves nothing in flight, so that deployment is complete when it answers.
    let instant = given_strategy(&router, "revert-instant", 0, 0).await;

    for (index, row) in SERVED_ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = index + 1;
        let application = create_application(&router, &format!("revert-app-{suffix}")).await;
        let environment =
            create_environment(&router, &application, &format!("revert-env-{suffix}")).await;
        let profile =
            create_profile(&router, &application, &format!("revert-profile-{suffix}")).await;
        let previous = format!(r#"{{"release":"previous","row":{suffix}}}"#);
        let next = format!(r#"{{"release":"next","row":{suffix}}}"#);
        for content in [&previous, &next] {
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
        }
        let triple = (application.as_str(), environment.as_str(), profile.as_str());

        // Version 1 in force, with no extension associated yet: it is the configuration a
        // rolled-back deployment of version 2 has to leave behind.
        let seeding = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": instant,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "1",
            }),
        )
        .await;
        assert_eq!(
            seeding.json()["State"],
            json!("COMPLETE"),
            "case {case:?}: the instant deployment of version 1 must be complete, got {}",
            seeding.json()
        );
        assert_served_to_a_session(
            &router,
            case,
            "before the rollout",
            triple,
            previous.as_bytes(),
        )
        .await;

        // The steering extension, bound to the environment under rollout.
        let function = given_function(
            &router,
            &engine,
            case,
            &format!("revert-steer-{suffix}"),
            &steering_source(row.answer),
        )
        .await;
        let extension = given_tick_extension(
            &router,
            case,
            &format!("revert-steer-ext-{suffix}"),
            &function,
        )
        .await;
        let environment_arn = arn(&format!("application/{application}/environment/{environment}"));
        associate(&router, case, &extension, &environment_arn, &Value::Null).await;

        let strategy = given_paced_strategy(&router, &format!("revert-paced-{suffix}"), row).await;
        let started = start_deployment(
            &router,
            &application,
            &environment,
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "2",
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
        assert_eq!(
            started.json()["State"],
            json!("DEPLOYING"),
            "case {case:?}: a paced deployment is still rolling out when StartDeployment \
             answers, got {}",
            started.json()
        );

        // Read the deployment until it reaches a state it can no longer leave. The wait is
        // on the CONDITION, with a deadline generous enough that nothing here depends on
        // how fast the host runs; reading the deployment is also what runs the ticks the
        // clock has made due, the way a client watching its own rollout does.
        let deadline = Instant::now() + REVERTED_BY;
        let document = loop {
            let read = get_deployment(&router, &application, &environment, 2).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: GetDeployment answered {} / {}",
                read.status,
                String::from_utf8_lossy(&read.body)
            );
            let body = read.json();
            let state = body["State"].as_str().unwrap_or_default().to_string();
            // The withdrawal reaches the data plane from the instant the rollback STARTS,
            // not only once it has finished: a deployment being rolled back is returning to
            // the configuration that preceded it, so no session is served the version under
            // rollout while it is in either of those states.
            if WITHDRAWN_STATES.contains(&state.as_str()) {
                assert_served_to_a_session(
                    &router,
                    case,
                    &format!("a session opened while the deployment reported {state:?}"),
                    triple,
                    previous.as_bytes(),
                )
                .await;
            }
            if TERMINAL_STATES.contains(&state.as_str()) {
                break body;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the deployment never reached a terminal state, it is still \
                 {state:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };

        let state = document["State"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            state, row.expected_state,
            "case {case:?}: expected the rollout to end {}, got {state:?} with event log {:?}",
            row.expected_state,
            event_types(&document)
        );

        let types = event_types(&document);
        let in_service: &str = if row.expected_state == "ROLLED_BACK" {
            // An aborted rollout is available to NO target, its rollback is on its event
            // log at both ends, and it never completed.
            let percentage = document["PercentageComplete"].as_f64();
            assert_eq!(
                percentage,
                Some(0.0),
                "case {case:?}: an aborted rollout is available to no target, got \
                 {percentage:?}"
            );
            for owed in ["ROLLBACK_STARTED", "ROLLBACK_COMPLETED"] {
                assert!(
                    types.iter().any(|event| event == owed),
                    "case {case:?}: an aborted rollout logs {owed}, got {types:?}"
                );
            }
            assert!(
                !types.iter().any(|event| event == "DEPLOYMENT_COMPLETED"),
                "case {case:?}: an aborted rollout never completed, got {types:?}"
            );
            &previous
        } else {
            assert!(
                types.iter().any(|event| event == "DEPLOYMENT_COMPLETED"),
                "case {case:?}: a rollout nothing aborted completes, got {types:?}"
            );
            assert!(
                !types.iter().any(|event| event == "ROLLBACK_STARTED"),
                "case {case:?}: only a ROLL_BACK directive aborts a rollout, got {types:?}"
            );
            &next
        };

        // Nothing is left for a reader to finish: a second read of a settled deployment
        // reports the same state and the same share of targets, so the rollout it had
        // reached is not advancing any more.
        let again = get_deployment(&router, &application, &environment, 2).await.json();
        assert_eq!(
            (
                again["State"].as_str().unwrap_or_default(),
                again["PercentageComplete"].as_f64()
            ),
            (
                state.as_str(),
                document["PercentageComplete"].as_f64()
            ),
            "case {case:?}: a settled deployment keeps the state and the share it settled \
             at, got {again}"
        );

        // And the data plane follows it, for every session of the environment rather than
        // for the one position a single session happens to hold.
        for sample in 0..SESSION_SAMPLE {
            assert_served_to_a_session(
                &router,
                case,
                &format!("session {sample} of the settled environment"),
                triple,
                in_service.as_bytes(),
            )
            .await;
        }
    }
}
