// SPDX-License-Identifier: CC0-1.0

//! The TIME-BASED traffic shift a Lambda deployment performs, and what an ALARM
//! interrupting one does — end to end over the wire, with Lambda and CloudWatch
//! mounted beside CodeDeploy on one engine.
//!
//! `tests/all/lambda_traffic_shift.rs` holds the rule that a deployment carries
//! out the shift its AppSpec declares at all; this suite is about the INCREMENTS
//! that shift is carried out in and the window they hold open:
//!
//! * a deployment shifts traffic in the increments its resolved
//!   `trafficRoutingConfig` describes — a `TimeBasedCanary` in two, a
//!   `TimeBasedLinear` in equal ones, `AllAtOnce` in a single step — and while an
//!   increment other than the last holds, the alias its AppSpec names keeps
//!   invoking the version the deployment is shifting from with that increment's
//!   share of the traffic weighted onto the target version; the deployment stays
//!   non-terminal for the whole window and the last increment moves the alias over
//!   with no weighted routing left behind;
//! * an alarm the deployment's effective `alarmConfiguration` names that starts
//!   sounding WHILE that window is open stops the deployment (`Stopped`, with
//!   `errorInformation.code` `ALARM_ACTIVE`), and when the effective
//!   `autoRollbackConfiguration` names `DEPLOYMENT_STOP_ON_ALARM` the rollback
//!   deployment it creates puts the alias back on the version the stopped
//!   deployment started from.
//!
//! Every alias here is created through `CreateAlias` and read through `GetAlias`,
//! and every alarm through `PutMetricAlarm`/`SetAlarmState`: the operations a
//! client calls, so nothing reaches into any service's store.
//!
//! The setup below — a function with published versions and an alias, an AppSpec
//! that shifts it, a Lambda deployment group under a traffic routing
//! configuration, and the alias sampling that watches a shift happen — is
//! `pub(crate)` because `tests/all/alarm_blocked_lambda_release.rs` is about the
//! same alias under the same shifts; there is one such harness in this target,
//! not one per suite.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{terminal_deployment_within, Emulator};

/// The service role every deployment group here is created with.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The execution role every function here is created with.
const EXECUTION_ROLE: &str = "arn:aws:iam::123456789012:role/LambdaExecution";

/// The alias every deployment here shifts traffic on.
pub(crate) const ALIAS: &str = "live";

/// How long a case here waits for a transition that falls due without a configured
/// interval in front of it — a function reported `Active`, a deployment that shifts
/// all of its traffic at once reaching a terminal status, an increment that is due
/// the moment the deployment starts reaching the alias.
///
/// Generous for such a transition, and far below the span a configured interval
/// holds an increment for.
pub(crate) const WINDOW_DEADLINE: Duration = Duration::from_secs(30);

/// How long a case here waits for a deployment whose traffic routing configuration
/// states an INTERVAL to walk that window out.
///
/// `canaryInterval` and `linearInterval` are minutes the caller asked for, so a
/// deployment under one of them is non-terminal for at least that many minutes:
/// every case below states a one-minute interval, and this deadline leaves that
/// window room to close on a loaded host several times over.
pub(crate) const SHIFT_WINDOW_DEADLINE: Duration = Duration::from_secs(240);

/// See [`WINDOW_DEADLINE`]: fine enough to observe an increment while it holds.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_millis(40);

/// The deployment configuration that shifts all of a Lambda deployment's traffic in
/// ONE step, which is what the setup legs of a case here run under: a leg that only
/// has to leave the alias on a known version waits out no configured interval.
pub(crate) const ALL_AT_ONCE: &str = "CodeDeployDefault.LambdaAllAtOnce";

/// How many invocations one row of the split table draws through the alias while an
/// increment holds.
///
/// Large enough that the share observed is a share and not an accident: at this
/// many samples a correctly drawn split sits within [`SHARE_TOLERANCE`] of its
/// weight with overwhelming probability, while a shift that cut over in full, or
/// moved nothing at all, is far outside it.
const DRAWS_PER_CASE: usize = 200;

