// SPDX-License-Identifier: CC0-1.0

//! What Elastic Load Balancing v2 states as the MESSAGE of its `AllocationIdNotFound`.
//!
//! RULE: an Elastic IP address is EC2's resource, so an allocation id a `SubnetMapping` pins
//! is resolved by CALLING EC2 — and the refusal that call raised is what reaches the client:
//! the modelled `AllocationIdNotFound` (HTTP 400) carries as its message the EC2 fault
//! RELAYED WHOLE, which is EC2's own sentence about the allocation id the request supplied
//! followed by the clause an SDK renders around a failed call — the service that answered it,
//! the HTTP status and error code it answered with, and that call's own request id — closed
//! by `Proxy: null`. The refused request changes nothing.
//!
//! The rows differ along what the rule generalises over: the FORM of the value that names no
//! allocation (two distinct ids of EC2's own id form, and a value that is not an allocation
//! id at all, which EC2 answers under its other code), the OPERATION that pins it
//! (`CreateLoadBalancer` for the mapping a load balancer starts with, `SetSubnets` for the
//! mapping that replaces it), and how many mappings the request carries beside the refused
//! one — because the rule is about relaying the fault that call raised, not about one
//! recorded request.
//!
//! Nothing here is written by this suite: the subnets and the Elastic IP a case runs against
//! are minted by EC2's own operations on the SAME engine, and the sentence, status and error
//! code each assertion expects are read off what EC2 answers THIS suite for the same id —
//! so a row pins the relay rather than a transcription of EC2's wording. The two refusals a
//! row drives over the same id also pin that the clause names ONE call: the id is minted per
//! call, so the two never report the same one. That is why the suite lives beside the router:
//! it needs both services on one engine.
//!
//! Error source: `smithy_query(query="shape", shape="AllocationIdNotFoundException")` on the
//! ELBv2 model — `errorKind` client, `httpError` 400, `awsQueryError` code
//! `AllocationIdNotFound`.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{
    attach_internet_gateway, ec2, ec2_ok, elbv2, Answer, BOOT_STACK_SIZE,
};
use crate::query_wire::{element, opt_element};

/// The modelled awsQuery code of the refusal under test.
const ALLOCATION_ID_NOT_FOUND: &str = "AllocationIdNotFound";

/// How Amazon EC2 names ITSELF in the clause an SDK renders around a call made to it, which
/// is neither its endpoint prefix nor the casing of its `sdkId`.
const EC2_IN_A_RELAYED_FAULT: &str = "AmazonEC2";

// The two readers below are `pub(crate)` because the sibling suite
// `elbv2_subnet_and_elastic_ip_resolution.rs`, which tables WHICH class of refusal each
// value earns, asserts the message of its own allocation rows through them: one reading of
// the relayed form for the crate, so the two suites cannot drift into expecting two forms.

/// The close of that clause, after the raising call's own request id: there is no proxy
/// between two services of one emulator, and the clause states that as AWS's own SDK does.
const RELAYED_FAULT_TAIL: &str = "; Proxy: null)";

/// Ids of EC2's own allocation-id form that the account holds no Elastic IP for. Two of
/// them, so a row shows the id in the sentence comes from the REQUEST.
const ABSENT_ALLOCATIONS: [&str; 2] = [
    "eipalloc-00000000000000000",
    "eipalloc-0123456789abcdef0",
];

/// A value that is not an allocation id at all, which EC2 answers under its other
/// allocation-id code — so the relayed clause tells the two failures apart.
const MALFORMED_ALLOCATION: &str = "not-an-eip";

/// Whether `id` has the 8-4-4-4-12 hexadecimal form an AWS request id carries.
fn is_request_id(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != width || !group.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
    }
    groups.next().is_none()
}

/// What EC2 itself answers a read of `allocation_id`: the status, the error code and the
/// sentence — the three the relayed clause is built from, read from EC2 rather than
/// transcribed here.
pub(crate) struct FromEc2 {
    pub(crate) status: u16,
    pub(crate) code: String,
    pub(crate) message: String,
}

