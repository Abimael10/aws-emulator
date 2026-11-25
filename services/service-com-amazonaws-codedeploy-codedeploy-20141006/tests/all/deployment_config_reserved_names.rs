// SPDX-License-Identifier: CC0-1.0

//! The deployment-configuration names CodeDeploy keeps for itself, driven over
//! the wire.
//!
//! RULE under test: the `CodeDeployDefault.` prefix is CodeDeploy's own name
//! space, so `CreateDeploymentConfig` refuses ANY name beginning with it —
//! whether or not a configuration is shipped under that name — with HTTP 400
//! `InvalidDeploymentConfigNameException` carrying one fixed sentence about the
//! reserved prefix, and it does so BEFORE asking whether the name is taken, so a
//! shipped configuration's record is untouched and a reserved name nothing ships
//! under is still not a configuration afterwards. A name outside the prefix is an
//! ordinary name and is created.
//!
//! Every case goes through the same front door a client uses — the shared
//! [`Emulator`] harness of `codedeploy_lifecycle` — so the boundary validator,
//! this service's refusal wording and the awsJson1_1 envelope are exercised.

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{error_code, Emulator};
use crate::deployment_configs::PREDEFINED;

/// The one sentence the reserved name space is refused with: fixed text whose
/// only variable part is the reserved prefix itself.
const RESERVED_PREFIX_SENTENCE: &str =
    "The prefix CodeDeployDefault. is reserved for predefined configuration names";

/// The `$.message` of a failure body: the sentence the refusal carries.
fn message(case: &str, body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected a string $.message, got {body}"))
        .to_string()
}

/// The configuration body every case sends, so the rows differ only in the name:
/// an EC2/on-premises configuration with a healthy-host count, which is a body
/// `CreateDeploymentConfig` accepts under an ordinary name.
fn create_request(name: &str) -> Value {
    json!({
        "deploymentConfigName": name,
        "minimumHealthyHosts": {"type": "HOST_COUNT", "value": 1},
    })
}

/// RULE: every name under the reserved prefix is refused with
/// `InvalidDeploymentConfigNameException` and the one reserved-prefix sentence,
/// whatever follows the prefix.
#[tokio::test]
async fn a_name_under_the_reserved_prefix_is_refused() {
    let emulator = Emulator::new();

    // Rows differ along WHAT follows the reserved prefix: a configuration
    // CodeDeploy ships, a name nothing ships under, nothing at all, a name whose
    // suffix is punctuated, and one whose suffix is only digits. The shipped
    // catalog itself is walked below.
    let reserved = [
        "CodeDeployDefault.LambdaLinear10PercentEvery1Minute",
        "CodeDeployDefault.NotARealConfig",
        "CodeDeployDefault.",
        "CodeDeployDefault.one-at-a-time_v2",
        "CodeDeployDefault.99",
    ];

    for name in reserved {
        let (status, body) = emulator
            .call("CreateDeploymentConfig", create_request(name))
            .await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400 for a reserved name, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidDeploymentConfigNameException",
            "case {name:?}: expected InvalidDeploymentConfigNameException, got {body:?}"
        );
        let got = message(name, &body);
        assert_eq!(
            got, RESERVED_PREFIX_SENTENCE,
            "case {name:?}: expected {RESERVED_PREFIX_SENTENCE:?}, got {got:?}"
        );
    }
}

/// RULE: the refusal keys on the PREFIX, not on the shipped catalog — so every
/// configuration CodeDeploy ships is refused by the same sentence as a reserved
/// name nothing ships under, rather than as a name that is taken.
#[tokio::test]
async fn every_shipped_name_is_refused_for_the_prefix_not_for_being_taken() {
    let emulator = Emulator::new();

    // Rows: the whole shipped catalog, from the one list this target keeps of it.
    for name in PREDEFINED {
        let (status, body) = emulator
            .call("CreateDeploymentConfig", create_request(name))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidDeploymentConfigNameException"),
            "case {name:?}: expected 400 InvalidDeploymentConfigNameException, got {status} \
             {body:?}"
        );
        let got = message(name, &body);
        assert_eq!(
            got, RESERVED_PREFIX_SENTENCE,
            "case {name:?}: expected {RESERVED_PREFIX_SENTENCE:?}, got {got:?}"
        );
        // The sentence states the prefix and nothing else about the request: the
        // part of the name that follows the prefix never appears in it.
        let suffix = name.trim_start_matches("CodeDeployDefault.");
        assert!(
            !got.contains(suffix),
            "case {name:?}: the sentence must not name the requested configuration, got {got:?}"
        );
    }
}

