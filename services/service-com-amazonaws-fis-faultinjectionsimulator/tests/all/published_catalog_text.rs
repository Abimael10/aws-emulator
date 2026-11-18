// SPDX-License-Identifier: CC0-1.0

//! The TEXT FIS's two service-owned catalogs serve, end to end over HTTP.
//!
//! Every assertion here goes through the router, so each case exercises routing,
//! deserialization, the handler and serialization — the same path a client takes. The rule
//! under test is one rule over both catalogs and all four of their readers:
//!
//! * a catalog read reports the entry's PUBLISHED text verbatim — the description of an
//!   action, of a target resource type, and of each of their parameters is fixed data keyed
//!   by that entry's id, so it is reported character for character, punctuation included,
//!   and never restated in the emulator's own words;
//! * an action publishes each supported target under the PUBLISHED key for that action —
//!   the key an experiment template's action binds the target by — paired with the resource
//!   type that key names;
//! * the same id answers the same text from either reader (`ListActions` and `GetAction`,
//!   `ListTargetResourceTypes` and `GetTargetResourceType`), at any page size and however
//!   often it is read, because one table is behind all four;
//! * the type listing enumerates the published set in the published ORDER, so a client that
//!   pages it or indexes into it sees the entry the service puts at that index.
//!
//! SOURCE for the expected text below: the published catalogs themselves — the action
//! catalog the `Action` shape's own model documentation points at
//! (<https://docs.aws.amazon.com/fis/latest/userguide/fis-actions-reference.html>) and the
//! published type listing — which carry the description of every entry and parameter. The
//! expected strings are written out here rather than imported from `src/state/catalog.rs`,
//! so a restated entry in that table fails this suite instead of agreeing with it. The rows
//! span the entries whose published text differs from a restatement only in punctuation,
//! entries whose text carries a parenthetical or a documented default, and entries no issue
//! cited at all.
//!
//! The HTTP harness below is local to this suite, as each sibling suite's is.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use service_com_amazonaws_fis_faultinjectionsimulator::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the suite's requests are signed for.
const REGION: &str = "us-east-1";

/// `maxResults`' published ceiling (`@range(min: 1, max: 100)`), the page size the whole of
/// either catalog fits in.
const MAX_RESULTS_CEILING: i32 = 100;

/// A SigV4 credential scope naming this service. The service field is what routes the
/// request to fis.
fn signed_for(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/fis/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One GET against this service.
async fn get(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "fis.us-east-1.amazonaws.com")
        .header("authorization", signed_for(REGION))
        .header("content-type", "application/json")
        .body(Bytes::new())
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "GET {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, body)
}

/// Every entry of one paginated catalog, walked at `page_size` and concatenated in the
/// order the pages reported them.
async fn walk(
    router: &MultiServiceRouter,
    path: &str,
    member: &str,
    page_size: Option<i32>,
) -> Vec<Value> {
    let case = format!("{path} at maxResults={page_size:?}");
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..=256 {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("maxResults={size}"));
        }
        if let Some(next) = &token {
            query.push(format!("nextToken={next}"));
        }
        let uri = if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{}", query.join("&"))
        };
        let (status, body) = get(router, &uri).await;
        assert_eq!(
            status, 200,
            "case {case:?} page {page}: expected HTTP 200, got {status} with {body:?}"
        );
        let items = body[member]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?} page {page}: no {member} list in {body:?}"))
            .clone();
        collected.extend(items);
        match body["nextToken"].as_str() {
            None => return collected,
            Some(next) => token = Some(next.to_string()),
        }
    }
    panic!("case {case:?}: pagination did not terminate");
}

/// The published target resource types, in the published order.
async fn resource_types(router: &MultiServiceRouter, page_size: Option<i32>) -> Vec<Value> {
    walk(
        router,
        "/targetResourceTypes",
        "targetResourceTypes",
        page_size,
    )
    .await
}

/// The published actions, in the published order.
async fn actions(router: &MultiServiceRouter, page_size: Option<i32>) -> Vec<Value> {
    walk(router, "/actions", "actions", page_size).await
}

/// The entry of `listed` whose `member` equals `id`.
fn listed_entry<'a>(listed: &'a [Value], member: &str, id: &str) -> &'a Value {
    listed
        .iter()
        .find(|entry| entry[member].as_str() == Some(id))
        .unwrap_or_else(|| panic!("case {id:?}: the listing does not publish it: {listed:?}"))
}

