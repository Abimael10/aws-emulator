// SPDX-License-Identifier: CC0-1.0

//! The REFUSAL Elastic Load Balancing states when the security groups a request names
//! cannot be resolved, driven with both services on one engine.
//!
//! A load balancer's security groups are EC2's resources, so Elastic Load Balancing resolves
//! what a request names against EC2 before it places anything. When EC2 REFUSES that read
//! because a named value is not a security group id at all, the fault it answered is what
//! reaches the caller: the modelled `InvalidSecurityGroup` (HTTP 400) stating EC2's own
//! sentence — which names the value at fault — under the clause an SDK renders around a
//! failed call, naming the service that answered, its status, its error code and the id of
//! that call. Neither half alone is the message: the sentence without the clause is EC2's
//! refusal passed off as this service's own, and the clause without the sentence names
//! nothing the caller sent.
//!
//! EC2's half of that rule is here too, because the relayed sentence is only as good as what
//! EC2 answers: a `GroupIds` value outside the id form EC2 mints names no group of ANY
//! account, so `DescribeSecurityGroups` refuses it as malformed (`InvalidGroupId.Malformed`)
//! before the account's groups are read, exactly as every other EC2 operation that takes a
//! group id does.
//!
//! The rows span the FORMS a value can fail that id form in — no prefix, no suffix, a suffix
//! shorter than, between and longer than the two lengths EC2 mints, a group name, an ARN —
//! and each is driven through both operations that judge a load balancer's groups
//! (`CreateLoadBalancer` and `SetSecurityGroups`). The controls keep the two classes apart: a
//! WELL-FORMED id the account holds no group under, and a group of another VPC, are
//! configurations the named resources cannot hold (`InvalidConfigurationRequest`), and the
//! load balancer's own group is served.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread. The request plumbing is
//! `elbv2_network_resolution.rs`'s, reused rather than written twice.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{ec2, ec2_ok, elbv2, Answer, BOOT_STACK_SIZE};
use crate::query_wire::{element, elements};

/// The awsQuery code of `InvalidSecurityGroupException`
/// (`smithy_query(query="operation",operation="CreateLoadBalancer")`), which the model binds
/// to HTTP 400 and `SetSecurityGroups` declares as well.
const INVALID_SECURITY_GROUP: &str = "InvalidSecurityGroup";

/// The awsQuery code of `InvalidConfigurationRequestException`, the class the controls stay
/// in.
const INVALID_CONFIGURATION: &str = "InvalidConfigurationRequest";

/// What Elastic Load Balancing states when the groups a request names cannot govern this
/// load balancer's traffic.
const INVALID_SECURITY_GROUPS: &str = "One or more security groups are invalid";

/// The EC2 error code for a value that is not a security group id at all (EC2 API error
/// codes, `InvalidGroupId.Malformed`).
const GROUP_ID_MALFORMED: &str = "InvalidGroupId.Malformed";

/// How Amazon EC2 names itself in the fault clause an SDK renders around a call made to it.
const EC2_SERVICE_NAME: &str = "AmazonEC2";

/// The tail of that clause: there is no proxy in front of the call this emulator made.
const CLAUSE_TAIL: &str = "; Proxy: null)";

/// A well-formed security group id this account holds no group for: EC2 mints ids of 17
/// hexadecimal digits, and nothing in this suite creates this one.
const ABSENT_GROUP: &str = "sg-0fedcba9876543210";

/// The values a row names a security group by that EC2 mints no id in the form of.
///
/// The form itself is EC2's: `sg-` followed by the 8 or 17 hexadecimal digits of its short
/// and long id forms, which is what `DescribeSecurityGroups` judges a `GroupIds` value by.
/// The rows differ in HOW each value falls outside it, and the value the recorded divergence
/// carried (`sg-bogus`) is one row among them rather than the case under test.
const MALFORMED_VALUES: &[(&str, &str)] = &[
    ("a value with no group prefix at all", "bogus"),
    ("a prefix with a non-hexadecimal suffix", "sg-bogus"),
    ("a prefix with no suffix", "sg-"),
    ("a suffix one digit short of the short form", "sg-1234567"),
    ("a suffix between the two forms", "sg-0123456789abcdef"),
    ("a suffix one digit past the long form", "sg-0123456789abcdef01"),
    ("a group NAME where an id belongs", "default"),
    (
        "the ARN of a group instead of its id",
        "arn:aws:ec2:us-west-2:123456789012:security-group/sg-0123456789abcdef0",
    ),
];

/// The network every row runs against: one VPC with two subnets in different zones and a
/// security group of its own, beside a second VPC whose group is one this load balancer
/// cannot be given.
struct Network {
    subnets: Vec<String>,
    security_group: String,
    other_security_group: String,
}

