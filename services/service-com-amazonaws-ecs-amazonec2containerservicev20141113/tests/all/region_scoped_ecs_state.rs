// SPDX-License-Identifier: CC0-1.0

//! Which Region's ECS resources an endpoint of this service answers for.
//!
//! The rule under test, over a TABLE of Region pairs rather than one request:
//!
//! > Amazon ECS is a REGIONAL service — "Amazon ECS clusters are Region specific" — so a
//! > cluster and a task definition are created against ONE Region's endpoint and exist only
//! > there: another Region's endpoint lists neither, describes neither and acts in neither,
//! > one name is a separate resource in each Region, and every ARN an endpoint returns
//! > carries the Region of the endpoint that served it.
//!
//! Every case goes through the wire front door with the SigV4 credential scope of the Region
//! it is addressed to (the same place a handler reads the Region from), so nothing here
//! reaches into the state struct and the Region is carried exactly as a client carries it.
//!
//! What the rows span, beyond the one pair the defect was found on: the Region pair itself
//! (five pairs, across three partitions' Region codes), how many clusters the serving Region
//! holds of its own (none, and one of another name), a cluster name that exists in BOTH
//! Regions, the operation kind (a listing, a batch describe, a read of what the cluster
//! holds, a mutation and a delete), and the task-definition listings beside the cluster ones.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{list, string, ACCOUNT};

/// `Failure$reason` for an identifier the endpoint holds no resource for.
const MISSING: &str = "MISSING";

/// The error code every cluster-scoped operation answers for a cluster its endpoint does not
/// hold.
const CLUSTER_NOT_FOUND: &str = "ClusterNotFoundException";

/// The Region pairs the rows run: the Region the resources are created in, and a Region that
/// must hold none of them. Region codes from the AWS General Reference's Regions table,
/// spanning three partitions, plus one pair whose two Regions differ only in their suffix.
const REGION_PAIRS: [(&str, &str); 5] = [
    ("us-west-2", "us-east-2"),
    ("us-east-1", "eu-west-1"),
    ("eu-central-1", "ap-southeast-4"),
    ("us-gov-west-1", "us-gov-east-1"),
    ("ap-northeast-1", "ap-northeast-3"),
];

/// One `awsJson1_1` call to this service, signed for `region` — the SigV4 credential scope is
/// what tells the endpoint which Region it is answering for.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header("host", format!("ecs.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{region}\
                 /ecs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// A call to `region`'s endpoint that must be answered 200, returning its body.
async fn ok_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> Value {
    let (status, parsed) = call_in(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} in {region} to answer 200, got {status} with \
         {parsed}"
    );
    parsed
}

