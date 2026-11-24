// SPDX-License-Identifier: CC0-1.0

//! A rollout that has reached part of its targets is partial in WHAT BOTH OF APPCONFIG'S
//! READERS SERVE, whichever store the configuration itself lives in.
//!
//! RULE: a deployment's `PercentageComplete` is "the percentage of targets for which the
//! deployment is available" (`smithy_query(query="doc", shape="Deployment",
//! member="PercentageComplete")`) and a strategy's `GrowthFactor` is "the percentage of
//! targets that received a deployed configuration during each interval"
//! (`smithy_query(query="shape", shape="DeploymentStrategy")`), so while a rollout is under
//! way the version it deploys is in force for exactly the share of targets it has already
//! passed and the version the preceding deployment put in service is still in force for the
//! rest — each target served one version's exact bytes, never a blend — and the fleet
//! converges on the new version only once the rollout has reached every target. A target's
//! place in the rollout is its own and fixed by the key that identifies it: the caller's
//! `ClientId` for a `GetConfiguration` read, which "enables AppConfig to deploy the
//! configuration in intervals, as defined in the deployment strategy"
//! (`smithy_query(query="doc", shape="GetConfigurationRequest", member="ClientId")`), and the
//! configuration session for an `appconfigdata` poll. WHICH STORE the two versions' bytes come
//! out of is a separate question — the profile's `LocationUri`, `hosted` or an external source
//! (`smithy_query(query="doc", shape="CreateConfigurationProfileRequest",
//! member="LocationUri")`) — so the share a rollout has reached gates a parameter-backed
//! profile exactly as it gates a hosted one.
//!
//! Only a cross-service suite can state that end to end: for two of the rows the two versions
//! are Systems Manager Parameter Store versions, written through Parameter Store's own
//! operations, while AppConfig owns the profile and the rollout — and what the fleet receives
//! is read where a running application reads it, through BOTH of AppConfig's reader surfaces
//! on the whole assembled emulator (`GetConfiguration` and AppConfigData's
//! `StartConfigurationSession` + `GetLatestConfiguration`), never out of the control-plane
//! answer being checked.
//!
//! What the neighbours pin and this one does not repeat: the AppConfig crate's
//! `rollout_percentage_gates_serving` tables the gate over `GetConfiguration` for a HOSTED
//! profile, the AppConfigData crate's `rollout_percentage_splits_sessions` and
//! `zz_growth_schedule_gates_sessions` do the same for sessions over a hosted profile, and
//! this file's own neighbour `appconfig_external_source_configuration` follows an external
//! source through rollouts it waits out COMPLETELY. None of them reads a fleet while a rollout
//! of an externally stored configuration is part-way through, and none of them asserts that
//! the share the CONTROL plane reports and the share the two readers serve at are the same
//! one.
//!
//! The rows differ along what the rule generalises over: which store the two versions live in
//! (AppConfig's hosted store, a Parameter Store parameter addressed by `ssm-parameter://`, and
//! a parameter addressed by its ARN), the shape of the parameter's name, its `ParameterType`,
//! and the media type and bytes of the two configurations. Within each row the shares read
//! span a rollout that has reached nobody, three partial shares and every target.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appconfig_amazonappconfig::state as appconfig_state;

use crate::query_wire::form_encode;

/// The Region every request of this suite is signed for, and so the Region its parameters
/// live in and its configurations are read at.
const REGION: &str = "us-east-1";

/// The media type a control-plane JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The media type a Parameter Store parameter's value is described by: a parameter's value is
/// text whatever its `ParameterType` is.
const TEXT_CONTENT_TYPE: &str = "text/plain";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The `LocationUri` prefix that selects a Parameter Store parameter by name.
const PARAMETER_URI_PREFIX: &str = "ssm-parameter://";

/// The `DeploymentState` a rollout still reaching its targets is in, and the one a rollout
/// nobody stopped ends in (`smithy_query(query="shape", shape="DeploymentState")`).
const DEPLOYING: &str = "DEPLOYING";
/// See [`DEPLOYING`].
const COMPLETE: &str = "COMPLETE";

