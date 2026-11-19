// SPDX-License-Identifier: CC0-1.0

//! An `AWS::ECS::ClusterCapacityProviderAssociations`, an `AWS::ECS::TaskSet` and an
//! `AWS::ECS::PrimaryTaskSet` a stack declares reach Amazon ECS itself.
//!
//! The rule under test: **a resource type this emulator provisions is created in the service
//! that OWNS it, through that service's own API, under the identifier the stack publishes for
//! it** — and for a resource that is a BINDING rather than a named entity, that identifier is
//! the values the owning API addresses one by:
//!
//! * an association IS one cluster's capacity-provider configuration, which
//!   `PutClusterCapacityProviders` addresses by the cluster alone, so `Ref` returns that
//!   cluster and `DescribeClusters` then reports the declared providers in
//!   `capacityProviders` and the declared `defaultCapacityProviderStrategy`;
//! * a task set is addressed by the cluster, the service and the `id` Amazon ECS MINTS, so
//!   `Ref` returns that triple and `Fn::GetAtt Id` the id itself — the only handle
//!   `DescribeTaskSets`, `DeleteTaskSet` and `UpdateServicePrimaryTaskSet$primaryTaskSet`
//!   take;
//! * a primary task set is which of a service's task sets serves production traffic, so the
//!   task set the declaration names is the one that service reports `PRIMARY`.
//!
//! The rows differ along what the rule generalises over: how many capacity providers the
//! association declares and whether it declares a default strategy at all (and with what base
//! and weight), how the declaration spells the cluster and the service (by `!Ref`, which is the
//! cluster's name and the service's ARN, and by `!GetAtt … Name`), whether the task set states
//! an external id, a scale and tags, and both template spellings (YAML and JSON).
//!
//! Two tests read the same rule the other way: a default strategy naming a provider the
//! association does not carry is refused by Amazon ECS, so the stack rolls back and the cluster
//! is left exactly as it was — what makes a CREATE_COMPLETE mean the association is in force —
//! and the task set a stack created is gone from Amazon ECS once the stack is deleted, which is
//! what makes it the stack's resource rather than a leftover.
//!
//! Everything goes over the wire each service speaks — awsQuery for CloudFormation, awsJson1.1
//! for Amazon ECS — through the one router `harness.rs` builds, so what is asserted is what a
//! client that deploys the template and then reads the resources back observes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::ecs;
use crate::harness::{settled_stack_status, stack_event_reasons};
use crate::stack_lifecycle::{
    call, create_stack, member, resource_event_reason, router, xml_text, REGION,
};

/// How a read waits for a clock-owed span to come due: look again, under a deadline generous
/// enough that a loaded host still reaches it. What every case asserts is the CONDITION, never
/// how long it took.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// One association deployment: what the template declares for the cluster, and what Amazon ECS
/// must then report about the cluster the stack claims to have configured.
struct Association {
    case: &'static str,
    stack: &'static str,
    /// The row's whole template.
    template: &'static str,
    /// The providers `DescribeClusters` must report in `capacityProviders`, in order.
    capacity_providers: &'static [&'static str],
    /// The default strategy it must report, as (provider, base, weight) in order.
    default_strategy: &'static [(&'static str, i64, i64)],
}

