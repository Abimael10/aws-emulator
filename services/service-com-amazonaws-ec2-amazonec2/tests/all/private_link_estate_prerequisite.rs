// SPDX-License-Identifier: CC0-1.0

//! The PRIVATE ESTATE a client stands up in front of a PrivateLink-dependent workflow, driven
//! end to end over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: an interface VPC endpoint to a published endpoint service of Amazon Web
//! Services whose catalogue entry carries a default public DNS name — "the default public DNS
//! name for the service for the Region (for example, kinesis.us-east-1.amazonaws.com)"
//! (`smithy_query(query="doc",shape="CreateVpcEndpointRequest",member="PrivateDnsEnabled")`) —
//! is CREATED with `PrivateDnsEnabled=true` and reaches `available`, whichever name family the
//! service is published under.
//!
//! What this suite covers that its siblings do not: the WHOLE prerequisite a client makes in ONE
//! sequence, per name family — the catalogue announces the name, the create carrying the security
//! group and the tags a client sends is served, the endpoint answers on that name, a readiness
//! poll ends at `available`, and the tags the create assigned are readable from the endpoint that
//! is then in service. The halves of it live in siblings that each take one step:
//! `endpoint_service_private_dns_names.rs` pins the names the catalogue reports and the refusal
//! a service with none earns, `vpc_endpoint_private_dns_domains.rs` the domain a private hosted
//! zone answers, and `vpc_endpoint_readiness_poll.rs` the vocabulary a poll terminates on.
//!
//! The rows differ along what the rule generalises over: the name family, whether the service's
//! label is dotted, how many subnets the endpoint is served in, whether a security group is
//! named, and whether the create assigns tags. None of those decides whether private DNS may be
//! enabled — the service's own catalogue entry does.

use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, indexed, tag_report, tag_specification};
use super::wire::{child, document, param, Param};

/// The Region every call here is signed for, and the Region of every endpoint service below.
const REGION: &str = "eu-west-1";

/// How long a readiness poll is given before it reports the endpoint never left the
/// non-terminal state: generously past [`ec2::state::TRANSITION_DELAY`], the span the clock owes
/// the move, so a loaded or slow host fails nothing, while a state the loop's guard never matches
/// still ends the test instead of hanging. The same bound the sibling poll suite takes.
const POLL_DEADLINE: Duration = Duration::from_secs(10);

