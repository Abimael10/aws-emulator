// SPDX-License-Identifier: CC0-1.0

//! The address Amazon ECR publishes for a repository is a REGISTRY authority a reader can
//! resolve the owning account and the Region out of — wherever a client picks that address
//! up, including off an artifact another service derived from it.
//!
//! The rule under test: **the URI Amazon ECR reports for a repository carries the registry
//! host layout that registry publishes — the owning account's 12-digit id, the two fixed
//! labels `dkr` and `ecr` and the registry's Region as the first four DNS labels, then at
//! least one further label for the zone the registry is served in — followed by the
//! repository's own name as the path, and every value another service derives from that URI
//! carries the same authority.** `Repository$repositoryUri` is "The URI for the repository.
//! You can use this URI for container image `push` and `pull` operations", so it is read two
//! ways at once: a container client dials it, and a client PARSES it for the registry that
//! holds the repository, which is what lets it recover the account and the Region and build
//! an `ecr:` ARN, a `docker login` target or a compute service's image reference from the one
//! value it was handed. An address that carried no such labels — a bare `host:port`, say —
//! leaves every one of those readings with nothing to parse, and the value changes with
//! whatever socket the emulator happens to be reached on.
//!
//! Three crates have to agree for one of these deployments to publish that address:
//! CloudFormation makes the create and publishes the repository's `RepositoryUri` attribute,
//! Amazon ECR assigns and reports the URI, and Amazon ECS stores whatever the registration
//! carried in `ContainerDefinition$image`. So the suite lives here — a crate-local test sees
//! one side of that call only — and each row reads the same address back through all three:
//! the stack's own output, Amazon ECR's `DescribeRepositories`, and the image of the revision
//! Amazon ECS registered.
//!
//! The rows differ along what the rule generalises over: the Region (and so the partition)
//! the stack deploys in, the address this emulator is SERVED at — an IPv4 socket, an IPv6
//! socket, a host an operator serves it at, and one carrying no port — which is the zone and
//! port the published authority is anchored on, whether the template NAMES the repository or
//! leaves the name to CloudFormation, whether that name carries a namespace path, and how the
//! declaration derives the container image from the attribute (`Fn::Sub` over it, `Fn::Join`
//! over it, and the bare `Fn::GetAtt`, which carries no tag at all).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The account id this emulator mints every identity and every registry id from, which is the
/// first label of a registry host it publishes.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The two fixed labels an Amazon ECR registry host carries between the owning account's id
/// and the registry's Region, as `AuthorizationData$proxyEndpoint` spells the registry URL
/// format (`https://aws_account_id.dkr.ecr.region.amazonaws.com`). Spelled out here rather
/// than imported, so a wrong label or a wrong order in the implementation fails these rows.
const REGISTRY_LABELS: [&str; 2] = ["dkr", "ecr"];

/// The `X-Amz-Target` prefix each service's model fixes (Amazon ECR: `sigv4Name: ecr`, service
/// shape `AmazonEC2ContainerRegistry_V20150921`; Amazon ECS: `sigv4Name: ecs`, service shape
/// `AmazonEC2ContainerServiceV20141113`). Both speak awsJson1.1.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// How a stack read waits for the deployment's clock-owed span to come due: look again, with a
/// deadline generous enough that a loaded host still reaches it.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 120;

/// The emulator with the three services a row exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them, SERVED at `served_base` — which is the address
/// `OperationContext::endpoint` reports, and so the zone and the port every address these
/// services announce is anchored on. Request authentication is off, as in every other suite
/// here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router(served_base: &str) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.set_url_base(
        EndpointUrl::parse(served_base)
            .unwrap_or_else(|| panic!("the base URL {served_base} must parse")),
    );
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and which Region it is made in. The signature is never checked:
/// this router runs with request authentication off.
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

/// Call one awsJson1.1 operation of the service `signing_name` routes to, in `region`.
async fn json_rpc(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name, region))
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