/// RULE: the prefix rule is evaluated before anything asks whether the name is
/// taken, so a refused create leaves a shipped configuration's record exactly as
/// it was — and leaves a reserved name nothing ships under still unknown.
#[tokio::test]
async fn a_refused_create_leaves_the_name_space_as_it_was() {
    let emulator = Emulator::new();

    // Rows differ along whether a configuration is shipped under the name: one
    // the catalog holds (read before and after), and one it does not.
    let shipped = "CodeDeployDefault.LambdaLinear10PercentEvery1Minute";
    let before = emulator
        .ok(
            "GetDeploymentConfig",
            json!({"deploymentConfigName": shipped}),
        )
        .await;

    for name in [shipped, "CodeDeployDefault.NotARealConfig"] {
        let (status, body) = emulator
            .call("CreateDeploymentConfig", create_request(name))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidDeploymentConfigNameException"),
            "case {name:?}: expected 400 InvalidDeploymentConfigNameException, got {status} \
             {body:?}"
        );
    }

    let after = emulator
        .ok(
            "GetDeploymentConfig",
            json!({"deploymentConfigName": shipped}),
        )
        .await;
    assert_eq!(
        after, before,
        "case {shipped:?}: a refused create must leave the shipped record unchanged, got {after:?}"
    );
    assert_eq!(
        after["deploymentConfigInfo"]["minimumHealthyHosts"],
        Value::Null,
        "case {shipped:?}: the refused request's member must not have been stored, got {after:?}"
    );

    // The reserved name nothing ships under is still no configuration at all.
    let (status, body) = emulator
        .call(
            "GetDeploymentConfig",
            json!({"deploymentConfigName": "CodeDeployDefault.NotARealConfig"}),
        )
        .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "DeploymentConfigDoesNotExistException"),
        "case CodeDeployDefault.NotARealConfig: a refused create must store nothing, got {status} \
         {body:?}"
    );
    let listed = emulator.ok("ListDeploymentConfigs", json!({})).await;
    let names: Vec<&str> = listed["deploymentConfigsList"]
        .as_array()
        .map(|entries| entries.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        !names.contains(&"CodeDeployDefault.NotARealConfig"),
        "case CodeDeployDefault.NotARealConfig: a refused create must not be listed, got \
         {names:?}"
    );
}

/// RULE: only the prefix is reserved — a name that stops short of it, spells it
/// differently or merely carries it further along is an ordinary name, and the
/// very body a reserved name was refused with is accepted under it.
#[tokio::test]
async fn a_name_outside_the_reserved_prefix_is_created() {
    let emulator = Emulator::new();

    // Rows differ along HOW the name misses the prefix: it stops before the dot,
    // differs in case, carries the prefix after something else, or looks nothing
    // like it — the last being the positive control with the same request body as
    // the refused rows.
    let ordinary = [
        "CodeDeployDefault",
        "codedeploydefault.LambdaLinear10PercentEvery1Minute",
        "my-CodeDeployDefault.LambdaLinear10PercentEvery1Minute",
        "LambdaLinear10PercentEvery1Minute",
    ];

    for name in ordinary {
        let (status, body) = emulator
            .call("CreateDeploymentConfig", create_request(name))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: a name outside the reserved prefix must be created, got {status} \
             {body:?}"
        );
        assert!(
            body["deploymentConfigId"].as_str().is_some(),
            "case {name:?}: a created configuration answers its minted id, got {body:?}"
        );
        let read = emulator
            .ok("GetDeploymentConfig", json!({"deploymentConfigName": name}))
            .await;
        let info = &read["deploymentConfigInfo"];
        assert_eq!(
            (
                info["deploymentConfigName"].as_str(),
                info["minimumHealthyHosts"]["type"].as_str(),
                info["minimumHealthyHosts"]["value"].as_i64(),
            ),
            (Some(name), Some("HOST_COUNT"), Some(1)),
            "case {name:?}: expected the record the create stored, got {read:?}"
        );
    }
}
