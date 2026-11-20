// SPDX-License-Identifier: CC0-1.0

//! What a deployment's LIFECYCLE EVENT HOOKS do to it, driven over the wire.
//!
//! A Lambda deployment's revision declares, in the `Hooks` section of its AppSpec,
//! the Lambda validation function that runs for a lifecycle event — "the function
//! validates lifecycle hooks during a deployment that uses the Lambda or Amazon
//! ECS compute platform […] Lambda validation functions return Succeeded or
//! Failed"
//! (`smithy_query(query="doc",shape="PutLifecycleEventHookExecutionStatus")`). So
//! the RULE under test is that the result that function reports is what the
//! lifecycle event completed with: the deployment stays in flight until it
//! reports, a `Succeeded` result lets the deployment go on (and its traffic
//! shift happen), and a `Failed` result fails the deployment with the
//! `HOOK_EXECUTION_FAILURE` error information — which, for a group whose
//! `autoRollbackConfiguration` names `DEPLOYMENT_FAILURE`, is what starts the
//! rollback deployment.
//!
//! Nothing here synthesises a result: the function is created through AWS Lambda's
//! own front door, CodeDeploy invokes it, and the function itself calls
//! `PutLifecycleEventHookExecutionStatus` back. Both services are mounted on one
//! engine and driven through the one router a client reaches, so the invocation,
//! the report and the alias move all cross the same wire a client's would.

use std::io::Write;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{error_code, terminal_deployment_within, Emulator};

/// The service role every deployment group here is created with.
const SERVICE_ROLE: &str = "arn:aws:iam::123456789012:role/CodeDeployRole";

/// The collection a function is created in, and under which its versions and
/// aliases bind: `POST /2015-03-31/functions`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// How long a case here waits for a deployment whose validation function has to
/// run inside the window: one invocation of a Python handler, plus the span the
/// deployment itself takes.
const HOOKED_DEADLINE: Duration = Duration::from_secs(30);

/// How long the cases that read a target's whole lifecycle sequence wait for a
/// deployment to reach its terminal status.
///
/// None of those cases is about how FAST anything is — each waits for the
/// deployment's own outcome, which the clock owes it after its validation
/// function has run — so the budget is deliberately generous: a loaded host runs
/// a Python handler and this emulator's own clock no faster than it runs
/// everything else on it.
const SEQUENCE_DEADLINE: Duration = Duration::from_secs(180);

/// The alias every deployment of this suite moves.
const ALIAS: &str = "live";

/// A .zip deployment package holding `source` as the handler's module, written in
/// memory the way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The source of a validation function that reports `status` for the lifecycle
/// event it is invoked for, after sleeping `delay_seconds`.
///
/// It is the function AWS documents for a Lambda hook: it reads the deployment id
/// and the hook execution id out of the event CodeDeploy handed it and reports its
/// result through `PutLifecycleEventHookExecutionStatus`.
fn validation_function_source(status: &str, delay_seconds: f64) -> String {
    format!(
        "import time\n\
         \n\
         import boto3\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   time.sleep({delay_seconds})\n\
         \x20   boto3.client(\"codedeploy\").put_lifecycle_event_hook_execution_status(\n\
         \x20       deploymentId=event[\"DeploymentId\"],\n\
         \x20       lifecycleEventHookExecutionId=event[\"LifecycleEventHookExecutionId\"],\n\
         \x20       status=\"{status}\",\n\
         \x20   )\n\
         \x20   return {{\"reported\": \"{status}\"}}\n"
    )
}

/// Create the function `name` running `source`, and bring the activation Lambda's
/// own clock owes it forward so no case here waits for that window.
async fn given_function(emulator: &Emulator, case: &str, name: &str, source: &str) {
    emulator
        .lambda_ok(
            case,
            "POST",
            FUNCTIONS_URI,
            json!({
                "FunctionName": name,
                "Role": format!("arn:aws:iam::{}:role/handler", engine::emulator_account_id()),
                "Runtime": "python3.12",
                "Handler": "lambda_function.lambda_handler",
                "Timeout": 20,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
                },
            }),
            201,
        )
        .await;
    activate(emulator);
}

