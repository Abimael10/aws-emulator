// SPDX-License-Identifier: CC0-1.0

//! What Amazon ElastiCache answers when a client TEARS DOWN what it built — the two rules a
//! cleanup script, a CloudFormation stack delete or a test harness depends on.
//!
//! RULE 1: `DeleteCacheSubnetGroup` deletes a cache subnet group ("You cannot delete a
//! default cache subnet group or one that is associated with any clusters"), so a delete of a
//! named, non-default group that no cache cluster and no replication group is placed in
//! answers the modelled success — HTTP 200 with the operation's empty result envelope and no
//! error envelope — and REMOVES the group, so the read that follows reports it absent and the
//! name is free again; a delete of a group a cluster IS placed in is the modelled
//! `CacheSubnetGroupInUse` refusal and leaves the group where it is, and the delete succeeds
//! once that cluster is gone.
//!
//! RULE 2: the `CacheParameterGroupNotFound` fault `DescribeCacheParameterGroups` answers a
//! `CacheParameterGroupName` this account holds no group under is worded as a sentence that
//! names the group — the fixed text `CacheParameterGroup ` and ` not found.` around the name
//! the REQUEST supplied — so a client that named several groups can tell from the message
//! which one is missing.
//!
//! This suite lives beside the router because a cache subnet group is built out of SUBNETS,
//! and a subnet is Amazon EC2's resource: every identifier the creates below name is one EC2
//! itself minted on the SAME engine that serves ElastiCache, so a delete that answers 200
//! here is a delete of a group that was really admitted rather than of one this suite
//! asserted into existence. Nothing reaches into either service's state struct; every
//! assertion is a wire answer.
//!
//! The rows differ along what each rule generalises over: for rule 1, how many of the
//! account's subnets the group holds, which VPC they belong to, the case the name is created
//! and deleted in and whether the create carried tags; for rule 2, the way the name misses
//! the store and the form of the name — its length (one character, and one at the documented
//! bound), its case and the characters it carries.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use service_com_amazonaws_elasticache_amazonelasticachev9::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, element, elements, form_encode, opt_element};

/// The region every request below is signed for, and so the region the resources belong to.
const REGION: &str = "us-east-1";

/// The API version an ec2Query client sends beside `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version an ElastiCache awsQuery client sends beside `Action`.
const ELASTICACHE_API_VERSION: &str = "2015-02-02";

/// The awsQuery error code the model gives `CacheParameterGroupNotFoundFault`.
const PARAMETER_GROUP_NOT_FOUND: &str = "CacheParameterGroupNotFound";

/// How long this suite is willing to wait for a status change the CLOCK owes, and how often
/// it looks again while it waits. Generous: it waits for the CONDITION rather than for a
/// fixed time, so what it asserts does not depend on how fast or how loaded the host is.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator carrying Amazon EC2 and Amazon ElastiCache over ONE shared engine, exactly as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticache_amazonelasticachev9::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One query-protocol answer, reduced to what a client branches on.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    /// The awsQuery error envelope's `Code`, or `""` when the answer carries no envelope.
    fn error_code(&self) -> String {
        blocks_of(&self.body, "Error")
            .first()
            .and_then(|envelope| opt_element(envelope, "Code"))
            .unwrap_or_default()
    }

    /// The error envelope's `Message`, or `""` when the answer carries no envelope.
    fn error_message(&self) -> String {
        blocks_of(&self.body, "Error")
            .first()
            .and_then(|envelope| opt_element(envelope, "Message"))
            .unwrap_or_default()
    }
}

/// Call one query-protocol operation of `signing_name`, at `version`, with these form members.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Reply {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Reply {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let reply = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        reply.status, 200,
        "setup: EC2 {action} must succeed, got {} with {}",
        reply.status, reply.body
    );
    reply.body
}

/// Call one Amazon ElastiCache operation over its awsQuery wire.
async fn elasticache(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Reply {
    query(
        router,
        "elasticache",
        ELASTICACHE_API_VERSION,
        action,
        members,
    )
    .await
}

/// One VPC of the account with the subnets EC2 minted in it.
struct Vpc {
    subnets: Vec<String>,
}

/// Create one VPC carrying one subnet per entry of `zones`, through EC2's own operations, so
/// every identifier this suite hands ElastiCache is one the owner of subnets handed out.
async fn ec2_vpc(router: &MultiServiceRouter, second_octet: u8, zones: &[&str]) -> Vpc {
    let cidr = format!("10.{second_octet}.0.0/16");
    let vpc_id = element(&ec2(router, "CreateVpc", &[("CidrBlock", &cidr)]).await, "vpcId");
    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc_id.as_str()),
                    ("CidrBlock", subnet_cidr.as_str()),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    Vpc { subnets }
}

