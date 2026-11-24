// SPDX-License-Identifier: CC0-1.0

//! The DEFAULTS a usage profile's job configuration may set for a job's capacity.
//!
//! RULE: a job's capacity is the kind of worker and the number of workers together ("you
//! should specify a `Worker type` and the `Number of workers`",
//! `smithy_query(query="doc",shape="CreateJobRequest",member="MaxCapacity")`), so a usage
//! profile whose `JobConfiguration` sets a `DefaultValue` for exactly one of those two
//! parameters configures half a capacity: the write — create or update — is refused 400
//! `InvalidInputException` with the sentence the service states that rule in, and the stored
//! profile is left exactly as it was. A configuration that defaults both, or neither, is
//! stored.
//!
//! The rows differ along what the rule generalises over: which half of the pair carries the
//! default, whether the other half is absent or present-but-undefaulted, whether other
//! parameters are configured beside them, which section they arrive under, and which write
//! carries them.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::usage_profiles::assert_configuration;
use crate::wire::{call, emulator, error_code, error_message, ok};

/// The sentence the service refuses a half-defaulted capacity pair with. Written out here
/// independently of the service, so this suite pins the wording and not the code: fixed text
/// that names no value of the request.
const HALF_A_PAIR: &str = "Please set default value for both Worker type and Number of \
                           workers in jobs configuration, if you intend to set default for \
                           either parameter.";

/// The `Configuration` a profile is created with before an update is made against it: both
/// halves of the pair defaulted, so the stored profile satisfies the rule.
fn stored_configuration() -> Value {
    json!({
        "JobConfiguration": {
            "workerType": { "DefaultValue": "G.1X" },
            "numberOfWorkers": { "DefaultValue": "5" },
        },
    })
}

/// Register one usage profile holding [`stored_configuration`].
async fn given_profile(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateUsageProfile",
        &json!({ "Name": name, "Configuration": stored_configuration() }),
    )
    .await;
}

/// The `Configuration` the profile `name` holds.
async fn held_configuration(router: &MultiServiceRouter, name: &str) -> Value {
    let read = ok(router, "GetUsageProfile", &json!({ "Name": name })).await;
    read.get("Configuration").cloned().unwrap_or(Value::Null)
}

/// The configurations that default exactly one half of the pair, each one differently.
fn half_a_pair() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a worker type default with no worker count at all",
            json!({ "JobConfiguration": { "workerType": { "DefaultValue": "G.2X" } } }),
        ),
        (
            "a worker count default with no worker type at all",
            json!({ "JobConfiguration": { "numberOfWorkers": { "DefaultValue": "4" } } }),
        ),
        (
            "a worker type default beside a worker count that is only bounded",
            json!({
                "JobConfiguration": {
                    "workerType": { "DefaultValue": "G.2X" },
                    "numberOfWorkers": { "MinValue": "1", "MaxValue": "10" },
                },
            }),
        ),
        (
            "a worker count default beside a worker type that only enumerates its values",
            json!({
                "JobConfiguration": {
                    "numberOfWorkers": { "DefaultValue": "4", "MinValue": "1", "MaxValue": "10" },
                    "workerType": { "AllowedValues": ["G.1X", "G.2X"] },
                },
            }),
        ),
        (
            "a worker count default beside other parameters and a session section",
            json!({
                "JobConfiguration": {
                    "numberOfWorkers": { "DefaultValue": "2" },
                    "maxCapacity": { "DefaultValue": "4", "MinValue": "2", "MaxValue": "8" },
                },
                "SessionConfiguration": { "idleTimeout": { "DefaultValue": "30" } },
            }),
        ),
    ]
}

