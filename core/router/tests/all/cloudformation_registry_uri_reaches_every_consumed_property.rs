// SPDX-License-Identifier: CC0-1.0

//! The address Amazon ECR assigned a stack's repository reaches EVERY property that consumes
//! it, not just the container image.
//!
//! The rule under test: **a value only the owning service can assign is resolved to what that
//! service really assigned in every property of every resource that consumes it — whatever
//! position that property value stands in, and however the declaration composed it.**
//! CloudFormation answers a reference to such an attribute with a stand-in while the owning
//! service has not been asked yet, and the deployment re-points that stand-in once the create
//! has answered. A re-pointing that reached only the one member the symptom was first seen on
//! would leave every other property of the same resource holding a host nothing here answers
//! for: the environment variable an application reads its registry from, the label an
//! operator greps for, the argument a command line passes to a pull.
//!
//! Amazon ECR owns the address (`Repository$repositoryUri`: "The URI for the repository. You
//! can use this URI for container image `push` and `pull` operations"), CloudFormation
//! publishes it as that resource type's `RepositoryUri` attribute, and Amazon ECS stores
//! whatever the registration carried — so three crates have to agree for one of these
//! templates to deploy, which is why the suite lives here. The router, the deploy-and-poll and
//! each service's wire call are the sibling
//! [`super::cloudformation_registry_backed_task_definition`]'s, which reads this rule on the
//! image member; this one reads it on the positions beside it.
//!
//! The rows differ along what the rule generalises over: the property POSITION the URI is
//! consumed in (a scalar member, a value inside a list of structures, a value of a data map
//! whose keys are the caller's own, an element of a string list, a run inside a longer
//! sentence), how the declaration composes it (`Fn::GetAtt`, `Fn::Sub`, `Fn::Join`, and the
//! registry host spelled out of the pseudo parameters and the repository's name — the
//! spelling that IS the stand-in), whether the template names the repository, whether that
//! name carries a path segment, and both template spellings (JSON and YAML).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudformation_registry_backed_task_definition::{
    build_router, created_repository_name, deploy, ecr, ecs, published_repository_uri,
};
use super::query_wire::outputs_of;

/// The registry host AWS itself serves a private repository at
/// (`aws_account_id.dkr.ecr.region.amazonaws.com`, as `ContainerDefinition$image` spells it in
/// its own examples) — what every stand-in for a `RepositoryUri` carries, and so what no value
/// a deployed resource holds may still carry here.
///
/// The address Amazon ECR ASSIGNS here carries neither mark: a repository URI is an ADDRESS a
/// container client dials, so what this registry assigns is the endpoint this emulator answers
/// on ([`published_repository_uri`]). A value still carrying both marks is therefore a
/// stand-in the deployment never re-pointed.
const AWS_REGISTRY_LABELS_MARK: &str = ".dkr.ecr.";
const AWS_REGISTRY_ZONE_MARK: &str = ".amazonaws.com";

/// Whether `text` still carries a registry host in AWS's OWN zone, which is what a stand-in
/// left behind looks like (see [`AWS_REGISTRY_LABELS_MARK`]).
fn names_the_aws_registry_host(text: &str) -> bool {
    text.contains(AWS_REGISTRY_LABELS_MARK) && text.contains(AWS_REGISTRY_ZONE_MARK)
}

/// The container every row declares, and the name of the environment variable and the docker
/// label it carries the registry address in.
const CONTAINER: &str = "worker";
const REGISTRY_VARIABLE: &str = "REGISTRY";
const REGISTRY_LABEL: &str = "registry";