/// Ask EC2 about `allocation_id` the way this service's own lookup does, and answer what it
/// refused the read with.
pub(crate) fn refused_by_ec2(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    allocation_id: &str,
) -> FromEc2 {
    let answer = ec2(
        router,
        runtime,
        "DescribeAddresses",
        &[("AllocationId.1", allocation_id)],
    );
    assert_eq!(
        answer.status, 400,
        "setup: EC2 must refuse a read of {allocation_id:?}, got {answer:?}"
    );
    let from_ec2 = FromEc2 {
        status: answer.status,
        code: answer.code(),
        message: answer.message(),
    };
    assert!(
        from_ec2.message.contains(allocation_id),
        "setup: EC2's own sentence must name {allocation_id:?}, got {:?}",
        from_ec2.message
    );
    from_ec2
}

/// Assert one ELBv2 refusal relays the EC2 fault whole, and answer the request id the
/// relayed clause named.
pub(crate) fn assert_relays_the_ec2_fault(
    case: &str,
    answer: &Answer,
    from_ec2: &FromEc2,
) -> String {
    assert_eq!(
        (answer.status, answer.code()),
        (400, ALLOCATION_ID_NOT_FOUND.to_string()),
        "case {case:?}: expected 400 {ALLOCATION_ID_NOT_FOUND}, got {answer:?}"
    );
    let stated = answer.message();
    let head = format!(
        "{} (Service: {EC2_IN_A_RELAYED_FAULT}; Status Code: {}; Error Code: {}; Request ID: ",
        from_ec2.message, from_ec2.status, from_ec2.code
    );
    let relayed = stated
        .strip_prefix(&head)
        .and_then(|tail| tail.strip_suffix(RELAYED_FAULT_TAIL))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the EC2 fault relayed as \
                 {head:?}<request id>{RELAYED_FAULT_TAIL:?}, got {stated:?}"
            )
        });
    assert!(
        is_request_id(relayed),
        "case {case:?}: expected the relayed clause to name a request id, got {relayed:?}"
    );
    // The relayed id names the EC2 call, not this response: a caller reading both sees two
    // calls rather than one id reported twice.
    assert_ne!(
        Some(relayed.to_string()),
        opt_element(&answer.text, "RequestId"),
        "case {case:?}: the relayed clause must name the EC2 call, not this response"
    );
    relayed.to_string()
}

/// One row: the case, the value the refused mapping pins, and how many mappings the request
/// carries in all.
struct Row {
    case: &'static str,
    pinned: &'static str,
    /// Whether a mapping holding an allocation EC2 DOES hold travels in the same request, so
    /// the refusal is relayed for a read that named several ids.
    beside_a_held_allocation: bool,
}

/// The values and shapes a refused mapping takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an id EC2 holds no allocation for",
            pinned: ABSENT_ALLOCATIONS[0],
            beside_a_held_allocation: false,
        },
        Row {
            case: "another id EC2 holds no allocation for",
            pinned: ABSENT_ALLOCATIONS[1],
            beside_a_held_allocation: false,
        },
        Row {
            case: "a value that is not an allocation id at all",
            pinned: MALFORMED_ALLOCATION,
            beside_a_held_allocation: false,
        },
        Row {
            case: "an absent id beside an allocation EC2 holds",
            pinned: ABSENT_ALLOCATIONS[0],
            beside_a_held_allocation: true,
        },
    ]
}