/// The configurations that default both halves of the pair, or neither.
fn whole_or_nothing() -> Vec<(&'static str, Value)> {
    vec![
        (
            "both halves defaulted",
            json!({
                "JobConfiguration": {
                    "workerType": { "DefaultValue": "G.2X" },
                    "numberOfWorkers": { "DefaultValue": "4" },
                },
            }),
        ),
        (
            "both halves defaulted, both bounded, beside another parameter",
            json!({
                "JobConfiguration": {
                    "workerType": {
                        "DefaultValue": "G.2X",
                        "AllowedValues": ["G.1X", "G.2X"],
                    },
                    "numberOfWorkers": { "DefaultValue": "4", "MinValue": "1", "MaxValue": "10" },
                    "timeout": { "DefaultValue": "60" },
                },
            }),
        ),
        (
            "neither half defaulted, both merely bounded",
            json!({
                "JobConfiguration": {
                    "workerType": { "AllowedValues": ["G.1X", "G.2X"] },
                    "numberOfWorkers": { "MinValue": "1", "MaxValue": "10" },
                },
            }),
        ),
        (
            "neither half configured at all",
            json!({ "JobConfiguration": { "maxCapacity": { "DefaultValue": "4" } } }),
        ),
        (
            // The sentence states the rule over the JOB configuration, so the same half-pair
            // in the session section is no defect.
            "half the pair, in the session section",
            json!({ "SessionConfiguration": { "numberOfWorkers": { "DefaultValue": "2" } } }),
        ),
        (
            "half the pair in the session section, the whole pair in the job one",
            json!({
                "JobConfiguration": {
                    "workerType": { "DefaultValue": "G.1X" },
                    "numberOfWorkers": { "DefaultValue": "2" },
                },
                "SessionConfiguration": { "workerType": { "DefaultValue": "G.1X" } },
            }),
        ),
        ("no configuration map at all", json!({})),
    ]
}

/// A create whose job configuration defaults exactly one half of the capacity pair is refused
/// with the service's own sentence, and stores nothing.
#[tokio::test]
async fn a_create_defaulting_half_a_capacity_pair_is_refused() {
    let router = emulator();

    for (index, (case, configuration)) in half_a_pair().iter().enumerate() {
        let name = format!("half-create-{index}");
        let (status, body) = call(
            &router,
            "CreateUsageProfile",
            &json!({ "Name": &name, "Configuration": configuration }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: half a capacity pair must be refused with 400, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).contains("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            HALF_A_PAIR,
            "case {case:?}: unexpected wording for {configuration}"
        );

        // A refused create left the name free, so the read misses.
        let (status, body) = call(&router, "GetUsageProfile", &json!({ "Name": &name })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create must store no profile, got {status} with {body}"
        );
    }
}

/// The same rule on the UPDATE: the write is refused with the same sentence and the stored
/// profile keeps the configuration it had.
#[tokio::test]
async fn an_update_defaulting_half_a_capacity_pair_is_refused_and_writes_nothing() {
    let router = emulator();
    given_profile(&router, "held-profile").await;
    let before = held_configuration(&router, "held-profile").await;

    for (case, configuration) in half_a_pair() {
        let (status, body) = call(
            &router,
            "UpdateUsageProfile",
            &json!({ "Name": "held-profile", "Configuration": configuration }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: half a capacity pair must be refused with 400, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).contains("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            HALF_A_PAIR,
            "case {case:?}: unexpected wording for {configuration}"
        );

        let after = held_configuration(&router, "held-profile").await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must write nothing, expected {before}, got {after}"
        );
    }
}

/// A configuration that defaults both halves of the pair, or neither, is stored by the create
/// and read back — which is what makes the refusals above refusals of the half pair and not of
/// something else about those requests.
#[tokio::test]
async fn a_configuration_defaulting_both_halves_or_neither_is_stored() {
    let router = emulator();

    for (index, (case, configuration)) in whole_or_nothing().iter().enumerate() {
        let name = format!("whole-create-{index}");
        ok(
            &router,
            "CreateUsageProfile",
            &json!({ "Name": &name, "Configuration": configuration }),
        )
        .await;
        assert_configuration(
            case,
            configuration,
            &held_configuration(&router, &name).await,
        );
    }
}

/// And the same on the UPDATE: such a configuration replaces the stored one, so a later read
/// reports it.
#[tokio::test]
async fn a_configuration_defaulting_both_halves_or_neither_replaces_the_stored_one() {
    let router = emulator();

    for (index, (case, configuration)) in whole_or_nothing().iter().enumerate() {
        let name = format!("whole-update-{index}");
        given_profile(&router, &name).await;
        ok(
            &router,
            "UpdateUsageProfile",
            &json!({ "Name": &name, "Configuration": configuration }),
        )
        .await;
        assert_configuration(
            case,
            configuration,
            &held_configuration(&router, &name).await,
        );
    }
}