/// One published TARGET RESOURCE TYPE row: its id and the description the reference
/// publishes for it.
struct TypeText {
    id: &'static str,
    description: &'static str,
}

/// The rows the text rule is asserted over for the resource-type catalog.
///
/// They differ along what the rule generalises over: whether the published description ends
/// in a period (`aws:ec2:subnet`) or not (`aws:dynamodb:global-table`), whether it spells a
/// service's name out (`aws:ec2:ebs-volume`) or abbreviates it (`aws:ec2:autoscaling-group`),
/// whether the resource noun is a product name in title case (`aws:ec2:transit-gateway`) or
/// a plain noun (`aws:ec2:instance`), whether it carries a parenthetical
/// (`aws:directconnect:virtual-interface`), and which AWS service and family of resource the
/// id names. `aws:dsql:cluster`, `aws:ecs:task`, `aws:iam:role` and `aws:ec2:spot-instance`
/// are rows no issue cited.
fn type_text() -> Vec<TypeText> {
    vec![
        TypeText {
            id: "aws:arc:zonal-shift-managed-resource",
            description: "An ARC Zonal Shift Managed Resource.",
        },
        TypeText {
            id: "aws:directconnect:virtual-interface",
            description: "A Direct Connect Virtual Interface (VIF).",
        },
        TypeText {
            id: "aws:dsql:cluster",
            description: "An Amazon Aurora DSQL cluster.",
        },
        TypeText {
            id: "aws:dynamodb:global-table",
            description: "An Amazon DynamoDB global table",
        },
        TypeText {
            id: "aws:ec2:autoscaling-group",
            description: "An Amazon EC2 Auto Scaling group",
        },
        TypeText {
            id: "aws:ec2:ebs-volume",
            description: "An Amazon Elastic Block Store volume",
        },
        TypeText {
            id: "aws:ec2:instance",
            description: "An Amazon EC2 instance.",
        },
        TypeText {
            id: "aws:ec2:subnet",
            description: "An Amazon EC2 subnet.",
        },
        TypeText {
            id: "aws:ec2:spot-instance",
            description: "An Amazon EC2 Spot Instance.",
        },
        TypeText {
            id: "aws:ec2:transit-gateway",
            description: "An Amazon EC2 Transit Gateway.",
        },
        // A row whose published text names neither the resource's AWS service nor "Amazon":
        // the listing publishes the plain resource noun for it.
        TypeText {
            id: "aws:ec2:vpc-endpoint",
            description: "A Virtual Private Cloud (VPC) endpoint.",
        },
        TypeText {
            id: "aws:ecs:task",
            description: "An Amazon ECS task.",
        },
        TypeText {
            id: "aws:eks:nodegroup",
            description: "An Amazon EKS node group.",
        },
        TypeText {
            id: "aws:iam:role",
            description: "An IAM role.",
        },
    ]
}

/// One published ACTION row: its id, the description the reference publishes for it, and the
/// description it publishes for each of the action's parameters.
struct ActionText {
    id: &'static str,
    description: &'static str,
    parameters: &'static [(&'static str, &'static str)],
}