/// The `DeploymentState` values a deployment can no longer leave.
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// The shares of its targets each row's rollout is read at: none of the fleet reached, three
/// partial shares, and every target reached.
const SHARES: &[f32] = &[0.0, 25.0, 50.0, 80.0, 100.0];

/// The share the SESSION fleet is sampled at as well, which is a share strictly between the
/// two ends — the one the rule says both versions are in service at.
const PARTIAL_SHARE: f32 = 50.0;

/// How many clients read each share through `GetConfiguration`.
///
/// A client's place in the rollout follows from its `ClientId` and from nothing else, so this
/// fleet is the same fleet on every run and at every row; enough of them that each partial
/// share has clients on both sides of it.
const CLIENTS: usize = 24;

/// How many INDEPENDENT configuration sessions are opened at the partial share. A session's
/// place in the rollout is minted with the session, so a handful of them at a half-finished
/// rollout lands on both sides of it with certainty (the chance of all of them landing on one
/// side is below one in a hundred).
const SESSIONS: usize = 8;

/// How far two percentages may differ and still be the same share: the member is a float on
/// the wire, so a reading is compared with slack rather than for bit equality.
const SHARE_SLACK: f64 = 0.01;

/// How often work the clock owes — a deployment settling, a rollout reaching every target —
/// is looked for again, and how long the looking goes on.
///
/// Those spans belong to the service that owns them, so nothing here sleeps on its behalf:
/// every wait is on the CONDITION, and the deadline is a ceiling on the WAIT and nothing
/// else, generous enough that a loaded host cannot decide any outcome. No assertion here is
/// about how long anything took.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const SETTLED_BY: Duration = Duration::from_secs(120);

/// Which store a row's two configuration versions live in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Store {
    /// `hosted`: AppConfig's own configuration store.
    Hosted,
    /// A Parameter Store parameter, addressed as `ssm-parameter://<parameter name>`.
    ParameterByUri,
    /// The same kind of parameter, addressed by its ARN — the other form the member
    /// documents.
    ParameterByArn,
}

/// One row: the store the configuration lives in, and the two versions of it that store
/// holds.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    store: Store,
    /// The name Parameter Store holds the parameter under (unused by the hosted row).
    parameter_name: &'static str,
    /// A `ParameterType` value (`smithy_query(query="shape", shape="ParameterType")` of the
    /// Systems Manager model).
    parameter_type: &'static str,
    /// The media type both versions of this row are written with.
    content_type: &'static str,
    /// The bytes of the version in service before the rollout starts.
    previous: &'static str,
    /// The bytes of the version the rollout carries.
    new: &'static str,
}

/// The hosted store, a freeform JSON document.
const HOSTED_ROW: Row = Row {
    case: "the AppConfig hosted configuration store",
    store: Store::Hosted,
    parameter_name: "",
    parameter_type: "",
    content_type: JSON_CONTENT_TYPE,
    previous: r#"{"tier":"one"}"#,
    new: r#"{"tier":"two"}"#,
};

/// A top-level `String` parameter, addressed by the `ssm-parameter://` form.
const PARAMETER_BY_URI_ROW: Row = Row {
    case: "a String parameter addressed by ssm-parameter://",
    store: Store::ParameterByUri,
    parameter_name: "fleet-tier",
    parameter_type: "String",
    content_type: TEXT_CONTENT_TYPE,
    previous: "tier-one",
    new: "tier-two",
};

/// A hierarchical `StringList` parameter, addressed by its ARN.
const PARAMETER_BY_ARN_ROW: Row = Row {
    case: "a StringList parameter addressed by its ARN",
    store: Store::ParameterByArn,
    parameter_name: "/fleet/eu/endpoints",
    parameter_type: "StringList",
    content_type: TEXT_CONTENT_TYPE,
    previous: "alpha,beta",
    new: "alpha,beta,gamma",
};

