// SPDX-License-Identifier: CC0-1.0

//! WHO reaches a cache at the endpoint the control plane published for it: the cache's own VPC
//! SECURITY GROUPS, read on every connection.
//!
//! RULE: a cache placed in a VPC of the account is reached through an interface the VPC
//! security groups it carries govern, and a security group is a stateful filter on that
//! interface whose inbound rules are the only grants there are — a rule names a protocol, a
//! port range and a source, "all rules are permissive" so there is no deny rule, and traffic no
//! rule of any of those groups names is DROPPED rather than refused. So a connection to a cache
//! node's endpoint is served only where one inbound rule of one group guarding the cache names
//! TCP, the port the cache's engine accepts connections on, and the source the connection comes
//! from; one no rule names is answered with nothing at all, which also means the command it
//! carried never reaches the engine and leaves no key behind. The rules are read at the instant
//! a connection is established, so revoking the only admitting rule stops the NEXT connection
//! while the sessions already open carry on, and authorizing the identical rule again readmits
//! the same client to the keyspace it wrote. A cache in no VPC of the account is behind no such
//! interface and is guarded by no VPC security group.
//!
//! The rows differ along what the rule generalises over: the PROTOCOL a rule names (the one a
//! client's connection is carried over, another one, every protocol), the PORT RANGE it names
//! (the cache's own port, a range containing it, another port), the KIND of source it names (an
//! address range that holds the client, one that does not, every address, another security
//! group the client is a member of, one it is not), how many rules the group holds (none, one,
//! two), whether the client STATES the address it reaches the cache from, and whether the cache
//! was placed in a VPC at all.
//!
//! What a row does NOT assert is which address or port the endpoint carries — those are
//! [`crate::published_cache_endpoint_is_dialable`]'s subject and are per-run values — nor what
//! a session may DO once it is served, which is the access control
//! [`crate::cache_data_plane`] covers. What is asserted here is only whether the connection is
//! served at all.
//!
//! Every assertion goes over a wire: the control-plane reads are `awsQuery` requests through
//! the production router, the rules are written and taken back through Amazon EC2's OWN
//! `AuthorizeSecurityGroupIngress` / `RevokeSecurityGroupIngress` (the service that owns
//! security groups), the cache node's address in the VPC is read through that service's own
//! `DescribeNetworkInterfaces`, and every connection is opened to the endpoint the control
//! plane published. Nothing sleeps for a span: the cluster is waited for by LOOKING AGAIN at
//! its status (the crate's own `poll_until`), and a connection the cache drops is told apart
//! from a served one by what it ANSWERS, never by how long it takes.

use ::std::net::SocketAddr;

use ::aws_sdk_ec2::types::IpPermission;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cache_cluster_lifecycle::{account_subnet_ids, subnet_group_params};
use crate::cache_data_plane::{node_endpoint, open, open_in_vpc_from, Session};
use crate::harness::{params, poll_until, Emulator, REGION};
use service_com_amazonaws_elasticache_amazonelasticachev9::state::ec2_cache_node_interfaces::{
    interface_description, CACHE_INTERFACE_REQUESTER,
};

/// The cache subnet group every placed cluster of this suite sits in.
const SUBNET_GROUP: &str = "gate-subnets";

/// The port the clusters of this suite are created with — the port their engine accepts
/// connections on, which is the port a rule guarding them names.
const CACHE_PORT: i32 = 6379;

/// The `CacheNodeId` of the one node a single-node cluster runs.
const FIRST_NODE: &str = "0001";

/// What a client sends and what a served cache answers it, as the RESP wire carries them.
const PONG: &str = "+PONG\r\n";
/// See [`PONG`].
const OK: &str = "+OK\r\n";
/// A RESP null bulk string: what a `GET` of a key the cache does not hold answers.
const NIL: &str = "$-1\r\n";

/// Send one command and read what the cache answers it, as a client's parser reads a reply.
///
/// `None` is a connection the cache DROPPED — closed with nothing served — which is what a
/// client whose packet no rule of the cache's groups admits is left with. A write to such a
/// connection may still be buffered, so what tells the two apart is the ANSWER and not the
/// send.
async fn answered(session: &mut Session, arguments: &[&str]) -> Option<String> {
    let mut request = format!("*{}\r\n", arguments.len());
    for argument in arguments {
        request.push_str(&format!("${}\r\n{argument}\r\n", argument.len()));
    }
    session.write_all(request.as_bytes()).await.ok()?;
    session.flush().await.ok()?;
    let mut buffer = [0u8; 512];
    match session.read(&mut buffer).await {
        Ok(0) | Err(_) => None,
        Ok(read) => Some(String::from_utf8_lossy(&buffer[..read]).to_string()),
    }
}