/// How long the poll waits between reads, as a client's own waiter does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// Every `dnsName` of a response's `dnsEntrySet`, read through the XML parser so one set's
/// entries are never read as another's.
fn dns_names(case: &str, body: &str) -> Vec<String> {
    document(case, body)
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "dnsEntrySet")
        .map(|set| {
            set.children()
                .filter(|item| item.is_element() && item.tag_name().name() == "item")
                .filter_map(|item| child(item, "dnsName"))
                .map(|found| found.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The estate every row is created in
// ---------------------------------------------------------------------------

/// The private network a client builds first: a VPC with both DNS attributes on, two subnets in
/// different Availability Zones, and a security group of its own.
struct Estate {
    vpc_id: String,
    subnet_ids: Vec<String>,
    group_id: String,
}

/// Build that estate through the API, so every endpoint below is created against resources the
/// emulator really stores.
///
/// "To use private DNS, the VPC must have DNS hostnames and DNS resolution enabled"
/// (`CreateVpcEndpointRequest$PrivateDnsEnabled`), and a VPC a caller creates has hostnames OFF
/// (Amazon VPC User Guide, "DNS attributes in your VPC") — so the estate turns both on, the way
/// a client does, leaving the rows free to be about the endpoint.
async fn estate(case: &str, router: &MultiServiceRouter) -> Estate {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", "10.61.0.0/16")]).await;
    let vpc_id = member(case, &body, "vpcId");
    for attribute in ["EnableDnsSupport", "EnableDnsHostnames"] {
        ok(
            case,
            router,
            "ModifyVpcAttribute",
            &[
                param("VpcId", &vpc_id),
                param(&format!("{attribute}.Value"), "true"),
            ],
        )
        .await;
    }
    let mut subnet_ids = Vec::new();
    for (index, letter) in ["a", "b"].iter().enumerate() {
        let body = ok(
            case,
            router,
            "CreateSubnet",
            &[
                param("VpcId", &vpc_id),
                param("CidrBlock", &format!("10.61.{}.0/24", index + 1)),
                param("AvailabilityZone", &format!("{REGION}{letter}")),
            ],
        )
        .await;
        subnet_ids.push(member(case, &body, "subnetId"));
    }
    let body = ok(
        case,
        router,
        "CreateSecurityGroup",
        &[
            param("VpcId", &vpc_id),
            param("GroupName", "private-link-estate"),
            param("GroupDescription", "the group the estate suite created"),
        ],
    )
    .await;
    let group_id = member(case, &body, "groupId");
    Estate {
        vpc_id,
        subnet_ids,
        group_id,
    }
}

// ---------------------------------------------------------------------------
// The rows: one published service per name family
// ---------------------------------------------------------------------------

/// One row: a published endpoint service with a default public DNS name, and the shape of the
/// create a client makes for it.
struct EstateRow {
    case: &'static str,
    /// The service's whole name, in its own family's form.
    service_name: &'static str,
    /// The default public DNS name the catalogue publishes for it, and the name the endpoint's
    /// private hosted zone then answers on. Written out here rather than derived, so the suite
    /// pins the names and not the code that builds them.
    private_dns_name: &'static str,
    /// How many of the estate's subnets the create names.
    subnets: usize,
    /// Whether the create names the estate's security group.
    with_group: bool,
    /// The tags the create assigns, which a client sends in `TagSpecifications`.
    tags: &'static [(&'static str, &'static str)],
}

/// The rows, one per published name family, plus a dotted label so the reverse-domain reading of
/// a label is exercised beside the plain one. The source of each name is the Region's own
/// catalogue, which `DescribeVpcEndpointServices` announces — asserted per row below, so no row
/// names a service no Region offers.
static ESTATE_ROWS: [EstateRow; 4] = [
    EstateRow {
        // The service cell a client reaches interactive Amazon EMR sessions through, published
        // under the `aws.api` label rather than under `com.amazonaws`.
        case: "a service of the aws.api family, with private DNS, a group and tags",
        service_name: "aws.api.eu-west-1.emr-service-cell01",
        private_dns_name: "emr-service-cell01.eu-west-1.api.aws",
        subnets: 1,
        with_group: true,
        tags: &[("Name", "emr-session-endpoint"), ("Estate", "private")],
    },
    EstateRow {
        // Published under `api.aws`, the other of the two families named in that domain.
        case: "a service of the api.aws family, in two subnets with no group",
        service_name: "api.aws.eu-west-1.cassandra-streams",
        private_dns_name: "cassandra-streams.eu-west-1.api.aws",
        subnets: 2,
        with_group: false,
        tags: &[],
    },
    EstateRow {
        case: "a one-component label under com.amazonaws",
        service_name: "com.amazonaws.eu-west-1.sts",
        private_dns_name: "sts.eu-west-1.amazonaws.com",
        subnets: 1,
        with_group: true,
        tags: &[("Estate", "private")],
    },
    EstateRow {
        case: "a dotted label under com.amazonaws, read as reverse domain",
        service_name: "com.amazonaws.eu-west-1.ecr.dkr",
        private_dns_name: "dkr.ecr.eu-west-1.amazonaws.com",
        subnets: 2,
        with_group: true,
        tags: &[("Name", "registry-endpoint")],
    },
];

/// The form fields of the `CreateVpcEndpoint` a client makes for `row` in `built`.
fn create_params(row: &EstateRow, built: &Estate) -> Vec<Param> {
    let mut params = vec![
        param("VpcId", &built.vpc_id),
        param("ServiceName", row.service_name),
        param("VpcEndpointType", "Interface"),
        param("PrivateDnsEnabled", "true"),
    ];
    let chosen: Vec<&str> = built
        .subnet_ids
        .iter()
        .take(row.subnets)
        .map(String::as_str)
        .collect();
    params.extend(indexed("SubnetId", &chosen));
    if row.with_group {
        params.extend(indexed("SecurityGroupId", &[built.group_id.as_str()]));
    }
    params.extend(tag_specification(
        ec2::state::RESOURCE_TYPE_VPC_ENDPOINT,
        row.tags,
    ));
    params
}

/// The state `DescribeVpcEndpoints` reports for the one endpoint `endpoint_id`.
async fn described_state(case: &str, router: &MultiServiceRouter, endpoint_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[endpoint_id]),
    )
    .await;
    member(case, &body, "state")
}

/// Poll `DescribeVpcEndpoints` while the endpoint reports the non-terminal state, exactly as a
/// client's readiness waiter does, and answer the state it finally reported.
///
/// The loop waits for the CONDITION rather than for a span: a state the vocabulary spells
/// otherwise leaves it at once, and [`POLL_DEADLINE`] only keeps a stuck endpoint from hanging
/// the suite.
async fn poll_until_not_pending(
    case: &str,
    router: &MultiServiceRouter,
    endpoint_id: &str,
) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut state = described_state(case, router, endpoint_id).await;
    while state == ec2::state::VPC_ENDPOINT_STATE_PENDING && Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        state = described_state(case, router, endpoint_id).await;
    }
    state
}

