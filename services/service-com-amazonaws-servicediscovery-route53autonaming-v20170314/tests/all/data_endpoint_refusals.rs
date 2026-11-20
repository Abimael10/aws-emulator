// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH error code a Cloud Map request outside the constraints the
//! model declares is refused with, over the wire.
//!
//! RULE: an operation the model hosts on an endpoint of its own — one carrying an
//! `@endpoint` host prefix, which for this model is the two discovery reads
//! (`smithy_query(query="operation", operation="DiscoverInstances,DiscoverInstancesRevision")`
//! reports `endpoint.hostPrefix` `data-` on both) — is fronted by AWS's generic
//! request-validation layer and refuses such a request with HTTP 400 and the error code
//! `ValidationException`, a shape this model does not declare. An operation on the
//! service's main endpoint is refused by the service itself, under the one error ITS model
//! declares for the class, `InvalidInput`. Only the CODE differs: both state the same
//! counted envelope, naming the member, the value the request carried and the requirement
//! it failed.
//!
//! The rows differ along what the rule generalises over: the constraint class (an
//! enumerated type, a `@range` bound, a `@length` bound, a `@pattern`, an absent
//! `@required` member), which of the two endpoints hosts the operation, which operation of
//! that endpoint carried it, and — for the bounded members — each published bound and one
//! value past it. The last table is the positive control: every value the model DOES admit
//! is served.
//!
//! Every request goes through `MultiServiceRouter::dispatch`, the same path the emulator
//! serves in production; the harness lives in `cloud_map_core`.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_servicediscovery_route53autonaming_v20170314 as cloudmap;

use crate::cloud_map_core::{call, emulator, namespace, ok, refusal, service};

/// The error code the generic request-validation layer in front of the data endpoint
/// refuses a request with.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The one error this model declares for a bad request, which is what the service's main
/// endpoint refuses one with.
const INVALID_INPUT: &str = "InvalidInput";

/// `HealthStatusFilter`, whose members are `ALL`, `HEALTHY`, `HEALTHY_OR_ELSE_ALL` and
/// `UNHEALTHY` (`smithy_query(query="shape", shape="HealthStatusFilter")`), listed in the
/// order the table of those values is walked: each value's bucket ascending, the bucket
/// being the low five bits of its 31-based string hash — `ALL` 1, `UNHEALTHY` 4,
/// `HEALTHY_OR_ELSE_ALL` 21, `HEALTHY` 29, no two sharing a bucket.
const HEALTH_STATUS_FILTER_SET: &str = "ALL, UNHEALTHY, HEALTHY_OR_ELSE_ALL, HEALTHY";

/// `CustomHealthStatus`, whose members are `HEALTHY` and `UNHEALTHY`
/// (`smithy_query(query="shape", shape="CustomHealthStatus")`), in the same order: bucket
/// 4 before bucket 29.
const CUSTOM_HEALTH_STATUS_SET: &str = "UNHEALTHY, HEALTHY";

/// The `@pattern` the model declares on `NamespaceName`
/// (`smithy_query(query="shape", shape="NamespaceName")`), which a clause quotes as the
/// model states it.
const NAMESPACE_NAME_PATTERN: &str = "^[!-~]{1,1024}$";