/// RULE for AppConfig's own hosted store.
#[tokio::test]
async fn a_partial_rollout_of_a_hosted_configuration_splits_the_fleet() {
    watch_a_rollout(&HOSTED_ROW).await;
}

/// RULE for a configuration that lives at a Parameter Store parameter the profile addresses
/// by name: where the bytes live is not what decides the share they are served to.
#[tokio::test]
async fn a_partial_rollout_of_a_parameter_backed_configuration_splits_the_fleet() {
    watch_a_rollout(&PARAMETER_BY_URI_ROW).await;
}

/// RULE for the other documented way of naming that source, and another `ParameterType`.
#[tokio::test]
async fn a_partial_rollout_of_a_parameter_named_by_arn_splits_the_fleet() {
    watch_a_rollout(&PARAMETER_BY_ARN_ROW).await;
}

/// Put the row's first version in service, start a gradual rollout of its second, and read the
/// fleet at each share of [`SHARES`]: the share the control plane reports is the share the
/// readers serve at, a partial one serves BOTH versions' exact bytes, a rollout that has
/// reached nobody serves the previous version to every target, and the fleet converges on the
/// new version once the rollout — carried by the CLOCK, with nothing pinned — has reached
/// every target.
async fn watch_a_rollout(row: &Row) {
    let case = row.case;
    let (router, engine) = emulator();
    let hierarchy = given_a_configuration(&router, &engine, row).await;

    // The previous version goes out with a strategy that paces nothing, so it is in force for
    // every target before the rollout starts — which is what makes the previous version the
    // one a target the rollout has not reached must be served.
    let instant = given_strategy(&router, case, "instant", 0).await;
    let seeded = start_deployment(&router, case, &hierarchy, &instant, "1").await;
    let settled = settled_deployment(&router, case, &hierarchy, seeded).await;
    assert_eq!(
        settled["State"],
        json!(COMPLETE),
        "case {case:?}: the deployment of the previous version must complete, got {settled}"
    );
    let baseline = read_the_client_fleet(&router, case, &hierarchy, row, None).await;
    assert!(
        baseline.on_new.is_empty(),
        "case {case:?}: before the rollout every client must be served the previous version, \
         {} of {CLIENTS} were served the new one",
        baseline.on_new.len()
    );

    // The rollout of the new version declares a window to grow over, so the deployment is in
    // flight at whatever share it has reached while the fleet below reads it.
    let gradual = given_strategy(&router, case, "gradual", 1_440).await;
    let number = start_deployment(&router, case, &hierarchy, &gradual, "2").await;

    let mut reached_before: BTreeSet<String> = BTreeSet::new();
    for share in SHARES {
        let share = *share;
        let at = format!("{case:?} at {share}% of its targets");

        // What the CONTROL plane reports of the same rollout the readers below are served
        // from: the readers must serve at the share it reports, not at one of their own.
        pin_the_share(&engine, &hierarchy, share);
        let document = deployment(&router, case, &hierarchy, number).await;
        let reported = document["PercentageComplete"].as_f64().unwrap_or_else(|| {
            panic!("case {at}: a Deployment document reports a PercentageComplete, got {document}")
        });
        assert!(
            (reported - f64::from(share)).abs() <= SHARE_SLACK,
            "case {at}: GetDeployment must report the share the rollout has reached, got \
             {reported}"
        );
        assert_eq!(
            document["State"],
            json!(DEPLOYING),
            "case {at}: the rollout must still be under way while the fleet reads it, got \
             {document}"
        );

        let fleet = read_the_client_fleet(&router, case, &hierarchy, row, Some(share)).await;
        assert!(
            fleet.unexpected.is_empty(),
            "case {at}: every client must be served one of the two deployed versions' exact \
             bytes, {:?} were served something else",
            fleet.unexpected
        );
        if share <= 0.0 {
            assert!(
                fleet.on_new.is_empty(),
                "case {at}: a rollout that has reached no target serves the new version to \
                 nobody, got {:?}",
                fleet.on_new
            );
        } else if share >= 100.0 {
            assert_eq!(
                fleet.on_new.len(),
                CLIENTS,
                "case {at}: a rollout that has reached every target serves the new version to \
                 all of them, got {} of {CLIENTS}",
                fleet.on_new.len()
            );
        } else {
            assert!(
                !fleet.on_new.is_empty() && fleet.on_new.len() < CLIENTS,
                "case {at}: a partial rollout must leave BOTH versions in service, got {} of \
                 {CLIENTS} clients on the new version",
                fleet.on_new.len()
            );
        }

        // The share a rollout has reached only grows, and a target's place in it is its own,
        // so a client the rollout had already reached at a smaller share is still on the new
        // version at a larger one.
        let left_behind: Vec<&String> = reached_before.difference(&fleet.on_new).collect();
        assert!(
            left_behind.is_empty(),
            "case {at}: a client the rollout had already reached must not be taken back to the \
             previous version, {left_behind:?} were"
        );
        reached_before = fleet.on_new;

        if (share - PARTIAL_SHARE).abs() <= f32::EPSILON {
            let sessions = read_the_session_fleet(&router, case, &hierarchy, row, share).await;
            assert!(
                sessions.unexpected.is_empty(),
                "case {at}: every session must be served one of the two deployed versions' \
                 exact bytes, {:?} were served something else",
                sessions.unexpected
            );
            assert!(
                sessions.on_new > 0 && sessions.on_new < SESSIONS,
                "case {at}: independently opened sessions must be SPLIT by a partial rollout, \
                 got {} of {SESSIONS} on the new version",
                sessions.on_new
            );
        }
    }

    // And the fleet converges, with nothing pinned: the rollout is carried to its end by the
    // clock, and from there every reader of either surface is served the new version.
    let settled = settled_deployment(&router, case, &hierarchy, number).await;
    assert_eq!(
        settled["State"],
        json!(COMPLETE),
        "case {case:?}: a rollout nobody stopped must reach {COMPLETE}, got {settled}"
    );
    let converged = read_the_client_fleet(&router, case, &hierarchy, row, None).await;
    assert_eq!(
        converged.on_new.len(),
        CLIENTS,
        "case {case:?}: once the rollout has reached every target every client must be served \
         the new version, {} of {CLIENTS} were not",
        CLIENTS - converged.on_new.len()
    );
    let sessions = read_the_session_fleet_unpinned(&router, case, &hierarchy, row).await;
    assert_eq!(
        sessions.on_new, SESSIONS,
        "case {case:?}: once the rollout has reached every target every session must be served \
         the new version, {} of {SESSIONS} were not",
        SESSIONS - sessions.on_new
    );
}

