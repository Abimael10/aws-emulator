// SPDX-License-Identifier: CC0-1.0

//! A service-managed OpenSearch VPC endpoint carries security groups the account HOLDS.
//!
//! RULE: `VpcOptions$SecurityGroupIds` names security groups of the service that owns them,
//! so a well-formed identifier that service answers for NO group names nothing the
//! endpoint's network interfaces could carry: the call is refused synchronously with the
//! modelled `ValidationException` (HTTP 400) stating `Invalid id: "<id>"` — the fixed text
//! `Invalid id: ` and the offending identifier from the REQUEST in double quotes — and
//! nothing is written, so a read still reports the endpoint's original status and security
//! groups. The same call naming groups the account does hold is served (200) and the endpoint
//! settles carrying them. Both calls that take `VpcOptions` for an endpoint are judged by it:
//! `CreateVpcEndpoint` and `UpdateVpcEndpoint`.
//!
//! A security group is Amazon EC2's resource, so whether the account holds one is EC2's to
//! say: this suite lives beside the router because every VPC, subnet and group below is
//! minted by the real EC2 operations on the SAME engine that serves OpenSearch Service, so
//! every refusal is grounded in what EC2 actually answered and every accepted identifier is
//! one EC2 handed out. The wire harness is the one
//! [`crate::opensearch_vpc_endpoint_vpc_membership`] publishes. Where no such service is
//! mounted nothing is resolved and nothing is refused, which the service crate's own
//! `vpc_security_groups_grounded_in_ec2` suite drives; the FORM an identifier has to take is
//! decided without resolving anything and is driven there too.
//!
//! The rows differ along what the rule generalises over: which identifier form the unknown
//! value takes (the long and the short one EC2 issues), how many groups the call names, where
//! in the list the unknown one sits, and whether the groups the account does hold are a
//! client's own or the default group EC2 makes with every VPC.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::opensearch_vpc_endpoint_vpc_membership::{
    ec2, emulator, opensearch, security_group, vpc_with_subnets, Reply, PREFIX,
};
use crate::query_wire::element;

/// The engine release the domain below runs — one the service's catalog publishes for the
/// instance type it uses.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// The name EC2 gives the group it creates with every VPC, which is how the default group of
/// a VPC is READ here rather than assumed ("your VPC automatically comes with a default
/// security group", Amazon VPC User Guide).
const DEFAULT_GROUP_NAME: &str = "default";

/// Well-formed identifiers no `CreateSecurityGroup` below handed out: EC2 answers for no
/// group under either, in each of the two identifier widths it issues.
const UNKNOWN_LONG: &str = "sg-0baadf00d00000001";
const UNKNOWN_SHORT: &str = "sg-0baadf01";

/// How long a poll waits for a span the clock owes an endpoint or a domain to fall due.
///
/// Generous on purpose: the condition is what is waited for, not a duration, so a loaded host
/// only makes the loop go round more times.
const READY_DEADLINE: Duration = Duration::from_secs(120);

/// How long one turn of a poll loop pauses before looking again.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// How AWS words a refusal of an identifier it holds no security group for: fixed text and
/// the value from the request in double quotes. Spelled out here rather than taken from the
/// crate, so a change to either part fails this suite.
fn invalid_id(value: &str) -> String {
    format!("Invalid id: \"{value}\"")
}

/// `CreateVpcEndpoint` for `domain_arn`, as it answered.
async fn create_endpoint(
    router: &MultiServiceRouter,
    domain_arn: &str,
    subnets: &[String],
    groups: &[String],
) -> Reply {
    opensearch(
        router,
        "POST",
        &format!("{PREFIX}/opensearch/vpcEndpoints"),
        Some(json!({
            "DomainArn": domain_arn,
            "VpcOptions": { "SubnetIds": subnets, "SecurityGroupIds": groups },
        })),
    )
    .await
}

/// `UpdateVpcEndpoint` with the `VpcOptions` a row names, as it answered.
async fn update_endpoint(
    router: &MultiServiceRouter,
    id: &str,
    subnets: &[String],
    groups: &[String],
) -> Reply {
    opensearch(
        router,
        "POST",
        &format!("{PREFIX}/opensearch/vpcEndpoints/update"),
        Some(json!({
            "VpcEndpointId": id,
            "VpcOptions": { "SubnetIds": subnets, "SecurityGroupIds": groups },
        })),
    )
    .await
}

/// The endpoint `DescribeVpcEndpoints` reports for `id`.
async fn read_endpoint(router: &MultiServiceRouter, id: &str) -> Value {
    let read = opensearch(
        router,
        "POST",
        &format!("{PREFIX}/opensearch/vpcEndpoints/describe"),
        Some(json!({ "VpcEndpointIds": [id] })),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "reading endpoint {id:?}: expected 200, got {} with {}",
        read.status, read.body
    );
    read.body["VpcEndpoints"][0].clone()
}

/// How many endpoints `ListVpcEndpoints` reports for this account.
async fn listed_endpoint_count(router: &MultiServiceRouter) -> usize {
    let listed = opensearch(
        router,
        "GET",
        &format!("{PREFIX}/opensearch/vpcEndpoints"),
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "a listing of this account's endpoints must be served, got {} with {}",
        listed.status, listed.body
    );
    listed.body["VpcEndpointSummaryList"]
        .as_array()
        .map_or(0, Vec::len)
}