/// The counted envelope over ONE clause: the value the request submitted (`None` for a
/// member it left out, which has none), the member path the clause states it at, and the
/// requirement it failed.
fn one_violation(submitted: Option<&str>, path: &str, requirement: &str) -> String {
    let value = match submitted {
        Some(value) => format!("Value '{value}'"),
        None => "Value null".to_string(),
    };
    format!(
        "1 validation error detected: {value} at '{path}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// The requirement an enumerated type states: the set it admits, in the order this service
/// lists it.
fn enum_value_set(values: &str) -> String {
    format!("Member must satisfy enum value set: [{values}]")
}

/// One row: the operation to call, the request, and the code and message it must be refused
/// with.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
    code: &'static str,
    message: String,
}

/// The set an enum clause listed, in the order it listed it.
fn listed_set(message: &str, case: &str) -> BTreeSet<String> {
    message
        .split_once("enum value set: [")
        .and_then(|(_, tail)| tail.strip_suffix(']'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a stated value set, got {message:?}"))
        .split(", ")
        .map(str::to_string)
        .collect()
}

/// Refuse every row and assert the status, the code and the whole message.
async fn assert_rows(router: &smithy_server_runtime::MultiServiceRouter, rows: Vec<Row>) {
    for row in rows {
        let Row {
            case,
            operation,
            body,
            code: expected_code,
            message: expected_message,
        } = row;
        let (status, code, message) = refusal(router, operation, body.clone()).await;
        let message = message.unwrap_or_default();
        assert_eq!(
            (status, code.as_str()),
            (400, expected_code),
            "case {case:?} on {operation} with {body}: expected 400 {expected_code}, got {status} \
             {code} {message:?}"
        );
        assert_eq!(
            message, expected_message,
            "case {case:?} on {operation}: expected {expected_message:?}, got {message:?}"
        );
    }
}

/// Every constraint class, refused at the DATA endpoint under the generic validation code.
#[tokio::test]
async fn the_data_endpoint_refuses_a_bad_value_with_the_generic_validation_code() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "data-endpoint-ns").await;
    service(&router, &namespace_id, "data-endpoint-svc").await;

    /// A `DiscoverInstances` request for the namespace and service created above, so a row
    /// changes one member and nothing else.
    fn discover() -> Value {
        json!({ "NamespaceName": "data-endpoint-ns", "ServiceName": "data-endpoint-svc" })
    }

    let mut rows: Vec<Row> = Vec::new();

    // The enumerated type: two values outside `HealthStatusFilter`, one of them a member
    // in another case, since an enumerated value is matched as the model spells it.
    for bad in ["BOGUS", "healthy"] {
        let mut body = discover();
        body["HealthStatus"] = json!(bad);
        rows.push(Row {
            case: "a health status outside HealthStatusFilter",
            operation: "DiscoverInstances",
            body,
            code: VALIDATION_EXCEPTION,
            message: one_violation(
                Some(bad),
                "healthStatus",
                &enum_value_set(HEALTH_STATUS_FILTER_SET),
            ),
        });
    }

    // The `@range` bounds of `DiscoverMaxResults` (range 1..1000): one under the published
    // minimum and one over the published maximum.
    for (bad, requirement) in [
        (0, "Member must have value greater than or equal to 1"),
        (1001, "Member must have value less than or equal to 1000"),
    ] {
        let mut body = discover();
        body["MaxResults"] = json!(bad);
        rows.push(Row {
            case: "a MaxResults outside the declared range",
            operation: "DiscoverInstances",
            body,
            code: VALIDATION_EXCEPTION,
            message: one_violation(Some(&bad.to_string()), "maxResults", requirement),
        });
    }

    // The `@length` bounds of `AWSAccountId` (len 12..12), on both discovery reads: one
    // short of the width and one past it.
    for operation in ["DiscoverInstances", "DiscoverInstancesRevision"] {
        for (bad, requirement) in [
            ("1".repeat(11), "Member must have length greater than or equal to 12"),
            ("1".repeat(13), "Member must have length less than or equal to 12"),
        ] {
            let mut body = discover();
            body["OwnerAccount"] = json!(bad);
            rows.push(Row {
                case: "an OwnerAccount that is not 12 characters",
                operation,
                body,
                code: VALIDATION_EXCEPTION,
                message: one_violation(Some(&bad), "ownerAccount", requirement),
            });
        }
    }

    // The `@pattern` of `NamespaceName`, on both discovery reads: a name carrying a
    // character the expression excludes, and the empty name its `@length` minimum of 0
    // admits as far as the length.
    for operation in ["DiscoverInstances", "DiscoverInstancesRevision"] {
        for bad in ["has a space", ""] {
            let mut body = discover();
            body["NamespaceName"] = json!(bad);
            rows.push(Row {
                case: "a NamespaceName outside the declared pattern",
                operation,
                body,
                code: VALIDATION_EXCEPTION,
                message: one_violation(
                    Some(bad),
                    "namespaceName",
                    &format!(
                        "Member must satisfy regular expression pattern: {NAMESPACE_NAME_PATTERN}"
                    ),
                ),
            });
        }
    }

    // An absent `@required` member, on both discovery reads and on each of the two members
    // the model requires.
    for operation in ["DiscoverInstances", "DiscoverInstancesRevision"] {
        for (omitted, path) in [("NamespaceName", "namespaceName"), ("ServiceName", "serviceName")] {
            let mut body = discover();
            body.as_object_mut().expect("an object body").remove(omitted);
            rows.push(Row {
                case: "a required member the request omits",
                operation,
                body,
                code: VALIDATION_EXCEPTION,
                message: one_violation(None, path, "Member must not be null"),
            });
        }
    }

    // The refusal is the request's, not the store's: a bad value naming resources nothing
    // created is still refused by the validation ahead of the handler rather than with the
    // handler's own not-found error.
    rows.push(Row {
        case: "a bad value naming a namespace and service nothing created",
        operation: "DiscoverInstances",
        body: json!({
            "NamespaceName": "never-created-ns",
            "ServiceName": "never-created-svc",
            "HealthStatus": "BOGUS",
        }),
        code: VALIDATION_EXCEPTION,
        message: one_violation(
            Some("BOGUS"),
            "healthStatus",
            &enum_value_set(HEALTH_STATUS_FILTER_SET),
        ),
    });

    assert_rows(&router, rows).await;

    // The set LISTED holds exactly the values the model declares, whatever order it is
    // listed in: a listing that dropped or invented one would satisfy the messages above
    // only by being wrong in both places at once.
    let mut body = discover();
    body["HealthStatus"] = json!("BOGUS");
    let (_, _, message) = refusal(&router, "DiscoverInstances", body).await;
    let message = message.unwrap_or_default();
    let listed = listed_set(&message, "the listed value set");
    let modelled: BTreeSet<String> = cloudmap::ops::types::HEALTH_STATUS_FILTER_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    assert_eq!(
        listed, modelled,
        "expected the declared values {modelled:?}, got {listed:?}"
    );
}

/// The SAME constraint classes on the service's main endpoint keep the one code this model
/// declares, so what decides the code is the endpoint and not the class.
#[tokio::test]
async fn the_main_endpoint_refuses_the_same_classes_with_this_model_s_own_code() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "main-endpoint-ns").await;
    let service_id = service(&router, &namespace_id, "main-endpoint-svc").await;

    let rows: Vec<Row> = vec![
        Row {
            // `CustomHealthStatus` admits HEALTHY and UNHEALTHY alone.
            case: "a status outside a declared enum",
            operation: "UpdateInstanceCustomHealthStatus",
            body: json!({ "ServiceId": service_id, "InstanceId": "i-1", "Status": "DEGRADED" }),
            code: INVALID_INPUT,
            message: one_violation(
                Some("DEGRADED"),
                "status",
                &enum_value_set(CUSTOM_HEALTH_STATUS_SET),
            ),
        },
        Row {
            // `ListInstancesRequest$MaxResults` has range 1..100.
            case: "a value under a declared range minimum",
            operation: "ListInstances",
            body: json!({ "ServiceId": service_id, "MaxResults": 0 }),
            code: INVALID_INPUT,
            message: one_violation(
                Some("0"),
                "maxResults",
                "Member must have value greater than or equal to 1",
            ),
        },
        Row {
            case: "a value over a declared range maximum",
            operation: "ListInstances",
            body: json!({ "ServiceId": service_id, "MaxResults": 101 }),
            code: INVALID_INPUT,
            message: one_violation(
                Some("101"),
                "maxResults",
                "Member must have value less than or equal to 100",
            ),
        },
        Row {
            // `CreateServiceRequest$Name` is `@required`.
            case: "a required member the request omits",
            operation: "CreateService",
            body: json!({ "NamespaceId": namespace_id }),
            code: INVALID_INPUT,
            message: one_violation(None, "name", "Member must not be null"),
        },
        Row {
            // `GetInstancesHealthStatusRequest$MaxResults` has range 1..100 too, so the
            // class is reached through a second operation of this endpoint.
            case: "a value over a declared range maximum on another read",
            operation: "GetInstancesHealthStatus",
            body: json!({ "ServiceId": service_id, "MaxResults": 101 }),
            code: INVALID_INPUT,
            message: one_violation(
                Some("101"),
                "maxResults",
                "Member must have value less than or equal to 100",
            ),
        },
    ];

    assert_rows(&router, rows).await;
}

