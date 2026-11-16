// SPDX-License-Identifier: CC0-1.0

//! A gate AppConfig consults before starting a deployment REFUSES that deployment in
//! AppConfig's own words when its target does not answer.
//!
//! The rule under test: an action point the service consults before performing a call gates
//! it, so an action whose target failed refuses the call with `BadRequestException` / HTTP
//! 400 (`smithy_query(query="operation",operation="StartDeployment")`), with a message
//! naming the extension whose action failed, what the invocation reported, and — when the
//! failure came from the target's own run — the id of the invocation that reported it:
//! "Error invoking extension <extension name>: <what was reported>, Lambda Request ID:
//! <invocation's request id>". Nothing is persisted for a refused call: no deployment
//! record and no deployment number.
//!
//! Only a Lambda function can fail an invocation with an answer of its own, so the gate of
//! every row below is a function this emulator runs, and the rows differ along what the
//! target does: return, raise, or not exist at all. The request id is the FAILING
//! invocation's own, so two refusals of the same gate never name the same one — which is
//! asserted rather than the recorded id, which belongs to one run of one invocation.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile, json_call,
};
use crate::configuration_profile_validators::emulator;
use crate::deployment_tick_directive::given_function;
use crate::extension_action_points::{
    associate, create_version, given_extension, given_strategy, start_deployment,
};
use crate::resource_tags::arn;

/// The Region every request below is signed for, which is also the Region of the function
/// an action's `Uri` names.
const REGION: &str = "us-east-1";

/// The fixed text between what the invocation reported and the id of the invocation that
/// reported it, in the refusal AppConfig states for a failed extension action.
const REQUEST_ID_MARKER: &str = ", Lambda Request ID: ";

/// What a run whose handler raised reports as its `FunctionError`, which is what AppConfig
/// relays as the reason the action failed.
const UNHANDLED: &str = "Unhandled error";

/// The deployments one environment holds, as `ListDeployments` reports them.
async fn deployment_numbers(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
) -> Vec<i64> {
    let listed = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &Value::Null,
    )
    .await;
    listed.json()["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["DeploymentNumber"].as_i64())
        .collect()
}

/// A gate bound to its own environment: each row gets an environment of its own, so the
/// association under test is the only one the deployment's workflow resolves. Answers the
/// environment's id and the extension's name.
async fn given_gated_environment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    label: &str,
    uri: &str,
) -> (String, String) {
    let environment = create_environment(router, application, &format!("{label}-env")).await;
    let (extension, extension_name) = given_extension(
        router,
        &format!("{label}-gate"),
        &["PRE_START_DEPLOYMENT"],
        uri,
        &[],
    )
    .await;
    associate(
        router,
        case,
        &extension,
        &arn(&format!(
            "application/{application}/environment/{environment}"
        )),
        &Value::Null,
    )
    .await;
    (environment, extension_name)
}

/// RULE: a target that answers lets the deployment be created; a target that fails refuses
/// it with the service's own message and leaves no record. The rows differ along what the
/// gate's target does.
#[tokio::test]
async fn a_gate_whose_target_fails_refuses_the_deployment_in_appconfigs_own_words() {
    let (router, engine) = emulator();
    let application = create_application(&router, "gate-app").await;
    let profile = create_profile(&router, &application, "gate-profile").await;
    let seeded = create_version(
        &router,
        &application,
        &profile,
        "application/json",
        br#"{"v":1}"#,
    )
    .await;
    assert_eq!(
        seeded.status, 201,
        "seeding a version answered {}",
        seeded.status
    );
    // A strategy that paces no stage, so a row's deployment is over when its start answers
    // and no row waits on another's rollout.
    let strategy = given_strategy(&router, "gate-instant", 0, 0).await;

    let answering = given_function(
        &router,
        &engine,
        "a target that returns",
        "gate-returns",
        "def lambda_handler(event, context):\n\x20   return None\n",
    )
    .await;
    let raising = given_function(
        &router,
        &engine,
        "a target that raises",
        "gate-raises",
        "def lambda_handler(event, context):\n\
         \x20   raise RuntimeError(\"this release is refused\")\n",
    )
    .await;
    // A function nothing created: Lambda itself refuses the invocation, so there is no run
    // of the target's own and no request id of one to name.
    let missing = format!(
        "arn:aws:lambda:{REGION}:{}:function:nobody-created-me",
        engine::emulator_account_id()
    );

    // (case, the gate's target, the label its resources carry, whether the deployment is
    // created, whether the refusal names the failing invocation's request id).
    let rows: [(&str, &str, &str, bool, bool); 3] = [
        ("a target that returns", answering.as_str(), "answering", true, false),
        ("a target that raises", raising.as_str(), "raising", false, true),
        ("a target nothing created", missing.as_str(), "missing", false, false),
    ];
    for (case, uri, label, created, names_an_invocation) in rows {
        let (environment, extension_name) =
            given_gated_environment(&router, case, &application, label, uri).await;
        let body = json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": "1",
        });
        let answer = start_deployment(&router, &application, &environment, &body).await;
        if created {
            assert_eq!(
                answer.status,
                201,
                "case {case:?}: expected 201, got {} / {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            // Read back through the implemented reader: the record the gate let through.
            let numbers = deployment_numbers(&router, &application, &environment).await;
            assert_eq!(
                numbers,
                vec![1],
                "case {case:?}: expected the deployment the gate let through, got {numbers:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status,
            400,
            "case {case:?}: expected the modelled 400 refusal, got {} / {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code().split('#').next_back(),
            Some("BadRequestException"),
            "case {case:?}: expected BadRequestException, got {}",
            answer.error_code()
        );
        let message = answer.string("Message").unwrap_or_default();
        let prefix = format!("Error invoking extension {extension_name}: ");
        assert!(
            message.starts_with(&prefix),
            "case {case:?}: expected a message opening {prefix:?}, got {message:?}"
        );
        if names_an_invocation {
            let expected = format!("{prefix}{UNHANDLED}{REQUEST_ID_MARKER}");
            assert!(
                message.starts_with(&expected),
                "case {case:?}: expected a message opening {expected:?}, got {message:?}"
            );
            let first = message[expected.len()..].to_string();
            assert!(
                !first.is_empty(),
                "case {case:?}: the refusal must name the invocation that failed, got \
                 {message:?}"
            );
            // The id belongs to the invocation that failed, so the next refusal of the same
            // gate names a different one.
            let again = start_deployment(&router, &application, &environment, &body).await;
            let repeated = again.string("Message").unwrap_or_default();
            assert!(
                repeated.starts_with(&expected),
                "case {case:?}: expected the same message template, got {repeated:?}"
            );
            let second = repeated[expected.len()..].to_string();
            assert_ne!(
                first, second,
                "case {case:?}: each refusal names its own invocation, got {first:?} twice"
            );
        } else {
            assert!(
                !message.contains(REQUEST_ID_MARKER),
                "case {case:?}: an invocation that never ran has no request id to name, got \
                 {message:?}"
            );
        }

        // A refused call persists nothing: no record, and the environment's next deployment
        // number is untouched, which the next successful start would show.
        let numbers = deployment_numbers(&router, &application, &environment).await;
        assert!(
            numbers.is_empty(),
            "case {case:?}: a refused deployment leaves no record, got {numbers:?}"
        );
    }
}
