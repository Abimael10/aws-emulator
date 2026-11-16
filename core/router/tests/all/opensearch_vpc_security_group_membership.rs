// SPDX-License-Identifier: CC0-1.0

//! A VPC-access Amazon OpenSearch Service placement carries security groups of its OWN VPC.
//!
//! RULE: a placement created from `VPCOptions` is reached through network interfaces that
//! live in the subnets it names, and a security group governs traffic only to interfaces of
//! the VPC it belongs to — so the groups a request names must belong to the VPC the
//! placement is in, and a request naming one the account's network puts in another VPC is
//! refused with the modelled `ValidationException` (HTTP 400), creating and changing
//! nothing. Every operation that carries `VPCOptions` is judged by it: `CreateDomain` and
//! `UpdateDomainConfig` for a domain, `CreateVpcEndpoint` and `UpdateVpcEndpoint` for a
//! service-managed endpoint.
//!
//! A subnet and a security group are both Amazon EC2's resources, so which VPC each belongs
//! to is EC2's to say: this suite lives beside the router because every VPC, subnet and
//! group below is minted by the real EC2 operations on the SAME engine that serves
//! OpenSearch Service, so every identifier in a request is one EC2 handed out and every
//! refusal is grounded in what EC2 actually answered. The wire harness is the one
//! [`crate::opensearch_vpc_endpoint_vpc_membership`] publishes, which drives the one-VPC
//! rule about the SUBNETS of a placement.
//!
//! The rows differ along what the rule generalises over: which VPC a named group belongs to,
//! how many groups a request names and in which position the foreign one sits, whether the
//! group is one a client created or the default group EC2 makes with every VPC, whether an
//! identifier resolves to a group at all, whether the request names groups in the first
//! place, and which of the estate's VPCs the placement itself is in. Each refusal is
//! followed by a served call, so a refused one is shown to leave the operation working.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::opensearch_vpc_endpoint_vpc_membership::{
    ec2, emulator, opensearch, vpc_with_subnets, Reply, PREFIX,
};
use crate::query_wire::element;

/// The engine release every domain below runs — one the service's catalog publishes for the
/// instance type the rows use.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// How AWS words a placement whose security groups do not belong to the VPC its subnets are
/// in, pinned here as a literal so a change to the wording moves no expectation with it.
const NOT_IN_SAME_VPC: &str = "Subnets and security groups must be in the same VPC.";

/// A well-formed security group identifier no `CreateSecurityGroup` below handed out, so
/// EC2 resolves nothing for it and it grounds no refusal.
const UNRESOLVABLE_GROUP: &str = "sg-0baadf00d00000001";

/// The name EC2 gives the group it creates with every VPC, which is how the default group of
/// a VPC is READ here rather than assumed ("your VPC automatically comes with a default
/// security group", Amazon VPC User Guide).
const DEFAULT_GROUP_NAME: &str = "default";

/// How long a poll waits for a span the clock owes a domain or an endpoint to fall due.
///
/// Generous on purpose: the condition is what is waited for, not a duration, so a loaded
/// host only makes the loop go round more times.
const READY_DEADLINE: Duration = Duration::from_secs(120);

/// How long one turn of a poll loop pauses before looking again.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// One VPC of the estate: the ids EC2 minted for it, its subnets, the group a client created
/// in it and the default group EC2 made with it.
struct Network {
    vpc_id: String,
    subnets: Vec<String>,
    named_group: String,
    default_group: String,
}