/// Create a stack from `template` and read it to a terminal status that must be
/// `CREATE_COMPLETE`. The answer is the outputs that read published.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack: &str,
    template: &str,
) -> BTreeMap<String, String> {
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
    let (_, events) = cloudformation(
        router,
        region,
        "DescribeStackEvents",
        &[("StackName", stack.to_string())],
    )
    .await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete; reason {:?}; events {events}",
        opt_element(&described, "StackStatusReason"),
    );
    outputs_of(&described)
}

/// How a declaration derives the container image from the repository's `RepositoryUri`
/// attribute: every form resolves to the address Amazon ECR assigned, so the image the
/// registered revision carries names that registry.
#[derive(Debug, Clone, Copy)]
enum Derived {
    /// `Fn::Sub` over the attribute, with a tag appended.
    Sub(&'static str),
    /// `Fn::Join` over the attribute, with a tag appended.
    Join(&'static str),
    /// The bare attribute, which carries no tag at all.
    GetAtt,
}

impl Derived {
    /// The `Image` member a declaration states for this derivation.
    fn image(self, registry: &str) -> Value {
        match self {
            Self::Sub(tag) => json!({ "Fn::Sub": format!("${{{registry}.RepositoryUri}}:{tag}") }),
            Self::Join(tag) => json!({
                "Fn::Join": ["", [{ "Fn::GetAtt": [registry, "RepositoryUri"] }, format!(":{tag}")]]
            }),
            Self::GetAtt => json!({ "Fn::GetAtt": [registry, "RepositoryUri"] }),
        }
    }

    /// What this derivation appends to the address it resolved: the tag it states, or nothing
    /// at all for the bare attribute.
    fn tag_suffix(self) -> String {
        match self {
            Self::Sub(tag) | Self::Join(tag) => format!(":{tag}"),
            Self::GetAtt => String::new(),
        }
    }

    /// The image that derivation must resolve to, given the address `uri` Amazon ECR assigned.
    fn resolved(self, uri: &str) -> String {
        format!("{uri}{}", self.tag_suffix())
    }
}

/// One deployment: where the emulator is served, the Region the stack is made in, what the
/// template declares, and the zone and port the published registry authority must therefore
/// carry.
struct Row {
    case: &'static str,
    /// The address this emulator is SERVED at for this row.
    served_base: &'static str,
    /// The DNS zone the registry authority announced to a caller of that emulator is served
    /// in: the zone of the served address itself, and the loopback zone RFC 6761 reserves for
    /// the interface where that address is an IP ADDRESS and so names no zone. Spelled out
    /// here rather than imported, so a wrong zone in the implementation fails these rows.
    zone: &'static str,
    /// The port that address carries, which the announced authority carries too, or `None`
    /// for a served address naming no port.
    port: Option<&'static str>,
    region: &'static str,
    stack: &'static str,
    /// The repository name the template states, or `None` where CloudFormation generates it.
    repository_name: Option<&'static str>,
    /// The family the declared revision belongs to, which is how it is read back.
    family: &'static str,
    /// How the declaration derives the container image from the repository's address.
    derived: Derived,
}

const ROWS: &[Row] = &[
    Row {
        case: "a named repository and an Fn::Sub image, served on an IPv4 socket",
        served_base: "https://127.0.0.1:36063",
        zone: "localhost",
        port: Some("36063"),
        region: "us-west-2",
        stack: "registry-authority-ipv4",
        repository_name: Some("registry-authority-worker"),
        family: "registry-authority-worker",
        derived: Derived::Sub("latest"),
    },
    Row {
        case: "a namespaced repository and an Fn::Join image, in another partition's Region, \
               served on an IPv6 socket",
        served_base: "https://[::1]:9443",
        zone: "localhost",
        port: Some("9443"),
        region: "cn-north-1",
        stack: "registry-authority-ipv6",
        repository_name: Some("team/registry-authority-worker"),
        family: "registry-authority-namespaced",
        derived: Derived::Join("2026-01"),
    },
    Row {
        case: "a repository CloudFormation names and an Fn::Sub image, served at a host an \
               operator chose",
        served_base: "https://registry.emulator.test:8443",
        zone: "registry.emulator.test",
        port: Some("8443"),
        region: "eu-central-1",
        stack: "registry-authority-named-host",
        repository_name: None,
        family: "registry-authority-generated",
        derived: Derived::Sub("1.0"),
    },
    Row {
        case: "a named repository and the bare attribute as the image, served at a host \
               carrying no port",
        served_base: "https://registry.emulator.test",
        zone: "registry.emulator.test",
        port: None,
        region: "us-east-1",
        stack: "registry-authority-no-port",
        repository_name: Some("registry-authority-untagged"),
        family: "registry-authority-untagged",
        derived: Derived::GetAtt,
    },
];

/// The template a row deploys: the repository, the revision whose image is derived from the
/// repository's `RepositoryUri` attribute, and the outputs a client reads both back from.
fn template(row: &Row) -> String {
    let mut repository = json!({ "Type": "AWS::ECR::Repository" });
    if let Some(name) = row.repository_name {
        repository["Properties"] = json!({ "RepositoryName": name });
    }
    json!({
        "Resources": {
            "Registry": repository,
            "Worker": {
                "Type": "AWS::ECS::TaskDefinition",
                "DependsOn": "Registry",
                "Properties": {
                    "Family": row.family,
                    "NetworkMode": "bridge",
                    "ContainerDefinitions": [{
                        "Name": "worker",
                        "Memory": 512,
                        "Essential": true,
                        "Image": row.derived.image("Registry"),
                    }],
                },
            },
        },
        "Outputs": {
            // "Ref returns the repository name", which is the resource's physical id.
            "RepositoryName": { "Value": { "Ref": "Registry" } },
            "RepositoryUri": { "Value": { "Fn::GetAtt": ["Registry", "RepositoryUri"] } },
        },
    })
    .to_string()
}

/// The `host[:port]` and the path of `uri`, split on the FIRST `/`: a client splits a
/// repository URI exactly so, the authority being what it dials and parses and the remainder
/// being the repository's own name — which may itself carry `/` as a namespace path.
fn authority_and_path<'uri>(case: &str, uri: &'uri str) -> (&'uri str, &'uri str) {
    uri.split_once('/').unwrap_or_else(|| {
        panic!("case {case:?}: expected a repositoryUri of the form <authority>/<name>, got {uri:?}")
    })
}

/// The host and the port of an authority: an IPv6 host keeps its brackets, which is where its
/// own colons live.
fn host_and_port(authority: &str) -> (&str, Option<&str>) {
    let after_host = match authority.find(']') {
        Some(end) => end + 1,
        None => 0,
    };
    match authority[after_host..].rsplit_once(':') {
        Some((host, port)) => (&authority[..after_host + host.len()], Some(port)),
        None => (authority, None),
    }
}

/// Assert that `uri` is the URI an Amazon ECR registry publishes for `repository`, as the rule
/// this suite states reads it, and answer the labels its authority carries.
fn assert_registry_uri(case: &str, row: &Row, uri: &str, repository: &str) {
    let (authority, path) = authority_and_path(case, uri);
    let (host, port) = host_and_port(authority);
    let labels: Vec<&str> = host.split('.').collect();
    assert_eq!(
        labels.first().copied(),
        Some(ACCOUNT),
        "case {case:?}: expected the registry's own account id as the first label of {uri:?}, \
         got {labels:?}"
    );
    assert_eq!(
        labels.get(1..3),
        Some(&REGISTRY_LABELS[..]),
        "case {case:?}: expected the fixed registry labels {REGISTRY_LABELS:?} after the \
         account id in {uri:?}, got {labels:?}"
    );
    assert_eq!(
        labels.get(3).copied(),
        Some(row.region),
        "case {case:?}: expected the Region holding the repository as the fourth label of \
         {uri:?}, got {labels:?}"
    );
    assert!(
        labels.len() > 4,
        "case {case:?}: expected at least one label for the zone the registry is served in \
         after the Region in {uri:?}, got {labels:?}"
    );
    // The zone and the port are the SERVED address's own, so the authority a reader parses is
    // also one a container client on this machine can dial.
    assert_eq!(
        labels[4..].join("."),
        row.zone,
        "case {case:?}: expected the registry authority of {uri:?} to be served in the zone of \
         {:?}, got {labels:?}",
        row.served_base
    );
    assert_eq!(
        port, row.port,
        "case {case:?}: expected the port of {:?} on the registry authority of {uri:?}, got \
         {port:?}",
        row.served_base
    );
    assert_eq!(
        path, repository,
        "case {case:?}: expected the repository's own name as the path of {uri:?}, got {path:?}"
    );
}

/// Every address a client picks the repository up at — the stack's published attribute, the
/// registry's own read, and the image of the revision the stack registered — is a registry
/// authority of the owning account and Region.
#[tokio::test]
async fn a_published_repository_uri_names_the_registry_that_holds_it() {
    // The first label is an account id, which an Amazon ECR registry id is: "the 12-digit ID
    // of the Amazon Web Services account" (`Repository$registryId`).
    assert!(
        ACCOUNT.len() == 12 && ACCOUNT.chars().all(|digit| digit.is_ascii_digit()),
        "expected this emulator's account id to be 12 digits, got {ACCOUNT:?}"
    );
    for row in ROWS {
        let case = row.case;
        let router = build_router(row.served_base);
        let outputs = deploy(&router, case, row.region, row.stack, &template(row)).await;
        let repository = outputs.get("RepositoryName").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: no RepositoryName output with a value, got {outputs:?}")
        });
        let published = outputs.get("RepositoryUri").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: no RepositoryUri output with a value, got {outputs:?}")
        });

        // What the stack published for the attribute.
        assert_registry_uri(case, row, &published, &repository);

        // What the registry that OWNS the address reports for the repository: the same value,
        // read through its own API, with the registry id reported beside it.
        let (status, reported) = json_rpc(
            &router,
            row.region,
            "ecr",
            ECR_TARGET_PREFIX,
            "DescribeRepositories",
            &json!({ "repositoryNames": [&repository] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ecr DescribeRepositories on {repository:?} to answer 200, \
             got {status} with body {reported}"
        );
        let held = &reported["repositories"][0];
        let described = held["repositoryUri"].as_str().unwrap_or_default();
        assert_registry_uri(case, row, described, &repository);
        assert_eq!(
            (described, held["registryId"].as_str()),
            (published.as_str(), Some(ACCOUNT)),
            "case {case:?}: expected Amazon ECR to report the address the stack published, got \
             {reported}"
        );

        // And the artifact another service derived from it: the image of the revision Amazon
        // ECS registered, which is the value a client reads to learn where that container is
        // pulled from.
        let (status, reported) = json_rpc(
            &router,
            row.region,
            "ecs",
            ECS_TARGET_PREFIX,
            "DescribeTaskDefinition",
            &json!({ "taskDefinition": row.family }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ecs DescribeTaskDefinition on {:?} to answer 200, got \
             {status} with body {reported}",
            row.family
        );
        let image = reported["taskDefinition"]["containerDefinitions"][0]["image"]
            .as_str()
            .unwrap_or_default();
        let expected = row.derived.resolved(&published);
        assert_eq!(
            image, expected,
            "case {case:?}: expected the registered revision to carry {expected:?} as its \
             image, got {reported}"
        );
        // The image is that address with the tag the declaration stated, so the reference a
        // puller reads it as names this registry too.
        let suffix = row.derived.tag_suffix();
        let image_uri = image.strip_suffix(&suffix).unwrap_or(image);
        assert_registry_uri(case, row, image_uri, &repository);
    }
}
