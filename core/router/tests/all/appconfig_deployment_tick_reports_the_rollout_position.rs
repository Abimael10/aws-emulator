// SPDX-License-Identifier: CC0-1.0

//! An `AT_DEPLOYMENT_TICK` extension action is asked to rule on a rollout, so the event it is
//! handed REPORTS THE ROLLOUT POSITION it is asking about.
//!
//! RULE: while a deployment is in flight, every action an associated extension binds at a point
//! of that deployment's own lifecycle is handed an event carrying the share of targets the
//! deployment is available to at the instant the point is dispatched, under the member name the
//! API itself reports that quantity by — `PercentageComplete`, "the percentage of targets for
//! which the deployment is available"
//! (`smithy_query(query="shape",shape="Deployment")`, member target `Percentage`) — so the
//! position a tick reports is the position the deployment itself reports at that moment, and the
//! positions the consecutive ticks of one rollout report never go backwards.
//!
//! Only a cross-service suite can state that end to end: an action's `Uri` names a resource of
//! ANOTHER service, and an Amazon SQS queue is the target kind that takes the event and answers
//! nothing, so the event is read where a client reads it — out of the queue, through SQS's own
//! API — rather than out of any AppConfig answer. A handler written against AWS reads
//! `event["PercentageComplete"]` to decide whether the blast radius is still small enough to
//! abort, so an event without it makes every percentage-gated canary rule unimplementable.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the rollout the ticks are
//! asked about — its `GrowthType` and its `GrowthFactor`, hence how many growth steps it passes
//! through and which shares those are, from a rollout that reaches every target in one step (and
//! so is never partial) to one that creeps up a five-step exponential canary grid.
//!
//! What the neighbours pin and this one does not repeat: the AppConfig crate's
//! `deployment_tick_directive` pins what a tick's DIRECTIVE makes of the rollout,
//! `extension_action_points` the members each action point's event carries, and
//! `rollout_advances_over_the_wire` the grid a rollout reports to its own readers. None of them
//! states, across growth shapes, that the position a tick is handed is the position the
//! deployment stands at.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region the queue ARN and the
/// AppConfig hierarchy belong to.
const REGION: &str = "us-east-1";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The media type a JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The action point under test, as the `Actions` map is keyed by it.
const AT_DEPLOYMENT_TICK: &str = "AT_DEPLOYMENT_TICK";

/// The `Type` a tick event names its action point by: the action point in PascalCase rather than
/// the SCREAMING_SNAKE enum value.
const TICK_TYPE: &str = "AtDeploymentTick";

/// The member a deployment's rollout position is reported by, on the API and in the event.
const POSITION: &str = "PercentageComplete";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape",shape="DeploymentState")`).
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// The `DeploymentState` values of a deployment the service is still working through, which is
/// when the position a tick is asking about is a position the deployment itself reports.
const IN_FLIGHT_STATES: &[&str] = &["DEPLOYING", "BAKING"];

/// The share of targets a finished rollout has reached, and the ceiling of the `Percentage`
/// shape the deployment's `GrowthFactor` and `PercentageComplete` both target.
const EVERY_TARGET: f64 = 100.0;

/// The share of targets a rollout has reached before its first growth step: none of them.
const NO_TARGET: f64 = 0.0;

/// How far two percentages may differ and still be the same point of a rollout's grid: both are
/// `Percentage`, a float, so a share is compared with a tolerance rather than by equality.
const SAME_POINT: f64 = 0.01;

/// How often work the clock owes is looked for again — a rollout growing, a tick reaching its
/// target — and how long the looking goes on.
///
/// Those spans belong to the service that owns them, so nothing here sleeps a fixed span on
/// their behalf: every wait is on the CONDITION. The deadline is a ceiling on the WAIT and
/// nothing else, generous enough that a loaded host cannot decide any outcome.
const LOOK: Duration = Duration::from_millis(25);
const SETTLED_BY: Duration = Duration::from_secs(120);

