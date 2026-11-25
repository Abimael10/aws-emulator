// SPDX-License-Identifier: CC0-1.0

//! Classic Elastic Load Balancing resolves the EC2 INSTANCES a request names against Amazon
//! EC2, which owns them.
//!
//! RULE: an instance is EC2's resource, so every instance id a Classic load balancer
//! request names is resolved against EC2 in the Region the request was signed for, and an id
//! EC2 holds no instance for names no endpoint: the request is refused with the error the
//! operation models for that — `InvalidEndPointException`, HTTP 400 `text/xml` awsQuery
//! `ErrorResponse` with `<Code>InvalidInstance</Code>` — and NOTHING of the request takes
//! effect, so the load balancer's registered set is left exactly as it was. An id EC2 DOES
//! hold an instance for is this service's own business again: it is registered, deregistered
//! or reported on as before, and an instance that exists but is not registered with the load
//! balancer is REPORTED (`InstanceState$Description` "Instance is not currently registered
//! with the LoadBalancer.") rather than refused.
//!
//! This suite lives beside the router because the instances are EC2's: every instance below
//! is launched by EC2's own `RunInstances` on the SAME engine that serves Elastic Load
//! Balancing, so each id in an assertion is one EC2 handed out rather than one this suite
//! made up, and each refusal is grounded in what EC2 actually answered.
//!
//! The rows differ along what the rule generalises over: the FORM of the value that names no
//! instance (EC2's 17-hexadecimal-digit id form, its older 8-digit form, a value that is no
//! instance id at all, and the id of an instance that exists in ANOTHER Region, EC2 being
//! reached through a regional endpoint), the operation that names it
//! (`RegisterInstancesWithLoadBalancer`, `DeregisterInstancesFromLoadBalancer`,
//! `DescribeInstanceHealth`), and how many ids the request carries and where the unknown one
//! sits among them — because the rule is about resolving the ids, not about one recorded
//! request.
//!
//! Error source: `smithy_query(query="shape", shape="InvalidEndPointException")` on the
//! Classic model — `errorKind` client, `httpError` 400, `awsQueryError` code
//! `InvalidInstance` with `httpResponseCode` 400.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, element, form_encode, opt_element};

/// The Region the load balancer and its backends live in.
const REGION: &str = "us-west-2";

/// Another Region, where one instance is launched: EC2 is reached through a regional
/// endpoint, so an instance of this Region is one the load balancer's Region holds none of.
const OTHER_REGION: &str = "us-east-1";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The Classic Elastic Load Balancing API version a client sends in its awsQuery body.
const ELB_API_VERSION: &str = "2012-06-01";

/// The signing name both halves of the Elastic Load Balancing endpoint share, which is what
/// routes a Classic request to the Classic model.
const ELB_SIGNING_NAME: &str = "elasticloadbalancing";

/// The AMI every launch below names. Any well-formed image id: what the rule turns on is
/// whether EC2 holds an instance under the id, not the image it came from.
const AMI: &str = "ami-0123456789abcdef0";

/// The load balancer every request below names.
const LOAD_BALANCER: &str = "instances-resolved-elb";