const ASSOCIATIONS: &[Association] = &[
    Association {
        case: "one managed provider and a default strategy over it, declared in YAML with the \
               cluster named by !Ref",
        stack: "cfn-cpassoc-one-provider",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::ECS::Cluster\n",
            "    Properties:\n",
            "      ClusterName: cfn-cpassoc-one\n",
            "  CPAssoc:\n",
            "    Type: AWS::ECS::ClusterCapacityProviderAssociations\n",
            "    Properties:\n",
            "      Cluster: !Ref Cluster\n",
            "      CapacityProviders:\n",
            "        - FARGATE\n",
            "      DefaultCapacityProviderStrategy:\n",
            "        - CapacityProvider: FARGATE\n",
            "          Weight: 1\n",
            "          Base: 0\n",
            "Outputs:\n",
            "  ClusterName:\n",
            "    Value: !Ref Cluster\n",
            "  AssocRef:\n",
            "    Value: !Ref CPAssoc\n",
        ),
        capacity_providers: &["FARGATE"],
        default_strategy: &[("FARGATE", 0, 1)],
    },
    Association {
        case: "two providers and a two-entry strategy with a base on one of them, declared in \
               YAML",
        stack: "cfn-cpassoc-two-providers",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::ECS::Cluster\n",
            "    Properties:\n",
            "      ClusterName: cfn-cpassoc-two\n",
            "  CPAssoc:\n",
            "    Type: AWS::ECS::ClusterCapacityProviderAssociations\n",
            "    Properties:\n",
            "      Cluster: !Ref Cluster\n",
            "      CapacityProviders:\n",
            "        - FARGATE\n",
            "        - FARGATE_SPOT\n",
            "      DefaultCapacityProviderStrategy:\n",
            "        - CapacityProvider: FARGATE\n",
            "          Base: 2\n",
            "          Weight: 1\n",
            "        - CapacityProvider: FARGATE_SPOT\n",
            "          Weight: 4\n",
            "Outputs:\n",
            "  ClusterName:\n",
            "    Value: !Ref Cluster\n",
            "  AssocRef:\n",
            "    Value: !Ref CPAssoc\n",
        ),
        capacity_providers: &["FARGATE", "FARGATE_SPOT"],
        default_strategy: &[("FARGATE", 2, 1), ("FARGATE_SPOT", 0, 4)],
    },
    Association {
        case: "a provider the same stack created, with NO default strategy at all, declared in \
               JSON",
        stack: "cfn-cpassoc-created-provider",
        template: r#"{
          "Resources": {
            "Cluster": {
              "Type": "AWS::ECS::Cluster",
              "Properties": {"ClusterName": "cfn-cpassoc-created"}
            },
            "Vpc": {"Type": "AWS::EC2::VPC", "Properties": {"CidrBlock": "10.79.0.0/16"}},
            "SubnetA": {
              "Type": "AWS::EC2::Subnet",
              "Properties": {
                "VpcId": {"Ref": "Vpc"},
                "CidrBlock": "10.79.1.0/24",
                "AvailabilityZone": "us-east-1a"
              }
            },
            "Lt": {
              "Type": "AWS::EC2::LaunchTemplate",
              "Properties": {
                "LaunchTemplateData": {
                  "ImageId": "ami-00000000000000079",
                  "InstanceType": "t3.micro"
                }
              }
            },
            "Asg": {
              "Type": "AWS::AutoScaling::AutoScalingGroup",
              "Properties": {
                "MinSize": "0",
                "MaxSize": "2",
                "VPCZoneIdentifier": [{"Ref": "SubnetA"}],
                "LaunchTemplate": {
                  "LaunchTemplateId": {"Ref": "Lt"},
                  "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]}
                }
              }
            },
            "Cap": {
              "Type": "AWS::ECS::CapacityProvider",
              "Properties": {
                "Name": "cfn-cpassoc-provider",
                "AutoScalingGroupProvider": {"AutoScalingGroupArn": {"Ref": "Asg"}}
              }
            },
            "CPAssoc": {
              "Type": "AWS::ECS::ClusterCapacityProviderAssociations",
              "Properties": {
                "Cluster": {"Ref": "Cluster"},
                "CapacityProviders": [{"Ref": "Cap"}],
                "DefaultCapacityProviderStrategy": []
              }
            }
          },
          "Outputs": {
            "ClusterName": {"Value": {"Ref": "Cluster"}},
            "AssocRef": {"Value": {"Ref": "CPAssoc"}}
          }
        }"#,
        capacity_providers: &["cfn-cpassoc-provider"],
        default_strategy: &[],
    },
];