/// What one sweep of a fleet of clients was served.
#[derive(Debug, Default)]
struct ClientFleet {
    /// The clients served the new version's bytes.
    on_new: BTreeSet<String>,
    /// The clients served neither deployed version's bytes.
    unexpected: Vec<String>,
}

/// Read the configuration once for each of [`CLIENTS`] clients through `GetConfiguration` and
/// report which version each was served.
///
/// `share`, when a caller names one, is re-pinned immediately before each read: how far a
/// rollout has got is owed by the CLOCK, so a sweep that must read the fleet AT one share puts
/// the rollout back at that share rather than racing the schedule.
async fn read_the_client_fleet(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    row: &Row,
    share: Option<f32>,
) -> ClientFleet {
    let mut fleet = ClientFleet::default();
    for index in 0..CLIENTS {
        let client = format!("canary-client-{index:02}");
        if let Some(share) = share {
            pin_the_share(&hierarchy.engine, hierarchy, share);
        }
        let read = appconfig(
            router,
            "GET",
            &format!(
                "/applications/{}/environments/{}/configurations/{}?client_id={}",
                hierarchy.application,
                hierarchy.environment,
                hierarchy.profile,
                form_encode(&client)
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetConfiguration for {client} must answer 200, got {} with {}",
            read.status,
            read.text()
        );
        match read.text() {
            served if served == row.new => {
                fleet.on_new.insert(client);
            }
            served if served == row.previous => {}
            served => fleet.unexpected.push(format!("{client} was served {served:?}")),
        }
    }
    fleet
}

/// What one sweep of a fleet of configuration sessions was served.
#[derive(Debug, Default)]
struct SessionFleet {
    /// How many sessions were served the new version's bytes.
    on_new: usize,
    /// The sessions served neither deployed version's bytes.
    unexpected: Vec<String>,
}

/// Open [`SESSIONS`] independent sessions over the triple, poll each once, and report which
/// version each was served — with the rollout re-pinned at `share` before each session, which
/// is the share that session's own place in the rollout is read against.
async fn read_the_session_fleet(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    row: &Row,
    share: f32,
) -> SessionFleet {
    let mut fleet = SessionFleet::default();
    for index in 0..SESSIONS {
        pin_the_share(&hierarchy.engine, hierarchy, share);
        classify_a_session(router, case, hierarchy, row, index, &mut fleet).await;
    }
    fleet
}

/// The same sweep with NOTHING pinned, for the fleet a settled rollout has reached.
async fn read_the_session_fleet_unpinned(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    row: &Row,
) -> SessionFleet {
    let mut fleet = SessionFleet::default();
    for index in 0..SESSIONS {
        classify_a_session(router, case, hierarchy, row, index, &mut fleet).await;
    }
    fleet
}

/// Open one session, poll it once, and record which of the two versions it was served.
async fn classify_a_session(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    row: &Row,
    index: usize,
    fleet: &mut SessionFleet,
) {
    let opened = appconfigdata(
        router,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": hierarchy.application,
            "EnvironmentIdentifier": hierarchy.environment,
            "ConfigurationProfileIdentifier": hierarchy.profile,
        }),
    )
    .await;
    assert_eq!(
        opened.status,
        201,
        "case {case:?}: StartConfigurationSession {index} must answer 201, got {} with {}",
        opened.status,
        opened.text()
    );
    let token = opened.string("InitialConfigurationToken").unwrap_or_else(|| {
        panic!(
            "case {case:?}: StartConfigurationSession answered no token: {}",
            opened.text()
        )
    });
    let read = appconfigdata(
        router,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetLatestConfiguration for session {index} must answer 200, got {} with \
         {}",
        read.status,
        read.text()
    );
    match read.text() {
        served if served == row.new => fleet.on_new += 1,
        served if served == row.previous => {}
        served => fleet
            .unexpected
            .push(format!("session {index} was served {served:?}")),
    }
    // A served document is one hosted or source version's own bytes, described by that
    // store's own media type: a session the rollout has not reached is served the previous
    // version whole rather than anything derived from the one being rolled out.
    let content_type = read.header("content-type").unwrap_or_default();
    assert_eq!(
        content_type, row.content_type,
        "case {case:?}: session {index} must be served the configuration's own media type {:?}, \
         got {content_type:?}",
        row.content_type
    );
}

