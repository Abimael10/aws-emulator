// SPDX-License-Identifier: CC0-1.0

//! The barrier a release waits at is on the CONTROL plane and what it is waiting FOR is on the
//! DATA plane, so the two AppConfig APIs are asserted against each other here.
//!
//! RULE: `Deployment$State` is served as the member `DeploymentState` declares — the members
//! carry no `@enumValue` override (`smithy_query(query="shape",shape="DeploymentState")`), so
//! each member's wire value is its own name, `COMPLETE` and `ROLLED_BACK` included — and that
//! same token is what decides the data plane: the configuration a session is served is the one a
//! deployment the control plane reports `COMPLETE` put in service, while a release a terminal
//! member says was withdrawn put nothing in service, so no session is served its bytes.
//!
//! Only a cross-service suite can state that. AppConfig is two APIs: a client starts a
//! deployment on `appconfig`, polls `GetDeployment` until `State` is one of the members it knows
//! a deployment cannot leave, and only then opens a session on `appconfigdata` and reads the
//! configuration. The terminal set below is therefore written out as a WAITING CLIENT writes it,
//! not read from the crate under test: a document carrying any other spelling of the member
//! never satisfies that wait, and then the hand-over never happens — the client reports a failed
//! release for one that settled, and everything gated behind a settled release is unreachable.
//!
//! The rows differ along what the rule generalises over: WHAT paces the release before it settles
//! (a strategy that paces nothing, so the call that starts it answers a finished deployment; a
//! rollout the clock walks through in several growth steps, read at an in-flight member on the
//! way), HOW it ends (the clock completes it, or a caller's stop withdraws one still in flight)
//! and therefore WHICH terminal member releases the wait — and, with it, whether the data plane
//! is handed that release's configuration or must not serve it.
//!
//! What the neighbours pin and this one does not repeat: the AppConfig crate's
//! `deployment_state_wire_form` pins the member-to-wire-value catalog and the refusal that spells
//! a state into a sentence, `deployment_and_environment_state_tokens` the two state enums of one
//! release against each other, and `environment_state_wire_form` the sibling enum's own form.
//! None of them crosses to the data plane, which is the only thing asserted here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::ops::types::DEPLOYMENT_STATE_VALUES;

use crate::query_wire::form_encode;
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is addressed at, which is the Region the sibling's
/// [`authorization`] credential scope names too.
const REGION: &str = "us-east-1";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The media type a JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The `DeploymentState` members a deployment can no longer leave, as a WAITING CLIENT writes
/// them: the three terminal members of the enum, spelled here rather than read from the crate
/// under test, so a crate that re-spelled its own constants would not re-spell this wait.
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// The member a deployment that ran to completion reports, and the one a client gates its read of
/// the served configuration on.
const COMPLETE: &str = "COMPLETE";

/// How often work the clock owes is looked for again, and how long the looking goes on.
///
/// The spans belong to the services that own them, so nothing here sleeps a fixed span on their
/// behalf: every wait is on the CONDITION. The deadline is a ceiling on the WAIT and nothing
/// else, generous enough that a loaded host cannot decide any outcome.
const LOOK: Duration = Duration::from_millis(25);
const SETTLED_BY: Duration = Duration::from_secs(120);

/// The emulator as it is ASSEMBLED, on one engine: the control plane a deployment is started on
/// and the data plane its configuration is read from are two services of the same router, over
/// the one store.
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

    /// One top-level string member of a JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    }
}