/// The rows the text rule is asserted over for the action catalog.
///
/// Every row's description is a string the RECORDED page establishes for that id: the seven
/// the report quotes verbatim, `aws:ssm:send-command` (whose string the replay of that page
/// confirms), and the two rows the recording reports no divergence on at all
/// (`aws:arc:start-zonal-autoshift`, `aws:ec2:reboot-instances`). A row whose published text
/// is not established is NOT pinned here — `aws:fis:wait` is one such id, and it carries no
/// row — because this table is the authority check: an unverified expectation in it would only
/// re-assert what the table under test carries.
///
/// They differ along what the rule generalises over: how many parameters the action takes
/// (none, one, two, three, four) and whether a parameter's published text names a unit or a
/// format; and, for the description, whether the published string is imperative
/// (`aws:ec2:reboot-instances`, `aws:ssm:send-command`), states the fault as the consequence a
/// caller observes (`aws:ec2:api-insufficient-instance-capacity-error`), names the tool that
/// performs it inside the container (`aws:ecs:task-cpu-stress`), or opens with a bare
/// percentage (`aws:ecs:drain-container-instances`). `aws:ec2:reboot-instances` and
/// `aws:ssm:send-command` are rows no issue quoted a string for.
fn action_text() -> Vec<ActionText> {
    vec![
        ActionText {
            id: "aws:arc:start-zonal-autoshift",
            description: "Shift application traffic away from, and back to, an Availability Zone.",
            parameters: &[
                (
                    "availabilityZoneIdentifier",
                    "Traffic moves away from this Availability Zone.",
                ),
                (
                    "duration",
                    "The length of time to shift away from the Availability Zone \
                     (ISO 8601 duration).",
                ),
            ],
        },
        ActionText {
            id: "aws:ec2:reboot-instances",
            description: "Reboot the specified EC2 instances.",
            parameters: &[],
        },
        ActionText {
            id: "aws:ec2:api-insufficient-instance-capacity-error",
            description: "Cause the EC2 service to return insufficient capacity error responses \
                          for specific callers.",
            parameters: &[
                (
                    "availabilityZoneIdentifiers",
                    "The Availability Zones, separated by commas.",
                ),
                ("duration", "The duration of the action, in ISO 8601 format."),
                ("percentage", "The percentage of calls to affect."),
            ],
        },
        ActionText {
            id: "aws:ec2:asg-insufficient-instance-capacity-error",
            description: "Cause the targeted AutoScaling Groups to receive insufficient instance \
                          capacity errors when attempting to provision new instances.",
            parameters: &[
                (
                    "availabilityZoneIdentifiers",
                    "The Availability Zones, separated by commas.",
                ),
                ("duration", "The duration of the action, in ISO 8601 format."),
                ("percentage", "The percentage of calls to affect."),
            ],
        },
        ActionText {
            id: "aws:ec2:send-spot-instance-interruptions",
            description: "Interrupt the specified EC2 Spot instances.",
            parameters: &[(
                "durationBeforeInterruption",
                "The length of time before the interruption notice is sent, in ISO 8601 format.",
            )],
        },
        ActionText {
            id: "aws:ecs:drain-container-instances",
            description: "Drain percentage of underlying EC2 instances on an ECS cluster.",
            parameters: &[
                (
                    "drainagePercentage",
                    "The percentage of container instances to drain.",
                ),
                ("duration", "The duration of the action, in ISO 8601 format."),
            ],
        },
        ActionText {
            id: "aws:ecs:stop-task",
            description: "Stop the specified ECS tasks of an ECS cluster.",
            parameters: &[],
        },
        ActionText {
            id: "aws:ecs:task-cpu-stress",
            description: "It runs CPU stress via stress-ng tool.",
            parameters: &[
                ("duration", "The duration of the action, in ISO 8601 format."),
                (
                    "installDependencies",
                    "Whether to install the dependencies the action needs on the target tasks.",
                ),
                ("percent", "The percentage of CPU load to generate."),
                ("workers", "The number of stress workers to run."),
            ],
        },
        ActionText {
            id: "aws:ecs:task-io-stress",
            description: "It runs IO stress via stress-ng tool.",
            parameters: &[
                ("duration", "The duration of the action, in ISO 8601 format."),
                (
                    "installDependencies",
                    "Whether to install the dependencies the action needs on the target tasks.",
                ),
                ("percent", "The percentage of the file system to use."),
                ("workers", "The number of stress workers to run."),
            ],
        },
        ActionText {
            id: "aws:ssm:send-command",
            description: "Run the specified SSM document.",
            parameters: &[
                (
                    "documentArn",
                    "The Amazon Resource Name (ARN) of the document.",
                ),
                (
                    "documentParameters",
                    "The document parameters, as a JSON object.",
                ),
                ("documentVersion", "The version of the document."),
                ("duration", "The duration of the action, in ISO 8601 format."),
            ],
        },
    ]
}

/// One published action's TARGET contract: every key the catalog publishes a supported target
/// under, paired with the `ActionTarget$resourceType` that key names.
struct ActionTargets {
    id: &'static str,
    targets: &'static [(&'static str, &'static str)],
}

