// SPDX-License-Identifier: CC0-1.0

//! The SCOPE one `DescribeServiceDeployments` batch may ask about, driven end to end over the
//! shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test:
//!
//! > A service deployment is a rollout OF one service, so every ARN of one
//! > `DescribeServiceDeployments` batch has to belong to a single service: a batch spanning
//! > two services is a cross-service request, refused whole with HTTP 400
//! > `InvalidParameterException` and the fixed sentence AWS publishes for it, before any of
//! > the ARNs is resolved — so neither the deployments it could resolve nor a `MISSING`
//! > failure for the others is reported. A batch that stays inside one service is answered,
//! > however many of its entries name the same rollout, and a service is identified by the
//! > CLUSTER that holds it and its own name together, so one name under two clusters is two
//! > services.
//!
//! The docs the rule is read off:
//!
//! * `DescribeServiceDeployments$serviceDeploymentArns`: "The ARN of the service deployment.
//!   You can specify a maximum of 20 ARNs" (required, a `StringList`).
//! * `DescribeServiceDeployments`' modelled errors (`smithy_query(query="operation",
//!   operation="DescribeServiceDeployments")`), which include `InvalidParameterException` at
//!   HTTP 400.
//! * `ListServiceDeployments`: "A service deployment happens when you release a software
//!   update for the service", and the ARN this service mints for one, which carries the
//!   cluster and the service it belongs to
//!   (`service-deployment/<cluster>/<service>/<id>`).
//!
//! The rows differ along what the rule generalises over: how many services the batch spans
//! and how it spans them — one service named once and twice over, two services of one
//! cluster, two services of the same NAME in two clusters, and a batch of the widest size
//! that still names one service.
//!
//! Nothing here reaches into the state struct: every ARN a row names is one a create minted,
//! read back out of `CreateService`'s own answer.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, refusal, register_definition, service, string,
};

/// The cluster most services below run in.
const CLUSTER: &str = "deploymentscope";

/// A SECOND cluster, which holds a service of the same name as one of this one's: a service
/// is the cluster and the name together, so those two are two services.
const OTHER_CLUSTER: &str = "deploymentscopetwo";

/// `InvalidParameterException$message` for a batch spanning two services: entirely fixed
/// text, which names neither of them — and, as AWS publishes it, carries no trailing period.
const CROSS_SERVICE: &str =
    "Cross service requests are not allowed. All requested service deployments must be for a \
     single service";

/// Create a service in `cluster` whose rollout the ECS scheduler owns, and return the ARN of
/// the service deployment that rollout is recorded as.
async fn create_service(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    definition: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "CreateService",
        json!({
            "cluster": cluster,
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

/// One row of the scope table: the batch a row names, and the one service's deployment ARNs
/// it is expected to report — `None` for a batch that spans two services and is refused
/// whole.
struct ScopeRow {
    case: &'static str,
    batch: Vec<String>,
    reported: Option<Vec<String>>,
}

/// RULE: one batch of deployment ARNs is answered exactly when it names a single service.
#[tokio::test]
async fn a_deployment_batch_is_answered_only_for_a_single_service() {
    let case = "one batch, one service";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    cluster_with_capacity(&router, OTHER_CLUSTER, case).await;
    let definition = register_definition(&router, "scopeapp", case).await;
    // Two services of one cluster, and a third of ANOTHER cluster carrying the same name as
    // the first — so the pairs that span two services differ in the service, in the cluster,
    // and in both.
    let first = create_service(&router, CLUSTER, "scope-one", &definition, case).await;
    let second = create_service(&router, CLUSTER, "scope-two", &definition, case).await;
    let elsewhere = create_service(&router, OTHER_CLUSTER, "scope-one", &definition, case).await;

    // Each row: the batch, and the one service's ARNs it is expected to report — `None` for
    // a batch that spans two services and is refused whole.
    let rows = vec![
        ScopeRow {
            case: "one deployment of one service",
            batch: vec![first.clone()],
            reported: Some(vec![first.clone()]),
        },
        ScopeRow {
            case: "the same deployment named twice",
            batch: vec![first.clone(), first.clone()],
            reported: Some(vec![first.clone()]),
        },
        ScopeRow {
            case: "twenty entries, all of one service",
            batch: vec![first.clone(); 20],
            reported: Some(vec![first.clone()]),
        },
        ScopeRow {
            case: "two services of one cluster",
            batch: vec![first.clone(), second.clone()],
            reported: None,
        },
        ScopeRow {
            case: "the same two, named the other way round",
            batch: vec![second.clone(), first.clone()],
            reported: None,
        },
        ScopeRow {
            case: "one service name under two clusters",
            batch: vec![first.clone(), elsewhere.clone()],
            reported: None,
        },
    ];

    for row in rows {
        let case = row.case;
        match row.reported {
            Some(reported) => {
                let body = ok(
                    &router,
                    "DescribeServiceDeployments",
                    json!({ "serviceDeploymentArns": row.batch }),
                    case,
                )
                .await;
                let arns: Vec<String> = list(&body, "serviceDeployments", case)
                    .iter()
                    .map(|deployment| string(deployment, "serviceDeploymentArn", case))
                    .collect();
                assert_eq!(
                    arns, reported,
                    "case {case:?}: expected the rollouts of the one service the batch \
                     names, got {body}"
                );
                assert!(
                    list(&body, "failures", case).is_empty(),
                    "case {case:?}: every entry named a stored rollout, so none of them \
                     failed, got {body}"
                );
            }
            None => {
                let (code, message) = refusal(
                    &router,
                    "DescribeServiceDeployments",
                    json!({ "serviceDeploymentArns": row.batch }),
                    case,
                )
                .await;
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    ("InvalidParameterException", CROSS_SERVICE),
                    "case {case:?}: expected the cross-service refusal, got {code} with \
                     {message:?}"
                );
            }
        }
    }
}

/// A batch spanning two services is refused for the request it is: it is answered before any
/// ARN of it is resolved, so an entry that names nothing stored does not turn the refusal
/// into the `MISSING` failure such an entry earns inside one service.
#[tokio::test]
async fn the_cross_service_refusal_comes_before_any_arn_is_resolved() {
    let case = "a cross-service batch whose other entry names nothing stored";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let definition = register_definition(&router, "scopeorderapp", case).await;
    let stored = create_service(&router, CLUSTER, "scope-order", &definition, case).await;
    // A WELL-FORMED ARN of another service of this cluster that no rollout was ever recorded
    // under: inside one service such an entry is a `MISSING` failure beside the rollouts the
    // batch did resolve.
    let absent = stored.replace("scope-order", "scope-order-other");

    let (code, message) = refusal(
        &router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [stored.clone(), absent.clone()] }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("InvalidParameterException", CROSS_SERVICE),
        "case {case:?}: expected the cross-service refusal rather than a failure for the \
         entry that names nothing, got {code} with {message:?}"
    );

    // And the same read of that absent ARN ALONE names one service, so it is answered with
    // the failure its own entry earns.
    let case = "the absent ARN on its own";
    let body = ok(
        &router,
        "DescribeServiceDeployments",
        json!({ "serviceDeploymentArns": [absent.clone()] }),
        case,
    )
    .await;
    let failures: Vec<(String, String)> = list(&body, "failures", case)
        .iter()
        .map(|failure| {
            (
                string(failure, "arn", case),
                string(failure, "reason", case),
            )
        })
        .collect();
    assert_eq!(
        failures,
        vec![(absent.clone(), "MISSING".to_string())],
        "case {case:?}: expected the entry that names nothing to fail on its own, got {body}"
    );
}