/// One task-set deployment: what the template declares, and what Amazon ECS must then report
/// about the task set the stack claims to have created.
struct TaskSet {
    case: &'static str,
    stack: &'static str,
    /// The row's whole template.
    template: &'static str,
    /// The output whose value the task set's declaration named the service by, which is the
    /// middle part of the identifier `Ref` returns.
    service_output: &'static str,
    /// The `externalId` the task set must report, where the declaration states one.
    external_id: Option<&'static str>,
    /// The `scale` it must report, as (unit, value).
    scale: Option<(&'static str, f64)>,
    /// The tags it must carry.
    tags: &'static [(&'static str, &'static str)],
}

/// The cluster, the EXTERNAL-controller service and the task definition every YAML task-set row
/// declares. A service the EXTERNAL controller deploys names no task definition of its own —
/// "the service itself has none to name" — so the revision lives on the task set.
const SERVICE_YAML: &str = concat!(
    "Resources:\n",
    "  Cluster:\n",
    "    Type: AWS::ECS::Cluster\n",
    "    Properties:\n",
    "      ClusterName: cfn-taskset-cluster\n",
    "  Task:\n",
    "    Type: AWS::ECS::TaskDefinition\n",
    "    Properties:\n",
    "      Family: cfn-taskset-family\n",
    "      ContainerDefinitions:\n",
    "        - Name: app\n",
    "          Image: public.ecr.aws/nginx/nginx:latest\n",
    "          Memory: 256\n",
    "  Svc:\n",
    "    Type: AWS::ECS::Service\n",
    "    Properties:\n",
    "      ServiceName: cfn-taskset-service\n",
    "      Cluster: !Ref Cluster\n",
    "      DesiredCount: 2\n",
    "      DeploymentController:\n",
    "        Type: EXTERNAL\n",
);

/// The outputs every YAML task-set row publishes: the cluster and the service the task set was
/// declared against, the identifier `Ref` returns for the task set and the id `Fn::GetAtt`
/// reads off it.
const TASK_SET_OUTPUTS_YAML: &str = concat!(
    "Outputs:\n",
    "  ClusterName:\n",
    "    Value: !Ref Cluster\n",
    "  ServiceName:\n",
    "    Value: !GetAtt Svc.Name\n",
    "  ServiceRef:\n",
    "    Value: !Ref Svc\n",
    "  TaskSetRef:\n",
    "    Value: !Ref TaskSet\n",
    "  TaskSetId:\n",
    "    Value: !GetAtt TaskSet.Id\n",
);

const TASK_SETS: &[TaskSet] = &[
    TaskSet {
        case: "a task set with an external id, a scale and a tag, declared in YAML with the \
               service named by !GetAtt Svc.Name",
        stack: "cfn-taskset-scaled",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::ECS::Cluster\n",
            "    Properties:\n",
            "      ClusterName: cfn-taskset-cluster\n",
            "  Task:\n",
            "    Type: AWS::ECS::TaskDefinition\n",
            "    Properties:\n",
            "      Family: cfn-taskset-family\n",
            "      ContainerDefinitions:\n",
            "        - Name: app\n",
            "          Image: public.ecr.aws/nginx/nginx:latest\n",
            "          Memory: 256\n",
            "  Svc:\n",
            "    Type: AWS::ECS::Service\n",
            "    Properties:\n",
            "      ServiceName: cfn-taskset-service\n",
            "      Cluster: !Ref Cluster\n",
            "      DesiredCount: 2\n",
            "      DeploymentController:\n",
            "        Type: EXTERNAL\n",
            "  TaskSet:\n",
            "    Type: AWS::ECS::TaskSet\n",
            "    Properties:\n",
            "      Cluster: !Ref Cluster\n",
            "      Service: !GetAtt Svc.Name\n",
            "      TaskDefinition: !Ref Task\n",
            "      LaunchType: EC2\n",
            "      ExternalId: blue\n",
            "      Scale:\n",
            "        Unit: PERCENT\n",
            "        Value: 50\n",
            "      Tags:\n",
            "        - Key: Colour\n",
            "          Value: blue\n",
            "  Primary:\n",
            "    Type: AWS::ECS::PrimaryTaskSet\n",
            "    Properties:\n",
            "      Cluster: !Ref Cluster\n",
            "      Service: !GetAtt Svc.Name\n",
            "      TaskSetId: !GetAtt TaskSet.Id\n",
            "Outputs:\n",
            "  ClusterName:\n",
            "    Value: !Ref Cluster\n",
            "  ServiceName:\n",
            "    Value: !GetAtt Svc.Name\n",
            "  ServiceRef:\n",
            "    Value: !Ref Svc\n",
            "  TaskSetRef:\n",
            "    Value: !Ref TaskSet\n",
            "  TaskSetId:\n",
            "    Value: !GetAtt TaskSet.Id\n",
        ),
        service_output: "ServiceName",
        external_id: Some("blue"),
        scale: Some(("PERCENT", 50.0)),
        tags: &[("Colour", "blue")],
    },
    TaskSet {
        case: "a task set with no external id, no scale and no tags, declared in JSON with the \
               service named by its ARN",
        stack: "cfn-taskset-plain",
        template: r#"{
          "Resources": {
            "Cluster": {
              "Type": "AWS::ECS::Cluster",
              "Properties": {"ClusterName": "cfn-taskset-plain-cluster"}
            },
            "Task": {
              "Type": "AWS::ECS::TaskDefinition",
              "Properties": {
                "Family": "cfn-taskset-plain-family",
                "ContainerDefinitions": [
                  {"Name": "app", "Image": "public.ecr.aws/nginx/nginx:latest", "Memory": 256}
                ]
              }
            },
            "Svc": {
              "Type": "AWS::ECS::Service",
              "Properties": {
                "ServiceName": "cfn-taskset-plain-service",
                "Cluster": {"Ref": "Cluster"},
                "DeploymentController": {"Type": "EXTERNAL"}
              }
            },
            "TaskSet": {
              "Type": "AWS::ECS::TaskSet",
              "Properties": {
                "Cluster": {"Ref": "Cluster"},
                "Service": {"Ref": "Svc"},
                "TaskDefinition": {"Ref": "Task"},
                "LaunchType": "EC2"
              }
            },
            "Primary": {
              "Type": "AWS::ECS::PrimaryTaskSet",
              "Properties": {
                "Cluster": {"Ref": "Cluster"},
                "Service": {"Ref": "Svc"},
                "TaskSetId": {"Fn::GetAtt": ["TaskSet", "Id"]}
              }
            }
          },
          "Outputs": {
            "ClusterName": {"Value": {"Ref": "Cluster"}},
            "ServiceName": {"Value": {"Fn::GetAtt": ["Svc", "Name"]}},
            "ServiceRef": {"Value": {"Ref": "Svc"}},
            "TaskSetRef": {"Value": {"Ref": "TaskSet"}},
            "TaskSetId": {"Value": {"Fn::GetAtt": ["TaskSet", "Id"]}}
          }
        }"#,
        service_output: "ServiceRef",
        external_id: None,
        scale: None,
        tags: &[],
    },
];