/// Pin the in-flight deployment's `PercentageComplete` at `share`, with its start instant
/// moved to now so the clock owes the record nothing while the read that follows takes it.
///
/// The one thing no operation exposes: how far a rollout has got is owed by the CLOCK, and a
/// suite that reads the fleet AT a given share writes that share rather than sleeping through
/// the schedule. Nothing else is written — no state, no event — and a deployment that is not
/// in flight is left exactly as it stands, because the share it reached is not the share it is
/// available to once it has been undone.
fn pin_the_share(engine: &Arc<engine::InMemoryEngine>, hierarchy: &Hierarchy, share: f32) {
    let held = engine.state::<appconfig_state::ComAmazonawsAppconfigAmazonappconfigState>();
    let mut store = held.lock();
    let environment = store
        .application_mut(&hierarchy.application)
        .and_then(|application| application.environments.get_mut(&hierarchy.environment))
        .expect("the environment the rollout is deploying into");
    let now = SystemTime::now();
    for deployment in environment.deployments.values_mut() {
        if deployment.is_in_flight() {
            deployment.started_at = now;
            deployment.percentage_complete = share;
        }
    }
}

/// One row's AppConfig hierarchy as the control plane minted it, beside the engine the
/// emulator runs on — which is what the share a rollout has reached is pinned through.
struct Hierarchy {
    application: String,
    environment: String,
    profile: String,
    engine: Arc<engine::InMemoryEngine>,
}