/// Create that network through EC2 itself, so every id a row names is one EC2 minted.
fn seed(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Network {
    let vpc = element(
        &ec2_ok(router, runtime, "CreateVpc", &[("CidrBlock", "10.70.0.0/16")]),
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (cidr, zone) in [("10.70.1.0/24", "us-west-2a"), ("10.70.2.0/24", "us-west-2b")] {
        subnets.push(element(
            &ec2_ok(
                router,
                runtime,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", zone),
                ],
            ),
            "subnetId",
        ));
    }
    let security_group = element(
        &ec2_ok(
            router,
            runtime,
            "CreateSecurityGroup",
            &[
                ("GroupName", "elbv2-relay-own"),
                ("GroupDescription", "the load balancers own VPC"),
                ("VpcId", vpc.as_str()),
            ],
        ),
        "groupId",
    );
    let other_vpc = element(
        &ec2_ok(router, runtime, "CreateVpc", &[("CidrBlock", "10.71.0.0/16")]),
        "vpcId",
    );
    let other_security_group = element(
        &ec2_ok(
            router,
            runtime,
            "CreateSecurityGroup",
            &[
                ("GroupName", "elbv2-relay-foreign"),
                ("GroupDescription", "another VPC"),
                ("VpcId", other_vpc.as_str()),
            ],
        ),
        "groupId",
    );
    Network {
        subnets,
        security_group,
        other_security_group,
    }
}

/// The refusal an answer carries: its status, code and message, so one assertion names all
/// three.
fn refusal(answer: &Answer) -> (u16, String, String) {
    (answer.status, answer.code(), answer.message())
}

/// The sentence EC2 words for a value that is not a security group id at all: the fixed
/// `Invalid id: ` and the value the request sent, quoted.
fn ec2_sentence(value: &str) -> String {
    format!("Invalid id: \"{value}\"")
}

/// What the relayed fault reads BEFORE the id of the call that answered it.
fn relayed_opening(value: &str) -> String {
    format!(
        "{} (Service: {EC2_SERVICE_NAME}; Status Code: 400; Error Code: \
         {GROUP_ID_MALFORMED}; Request ID: ",
        ec2_sentence(value)
    )
}

/// The id of the relayed call named inside `message`, asserted to be a non-empty value the
/// clause carries exactly one of.
///
/// `case` and `value` only name the row in the assertion messages.
fn relayed_request_id(case: &str, value: &str, message: &str) -> String {
    let opening = relayed_opening(value);
    assert!(
        message.starts_with(&opening) && message.ends_with(CLAUSE_TAIL),
        "case {case:?}: expected a message opening {opening:?} and ending {CLAUSE_TAIL:?}, \
         got {message:?}"
    );
    let request_id = message[opening.len()..message.len() - CLAUSE_TAIL.len()].to_string();
    assert!(
        !request_id.is_empty() && !request_id.contains(char::is_whitespace),
        "case {case:?}: expected the clause to name the id of the relayed call, got \
         {request_id:?}"
    );
    request_id
}

/// Create one internal application load balancer in the seeded network, carrying `groups`,
/// and answer the whole response.
fn create(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    network: &Network,
    name: &str,
    groups: &[&str],
) -> Answer {
    let mut params = vec![
        ("Name", name),
        ("Scheme", "internal"),
        ("Subnets.member.1", network.subnets[0].as_str()),
        ("Subnets.member.2", network.subnets[1].as_str()),
    ];
    let keys = ["SecurityGroups.member.1", "SecurityGroups.member.2"];
    for (key, id) in keys.iter().zip(groups.iter()) {
        params.push((key, id));
    }
    elbv2(router, runtime, "CreateLoadBalancer", &params)
}

#[test]
fn a_value_that_is_no_security_group_id_relays_the_fault_ec2_answered() {
    std::thread::Builder::new()
        .name("elbv2-security-group-relay".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                // See the sibling `elbv2_dualstack_and_security_group_classes` suite: the IO
                // and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            let network = seed(&router, &runtime);

            // The load balancer the `SetSecurityGroups` half of every row runs against,
            // created with the group of its own VPC.
            let held = create(
                &router,
                &runtime,
                &network,
                "relay-set-groups",
                &[network.security_group.as_str()],
            );
            assert_eq!(
                held.status, 200,
                "setup: the load balancer whose groups the rows replace must be created, got \
                 {held:?}"
            );
            let arn = element(&held.text, "LoadBalancerArn");

            let mut request_ids: Vec<String> = Vec::new();
            for (case, value) in MALFORMED_VALUES {
                // EC2's own answer about the value: refused as malformed, naming the value,
                // BEFORE the account's groups are read — so the id is judged the same way
                // whether the account holds a group or not.
                let described = ec2(
                    &router,
                    &runtime,
                    "DescribeSecurityGroups",
                    &[("GroupId.1", value)],
                );
                assert_eq!(
                    refusal(&described),
                    (
                        400,
                        GROUP_ID_MALFORMED.to_string(),
                        ec2_sentence(value)
                    ),
                    "case {case:?}: expected EC2 to refuse the read 400 {GROUP_ID_MALFORMED} \
                     {:?}, got {described:?}",
                    ec2_sentence(value)
                );

                // The create: the modelled `InvalidSecurityGroup`, stating that fault whole.
                let name = format!("relay-create-{}", request_ids.len());
                let created = create(&router, &runtime, &network, &name, &[value]);
                assert_eq!(
                    (created.status, created.code()),
                    (400, INVALID_SECURITY_GROUP.to_string()),
                    "case {case:?}: expected 400 {INVALID_SECURITY_GROUP}, got {created:?}"
                );
                request_ids.push(relayed_request_id(case, value, &created.message()));
                // A refused create places no load balancer.
                let read = elbv2(
                    &router,
                    &runtime,
                    "DescribeLoadBalancers",
                    &[("Names.member.1", name.as_str())],
                );
                assert_eq!(
                    (read.status, read.code()),
                    (400, "LoadBalancerNotFound".to_string()),
                    "case {case:?}: a refused create must store no load balancer, got {read:?}"
                );

                // And the replacement, which judges the groups by the same rule: one good id
                // beside the value is refused for the value, so a row also covers a request
                // whose other group is usable.
                let replaced = elbv2(
                    &router,
                    &runtime,
                    "SetSecurityGroups",
                    &[
                        ("LoadBalancerArn", arn.as_str()),
                        ("SecurityGroups.member.1", network.security_group.as_str()),
                        ("SecurityGroups.member.2", value),
                    ],
                );
                assert_eq!(
                    (replaced.status, replaced.code()),
                    (400, INVALID_SECURITY_GROUP.to_string()),
                    "case {case:?}: expected the replacement refused 400 \
                     {INVALID_SECURITY_GROUP}, got {replaced:?}"
                );
                request_ids.push(relayed_request_id(case, value, &replaced.message()));
                // A refused replacement replaces nothing.
                let read = elbv2(
                    &router,
                    &runtime,
                    "DescribeLoadBalancers",
                    &[("LoadBalancerArns.member.1", arn.as_str())],
                );
                assert_eq!(
                    elements(&read.text, "member")
                        .iter()
                        .filter(|member| *member == &network.security_group)
                        .count(),
                    1,
                    "case {case:?}: expected the stored group {:?} to be untouched, got \
                     {read:?}",
                    network.security_group
                );
            }

            // Every relayed clause names the id of ITS OWN call: the request id is minted per
            // call, never a stored or derived value, so no two of the refusals above share
            // one.
            let mut unique = request_ids.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(
                unique.len(),
                request_ids.len(),
                "expected each relayed fault to name the id of its own call, got \
                 {request_ids:?}"
            );

            // -----------------------------------------------------------------
            // The controls: the classes the relayed fault does not swallow.
            // -----------------------------------------------------------------

            // (case, the groups the request names, the code it earns, the message it states)
            let controls: Vec<(&str, Vec<&str>, &str, &str)> = vec![
                (
                    "a WELL-FORMED id the account holds no group under",
                    vec![ABSENT_GROUP],
                    INVALID_CONFIGURATION,
                    INVALID_SECURITY_GROUPS,
                ),
                (
                    "a group of another VPC",
                    vec![network.other_security_group.as_str()],
                    INVALID_CONFIGURATION,
                    INVALID_SECURITY_GROUPS,
                ),
            ];
            for (index, (case, groups, code, message)) in controls.into_iter().enumerate() {
                let name = format!("relay-control-{index}");
                let answer = create(&router, &runtime, &network, &name, &groups);
                assert_eq!(
                    refusal(&answer),
                    (400, code.to_string(), message.to_string()),
                    "case {case:?}: expected 400 {code} {message:?}, got {answer:?}"
                );
            }

            // And the group of the load balancer's own VPC is served, so the rule above
            // refuses values rather than security groups.
            let served = create(
                &router,
                &runtime,
                &network,
                "relay-served",
                &[network.security_group.as_str()],
            );
            assert_eq!(
                served.status, 200,
                "case \"the group of the load balancer's own VPC\": expected the create \
                 served, got {served:?}"
            );
            let replaced = elbv2(
                &router,
                &runtime,
                "SetSecurityGroups",
                &[
                    ("LoadBalancerArn", arn.as_str()),
                    ("SecurityGroups.member.1", network.security_group.as_str()),
                ],
            );
            assert_eq!(
                replaced.status, 200,
                "case \"replacing with the group of the load balancer's own VPC\": expected \
                 200, got {replaced:?}"
            );
        })
        .expect("the suite thread starts")
        .join()
        .expect("the suite thread completes");
}