/// The rows the target-key rule is asserted over.
///
/// A target's key is published data, not a label: it is the key an experiment template's
/// action names in `ExperimentTemplateAction$targets` to bind the action to a declared
/// target, so a client that reads an action's target contract here and then builds a
/// template has to find the same key the published catalog uses. It is published PER ACTION
/// and is derivable neither from the resource type (`aws:ec2:subnet` is published under
/// `Subnets` here, `aws:kinesis:stream` under `KinesisStreams`) nor from the resource's
/// spelling elsewhere (`VPCEndpoints`, not `VpcEndpoints`).
///
/// The rows differ along what the rule generalises over: how many targets an action
/// publishes (none for `aws:ssm:start-automation-execution`, one for the rest), and whether
/// the key is the plain plural of the resource (`Pods`, `Subnets`), the resource qualified by
/// its service (`KinesisStreams`), the resource qualified by its topology
/// (`MultiRegionClusters`), an acronym (`VPCEndpoints`), or a noun the action's own id does
/// not carry (`Subnets` for the route-table action, which is pointed at the subnets a route
/// table serves). `aws:eks:pod-delete`, `aws:network:disrupt-connectivity` and
/// `aws:ssm:start-automation-execution` are rows no issue cited.
fn action_targets() -> Vec<ActionTargets> {
    vec![
        ActionTargets {
            id: "aws:eks:pod-delete",
            targets: &[("Pods", "aws:eks:pod")],
        },
        ActionTargets {
            id: "aws:kinesis:stream-expired-iterator-exception",
            targets: &[("KinesisStreams", "aws:kinesis:stream")],
        },
        ActionTargets {
            id: "aws:kinesis:stream-provisioned-throughput-exception",
            targets: &[("KinesisStreams", "aws:kinesis:stream")],
        },
        ActionTargets {
            id: "aws:memorydb:multi-region-cluster-pause-replication",
            targets: &[("MultiRegionClusters", "aws:memorydb:multi-region-cluster")],
        },
        ActionTargets {
            id: "aws:network:disrupt-connectivity",
            targets: &[("Subnets", "aws:ec2:subnet")],
        },
        ActionTargets {
            id: "aws:network:disrupt-vpc-endpoint",
            targets: &[("VPCEndpoints", "aws:ec2:vpc-endpoint")],
        },
        ActionTargets {
            id: "aws:network:route-table-disrupt-cross-region-connectivity",
            targets: &[("Subnets", "aws:ec2:subnet")],
        },
        ActionTargets {
            id: "aws:ssm:start-automation-execution",
            targets: &[],
        },
    ]
}

/// The `targets` map `reported` carries, as key -> `resourceType` pairs.
fn reported_targets(case: &str, reported: &Value) -> BTreeMap<String, String> {
    reported
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a targets map, got {reported:?}"))
        .iter()
        .map(|(key, target)| {
            let resource_type = target["resourceType"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: target {key:?} names no resourceType: {target:?}")
            });
            (key.clone(), resource_type.to_string())
        })
        .collect()
}

/// A row's expected `targets` map, in the same form [`reported_targets`] reports.
fn expected_targets(targets: &[(&str, &str)]) -> BTreeMap<String, String> {
    targets
        .iter()
        .map(|(key, resource_type)| ((*key).to_string(), (*resource_type).to_string()))
        .collect()
}

/// The parameter text of the resource types that publish a parameter contract, as the
/// reference states it — including the DEFAULT one of them documents, which is part of the
/// published text and not a separate member.
fn type_parameter_text() -> Vec<(&'static str, &'static [(&'static str, &'static str)])> {
    vec![
        (
            "aws:arc:zonal-shift-managed-resource",
            &[
                (
                    "managedResourceTypes",
                    "The resource types that will use zonal autoshift.",
                ),
                (
                    "zonalAutoshiftStatus",
                    "The zonalAutoshiftStatus of the resources you want to target. The default \
                     is enabled.",
                ),
            ],
        ),
        (
            "aws:ecs:task",
            &[
                ("cluster", "The name or ARN of the cluster the tasks run on."),
                (
                    "service",
                    "The name of the service whose tasks are identified.",
                ),
            ],
        ),
    ]
}

/// The published ORDER of the type listing: the ids `ListTargetResourceTypes` enumerates, in
/// the order it enumerates them.
///
/// Written out so the INDEX a client reads an entry at is asserted, not just the set: the
/// ids ascend, which is the order the published listing enumerates them in.
const PUBLISHED_TYPE_ORDER: &[&str] = &[
    "aws:arc:zonal-shift-managed-resource",
    "aws:directconnect:virtual-interface",
    "aws:dsql:cluster",
    "aws:dynamodb:global-table",
    "aws:ec2:autoscaling-group",
    "aws:ec2:ebs-volume",
    "aws:ec2:instance",
    "aws:ec2:spot-instance",
    "aws:ec2:subnet",
    "aws:ec2:transit-gateway",
    "aws:ec2:vpc-endpoint",
    "aws:ecs:cluster",
    "aws:ecs:task",
    "aws:eks:cluster",
    "aws:eks:nodegroup",
    "aws:eks:pod",
    "aws:elasticache:replicationgroup",
    "aws:iam:role",
    "aws:kinesis:stream",
    "aws:lambda:function",
    "aws:memorydb:multi-region-cluster",
    "aws:rds:cluster",
    "aws:rds:db",
    "aws:s3:bucket",
];

