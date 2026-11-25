// SPDX-License-Identifier: CC0-1.0

//! Which `stopType` `StopServiceDeployment` CARRIES OUT, and how it refuses the ones it does
//! not — driven end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test:
//!
//! > `StopServiceDeployment` carries out exactly the stop type its own member documents as
//! > valid. A value outside the modelled value set is a bad parameter: HTTP 400
//! > `InvalidParameterException` with the fixed sentence AWS publishes for that member. A
//! > value INSIDE that set which the API does not carry out is not a bad parameter and not a
//! > conflict with the rollout's state either — it is a feature the API does not offer, so it
//! > is refused HTTP 400 `UnsupportedFeatureException`, before the rollout the request names
//! > is resolved and whatever state that rollout is in. Neither refusal changes anything.
//!
//! The docs the rule is read off:
//!
//! * `StopServiceDeployment$stopType` -> `StopServiceDeploymentStopType`
//!   (`smithy_query(query="shape", shape="StopServiceDeploymentStopType")`: ABORT, ROLLBACK),
//!   documented as "How you want Amazon ECS to stop the service. The valid values are
//!   ROLLBACK", and the operation's own "The following stop types are avaiable: ROLLBACK -
//!   This option rolls back the service deployment to the previous service revision."
//! * `StopServiceDeployment`'s modelled errors
//!   (`smithy_query(query="operation", operation="StopServiceDeployment")`), which include
//!   `InvalidParameterException`, `UnsupportedFeatureException` and `ConflictException`, all
//!   three at HTTP 400.
//! * `UnsupportedFeatureException`: "The specified task isn't supported in this Region."
//! * `InvalidParameterException`: "The specified parameter isn't valid. Review the available
//!   parameters for the API request."
//!
//! The rows differ along what the rule generalises over: the stop type's MEMBERSHIP of the
//! modelled set (in it and carried out, in it and unsupported, outside it), and — for the
//! values outside the set — the FORM of the value, since a value is checked against the set
//! exactly: another spelling of a value in the set, a stop type of some other API, an empty
//! string, and a value with the padding a careless client leaves on.
//!
//! Nothing here reaches into the state struct: the rollout each row refuses a stop of is
//! created through `CreateService`, and what the refusals left of it is read back through
//! `DescribeServiceDeployments`.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, describe_service_deployment, ok, refusal, register_definition, service,
    string,
};

/// The cluster the services below run in.
const CLUSTER: &str = "stoptypes";

/// `InvalidParameterException$message` for a `stopType` outside the modelled value set:
/// entirely fixed text, which names neither the value the request carried nor the stop types
/// the member takes.
const INVALID_STOP_TYPE: &str = "The StopType is invalid.";

/// `UnsupportedFeatureException$message` for the modelled stop type this API does not carry
/// out: the fixed sentence names the API and the stop type asked for.
const UNSUPPORTED_ABORT: &str =
    "ECS StopServiceDeployment API with ABORT stopType is not supported.";

/// One row of the stop-type table: the value the request names, and the refusal it earns.
struct StopTypeCase {
    case: &'static str,
    stop_type: &'static str,
    /// The modelled error the refusal carries, and its message.
    expected: (&'static str, &'static str),
}

/// Create a service whose rollout the ECS scheduler owns, and return the ARN of the service
/// deployment that rollout is recorded as.
async fn create_service(
    router: &MultiServiceRouter,
    name: &str,
    definition: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": definition,
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    string(&body["service"], "currentServiceDeployment", case)
}

/// RULE: every stop type this API does not carry out is refused, each in the class its own
/// condition states, and the rollout the request named is left exactly as it was.
#[tokio::test]
async fn a_stop_type_this_api_does_not_carry_out_is_refused() {
    let cases = [
        StopTypeCase {
            case: "the modelled stop type this API does not support",
            stop_type: "ABORT",
            expected: ("UnsupportedFeatureException", UNSUPPORTED_ABORT),
        },
        StopTypeCase {
            case: "the lower-case spelling of a value in the set",
            stop_type: "rollback",
            expected: ("InvalidParameterException", INVALID_STOP_TYPE),
        },
        StopTypeCase {
            case: "a stop type of some other API",
            stop_type: "CANCEL",
            expected: ("InvalidParameterException", INVALID_STOP_TYPE),
        },
        StopTypeCase {
            case: "a stop type that names nothing at all",
            stop_type: "",
            expected: ("InvalidParameterException", INVALID_STOP_TYPE),
        },
        StopTypeCase {
            case: "a value in the set with padding around it",
            stop_type: " ROLLBACK ",
            expected: ("InvalidParameterException", INVALID_STOP_TYPE),
        },
    ];

    let router = service();
    cluster_with_capacity(&router, CLUSTER, "capacity").await;
    let definition = register_definition(&router, "stoptypeapp", "capacity").await;
    let rollout = create_service(&router, "stop-type-service", &definition, "capacity").await;
    let before = describe_service_deployment(&router, &rollout, "capacity").await;

    for row in &cases {
        let case = row.case;
        let (code, message) = refusal(
            &router,
            "StopServiceDeployment",
            json!({ "serviceDeploymentArn": rollout, "stopType": row.stop_type }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            row.expected,
            "case {case:?}: expected {:?}, got {code} with {message:?}",
            row.expected
        );
        // Nothing the refusal touched moved: the rollout is still the one the create
        // started, in the state it was in.
        let after = describe_service_deployment(&router, &rollout, case).await;
        assert_eq!(
            (
                string(&after, "status", case),
                after["rollback"].clone(),
                after["stoppedAt"].clone(),
            ),
            (
                string(&before, "status", case),
                before["rollback"].clone(),
                before["stoppedAt"].clone(),
            ),
            "case {case:?}: expected a refused stop to change nothing, got {after}"
        );
    }
}

/// The unsupported stop type is refused for being unsupported whatever state the rollout it
/// names is in — the refusal comes before that state is weighed, so it never turns into the
/// conflict a stop of a rollout that is not ongoing earns.
#[tokio::test]
async fn the_unsupported_stop_type_is_refused_before_the_rollouts_state_is_weighed() {
    let case = "an unsupported stop of a rollout that already ended";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let first = register_definition(&router, "stoptypeended", case).await;
    let second = register_definition(&router, "stoptypeended", case).await;
    let replaced = create_service(&router, "stop-type-ended-service", &first, case).await;
    // A rollout a later release REPLACED, so it ended without anybody asking it to: a stop
    // of it that this API carries out is refused for that state.
    ok(
        &router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": "stop-type-ended-service",
            "taskDefinition": second,
        }),
        case,
    )
    .await;

    let (code, message) = refusal(
        &router,
        "StopServiceDeployment",
        json!({ "serviceDeploymentArn": replaced, "stopType": "ROLLBACK" }),
        case,
    )
    .await;
    assert_eq!(
        code, "ConflictException",
        "case {case:?}: expected the stop this API carries out to be refused for the \
         rollout's state, got {code} with {message:?}"
    );

    let (code, message) = refusal(
        &router,
        "StopServiceDeployment",
        json!({ "serviceDeploymentArn": replaced, "stopType": "ABORT" }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("UnsupportedFeatureException", UNSUPPORTED_ABORT),
        "case {case:?}: expected the unsupported stop type to be refused for being \
         unsupported, got {code} with {message:?}"
    );
}