/// Create the row's application, environment and configuration profile, and write the two
/// versions of its configuration into the store its `LocationUri` names.
async fn given_a_configuration(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Row,
) -> Hierarchy {
    let case = row.case;
    // Each row's parameter holds both versions before anything is deployed: a deployment names
    // a version of the SOURCE, so both have to exist at the source for either to be deployed.
    if row.store != Store::Hosted {
        for (expected, value) in [(1, row.previous), (2, row.new)] {
            let minted = put_parameter(router, row, value, expected > 1).await;
            assert_eq!(
                minted, expected,
                "case {case:?}: writing {value:?} to {:?} must mint version {expected}, got \
                 {minted}",
                row.parameter_name
            );
        }
    }

    let application = minted_id(
        case,
        "the application",
        &appconfig(
            router,
            "POST",
            "/applications",
            &json!({"Name": "canary-app"}),
        )
        .await,
    );
    let environment = minted_id(
        case,
        "the environment",
        &appconfig(
            router,
            "POST",
            &format!("/applications/{application}/environments"),
            &json!({"Name": "production"}),
        )
        .await,
    );
    let mut request = json!({"Name": "flags", "LocationUri": location_uri(row)});
    if row.store != Store::Hosted {
        // A `RetrievalRoleArn` is "required for all other sources that store your
        // configuration" (`smithy_query(query="doc",
        // shape="CreateConfigurationProfileRequest", member="RetrievalRoleArn")`).
        request["RetrievalRoleArn"] = json!(::engine::emulator_arn(
            "iam",
            "",
            "role/appconfig-canary-retrieval"
        ));
    }
    let profile = minted_id(
        case,
        "the configuration profile",
        &appconfig(
            router,
            "POST",
            &format!("/applications/{application}/configurationprofiles"),
            &request,
        )
        .await,
    );

    if row.store == Store::Hosted {
        for (expected, content) in [(1, row.previous), (2, row.new)] {
            let written = dispatch(
                router,
                "appconfig",
                "POST",
                &format!(
                    "/applications/{application}/configurationprofiles/{profile}\
                     /hostedconfigurationversions"
                ),
                row.content_type,
                content.as_bytes().to_vec(),
            )
            .await;
            assert_eq!(
                written.status,
                201,
                "case {case:?}: writing hosted version {expected} must answer 201, got {} with \
                 {}",
                written.status,
                written.text()
            );
        }
    }

    Hierarchy {
        application,
        environment,
        profile,
        engine: Arc::clone(engine),
    }
}

/// The `LocationUri` a row's profile is created with: the hosted store, the parameter's
/// `ssm-parameter://` form, or the parameter's own ARN
/// (`arn:aws:ssm:<region>:<account>:parameter/<name>`, whose path keeps a hierarchical name's
/// whole path after `parameter/`).
fn location_uri(row: &Row) -> String {
    match row.store {
        Store::Hosted => HOSTED.to_string(),
        Store::ParameterByUri => format!("{PARAMETER_URI_PREFIX}{}", row.parameter_name),
        Store::ParameterByArn => ::engine::emulator_arn(
            "ssm",
            REGION,
            &format!(
                "parameter/{}",
                row.parameter_name
                    .strip_prefix('/')
                    .unwrap_or(row.parameter_name)
            ),
        ),
    }
}