/// Bring every function's activation forward: the transition is owed by Lambda's
/// CLOCK, so nothing here sleeps on the emulator's behalf.
fn activate(emulator: &Emulator) {
    let state = emulator
        .engine
        .state::<service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = std::time::SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// Publish `versions` versions of `name`, so the versions an AppSpec may target
/// exist, and create the alias [`ALIAS`] invoking version 1.
async fn given_versions_and_alias(emulator: &Emulator, case: &str, name: &str, versions: usize) {
    for version in 1..=versions {
        emulator
            .lambda_ok(
                case,
                "POST",
                &format!("{FUNCTIONS_URI}/{name}/versions"),
                json!({"Description": format!("snapshot {version}")}),
                201,
            )
            .await;
    }
    emulator
        .lambda_ok(
            case,
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/aliases"),
            json!({"Name": ALIAS, "FunctionVersion": "1"}),
            201,
        )
        .await;
}

/// The version the alias of `function` invokes.
async fn alias_version(emulator: &Emulator, case: &str, function: &str) -> String {
    emulator
        .lambda_ok(
            case,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/aliases/{ALIAS}"),
            Value::Null,
            200,
        )
        .await["FunctionVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A Lambda application with one deployment group on it, carrying `members`.
async fn given_lambda_group(emulator: &Emulator, application: &str, members: Value) -> String {
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
        "deploymentConfigName": "CodeDeployDefault.LambdaAllAtOnce",
        "serviceRoleArn": SERVICE_ROLE,
    });
    if let Some(members) = members.as_object() {
        for (name, value) in members {
            request[name] = value.clone();
        }
    }
    emulator.ok("CreateDeploymentGroup", request).await;
    group
}

/// The AppSpec of a Lambda deployment shifting the alias from version 1, which is
/// where [`given_versions_and_alias`] leaves it; see [`app_spec_from`].
fn app_spec(function: &str, target: &str, hooks: &[(&str, &str)]) -> String {
    app_spec_from(function, "1", target, hooks)
}

/// The AppSpec of a Lambda deployment, as the AWS CodeDeploy User Guide writes
/// one: the `Resources` entry naming the function, the alias and the two versions
/// traffic moves between, plus one `Hooks` entry per lifecycle event the caller
/// declares a validation function for.
///
/// `current` is the version the deployment shifts FROM, which the SECOND
/// deployment of one alias states as the version the first one left it at.
fn app_spec_from(
    function: &str,
    current: &str,
    target: &str,
    hooks: &[(&str, &str)],
) -> String {
    let mut document = format!(
        "version: 0.0\n\
         Resources:\n\
         \x20 - {function}:\n\
         \x20     Type: AWS::Lambda::Function\n\
         \x20     Properties:\n\
         \x20       Name: {function}\n\
         \x20       Alias: {ALIAS}\n\
         \x20       CurrentVersion: \"{current}\"\n\
         \x20       TargetVersion: \"{target}\"\n"
    );
    if !hooks.is_empty() {
        document.push_str("Hooks:\n");
        for (event, hook_function) in hooks {
            document.push_str(&format!("  - {event}: \"{hook_function}\"\n"));
        }
    }
    document
}

/// How the KEYS of an AppSpec document are spelled; see [`app_spec_cased`].
#[derive(Debug, Clone, Copy)]
enum KeyCase {
    /// As the AWS CodeDeploy User Guide writes them.
    Reference,
    /// All in lower case.
    Lower,
    /// All in upper case.
    Upper,
}

impl KeyCase {
    /// `key` spelled in this case.
    fn spell(self, key: &str) -> String {
        match self {
            Self::Reference => key.to_string(),
            Self::Lower => key.to_ascii_lowercase(),
            Self::Upper => key.to_ascii_uppercase(),
        }
    }
}

/// [`app_spec_from`] with every structural KEY — and every lifecycle event name a
/// hook is declared under — spelled in `case`.
///
/// The document says exactly the same thing in each of them: the resource type
/// and the values it carries are untouched, only the keys are respelled.
fn app_spec_cased(
    case: KeyCase,
    function: &str,
    current: &str,
    target: &str,
    hooks: &[(&str, &str)],
) -> String {
    let key = |name: &str| case.spell(name);
    let mut document = format!(
        "{version}: 0.0\n\
         {resources}:\n\
         \x20 - {function}:\n\
         \x20     {kind}: AWS::Lambda::Function\n\
         \x20     {properties}:\n\
         \x20       {name}: {function}\n\
         \x20       {alias}: {ALIAS}\n\
         \x20       {current_version}: \"{current}\"\n\
         \x20       {target_version}: \"{target}\"\n",
        version = key("version"),
        resources = key("Resources"),
        kind = key("Type"),
        properties = key("Properties"),
        name = key("Name"),
        alias = key("Alias"),
        current_version = key("CurrentVersion"),
        target_version = key("TargetVersion"),
    );
    if !hooks.is_empty() {
        document.push_str(&format!("{}:\n", key("Hooks")));
        for (event, hook_function) in hooks {
            document.push_str(&format!("  - {}: \"{hook_function}\"\n", key(event)));
        }
    }
    document
}

/// Create a deployment of `content` as an `appSpecContent` revision, answering its
/// minted id.
async fn deploy(emulator: &Emulator, application: &str, group: &str, content: &str) -> String {
    emulator
        .ok(
            "CreateDeployment",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "revision": {
                    "revisionType": "AppSpecContent",
                    "appSpecContent": {"content": content},
                },
            }),
        )
        .await["deploymentId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every lifecycle event of `deployment_id`'s one target, as `(lifecycle event
/// name, status)` pairs in the order the target reports them, read through the
/// target readers a client uses.
///
/// A Lambda deployment's one target is reported under the function its revision
/// names, so the listing answers that id and the single-target reader answers the
/// `lambdaTarget` whose `lifecycleEvents` carry the deployment's progression
/// through the lifecycle events of its compute platform.
async fn lifecycle_events(
    emulator: &Emulator,
    case: &str,
    deployment_id: &str,
) -> Vec<(String, String)> {
    let listed = emulator
        .ok(
            "ListDeploymentTargets",
            json!({"deploymentId": deployment_id}),
        )
        .await;
    let target_id = listed["targetIds"][0]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the deployment to expose its target, got {listed:?}")
        })
        .to_string();
    let read = emulator
        .ok(
            "GetDeploymentTarget",
            json!({"deploymentId": deployment_id, "targetId": target_id}),
        )
        .await;
    read["deploymentTarget"]["lambdaTarget"]["lifecycleEvents"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| {
            (
                event["lifecycleEventName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                event["status"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The `status` the lifecycle event `event` of `deployment_id`'s one target
/// reads; `None` for a target that reports no such event. One reading of
/// [`lifecycle_events`], so both read a target the same way.
async fn lifecycle_event_status(
    emulator: &Emulator,
    case: &str,
    deployment_id: &str,
    event: &str,
) -> Option<String> {
    lifecycle_events(emulator, case, deployment_id)
        .await
        .into_iter()
        .find(|(name, _)| name == event)
        .map(|(_, status)| status)
}

/// One row of the hook table.
struct Hooked {
    /// What the row is about.
    case: &'static str,
    /// The lifecycle event the AppSpec declares a validation function for.
    lifecycle_event: &'static str,
    /// The result that function reports.
    reports: &'static str,
    /// How long the function takes before reporting, which is the window the
    /// deployment has to stay in flight for.
    delay_seconds: f64,
    /// The terminal status the deployment must reach.
    status: &'static str,
    /// The version the alias must invoke once it has.
    alias: &'static str,
}

/// RULE: a lifecycle event of a deployment completes with the result its Lambda
/// validation function reports — the deployment stays in flight until that report
/// arrives, a `Succeeded` result lets the deployment reach `Succeeded` with its
/// traffic shifted, and a `Failed` result fails it with `HOOK_EXECUTION_FAILURE`.
#[tokio::test]
async fn a_deployments_lifecycle_event_completes_with_what_its_validation_function_reports() {
    // Rows differ along which lifecycle event the hook is declared for (one
    // before traffic moves, one after), the result the function reports, and how
    // long it takes to report it.
    let cases: &[Hooked] = &[
        Hooked {
            case: "a BeforeAllowTraffic hook that succeeds",
            lifecycle_event: "BeforeAllowTraffic",
            reports: "Succeeded",
            delay_seconds: 0.0,
            status: "Succeeded",
            alias: "2",
        },
        Hooked {
            case: "a BeforeAllowTraffic hook that takes a while to succeed",
            lifecycle_event: "BeforeAllowTraffic",
            reports: "Succeeded",
            delay_seconds: 2.0,
            status: "Succeeded",
            alias: "2",
        },
        Hooked {
            // Traffic does not move: the hook that validates it reported a
            // failure, so the alias stays where it was.
            case: "a BeforeAllowTraffic hook that fails",
            lifecycle_event: "BeforeAllowTraffic",
            reports: "Failed",
            delay_seconds: 0.0,
            status: "Failed",
            alias: "1",
        },
        Hooked {
            case: "an AfterAllowTraffic hook that succeeds",
            lifecycle_event: "AfterAllowTraffic",
            reports: "Succeeded",
            delay_seconds: 0.0,
            status: "Succeeded",
            alias: "2",
        },
        Hooked {
            // This hook runs AFTER the shift, so the alias has already moved by
            // the time it fails the deployment.
            case: "an AfterAllowTraffic hook that fails",
            lifecycle_event: "AfterAllowTraffic",
            reports: "Failed",
            delay_seconds: 0.0,
            status: "Failed",
            alias: "2",
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let emulator = Emulator::with_lambda();
        let application = format!("hooked-{index:02}");
        let function = format!("handler{index:02}");
        let hook_function = format!("validator{index:02}");
        let group = given_lambda_group(&emulator, &application, json!({})).await;
        given_function(&emulator, case.case, &function, "def lambda_handler(e, c):\n    return e\n")
            .await;
        given_versions_and_alias(&emulator, case.case, &function, 2).await;
        given_function(
            &emulator,
            case.case,
            &hook_function,
            &validation_function_source(case.reports, case.delay_seconds),
        )
        .await;

        let content = app_spec(
            &function,
            "2",
            &[(case.lifecycle_event, hook_function.as_str())],
        );
        let deployment_id = deploy(&emulator, &application, &group, &content).await;

        // While the validation function has not reported, the deployment is in
        // flight — whatever the clock would otherwise have owed it — and its
        // deployment group takes no other deployment.
        if case.delay_seconds > 0.0 {
            let info = emulator
                .ok("GetDeployment", json!({"deploymentId": deployment_id}))
                .await["deploymentInfo"]
                .clone();
            assert!(
                matches!(info["status"].as_str(), Some("Created" | "InProgress")),
                "case {:?}: expected the deployment to be in flight while its hook runs, got \
                 {info:?}",
                case.case
            );
            let (status, refused) = emulator
                .call(
                    "CreateDeployment",
                    json!({
                        "applicationName": application,
                        "deploymentGroupName": group,
                        "revision": {
                            "revisionType": "AppSpecContent",
                            "appSpecContent": {"content": content},
                        },
                    }),
                )
                .await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, "DeploymentLimitExceededException"),
                "case {:?}: expected a second deployment to the busy group to be refused, got \
                 {status} {refused:?}",
                case.case
            );
            // The lifecycle event the function is validating is reported as
            // under way: it has been dispatched and no result has arrived. The
            // target reads only once CodeDeploy has dispatched the deployment,
            // which is what `DeploymentNotStartedException` refuses before.
            let deadline_at = Instant::now() + HOOKED_DEADLINE;
            loop {
                let polled = emulator
                    .ok("GetDeployment", json!({"deploymentId": deployment_id}))
                    .await["deploymentInfo"]["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if polled != "Created" {
                    break;
                }
                assert!(
                    Instant::now() < deadline_at,
                    "case {:?}: expected the deployment to be dispatched, got {polled}",
                    case.case
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let running =
                lifecycle_event_status(&emulator, case.case, &deployment_id, case.lifecycle_event)
                    .await;
            assert!(
                matches!(running.as_deref(), Some("Pending" | "InProgress")),
                "case {:?}: expected the {} lifecycle event to be under way, got {running:?}",
                case.case,
                case.lifecycle_event
            );
        }

        let info = terminal_deployment_within(&emulator, &deployment_id, case.case, HOOKED_DEADLINE)
            .await;
        assert_eq!(
            info["status"].as_str(),
            Some(case.status),
            "case {:?}: expected the deployment to end {}, got {info:?}",
            case.case,
            case.status
        );
        if case.status == "Failed" {
            assert_eq!(
                info["errorInformation"]["code"].as_str(),
                Some("HOOK_EXECUTION_FAILURE"),
                "case {:?}: expected the hook-execution-failure error code, got {info:?}",
                case.case
            );
            assert!(
                info["errorInformation"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(case.lifecycle_event)),
                "case {:?}: expected the message to name the lifecycle event, got {info:?}",
                case.case
            );
        } else {
            assert!(
                info["errorInformation"].is_null(),
                "case {:?}: expected no error information, got {info:?}",
                case.case
            );
        }

        let moved = alias_version(&emulator, case.case, &function).await;
        assert_eq!(
            moved, case.alias,
            "case {:?}: expected the alias to invoke version {}, got {moved}",
            case.case, case.alias
        );

        // The lifecycle event the function validated reads the result it
        // reported, on the target the deployment deployed to — which is the
        // function its revision names.
        let reported =
            lifecycle_event_status(&emulator, case.case, &deployment_id, case.lifecycle_event)
                .await;
        assert_eq!(
            reported.as_deref(),
            Some(case.reports),
            "case {:?}: expected the {} lifecycle event to read {}, got {reported:?}",
            case.case,
            case.lifecycle_event,
            case.reports
        );
    }
}

/// RULE: a validation function that reports NOTHING leaves its lifecycle event
/// with no result, which is not a result to deploy on: the deployment fails with
/// `HOOK_EXECUTION_FAILURE` rather than reporting a success nothing validated.
#[tokio::test]
async fn a_hook_that_reports_no_result_fails_the_deployment() {
    // Rows differ along why no result arrives: a function AWS Lambda does not
    // hold, and one that returns without reporting.
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("a hook function nothing created", "absent", None),
        (
            "a hook function that returns without reporting",
            "silent",
            Some("def lambda_handler(event, context):\n    return {\"reported\": None}\n"),
        ),
    ];

    for (case, hook_function, source) in cases {
        let emulator = Emulator::with_lambda();
        let application = "unreported";
        let group = given_lambda_group(&emulator, application, json!({})).await;
        given_function(
            &emulator,
            case,
            "handler",
            "def lambda_handler(e, c):\n    return e\n",
        )
        .await;
        given_versions_and_alias(&emulator, case, "handler", 2).await;
        if let Some(source) = source {
            given_function(&emulator, case, hook_function, source).await;
        }

        let content = app_spec("handler", "2", &[("BeforeAllowTraffic", hook_function)]);
        let deployment_id = deploy(&emulator, application, &group, &content).await;
        let info =
            terminal_deployment_within(&emulator, &deployment_id, case, HOOKED_DEADLINE).await;
        assert_eq!(
            (
                info["status"].as_str(),
                info["errorInformation"]["code"].as_str(),
            ),
            (Some("Failed"), Some("HOOK_EXECUTION_FAILURE")),
            "case {case:?}: expected the deployment to fail for its hook, got {info:?}"
        );
        let stayed = alias_version(&emulator, case, "handler").await;
        assert_eq!(
            stayed, "1",
            "case {case:?}: expected the alias left where it was, got {stayed}"
        );
        // The lifecycle event the function was validating failed with the
        // deployment: no result arrived for it, so it is not a succeeded event.
        let reported =
            lifecycle_event_status(&emulator, case, &deployment_id, "BeforeAllowTraffic").await;
        assert_eq!(
            reported.as_deref(),
            Some("Failed"),
            "case {case:?}: expected the BeforeAllowTraffic lifecycle event to read Failed, got \
             {reported:?}"
        );
    }
}

/// RULE: a deployment that FAILED is rolled back when the automatic rollback
/// configuration it runs under is enabled for `DEPLOYMENT_FAILURE` — CodeDeploy
/// creates a rollback deployment of the revision the group was on before it, which
/// puts the alias back and becomes the group's last successful deployment — and is
/// not rolled back when the configuration does not ask for that.
#[tokio::test]
async fn a_failed_deployment_is_rolled_back_when_its_configuration_asks_for_it() {
    // Rows differ along the automatic rollback configuration the group carries:
    // the event that asks for this rollback, another event that does not, a
    // disabled configuration listing the event, and no configuration at all.
    let cases: &[(&str, Value, bool)] = &[
        (
            "enabled for DEPLOYMENT_FAILURE",
            json!({"enabled": true, "events": ["DEPLOYMENT_FAILURE"]}),
            true,
        ),
        (
            "enabled for another event only",
            json!({"enabled": true, "events": ["DEPLOYMENT_STOP_ON_REQUEST"]}),
            false,
        ),
        (
            "disabled, though it lists the event",
            json!({"enabled": false, "events": ["DEPLOYMENT_FAILURE"]}),
            false,
        ),
        ("no configuration at all", Value::Null, false),
    ];

    for (index, (case, configuration, rolls_back)) in cases.iter().enumerate() {
        let emulator = Emulator::with_lambda();
        let application = format!("rollback-{index:02}");
        let members = if configuration.is_null() {
            json!({})
        } else {
            json!({"autoRollbackConfiguration": configuration})
        };
        let group = given_lambda_group(&emulator, &application, members).await;
        given_function(
            &emulator,
            case,
            "handler",
            "def lambda_handler(e, c):\n    return e\n",
        )
        .await;
        given_versions_and_alias(&emulator, case, "handler", 3).await;
        given_function(
            &emulator,
            case,
            "approve",
            &validation_function_source("Succeeded", 0.0),
        )
        .await;
        given_function(
            &emulator,
            case,
            "reject",
            &validation_function_source("Failed", 0.0),
        )
        .await;

        // A first deployment the group's hook approves, so the group HAS a
        // revision to roll back to.
        let first = app_spec("handler", "2", &[("BeforeAllowTraffic", "approve")]);
        let approved = deploy(&emulator, &application, &group, &first).await;
        let info =
            terminal_deployment_within(&emulator, &approved, case, HOOKED_DEADLINE).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: expected the first deployment to succeed, got {info:?}"
        );

        // Then one its hook rejects.
        let second = app_spec("handler", "3", &[("BeforeAllowTraffic", "reject")]);
        let rejected = deploy(&emulator, &application, &group, &second).await;
        let info =
            terminal_deployment_within(&emulator, &rejected, case, HOOKED_DEADLINE).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Failed"),
            "case {case:?}: expected the hook-rejected deployment to fail, got {info:?}"
        );

        let rollback_id = info["rollbackInfo"]["rollbackDeploymentId"]
            .as_str()
            .map(str::to_string);
        if !*rolls_back {
            assert!(
                rollback_id.is_none(),
                "case {case:?}: expected no rollback deployment, got {info:?}"
            );
            // The alias is wherever the failed deployment left it: before traffic.
            let stayed = alias_version(&emulator, case, "handler").await;
            assert_eq!(
                stayed, "2",
                "case {case:?}: expected the alias left on the approved version, got {stayed}"
            );
            continue;
        }
        let rollback_id = rollback_id.unwrap_or_else(|| {
            panic!("case {case:?}: expected a rollback deployment id, got {info:?}")
        });
        let rolled_back =
            terminal_deployment_within(&emulator, &rollback_id, case, HOOKED_DEADLINE).await;
        assert_eq!(
            (
                rolled_back["status"].as_str(),
                rolled_back["creator"].as_str(),
            ),
            (Some("Succeeded"), Some("codeDeployRollback")),
            "case {case:?}: expected a rollback deployment the service created, got {rolled_back:?}"
        );
        // The rollback redeploys the revision the group was on, so the alias is
        // back on the version that deployment put it at.
        let deadline_at = Instant::now() + HOOKED_DEADLINE;
        loop {
            let back = alias_version(&emulator, case, "handler").await;
            if back == "2" {
                break;
            }
            assert!(
                Instant::now() < deadline_at,
                "case {case:?}: expected the rollback to put the alias back on version 2, got \
                 {back}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // And it is the group's last successful deployment.
        let read = emulator
            .ok(
                "GetDeploymentGroup",
                json!({"applicationName": application, "deploymentGroupName": group}),
            )
            .await["deploymentGroupInfo"]
            .clone();
        assert_eq!(
            read["lastSuccessfulDeployment"]["deploymentId"].as_str(),
            Some(rollback_id.as_str()),
            "case {case:?}: expected the rollback to be the group's last successful deployment, \
             got {read:?}"
        );
    }
}

/// How often a case samples the alias of a deployment nothing is calling
/// CodeDeploy about: fine enough to catch the window a post-traffic hook holds
/// open, and the only thing any of these waits is for is a CONDITION.
const ALIAS_SAMPLE_INTERVAL: Duration = Duration::from_millis(25);

/// One row of the handover table: what the SECOND deployment's post-traffic hook
/// reports, and what that deployment then ends as.
struct Handover {
    /// What the row is about.
    case: &'static str,
    /// The result its `AfterAllowTraffic` validation function reports, or `None`
    /// for a deployment that declares no hook at all.
    reports: Option<&'static str>,
    /// The terminal status that deployment must reach.
    status: &'static str,
}

/// RULE: a deployment's traffic shift happens at its `AllowTraffic` lifecycle
/// event — before the hook that runs after the traffic is dispatched — so it does
/// not depend on the verdict that hook decides, and the traffic it moved STAYS
/// where it put it: what puts it back is the ROLLBACK deployment, whose own
/// traffic shift falls due like every other deployment's. So a client watching
/// only the alias sees it reach the version the revision targets whatever the
/// deployment goes on to end as, and still reads it there once that deployment has
/// failed, until its rollback runs.
#[tokio::test]
async fn a_failed_deployments_shift_stands_until_its_rollback_puts_the_alias_back() {
    // Rows differ along what the second deployment's post-traffic hook reports —
    // and whether it declares one at all — which is what decides the verdict the
    // shift must be independent of.
    let cases: &[Handover] = &[
        Handover {
            case: "a second deployment whose post-traffic hook approves",
            reports: Some("Succeeded"),
            status: "Succeeded",
        },
        Handover {
            case: "a second deployment whose post-traffic hook fails",
            reports: Some("Failed"),
            status: "Failed",
        },
        Handover {
            case: "a second deployment that declares no hook at all",
            reports: None,
            status: "Succeeded",
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let emulator = Emulator::with_lambda();
        let application = format!("handover-{index:02}");
        let function = format!("handover{index:02}");
        let group = given_lambda_group(
            &emulator,
            &application,
            json!({
                "autoRollbackConfiguration": {
                    "enabled": true,
                    "events": ["DEPLOYMENT_FAILURE"],
                },
            }),
        )
        .await;
        given_function(
            &emulator,
            case.case,
            &function,
            "def lambda_handler(e, c):\n    return e\n",
        )
        .await;
        given_versions_and_alias(&emulator, case.case, &function, 3).await;
        // The first revision's hook approves, but takes a while over it. That
        // matters twice: the first deployment waits for it, and so does the
        // ROLLBACK of the second one, which redeploys this same revision and runs
        // its lifecycle events — which is the window in which the alias still
        // reads what the failed deployment left.
        given_function(
            &emulator,
            case.case,
            "approve",
            &validation_function_source("Succeeded", 2.0),
        )
        .await;
        if let Some(reported) = case.reports {
            // The second deployment's hook takes a while to report, so the alias
            // that deployment has ALREADY moved is readable for the whole of the
            // window its validation holds open.
            given_function(
                &emulator,
                case.case,
                "second",
                &validation_function_source(reported, 1.0),
            )
            .await;
        }

        // The first leg, polled to its terminal status through CodeDeploy: the
        // call that observes it ending is the one the next deployment is handed
        // over by.
        let first = deploy(
            &emulator,
            &application,
            &group,
            &app_spec_from(&function, "1", "2", &[("BeforeAllowTraffic", "approve")]),
        )
        .await;
        let info = terminal_deployment_within(&emulator, &first, case.case, HOOKED_DEADLINE).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {:?}: the first leg must succeed, got {info:?}",
            case.case
        );
        let moved = alias_version(&emulator, case.case, &function).await;
        assert_eq!(
            moved, "2",
            "case {:?}: the first leg must leave the alias on version 2, got {moved}",
            case.case
        );

        // The leg under test. From its creation until the alias has moved NOTHING
        // calls CodeDeploy: the shift is the clock's to carry out.
        let hooks: Vec<(&str, &str)> = case
            .reports
            .map(|_| vec![("AfterAllowTraffic", "second")])
            .unwrap_or_default();
        let second = deploy(
            &emulator,
            &application,
            &group,
            &app_spec_from(&function, "2", "3", &hooks),
        )
        .await;
        let deadline_at = Instant::now() + HOOKED_DEADLINE;
        loop {
            let reading = alias_version(&emulator, case.case, &function).await;
            if reading == "3" {
                break;
            }
            assert!(
                Instant::now() < deadline_at,
                "case {:?}: expected the alias on version 3 with nothing but the clock running, \
                 got {reading}",
                case.case
            );
            tokio::time::sleep(ALIAS_SAMPLE_INTERVAL).await;
        }

        let info = terminal_deployment_within(&emulator, &second, case.case, HOOKED_DEADLINE).await;
        assert_eq!(
            info["status"].as_str(),
            Some(case.status),
            "case {:?}: expected the second leg to end {}, got {info:?}",
            case.case,
            case.status
        );
        if case.status != "Failed" {
            continue;
        }
        assert_eq!(
            info["errorInformation"]["code"].as_str(),
            Some("HOOK_EXECUTION_FAILURE"),
            "case {:?}: expected the hook-execution-failure error code, got {info:?}",
            case.case
        );
        let rollback_id = info["rollbackInfo"]["rollbackDeploymentId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !rollback_id.is_empty(),
            "case {:?}: expected the failure to name its rollback deployment, got {info:?}",
            case.case
        );
        // The deployment has FAILED and the traffic it shifted is still where it
        // put it: what puts it back is the rollback deployment, which is holding
        // at the lifecycle event hook of the revision it redeploys.
        let left = alias_version(&emulator, case.case, &function).await;
        assert_eq!(
            left, "3",
            "case {:?}: expected the failed deployment's shift to stand until its rollback runs, \
             got {left}",
            case.case
        );
        // The rollback is a deployment of its own, driven by the same clock: it
        // puts the alias back on the version the failed deployment found it at.
        let deadline_at = Instant::now() + HOOKED_DEADLINE;
        loop {
            let back = alias_version(&emulator, case.case, &function).await;
            if back == "2" {
                break;
            }
            assert!(
                Instant::now() < deadline_at,
                "case {:?}: expected the rollback to put the alias back on version 2, got {back}",
                case.case
            );
            tokio::time::sleep(ALIAS_SAMPLE_INTERVAL).await;
        }
    }
}

/// The lifecycle events a Lambda deployment RUNS, in the order it runs them: the
/// AWS CodeDeploy User Guide's lifecycle events for the Lambda compute platform
/// are BeforeAllowTraffic, AllowTraffic and AfterAllowTraffic, and the two around
/// the move are the ones a validation function may be declared for ("For Lambda
/// deployments, the available lifecycle hooks are BeforeAllowTraffic and
/// AfterAllowTraffic",
/// `smithy_query(query="doc",shape="PutLifecycleEventHookExecutionStatus")`).
const LAMBDA_SEQUENCE: [&str; 3] = ["BeforeAllowTraffic", "AllowTraffic", "AfterAllowTraffic"];

/// One row of the lifecycle-sequence table.
struct Sequenced {
    /// What the row is about.
    case: &'static str,
    /// The lifecycle event the revision's AppSpec declares a validation function
    /// for and the result that function reports, or `None` for a revision that
    /// declares no hook at all.
    hook: Option<(&'static str, &'static str)>,
    /// Whether AWS Lambda HOLDS that function, so a row can declare a hook whose
    /// function reports nothing because there is none to invoke.
    function_exists: bool,
    /// The terminal status the deployment reaches.
    status: &'static str,
    /// The `LifecycleEventStatus` of each step of [`LAMBDA_SEQUENCE`], in that
    /// order.
    events: [&'static str; 3],
}

/// RULE: a deployment target reports ONE lifecycle event per step of the
/// lifecycle sequence its compute platform runs, in the order that platform runs
/// them, each carrying the `LifecycleEventStatus` that step reached — the result
/// its validation function reported where the revision declared one, and
/// otherwise how far the deployment itself took the step — so a Lambda target
/// always reports BeforeAllowTraffic, AllowTraffic and AfterAllowTraffic, whether
/// or not the revision declared a hook for either of them.
#[tokio::test]
async fn a_lambda_targets_lifecycle_events_are_the_whole_sequence_its_deployment_runs() {
    // Rows differ along which step of the sequence carries a validation function,
    // what that function reports — and whether it reports at all — and so along
    // which step the deployment got to.
    let cases: &[Sequenced] = &[
        Sequenced {
            case: "a revision that declares no hook at all",
            hook: None,
            function_exists: false,
            status: "Succeeded",
            // Nothing validated the two hook events, but the deployment ran
            // them: it moved the traffic and reached its outcome.
            events: ["Succeeded", "Succeeded", "Succeeded"],
        },
        Sequenced {
            case: "a pre-traffic hook that approves",
            hook: Some(("BeforeAllowTraffic", "Succeeded")),
            function_exists: true,
            status: "Succeeded",
            events: ["Succeeded", "Succeeded", "Succeeded"],
        },
        Sequenced {
            case: "a pre-traffic hook that fails",
            hook: Some(("BeforeAllowTraffic", "Failed")),
            function_exists: true,
            status: "Failed",
            // The step that failed is the one its function vetoed; the move it
            // gated, and the step after that move, never ran.
            events: ["Failed", "Skipped", "Skipped"],
        },
        Sequenced {
            case: "a post-traffic hook that fails",
            hook: Some(("AfterAllowTraffic", "Failed")),
            function_exists: true,
            status: "Failed",
            // This hook runs after the move, so everything before it completed.
            events: ["Succeeded", "Succeeded", "Failed"],
        },
        Sequenced {
            case: "a pre-traffic hook whose function AWS Lambda does not hold",
            hook: Some(("BeforeAllowTraffic", "Succeeded")),
            function_exists: false,
            status: "Failed",
            // No result arrived for the step, which is not a result to deploy
            // on: the step failed and the rest never ran.
            events: ["Failed", "Skipped", "Skipped"],
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let emulator = Emulator::with_lambda();
        let application = format!("sequenced-{index:02}");
        let function = format!("sequenced{index:02}");
        let hook_function = format!("validator{index:02}");
        let group = given_lambda_group(&emulator, &application, json!({})).await;
        given_function(
            &emulator,
            case.case,
            &function,
            "def lambda_handler(e, c):\n    return e\n",
        )
        .await;
        given_versions_and_alias(&emulator, case.case, &function, 2).await;
        let hooks: Vec<(&str, &str)> = case
            .hook
            .map(|(event, _)| vec![(event, hook_function.as_str())])
            .unwrap_or_default();
        // The function that reports the row's result, created through AWS
        // Lambda's own front door — except on the row that declares a hook AWS
        // Lambda holds no function for, where nothing reports for the step.
        if let Some((_, reports)) = case.hook.filter(|_| case.function_exists) {
            given_function(
                &emulator,
                case.case,
                &hook_function,
                &validation_function_source(reports, 0.0),
            )
            .await;
        }

        let content = app_spec(&function, "2", &hooks);
        let deployment_id = deploy(&emulator, &application, &group, &content).await;
        let info =
            terminal_deployment_within(&emulator, &deployment_id, case.case, SEQUENCE_DEADLINE)
                .await;
        assert_eq!(
            info["status"].as_str(),
            Some(case.status),
            "case {:?}: expected the deployment to end {}, got {info:?}",
            case.case,
            case.status
        );

        let expected: Vec<(String, String)> = LAMBDA_SEQUENCE
            .iter()
            .zip(case.events)
            .map(|(name, status)| ((*name).to_string(), status.to_string()))
            .collect();
        let reported = lifecycle_events(&emulator, case.case, &deployment_id).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected the Lambda lifecycle sequence {expected:?}, got {reported:?}",
            case.case
        );
    }
}

/// Every case an AppSpec's keys are spelled in here, with what the row calls it:
/// the reference's own spelling, and the two whole-document respellings a client
/// can write the same declaration in.
const KEY_CASES: [(&str, KeyCase); 3] = [
    ("the reference's own case", KeyCase::Reference),
    ("lower-case keys", KeyCase::Lower),
    ("upper-case keys", KeyCase::Upper),
];

/// Deploy one revision whose AppSpec is written with its keys in `key_case` and
/// whose BeforeAllowTraffic validation function reports `reports`, and hold the
/// deployment to the rule: the declaration is read the same way whichever case
/// its keys are in, so the deployment reports its one target under the function
/// the declaration names, its lifecycle sequence reads `events`, and the alias
/// ends on version `alias`.
///
/// `slot` keeps one row's resources apart from another's.
async fn a_cased_deployment_is_read_the_same_way(
    spelling: &str,
    key_case: KeyCase,
    slot: &str,
    reports: &str,
    events: [&str; 3],
    alias: &str,
) {
    let case = format!("{spelling} with a hook that reports {reports}");
    let emulator = Emulator::with_lambda();
    let application = format!("cased-{slot}");
    let function = format!("cased{slot}");
    let hook_function = format!("casedhook{slot}");
    let group = given_lambda_group(&emulator, &application, json!({})).await;
    given_function(
        &emulator,
        &case,
        &function,
        "def lambda_handler(e, c):\n    return e\n",
    )
    .await;
    given_versions_and_alias(&emulator, &case, &function, 2).await;
    given_function(
        &emulator,
        &case,
        &hook_function,
        &validation_function_source(reports, 0.0),
    )
    .await;

    let content = app_spec_cased(
        key_case,
        &function,
        "1",
        "2",
        &[("BeforeAllowTraffic", hook_function.as_str())],
    );
    let deployment_id = deploy(&emulator, &application, &group, &content).await;
    let info =
        terminal_deployment_within(&emulator, &deployment_id, &case, SEQUENCE_DEADLINE).await;
    let expected_status = if reports == "Failed" {
        "Failed"
    } else {
        "Succeeded"
    };
    assert_eq!(
        info["status"].as_str(),
        Some(expected_status),
        "case {case:?}: expected the deployment to end on its hook's verdict \
         ({expected_status}), got {info:?}"
    );

    // The one target the declaration names, listed and read under the function it
    // deploys to.
    let listed = emulator
        .ok(
            "ListDeploymentTargets",
            json!({"deploymentId": deployment_id}),
        )
        .await;
    assert_eq!(
        listed["targetIds"].as_array().map(Vec::len),
        Some(1),
        "case {case:?}: expected exactly the declared function as the target, got {listed:?}"
    );
    assert_eq!(
        listed["targetIds"][0].as_str(),
        Some(function.as_str()),
        "case {case:?}: expected the target to be reported under the function the declaration \
         names, got {listed:?}"
    );

    let reported = lifecycle_events(&emulator, &case, &deployment_id).await;
    let expected: Vec<(String, String)> = LAMBDA_SEQUENCE
        .iter()
        .zip(events)
        .map(|(name, status)| ((*name).to_string(), status.to_string()))
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the lifecycle sequence {expected:?}, got {reported:?}"
    );

    let moved = alias_version(&emulator, &case, &function).await;
    assert_eq!(
        moved, alias,
        "case {case:?}: expected the alias to invoke version {alias}, got {moved}"
    );
}

/// RULE: an AppSpec declares the same deployment whichever case its keys are
/// written in — CodeDeploy reads the document for the sections and properties it
/// HAS, not for one spelling of them — so the function, alias and versions its
/// Lambda declaration names, and the validation function its hooks section
/// declares, are read the same way: the deployment reports its one target under
/// that function, runs that hook, and ends on the hook's verdict with the alias
/// where that verdict leaves it.
///
/// Here with a hook that APPROVES, so the declaration's own traffic move happens.
#[tokio::test]
async fn an_app_spec_whose_keys_are_in_any_case_deploys_what_it_declares() {
    for (index, (spelling, key_case)) in KEY_CASES.iter().enumerate() {
        a_cased_deployment_is_read_the_same_way(
            spelling,
            *key_case,
            &format!("ok{index:02}"),
            "Succeeded",
            // Every step of the sequence ran: the hook approved, the move
            // happened, and the deployment reached its outcome.
            ["Succeeded", "Succeeded", "Succeeded"],
            // The approved deployment moves the alias to the version its
            // declaration targets.
            "2",
        )
        .await;
    }
}

/// The same rule, with a hook that FAILS: the declaration is read the same way,
/// so the hook it names runs and its veto is what the deployment and the alias
/// come to.
#[tokio::test]
async fn an_app_spec_whose_keys_are_in_any_case_runs_the_hook_it_declares() {
    for (index, (spelling, key_case)) in KEY_CASES.iter().enumerate() {
        a_cased_deployment_is_read_the_same_way(
            spelling,
            *key_case,
            &format!("no{index:02}"),
            "Failed",
            // The step that failed is the one the function vetoed; the move it
            // gated, and the step after that move, never ran.
            ["Failed", "Skipped", "Skipped"],
            // So the alias stays where it was.
            "1",
        )
        .await;
    }
}