/// A call to `region`'s endpoint that must be refused 400, returning the modelled error code
/// and the `message` the refusal carries.
async fn refusal_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
    case: &str,
) -> (String, String) {
    let (status, parsed) = call_in(router, region, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} in {region} to answer 400, got {status} with \
         {parsed}"
    );
    let code = parsed["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string();
    let message = parsed["message"]
        .as_str()
        .or_else(|| parsed["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    (code, message)
}

/// The ARN `region`'s endpoint reports for the cluster `name`.
fn cluster_arn(region: &str, name: &str) -> String {
    format!("arn:aws:ecs:{region}:{ACCOUNT}:cluster/{name}")
}

/// The Region segment of an ECS ARN.
fn arn_region(arn: &str) -> &str {
    arn.split(':').nth(3).unwrap_or_default()
}

/// Create a cluster through `region`'s endpoint and return the ARN it answered with.
async fn create_cluster_in(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    case: &str,
) -> String {
    let body = ok_in(
        router,
        region,
        "CreateCluster",
        json!({ "clusterName": name }),
        case,
    )
    .await;
    string(&body["cluster"], "clusterArn", case)
}

/// Register one revision of `family` through `region`'s endpoint and return its ARN.
async fn register_definition_in(
    router: &MultiServiceRouter,
    region: &str,
    family: &str,
    case: &str,
) -> String {
    let body = ok_in(
        router,
        region,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "containerDefinitions": [
                crate::task_lifecycle::container("web", "public.ecr.aws/nginx/nginx:latest"),
            ],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// The cluster ARNs `region`'s endpoint lists.
async fn listed_clusters(router: &MultiServiceRouter, region: &str, case: &str) -> Vec<String> {
    let body = ok_in(router, region, "ListClusters", json!({}), case).await;
    list(&body, "clusterArns", case)
        .into_iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The task-definition ARNs `region`'s endpoint lists.
async fn listed_task_definitions(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
) -> Vec<String> {
    let body = ok_in(router, region, "ListTaskDefinitions", json!({}), case).await;
    list(&body, "taskDefinitionArns", case)
        .into_iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect()
}

/// A cluster and a task definition created in one Region are listed, described and acted in
/// by that Region's endpoint ALONE, and the far Region's endpoint reports them as resources it
/// does not hold.
#[tokio::test]
async fn ecs_resources_belong_to_the_region_that_created_them() {
    let router = crate::ecs_wire::service();
    for (index, (home, far)) in REGION_PAIRS.into_iter().enumerate() {
        let case = format!("a cluster created in {home}, probed from {far}");
        let name = format!("homecluster{index}");
        let family = format!("homefamily{index}");
        // One cluster of the FAR Region's own, so the probes below are answered by an
        // endpoint that holds clusters and task definitions — just not these.
        let far_name = format!("farcluster{index}");
        let far_family = format!("farfamily{index}");

        let created = create_cluster_in(&router, home, &name, &case).await;
        let definition = register_definition_in(&router, home, &family, &case).await;
        let far_own = create_cluster_in(&router, far, &far_name, &case).await;
        register_definition_in(&router, far, &far_family, &case).await;

        // Every ARN an endpoint returns carries the Region of the endpoint that served it.
        assert_eq!(
            created,
            cluster_arn(home, &name),
            "case {case:?}: the created cluster's ARN must carry the creating Region, got \
             {created}"
        );
        assert_eq!(
            arn_region(&definition),
            home,
            "case {case:?}: the registered task definition's ARN must carry the creating \
             Region, got {definition}"
        );
        assert_eq!(
            arn_region(&far_own),
            far,
            "case {case:?}: the far Region's own cluster ARN must carry its Region, got \
             {far_own}"
        );

        // The home Region still serves both.
        let home_listing = listed_clusters(&router, home, &case).await;
        assert!(
            home_listing.contains(&created),
            "case {case:?}: {home} must list the cluster it created, got {home_listing:?}"
        );
        let home_definitions = listed_task_definitions(&router, home, &case).await;
        assert!(
            home_definitions.contains(&definition),
            "case {case:?}: {home} must list the revision it registered, got \
             {home_definitions:?}"
        );

        // The far Region lists neither, and lists its OWN resources instead.
        let far_listing = listed_clusters(&router, far, &case).await;
        assert!(
            !far_listing.contains(&created) && far_listing.contains(&far_own),
            "case {case:?}: {far} must list its own clusters and none of {home}'s, got \
             {far_listing:?}"
        );
        assert!(
            far_listing.iter().all(|arn| arn_region(arn) == far),
            "case {case:?}: every ARN {far} lists must carry {far}, got {far_listing:?}"
        );
        let far_definitions = listed_task_definitions(&router, far, &case).await;
        assert!(
            !far_definitions.contains(&definition),
            "case {case:?}: {far} must list none of {home}'s revisions, got {far_definitions:?}"
        );
        assert!(
            far_definitions.iter().all(|arn| arn_region(arn) == far),
            "case {case:?}: every revision ARN {far} lists must carry {far}, got \
             {far_definitions:?}"
        );

        // A batch describe reports the name as MISSING, under the ARN THIS endpoint would
        // carry for it rather than the one the other Region holds.
        let described = ok_in(
            &router,
            far,
            "DescribeClusters",
            json!({ "clusters": [name.clone()] }),
            &case,
        )
        .await;
        assert!(
            list(&described, "clusters", &case).is_empty(),
            "case {case:?}: {far} must describe no cluster of {home}, got {described}"
        );
        let failures = list(&described, "failures", &case);
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: {far} must report one failure for the name, got {described}"
        );
        assert_eq!(
            string(&failures[0], "reason", &case),
            MISSING,
            "case {case:?}: the failure's reason must be {MISSING}, got {described}"
        );
        assert_eq!(
            string(&failures[0], "arn", &case),
            cluster_arn(far, &name),
            "case {case:?}: the failure's ARN must carry the serving Region, got {described}"
        );

        // And a describe of the revision is answered as the revision this endpoint has none
        // of, in the class that condition carries.
        let (code, message) = refusal_in(
            &router,
            far,
            "DescribeTaskDefinition",
            json!({ "taskDefinition": family.clone() }),
            &case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("ClientException", "Unable to describe task definition."),
            "case {case:?}: {far} must refuse a describe of {home}'s family, got {code} \
             {message}"
        );
    }
}

/// Every cluster-scoped operation resolves its `cluster` in the Region it was addressed to:
/// the same request is served by the Region that holds the cluster and refused by the one
/// that does not, with the cluster refusal that condition carries.
#[tokio::test]
async fn cluster_scoped_operations_resolve_the_cluster_of_their_own_region() {
    let router = crate::ecs_wire::service();
    let home = "us-west-2";
    let far = "us-east-2";
    let cluster = "scopedcluster";
    create_cluster_in(&router, home, cluster, "setup").await;

    // One row per operation kind the rule reaches: a listing of what the cluster holds, a
    // batch read, a read of the cluster's attributes, a mutation of the cluster itself and
    // the delete.
    let rows: [(&str, Value); 7] = [
        ("ListServices", json!({ "cluster": cluster })),
        ("ListTasks", json!({ "cluster": cluster })),
        ("ListContainerInstances", json!({ "cluster": cluster })),
        (
            "ListAttributes",
            json!({ "cluster": cluster, "targetType": "container-instance" }),
        ),
        (
            "DescribeServices",
            json!({ "cluster": cluster, "services": ["anything"] }),
        ),
        (
            "UpdateCluster",
            json!({ "cluster": cluster, "settings": [{ "name": "containerInsights", "value": "enabled" }] }),
        ),
        ("DeleteCluster", json!({ "cluster": cluster })),
    ];
    for (operation, request) in rows {
        let case = format!("{operation} against a cluster of another Region");
        let (code, _) = refusal_in(&router, far, operation, request.clone(), &case).await;
        assert_eq!(
            code, CLUSTER_NOT_FOUND,
            "case {case:?}: {far} must refuse the call about the cluster, got {code}"
        );
        // The very same request is served by the Region that holds the cluster — so the
        // refusal is about the Region and not about the request.
        ok_in(&router, home, operation, request, &case).await;
    }

    // The far Region's refusals changed nothing: the cluster is the home Region's to delete,
    // and the delete above is what took it away there.
    let after = listed_clusters(&router, home, "after the home delete").await;
    assert!(
        after.is_empty(),
        "case \"after the home delete\": the home Region's delete must be the one that \
         applied, got {after:?}"
    );
}

/// The store's cluster bound is ONE budget for the whole store rather than one per Region:
/// a caller that varies the Region its calls are signed for adds a key to that budget, so the
/// number of stored clusters stays bounded however many Regions it names.
#[tokio::test]
async fn the_cluster_bound_is_the_whole_store_and_not_one_per_region() {
    let bound =
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::MAX_CLUSTERS;
    let router = crate::ecs_wire::service();
    // The Regions the fill is spread across, so the bound is reached with no single Region
    // holding it alone.
    let spread = ["us-west-2", "us-east-2", "eu-west-1", "ap-northeast-1"];
    for index in 0..bound {
        let region = spread[index % spread.len()];
        let case = format!("filling the store from {region}");
        create_cluster_in(&router, region, &format!("filler{index}"), &case).await;
    }

    // A Region that holds nothing of its own gets no budget of its own either.
    let case = "a create in a Region the fill never touched";
    let (code, message) = refusal_in(
        &router,
        "ap-southeast-4",
        "CreateCluster",
        json!({ "clusterName": "onemore" }),
        case,
    )
    .await;
    assert_eq!(
        code, "ClientException",
        "case {case:?}: the store bound must refuse the create, got {code} {message}"
    );
    assert!(
        message.contains(&bound.to_string()),
        "case {case:?}: the refusal must name the bound {bound}, got {message:?}"
    );
    let listed = listed_clusters(&router, "ap-southeast-4", case).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: the refused create must store nothing, got {listed:?}"
    );
}

/// One cluster name is a cluster of each Region separately: creating it in two Regions
/// creates two clusters, each endpoint reports its own, and a delete in one leaves the other
/// untouched.
#[tokio::test]
async fn one_cluster_name_is_a_separate_cluster_in_each_region() {
    let router = crate::ecs_wire::service();
    let regions = ["us-west-2", "us-east-2", "eu-west-1"];
    let shared = "sharedname";

    for region in regions {
        let case = format!("{shared} created in {region}");
        let created = create_cluster_in(&router, region, shared, &case).await;
        assert_eq!(
            created,
            cluster_arn(region, shared),
            "case {case:?}: each Region's create must answer its own ARN, got {created}"
        );
    }

    for region in regions {
        let case = format!("{shared} as {region} holds it");
        let listed = listed_clusters(&router, region, &case).await;
        assert_eq!(
            listed,
            vec![cluster_arn(region, shared)],
            "case {case:?}: each endpoint lists exactly its own cluster, got {listed:?}"
        );
        let described = ok_in(
            &router,
            region,
            "DescribeClusters",
            json!({ "clusters": [shared] }),
            &case,
        )
        .await;
        let reported = &list(&described, "clusters", &case)[0];
        assert_eq!(
            (
                string(reported, "clusterArn", &case),
                string(reported, "status", &case)
            ),
            (cluster_arn(region, shared), "ACTIVE".to_string()),
            "case {case:?}: each endpoint describes its own ACTIVE cluster, got {described}"
        );
    }

    // A delete is the delete of ONE Region's cluster.
    let deleted_in = regions[0];
    ok_in(
        &router,
        deleted_in,
        "DeleteCluster",
        json!({ "cluster": shared }),
        "deleting one Region's copy",
    )
    .await;
    for region in regions {
        let case = format!("{shared} in {region} after the delete in {deleted_in}");
        let listed = listed_clusters(&router, region, &case).await;
        let expected: Vec<String> = if region == deleted_in {
            Vec::new()
        } else {
            vec![cluster_arn(region, shared)]
        };
        assert_eq!(
            listed, expected,
            "case {case:?}: the delete applies to one Region's cluster alone, got {listed:?}"
        );
    }
}