/// The `SecurityGroupIds` an endpoint reports, in the order it reports them.
fn endpoint_groups(endpoint: &Value) -> Vec<String> {
    endpoint["VpcOptions"]["SecurityGroupIds"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// Look again until the endpoint rests in `ACTIVE`, so the next change is served on it, or a
/// generous deadline passes.
async fn await_active_endpoint(router: &MultiServiceRouter, id: &str) -> Value {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let endpoint = read_endpoint(router, id).await;
        if endpoint["Status"] == json!("ACTIVE") {
            return endpoint;
        }
        assert!(
            Instant::now() < deadline,
            "the endpoint {id:?} never became active: {endpoint}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Look again until the domain reports itself created with nothing in flight.
async fn await_settled_domain(router: &MultiServiceRouter, name: &str) -> Value {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let read = opensearch(
            router,
            "GET",
            &format!("{PREFIX}/opensearch/domain/{name}"),
            None,
        )
        .await;
        let status = read.body["DomainStatus"].clone();
        if status["Created"] == json!(true) && status["Processing"] == json!(false) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the domain {name:?} never settled: {status}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Assert `answer` is the modelled refusal naming `offending`.
fn assert_refused(answer: &Reply, offending: &str, case: &str) {
    assert_eq!(
        (answer.status, answer.error_code()),
        (400, "ValidationException"),
        "case {case:?}: expected a 400 ValidationException, got {} with {}",
        answer.status,
        answer.body
    );
    assert_eq!(
        answer.message(),
        invalid_id(offending),
        "case {case:?}: expected the sentence naming {offending:?}, got {}",
        answer.body
    );
}

#[tokio::test]
async fn a_vpc_endpoint_call_refuses_a_security_group_the_account_does_not_hold() {
    let router = emulator();

    // One VPC with three subnets, a group a client created in it and the default group EC2
    // made with it — every identifier below is one EC2 handed out, or one it answers for
    // nothing about.
    let (vpc_id, subnets) =
        vpc_with_subnets(&router, "10.150", &["us-west-2a", "us-west-2b", "us-west-2c"]).await;
    let named_group = security_group(&router, &vpc_id, "opensearch-sg-exists").await;
    let default_group = element(
        &ec2(
            &router,
            "DescribeSecurityGroups",
            &[
                ("Filter.1.Name", "vpc-id"),
                ("Filter.1.Value.1", vpc_id.as_str()),
                ("Filter.2.Name", "group-name"),
                ("Filter.2.Value.1", DEFAULT_GROUP_NAME),
            ],
        )
        .await,
        "groupId",
    );

    let domain = "sg-exists-domain";
    let created = opensearch(
        &router,
        "POST",
        &format!("{PREFIX}/opensearch/domain"),
        Some(json!({
            "DomainName": domain,
            "EngineVersion": ENGINE_VERSION,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "VPCOptions": {
                "SubnetIds": [subnets[0].clone()],
                "SecurityGroupIds": [named_group.clone()],
            },
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateDomain({domain:?}) must be served, got {} with {}",
        created.status, created.body
    );
    let domain_arn = await_settled_domain(&router, domain).await["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the domain reported no ARN"))
        .to_owned();

    // THE CREATE half: a create naming an identifier EC2 holds no group for mints nothing,
    // over the rows that differ in which identifier width the unknown value takes, how many
    // groups the call names and where the unknown one sits.
    let held_before = listed_endpoint_count(&router).await;
    let refused_rows: [(&str, Vec<String>, &str); 4] = [
        (
            "an identifier of the long form the account holds no group for",
            vec![UNKNOWN_LONG.to_string()],
            UNKNOWN_LONG,
        ),
        (
            "an identifier of the short form the account holds no group for",
            vec![UNKNOWN_SHORT.to_string()],
            UNKNOWN_SHORT,
        ),
        (
            "an unknown identifier after a group the account holds",
            vec![named_group.clone(), UNKNOWN_LONG.to_string()],
            UNKNOWN_LONG,
        ),
        (
            "an unknown identifier before a group the account holds",
            vec![UNKNOWN_SHORT.to_string(), default_group.clone()],
            UNKNOWN_SHORT,
        ),
    ];
    for (case, groups, offending) in &refused_rows {
        let refused = create_endpoint(&router, &domain_arn, &subnets[..1], groups).await;
        assert_refused(&refused, offending, case);
        assert_eq!(
            listed_endpoint_count(&router).await,
            held_before,
            "case {case:?}: a refused create must mint no endpoint"
        );
    }

    // ... while a create naming groups the account DOES hold is served.
    let own_groups = vec![named_group.clone(), default_group.clone()];
    let served = create_endpoint(&router, &domain_arn, &subnets[..2], &own_groups).await;
    assert_eq!(
        served.status, 200,
        "case a create naming two groups the account holds: expected it to be served, got {} \
         with {}",
        served.status, served.body
    );
    let id = served.body["VpcEndpoint"]["VpcEndpointId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reported no VpcEndpointId: {}", served.body))
        .to_owned();
    let active = await_active_endpoint(&router, &id).await;
    assert_eq!(
        endpoint_groups(&active),
        own_groups,
        "the endpoint must carry the groups the create named, got {active}"
    );

    // THE CHANGE half of the same rule: every refused change leaves the endpoint standing —
    // its options and the status it was resting in — and a change naming a group the account
    // holds is served and settles with it.
    for (case, groups, offending) in &refused_rows {
        let before = read_endpoint(&router, &id).await;
        let answer = update_endpoint(&router, &id, &subnets[..1], groups).await;
        assert_refused(&answer, offending, case);
        let after = read_endpoint(&router, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused change must leave the endpoint standing, got {after}"
        );
    }

    let moved = vec![default_group.clone()];
    let answer = update_endpoint(&router, &id, &subnets[..1], &moved).await;
    assert_eq!(
        answer.status, 200,
        "case a change naming the VPC's default group: expected it to be served, got {} with \
         {}",
        answer.status, answer.body
    );
    let settled = await_active_endpoint(&router, &id).await;
    assert_eq!(
        endpoint_groups(&settled),
        moved,
        "the endpoint must carry the group the change named, got {settled}"
    );
}