/// `CreateCacheSubnetGroup` for `name`, holding `subnets`, with whatever `tags` names.
async fn create_subnet_group(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[String],
    tags: &[(&str, &str)],
) -> Reply {
    let mut members: Vec<(String, String)> = vec![
        ("CacheSubnetGroupName".to_string(), name.to_string()),
        (
            "CacheSubnetGroupDescription".to_string(),
            format!("a group this suite tears down again ({name})"),
        ),
    ];
    for (index, subnet) in subnets.iter().enumerate() {
        members.push((
            format!("SubnetIds.SubnetIdentifier.{}", index + 1),
            subnet.clone(),
        ));
    }
    for (index, (key, value)) in tags.iter().enumerate() {
        members.push((format!("Tags.Tag.{}.Key", index + 1), (*key).to_string()));
        members.push((format!("Tags.Tag.{}.Value", index + 1), (*value).to_string()));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    elasticache(router, "CreateCacheSubnetGroup", &borrowed).await
}

/// `DeleteCacheSubnetGroup` for the group `name` names.
async fn delete_subnet_group(router: &MultiServiceRouter, name: &str) -> Reply {
    elasticache(
        router,
        "DeleteCacheSubnetGroup",
        &[("CacheSubnetGroupName", name)],
    )
    .await
}

/// `DescribeCacheSubnetGroups` for the group `name` names.
async fn describe_subnet_group(router: &MultiServiceRouter, name: &str) -> Reply {
    elasticache(
        router,
        "DescribeCacheSubnetGroups",
        &[("CacheSubnetGroupName", name)],
    )
    .await
}

/// Every `CacheSubnetGroupName` a describe of the whole account reports.
async fn all_subnet_group_names(router: &MultiServiceRouter) -> Vec<String> {
    let reply = elasticache(router, "DescribeCacheSubnetGroups", &[("MaxRecords", "100")]).await;
    assert_eq!(
        reply.status, 200,
        "a describe of every cache subnet group must answer 200, got {} with {}",
        reply.status, reply.body
    );
    elements(&reply.body, "CacheSubnetGroupName")
}

/// Assert `reply` is this operation's modelled SUCCESS: the 200, the operation's own result
/// envelope, a request id of its own and no error envelope at all — which is what tells a
/// client "the group is gone" apart from any of the operation's modelled refusals.
fn assert_modelled_success(case: &str, action: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: {action} must answer HTTP 200, got {} with {}",
        reply.status, reply.body
    );
    assert!(
        blocks_of(&reply.body, "Error").is_empty(),
        "case {case:?}: {action} must carry no error envelope, got {}",
        reply.body
    );
    // The operation's output shape is `Unit`, so its result envelope is there and EMPTY:
    // there is nothing to report but the 200 and the envelope's own request id.
    assert_eq!(
        blocks_of(&reply.body, &format!("{action}Result")),
        vec![""],
        "case {case:?}: {action} must answer its own empty result envelope, got {}",
        reply.body
    );
    assert!(
        opt_element(&reply.body, "RequestId").is_some(),
        "case {case:?}: {action} must report its request id, got {}",
        reply.body
    );
}

/// One row of the delete table: the case, the name the group is created with, the name the
/// delete spells it with, which of the account's subnets it holds and the tags the create
/// carried.
struct DeleteRow {
    case: &'static str,
    created_as: &'static str,
    deleted_as: &'static str,
    /// Which VPC of the two the account holds the subnets come from, and how many of them.
    vpc: usize,
    subnet_count: usize,
    tags: &'static [(&'static str, &'static str)],
}

/// The rows span what rule 1 generalises over: the number of subnets the group holds, the VPC
/// they belong to, the case the name is created and deleted in and the tag count.
const DELETE_ROWS: &[DeleteRow] = &[
    DeleteRow {
        case: "a lowercase name over one subnet of the first VPC, deleted as spelled",
        created_as: "teardown-one-subnet",
        deleted_as: "teardown-one-subnet",
        vpc: 0,
        subnet_count: 1,
        tags: &[],
    },
    DeleteRow {
        case: "a mixed-case name over two subnets, deleted in upper case",
        created_as: "Teardown-Two-Subnets",
        deleted_as: "TEARDOWN-TWO-SUBNETS",
        vpc: 0,
        subnet_count: 2,
        tags: &[],
    },
    DeleteRow {
        case: "an upper-case tagged name over three subnets, deleted in lower case",
        created_as: "TEARDOWN-THREE-SUBNETS",
        deleted_as: "teardown-three-subnets",
        vpc: 0,
        subnet_count: 3,
        tags: &[("team", "cache"), ("env", "test")],
    },
    DeleteRow {
        case: "a group placed in the OTHER VPC the account holds",
        created_as: "teardown-second-vpc",
        deleted_as: "teardown-second-vpc",
        vpc: 1,
        subnet_count: 2,
        tags: &[("owner", "nobody")],
    },
];