/// Dispatch one call to the service claiming `service`, with `headers` beside the ones every
/// request carries.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    // Both AppConfig APIs sign as `appconfig`, whichever endpoint a client addresses.
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", authorization("appconfig"))
        .header("content-type", JSON_CONTENT_TYPE);
    for (name, value) in headers {
        request = request.header(*name, value.clone());
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

/// One AppConfig CONTROL-plane call carrying a JSON body.
async fn appconfig(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(router, "appconfig", method, uri, &[], &payload).await
}

/// One AppConfig DATA-plane call, as a polling client makes one.
async fn appconfigdata(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(router, "appconfigdata", method, uri, &[], &payload).await
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

/// One row's AppConfig hierarchy as the control plane minted it: what the deployment is started
/// against, and what a data-plane session polls.
struct Hierarchy {
    application: String,
    environment: String,
    profile: String,
}

/// How a row's release ends, which decides which terminal member releases the client's wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Nothing is asked of it: the clock finishes the release.
    Settles,
    /// `StopDeployment` with no `Allow-Revert`, which withdraws a release still in flight.
    Stopped,
}

/// One row: the strategy pacing the release, the member it is read at on the way, how it ends and
/// which member then releases the wait.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The strategy's `DeploymentDurationInMinutes`; `MinutesBetween0And24Hours` is
    /// `range 0..1440`.
    duration: i64,
    /// The strategy's `GrowthFactor`; `Percentage` is `range 1..100`.
    growth_factor: f64,
    /// The strategy's `GrowthType`.
    growth_type: &'static str,
    /// A member the deployment is read at before it settles, or `None` for a release that is over
    /// by the time the call starting it answers.
    in_flight: Option<&'static str>,
    ending: Ending,
    /// The member that releases the wait, written out literally.
    terminal: &'static str,
}

/// The rows: a release the clock finishes at once, one the clock walks through several growth
/// steps first, and one a caller withdraws while it is still rolling out.
const ROWS: &[Row] = &[
    Row {
        // A strategy that paces no stage leaves AppConfig nothing to roll out and nothing to
        // bake, so the deployment is over by the time the call that started it answers.
        case: "a strategy that paces nothing",
        duration: 0,
        growth_factor: 100.0,
        growth_type: "LINEAR",
        in_flight: None,
        ending: Ending::Settles,
        terminal: "COMPLETE",
    },
    Row {
        // A quarter of the fleet per interval, so the release passes through four growth steps
        // and is read while it is still rolling out.
        case: "a rollout the clock walks through in quarters",
        duration: 1,
        growth_factor: 25.0,
        growth_type: "LINEAR",
        in_flight: Some("DEPLOYING"),
        ending: Ending::Settles,
        terminal: "COMPLETE",
    },
    Row {
        // One percent of the fleet per interval over the longest window the model allows, so the
        // rollout is still far from over when the caller withdraws it.
        case: "the longest rollout the model allows, withdrawn by a stop",
        duration: 1440,
        growth_factor: 1.0,
        growth_type: "LINEAR",
        in_flight: Some("DEPLOYING"),
        ending: Ending::Stopped,
        terminal: "ROLLED_BACK",
    },
];