/// Mint one VPC with one subnet per zone and one security group of its own, through EC2
/// itself.
async fn network(
    router: &MultiServiceRouter,
    cidr_prefix: &str,
    zones: &[&str],
    label: &str,
) -> Network {
    let (vpc_id, subnets) = vpc_with_subnets(router, cidr_prefix, zones).await;
    let named_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", &format!("opensearch-sg-{label}")),
                ("GroupDescription", "a group a placement names"),
                ("VpcId", vpc_id.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // What EC2 itself calls the default group of this VPC, read through its own filters
    // rather than being a value this suite made up.
    let default_group = element(
        &ec2(
            router,
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
    Network {
        vpc_id,
        subnets,
        named_group,
        default_group,
    }
}

/// One security group a row names.
#[derive(Debug, Clone, Copy)]
enum Group {
    /// The group a client created in the VPC at this index.
    Named(usize),
    /// The default group EC2 made with the VPC at this index.
    Default(usize),
    /// A well-formed identifier the account has no group for.
    Unresolvable,
}

/// The identifiers a row's groups resolve to, in the order it names them.
fn named_groups(estate: &[Network], groups: &[Group]) -> Vec<String> {
    groups
        .iter()
        .map(|group| match group {
            Group::Named(index) => estate[*index].named_group.clone(),
            Group::Default(index) => estate[*index].default_group.clone(),
            Group::Unresolvable => UNRESOLVABLE_GROUP.to_string(),
        })
        .collect()
}

/// The subnet identifiers a row names, as `(VPC index, subnet index)` pairs.
fn named_subnets(estate: &[Network], subnets: &[(usize, usize)]) -> Vec<String> {
    subnets
        .iter()
        .map(|(vpc, subnet)| estate[*vpc].subnets[*subnet].clone())
        .collect()
}

/// One `CreateDomain` to make.
struct CreateRow {
    case: &'static str,
    name: &'static str,
    /// The subnets the request names, as `(VPC index, subnet index)` pairs.
    subnets: &'static [(usize, usize)],
    /// The security groups the request names; empty names none at all.
    groups: &'static [Group],
    /// Whether the create is served.
    accepted: bool,
}

/// The create table: one row per way the pair of subnets and groups differs, spanning both
/// sides of the rule.
const CREATE_ROWS: &[CreateRow] = &[
    CreateRow {
        case: "one group of the subnets' own VPC",
        name: "sg-vpc-own-one",
        subnets: &[(0, 0)],
        groups: &[Group::Named(0)],
        accepted: true,
    },
    CreateRow {
        case: "the default group EC2 made with the subnets' own VPC",
        name: "sg-vpc-own-default",
        subnets: &[(0, 0), (0, 1)],
        groups: &[Group::Default(0)],
        accepted: true,
    },
    CreateRow {
        case: "both groups of the own VPC",
        name: "sg-vpc-own-both",
        subnets: &[(0, 1)],
        groups: &[Group::Named(0), Group::Default(0)],
        accepted: true,
    },
    CreateRow {
        case: "no security group at all",
        name: "sg-vpc-none",
        subnets: &[(0, 2)],
        groups: &[],
        accepted: true,
    },
    CreateRow {
        case: "an identifier the account holds no group for",
        name: "sg-vpc-unresolvable",
        subnets: &[(0, 0)],
        groups: &[Group::Unresolvable],
        accepted: true,
    },
    CreateRow {
        case: "subnets and a group of the second VPC",
        name: "sg-vpc-second",
        subnets: &[(1, 0), (1, 1)],
        groups: &[Group::Named(1)],
        accepted: true,
    },
    CreateRow {
        case: "a group of another VPC",
        name: "sg-vpc-foreign",
        subnets: &[(0, 0)],
        groups: &[Group::Named(1)],
        accepted: false,
    },
    CreateRow {
        case: "the default group of another VPC",
        name: "sg-vpc-foreign-default",
        subnets: &[(0, 0), (0, 1)],
        groups: &[Group::Default(1)],
        accepted: false,
    },
    CreateRow {
        case: "the foreign group named first",
        name: "sg-vpc-foreign-first",
        subnets: &[(0, 1)],
        groups: &[Group::Named(1), Group::Named(0)],
        accepted: false,
    },
    CreateRow {
        case: "a group of a third VPC between two own ones",
        name: "sg-vpc-foreign-middle",
        subnets: &[(0, 0), (0, 2)],
        groups: &[Group::Named(0), Group::Named(2), Group::Default(0)],
        accepted: false,
    },
    CreateRow {
        case: "an unresolvable identifier beside a foreign group",
        name: "sg-vpc-unresolved-foreign",
        subnets: &[(0, 2)],
        groups: &[Group::Unresolvable, Group::Named(1)],
        accepted: false,
    },
    CreateRow {
        case: "subnets of the second VPC with a group of the first",
        name: "sg-vpc-second-foreign",
        subnets: &[(1, 0)],
        groups: &[Group::Named(0)],
        accepted: false,
    },
];

/// One `UpdateDomainConfig` to apply to the domain created in the first VPC.
struct UpdateRow {
    case: &'static str,
    /// The subnets the change names; empty names none, so the change carries groups alone.
    subnets: &'static [(usize, usize)],
    groups: &'static [Group],
    /// Whether the change is a dry run, which "returns the results of a dry run analysis
    /// without actually applying the change".
    dry_run: bool,
    accepted: bool,
}

/// The update table: the same rule over the operation that CHANGES a placement.
const UPDATE_ROWS: &[UpdateRow] = &[
    UpdateRow {
        case: "a change naming own subnets and a group of another VPC",
        subnets: &[(0, 1)],
        groups: &[Group::Named(1)],
        dry_run: false,
        accepted: false,
    },
    UpdateRow {
        case: "a dry run naming a group of another VPC",
        subnets: &[(0, 1), (0, 2)],
        groups: &[Group::Named(2)],
        dry_run: true,
        accepted: false,
    },
    UpdateRow {
        case: "a change naming a group of another VPC and no subnet",
        subnets: &[],
        groups: &[Group::Named(1)],
        dry_run: false,
        accepted: false,
    },
    UpdateRow {
        case: "a change naming own subnets and the own VPC's default group",
        subnets: &[(0, 1), (0, 2)],
        groups: &[Group::Default(0)],
        dry_run: false,
        accepted: true,
    },
];

/// The `ClusterConfig` a call naming `subnets` carries: a VPC-access domain is reached
/// through ONE subnet per Availability Zone it is placed in, so a row naming several
/// subnets is a zone-aware cluster of that many zones, with one data node in each.
fn cluster_config(subnets: usize) -> Value {
    let mut cluster = json!({
        "InstanceType": "t3.small.search",
        "InstanceCount": subnets.max(1),
    });
    if subnets > 1 {
        cluster["ZoneAwarenessEnabled"] = json!(true);
        cluster["ZoneAwarenessConfig"] = json!({ "AvailabilityZoneCount": subnets });
    }
    cluster
}

/// A `CreateDomain` body naming `subnets` and `groups`.
fn create_body(name: &str, subnets: &[String], groups: &[String]) -> Value {
    let mut vpc_options = json!({ "SubnetIds": subnets });
    if !groups.is_empty() {
        vpc_options["SecurityGroupIds"] = json!(groups);
    }
    json!({
        "DomainName": name,
        "EngineVersion": ENGINE_VERSION,
        "ClusterConfig": cluster_config(subnets.len()),
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        "VPCOptions": vpc_options,
    })
}

/// `CreateDomain`, as it answered.
async fn create_domain(router: &MultiServiceRouter, body: Value) -> Reply {
    opensearch(
        router,
        "POST",
        &format!("{PREFIX}/opensearch/domain"),
        Some(body),
    )
    .await
}

/// `DescribeDomain`, as it answered.
async fn describe_domain(router: &MultiServiceRouter, name: &str) -> Reply {
    opensearch(
        router,
        "GET",
        &format!("{PREFIX}/opensearch/domain/{name}"),
        None,
    )
    .await
}

/// `UpdateDomainConfig` with the `VPCOptions` a row names, as it answered.
async fn update_config(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[String],
    groups: &[String],
    dry_run: bool,
) -> Reply {
    let mut vpc_options = json!({ "SecurityGroupIds": groups });
    let mut body = json!({ "DryRun": dry_run });
    if !subnets.is_empty() {
        vpc_options["SubnetIds"] = json!(subnets);
        // The placement a change names carries the topology it is placed in with it: one
        // subnet per Availability Zone.
        body["ClusterConfig"] = cluster_config(subnets.len());
    }
    body["VPCOptions"] = vpc_options;
    opensearch(
        router,
        "POST",
        &format!("{PREFIX}/opensearch/domain/{name}/config"),
        Some(body),
    )
    .await
}

/// The domain names `ListDomainNames` reports for this account.
async fn listed_domains(router: &MultiServiceRouter) -> Vec<String> {
    let listed = opensearch(router, "GET", &format!("{PREFIX}/domain"), None).await;
    assert_eq!(
        listed.status, 200,
        "a listing of this account's domains must be served, got {} with {}",
        listed.status, listed.body
    );
    listed.body["DomainNames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|info| info["DomainName"].as_str().map(str::to_owned))
        .collect()
}

/// The `VPCOptions` a `DomainStatus` reports, as `(subnets, security groups, vpc id)`.
fn placement_of(status: &Value) -> (Vec<String>, Vec<String>, Option<String>) {
    let list = |name: &str| {
        status["VPCOptions"][name]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect::<Vec<String>>()
    };
    (
        list("SubnetIds"),
        list("SecurityGroupIds"),
        status["VPCOptions"]["VPCId"].as_str().map(str::to_owned),
    )
}

/// Look again until the domain reports no work in flight, so the next configuration change
/// is served on it, or a generous deadline passes.
async fn await_idle_domain(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let read = describe_domain(router, name).await;
        if read.body["DomainStatus"]["Processing"] != json!(true) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the domain {name:?} never settled: {}",
            read.body
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Assert a refusal is the modelled one, worded as the service words this condition.
fn assert_refused(answer: &Reply, case: &str) {
    assert_eq!(
        (answer.status, answer.error_code()),
        (400, "ValidationException"),
        "case {case:?}: expected a 400 ValidationException, got {} with {}",
        answer.status,
        answer.body
    );
    assert_eq!(
        answer.message(),
        NOT_IN_SAME_VPC,
        "case {case:?}: expected the sentence AWS words this condition with, got {}",
        answer.body
    );
}

/// The three VPCs every row below runs against, each with its own subnets and groups.
async fn estate(router: &MultiServiceRouter) -> Vec<Network> {
    let plan: [(&str, &[&'static str], &str); 3] = [
        ("10.140", &["us-west-2a", "us-west-2b", "us-west-2c"], "one"),
        ("10.141", &["us-west-2a", "us-west-2b"], "two"),
        ("10.142", &["us-west-2a"], "three"),
    ];
    let mut estate: Vec<Network> = Vec::new();
    for (prefix, zones, label) in plan {
        estate.push(network(router, prefix, zones, label).await);
    }
    for (left, right) in [(0, 1), (0, 2), (1, 2)] {
        assert_ne!(
            estate[left].vpc_id, estate[right].vpc_id,
            "setup: the VPCs must be distinct, got {} and {}",
            estate[left].vpc_id, estate[right].vpc_id
        );
        assert_ne!(
            estate[left].named_group, estate[right].named_group,
            "setup: the groups must be distinct, got {} and {}",
            estate[left].named_group, estate[right].named_group
        );
    }
    estate
}

#[tokio::test]
async fn a_vpc_domain_carries_security_groups_of_its_own_vpc() {
    let router = emulator();
    let estate = estate(&router).await;

    for row in CREATE_ROWS {
        let case = row.case;
        let subnets = named_subnets(&estate, row.subnets);
        let groups = named_groups(&estate, row.groups);
        let answer = create_domain(&router, create_body(row.name, &subnets, &groups)).await;

        if !row.accepted {
            assert_refused(&answer, case);

            // A refused create leaves NO domain behind: the name resolves nowhere and the
            // account's listing does not carry it.
            let read = describe_domain(&router, row.name).await;
            assert_eq!(
                read.error_code(),
                "ResourceNotFoundException",
                "case {case:?}: a refused create must leave the name unresolvable, got {} \
                 with {}",
                read.status,
                read.body
            );
            assert!(
                !listed_domains(&router).await.contains(&row.name.to_string()),
                "case {case:?}: a refused create must leave the account's domains as they \
                 were"
            );
            continue;
        }

        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the create to be served, got {} with {}",
            answer.status, answer.body
        );

        // Read the domain back, so what is asserted is the placement the service STORED and
        // not only what the create echoed.
        let read = describe_domain(&router, row.name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the domain to be readable, got {} with {}",
            read.status, read.body
        );
        let placement_vpc = estate[row.subnets[0].0].vpc_id.clone();
        for (source, reported) in [
            ("the create", placement_of(&answer.body["DomainStatus"])),
            ("DescribeDomain", placement_of(&read.body["DomainStatus"])),
        ] {
            let (named_subnets, named_groups, vpc_id) = reported;
            assert_eq!(
                named_subnets, subnets,
                "case {case:?}: {source} must report the subnets the request named, got \
                 {named_subnets:?}"
            );
            assert_eq!(
                named_groups, groups,
                "case {case:?}: {source} must report the security groups the request named, \
                 got {named_groups:?}"
            );
            assert_eq!(
                vpc_id.as_deref(),
                Some(placement_vpc.as_str()),
                "case {case:?}: {source} must report the VPC its subnets are in"
            );
        }
    }

    // The CHANGE half of the same rule, on the domain created in the first VPC: the create's
    // own provisioning span falls due first, so the domain is idle and a configuration
    // change is served on it.
    let name = CREATE_ROWS[0].name;
    await_idle_domain(&router, name).await;

    for row in UPDATE_ROWS {
        let case = row.case;
        let subnets = named_subnets(&estate, row.subnets);
        let groups = named_groups(&estate, row.groups);
        let before = describe_domain(&router, name).await;
        let before = placement_of(&before.body["DomainStatus"]);
        let answer = update_config(&router, name, &subnets, &groups, row.dry_run).await;

        if row.accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the change to be served, got {} with {}",
                answer.status, answer.body
            );
            let read = describe_domain(&router, name).await;
            let (reported_subnets, reported_groups, vpc_id) =
                placement_of(&read.body["DomainStatus"]);
            assert_eq!(
                reported_subnets, subnets,
                "case {case:?}: the domain must carry the subnets the change named, got \
                 {reported_subnets:?}"
            );
            assert_eq!(
                reported_groups, groups,
                "case {case:?}: the domain must carry the groups the change named, got \
                 {reported_groups:?}"
            );
            assert_eq!(
                vpc_id.as_deref(),
                Some(estate[0].vpc_id.as_str()),
                "case {case:?}: the domain stays in the VPC its subnets are in, got {}",
                read.body
            );
            await_idle_domain(&router, name).await;
        } else {
            assert_refused(&answer, case);
            let read = describe_domain(&router, name).await;
            assert_eq!(
                placement_of(&read.body["DomainStatus"]),
                before,
                "case {case:?}: a refused change must leave the placement standing, got {}",
                read.body
            );
        }
    }
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

#[tokio::test]
async fn a_vpc_endpoint_carries_security_groups_of_its_own_vpc() {
    let router = emulator();
    let estate = estate(&router).await;

    // The domain the endpoints below are created for, placed in the first VPC with a group
    // of that VPC.
    let created = create_domain(
        &router,
        create_body(
            "sg-endpoint-domain",
            &estate[0].subnets[..1],
            &[estate[0].named_group.clone()],
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateDomain must be served, got {} with {}",
        created.status, created.body
    );
    let domain_arn = created.body["DomainStatus"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the domain reported no ARN: {}", created.body))
        .to_owned();

    // A create naming a group of another VPC mints NOTHING, over the rows that differ in
    // which VPC the foreign group belongs to, whether it is a client's or EC2's own default
    // group, and where in the list it sits.
    let held_before = listed_endpoint_count(&router).await;
    let refused_rows: [(&str, Vec<String>); 4] = [
        (
            "a create naming a group of the second VPC",
            vec![estate[1].named_group.clone()],
        ),
        (
            "a create naming the default group of the second VPC",
            vec![estate[1].default_group.clone()],
        ),
        (
            "a create naming the foreign group first",
            vec![estate[2].named_group.clone(), estate[0].named_group.clone()],
        ),
        (
            "a create naming an unresolvable id beside a foreign group",
            vec![UNRESOLVABLE_GROUP.to_string(), estate[1].named_group.clone()],
        ),
    ];
    for (case, groups) in &refused_rows {
        let refused = create_endpoint(&router, &domain_arn, &estate[0].subnets[..1], groups).await;
        assert_refused(&refused, case);
        assert_eq!(
            listed_endpoint_count(&router).await,
            held_before,
            "case {case:?}: a refused create must mint no endpoint"
        );
    }

    // ... while a create naming groups of the placement's own VPC is served.
    let own_groups = vec![
        estate[0].named_group.clone(),
        estate[0].default_group.clone(),
    ];
    let served = create_endpoint(&router, &domain_arn, &estate[0].subnets[..2], &own_groups).await;
    assert_eq!(
        served.status, 200,
        "case a create naming two groups of the own VPC: expected it to be served, got {} \
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

    // The CHANGE half of the same rule: a change naming a group of another VPC leaves the
    // endpoint exactly as it was, and one naming a group of its own VPC is served.
    for (case, groups) in &refused_rows {
        let before = read_endpoint(&router, &id).await;
        let answer = update_endpoint(&router, &id, &estate[0].subnets[..1], groups).await;
        assert_refused(&answer, case);
        let after = read_endpoint(&router, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused change must leave the endpoint standing, got {after}"
        );
    }

    let moved = vec![estate[0].default_group.clone()];
    let answer = update_endpoint(&router, &id, &estate[0].subnets[..1], &moved).await;
    assert_eq!(
        answer.status, 200,
        "case a change naming the own VPC's default group: expected it to be served, got {} \
         with {}",
        answer.status, answer.body
    );
    let settled = await_active_endpoint(&router, &id).await;
    assert_eq!(
        endpoint_groups(&settled),
        moved,
        "the endpoint must carry the groups the change named, got {settled}"
    );
}