/// Create the cache subnet group over the account's own subnets, and answer the VPC those
/// subnets belong to.
async fn placement(emulator: &Emulator) -> String {
    let reported = emulator.account_subnets().await;
    let vpc = reported
        .first()
        .map(|subnet| subnet.vpc.clone())
        .expect("the account must have a subnet to place a cache in");
    let subnets = account_subnet_ids(emulator).await;
    let created = emulator
        .call(
            "CreateCacheSubnetGroup",
            &subnet_group_params(SUBNET_GROUP, "the subnets this suite's caches sit in", &subnets[..2]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateCacheSubnetGroup must answer 200, got {} {}",
        created.status, created.body
    );
    vpc
}

/// Create a single-node Valkey / Redis OSS cluster and answer the node endpoint it publishes
/// once the clock has carried it to `available`: `(address, port)`.
///
/// `guarded_by` is the security group the cluster carries, and `None` places it in no VPC of
/// the account at all.
async fn served_cluster(
    emulator: &Emulator,
    id: &str,
    guarded_by: Option<&str>,
) -> (String, String) {
    let mut request = params(&[
        ("CacheClusterId", id),
        ("Engine", "redis"),
        ("CacheNodeType", "cache.t3.micro"),
        ("NumCacheNodes", "1"),
        ("Port", &CACHE_PORT.to_string()),
    ]);
    if let Some(group) = guarded_by {
        request.extend(params(&[
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("SecurityGroupIds.SecurityGroupId.1", group),
        ]));
    }
    let created = emulator.call("CreateCacheCluster", &request).await;
    assert_eq!(
        created.status, 200,
        "setup: CreateCacheCluster {id} must answer 200, got {} {}",
        created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        id,
        "the cluster reporting itself available",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", id), ("ShowCacheNodeInfo", "true")]),
        |reply| reply.text("CacheCluster/CacheClusterStatus") == Some("available"),
    )
    .await;
    node_endpoint(emulator, id, id).await
}

/// The private address the interface Amazon ElastiCache placed for the one node of the cluster
/// `id` holds in the account's VPC, read through `ec2:DescribeNetworkInterfaces` — the service
/// that owns network interfaces.
///
/// That address is a member of the groups the cluster carries, which is what makes a rule whose
/// source is one of those groups a grant to it.
async fn node_address(emulator: &Emulator, id: &str) -> String {
    let placed_by_elasticache = ::aws_sdk_ec2::types::Filter::builder()
        .name("requester-id")
        .values(CACHE_INTERFACE_REQUESTER)
        .build();
    let described = emulator
        .ec2(REGION)
        .describe_network_interfaces()
        .filters(placed_by_elasticache)
        .send()
        .await
        .expect("EC2 must report the account's network interfaces");
    let wanted = interface_description(id, FIRST_NODE);
    described
        .network_interfaces()
        .iter()
        .find(|interface| interface.description() == Some(wanted.as_str()))
        .and_then(|interface| interface.private_ip_address())
        .unwrap_or_else(|| {
            panic!("setup: the node of {id} must hold an interface of its own in the account's VPC")
        })
        .to_string()
}

/// One row: the rules the cache's security group holds, where the connection comes from, and
/// whether the cache serves it.
struct Row {
    case: &'static str,
    /// The inbound rules written on the cache's group before the connection is opened, taken
    /// back again afterwards — so each row is judged by its own rules alone.
    rules: Vec<IpPermission>,
    /// The address the client states its traffic reaches the cache from, and `None` for a
    /// client that states none.
    from: Option<SocketAddr>,
    /// Whether the connection must be served.
    served: bool,
}

/// An address a client of the VPC states, as a stated source: `address` with an ephemeral port.
fn source(address: &str) -> Option<SocketAddr> {
    Some(SocketAddr::new(
        address
            .parse()
            .unwrap_or_else(|error| panic!("{address} is an address: {error}")),
        49_152,
    ))
}

/// The rows: every rule form crossed with the sources it does and does not name.
///
/// `group` is the security group guarding the cache (a source a rule may name), and
/// `node_address` the address the cache's own node holds in the VPC — which is a member of that
/// group, and the only address of this emulator that is.
fn rows(group: &str, node_address: &str) -> Vec<Row> {
    let anywhere = |ports| Emulator::ingress_permission("tcp", ports, Some("0.0.0.0/0"), None);
    vec![
        // The rule a client writes for its cache: TCP on the cache's own port.
        Row {
            case: "the cache's own port from every address, by a client that states none",
            rules: vec![anywhere(Some((CACHE_PORT, CACHE_PORT)))],
            from: None,
            served: true,
        },
        Row {
            case: "the same rule, by a client that states where it comes from",
            rules: vec![anywhere(Some((CACHE_PORT, CACHE_PORT)))],
            from: source("10.77.3.9"),
            served: true,
        },
        // A range CONTAINING the port, and the rule on every protocol, which carries no range
        // at all: both name the cache's port.
        Row {
            case: "a port range containing the cache's own port",
            rules: vec![anywhere(Some((6000, 7000)))],
            from: None,
            served: true,
        },
        Row {
            case: "a rule on every protocol, which names every port",
            rules: vec![Emulator::ingress_permission("-1", None, Some("0.0.0.0/0"), None)],
            from: None,
            served: true,
        },
        // The source: an address range that holds the client, and one that does not.
        Row {
            case: "an address range that holds the client",
            rules: vec![Emulator::ingress_permission(
                "tcp",
                Some((CACHE_PORT, CACHE_PORT)),
                Some("10.0.0.0/8"),
                None,
            )],
            from: source("10.77.3.9"),
            served: true,
        },
        Row {
            case: "an address range that does not hold the client",
            rules: vec![Emulator::ingress_permission(
                "tcp",
                Some((CACHE_PORT, CACHE_PORT)),
                Some("10.0.0.0/8"),
                None,
            )],
            from: source("192.168.4.5"),
            served: false,
        },
        // The source as a SECURITY GROUP, which is the form a client uses for a cache its own
        // workloads reach: a member of that group is admitted and a client outside it is not.
        Row {
            case: "a source group the client is a member of",
            rules: vec![Emulator::ingress_permission(
                "tcp",
                Some((CACHE_PORT, CACHE_PORT)),
                None,
                Some(group),
            )],
            from: source(node_address),
            served: true,
        },
        Row {
            case: "a source group the client is not a member of",
            rules: vec![Emulator::ingress_permission(
                "tcp",
                Some((CACHE_PORT, CACHE_PORT)),
                None,
                Some(group),
            )],
            from: source("10.77.3.9"),
            served: false,
        },
        // Rules that name something else: another port, another protocol.
        Row {
            case: "another port only",
            rules: vec![anywhere(Some((11211, 11211)))],
            from: None,
            served: false,
        },
        Row {
            case: "another port only, by a client that states where it comes from",
            rules: vec![anywhere(Some((11211, 11211)))],
            from: source("10.77.3.9"),
            served: false,
        },
        Row {
            case: "another protocol on the cache's own port",
            rules: vec![Emulator::ingress_permission(
                "udp",
                Some((CACHE_PORT, CACHE_PORT)),
                Some("0.0.0.0/0"),
                None,
            )],
            from: None,
            served: false,
        },
        // Two rules, only the second of which names the connection.
        Row {
            case: "two rules, the second of which names the cache's port",
            rules: vec![anywhere(Some((80, 80))), anywhere(Some((CACHE_PORT, CACHE_PORT)))],
            from: None,
            served: true,
        },
        // The group as it is CREATED: a group with no inbound rule admits nothing.
        Row {
            case: "a group carrying no inbound rule at all",
            rules: Vec::new(),
            from: None,
            served: false,
        },
        Row {
            case: "a group carrying no inbound rule, by a client that states where it comes from",
            rules: Vec::new(),
            from: source("10.77.3.9"),
            served: false,
        },
    ]
}

#[tokio::test]
async fn a_cache_serves_only_the_connections_its_security_groups_admit() {
    let emulator = Emulator::new();
    placement(&emulator).await;
    let group = emulator.security_group_named("gate-cache").await;
    let (address, port) = served_cluster(&emulator, "gate-guarded", Some(&group)).await;
    let node = node_address(&emulator, "gate-guarded").await;

    for row in rows(&group, &node) {
        for rule in &row.rules {
            emulator.authorize_ingress(&group, rule.clone()).await;
        }
        let mut session = open_in_vpc_from(&emulator, row.case, &address, &port, row.from).await;
        let answer = answered(&mut session, &["PING"]).await;
        match row.served {
            true => assert_eq!(
                answer.as_deref(),
                Some(PONG),
                "case {:?}: a connection the cache's groups admit must be served, got {answer:?}",
                row.case
            ),
            false => assert_eq!(
                answer, None,
                "case {:?}: a connection no rule of the cache's groups admits must be dropped \
                 with nothing served, got {answer:?}",
                row.case
            ),
        }
        // The rules go back off the group, so the next row is judged by its own alone — and
        // the revoke is the owner's own call, which is what changes reachability.
        for rule in &row.rules {
            assert!(
                emulator.revoke_ingress(&group, rule.clone()).await,
                "case {:?}: EC2 must report the rule revoked",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn a_cache_in_no_vpc_of_the_account_is_guarded_by_no_security_group() {
    // The boundary of the rule: a cluster placed in no cache subnet group sits in no VPC of the
    // account, so it is behind no interface a security group governs — and no group of the
    // account, carrying no rule, can make it unreachable.
    let emulator = Emulator::new();
    let (address, port) = served_cluster(&emulator, "gate-vpc-less", None).await;
    let mut session = open("a cluster in no VPC of the account", &address, &port).await;
    let answer = answered(&mut session, &["PING"]).await;
    assert_eq!(
        answer.as_deref(),
        Some(PONG),
        "case \"a cluster in no VPC of the account\": the published endpoint must be served, got \
         {answer:?}"
    );
}

#[tokio::test]
async fn revoking_the_only_admitting_rule_closes_the_endpoint_to_new_connections() {
    let emulator = Emulator::new();
    placement(&emulator).await;
    let group = emulator.security_group_named("gate-revoked").await;
    let (address, port) = served_cluster(&emulator, "gate-sequence", Some(&group)).await;
    let admitting = Emulator::ingress_permission(
        "tcp",
        Some((CACHE_PORT, CACHE_PORT)),
        Some("0.0.0.0/0"),
        None,
    );

    // 1. Through the admitting rule the cache is usable: it answers, and it keeps what it is
    //    given.
    emulator.authorize_ingress(&group, admitting.clone()).await;
    let mut admitted = open_in_vpc_from(&emulator, "admitted", &address, &port, None).await;
    assert_eq!(
        answered(&mut admitted, &["SET", "gate:kept", "stored through the rule"])
            .await
            .as_deref(),
        Some(OK),
        "case \"admitted\": a SET through the admitting rule must be accepted"
    );

    // 2. The rule is taken back by its owner, which the owner reports.
    assert!(
        emulator.revoke_ingress(&group, admitting.clone()).await,
        "case \"revoked\": EC2 must report the only admitting rule revoked"
    );

    // 3. The session already open is NOT cut: a security group is a stateful filter, and what
    //    it judges is a connection as it is established.
    assert_eq!(
        answered(&mut admitted, &["PING"]).await.as_deref(),
        Some(PONG),
        "case \"revoked\": the session opened through the rule must carry on"
    );

    // 4. A NEW connection is dropped, and the command it carried never reaches the engine — so
    //    it leaves no key behind.
    let mut withdrawn = open_in_vpc_from(&emulator, "withdrawn", &address, &port, None).await;
    let refused = answered(&mut withdrawn, &["SET", "gate:withdrawn", "written with no rule"]).await;
    assert_eq!(
        refused, None,
        "case \"withdrawn\": a connection opened once the only admitting rule is gone must be \
         dropped with nothing served, got {refused:?}"
    );

    // 5. The identical rule readmits the same client, which finds the keyspace exactly as it
    //    left it: what it wrote through the rule is there, and what it attempted while the rule
    //    was withdrawn never landed.
    emulator.authorize_ingress(&group, admitting).await;
    let mut readmitted = open_in_vpc_from(&emulator, "readmitted", &address, &port, None).await;
    assert_eq!(
        answered(&mut readmitted, &["GET", "gate:kept"]).await.as_deref(),
        Some("$23\r\nstored through the rule\r\n"),
        "case \"readmitted\": the keyspace written before the revoke must be untouched"
    );
    assert_eq!(
        answered(&mut readmitted, &["GET", "gate:withdrawn"])
            .await
            .as_deref(),
        Some(NIL),
        "case \"readmitted\": a write attempted through a withdrawn rule must have left no key"
    );
}