/// The `State` a served document reports.
fn served_state(case: &str, what: &str, document: &Value) -> String {
    document["State"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {what} must report a State, got {document}"))
        .to_string()
}

/// The rule, applied to one served state: it is a member `DeploymentState` declares, which for an
/// enum with no `@enumValue` override is the value that member goes on the wire as — so a client
/// comparing the served token against the enum's members is comparing like with like.
fn assert_modelled_member(case: &str, what: &str, state: &str) {
    assert!(
        DEPLOYMENT_STATE_VALUES.contains(&state),
        "case {case:?}: {what} must report State as a member DeploymentState declares, one of \
         {DEPLOYMENT_STATE_VALUES:?}, got {state:?}"
    );
}

/// One `GetDeployment`, with the rule asserted on what it served.
async fn read_deployment(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    number: i64,
) -> Value {
    let read = appconfig(
        router,
        "GET",
        &format!(
            "/applications/{}/environments/{}/deployments/{number}",
            hierarchy.application, hierarchy.environment
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetDeployment({number}) must answer 200, got {} with {}",
        read.status,
        read.text()
    );
    let document = read.json();
    assert_modelled_member(
        case,
        "GetDeployment",
        &served_state(case, "GetDeployment", &document),
    );
    document
}

/// The client's wait: poll `GetDeployment` until the deployment is read at one of `wanted`, and
/// answer the member it was read at. The wait is on the CONDITION, so nothing here depends on how
/// fast the host runs.
async fn wait_until_read_at(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    number: i64,
    wanted: &[&str],
) -> String {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let document = read_deployment(router, case, hierarchy, number).await;
        let state = served_state(case, "GetDeployment", &document);
        if wanted.contains(&state.as_str()) {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected deployment {number} to be read at one of {wanted:?}, it is \
             still {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// `StartConfigurationSession` for the row's hierarchy, answered as it came back: a client that
/// has nothing in service is refused here rather than served empty bytes, so the answer is
/// returned unjudged.
async fn start_session(router: &MultiServiceRouter, hierarchy: &Hierarchy) -> Answered {
    appconfigdata(
        router,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": hierarchy.application,
            "EnvironmentIdentifier": hierarchy.environment,
            "ConfigurationProfileIdentifier": hierarchy.profile,
        }),
    )
    .await
}

/// What AppConfig's DATA plane serves one fresh client right now: a session of its own, then the
/// configuration that session is handed, answered as it came back off the wire.
async fn served_configuration(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
) -> Answered {
    let session = start_session(router, hierarchy).await;
    assert_eq!(
        session.status,
        201,
        "case {case:?}: StartConfigurationSession must answer 201, got {} with {}",
        session.status,
        session.text()
    );
    let token = session
        .string("InitialConfigurationToken")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartConfigurationSession answered no token: {}",
                session.text()
            )
        });
    appconfigdata(
        router,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &Value::Null,
    )
    .await
}

/// The hand-over: once the control plane reports the release [`COMPLETE`], the data plane serves
/// the bytes that release put in service. Read through a fresh session until they are there or
/// the deadline passes, so the wait is on the condition.
async fn assert_the_data_plane_serves(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    expected: &str,
) {
    let deadline = Instant::now() + SETTLED_BY;
    let mut read = served_configuration(router, case, hierarchy).await;
    while read.body.as_ref() != expected.as_bytes() && Instant::now() < deadline {
        tokio::time::sleep(LOOK).await;
        read = served_configuration(router, case, hierarchy).await;
    }
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetLatestConfiguration must answer 200, got {} with {}",
        read.status,
        read.text()
    );
    assert_eq!(
        read.text(),
        expected,
        "case {case:?}: the data plane must serve the configuration the settled release put in \
         service, got {}",
        read.text()
    );
}