#[tokio::test]
async fn deleting_an_unassociated_cache_subnet_group_succeeds_and_removes_it() {
    let router = emulator();
    let vpcs = [
        ec2_vpc(&router, 90, &["us-east-1a", "us-east-1b", "us-east-1c"]).await,
        ec2_vpc(&router, 91, &["us-east-1b", "us-east-1c"]).await,
    ];
    let before = all_subnet_group_names(&router).await;

    for row in DELETE_ROWS {
        let case = row.case;
        let subnets = &vpcs[row.vpc].subnets;
        assert!(
            row.subnet_count <= subnets.len(),
            "case {case:?}: EC2 minted {} subnets in this VPC, so {} cannot be named",
            subnets.len(),
            row.subnet_count
        );
        let held = &subnets[..row.subnet_count];

        // The group has to be ADMITTED before a delete of it can mean anything: the subnets
        // are EC2's own, so this create is the one a client makes.
        let created = create_subnet_group(&router, row.created_as, held, row.tags).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the create must be accepted over EC2's own subnets, got {} with {}",
            created.status, created.body
        );
        let read = describe_subnet_group(&router, row.deleted_as).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the group must be readable before the delete, got {} with {}",
            read.status, read.body
        );
        let stored: Vec<String> = elements(&read.body, "SubnetIdentifier");
        assert_eq!(
            stored, *held,
            "case {case:?}: the stored group must hold the subnets EC2 minted, got {}",
            read.body
        );

        // THE RULE: nothing is placed in the group, so the delete is the modelled success.
        let deleted = delete_subnet_group(&router, row.deleted_as).await;
        assert_modelled_success(case, "DeleteCacheSubnetGroup", &deleted);

        // And it REMOVED the group: the read reports it absent, the account no longer lists
        // it, and the name is free to create again.
        let gone = describe_subnet_group(&router, row.deleted_as).await;
        assert_eq!(
            (gone.status, gone.error_code().as_str()),
            (400, "CacheSubnetGroupNotFoundFault"),
            "case {case:?}: the deleted group must read as absent, got {} with {}",
            gone.status,
            gone.body
        );
        let listed = all_subnet_group_names(&router).await;
        assert!(
            !listed
                .iter()
                .any(|name| name.eq_ignore_ascii_case(row.created_as)),
            "case {case:?}: the account must no longer list the deleted group, got {listed:?}"
        );
        let again = create_subnet_group(&router, row.created_as, &subnets[..1], &[]).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the name must be free to create again, got {} with {}",
            again.status, again.body
        );
        let cleaned = delete_subnet_group(&router, row.deleted_as).await;
        assert_modelled_success(case, "DeleteCacheSubnetGroup", &cleaned);
    }

    // The deletes took exactly what they named: the account holds what it started with.
    let after = all_subnet_group_names(&router).await;
    assert_eq!(
        after, before,
        "the deletes must leave every other group in place, expected {before:?}, got {after:?}"
    );
}

#[tokio::test]
async fn a_cache_subnet_group_a_cluster_is_placed_in_is_refused_until_the_cluster_is_gone() {
    let case = "a cache cluster is placed in the group";
    let router = emulator();
    let vpc = ec2_vpc(&router, 92, &["us-east-1a", "us-east-1b"]).await;
    let group = "teardown-in-use";
    let cluster = "teardown-in-use-cluster";

    let created = create_subnet_group(&router, group, &vpc.subnets, &[]).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the create must be accepted, got {} with {}",
        created.status, created.body
    );
    let placed = elasticache(
        &router,
        "CreateCacheCluster",
        &[
            ("CacheClusterId", cluster),
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheNodes", "1"),
            ("CacheSubnetGroupName", group),
        ],
    )
    .await;
    assert_eq!(
        placed.status, 200,
        "case {case:?}: the cluster must be placed in the group, got {} with {}",
        placed.status, placed.body
    );

    // THE OTHER HALF of rule 1: a group a cluster is placed in is the modelled in-use
    // refusal, and it is still there afterwards — so the 200 above is not unconditional.
    let refused = delete_subnet_group(&router, group).await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, "CacheSubnetGroupInUse"),
        "case {case:?}: expected the modelled in-use refusal, got {} with {}",
        refused.status,
        refused.body
    );
    let still_there = describe_subnet_group(&router, group).await;
    assert_eq!(
        still_there.status, 200,
        "case {case:?}: the refused group must still be there, got {} with {}",
        still_there.status, still_there.body
    );

    // Release it by deleting the cluster, then the delete succeeds. A cache cluster is
    // created in a transient status and only a cluster the clock has made available takes a
    // delete, so the suite looks again for the CONDITION — each call being accepted — rather
    // than waiting a fixed time.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let released = elasticache(
            &router,
            "DeleteCacheCluster",
            &[("CacheClusterId", cluster)],
        )
        .await;
        if released.status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster must become deletable; the last delete answered {} with \
             {}",
            released.status,
            released.body
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let deleted = delete_subnet_group(&router, group).await;
        if deleted.status == 200 {
            assert_modelled_success(case, "DeleteCacheSubnetGroup", &deleted);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group must become deletable once the cluster is gone; the last \
             delete answered {} with {}",
            deleted.status,
            deleted.body
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
    let gone = describe_subnet_group(&router, group).await;
    assert_eq!(
        (gone.status, gone.error_code().as_str()),
        (400, "CacheSubnetGroupNotFoundFault"),
        "case {case:?}: the released group must read as absent, got {} with {}",
        gone.status,
        gone.body
    );
}

