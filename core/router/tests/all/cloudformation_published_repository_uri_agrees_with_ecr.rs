// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack PUBLISHES for a resource it created is what the OWNING
//! service reports for it.
//!
//! The rule under test: **an attribute value a deployment publishes for a resource it
//! created through the owning service's own API is the value that service answered with.**
//! Before a create answers, CloudFormation can only compute a stand-in for a value only the
//! owning service can produce; the deployment replaces that stand-in with what the create
//! really reported. A stack that published its own stand-in instead would hand a client a
//! value the owning service never minted, and the two would drift apart wherever the
//! service's own composition differs — which is exactly what the partitions below show.
//!
//! Amazon ECR's `Repository$repositoryUri` is read here because it is such a value: "The URI
//! for the repository. You can use this URI for container image `push` and `pull`
//! operations", which is the URI that registry publishes for the repository it holds. Two
//! crates have to agree for one of these templates to publish it —
//! CloudFormation makes the create and Amazon ECR serves it — so the suite lives here; a
//! crate-local test sees one side of that call only.
//!
//! Each row deploys one stack and reads the repository back through Amazon ECR's own
//! `DescribeRepositories`, then checks every value the stack published for it against that
//! read: the repository's name (which is what `Ref` resolves to, and the stack's physical
//! resource id), its ARN, and its URI however the template composed it. The rows differ
//! along what the rule generalises over: the Region — and so the partition — the stack
//! deploys in, whether the template NAMES the repository or leaves the name to
//! CloudFormation, whether the name carries a namespace path, how the output composes the
//! attribute (`Fn::GetAtt`, `Fn::Sub` over it, `Fn::Join` over it), and both template
//! spellings (JSON and YAML).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

// The reading of an announced repository URI is the sibling suite's, imported rather than
// re-derived, so the two cannot drift on what makes an address one this emulator answers at.
use crate::cloudformation_registry_backed_task_definition::announced_repository_path;