/// The whole prerequisite, per row: the catalogue announces the service's private DNS name, the
/// create carrying `PrivateDnsEnabled=true` is SERVED, the endpoint answers on that name, a
/// readiness poll ends at `available`, and the tags the create assigned are readable.
#[tokio::test]
async fn an_interface_endpoint_with_private_dns_is_served_and_reaches_available() {
    let router = super::ec2_query::emulator().0;
    let available = ec2::state::VPC_ENDPOINT_STATE_AVAILABLE;
    let pending = ec2::state::VPC_ENDPOINT_STATE_PENDING;
    let built = estate("the estate", &router).await;

    let mut created: Vec<(&'static str, &'static EstateRow, String)> = Vec::new();
    for row in &ESTATE_ROWS {
        let case = row.case;

        // The catalogue announces the service, with the private DNS name a private hosted zone
        // of it is the record set of — so the name the row expects is the Region's, not the
        // suite's invention.
        let announced = ok(
            case,
            &router,
            "DescribeVpcEndpointServices",
            &indexed("ServiceName", &[row.service_name]),
        )
        .await;
        assert_eq!(
            element(&announced, "serviceName").as_deref(),
            Some(row.service_name),
            "case {case:?}: the catalogue must announce the service asked about, got {announced}"
        );
        assert_eq!(
            element(&announced, "privateDnsName").as_deref(),
            Some(row.private_dns_name),
            "case {case:?}: expected the private DNS name {:?}, got {announced}",
            row.private_dns_name
        );

        // The create a client makes is served, and answers the endpoint it stored.
        let body = ok(
            case,
            &router,
            "CreateVpcEndpoint",
            &create_params(row, &built),
        )
        .await;
        let endpoint_id = member(case, &body, "vpcEndpointId");
        assert_eq!(
            element(&body, "privateDnsEnabled").as_deref(),
            Some("true"),
            "case {case:?}: expected the endpoint to report the private hosted zone, got {body}"
        );
        let answered_state = member(case, &body, "state");
        assert_eq!(
            answered_state, pending,
            "case {case:?}: expected the create to answer {pending:?}, got \
             {answered_state:?} in {body}"
        );
        let names = dns_names(case, &body);
        assert!(
            names.iter().any(|name| name == row.private_dns_name),
            "case {case:?}: expected the service's public name {:?} among {names:?}",
            row.private_dns_name
        );
        created.push((case, row, endpoint_id));
    }

    // The readiness poll each client then makes, on an endpoint nothing but the clock touched.
    for (case, row, endpoint_id) in &created {
        let polled = poll_until_not_pending(case, &router, endpoint_id).await;
        assert_eq!(
            polled, available,
            "case {case:?}: expected the poll on {endpoint_id:?} to end at {available:?}, got \
             {polled:?}"
        );

        // What the endpoint holds once it is in service: the private hosted zone, the name it
        // answers on, and the tags the create assigned.
        let described = ok(
            case,
            &router,
            "DescribeVpcEndpoints",
            &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
        )
        .await;
        assert_eq!(
            element(&described, "privateDnsEnabled").as_deref(),
            Some("true"),
            "case {case:?}: the stored endpoint must report the association, got {described}"
        );
        let names = dns_names(case, &described);
        assert!(
            names.iter().any(|name| name == row.private_dns_name),
            "case {case:?}: expected {:?} among the stored {names:?}",
            row.private_dns_name
        );
        let reported = tag_report(&router, REGION, case, endpoint_id).await;
        let expected: std::collections::BTreeSet<super::ec2_query::DescribedTag> = row
            .tags
            .iter()
            .map(|(key, value)| {
                (
                    endpoint_id.clone(),
                    ec2::state::RESOURCE_TYPE_VPC_ENDPOINT.to_string(),
                    (*key).to_string(),
                    (*value).to_string(),
                )
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the tags the create assigned, got {reported:?}"
        );
    }
}

