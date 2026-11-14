// SPDX-License-Identifier: CC0-1.0

//! The staging-area subnet a replication configuration names, resolved against the
//! service that owns it.
//!
//! The rules under test:
//!
//! 1. A subnet is Amazon EC2's resource, not this service's: a call that stores a
//!    caller-named `stagingAreaSubnetId` resolves it against EC2 first, and an id EC2
//!    reports no subnet for is a 400 `ValidationException` naming that id — with
//!    nothing stored, and with the stored template unchanged on an update.
//! 2. A subnet EC2 does hold is served, however it came to exist: created through EC2
//!    in this Region, or a default subnet of the Region nobody created.
//! 3. A refusal is grounded in what EC2 ANSWERED, never assumed from silence: an
//!    emulator with no EC2 mounted serves the create.
//!
//! The rows span what the rules generalise over: how an id fails to resolve (never
//! minted, minted and then deleted), how a subnet came to exist, and which operation
//! names it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_drs_elasticdisasterrecoveryservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use super::replication_configuration_template_lifecycle::{call, create_body, items, REGION};

/// The CIDR of the VPC these cases carve their subnets out of.
const VPC_CIDR: &str = "10.42.0.0/16";

/// DRS mounted beside the OWNER of the resource it resolves — Amazon EC2, which holds
/// the account's subnets — on ONE shared engine, plus an EC2 client over the router's
/// own in-process callback: the very seam the handler's resolution reaches EC2 through.
fn drs_with_ec2() -> (MultiServiceRouter, ::aws_sdk_ec2::Client) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());
    let ec2 = ::aws_sdk_ec2::Client::from_conf(
        ::aws_sdk_ec2::Config::builder()
            .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
            .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
            .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
            )
            .build(),
    );
    (router, ec2)
}

/// A VPC of `cidr`, as EC2 minted it.
async fn create_vpc(ec2: &::aws_sdk_ec2::Client, cidr: &str) -> String {
    ec2.create_vpc()
        .cidr_block(cidr)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must create the VPC {cidr}: {error:?}"))
        .vpc()
        .and_then(|vpc| vpc.vpc_id().map(str::to_string))
        .unwrap_or_else(|| panic!("setup: EC2 must report a VpcId for {cidr}"))
}

/// A subnet of `cidr` in `vpc`, as EC2 minted it.
async fn create_subnet(ec2: &::aws_sdk_ec2::Client, vpc: &str, cidr: &str) -> String {
    ec2.create_subnet()
        .vpc_id(vpc)
        .cidr_block(cidr)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must create the subnet {cidr}: {error:?}"))
        .subnet()
        .and_then(|subnet| subnet.subnet_id().map(str::to_string))
        .unwrap_or_else(|| panic!("setup: EC2 must report a SubnetId for {cidr}"))
}

/// One default subnet of [`REGION`], which the account holds without anybody creating
/// it.
async fn a_default_subnet(ec2: &::aws_sdk_ec2::Client) -> String {
    ec2.describe_subnets()
        .filters(
            ::aws_sdk_ec2::types::Filter::builder()
                .name("default-for-az")
                .values("true")
                .build(),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must report the default subnets: {error:?}"))
        .subnets()
        .iter()
        .find_map(|subnet| subnet.subnet_id().map(str::to_string))
        .unwrap_or_else(|| panic!("setup: expected {REGION} to hold a default subnet"))
}

/// An id shaped like a subnet id — `SubnetID`'s `@pattern` is
/// `^subnet-[0-9a-fA-F]{8,}$`, enforced at the boundary — that nothing in the account
/// holds. `label` keeps each row's id its own.
fn unresolvable_subnet(label: &str) -> String {
    format!("subnet-{label}")
}

/// The `message` member of an answer.
fn message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a message member, got {body}"))
        .to_string()
}