/// The sentence rule 2 requires: the fixed text `CacheParameterGroup ` and ` not found.`
/// around the name the REQUEST supplied. Written out here rather than taken from the helper
/// the handler calls, because a test built from that helper would pass whatever it said.
fn parameter_group_not_found_sentence(name: &str) -> String {
    format!("CacheParameterGroup {name} not found.")
}

#[tokio::test]
async fn describe_cache_parameter_groups_words_the_not_found_fault_as_a_sentence() {
    let router = emulator();

    // A group that DOES exist, so a row below cannot pass by the reader refusing everything,
    // and so the teardown sequence a client really runs — create, delete, read back — is the
    // last case here.
    let held = "teardown-held-parameter-group";
    let created = elasticache(
        &router,
        "CreateCacheParameterGroup",
        &[
            ("CacheParameterGroupName", held),
            ("CacheParameterGroupFamily", "redis7"),
            ("Description", "a group this suite tears down again"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the parameter group must be created, got {} with {}",
        created.status, created.body
    );
    let read = elasticache(
        &router,
        "DescribeCacheParameterGroups",
        &[("CacheParameterGroupName", held)],
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case \"a group this account holds\": expected 200, got {} with {}",
        read.status, read.body
    );

    // The axis: the way the name misses the store and the FORM of the name — its length (one
    // character, and one at the documented bound), its case and the characters it carries.
    // Source of the bound: `CacheParameterGroupName`'s own documented constraint, held in
    // `state::PARAMETER_GROUP_NAME_MAX_LEN`.
    let at_bound = format!("g{}", "a".repeat(state::PARAMETER_GROUP_NAME_MAX_LEN - 1));
    let prefix_of_held = &held[..4];
    let rows: [(&str, &str); 6] = [
        ("a name nothing ever created", "never-created-parameter-group"),
        ("a one-character name", "g"),
        ("a mixed-case name", "Mixed-Case-Parameter-Group"),
        ("a name carrying digits and hyphens", "group-7-of-9"),
        ("a prefix of a group this account holds", prefix_of_held),
        ("a name at the documented length bound", at_bound.as_str()),
    ];

    for (case, name) in rows {
        let refused = elasticache(
            &router,
            "DescribeCacheParameterGroups",
            &[("CacheParameterGroupName", name)],
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (404, PARAMETER_GROUP_NOT_FOUND),
            "case {case:?}: expected 404 {PARAMETER_GROUP_NOT_FOUND}, got {} with {}",
            refused.status,
            refused.body
        );
        let expected = parameter_group_not_found_sentence(name);
        assert_eq!(
            refused.error_message(),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            refused.error_message()
        );
    }

    // The recorded teardown step itself: the group a client created is deleted, and the read
    // that confirms it is gone is worded with the same sentence, carrying the name.
    let deleted = elasticache(
        &router,
        "DeleteCacheParameterGroup",
        &[("CacheParameterGroupName", held)],
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case \"the teardown delete\": expected 200, got {} with {}",
        deleted.status, deleted.body
    );
    let gone = elasticache(
        &router,
        "DescribeCacheParameterGroups",
        &[("CacheParameterGroupName", held)],
    )
    .await;
    assert_eq!(
        (gone.status, gone.error_code().as_str()),
        (404, PARAMETER_GROUP_NOT_FOUND),
        "case \"a group this account held until the teardown\": expected 404 \
         {PARAMETER_GROUP_NOT_FOUND}, got {} with {}",
        gone.status,
        gone.body
    );
    let expected = parameter_group_not_found_sentence(held);
    assert_eq!(
        gone.error_message(),
        expected,
        "case \"a group this account held until the teardown\": expected the message \
         {expected:?}, got {:?}",
        gone.error_message()
    );
}