/// RULE over the wire, across both AppConfig APIs of the assembled emulator: the control plane
/// reports `Deployment$State` as the member the model declares, so a client's poll for a terminal
/// member releases — and what the data plane then serves follows from WHICH member it released
/// on: the release's own configuration for one that completed, and nothing at all for one a stop
/// withdrew.
#[tokio::test]
async fn a_poll_for_a_modelled_deployment_state_releases_and_decides_the_served_configuration() {
    let router = build_router();
    // The literals this suite waits on are the model's own members, so the wait is stated against
    // the API rather than against a spelling of this emulator's.
    for state in TERMINAL_STATES {
        assert_modelled_member("the client's terminal set", "the wait", state);
    }

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
                &json!({"Name": format!("settle-barrier-app-{suffix}")}),
            )
            .await,
        );
        // Its own environment per row: an environment holds one deployment at a time.
        let environment = minted_id(
            case,
            "the environment",
            &appconfig(
                &router,
                "POST",
                &format!("/applications/{application}/environments"),
                &json!({"Name": format!("settle-barrier-env-{suffix}")}),
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
                    "Name": format!("settle-barrier-profile-{suffix}"),
                    "LocationUri": HOSTED,
                }),
            )
            .await,
        );
        let strategy = minted_id(
            case,
            "the deployment strategy",
            &appconfig(
                &router,
                "POST",
                "/deploymentstrategies",
                &json!({
                    "Name": format!("settle-barrier-strategy-{suffix}"),
                    "DeploymentDurationInMinutes": row.duration,
                    // No bake window: what this suite waits for is the rollout and the stop.
                    "FinalBakeTimeInMinutes": 0,
                    "GrowthFactor": row.growth_factor,
                    "GrowthType": row.growth_type,
                    "ReplicateTo": "NONE",
                }),
            )
            .await,
        );
        let hierarchy = Hierarchy {
            application: application.clone(),
            environment: environment.clone(),
            profile: profile.clone(),
        };

        // The bytes this release puts in service, one content per row, so a row cannot pass on
        // what another row's release deployed.
        let content = format!(r#"{{"row":{suffix},"release":"{case}"}}"#);
        let written = dispatch(
            &router,
            "appconfig",
            "POST",
            &format!(
                "/applications/{application}/configurationprofiles/{profile}\
                 /hostedconfigurationversions"
            ),
            &[],
            content.as_bytes(),
        )
        .await;
        assert_eq!(
            written.status,
            201,
            "case {case:?}: writing the configuration must answer 201, got {} with {}",
            written.status,
            written.text()
        );

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
            "case {case:?}: StartDeployment must answer 201, got {} with {}",
            started.status,
            started.text()
        );
        // The call's OWN answer, before anything polls: a client that branches on the deployment
        // it just created reads the state from there, so that projection carries the member too.
        let accepted = started.json();
        assert_modelled_member(
            case,
            "StartDeployment",
            &served_state(case, "StartDeployment", &accepted),
        );
        let number = accepted["DeploymentNumber"].as_i64().unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartDeployment answered no DeploymentNumber: {}",
                started.text()
            )
        });

        if let Some(in_flight) = row.in_flight {
            let reached = wait_until_read_at(&router, case, &hierarchy, number, &[in_flight]).await;
            assert_eq!(
                reached, in_flight,
                "case {case:?}: the release must be read at {in_flight:?} while it is still in \
                 flight, got {reached:?}"
            );
        }

        if row.ending == Ending::Stopped {
            let withdrawn = dispatch(
                &router,
                "appconfig",
                "DELETE",
                &format!(
                    "/applications/{application}/environments/{environment}/deployments/{number}"
                ),
                &[],
                &[],
            )
            .await;
            assert_eq!(
                withdrawn.status,
                202,
                "case {case:?}: StopDeployment must answer 202, got {} with {}",
                withdrawn.status,
                withdrawn.text()
            );
            assert_modelled_member(
                case,
                "StopDeployment",
                &served_state(case, "StopDeployment", &withdrawn.json()),
            );
        }

        // The barrier itself: the wait a client writes, released by a member of the enum.
        let terminal = wait_until_read_at(&router, case, &hierarchy, number, TERMINAL_STATES).await;
        assert_eq!(
            terminal, row.terminal,
            "case {case:?}: the release must settle at {:?}, got {terminal:?}",
            row.terminal
        );

        // And the hand-over the barrier gates, which is what the two APIs are asserted against
        // each other for.
        if terminal == COMPLETE {
            assert_the_data_plane_serves(&router, case, &hierarchy, &content).await;
        } else {
            // Nothing is in service for this profile, so the data plane has no configuration to
            // hand a client: a session cannot be opened at all, and the withdrawn bytes are in
            // no part of the answer.
            let refused = start_session(&router, &hierarchy).await;
            assert_eq!(
                refused.status,
                404,
                "case {case:?}: a release the control plane reports {terminal:?} put nothing in \
                 service, so StartConfigurationSession is refused with the modelled \
                 ResourceNotFoundException's HTTP 404, got {} with {}",
                refused.status,
                refused.text()
            );
            assert!(
                !refused.text().contains(&content),
                "case {case:?}: the configuration of a release reported {terminal:?} is not \
                 served, got {}",
                refused.text()
            );
        }
    }
}