/// One deployment: the template, and what each property that consumes the repository's address
/// must hold afterwards. Each expectation is a template over `{uri}`, which is the address
/// Amazon ECR assigned the repository the stack created ([`published_repository_uri`]) — so a
/// row states the SHAPE of the value it expects and never a host of its own.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    /// The logical id of the declared `AWS::ECR::Repository`.
    registry: &'static str,
    /// The repository name the template states, or `None` where CloudFormation generates it.
    repository_name: Option<&'static str>,
    /// The family the declared revision belongs to, which is how the revision is read back.
    family: &'static str,
    /// `ContainerDefinition$image`: a scalar member.
    image: &'static str,
    /// The value of the `REGISTRY` entry of `ContainerDefinition$environment`: a member of a
    /// structure inside a list.
    environment: &'static str,
    /// The value of the `registry` entry of `ContainerDefinition$dockerLabels`: a value of a
    /// map whose keys are the caller's own data.
    label: &'static str,
    /// The first element of `ContainerDefinition$command`: an element of a string list, and in
    /// every row a longer sentence the address stands inside.
    command: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a named repository whose address is consumed through Fn::GetAtt and Fn::Sub, in \
               a scalar, a list of structures, a data map and a sentence",
        stack: "registry-uri-consumers",
        template: r#"{
          "Resources": {
            "Registry": {
              "Type": "AWS::ECR::Repository",
              "Properties": { "RepositoryName": "registry-uri-consumers" }
            },
            "Worker": {
              "Type": "AWS::ECS::TaskDefinition",
              "DependsOn": "Registry",
              "Properties": {
                "Family": "registry-uri-consumers",
                "NetworkMode": "bridge",
                "ContainerDefinitions": [{
                  "Name": "worker",
                  "Memory": 256,
                  "Essential": true,
                  "Image": { "Fn::Sub": "${Registry.RepositoryUri}:latest" },
                  "Environment": [{
                    "Name": "REGISTRY",
                    "Value": { "Fn::GetAtt": ["Registry", "RepositoryUri"] }
                  }],
                  "DockerLabels": {
                    "registry": { "Fn::GetAtt": ["Registry", "RepositoryUri"] }
                  },
                  "Command": [{ "Fn::Sub": "pull ${Registry.RepositoryUri}:latest now" }]
                }]
              }
            }
          },
          "Outputs": {
            "RepositoryUri": { "Value": { "Fn::GetAtt": ["Registry", "RepositoryUri"] } },
            "RepositoryName": { "Value": { "Ref": "Registry" } }
          }
        }"#,
        registry: "Registry",
        repository_name: Some("registry-uri-consumers"),
        family: "registry-uri-consumers",
        image: "{uri}:latest",
        environment: "{uri}",
        label: "{uri}",
        command: "pull {uri}:latest now",
    },
    Row {
        case: "a repository CloudFormation names, whose address is consumed through Fn::Join \
               and extended with a path, in YAML",
        stack: "registry-uri-joined",
        template: concat!(
            "Resources:\n",
            "  Registry:\n",
            "    Type: AWS::ECR::Repository\n",
            "    Properties:\n",
            "      ImageScanningConfiguration:\n",
            "        ScanOnPush: false\n",
            "  Worker:\n",
            "    Type: AWS::ECS::TaskDefinition\n",
            "    Properties:\n",
            "      Family: registry-uri-joined\n",
            "      NetworkMode: bridge\n",
            "      ContainerDefinitions:\n",
            "        - Name: worker\n",
            "          Memory: 256\n",
            "          Essential: true\n",
            "          Image:\n",
            "            Fn::Join:\n",
            "              - \"\"\n",
            "              - - !GetAtt Registry.RepositoryUri\n",
            "                - \":2026-02\"\n",
            "          Environment:\n",
            "            - Name: REGISTRY\n",
            "              Value:\n",
            "                Fn::Join:\n",
            "                  - \"/\"\n",
            "                  - - !GetAtt Registry.RepositoryUri\n",
            "                    - manifests\n",
            "          DockerLabels:\n",
            "            registry: !GetAtt Registry.RepositoryUri\n",
            "          Command:\n",
            "            - Fn::Join:\n",
            "                - \" \"\n",
            "                - - pull\n",
            "                  - !GetAtt Registry.RepositoryUri\n",
            "                  - first\n",
            "Outputs:\n",
            "  RepositoryUri:\n",
            "    Value: !GetAtt Registry.RepositoryUri\n",
            "  RepositoryName:\n",
            "    Value: !Ref Registry\n",
        ),
        registry: "Registry",
        repository_name: None,
        family: "registry-uri-joined",
        image: "{uri}:2026-02",
        environment: "{uri}/manifests",
        label: "{uri}",
        command: "pull {uri} first",
    },
    Row {
        case: "a path-bearing repository name whose address every property spells out of the \
               pseudo parameters, which is the stand-in itself",
        stack: "registry-uri-spelled",
        template: r#"{
          "Resources": {
            "Registry": {
              "Type": "AWS::ECR::Repository",
              "Properties": { "RepositoryName": "team/registry-uri-spelled" }
            },
            "Worker": {
              "Type": "AWS::ECS::TaskDefinition",
              "DependsOn": "Registry",
              "Properties": {
                "Family": "registry-uri-spelled",
                "NetworkMode": "bridge",
                "ContainerDefinitions": [{
                  "Name": "worker",
                  "Memory": 256,
                  "Essential": true,
                  "Image": {
                    "Fn::Sub": "${AWS::AccountId}.dkr.ecr.${AWS::Region}.amazonaws.com/${Registry}:3.1"
                  },
                  "Environment": [{
                    "Name": "REGISTRY",
                    "Value": {
                      "Fn::Sub": "${AWS::AccountId}.dkr.ecr.${AWS::Region}.amazonaws.com/${Registry}"
                    }
                  }],
                  "DockerLabels": {
                    "registry": {
                      "Fn::Sub": "${AWS::AccountId}.dkr.ecr.${AWS::Region}.amazonaws.com/${Registry}"
                    }
                  },
                  "Command": [{
                    "Fn::Sub": "pull ${AWS::AccountId}.dkr.ecr.${AWS::Region}.amazonaws.com/${Registry} first"
                  }]
                }]
              }
            }
          },
          "Outputs": {
            "RepositoryUri": { "Value": { "Fn::GetAtt": ["Registry", "RepositoryUri"] } },
            "RepositoryName": { "Value": { "Ref": "Registry" } }
          }
        }"#,
        registry: "Registry",
        repository_name: Some("team/registry-uri-spelled"),
        family: "registry-uri-spelled",
        image: "{uri}:3.1",
        environment: "{uri}",
        label: "{uri}",
        command: "pull {uri} first",
    },
];