/// The `X-Amz-Target` prefix Amazon ECR's model fixes, and the signing name that routes a
/// request to it (`sigv4Name: ecr`, service shape `AmazonEC2ContainerRegistry_V20150921`,
/// awsJson1.1).
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// How a stack read waits for the deployment's clock-owed span to come due: look again, with
/// a deadline generous enough that a loaded host still reaches it.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 80;

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and which Region it is made in. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, in `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon ECR operation over its awsJson1.1 wire, in `region`.
async fn ecr(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecr.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECR_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ecr", region))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Read a stack until it reports a status no longer in progress, and answer that read's body.
async fn poll_to_terminal(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack: &str,
) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) = cloudformation(
            router,
            region,
            "DescribeStacks",
            &[("StackName", stack.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    described
}

/// The stack's own events, which is where the reason a failed deployment stopped is read.
async fn stack_events(router: &MultiServiceRouter, region: &str, stack: &str) -> String {
    let (_, body) = cloudformation(
        router,
        region,
        "DescribeStackEvents",
        &[("StackName", stack.to_string())],
    )
    .await;
    body
}

/// Create a stack from `template` and read it to a terminal status that must be
/// `CREATE_COMPLETE`. The answer is that read's body.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        region,
        "CreateStack",
        &[
            ("StackName", stack.to_string()),
            ("TemplateBody", template.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = poll_to_terminal(router, case, region, stack).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete; reason {:?}; events {}",
        opt_element(&described, "StackStatusReason"),
        stack_events(router, region, stack).await,
    );
    described
}

/// The stack's logical-id -> physical-id map, which is where a client reads the identifier a
/// declared resource was created under.
async fn stack_resources(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, body) = cloudformation(
        router,
        region,
        "DescribeStackResources",
        &[("StackName", stack.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    physical_ids_of(&body)
}

/// One deployment: the template, the Region it is deployed in, and what Amazon ECR must then
/// hold.
struct Row {
    case: &'static str,
    region: &'static str,
    stack: &'static str,
    template: &'static str,
    /// The logical id of the declared `AWS::ECR::Repository`.
    registry: &'static str,
    /// The repository name the template states, or `None` where CloudFormation generates it.
    repository_name: Option<&'static str>,
}

/// The outputs every template below publishes the repository's URI under, and how each
/// composes it from the same attribute. Every one of them is the value Amazon ECR reports,
/// because a template function resolves the attribute the deployment re-pointed.
const URI_OUTPUTS: [&str; 3] = ["UriThroughGetAtt", "UriThroughSub", "UriThroughJoin"];

const ROWS: &[Row] = &[
    Row {
        case: "a named repository in us-east-1, published in JSON",
        region: "us-east-1",
        stack: "published-registry-uri",
        template: r#"{
          "Resources": {
            "Registry": {
              "Type": "AWS::ECR::Repository",
              "Properties": { "RepositoryName": "published-registry-uri" }
            }
          },
          "Outputs": {
            "RepositoryName": { "Value": { "Ref": "Registry" } },
            "RepositoryArn": { "Value": { "Fn::GetAtt": ["Registry", "Arn"] } },
            "UriThroughGetAtt": { "Value": { "Fn::GetAtt": ["Registry", "RepositoryUri"] } },
            "UriThroughSub": { "Value": { "Fn::Sub": "${Registry.RepositoryUri}" } },
            "UriThroughJoin": {
              "Value": {
                "Fn::Join": ["", [{ "Fn::GetAtt": ["Registry", "RepositoryUri"] }]]
              }
            }
          }
        }"#,
        registry: "Registry",
        repository_name: Some("published-registry-uri"),
    },
    Row {
        case: "a repository CloudFormation names itself, in eu-west-1",
        region: "eu-west-1",
        stack: "generated-registry-uri",
        template: r#"{
          "Resources": {
            "Registry": { "Type": "AWS::ECR::Repository" }
          },
          "Outputs": {
            "RepositoryName": { "Value": { "Ref": "Registry" } },
            "RepositoryArn": { "Value": { "Fn::GetAtt": ["Registry", "Arn"] } },
            "UriThroughGetAtt": { "Value": { "Fn::GetAtt": ["Registry", "RepositoryUri"] } },
            "UriThroughSub": { "Value": { "Fn::Sub": "${Registry.RepositoryUri}" } },
            "UriThroughJoin": {
              "Value": {
                "Fn::Join": ["", [{ "Fn::GetAtt": ["Registry", "RepositoryUri"] }]]
              }
            }
          }
        }"#,
        registry: "Registry",
        repository_name: None,
    },
    Row {
        // A repository name may carry a namespace path ("A repository name must start with a
        // letter and can only contain lowercase letters, numbers, hyphens, underscores, and
        // forward slashes"), which the URI appends to the registry host as it stands.
        case: "a namespaced repository in ap-southeast-2, published in YAML",
        region: "ap-southeast-2",
        stack: "namespaced-registry-uri",
        template: concat!(
            "Resources:\n",
            "  Registry:\n",
            "    Type: AWS::ECR::Repository\n",
            "    Properties:\n",
            "      RepositoryName: team/namespaced-registry-uri\n",
            "Outputs:\n",
            "  RepositoryName:\n",
            "    Value: !Ref Registry\n",
            "  RepositoryArn:\n",
            "    Value: !GetAtt Registry.Arn\n",
            "  UriThroughGetAtt:\n",
            "    Value: !GetAtt Registry.RepositoryUri\n",
            "  UriThroughSub:\n",
            "    Value:\n",
            "      Fn::Sub: \"${Registry.RepositoryUri}\"\n",
            "  UriThroughJoin:\n",
            "    Value:\n",
            "      Fn::Join:\n",
            "        - \"\"\n",
            "        - - !GetAtt Registry.RepositoryUri\n",
        ),
        registry: "Registry",
        repository_name: Some("team/namespaced-registry-uri"),
    },
    Row {
        // GovCloud: a Region whose endpoints AWS publishes under the commercial suffix, and
        // whose ARNs carry their own partition.
        case: "a named repository in us-gov-west-1, published in YAML",
        region: "us-gov-west-1",
        stack: "govcloud-registry-uri",
        template: concat!(
            "Resources:\n",
            "  Registry:\n",
            "    Type: AWS::ECR::Repository\n",
            "    Properties:\n",
            "      RepositoryName: govcloud-registry-uri\n",
            "Outputs:\n",
            "  RepositoryName:\n",
            "    Value: !Ref Registry\n",
            "  RepositoryArn:\n",
            "    Value: !GetAtt Registry.Arn\n",
            "  UriThroughGetAtt:\n",
            "    Value: !GetAtt Registry.RepositoryUri\n",
            "  UriThroughSub:\n",
            "    Value:\n",
            "      Fn::Sub: \"${Registry.RepositoryUri}\"\n",
            "  UriThroughJoin:\n",
            "    Value:\n",
            "      Fn::Join:\n",
            "        - \"\"\n",
            "        - - !GetAtt Registry.RepositoryUri\n",
        ),
        registry: "Registry",
        repository_name: Some("govcloud-registry-uri"),
    },
    Row {
        // China: the one partition whose endpoint suffix is not `amazonaws.com`, so the host
        // CloudFormation could spell for the attribute differs from every row above.
        case: "a repository CloudFormation names itself, in cn-north-1",
        region: "cn-north-1",
        stack: "china-registry-uri",
        template: r#"{
          "Resources": {
            "Registry": { "Type": "AWS::ECR::Repository" }
          },
          "Outputs": {
            "RepositoryName": { "Value": { "Ref": "Registry" } },
            "RepositoryArn": { "Value": { "Fn::GetAtt": ["Registry", "Arn"] } },
            "UriThroughGetAtt": { "Value": { "Fn::GetAtt": ["Registry", "RepositoryUri"] } },
            "UriThroughSub": { "Value": { "Fn::Sub": "${Registry.RepositoryUri}" } },
            "UriThroughJoin": {
              "Value": {
                "Fn::Join": ["", [{ "Fn::GetAtt": ["Registry", "RepositoryUri"] }]]
              }
            }
          }
        }"#,
        registry: "Registry",
        repository_name: None,
    },
];

/// Every row's repository exists in Amazon ECR, and every value the stack published for it —
/// its name, its ARN and its URI however the template composed it — is the one that service
/// reports.
#[tokio::test]
async fn a_stack_publishes_the_repository_values_amazon_ecr_itself_reports() {
    let router = build_router();
    for row in ROWS {
        let described = deploy(&router, row.case, row.region, row.stack, row.template).await;
        let outputs = outputs_of(&described);
        let resources = stack_resources(&router, row.case, row.region, row.stack).await;

        // The repository's name: what the template stated, or what CloudFormation generated
        // for the resource — which is also the physical resource id, since "Ref returns the
        // repository name".
        let repository = outputs.get("RepositoryName").cloned().unwrap_or_else(|| {
            panic!(
                "case {:?}: no RepositoryName output with a value, got {outputs:?}",
                row.case
            )
        });
        match row.repository_name {
            Some(named) => assert_eq!(
                repository, named,
                "case {:?}: expected the declared repository name, got {repository:?}",
                row.case
            ),
            None => {
                let prefix = format!("{}-{}-", row.stack, row.registry.to_ascii_lowercase());
                assert!(
                    repository.starts_with(&prefix),
                    "case {:?}: expected a generated repository name starting {prefix:?}, got \
                     {repository:?}",
                    row.case
                );
            }
        }
        assert_eq!(
            resources.get(row.registry).map(String::as_str),
            Some(repository.as_str()),
            "case {:?}: expected the stack resource to report the repository it created, got \
             {resources:?}",
            row.case
        );

        // Amazon ECR's own read: the repository the stack says it created is there, under the
        // identifier the stack published.
        let (status, reported) = ecr(
            &router,
            row.region,
            "DescribeRepositories",
            &json!({ "repositoryNames": [repository.clone()] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected ecr DescribeRepositories on {repository:?} to answer 200, got \
             {status} with body {reported}",
            row.case
        );
        let held = &reported["repositories"][0];
        assert_eq!(
            held["repositoryName"].as_str(),
            Some(repository.as_str()),
            "case {:?}: expected the repository the stack created, got {reported}",
            row.case
        );

        // The URI that service reports is the address it publishes the repository at, with
        // the repository's own name as its path.
        let held_uri = held["repositoryUri"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            announced_repository_path(&held_uri),
            Some(repository.as_str()),
            "case {:?}: expected Amazon ECR to report an address it answers registry requests \
             at followed by {repository:?}, got {held_uri:?}",
            row.case
        );
        assert_eq!(
            held["registryId"].as_str(),
            Some(ACCOUNT),
            "case {:?}: expected the owning account beside the URI, got {reported}",
            row.case
        );

        // Every value the stack published for that attribute is the value the create
        // answered, however the output composed it — not a stand-in CloudFormation spelled
        // for itself, which in a partition of its own would name another host entirely.
        for output in URI_OUTPUTS {
            let published = outputs.get(output).cloned().unwrap_or_default();
            assert_eq!(
                published, held_uri,
                "case {:?}: expected the stack's {output} output to be the URI Amazon ECR \
                 reports ({held_uri:?}), got {published:?}",
                row.case
            );
        }

        // The ARN is the value an address is not: a LABEL, naming the account and the
        // Region. The stack publishes what the create answered for it too, and it names the
        // Region the stack was deployed in.
        let held_arn = held["repositoryArn"].as_str().unwrap_or_default().to_string();
        let published_arn = outputs.get("RepositoryArn").cloned().unwrap_or_default();
        assert_eq!(
            published_arn, held_arn,
            "case {:?}: expected the stack to publish the ARN Amazon ECR reports ({held_arn:?}), \
             got {published_arn:?}",
            row.case
        );
        assert!(
            held_arn.contains(&format!(":{}:", row.region))
                && held_arn.contains(&format!(":{ACCOUNT}:")),
            "case {:?}: expected the repository ARN to name this account and the Region the \
             stack deployed in, got {held_arn:?}",
            row.case
        );
    }
}
