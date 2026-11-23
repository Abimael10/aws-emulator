// SPDX-License-Identifier: CC0-1.0

//! What `CreateCapacityProvider` persists, over the dimensions the create's own rule
//! generalises over: the region the request is signed for (which the provider's ARN carries,
//! and which every later read of that provider repeats), the identifier it mints per
//! provider, and the `clientToken` that makes a repeated request answer with the provider it
//! first produced rather than minting a second one.
//!
//! Each row drives the service over HTTP through one router on one shared engine, so a create
//! and the reads that follow it are asserted against the same store.
//!
//! Sources: `smithy_query(query="operation",
//! operation="CreateCapacityProvider,GetCapacityProvider,ListCapacityProviders")` for every
//! member asserted here; `smithy_query(query="shape",shape="CapacityProviderArn")` for the
//! ARN form (`^arn:aws(-[^:]+)?:bedrock-agentcore:[a-z0-9-]+:[0-9]{12}:capacity-provider/[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$`),
//! `shape="CapacityProviderId"` for the minted id's form, and the operation's documentation
//! for the idempotency token ("If this token matches a previous request, the service ignores
//! the request, but doesn't return an error") and for the `CREATING` state a new provider
//! enters.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A sigv4 authorization header whose credential scope names `region`, which is where this
/// service reads the region of a request from.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization(region))
        .header(
            "host",
            format!("bedrock-agentcore-control.{region}.amazonaws.com"),
        );
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn create_provider(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    send(router, region, "PUT", "/capacity-providers", Some(body)).await
}

async fn get_provider(router: &MultiServiceRouter, region: &str, id: &str) -> (u16, Value) {
    send(
        router,
        region,
        "GET",
        &format!("/capacity-providers/{id}"),
        None,
    )
    .await
}

async fn list_provider_ids(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let (status, listed) = send(router, region, "POST", "/capacity-providers", None).await;
    assert_eq!(
        status, 200,
        "the listing answers HTTP 200, got {status} ({listed})"
    );
    listed
        .get("capacityProviders")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| text(summary, "capacityProviderId"))
        .collect()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The compute configuration a provider is created with: one union member carrying the EC2
/// configuration, every value inside its own modelled constraint.
fn compute_configuration(instance_type: &str) -> Value {
    json!({
        "ec2Configuration": {
            "launchTemplateSource": {
                "launchParameters": {
                    "operatingSystem": "LINUX_X86_64",
                    "instanceRequirements": {"allowedInstanceTypes": [instance_type]}
                }
            },
            "vpcConfiguration": {
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroups": ["sg-0123456789abcdef0"]
            }
        }
    })
}

fn permissions_configuration() -> Value {
    json!({"capacityProviderOperatorRoleArn":
        "arn:aws:iam::000000000000:role/AgentCoreCapacityProvider"})
}

fn create_body(name: &str) -> Value {
    json!({
        "name": name,
        "permissionsConfiguration": permissions_configuration(),
        "computeConfiguration": compute_configuration("m5.large")
    })
}

/// The provider's ARN carries the region the request was signed for, and the id the create
/// minted keeps that ARN on every later read: the create computes the ARN from the request
/// rather than from one fixed region.
#[tokio::test]
async fn a_created_providers_arn_carries_the_requests_region() {
    // Regions differing in their shape within an ARN segment's `[a-z0-9-]+` grammar: two
    // parts, three parts, and a part carrying a digit.
    let regions = ["us-east-1", "eu-west-2", "ap-southeast-2", "il-central-1"];

    for (index, region) in regions.iter().enumerate() {
        let router = router();
        let name = format!("regionalProvider{index}");
        let (status, created) = create_provider(&router, region, &create_body(&name)).await;
        assert_eq!(
            status, 202,
            "case {region:?}: expected HTTP 202 from CreateCapacityProvider, got {status} \
             ({created})"
        );
        let id = text(&created, "capacityProviderId");
        let expected_arn =
            format!("arn:aws:bedrock-agentcore:{region}:{ACCOUNT}:capacity-provider/{id}");
        assert_eq!(
            text(&created, "capacityProviderArn"),
            expected_arn,
            "case {region:?}: the create stamps the signed region into the provider's ARN, got \
             {created}"
        );
        assert_eq!(
            (text(&created, "name"), text(&created, "status")),
            (name.clone(), "CREATING".to_string()),
            "case {region:?}: a new provider reports its name and the status it enters, got \
             {created}"
        );

        let (status, fetched) = get_provider(&router, region, &id).await;
        assert_eq!(
            (status, text(&fetched, "capacityProviderArn")),
            (200, expected_arn),
            "case {region:?}: a read reports the ARN the create minted, got {status} ({fetched})"
        );
    }
}