#[tokio::test]
async fn a_create_resolves_its_staging_area_subnet_against_ec2() {
    let (router, ec2) = drs_with_ec2();
    let vpc = create_vpc(&ec2, VPC_CIDR).await;
    let held = create_subnet(&ec2, &vpc, "10.42.1.0/24").await;
    let by_default = a_default_subnet(&ec2).await;
    let deleted = create_subnet(&ec2, &vpc, "10.42.2.0/24").await;
    ec2.delete_subnet()
        .subnet_id(&deleted)
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must delete {deleted}: {error:?}"));

    // (case, the subnet the create names, whether EC2 holds it)
    let cases: Vec<(&str, String, bool)> = vec![
        ("a subnet created through EC2", held.clone(), true),
        ("a default subnet of the Region", by_default, true),
        (
            "an id no subnet was ever minted under",
            unresolvable_subnet("00000000deadbeef"),
            false,
        ),
        (
            "another id nothing holds",
            unresolvable_subnet("ffffffffffffffff"),
            false,
        ),
        ("a subnet that was deleted", deleted, false),
    ];

    let mut created = 0;
    for (case, subnet, resolves) in &cases {
        let answer = call(
            &router,
            "CreateReplicationConfigurationTemplate",
            &create_body(subnet, json!({})),
        )
        .await;
        if *resolves {
            created += 1;
            assert_eq!(
                answer.status, 201,
                "case {case} ({subnet}): expected 201, got {answer:?}"
            );
            assert_eq!(
                answer.member("stagingAreaSubnetId").and_then(Value::as_str),
                Some(subnet.as_str()),
                "case {case}: the created template must carry the subnet asked for: {answer:?}"
            );
        } else {
            assert_eq!(
                answer.status, 400,
                "case {case} ({subnet}): expected 400, got {answer:?}"
            );
            assert_eq!(
                answer.error_code.as_deref(),
                Some("ValidationException"),
                "case {case} ({subnet}): expected ValidationException, got {answer:?}"
            );
            assert_eq!(
                message(&answer.body),
                format!("SubnetId {subnet} wasn't found"),
                "case {case}: wrong message in {answer:?}"
            );
            assert_eq!(
                answer.member("reason").and_then(Value::as_str),
                Some("FIELD_VALIDATION_FAILED"),
                "case {case}: wrong reason in {answer:?}"
            );
        }
    }

    // A refused create stored nothing: only the resolved ones are in the store.
    let listed = call(
        &router,
        "DescribeReplicationConfigurationTemplates",
        &json!({}),
    )
    .await;
    assert_eq!(
        items(&listed).len(),
        created,
        "expected {created} stored templates, got {listed:?}"
    );
}

#[tokio::test]
async fn an_update_resolves_a_replacement_staging_area_subnet() {
    let (router, ec2) = drs_with_ec2();
    let vpc = create_vpc(&ec2, VPC_CIDR).await;
    let first = create_subnet(&ec2, &vpc, "10.42.3.0/24").await;
    let second = create_subnet(&ec2, &vpc, "10.42.4.0/24").await;

    let created = call(
        &router,
        "CreateReplicationConfigurationTemplate",
        &create_body(&first, json!({})),
    )
    .await;
    assert_eq!(created.status, 201, "setup: create {created:?}");
    let id = created.body["replicationConfigurationTemplateID"]
        .as_str()
        .expect("a template id")
        .to_string();

    // An id nothing holds is refused and the stored subnet is untouched; an update
    // that names no subnet at all resolves nothing and is served; a subnet EC2 holds
    // replaces the stored one.
    let absent = unresolvable_subnet("00000000c0ffee00");
    let refused = call(
        &router,
        "UpdateReplicationConfigurationTemplate",
        &json!({"replicationConfigurationTemplateID": id, "stagingAreaSubnetId": absent}),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case an absent subnet: expected 400, got {refused:?}"
    );
    assert_eq!(
        message(&refused.body),
        format!("SubnetId {absent} wasn't found"),
        "case an absent subnet: wrong message in {refused:?}"
    );

    let untouched = call(
        &router,
        "UpdateReplicationConfigurationTemplate",
        &json!({"replicationConfigurationTemplateID": id, "bandwidthThrottling": 5}),
    )
    .await;
    assert_eq!(
        untouched.status, 200,
        "case no subnet supplied: expected 200, got {untouched:?}"
    );
    assert_eq!(
        untouched.member("stagingAreaSubnetId").and_then(Value::as_str),
        Some(first.as_str()),
        "case an absent subnet: the stored subnet must still be {first}: {untouched:?}"
    );

    let replaced = call(
        &router,
        "UpdateReplicationConfigurationTemplate",
        &json!({"replicationConfigurationTemplateID": id, "stagingAreaSubnetId": second}),
    )
    .await;
    assert_eq!(
        replaced.status, 200,
        "case a subnet EC2 holds: expected 200, got {replaced:?}"
    );
    assert_eq!(
        replaced.member("stagingAreaSubnetId").and_then(Value::as_str),
        Some(second.as_str()),
        "case a subnet EC2 holds: expected the replacement, got {replaced:?}"
    );
}

/// With no EC2 mounted there is nobody to ask, so the create is SERVED: a refusal is
/// grounded in what EC2 answered, never in this emulator failing to ask.
#[tokio::test]
async fn an_emulator_with_no_ec2_serves_the_create() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));

    let subnet = unresolvable_subnet("00000000deadbeef");
    let answer = call(
        &router,
        "CreateReplicationConfigurationTemplate",
        &create_body(&subnet, json!({})),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "case no EC2 mounted: expected the create to be served, got {answer:?}"
    );
}