/// One rollout the ticks are asked about: the strategy's growth shape and the cumulative shares
/// of its targets that shape prescribes.
///
/// The grid is the model's own formula rather than this emulator's: `LINEAR` "deploys the
/// configuration to the targets at a constant rate", so step `n` has added the growth factor
/// `n + 1` times, and `EXPONENTIAL` "grows the deployment following the equation `G*(2^N)`"
/// (`smithy_query(query="shape",shape="GrowthType")`), each share capped at every target.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The strategy's `GrowthType`.
    growth_type: &'static str,
    /// The strategy's `GrowthFactor`.
    growth_factor: f64,
    /// The cumulative shares of its targets this rollout passes through, in order.
    grid: &'static [f64],
}

/// The rows: both growth types, and within each a factor that takes a different number of steps
/// to reach every target — including the one-step rollout that is never partial, and an
/// exponential canary whose first step is a tenth of the fleet.
const ROWS: &[Row] = &[
    Row {
        case: "linear, 25% per interval",
        growth_type: "LINEAR",
        growth_factor: 25.0,
        grid: &[25.0, 50.0, 75.0, 100.0],
    },
    Row {
        case: "linear, every target in one interval",
        growth_type: "LINEAR",
        growth_factor: 100.0,
        grid: &[100.0],
    },
    Row {
        case: "exponential, a 10% canary",
        growth_type: "EXPONENTIAL",
        growth_factor: 10.0,
        grid: &[10.0, 20.0, 40.0, 80.0, 100.0],
    },
    Row {
        case: "exponential, a 25% canary",
        growth_type: "EXPONENTIAL",
        growth_factor: 25.0,
        grid: &[25.0, 50.0, 100.0],
    },
];

/// The emulator as it is ASSEMBLED, on one engine: an extension action's invocation is dispatched
/// back into this same router, so the queue its `Uri` names is a record of this emulator's own.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router
}

/// What one dispatched request answered.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The body as text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One string member of a JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// Dispatch one call to the service claiming `service`, with `body` as its request body.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    target: Option<&str>,
    body: &[u8],
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", authorization(service))
        .header("content-type", content_type);
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// One AppConfig control-plane call carrying a JSON body.
async fn appconfig(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        "appconfig",
        method,
        uri,
        JSON_CONTENT_TYPE,
        None,
        &payload,
    )
    .await
}

/// One Amazon SQS `awsJson1_0` RPC.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let answered = dispatch(
        router,
        "sqs",
        "POST",
        "/",
        "application/x-amz-json-1.0",
        Some(&target),
        body.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "SQS {operation} must succeed, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// A queue for the tick action's `Uri` to name; answers `(url, arn)`.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue({name:?}) answered no QueueUrl: {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the queue {name:?} reports no QueueArn: {attributes}"))
        .to_string();
    (url, arn)
}

/// Every event waiting on the queue, parsed, oldest first, each DELETED by its receipt handle as
/// it is read — so a later read answers what arrived since and nothing is counted twice.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut events = Vec::with_capacity(messages.len());
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        let body = message["Body"].as_str().unwrap_or_default();
        events.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("an action-point event must be a JSON document ({error}): {body:?}")
        }));
    }
    events
}

/// One minted id out of a 201 answer.
fn minted_id(case: &str, what: &str, answered: &Answered) -> String {
    assert_eq!(
        answered.status,
        201,
        "case {case:?}: {what} must be created, got {} with {}",
        answered.status,
        answered.text()
    );
    answered
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no Id: {}", answered.text()))
}

/// An extension binding ONE action at `AT_DEPLOYMENT_TICK`, pointing at `uri`; answers its id.
async fn given_tick_extension(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    uri: &str,
) -> String {
    let actions: BTreeMap<String, Value> = BTreeMap::from([(
        AT_DEPLOYMENT_TICK.to_string(),
        json!([{"Name": format!("{name}-action"), "Uri": uri}]),
    )]);
    let created = appconfig(
        router,
        "POST",
        "/extensions",
        &json!({"Name": name, "Actions": actions}),
    )
    .await;
    minted_id(case, "the tick extension", &created)
}

/// A deployment strategy growing the way `row` prescribes, with no final bake time: what the
/// ticks are asked about is the ROLLOUT's position, so nothing here waits on a bake window.
async fn given_strategy(router: &MultiServiceRouter, case: &str, name: &str, row: &Row) -> String {
    let created = appconfig(
        router,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": name,
            // A rollout window at all: a strategy that paces no stage leaves no deployment in
            // flight, and an action point of a deployment's lifecycle fires for one that is.
            "DeploymentDurationInMinutes": 1,
            "FinalBakeTimeInMinutes": 0,
            "GrowthFactor": row.growth_factor,
            "GrowthType": row.growth_type,
            "ReplicateTo": "NONE",
        }),
    )
    .await;
    minted_id(case, "the deployment strategy", &created)
}