/// Deploy a template and answer the status its create settled on, polled the way a client's own
/// waiter polls.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    settled_stack_status(case, router, REGION, stack).await
}

/// One output's `OutputValue`, which is where a client reads the identifier a declared resource
/// was created under.
async fn output(router: &MultiServiceRouter, case: &str, stack: &str, key: &str) -> String {
    let (status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
    );
    crate::harness::xml_blocks(&body, "member")
        .iter()
        .find(|entry| xml_text(entry, "OutputKey").as_deref() == Some(key))
        .and_then(|entry| xml_text(entry, "OutputValue"))
        .unwrap_or_else(|| panic!("case {case:?}: no {key:?} output with a value in {body}"))
}

/// Amazon ECS's own report of one cluster, with its attachments asked for: the capacity
/// providers it holds and the default strategy it places tasks with.
async fn described_cluster(router: &MultiServiceRouter, case: &str, cluster: &str) -> Value {
    let (status, answer) = ecs(
        router,
        "DescribeClusters",
        &json!({"clusters": [cluster], "include": ["ATTACHMENTS"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon ECS to answer for {cluster:?}, got {status} {answer}"
    );
    answer["clusters"][0].clone()
}

/// The capacity-provider configuration of a cluster as Amazon ECS reports it: the providers and
/// the default strategy, read in the one shape both the rows and the assertions compare.
fn configuration_of(cluster: &Value) -> (Vec<String>, Vec<(String, i64, i64)>) {
    let providers = cluster["capacityProviders"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|provider| provider.as_str().map(str::to_string))
        .collect();
    let strategy = cluster["defaultCapacityProviderStrategy"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["capacityProvider"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                entry["base"].as_i64().unwrap_or_default(),
                entry["weight"].as_i64().unwrap_or_default(),
            )
        })
        .collect();
    (providers, strategy)
}

/// RULE: the association a stack declares is the capacity-provider configuration Amazon ECS
/// reports for the cluster, and the handle the stack publishes for it IS that cluster.
#[tokio::test]
async fn a_declared_association_is_the_clusters_configuration_in_amazon_ecs() {
    for row in ASSOCIATIONS {
        let case = row.case;
        let router = router();
        let settled = deploy(&router, case, row.stack, row.template).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {settled:?} because {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        // The handle the stack publishes for the resource: "Ref returns the cluster name" —
        // the resource IS that cluster's configuration, so a reference to it resolves to a
        // value Amazon ECS takes as a cluster.
        let cluster = output(&router, case, row.stack, "ClusterName").await;
        let published = output(&router, case, row.stack, "AssocRef").await;
        assert_eq!(
            published, cluster,
            "case {case:?}: expected the association's Ref to be the cluster {cluster:?}, got \
             {published:?}"
        );

        // THE RULE: the providers and the default strategy the declaration states are the ones
        // Amazon ECS reports for that cluster.
        let described = described_cluster(&router, case, &published).await;
        let (providers, strategy) = configuration_of(&described);
        let expected_providers: Vec<String> = row
            .capacity_providers
            .iter()
            .map(|provider| (*provider).to_string())
            .collect();
        let expected_strategy: Vec<(String, i64, i64)> = row
            .default_strategy
            .iter()
            .map(|(provider, base, weight)| ((*provider).to_string(), *base, *weight))
            .collect();
        assert_eq!(
            (providers, strategy),
            (expected_providers.clone(), expected_strategy.clone()),
            "case {case:?}: expected the cluster to hold {expected_providers:?} with the \
             default strategy {expected_strategy:?}, got {described}"
        );
    }
}

/// RULE, read the other way: a default strategy naming a provider the association does not
/// carry is refused by Amazon ECS — "a capacity provider must be associated with the cluster to
/// be used in a capacity provider strategy" — so the stack rolls back and the cluster is left
/// exactly as it was.
#[tokio::test]
async fn a_strategy_over_an_unassociated_provider_leaves_the_cluster_alone() {
    let router = router();
    let case = "a default strategy naming a provider the associations list does not carry";
    let cluster_name = "cfn-cpassoc-unassociated-cluster";

    // The CONTROL: a cluster of its own stack, so what the refused association left behind is
    // read on a cluster that is still there rather than on one a rollback took away.
    let control = deploy(
        &router,
        case,
        "cfn-cpassoc-control",
        concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::ECS::Cluster\n",
            "    Properties:\n",
            "      ClusterName: cfn-cpassoc-unassociated-cluster\n",
        ),
    )
    .await;
    assert_eq!(
        control, "CREATE_COMPLETE",
        "case {case:?}: expected the control cluster's stack to complete, got {control:?} \
         because {:?}",
        stack_event_reasons(&router, REGION, "cfn-cpassoc-control").await
    );

    let stack = "cfn-cpassoc-unassociated";
    let template = concat!(
        "Resources:\n",
        "  CPAssoc:\n",
        "    Type: AWS::ECS::ClusterCapacityProviderAssociations\n",
        "    Properties:\n",
        "      Cluster: cfn-cpassoc-unassociated-cluster\n",
        "      CapacityProviders:\n",
        "        - FARGATE\n",
        "      DefaultCapacityProviderStrategy:\n",
        "        - CapacityProvider: FARGATE_SPOT\n",
        "          Weight: 1\n",
    );

    // "ROLLBACK — Roll back the stack creation. This is the default": a create whose only
    // resource failed settles there, with nothing left created.
    let settled = deploy(&router, case, stack, template).await;
    assert_eq!(
        settled, "ROLLBACK_COMPLETE",
        "case {case:?}: expected the stack to roll back, got {settled:?} because {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let reason = resource_event_reason(&router, stack, "CPAssoc", "CREATE_FAILED").await;
    assert!(
        reason.contains("capacity provider strategy"),
        "case {case:?}: expected the resource to carry Amazon ECS's own refusal of the \
         strategy, got {reason:?}"
    );

    // The cluster is left exactly as it was: a refused put writes no part of the
    // configuration it was asked for, so neither the provider the association DID carry nor
    // the strategy it was refused over is on it.
    let described = described_cluster(&router, case, cluster_name).await;
    let (providers, strategy) = configuration_of(&described);
    assert_eq!(
        (
            described["clusterName"].as_str(),
            described["status"].as_str(),
            providers,
            strategy,
        ),
        (Some(cluster_name), Some("ACTIVE"), Vec::new(), Vec::new()),
        "case {case:?}: expected the cluster to be left unmodified, got {described}"
    );
}

/// RULE: the task set a stack declares is one Amazon ECS holds, under the identifier it minted
/// — which is what the stack's `Ref` and `Fn::GetAtt Id` publish — and the task set an
/// `AWS::ECS::PrimaryTaskSet` names is the one that service reports serving production traffic.
#[tokio::test]
async fn a_declared_task_set_is_one_amazon_ecs_holds() {
    for row in TASK_SETS {
        let case = row.case;
        let router = router();
        let settled = deploy(&router, case, row.stack, row.template).await;
        assert_eq!(
            settled, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {settled:?} because {:?}",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        let cluster = output(&router, case, row.stack, "ClusterName").await;
        let service = output(&router, case, row.stack, "ServiceName").await;
        let declared_service = output(&router, case, row.stack, row.service_output).await;
        let published = output(&router, case, row.stack, "TaskSetRef").await;
        let published_id = output(&router, case, row.stack, "TaskSetId").await;

        // THE RULE: the task set the stack claims to have created is one Amazon ECS holds,
        // under the id THAT service minted — which is what `Fn::GetAtt Id` publishes.
        let (status, answer) = ecs(
            &router,
            "DescribeTaskSets",
            &json!({
                "cluster": cluster,
                "service": service,
                "taskSets": [published_id],
                "include": ["TAGS"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon ECS to answer for {published_id:?}, got {status} \
             {answer}"
        );
        let task_sets = answer["taskSets"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            (task_sets.len(), &answer["failures"]),
            (1, &json!([])),
            "case {case:?}: expected exactly one task set {published_id:?} and no failures, got \
             {answer}"
        );
        let task_set = &task_sets[0];
        assert_eq!(
            task_set["id"].as_str(),
            Some(published_id.as_str()),
            "case {case:?}: expected the published id to be the task set's own, got {task_set}"
        );

        // ...and the handle `Ref` publishes is the TRIPLE Amazon ECS addresses one by: the
        // cluster and the service the declaration named, and that id.
        let expected = format!("{cluster}|{declared_service}|{published_id}");
        assert_eq!(
            published, expected,
            "case {case:?}: expected the task set's Ref to be its Cluster|Service|Id, got \
             {published:?}"
        );

        // The configuration the declaration asked for is the task set's own, and the task set
        // the stack's `AWS::ECS::PrimaryTaskSet` named is the one serving production traffic.
        assert_eq!(
            (
                task_set["externalId"].as_str(),
                task_set["taskDefinition"].as_str().is_some(),
                task_set["launchType"].as_str(),
                task_set["status"].as_str(),
            ),
            (row.external_id, true, Some("EC2"), Some("PRIMARY")),
            "case {case:?}: expected the declared configuration on a PRIMARY task set, got \
             {task_set}"
        );
        if let Some((unit, value)) = row.scale {
            assert_eq!(
                (
                    task_set["scale"]["unit"].as_str(),
                    task_set["scale"]["value"].as_f64(),
                ),
                (Some(unit), Some(value)),
                "case {case:?}: expected the declared scale, got {task_set}"
            );
        }
        let reported_tags: Vec<(String, String)> = task_set["tags"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|tag| {
                Some((
                    tag["key"].as_str()?.to_string(),
                    tag["value"].as_str()?.to_string(),
                ))
            })
            .collect();
        for (key, value) in row.tags {
            assert!(
                reported_tags.contains(&((*key).to_string(), (*value).to_string())),
                "case {case:?}: expected the task set to carry the tag {key:?}={value:?}, got \
                 {reported_tags:?}"
            );
        }
    }
}

/// RULE at the end of the resource's life: the task set the stack created is deleted in Amazon
/// ECS when the stack is deleted, which is what makes it the stack's resource rather than a
/// leftover.
#[tokio::test]
async fn deleting_the_stack_deletes_the_task_set_it_created() {
    let router = router();
    let case = "a task set deleted with its stack";
    let stack = "cfn-taskset-deleted";
    let template = format!(
        "{SERVICE_YAML}{}{TASK_SET_OUTPUTS_YAML}",
        concat!(
            "  TaskSet:\n",
            "    Type: AWS::ECS::TaskSet\n",
            "    Properties:\n",
            "      Cluster: !Ref Cluster\n",
            "      Service: !GetAtt Svc.Name\n",
            "      TaskDefinition: !Ref Task\n",
            "      LaunchType: EC2\n",
            "  Primary:\n",
            "    Type: AWS::ECS::PrimaryTaskSet\n",
            "    Properties:\n",
            "      Cluster: !Ref Cluster\n",
            "      Service: !GetAtt Svc.Name\n",
            "      TaskSetId: !GetAtt TaskSet.Id\n",
        )
    );

    let settled = deploy(&router, case, stack, &template).await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {settled:?} because {:?}",
        stack_event_reasons(&router, REGION, stack).await
    );
    let cluster = output(&router, case, stack, "ClusterName").await;
    let service = output(&router, case, stack, "ServiceName").await;
    let id = output(&router, case, stack, "TaskSetId").await;

    let (status, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {body}"
    );

    // The task set is gone from Amazon ECS: that read either reports no task set under the id
    // at all, or is refused because the service or the cluster it lived in went with the stack
    // — judged on that service's own error CODE and on nothing else. Waited for as the
    // CONDITION, under a deadline generous enough that a loaded host cannot decide the outcome.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, answer) = ecs(
            &router,
            "DescribeTaskSets",
            &json!({"cluster": cluster, "service": service, "taskSets": [id]}),
        )
        .await;
        let refused_as_gone = status == 400
            && ["ClusterNotFoundException", "ServiceNotFoundException"].iter().any(|code| {
                answer["__type"]
                    .as_str()
                    .is_some_and(|reported| reported.ends_with(code))
            });
        let reported_none = status == 200
            && answer["taskSets"]
                .as_array()
                .map(|task_sets| task_sets.is_empty())
                .unwrap_or(true);
        if refused_as_gone || reported_none {
            break;
        }
        assert!(
            status == 200,
            "case {case:?}: expected Amazon ECS to answer the read or refuse it as a service \
             it no longer holds, got {status} with {answer}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected Amazon ECS to hold no task set {id:?}, last read {answer}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}