/// How far the observed share of the version being released may sit from the weight
/// the increment put on it: several standard deviations of a draw of
/// [`DRAWS_PER_CASE`] samples, and narrower than the gap between the rows below.
const SHARE_TOLERANCE: f64 = 0.15;

/// The interval, in minutes, the configurations of the split table state.
///
/// The round of draws is taken while the FIRST increment holds, so the window only
/// has to outlast that round on any host: an interval of several minutes leaves it
/// room to spare, and no case waits for the window to close — each ends its
/// deployment with `StopDeployment`.
const HOLDING_INTERVAL_MINUTES: i64 = 5;

/// The `ErrorCode` a deployment its alarms stopped reports
/// (`smithy_query(query="shape",shape="ErrorCode")` carries it).
pub(crate) const ALARM_ACTIVE: &str = "ALARM_ACTIVE";

/// The `DeploymentCreator` of a deployment CodeDeploy created to roll another one
/// back (`smithy_query(query="shape",shape="DeploymentCreator")`).
pub(crate) const CREATOR_ROLLBACK: &str = "codeDeployRollback";

/// What one alias reports at an instant: the version it invokes, and the weights
/// its routing configuration carries.
pub(crate) type AliasReading = (String, Vec<(String, f64)>);

/// Create a function named `name` and publish `versions` versions of it, then
/// point a `live` alias at version 1.
///
/// The deployment package is a blob uploaded inline: nothing here invokes the
/// function, so what the archive holds is immaterial — only the VERSIONS it is
/// published as are, because those are what an alias and a traffic shift name.
pub(crate) async fn function_with_versions(emulator: &Emulator, name: &str, versions: usize) {
    emulator
        .lambda_ok(
            "CreateFunction",
            "POST",
            "/2015-03-31/functions",
            json!({
                "FunctionName": name,
                "Role": EXECUTION_ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": {"ZipFile": "cGFja2FnZQ=="},
            }),
            201,
        )
        .await;
    wait_until_active(emulator, name).await;
    for version in 1..=versions {
        emulator
            .lambda_ok(
                "PublishVersion",
                "POST",
                &format!("/2015-03-31/functions/{name}/versions"),
                // A description per version: "Lambda doesn't publish a version if
                // the function's configuration and code haven't changed since the
                // last version", and the description is part of that
                // configuration, so each call publishes its own version.
                json!({"Description": format!("version {version}")}),
                201,
            )
            .await;
    }
    emulator
        .lambda_ok(
            "CreateAlias",
            "POST",
            &format!("/2015-03-31/functions/{name}/aliases"),
            json!({"Name": ALIAS, "FunctionVersion": "1"}),
            201,
        )
        .await;
}