#[test]
fn an_elastic_ip_refusal_relays_the_fault_the_ec2_lookup_raised() {
    std::thread::Builder::new()
        .name("elbv2-relayed-elastic-ip-allocation-fault".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs, as the sibling
                // `elbv2_subnet_and_elastic_ip_resolution` suite records.
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // The network every case runs against, minted by EC2 itself. An Elastic IP is
            // pinned on an internet-facing Network Load Balancer, which can only be placed
            // in a VPC that has an internet gateway attached.
            let vpc = element(
                &ec2_ok(&router, &runtime, "CreateVpc", &[("CidrBlock", "10.71.0.0/16")]),
                "vpcId",
            );
            let mut subnets: Vec<String> = Vec::new();
            for (cidr, zone) in [("10.71.1.0/24", "us-west-2a"), ("10.71.2.0/24", "us-west-2b")] {
                subnets.push(element(
                    &ec2_ok(
                        &router,
                        &runtime,
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
            let mut held: Vec<String> = Vec::new();
            for _ in 0..2 {
                held.push(element(
                    &ec2_ok(&router, &runtime, "AllocateAddress", &[("Domain", "vpc")]),
                    "allocationId",
                ));
            }
            attach_internet_gateway(&router, &runtime, &vpc);

            // The load balancer whose mapping `SetSubnets` then tries to replace: both of
            // its placements pin an Elastic IP EC2 handed out.
            let fixture = elbv2(
                &router,
                &runtime,
                "CreateLoadBalancer",
                &[
                    ("Name", "relayed-eip-fault"),
                    ("Type", "network"),
                    ("Scheme", "internet-facing"),
                    ("SubnetMappings.member.1.SubnetId", subnets[0].as_str()),
                    ("SubnetMappings.member.1.AllocationId", held[0].as_str()),
                    ("SubnetMappings.member.2.SubnetId", subnets[1].as_str()),
                    ("SubnetMappings.member.2.AllocationId", held[1].as_str()),
                ],
            );
            assert_eq!(
                fixture.status, 200,
                "setup: the fixture must be created, got {fixture:?}"
            );
            let arn = element(&fixture.text, "LoadBalancerArn");

            for (index, row) in rows().iter().enumerate() {
                let from_ec2 = refused_by_ec2(&router, &runtime, row.pinned);

                // The mapping a load balancer STARTS with.
                let name = format!("relayed-eip-create-{index}");
                let mut created = vec![
                    ("Name", name.as_str()),
                    ("Type", "network"),
                    ("Scheme", "internet-facing"),
                    ("SubnetMappings.member.1.SubnetId", subnets[0].as_str()),
                    ("SubnetMappings.member.1.AllocationId", row.pinned),
                ];
                if row.beside_a_held_allocation {
                    created.push(("SubnetMappings.member.2.SubnetId", subnets[1].as_str()));
                    created.push(("SubnetMappings.member.2.AllocationId", held[1].as_str()));
                }
                let answer = elbv2(&router, &runtime, "CreateLoadBalancer", &created);
                let on_the_create = assert_relays_the_ec2_fault(
                    &format!("CreateLoadBalancer with {}", row.case),
                    &answer,
                    &from_ec2,
                );
                // A refused create stores nothing: the name is still free.
                let read = elbv2(
                    &router,
                    &runtime,
                    "DescribeLoadBalancers",
                    &[("Names.member.1", name.as_str())],
                );
                assert_eq!(
                    (read.status, read.code()),
                    (400, "LoadBalancerNotFound".to_string()),
                    "case {:?}: a refused create must store nothing, got {read:?}",
                    row.case
                );

                // The mapping that REPLACES one, which is where the fixture's own pinned
                // addresses have to survive the refusal.
                let mut replaced = vec![
                    ("LoadBalancerArn", arn.as_str()),
                    ("SubnetMappings.member.1.SubnetId", subnets[0].as_str()),
                    ("SubnetMappings.member.1.AllocationId", row.pinned),
                ];
                if row.beside_a_held_allocation {
                    replaced.push(("SubnetMappings.member.2.SubnetId", subnets[1].as_str()));
                    replaced.push(("SubnetMappings.member.2.AllocationId", held[1].as_str()));
                }
                let answer = elbv2(&router, &runtime, "SetSubnets", &replaced);
                let on_the_replace = assert_relays_the_ec2_fault(
                    &format!("SetSubnets with {}", row.case),
                    &answer,
                    &from_ec2,
                );
                // The clause names ONE EC2 call, so the two refusals this row drove over the
                // same unresolvable id name two calls: the id is minted per call rather than
                // derived from the request, which is what lets a caller find each attempt in
                // EC2's own records.
                assert_ne!(
                    on_the_create, on_the_replace,
                    "case {:?}: expected each relayed clause to name its own EC2 call, got \
                     {on_the_create:?} twice",
                    row.case
                );
                // The refused call left the load balancer's own mapping where it was: both
                // of the allocations it was created with are still reported for it.
                let after = elbv2(
                    &router,
                    &runtime,
                    "DescribeLoadBalancers",
                    &[("LoadBalancerArns.member.1", arn.as_str())],
                );
                for pinned in &held {
                    assert!(
                        after.text.contains(pinned.as_str()),
                        "case {:?}: a refused SetSubnets must leave the mapping as it was, \
                         expected {pinned:?} to still be reported, got {}",
                        row.case,
                        after.text
                    );
                }
                assert!(
                    !after.text.contains(row.pinned),
                    "case {:?}: the refused value must not be stored, got {}",
                    row.case,
                    after.text
                );
            }
        })
        .expect("the suite thread starts")
        .join()
        .expect("the suite thread completes");
}