/// One emulator carrying EC2 and Classic Elastic Load Balancing over ONE shared engine, as
/// the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One query-protocol answer: its HTTP status and its XML body.
struct Answer {
    status: u16,
    body: String,
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`,
/// which is what routes the request and which Region it is answered for; the signature is
/// never checked.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// POST one query-protocol action, signed for `signing_name` in `region`.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    region: &str,
    version: &str,
    action: &str,
    params: &[(String, String)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// One Classic Elastic Load Balancing action in [`REGION`].
async fn elb(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> Answer {
    query(
        router,
        ELB_SIGNING_NAME,
        REGION,
        ELB_API_VERSION,
        action,
        params,
    )
    .await
}

/// Launch ONE instance through EC2's own API in `region` and answer the id EC2 minted for
/// it, so every id this suite registers is EC2's own.
async fn launch(router: &MultiServiceRouter, region: &str, case: &str) -> String {
    let answer = query(
        router,
        "ec2",
        region,
        EC2_API_VERSION,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: RunInstances must answer 200, got {} with {}",
        answer.status, answer.body
    );
    element(&answer.body, "instanceId")
}

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The `Instances.member.<n>.InstanceId` fields naming `instance_ids`, in order.
fn instance_params(load_balancer: &str, instance_ids: &[&str]) -> Vec<(String, String)> {
    let mut params = vec![param("LoadBalancerName", load_balancer)];
    for (index, instance_id) in instance_ids.iter().enumerate() {
        params.push(param(
            &format!("Instances.member.{}.InstanceId", index + 1),
            instance_id,
        ));
    }
    params
}

/// The `<Code>` of an `<ErrorResponse>` envelope, or an empty string.
fn code(answer: &Answer) -> String {
    opt_element(&answer.body, "Code").unwrap_or_default()
}

/// The `<Message>` of a refusal envelope, or an empty string.
fn message(answer: &Answer) -> String {
    opt_element(&answer.body, "Message").unwrap_or_default()
}

/// The instance ids `DescribeInstanceHealth` reports for the load balancer when the request
/// names none of its own — which is the load balancer's REGISTERED set, read back through the
/// API.
async fn registered(router: &MultiServiceRouter) -> Vec<String> {
    let answer = elb(
        router,
        "DescribeInstanceHealth",
        &[param("LoadBalancerName", LOAD_BALANCER)],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "a filter-less DescribeInstanceHealth must answer 200, got {} with {}",
        answer.status, answer.body
    );
    health_members(&answer.body)
        .into_iter()
        .map(|(instance_id, ..)| instance_id)
        .collect()
}

/// Every `InstanceState` of an answer as `(InstanceId, State, ReasonCode, Description)`, in
/// the order the answer lists them.
fn health_members(body: &str) -> Vec<(String, String, String, String)> {
    let Some(list) = block_of(body, "InstanceStates") else {
        return Vec::new();
    };
    blocks_of(list, "member")
        .into_iter()
        .map(|member| {
            (
                element(member, "InstanceId"),
                element(member, "State"),
                element(member, "ReasonCode"),
                element(member, "Description"),
            )
        })
        .collect()
}

/// Whether `message` relays the EC2 fault the read ran into: EC2's own sentence about the id,
/// followed by the context of the call that raised it.
fn relays_ec2_fault(message: &str, instance_id: &str) -> bool {
    message.contains(instance_id)
        && message.contains("(Service: AmazonEC2; Status Code: 400; Error Code: InvalidInstanceID.")
        && message.ends_with("; Proxy: null)")
}

/// One row of the refusal table: the case, the action, the form fields, and the id the
/// refusal is about.
type RefusalRow = (String, &'static str, Vec<(String, String)>, String);

#[tokio::test]
async fn classic_load_balancer_instances_resolve_against_ec2() {
    let router = emulator();
    // Three instances of this Region — two registered below, one left unregistered — and one
    // of another Region, every one of them launched by EC2 itself.
    let first = launch(&router, REGION, "the first backend").await;
    let second = launch(&router, REGION, "the second backend").await;
    let unregistered = launch(&router, REGION, "a backend nothing registers").await;
    let elsewhere = launch(&router, OTHER_REGION, "a backend of another Region").await;

    let created = elb(
        &router,
        "CreateLoadBalancer",
        &[
            param("LoadBalancerName", LOAD_BALANCER),
            param("Listeners.member.1.Protocol", "HTTP"),
            param("Listeners.member.1.LoadBalancerPort", "80"),
            param("Listeners.member.1.InstancePort", "80"),
            param("AvailabilityZones.member.1", &format!("{REGION}a")),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the fixture must be created, got {} with {}",
        created.status, created.body
    );

    // Registering the instances EC2 DOES hold is accepted, and the set reads back.
    let registered_pair = elb(
        &router,
        "RegisterInstancesWithLoadBalancer",
        &instance_params(LOAD_BALANCER, &[&first, &second]),
    )
    .await;
    assert_eq!(
        registered_pair.status, 200,
        "registering two instances EC2 holds must answer 200, got {} with {}",
        registered_pair.status, registered_pair.body
    );
    assert_eq!(
        registered(&router).await,
        vec![first.clone(), second.clone()],
        "the registered set must be the two instances EC2 holds"
    );

    // Values that name no instance of the load balancer's Region, one per FORM such a value
    // comes in. The last is an instance that EXISTS — in another Region — which this
    // Region's endpoint holds none of.
    let unknown_ids: Vec<(String, String)> = vec![
        (
            "EC2's 17-hexadecimal-digit id form".to_string(),
            "i-0f1e2d3c4b5a69780".to_string(),
        ),
        (
            "EC2's older 8-hexadecimal-digit id form".to_string(),
            "i-1a2b3c4d".to_string(),
        ),
        (
            "a value that is no instance id at all".to_string(),
            "not-an-instance".to_string(),
        ),
        (
            format!("an instance of {OTHER_REGION}"),
            elsewhere.clone(),
        ),
    ];

    let mut rows: Vec<RefusalRow> = Vec::new();
    for (form, id) in &unknown_ids {
        rows.push((
            format!("a registration naming an id EC2 does not hold, in {form}"),
            "RegisterInstancesWithLoadBalancer",
            instance_params(LOAD_BALANCER, &[id]),
            id.clone(),
        ));
        rows.push((
            format!("a registration naming an instance EC2 holds beside one it does not, in {form}"),
            "RegisterInstancesWithLoadBalancer",
            instance_params(LOAD_BALANCER, &[&unregistered, id]),
            id.clone(),
        ));
        rows.push((
            format!("a registration whose unknown id comes FIRST, in {form}"),
            "RegisterInstancesWithLoadBalancer",
            instance_params(LOAD_BALANCER, &[id, &unregistered]),
            id.clone(),
        ));
        rows.push((
            format!("a deregistration naming an id EC2 does not hold, in {form}"),
            "DeregisterInstancesFromLoadBalancer",
            instance_params(LOAD_BALANCER, &[id]),
            id.clone(),
        ));
        rows.push((
            format!("a deregistration naming a registered instance beside one EC2 does not hold, in {form}"),
            "DeregisterInstancesFromLoadBalancer",
            instance_params(LOAD_BALANCER, &[&first, id]),
            id.clone(),
        ));
        rows.push((
            format!("a health read naming an id EC2 does not hold, in {form}"),
            "DescribeInstanceHealth",
            instance_params(LOAD_BALANCER, &[id]),
            id.clone(),
        ));
        rows.push((
            format!("a health read naming a registered instance beside one EC2 does not hold, in {form}"),
            "DescribeInstanceHealth",
            instance_params(LOAD_BALANCER, &[&first, id]),
            id.clone(),
        ));
    }

    for (case, action, params, instance_id) in rows {
        let refused = elb(&router, action, &params).await;
        assert_eq!(
            (refused.status, code(&refused).as_str()),
            (400, "InvalidInstance"),
            "case {case:?}: expected 400 InvalidInstance from {action}, got {} with {}",
            refused.status,
            refused.body
        );
        assert!(
            refused.body.contains("<ErrorResponse"),
            "case {case:?}: an awsQuery client must get an <ErrorResponse> envelope, got {}",
            refused.body
        );
        let stated = message(&refused);
        if action == "DescribeInstanceHealth" {
            // The sentence Elastic Load Balancing words itself for a health read.
            assert_eq!(
                stated,
                format!("Could not find EC2 instance {instance_id}."),
                "case {case:?}: expected the health read's own sentence, got {stated:?}"
            );
        } else {
            // The EC2 fault the registration read ran into, relayed whole.
            assert!(
                relays_ec2_fault(&stated, &instance_id),
                "case {case:?}: expected EC2's own sentence about {instance_id} and the \
                 context of the call that raised it, got {stated:?}"
            );
        }

        // Nothing of a refused request took effect: neither the registration it would have
        // added nor the deregistration it would have performed.
        assert_eq!(
            registered(&router).await,
            vec![first.clone(), second.clone()],
            "case {case:?}: a refused request must leave the registered set as it was"
        );
    }

    // An instance EC2 DOES hold that is simply not registered is REPORTED, not refused:
    // "If instances are specified, their state is returned even if they are no longer
    // registered with the load balancer."
    let stranger = elb(
        &router,
        "DescribeInstanceHealth",
        &instance_params(LOAD_BALANCER, &[&unregistered]),
    )
    .await;
    assert_eq!(
        stranger.status, 200,
        "a health read of an unregistered instance EC2 holds must answer 200, got {} with {}",
        stranger.status, stranger.body
    );
    assert_eq!(
        health_members(&stranger.body),
        vec![(
            unregistered.clone(),
            "OutOfService".to_string(),
            "ELB".to_string(),
            "Instance is not currently registered with the LoadBalancer.".to_string(),
        )],
        "an unregistered instance must be reported as not registered, got {}",
        stranger.body
    );

    // And a deregistration naming only instances EC2 holds takes effect.
    let deregistered = elb(
        &router,
        "DeregisterInstancesFromLoadBalancer",
        &instance_params(LOAD_BALANCER, &[&first]),
    )
    .await;
    assert_eq!(
        deregistered.status, 200,
        "deregistering an instance EC2 holds must answer 200, got {} with {}",
        deregistered.status, deregistered.body
    );
    assert_eq!(
        registered(&router).await,
        vec![second.clone()],
        "the deregistration must leave the other instance registered"
    );
}