/// The rollout position a document reports, as the `PercentageComplete` member carries it.
fn position(case: &str, what: &str, document: &Value) -> f64 {
    document[POSITION].as_f64().unwrap_or_else(|| {
        panic!(
            "case {case:?}: {what} must report the rollout position it is asking about under \
             {POSITION}, got {document}"
        )
    })
}

/// Whether `share` is one of the cumulative shares `grid` prescribes, or the share a rollout
/// stands at before its first growth step.
fn on_the_grid(share: f64, grid: &[f64]) -> bool {
    (share - NO_TARGET).abs() < SAME_POINT
        || grid.iter().any(|step| (share - step).abs() < SAME_POINT)
}

/// One tick as it was observed: the position it reported, and the positions the reads that
/// BRACKET it reported for the deployment itself.
struct Observed {
    reported: f64,
    bracket: [f64; 2],
    in_flight: bool,
}

/// Drive one deployment to a state it can no longer leave, collecting every tick event its ticks
/// delivered to the observation queue along the way.
///
/// The wait is on the CONDITION: the loop looks again until the state is terminal or
/// [`SETTLED_BY`] has passed, so nothing here depends on how fast the host runs. Reading the
/// deployment is also what runs the ticks the clock has made due — the way a client watching its
/// own rollout does — so the queue is drained right after each read, which is what lets a tick's
/// reported position be checked against the position the deployment stood at around it.
async fn observe_ticks(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
    queue_url: &str,
) -> (Value, Vec<Observed>) {
    let deadline = Instant::now() + SETTLED_BY;
    let mut observed: Vec<Observed> = Vec::new();
    let mut previous = NO_TARGET;
    let mut previously_in_flight = true;
    loop {
        let read = appconfig(
            router,
            "GET",
            &format!("/applications/{application}/environments/{environment}/deployments/{number}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment({number}) must succeed, got {} with {}",
            read.status,
            read.text()
        );
        let document = read.json();
        let state = document["State"].as_str().unwrap_or_default().to_string();
        let reported = position(case, "a Deployment document", &document);
        let in_flight = IN_FLIGHT_STATES.contains(&state.as_str());
        for event in drain(router, queue_url).await {
            assert_eq!(
                event["Type"],
                json!(TICK_TYPE),
                "case {case:?}: only the tick action is bound, so only its events reach the \
                 queue, got {event}"
            );
            assert_eq!(
                event["DeploymentNumber"],
                json!(number),
                "case {case:?}: a tick names the deployment it paces, got {event}"
            );
            assert_eq!(
                event["Environment"]["Id"],
                json!(environment),
                "case {case:?}: a tick names the environment under deployment, got {event}"
            );
            observed.push(Observed {
                reported: position(case, "a tick event", &event),
                bracket: [previous, reported],
                in_flight: previously_in_flight && in_flight,
            });
        }
        if TERMINAL_STATES.contains(&state.as_str()) {
            return (document, observed);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deployment never reached a terminal state, it is still {state:?} \
             at {reported}% of its targets"
        );
        previous = reported;
        previously_in_flight = in_flight;
        tokio::time::sleep(LOOK).await;
    }
}

/// RULE over the wire, across growth shapes: every `AT_DEPLOYMENT_TICK` event reports the share
/// of targets its deployment is available to at the tick — a share on the grid the rollout's
/// growth shape prescribes, one the deployment itself reports while it is in flight, and never
/// one below what an earlier tick of the same rollout already reported.
#[tokio::test]
async fn an_at_deployment_tick_event_reports_the_rollout_position_it_is_asking_about() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = index + 1;
        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                "POST",
                "/applications",
                &json!({"Name": format!("tick-position-app-{suffix}")}),
            )
            .await,
        );
        let environment = minted_id(
            case,
            "the environment",
            &appconfig(
                &router,
                "POST",
                &format!("/applications/{application}/environments"),
                &json!({"Name": format!("tick-position-env-{suffix}")}),
            )
            .await,
        );
        let profile = minted_id(
            case,
            "the configuration profile",
            &appconfig(
                &router,
                "POST",
                &format!("/applications/{application}/configurationprofiles"),
                &json!({
                    "Name": format!("tick-position-profile-{suffix}"),
                    "LocationUri": HOSTED,
                }),
            )
            .await,
        );
        let written = dispatch(
            &router,
            "appconfig",
            "POST",
            &format!(
                "/applications/{application}/configurationprofiles/{profile}\
                 /hostedconfigurationversions"
            ),
            JSON_CONTENT_TYPE,
            None,
            format!(r#"{{"release":"canary","row":{suffix}}}"#).as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: writing the configuration must succeed, got {} with {}",
            written.status,
            written.text()
        );

        // The observation target: a queue takes the event and answers nothing, so it reports
        // what the tick was handed without steering the rollout it is reporting on.
        let (queue_url, queue_arn) =
            given_queue(&router, &format!("tick-position-observations-{suffix}")).await;
        let extension = given_tick_extension(
            &router,
            case,
            &format!("tick-position-ext-{suffix}"),
            &queue_arn,
        )
        .await;
        let environment_arn = engine::emulator_arn(
            "appconfig",
            REGION,
            &format!("application/{application}/environment/{environment}"),
        );
        let associated = appconfig(
            &router,
            "POST",
            "/extensionassociations",
            &json!({
                "ExtensionIdentifier": extension,
                "ResourceIdentifier": environment_arn,
            }),
        )
        .await;
        minted_id(case, "the extension association", &associated);

        let strategy = given_strategy(
            &router,
            case,
            &format!("tick-position-strategy-{suffix}"),
            row,
        )
        .await;
        let started = appconfig(
            &router,
            "POST",
            &format!("/applications/{application}/environments/{environment}/deployments"),
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
            "case {case:?}: StartDeployment must be accepted, got {} with {}",
            started.status,
            started.text()
        );
        let number = started.json()["DeploymentNumber"]
            .as_i64()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: StartDeployment answered no DeploymentNumber: {}",
                    started.text()
                )
            });

        let (settled, ticks) = observe_ticks(
            &router,
            case,
            &application,
            &environment,
            number,
            &queue_url,
        )
        .await;
        assert_eq!(
            settled["State"],
            json!("COMPLETE"),
            "case {case:?}: a rollout nothing aborted completes, got {settled}"
        );
        assert!(
            !ticks.is_empty(),
            "case {case:?}: an in-flight deployment is ticked, so its tick action must have been \
             handed at least one event, got none"
        );

        let mut highest = NO_TARGET;
        for tick in &ticks {
            let reported = tick.reported;
            assert!(
                (NO_TARGET..=EVERY_TARGET).contains(&reported),
                "case {case:?}: a reported position is a Percentage, so it lies in \
                 {NO_TARGET}..={EVERY_TARGET}, got {reported}"
            );
            assert!(
                on_the_grid(reported, row.grid),
                "case {case:?}: expected a position this rollout stands at — {NO_TARGET} or one \
                 of {:?} — got {reported}",
                row.grid
            );
            assert!(
                reported >= highest,
                "case {case:?}: the positions consecutive ticks of one rollout report never go \
                 back, got {reported} after {highest}"
            );
            highest = reported;
            // While the rollout stands, the position a tick is asking about is the position the
            // deployment itself reports: the one read just before the tick was handed over, or
            // the one read just before that, which are the reads that bracket it.
            if tick.in_flight {
                let [before, after] = tick.bracket;
                assert!(
                    (reported - before).abs() < SAME_POINT || (reported - after).abs() < SAME_POINT,
                    "case {case:?}: a tick reports the position its deployment stood at, so it \
                     is one of the positions the reads bracketing it reported ({before} then \
                     {after}), got {reported}"
                );
            }
        }
        // A rollout that passes through SEVERAL growth steps is ticked while it is part-way
        // through, so its ticks report shares it grew to: ticks stuck at the share it started
        // from would be reporting a stale position.
        if row.grid.len() > 1 {
            assert!(
                highest > NO_TARGET,
                "case {case:?}: a rollout growing through {:?} is ticked after it has grown, so \
                 its ticks must report a share above {NO_TARGET}, the highest was {highest}",
                row.grid
            );
        }
    }
}