/// Write one version of a row's parameter through Parameter Store's own wire, and answer the
/// version number it minted.
async fn put_parameter(
    router: &MultiServiceRouter,
    row: &Row,
    value: &str,
    overwrite: bool,
) -> i64 {
    let answered = dispatch_rpc(
        router,
        "ssm",
        "AmazonSSM.PutParameter",
        &json!({
            "Name": row.parameter_name,
            "Type": row.parameter_type,
            "Value": value,
            "Overwrite": overwrite,
        }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {:?}: PutParameter({:?}) must succeed, got {} with {}",
        row.case,
        row.parameter_name,
        answered.status,
        answered.text()
    );
    answered.json()["Version"].as_i64().unwrap_or_else(|| {
        panic!(
            "case {:?}: PutParameter answered no Version: {}",
            row.case,
            answered.text()
        )
    })
}

/// A deployment strategy that reaches every target in one growth step and bakes for no time,
/// over a rollout window of `duration_in_minutes` — a strategy that declares neither paces
/// nothing at all, so its deployment is already settled when `StartDeployment` answers.
async fn given_strategy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    duration_in_minutes: i32,
) -> String {
    let created = appconfig(
        router,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": name,
            "DeploymentDurationInMinutes": duration_in_minutes,
            "GrowthFactor": 100.0,
            "GrowthType": "LINEAR",
            "FinalBakeTimeInMinutes": 0,
            "ReplicateTo": "NONE",
        }),
    )
    .await;
    minted_id(case, &format!("the {name} deployment strategy"), &created)
}

/// `StartDeployment` of `version` into the row's environment, answering the deployment number.
async fn start_deployment(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    strategy: &str,
    version: &str,
) -> i64 {
    let started = appconfig(
        router,
        "POST",
        &format!(
            "/applications/{}/environments/{}/deployments",
            hierarchy.application, hierarchy.environment
        ),
        &json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": hierarchy.profile,
            "ConfigurationVersion": version,
        }),
    )
    .await;
    assert_eq!(
        started.status,
        201,
        "case {case:?}: StartDeployment({version:?}) must answer 201, got {} with {}",
        started.status,
        started.text()
    );
    started.json()["DeploymentNumber"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartDeployment answered no DeploymentNumber: {}",
                started.text()
            )
        })
}

/// One `GetDeployment`, which must be answered.
async fn deployment(
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
    read.json()
}

/// Read one deployment until it reaches a state it can no longer leave, and answer the document
/// that reported it. The transitions are the service's own, so the wait is on the CONDITION.
async fn settled_deployment(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    number: i64,
) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let document = deployment(router, case, hierarchy, number).await;
        let state = document["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {number} never settled, it is still {state:?} at {}% of \
             its targets",
            document["PercentageComplete"]
        );
        tokio::time::sleep(LOOK).await;
    }
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

/// The whole emulator over one engine, exactly as the binary assembles it, with the engine
/// handed back beside it: the share a rollout has reached is pinned through that engine.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    (router, engine)
}

/// What one dispatched request answered.
struct Answered {
    status: u16,
    headers: ::http::HeaderMap,
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

    /// The body as text: the served configuration's own bytes for a data-plane read, and the
    /// document for everything else.
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

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and the Region
/// this suite signs for, which is what routes the request and what each handler reads its
/// Region from. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one `restJson1` call at the endpoint host of `service`. Both of AppConfig's APIs
/// sign as `appconfig`, whichever endpoint a client addresses.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header("authorization", authorization("appconfig"))
        .header("content-type", content_type)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Dispatch one `awsJson1_1` RPC, which names its operation in `X-Amz-Target`.
async fn dispatch_rpc(
    router: &MultiServiceRouter,
    service: &str,
    target: &str,
    body: &Value,
) -> Answered {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host.clone())
        .header("authorization", authorization(service))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string().into_bytes()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One AppConfig CONTROL-plane call carrying a JSON body.
async fn appconfig(
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
    dispatch(router, "appconfig", method, uri, JSON_CONTENT_TYPE, payload).await
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
    dispatch(
        router,
        "appconfigdata",
        method,
        uri,
        JSON_CONTENT_TYPE,
        payload,
    )
    .await
}