/// Poll `GetFunctionConfiguration` until Lambda reports the function `Active`: a
/// freshly created function is owed that transition before a version of it can be
/// published.
async fn wait_until_active(emulator: &Emulator, name: &str) {
    let deadline = Instant::now() + WINDOW_DEADLINE;
    loop {
        let reported = emulator
            .lambda_ok(
                "GetFunctionConfiguration",
                "GET",
                &format!("/2015-03-31/functions/{name}/configuration"),
                Value::Null,
                200,
            )
            .await;
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {name:?} Active, last {reported:?}"
        );
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// What the alias of `function` reports right now.
pub(crate) async fn alias_reading(emulator: &Emulator, function: &str) -> AliasReading {
    let reported = emulator
        .lambda_ok(
            "GetAlias",
            "GET",
            &format!("/2015-03-31/functions/{function}/aliases/{ALIAS}"),
            Value::Null,
            200,
        )
        .await;
    let version = reported
        .get("FunctionVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut weights: Vec<(String, f64)> = reported
        .get("RoutingConfig")
        .and_then(|routing| routing.get("AdditionalVersionWeights"))
        .and_then(Value::as_object)
        .map(|weights| {
            weights
                .iter()
                .map(|(version, weight)| {
                    (version.clone(), weight.as_f64().unwrap_or_default())
                })
                .collect()
        })
        .unwrap_or_default();
    weights.sort_by(|left, right| left.0.cmp(&right.0));
    (version, weights)
}

/// An `appSpecContent` revision shifting `alias` of `function` from
/// `current_version` to `target_version`, in the AppSpec reference's own spelling.
pub(crate) fn appspec_revision(
    function: &str,
    alias: &str,
    current_version: &str,
    target_version: &str,
) -> Value {
    let content = format!(
        "version: 0.0\nResources:\n  - {function}:\n      Type: AWS::Lambda::Function\n      \
         Properties:\n        Name: {function}\n        Alias: {alias}\n        \
         CurrentVersion: \"{current_version}\"\n        TargetVersion: \"{target_version}\"\n"
    );
    json!({"revisionType": "AppSpecContent", "appSpecContent": {"content": content}})
}

/// A `trafficRoutingConfig` of `kind` shifting `percentage` every `interval`
/// minutes; `AllAtOnce` carries neither increment configuration.
pub(crate) fn traffic_routing(kind: &str, percentage: i64, interval: i64) -> Value {
    match kind {
        "TimeBasedCanary" => json!({
            "type": kind,
            "timeBasedCanary": {"canaryPercentage": percentage, "canaryInterval": interval},
        }),
        "TimeBasedLinear" => json!({
            "type": kind,
            "timeBasedLinear": {"linearPercentage": percentage, "linearInterval": interval},
        }),
        _ => json!({"type": kind}),
    }
}

/// Create a deployment configuration of `name` carrying `routing`, an application
/// and one Lambda deployment group of it carrying `extras`, answering the group's
/// name.
pub(crate) async fn lambda_group(
    emulator: &Emulator,
    application: &str,
    routing: Option<Value>,
    extras: &Value,
) -> String {
    let config = match routing {
        Some(routing) => {
            let name = format!("{application}-config");
            emulator
                .ok(
                    "CreateDeploymentConfig",
                    json!({
                        "deploymentConfigName": name,
                        "computePlatform": "Lambda",
                        "trafficRoutingConfig": routing,
                    }),
                )
                .await;
            name
        }
        None => "CodeDeployDefault.LambdaAllAtOnce".to_string(),
    };
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": "Lambda"}),
        )
        .await;
    let group = format!("{application}-dg");
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "serviceRoleArn": SERVICE_ROLE,
        "deploymentConfigName": config,
    });
    for (member, value) in extras.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    emulator.ok("CreateDeploymentGroup", request).await;
    group
}

/// Create a deployment of `revision`, answering the id CodeDeploy minted.
async fn create_deployment(
    emulator: &Emulator,
    application: &str,
    group: &str,
    revision: &Value,
) -> String {
    create_deployment_under(emulator, application, group, revision, None).await
}

/// [`create_deployment`] under the deployment configuration `config` names rather
/// than the group's own, which is how a client asks one deployment for a different
/// traffic routing configuration (`CreateDeployment$deploymentConfigName`).
async fn create_deployment_under(
    emulator: &Emulator,
    application: &str,
    group: &str,
    revision: &Value,
    config: Option<&str>,
) -> String {
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "revision": revision,
    });
    if let Some(config) = config {
        request["deploymentConfigName"] = json!(config);
    }
    emulator.ok("CreateDeployment", request).await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Sample the alias of `function` until it holds `expected`, answering nothing: the