#[tokio::test]
async fn a_resource_type_reports_its_published_description_from_both_readers() {
    let router = emulator();
    let listed = resource_types(&router, None).await;

    for row in type_text() {
        let case = row.id;
        let summary = listed_entry(&listed, "resourceType", row.id);
        assert_eq!(
            summary["description"].as_str(),
            Some(row.description),
            "case {case:?}: expected the listing to publish the description {:?}, got {:?}",
            row.description,
            summary["description"]
        );

        let (status, body) = get(&router, &format!("/targetResourceTypes/{}", row.id)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading the type, got {status} with {body:?}"
        );
        let reported = &body["targetResourceType"];
        assert_eq!(
            reported["description"].as_str(),
            Some(row.description),
            "case {case:?}: expected the read to report the description {:?}, got {:?}",
            row.description,
            reported["description"]
        );
    }
}

#[tokio::test]
async fn a_resource_types_parameters_report_their_published_descriptions() {
    let router = emulator();

    for (id, parameters) in type_parameter_text() {
        let (status, body) = get(&router, &format!("/targetResourceTypes/{id}")).await;
        assert_eq!(
            status, 200,
            "case {id:?}: expected HTTP 200 reading the type, got {status} with {body:?}"
        );
        let reported = &body["targetResourceType"]["parameters"];
        for (name, description) in parameters {
            let case = format!("{id}/{name}");
            assert_eq!(
                reported[*name]["description"].as_str(),
                Some(*description),
                "case {case:?}: expected the published parameter description {description:?}, \
                 got {:?}",
                reported[*name]["description"]
            );
        }
        // A parameter map is a modelled MAP, so the wire order of its keys is not part of
        // the contract: the SET of published parameter names is.
        let reported_names: BTreeSet<&str> = reported
            .as_object()
            .unwrap_or_else(|| panic!("case {id:?}: expected a parameters map, got {reported:?}"))
            .keys()
            .map(String::as_str)
            .collect();
        let expected_names: BTreeSet<&str> = parameters.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            reported_names, expected_names,
            "case {id:?}: expected exactly the published parameters {expected_names:?}, got \
             {reported_names:?}"
        );
    }
}

#[tokio::test]
async fn an_action_reports_its_published_description_and_parameter_text() {
    let router = emulator();
    let listed = actions(&router, None).await;

    for row in action_text() {
        let case = row.id;
        let summary = listed_entry(&listed, "id", row.id);
        assert_eq!(
            summary["description"].as_str(),
            Some(row.description),
            "case {case:?}: expected the listing to publish the description {:?}, got {:?}",
            row.description,
            summary["description"]
        );

        let (status, body) = get(&router, &format!("/actions/{}", row.id)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading the action, got {status} with {body:?}"
        );
        let action = &body["action"];
        assert_eq!(
            action["description"].as_str(),
            Some(row.description),
            "case {case:?}: expected the read to report the description {:?}, got {:?}",
            row.description,
            action["description"]
        );

        let parameters = &action["parameters"];
        for (name, description) in row.parameters {
            let parameter_case = format!("{case}/{name}");
            assert_eq!(
                parameters[*name]["description"].as_str(),
                Some(*description),
                "case {parameter_case:?}: expected the published parameter description \
                 {description:?}, got {:?}",
                parameters[*name]["description"]
            );
        }
        // As above: the parameter map's key order is not part of the contract, its key set
        // is.
        let reported_names: BTreeSet<&str> = parameters
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected a parameters map, got {action:?}"))
            .keys()
            .map(String::as_str)
            .collect();
        let expected_names: BTreeSet<&str> = row.parameters.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            reported_names, expected_names,
            "case {case:?}: expected exactly the published parameters {expected_names:?}, got \
             {reported_names:?}"
        );
    }
}