/// Every value the model DOES admit is served at the data endpoint: the layer in front of
/// it refuses what the model excludes and nothing else.
#[tokio::test]
async fn the_data_endpoint_serves_every_value_the_model_admits() {
    let router = emulator();
    let (_, namespace_id) = namespace(&router, "admitted-ns").await;
    let service_id = service(&router, &namespace_id, "admitted-svc").await;
    ok(
        &router,
        "RegisterInstance",
        json!({
            "ServiceId": service_id,
            "InstanceId": "i-admitted",
            "Attributes": { "AWS_INSTANCE_IPV4": "192.0.2.7" },
        }),
    )
    .await;

    /// The request every row below changes one member of.
    fn discover() -> Value {
        json!({ "NamespaceName": "admitted-ns", "ServiceName": "admitted-svc" })
    }

    // (case, the member, the value it admits) — every member of `HealthStatusFilter`, each
    // published `MaxResults` bound, and an `OwnerAccount` of the declared width.
    let mut rows: Vec<(String, &str, Value)> = cloudmap::ops::types::HEALTH_STATUS_FILTER_VALUES
        .iter()
        .map(|value| {
            (
                format!("the health status {value}"),
                "HealthStatus",
                json!(value),
            )
        })
        .collect();
    rows.push((
        "the published MaxResults minimum".to_string(),
        "MaxResults",
        json!(1),
    ));
    rows.push((
        "the published MaxResults maximum".to_string(),
        "MaxResults",
        json!(1000),
    ));
    rows.push((
        "an OwnerAccount of the declared width".to_string(),
        "OwnerAccount",
        json!(engine::EMULATOR_ACCOUNT_ID),
    ));

    for (case, member, value) in rows {
        let mut body = discover();
        body[member] = value.clone();
        let (status, answer) = call(&router, "DiscoverInstances", body.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the model admits {value}, got {status}: {answer}"
        );
        let found: Vec<&str> = answer["Instances"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: Instances is a list, got {answer}"))
            .iter()
            .map(|instance| instance["InstanceId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            found, ["i-admitted"],
            "case {case:?}: the registered instance is still discovered, got {answer}"
        );
    }

    // The revision read, whose model declares no enumerated member, is served the same way.
    let revision = ok(&router, "DiscoverInstancesRevision", discover()).await;
    assert_eq!(
        revision["InstancesRevision"].as_i64(),
        Some(1),
        "the one register is the one revision, got {revision}"
    );
}