/// increment the clock owes the alias is written whether or not anybody polls, so
/// a case waits for the CONDITION rather than for a span of time.
pub(crate) async fn alias_reaches(
    emulator: &Emulator,
    function: &str,
    expected: &AliasReading,
    case: &str,
) {
    let deadline = Instant::now() + SHIFT_WINDOW_DEADLINE;
    loop {
        let reading = alias_reading(emulator, function).await;
        if &reading == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the alias at {expected:?} within \
             {SHIFT_WINDOW_DEADLINE:?}, last read {reading:?}"
        );
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// Sample the alias of `function` until `deployment` reaches a terminal status,
/// answering every distinct reading observed in order, and that terminal
/// `deploymentInfo`.
pub(crate) async fn readings_until_terminal(
    emulator: &Emulator,
    function: &str,
    deployment: &str,
    case: &str,
) -> (Vec<AliasReading>, Value) {
    readings_until_terminal_within(emulator, function, deployment, case, WINDOW_DEADLINE).await
}

/// [`readings_until_terminal`] with the deadline the caller allows, for a
/// deployment whose configuration holds its window open for a configured interval.
pub(crate) async fn readings_until_terminal_within(
    emulator: &Emulator,
    function: &str,
    deployment: &str,
    case: &str,
    within: Duration,
) -> (Vec<AliasReading>, Value) {
    let deadline = Instant::now() + within;
    let mut observed: Vec<AliasReading> = Vec::new();
    loop {
        let reading = alias_reading(emulator, function).await;
        if observed.last() != Some(&reading) {
            observed.push(reading);
        }
        let info = emulator
            .ok("GetDeployment", json!({"deploymentId": deployment}))
            .await["deploymentInfo"]
            .clone();
        if matches!(
            info["status"].as_str(),
            Some("Succeeded" | "Failed" | "Stopped")
        ) {
            // One last reading: the final increment is written in the same pass
            // that settles the deployment.
            let reading = alias_reading(emulator, function).await;
            if observed.last() != Some(&reading) {
                observed.push(reading);
            }
            return (observed, info);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a terminal status within {within:?}, got {:?} with the \
             alias at {:?}",
            info["status"],
            observed.last()
        );
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

/// RULE: stopping a deployment mid-shift ENDS its traffic shift where it stands,
/// and the rollback the caller asks for with it puts the alias back the way that
/// deployment found it — on the version its AppSpec was shifting from, with the
/// weighted routing the shift added cleared; a stop that asks for no rollback
/// leaves the alias on the increment that was in force.
#[tokio::test]
async fn a_stop_that_rolls_back_restores_the_alias_the_shift_had_moved() {
    // Rows differ along whether the stop asks for the rollback at all, which is
    // what decides whether the traffic that had moved goes back.
    let cases: &[(&str, Option<bool>, AliasReading)] = &[
        (
            "a stop that asks for a rollback",
            Some(true),
            ("1".to_string(), Vec::new()),
        ),
        (
            "a stop that asks for none",
            Some(false),
            ("1".to_string(), vec![("2".to_string(), 0.1)]),
        ),
        (
            "a stop that says nothing about a rollback",
            None,
            ("1".to_string(), vec![("2".to_string(), 0.1)]),
        ),
    ];

    for (index, (case, auto_rollback, expected)) in cases.iter().enumerate() {
        let emulator = Emulator::with_lambda();
        let function = format!("stop-fn-{index}");
        let application = format!("stop-app-{index}");
        function_with_versions(&emulator, &function, 2).await;
        // A canary whose interval is long enough that the window is comfortably
        // open while the stop is made: the SECOND increment is what the stop has
        // to come before, and nothing here measures how long the window is.
        let group = lambda_group(
            &emulator,
            &application,
            Some(traffic_routing("TimeBasedCanary", 10, 5)),
            &json!({}),
        )
        .await;
        let deployment = create_deployment(
            &emulator,
            &application,
            &group,
            &appspec_revision(&function, ALIAS, "1", "2"),
        )
        .await;

        // The canary increment is on the alias: the deployment has moved traffic,
        // which is what the stop then has to deal with.
        let canary = ("1".to_string(), vec![("2".to_string(), 0.1)]);
        let deadline = Instant::now() + WINDOW_DEADLINE;
        loop {
            if alias_reading(&emulator, &function).await == canary {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the canary increment never reached the alias"
            );
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }

        let mut request = json!({"deploymentId": deployment});
        if let Some(enabled) = auto_rollback {
            request["autoRollbackEnabled"] = json!(enabled);
        }
        emulator.ok("StopDeployment", request).await;

        let info = emulator
            .ok("GetDeployment", json!({"deploymentId": deployment}))
            .await["deploymentInfo"]
            .clone();
        assert_eq!(
            info["status"].as_str(),
            Some("Stopped"),
            "case {case:?}: expected the stop to end the deployment, got {info:?}"
        );
        // The stop has taken effect, so the alias holds what it holds: no further
        // increment of a terminal deployment can move it.
        let reading = alias_reading(&emulator, &function).await;
        assert_eq!(
            &reading, expected,
            "case {case:?}: expected the alias at {expected:?} after the stop, got {reading:?}"
        );
    }
}

/// RULE: a deployment shifts traffic in the increments its resolved
/// `trafficRoutingConfig` describes — each one but the last holding the version
/// the deployment started from with that increment's share weighted onto the
/// target version — and only the last increment moves the alias over and clears
/// the weighted routing.
#[tokio::test]
async fn a_deployment_walks_its_alias_through_the_increments_its_configuration_describes() {
    /// One configuration, and the weight ladder the alias must walk under it.
    struct Case {
        case: &'static str,
        /// `trafficRoutingConfig.type`, the percentage one increment moves and
        /// the interval in minutes between increments, or `None` for the built-in
        /// that shifts all traffic at once.
        routing: Option<(&'static str, i64, i64)>,
        /// The weights on the TARGET version, in order, before the cutover.
        ladder: Vec<f64>,
        /// How long the whole walk may take: a configuration that states an
        /// interval holds its first increment for it, one that states none cuts
        /// over as soon as the deployment runs.
        within: Duration,
    }

    // Rows differ along whether the configuration shifts in increments at all and
    // along the share its first increment moves; the configurations whose ladder
    // has SEVERAL rungs are walked by the test below, which reads each rung as it
    // falls due instead of waiting the whole ladder out.
    let cases = [
        Case {
            case: "a 10% canary",
            routing: Some(("TimeBasedCanary", 10, 1)),
            ladder: vec![0.1],
            within: SHIFT_WINDOW_DEADLINE,
        },
        Case {
            case: "the built-in that shifts all traffic at once",
            routing: None,
            ladder: Vec::new(),
            within: WINDOW_DEADLINE,
        },
        Case {
            // `canaryInterval`/`linearInterval` have a published minimum of 0
            // minutes, and a ladder with no wait between its rungs is owed every
            // one of them at the same instant: the alias is owed the state it
            // should hold NOW rather than each state it passed through, so the
            // walk is the cutover alone.
            case: "a 25% linear shift with no wait between its increments",
            routing: Some(("TimeBasedLinear", 25, 0)),
            ladder: Vec::new(),
            within: WINDOW_DEADLINE,
        },
    ];

    for (index, Case {
        case,
        routing,
        ladder,
        within,
    }) in cases.iter().enumerate()
    {
        let emulator = Emulator::with_lambda_and_cloudwatch();
        let function = format!("shift-fn-{index}");
        let application = format!("shift-app-{index}");
        function_with_versions(&emulator, &function, 2).await;
        let group = lambda_group(
            &emulator,
            &application,
            routing
                .map(|(kind, percentage, interval)| traffic_routing(kind, percentage, interval)),
            &Value::Null,
        )
        .await;
        let deployment = create_deployment(
            &emulator,
            &application,
            &group,
            &appspec_revision(&function, ALIAS, "1", "2"),
        )
        .await;

        let (observed, info) =
            readings_until_terminal_within(&emulator, &function, &deployment, case, *within)
                .await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: a shift nothing stopped must succeed, got {info:?}"
        );

        // What the alias passed through: the version the deployment started from
        // under each increment's weight, then the target version with none. The
        // first reading is taken as the deployment is accepted, before the clock
        // owes it its start, so an unweighted reading of the starting version
        // there is the state it is shifting FROM rather than an increment.
        let starting: AliasReading = ("1".to_string(), Vec::new());
        let walked = match observed.split_first() {
            Some((first, rest)) if *first == starting => rest.to_vec(),
            _ => observed.clone(),
        };
        let mut expected: Vec<AliasReading> = Vec::new();
        for weight in ladder {
            expected.push(("1".to_string(), vec![("2".to_string(), *weight)]));
        }
        expected.push(("2".to_string(), Vec::new()));
        assert_eq!(
            walked, expected,
            "case {case:?}: expected the alias to walk {expected:?}, got {observed:?}"
        );
    }
}

/// RULE: a traffic routing configuration that states an interval moves ONE
/// increment per interval — `linearInterval` is "the number of minutes between
/// each incremental traffic shift" — so the increment after the first is not on
/// the alias when the deployment starts, the deployment is still running once it
/// arrives, and the increments after THAT are still ahead of it.
#[tokio::test]
async fn a_shift_moves_one_increment_per_configured_interval() {
    let case = "a 25% linear shift every minute";
    let emulator = Emulator::with_lambda();
    let function = "ladder-fn";
    let application = "ladder-app";
    function_with_versions(&emulator, function, 2).await;
    let group = lambda_group(
        &emulator,
        application,
        Some(traffic_routing("TimeBasedLinear", 25, 1)),
        &Value::Null,
    )
    .await;
    let deployment = create_deployment(
        &emulator,
        application,
        &group,
        &appspec_revision(function, ALIAS, "1", "2"),
    )
    .await;

    // The first increment is due as the deployment starts: a quarter of the
    // traffic on the target version, the alias still invoking the version the
    // deployment is shifting from.
    let first: AliasReading = ("1".to_string(), vec![("2".to_string(), 0.25)]);
    alias_reaches(&emulator, function, &first, case).await;

    // The second falls due one configured interval later, and nothing before it
    // may: a deployment that walked its whole ladder at once would be past both
    // this rung and its own completion by now.
    let second: AliasReading = ("1".to_string(), vec![("2".to_string(), 0.5)]);
    alias_reaches(&emulator, function, &second, case).await;
    let info = emulator
        .ok("GetDeployment", json!({"deploymentId": deployment}))
        .await["deploymentInfo"]
        .clone();
    assert_eq!(
        info["status"].as_str(),
        Some("InProgress"),
        "case {case:?}: the two increments still ahead of this shift keep the deployment \
         running, got {info:?}"
    );

    // The rest of the ladder is a wait this case does not need: the stop ends it
    // where it stands.
    emulator
        .ok("StopDeployment", json!({"deploymentId": deployment}))
        .await;
}

/// RULE: an increment other than the last is LIVE TRAFFIC ROUTING, not a report:
/// while it holds, the alias serves the share of its invocations the increment put
/// on the target version and the rest on the version the deployment is shifting
/// from, so a client measuring a canary sees roughly the configured percentage on
/// the version being released.
#[tokio::test]
async fn an_increment_that_holds_splits_invocations_between_the_two_versions() {
    /// One row: the configuration, and the share of the invocations the version
    /// being released must receive while its first increment holds.
    struct Case {
        case: &'static str,
        kind: &'static str,
        percentage: i64,
    }

    // Rows differ along the share the increment moves and along the kind of
    // configuration that moves it. Each states a long interval, so the window the
    // round of draws is taken in cannot close under it on any host — how long a
    // window lasts is the test above's subject, not this one's — and the stop at
    // the end of each row is what ends the deployment.
    let cases = [
        Case {
            case: "a 10% canary",
            kind: "TimeBasedCanary",
            percentage: 10,
        },
        Case {
            case: "a 50% canary",
            kind: "TimeBasedCanary",
            percentage: 50,
        },
        Case {
            case: "a 75% canary",
            kind: "TimeBasedCanary",
            percentage: 75,
        },
        Case {
            case: "the first increment of a 25% linear shift",
            kind: "TimeBasedLinear",
            percentage: 25,
        },
    ];

    for (index, Case {
        case,
        kind,
        percentage,
    }) in cases.iter().enumerate()
    {
        let emulator = Emulator::with_lambda();
        let function = format!("split-fn-{index}");
        let application = format!("split-app-{index}");
        function_with_versions(&emulator, &function, 2).await;
        let group = lambda_group(
            &emulator,
            &application,
            Some(traffic_routing(kind, *percentage, HOLDING_INTERVAL_MINUTES)),
            &Value::Null,
        )
        .await;
        let deployment = create_deployment(
            &emulator,
            &application,
            &group,
            &appspec_revision(&function, ALIAS, "1", "2"),
        )
        .await;

        #[allow(clippy::cast_precision_loss)]
        let weight = *percentage as f64 / 100.0;
        let holding: AliasReading = ("1".to_string(), vec![("2".to_string(), weight)]);
        alias_reaches(&emulator, &function, &holding, case).await;

        // The draws are DRY RUNS of the alias — "validate parameter values and
        // verify that the user or role has permission to invoke the function" —
        // which resolve the qualifier and run nothing, so a case can take enough
        // of them to measure a share rather than observe an accident.
        let mut on_target = 0_usize;
        for draw in 0..DRAWS_PER_CASE {
            let executed = emulator.lambda_executed_version(&function, ALIAS).await;
            assert!(
                executed == "1" || executed == "2",
                "case {case:?} draw {draw}: an invocation may only reach one of the two \
                 versions the shift is between, got {executed:?}"
            );
            if executed == "2" {
                on_target += 1;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let observed = on_target as f64 / DRAWS_PER_CASE as f64;
        assert!(
            (observed - weight).abs() <= SHARE_TOLERANCE,
            "case {case:?}: the version being released receives the share the increment \
             weighted onto it, so over {DRAWS_PER_CASE} invocations its observed share must \
             be within {SHARE_TOLERANCE} of {weight}, got {observed} ({on_target} of \
             {DRAWS_PER_CASE})"
        );

        // The window was open for the whole round of draws: the increment that
        // holds is one of several, and the deployment is not over while it does.
        let info = emulator
            .ok("GetDeployment", json!({"deploymentId": deployment}))
            .await["deploymentInfo"]
            .clone();
        assert_eq!(
            info["status"].as_str(),
            Some("InProgress"),
            "case {case:?}: the deployment must still be running while its increment holds, \
             got {info:?}"
        );
        emulator
            .ok("StopDeployment", json!({"deploymentId": deployment}))
            .await;
    }
}

/// RULE: an alarm that starts sounding while a deployment's traffic-shift window
/// is open stops that deployment — `Stopped`, with `errorInformation.code`
/// `ALARM_ACTIVE` — and the rollback its `DEPLOYMENT_STOP_ON_ALARM` configuration
/// asks for puts the alias back the way it was before the stopped deployment
/// moved it; the same deployment under an alarm that stays quiet succeeds and
/// leaves the alias on the target version.
#[tokio::test]
async fn an_alarm_sounding_mid_shift_stops_the_deployment_and_its_rollback_restores_the_alias() {
    for (index, (case, breaches)) in [
        ("the alarm starts sounding mid-shift", true),
        ("the alarm stays quiet", false),
    ]
    .iter()
    .enumerate()
    {
        let emulator = Emulator::with_lambda_and_cloudwatch();
        let function = format!("gate-fn-{index}");
        let application = format!("gate-app-{index}");
        let alarm = format!("gate-alarm-{index}");
        function_with_versions(&emulator, &function, 3).await;
        emulator
            .cloudwatch_ok(
                "PutMetricAlarm",
                json!({
                    "AlarmName": alarm,
                    "Namespace": "Sim/CodeDeployShift",
                    "MetricName": "Errors",
                    "Statistic": "Sum",
                    "Period": 60,
                    "EvaluationPeriods": 1,
                    "Threshold": 1.0,
                    "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                    "TreatMissingData": "notBreaching",
                }),
            )
            .await;
        emulator
            .cloudwatch_ok(
                "SetAlarmState",
                json!({"AlarmName": alarm, "StateValue": "OK", "StateReason": "the control leg"}),
            )
            .await;
        let group = lambda_group(
            &emulator,
            &application,
            Some(traffic_routing("TimeBasedCanary", 10, 1)),
            &json!({
                "alarmConfiguration": {
                    "enabled": true,
                    "alarms": [{"name": alarm}],
                },
                "autoRollbackConfiguration": {
                    "enabled": true,
                    "events": ["DEPLOYMENT_STOP_ON_ALARM"],
                },
            }),
        )
        .await;

        // The leg that gives the group a revision to go back to, with the alarm
        // quiet: it moves the alias to version 2, in one step — this leg is the
        // setup, so it waits out no configured interval.
        let control = create_deployment_under(
            &emulator,
            &application,
            &group,
            &appspec_revision(&function, ALIAS, "1", "2"),
            Some(ALL_AT_ONCE),
        )
        .await;
        let control_info =
            terminal_deployment_within(&emulator, &control, case, WINDOW_DEADLINE).await;
        assert_eq!(
            control_info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the control leg must succeed, got {control_info:?}"
        );
        assert_eq!(
            alias_reading(&emulator, &function).await,
            ("2".to_string(), Vec::new()),
            "case {case:?}: the control leg must leave the alias on its target version"
        );

        // The leg under test: 2 -> 3, with the alarm driven to ALARM once the
        // canary increment is on the alias.
        let shifting = create_deployment(
            &emulator,
            &application,
            &group,
            &appspec_revision(&function, ALIAS, "2", "3"),
        )
        .await;
        let canary = ("2".to_string(), vec![("3".to_string(), 0.1)]);
        let deadline = Instant::now() + WINDOW_DEADLINE;
        loop {
            if alias_reading(&emulator, &function).await == canary {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the canary increment never reached the alias"
            );
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
        if *breaches {
            emulator
                .cloudwatch_ok(
                    "SetAlarmState",
                    json!({
                        "AlarmName": alarm,
                        "StateValue": "ALARM",
                        "StateReason": "the breach under test",
                    }),
                )
                .await;
        }

        let info =
            terminal_deployment_within(&emulator, &shifting, case, SHIFT_WINDOW_DEADLINE).await;
        if !*breaches {
            assert_eq!(
                info["status"].as_str(),
                Some("Succeeded"),
                "case {case:?}: a quiet alarm stops nothing, got {info:?}"
            );
            assert_eq!(
                alias_reading(&emulator, &function).await,
                ("3".to_string(), Vec::new()),
                "case {case:?}: the completed shift must leave the alias on version 3"
            );
            continue;
        }

        assert_eq!(
            (
                info["status"].as_str(),
                info["errorInformation"]["code"].as_str()
            ),
            (Some("Stopped"), Some(ALARM_ACTIVE)),
            "case {case:?}: a sounding alarm must stop the deployment, got {info:?}"
        );
        assert!(
            info["errorInformation"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains(&alarm),
            "case {case:?}: the stop must name the activated alarm, got {:?}",
            info["errorInformation"]["message"]
        );
        let rollback = info["rollbackInfo"]["rollbackDeploymentId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !rollback.is_empty(),
            "case {case:?}: a stop that rolls back names its rollback deployment, got {info:?}"
        );
        let rollback_info =
            terminal_deployment_within(&emulator, &rollback, case, WINDOW_DEADLINE).await;
        assert_eq!(
            (
                rollback_info["creator"].as_str(),
                rollback_info["rollbackInfo"]["rollbackTriggeringDeploymentId"].as_str()
            ),
            (Some(CREATOR_ROLLBACK), Some(shifting.as_str())),
            "case {case:?}: the rollback is CodeDeploy's own and points back at the stopped \
             deployment, got {rollback_info:?}"
        );

        // And the alias is back where the stopped deployment found it.
        let deadline = Instant::now() + WINDOW_DEADLINE;
        loop {
            let reading = alias_reading(&emulator, &function).await;
            if reading == ("2".to_string(), Vec::new()) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the rollback must restore the alias to version 2 with no \
                 weights, last {reading:?}"
            );
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    }
}