#[tokio::test]
async fn the_type_listing_enumerates_the_published_order_at_every_page_size() {
    let router = emulator();

    // Each row is a page size: the published floor, a size that cuts the catalog into
    // several pages, one past the catalog, the published ceiling, and no `maxResults` at
    // all. The published order is the same at each of them, and so is each entry's text.
    for page_size in [
        Some(1),
        Some(5),
        Some(i32::try_from(PUBLISHED_TYPE_ORDER.len() + 1).expect("the catalog size fits an i32")),
        Some(MAX_RESULTS_CEILING),
        None,
    ] {
        let case = format!("maxResults={page_size:?}");
        let listed = resource_types(&router, page_size).await;
        let ids: Vec<&str> = listed
            .iter()
            .map(|entry| {
                entry["resourceType"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no resourceType in {entry:?}"))
            })
            .collect();
        assert_eq!(
            ids, PUBLISHED_TYPE_ORDER,
            "case {case:?}: expected the published order {PUBLISHED_TYPE_ORDER:?}, got {ids:?}"
        );
    }
}

#[tokio::test]
async fn every_catalogued_entry_reports_the_same_text_from_both_readers() {
    // The class the rule holds over is the whole of both catalogs, so this case is driven
    // from the listings themselves rather than from a table: whatever either catalog
    // publishes, its point read answers the same text for it.
    let router = emulator();

    for entry in resource_types(&router, Some(4)).await {
        let id = entry["resourceType"]
            .as_str()
            .unwrap_or_else(|| panic!("a listed type carries no resourceType: {entry:?}"));
        let (status, body) = get(&router, &format!("/targetResourceTypes/{id}")).await;
        assert_eq!(
            status, 200,
            "case {id:?}: expected HTTP 200 reading the listed type, got {status} with {body:?}"
        );
        assert_eq!(
            body["targetResourceType"]["description"], entry["description"],
            "case {id:?}: expected the read to report the listed description {:?}, got {:?}",
            entry["description"], body["targetResourceType"]["description"]
        );
    }

    for entry in actions(&router, Some(7)).await {
        let id = entry["id"]
            .as_str()
            .unwrap_or_else(|| panic!("a listed action carries no id: {entry:?}"));
        let (status, body) = get(&router, &format!("/actions/{id}")).await;
        assert_eq!(
            status, 200,
            "case {id:?}: expected HTTP 200 reading the listed action, got {status} with {body:?}"
        );
        assert_eq!(
            body["action"]["description"], entry["description"],
            "case {id:?}: expected the read to report the listed description {:?}, got {:?}",
            entry["description"], body["action"]["description"]
        );
    }
}

#[tokio::test]
async fn an_action_publishes_its_targets_under_the_published_key_from_both_readers() {
    let router = emulator();
    let listed = actions(&router, None).await;

    for row in action_targets() {
        let case = row.id;
        let summary = listed_entry(&listed, "id", row.id);
        let expected = expected_targets(row.targets);
        assert_eq!(
            reported_targets(case, &summary["targets"]),
            expected,
            "case {case:?}: expected the listing to publish the targets {expected:?}, got {:?}",
            summary["targets"]
        );

        let (status, body) = get(&router, &format!("/actions/{}", row.id)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading the action, got {status} with {body:?}"
        );
        assert_eq!(
            reported_targets(case, &body["action"]["targets"]),
            expected,
            "case {case:?}: expected the read to report the targets {expected:?}, got {:?}",
            body["action"]["targets"]
        );
    }
}

#[tokio::test]
async fn reading_the_catalog_text_twice_answers_the_same_text() {
    // Catalog text is fixed data: a read changes nothing, so the same request answers the
    // same text afterwards — including after the other catalog and the point reads have
    // been exercised in between.
    let router = emulator();

    let before = resource_types(&router, None).await;
    let actions_before = actions(&router, None).await;
    for row in type_text() {
        let _ = get(&router, &format!("/targetResourceTypes/{}", row.id)).await;
    }
    for row in action_text() {
        let _ = get(&router, &format!("/actions/{}", row.id)).await;
    }
    let after = resource_types(&router, None).await;
    let actions_after = actions(&router, None).await;

    assert_eq!(
        after, before,
        "case targetResourceTypes: expected the same text on a second read, got {after:?} after \
         {before:?}"
    );
    assert_eq!(
        actions_after, actions_before,
        "case actions: expected the same text on a second read, got {actions_after:?} after \
         {actions_before:?}"
    );
}