/// Every create mints its own identifier: providers that differ only in their name read back
/// as distinct providers, each carrying the configuration its own create persisted.
#[tokio::test]
async fn each_create_mints_its_own_provider() {
    let region = "us-west-2";
    let router = router();
    // (name, the instance type that create's compute configuration allows)
    let rows = [
        ("firstProvider", "m5.large"),
        ("secondProvider", "c6i.xlarge"),
        ("thirdProvider", "r7g.2xlarge"),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (name, instance_type) in rows {
        let body = json!({
            "name": name,
            "permissionsConfiguration": permissions_configuration(),
            "computeConfiguration": compute_configuration(instance_type)
        });
        let (status, created) = create_provider(&router, region, &body).await;
        assert_eq!(
            status, 202,
            "case {name:?}: expected HTTP 202, got {status} ({created})"
        );
        let id = text(&created, "capacityProviderId");
        assert!(
            !minted.contains(&id),
            "case {name:?}: every create mints an id of its own, got the already-minted {id:?}"
        );
        minted.push(id.clone());

        let (status, fetched) = get_provider(&router, region, &id).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from GetCapacityProvider, got {status} ({fetched})"
        );
        assert_eq!(
            (
                text(&fetched, "name"),
                fetched.get("computeConfiguration").cloned()
            ),
            (name.to_string(), body.get("computeConfiguration").cloned()),
            "case {name:?}: the read reports what this create persisted, got {fetched}"
        );
    }

    let mut listed = list_provider_ids(&router, region).await;
    listed.sort();
    let mut expected = minted.clone();
    expected.sort();
    assert_eq!(
        listed, expected,
        "case \"the listing\": expected every minted provider exactly once, got {listed:?}"
    );
}

/// Which `clientToken` a create carries decides whether it mints a provider: a request
/// repeating a token a previous create used answers with the provider that token first
/// produced, while a token of its own — or no token at all — is a create of its own, even when
/// it repeats a name the account already holds. The rows differ in the token they carry, over
/// the forms `ClientToken` admits.
#[tokio::test]
async fn whether_a_create_mints_a_provider_follows_its_client_token() {
    let region = "eu-central-1";
    let router = router();
    // `ClientToken` is @length 33..256, pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`: a
    // token at the minimum length, and a longer one carrying the hyphens the pattern admits.
    let short_token = format!("t{}", "0".repeat(32));
    let hyphened_token = format!("u{}9", "9-".repeat(20));

    // (case, the name the create carries, the token it carries) — each row sent twice.
    let rows: [(&str, &str, Option<String>); 3] = [
        (
            "a token at the member's minimum length",
            "shortTokenProvider",
            Some(short_token),
        ),
        (
            "a longer token carrying hyphens",
            "hyphenTokenProvider",
            Some(hyphened_token),
        ),
        ("no token at all", "untokenedProvider", None),
    ];

    let mut held: Vec<String> = Vec::new();
    for (case, name, token) in &rows {
        let mut body = create_body(name);
        if let Some(token) = token {
            body["clientToken"] = json!(token);
        }
        let (status, created) = create_provider(&router, region, &body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateCapacityProvider, got {status} \
             ({created})"
        );
        let id = text(&created, "capacityProviderId");
        assert!(
            !held.contains(&id),
            "case {case:?}: a token of its own mints a provider of its own, got the \
             already-minted {id:?}"
        );
        held.push(id.clone());

        // The same request again: a tokened create answers with the provider its token first
        // produced, so it leaves no second provider behind; an untokened one is a create of
        // its own and mints a second provider under the same name.
        let (status, again) = create_provider(&router, region, &body).await;
        if token.is_some() {
            assert_eq!(
                (status, text(&again, "capacityProviderId")),
                (202, id.clone()),
                "case {case:?}: the repeated token answers with the provider it first \
                 produced, got {status} ({again})"
            );
        } else {
            let repeated = text(&again, "capacityProviderId");
            assert_eq!(
                status, 202,
                "case {case:?}: without a token the repeat is a create of its own, got {status} \
                 ({again})"
            );
            assert!(
                !held.contains(&repeated),
                "case {case:?}: the untokened repeat mints an id of its own, got the \
                 already-minted {repeated:?}"
            );
            held.push(repeated);
        }

        let mut listed = list_provider_ids(&router, region).await;
        listed.sort();
        let mut expected = held.clone();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {case:?}: the listing reports exactly the providers the row minted, got \
             {listed:?}"
        );
    }

    // Every provider the rows minted is still readable, each under its own id.
    for id in &held {
        let (status, fetched) = get_provider(&router, region, id).await;
        let name = text(&fetched, "name");
        assert_eq!(
            (status, id.starts_with(&format!("{name}-"))),
            (200, true),
            "case {id:?}: the provider reads back under the id the create minted, got {status} \
             ({fetched})"
        );
    }
}