/// One expectation with the address Amazon ECR assigned put in place of `{uri}`.
fn filled(expectation: &str, uri: &str) -> String {
    expectation.replace("{uri}", uri)
}

/// Every string anywhere inside `value`, so an assertion can be made about the WHOLE document
/// rather than about the members a row happens to name.
fn strings(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) => found.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, found)),
        Value::Object(members) => members.values().for_each(|member| strings(member, found)),
        _ => {}
    }
}

/// The container `CONTAINER` of the revision `family`, read back through Amazon ECS's own
/// `DescribeTaskDefinition`, with the whole answer beside it for a report to name.
async fn registered_container(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
) -> (Value, Value) {
    let (status, reported) = ecs(
        router,
        "DescribeTaskDefinition",
        &json!({ "taskDefinition": family }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs DescribeTaskDefinition on {family:?} to answer 200, got \
         {status} with body {reported}"
    );
    let container = reported["taskDefinition"]["containerDefinitions"]
        .as_array()
        .and_then(|containers| {
            containers
                .iter()
                .find(|container| container["name"].as_str() == Some(CONTAINER))
        })
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a container named {CONTAINER:?}, got {reported}")
        });
    (container, reported)
}

/// The value of the entry `name` of a `[{name, value}]` list, as
/// `ContainerDefinition$environment` is shaped.
fn pair_value(pairs: &Value, name: &str) -> Option<String> {
    pairs.as_array()?.iter().find_map(|pair| {
        if pair["name"].as_str() != Some(name) {
            return None;
        }
        pair["value"].as_str().map(str::to_string)
    })
}

#[tokio::test]
async fn every_property_that_consumes_the_repository_holds_the_address_ecr_assigned() {
    let router = build_router();
    for row in ROWS {
        let described = deploy(&router, row.case, row.stack, row.template).await;
        let outputs = outputs_of(&described);
        // The name the stack created the repository under, from the sibling's reader of that
        // rule: the declared name, or the one CloudFormation generated for the resource.
        let repository = created_repository_name(
            row.case,
            row.stack,
            row.registry,
            row.repository_name,
            &outputs,
        );

        // The address the OWNING service assigned, read from Amazon ECR itself: the one value
        // every expectation below is stated against, and the one the stack's own output for
        // that attribute must already agree with.
        let (status, reported) = ecr(
            &router,
            "DescribeRepositories",
            &json!({ "repositoryNames": [repository] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected ecr DescribeRepositories on {repository:?} to answer 200, got \
             {status} with body {reported}",
            row.case
        );
        let assigned = reported["repositories"][0]["repositoryUri"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            assigned,
            published_repository_uri(&repository),
            "case {:?}: expected Amazon ECR to publish the repository at the address this \
             emulator serves its registry at, got {reported}",
            row.case
        );
        assert_eq!(
            outputs.get("RepositoryUri").map(String::as_str),
            Some(assigned.as_str()),
            "case {:?}: expected the stack's RepositoryUri output to be the address Amazon ECR \
             assigned, got {outputs:?}",
            row.case
        );

        // Every property that consumed that address, whatever position it stands in.
        let (container, answer) = registered_container(&router, row.case, row.family).await;
        for (position, expected, actual) in [
            (
                "the image member",
                filled(row.image, &assigned),
                container["image"].as_str().map(str::to_string),
            ),
            (
                "the environment variable",
                filled(row.environment, &assigned),
                pair_value(&container["environment"], REGISTRY_VARIABLE),
            ),
            (
                "the docker label",
                filled(row.label, &assigned),
                container["dockerLabels"][REGISTRY_LABEL]
                    .as_str()
                    .map(str::to_string),
            ),
            (
                "the command line",
                filled(row.command, &assigned),
                container["command"][0].as_str().map(str::to_string),
            ),
        ] {
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "case {:?}: expected {position} to hold {expected:?}, got {actual:?} in {answer}",
                row.case
            );
        }

        // And nothing the revision holds still names the AWS registry host: a stand-in left
        // in any position is an address no client of this emulator can reach, so the whole
        // answer is read rather than only the members named above.
        let mut held = Vec::new();
        strings(&answer, &mut held);
        let standing: Vec<&String> = held
            .iter()
            .filter(|text| names_the_aws_registry_host(text))
            .collect();
        assert!(
            standing.is_empty(),
            "case {:?}: expected no value of the registered revision to still name a registry \
             host in AWS's own zone ({AWS_REGISTRY_LABELS_MARK:?} under \
             {AWS_REGISTRY_ZONE_MARK:?}), got {standing:?}",
            row.case
        );
    }
}
