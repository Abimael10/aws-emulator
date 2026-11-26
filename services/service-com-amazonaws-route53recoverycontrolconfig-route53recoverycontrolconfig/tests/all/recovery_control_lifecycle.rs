// SPDX-License-Identifier: CC0-1.0

//! The recovery-control lifecycle, end to end over HTTP: cluster -> control panel ->
//! routing control, then back down again.
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the handlers and the shared engine
//! together — not the handlers' Rust signatures.
//!
//! The tables below span the classes each rule generalises over: every `NetworkType`
//! the model declares plus the omitted case, page sizes from 1 to past the item count,
//! the published bounds of `MaxResults` and one step outside each, and every resource
//! level of the hierarchy for the not-found and conflict rules.
//!
//! Two suites here cover the members the endpoint puts on the wire BESIDE the
//! ones the public model declares: the generic `Arn` member a control panel and a safety
//! rule carry beside their typed ARN member, and the Route 53 health-check sequence
//! number a routing control carries — each asserted over every operation that projects
//! the resource rather than the one read that exposed it.
//!
//! The last four suites are about what FOLLOWS a routing control: the Amazon Route 53
//! health checks `ListAssociatedRoute53HealthChecks` reports. Those are resources of
//! Route 53, so they are built through that service's own API over the same shared engine,
//! and read back here.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The HTTP harness every suite in this target shares.
use super::common::{
    call, create_cluster, create_control_panel, create_routing_control, enc, error_code,
    panel_with_controls, router, REGION,
};

/// The account id every emulated ARN carries.
pub(crate) const ACCOUNT: &str = "000000000000";

/// The ARN prefix AWS documents for recovery-control resources:
/// `arn:aws:route53-recovery-control::<account>:...`.
pub(crate) const ARN_PREFIX: &str = "arn:aws:route53-recovery-control::000000000000:";

/// The `Status` values the model declares, via
/// `smithy_query(query="shape",shape="Status")`: `DEPLOYED | PENDING | PENDING_DELETION`.
pub(crate) const STATUS_PENDING: &str = "PENDING";
pub(crate) const STATUS_DEPLOYED: &str = "DEPLOYED";

/// The Regions a cluster publishes a data-plane endpoint in — five, "one for each
/// supported Amazon Web Services Region" (`CreateCluster`'s own documentation). The order
/// a cluster publishes them in is [`published_endpoint_regions`]; this is the SET.
pub(crate) const ENDPOINT_REGIONS: &[&str] = &[
    "ap-northeast-1",
    "ap-southeast-2",
    "eu-west-1",
    "us-east-1",
    "us-west-2",
];

/// The Region labels a cluster whose own Region is `home_region` publishes its endpoints
/// under, in the order it publishes them: the cluster's OWN Region first — the Region the
/// cluster was created in and is addressed on — then the remaining supported Regions.
///
/// A `home_region` that is none of the supported five publishes no endpoint of its own, so
/// there is nothing for it to lead with and the supported Regions keep their own order.
pub(crate) fn published_endpoint_regions(home_region: &str) -> Vec<&'static str> {
    let own = ENDPOINT_REGIONS
        .iter()
        .copied()
        .filter(|region| *region == home_region);
    let rest = ENDPOINT_REGIONS
        .iter()
        .copied()
        .filter(|region| *region != home_region);
    own.chain(rest).collect()
}

/// Longer than the span the service gives a `PENDING` -> `DEPLOYED` transition, so a
/// read after it must observe the deployed resource.
pub(crate) const PAST_DEPLOYMENT: Duration = Duration::from_millis(900);

// The constants above are `pub(crate)` because sibling suites in this same test target
// reuse them rather than keeping a second copy: see `tests/all/update_cluster.rs`.

/// A cluster is answered from the record the create call stored: `DescribeCluster`
/// returns the same name, endpoints, network type and owner, for every `NetworkType`
/// the model declares and for a request that names none.
#[tokio::test]
async fn create_persists_the_cluster_and_describe_answers_the_same_body() {
    let router = router();

    // Rows differ along the member the endpoint list and the echoed network type are
    // projected from. Source for the value set: `smithy_query(query="shape",
    // shape="NetworkType")` -> DUALSTACK | IPV4. The `None` row is the omitted
    // member, which the model gives no default.
    let cases: &[(&str, Option<&str>, &str)] = &[
        ("ipv4-explicit", Some("IPV4"), "IPV4"),
        ("dualstack-explicit", Some("DUALSTACK"), "DUALSTACK"),
        ("network-type-omitted", None, "IPV4"),
    ];

    let mut arns: Vec<String> = Vec::new();
    for (name, requested, expected) in cases {
        let mut body = json!({ "ClusterName": name, "Tags": { "team": "arc" } });
        if let Some(requested) = requested {
            body["NetworkType"] = json!(requested);
        }
        let created = create_cluster(&router, body).await;

        let arn = created["ClusterArn"].as_str().unwrap_or_default().to_string();
        assert!(
            arn.starts_with(ARN_PREFIX) && arn[ARN_PREFIX.len()..].starts_with("cluster/"),
            "case {name}: expected an ARN under {ARN_PREFIX}cluster/, got {arn:?}"
        );
        assert!(
            !arn.contains(REGION),
            "case {name}: a recovery-control ARN carries no Region, got {arn:?}"
        );
        assert_eq!(
            created["Name"], json!(name),
            "case {name}: expected the requested name, got {:?}",
            created["Name"]
        );
        assert_eq!(
            created["Owner"], json!(ACCOUNT),
            "case {name}: expected owner {ACCOUNT}, got {:?}",
            created["Owner"]
        );
        assert_eq!(
            created["NetworkType"], json!(expected),
            "case {name}: expected NetworkType {expected}, got {:?}",
            created["NetworkType"]
        );
        assert_eq!(
            created["Status"], json!(STATUS_PENDING),
            "case {name}: a freshly created cluster is {STATUS_PENDING}, got {:?}",
            created["Status"]
        );

        let endpoints = created["ClusterEndpoints"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let regions: Vec<&str> = endpoints
            .iter()
            .map(|e| e["Region"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            regions,
            published_endpoint_regions(REGION),
            "case {name}: expected one endpoint per supported Region, the cluster's own \
             Region first, got {regions:?}"
        );
        // `Endpoint` is the address a client SENDS a routing-control state call to, so
        // every entry is the base this emulator is reachable at — the Region label it is
        // published under is `Region`, not part of the address. The whole rule, over
        // several bases, is `crate::cluster_endpoint_addressing`.
        for endpoint in &endpoints {
            let url = endpoint["Endpoint"].as_str().unwrap_or_default();
            let region = endpoint["Region"].as_str().unwrap_or_default();
            assert_eq!(
                url,
                smithy_server_runtime::UNSERVED_URL_BASE,
                "case {name}: the endpoint for {region} must be the base this emulator is \
                 reachable at, got {url:?}"
            );
        }

        assert!(
            !arns.contains(&arn),
            "case {name}: each create mints a fresh ARN, but {arn} repeated"
        );
        arns.push(arn.clone());

        // Read the cluster back: every member must come from the stored record.
        let (status, read) = call(&router, "GET", &format!("/cluster/{}", enc(&arn)), None).await;
        assert_eq!(
            status, 200,
            "case {name}: expected DescribeCluster 200, got {status} {read:?}"
        );
        let read = read["Cluster"].clone();
        for member in ["ClusterArn", "Name", "Owner", "NetworkType", "ClusterEndpoints"] {
            assert_eq!(
                read[member], created[member],
                "case {name}: DescribeCluster must answer the stored {member}, got {:?} after {:?}",
                read[member], created[member]
            );
        }
    }
}

/// A cluster AWS returns `PENDING` reaches `DEPLOYED` on its own, without any call
/// moving it, and every resource of the hierarchy behaves the same way.
#[tokio::test]
async fn pending_resources_reach_deployed_without_being_written() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "deploying" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "panel" }),
    )
    .await;
    let panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let control = create_routing_control(
        &router,
        json!({
            "ClusterArn": cluster_arn,
            "ControlPanelArn": panel_arn,
            "RoutingControlName": "rc",
        }),
    )
    .await;
    let control_arn = control["RoutingControlArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for (level, created) in [
        ("cluster", &cluster),
        ("control panel", &panel),
        ("routing control", &control),
    ] {
        assert_eq!(
            created["Status"], json!(STATUS_PENDING),
            "case {level}: a create answers {STATUS_PENDING}, got {:?}",
            created["Status"]
        );
    }

    // No sleeping inside the handlers: the transition is due at an instant the create
    // recorded, so waiting out that span here is what makes it observable.
    std::thread::sleep(PAST_DEPLOYMENT);

    let reads: &[(&str, String, &str)] = &[
        ("cluster", format!("/cluster/{}", enc(&cluster_arn)), "Cluster"),
        (
            "control panel",
            format!("/controlpanel/{}", enc(&panel_arn)),
            "ControlPanel",
        ),
        (
            "routing control",
            format!("/routingcontrol/{}", enc(&control_arn)),
            "RoutingControl",
        ),
    ];
    for (level, path, member) in reads {
        let (status, read) = call(&router, "GET", path, None).await;
        assert_eq!(
            status, 200,
            "case {level}: expected 200 from {path}, got {status} {read:?}"
        );
        assert_eq!(
            read[member]["Status"], json!(STATUS_DEPLOYED),
            "case {level}: expected {STATUS_DEPLOYED} once the deployment is due, got {:?}",
            read[member]["Status"]
        );
    }
}

/// `RoutingControlCount` is counted from the routing controls actually stored under the
/// control panel: it tracks every create and every delete, at every count.
#[tokio::test]
async fn routing_control_count_tracks_the_stored_controls() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "counting" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "counted-panel" }),
    )
    .await;
    let panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    assert_eq!(
        panel["RoutingControlCount"], json!(0),
        "case fresh panel: expected 0 routing controls, got {:?}",
        panel["RoutingControlCount"]
    );
    assert_eq!(
        panel["DefaultControlPanel"], json!(false),
        "case fresh panel: a caller-created panel is not the default one, got {:?}",
        panel["DefaultControlPanel"]
    );
    assert_eq!(
        panel["ClusterArn"], json!(cluster_arn),
        "case fresh panel: expected the stored parent ARN, got {:?}",
        panel["ClusterArn"]
    );

    // Create three controls, asserting the count after each one, then delete them in
    // the reverse order, asserting it again: the count follows the store both ways.
    let mut created: Vec<String> = Vec::new();
    for n in 1..=3u32 {
        let control = create_routing_control(
            &router,
            json!({
                "ClusterArn": cluster_arn,
                "ControlPanelArn": panel_arn,
                "RoutingControlName": format!("rc-{n}"),
            }),
        )
        .await;
        created.push(
            control["RoutingControlArn"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        assert_eq!(
            control["ControlPanelArn"], json!(panel_arn),
            "case create rc-{n}: expected the owning panel ARN, got {:?}",
            control["ControlPanelArn"]
        );
        assert_eq!(
            described_count(&router, &panel_arn).await,
            i64::from(n),
            "case create rc-{n}: expected RoutingControlCount {n}"
        );
    }

    for (removed, arn) in created.iter().rev().enumerate() {
        let (status, body) = call(
            &router,
            "DELETE",
            &format!("/routingcontrol/{}", enc(arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case delete {arn}: expected 200, got {status} {body:?}"
        );
        let expected = 3 - i64::try_from(removed).expect("a small index") - 1;
        assert_eq!(
            described_count(&router, &panel_arn).await,
            expected,
            "case delete {arn}: expected RoutingControlCount {expected}"
        );
        // And the deleted control is gone from both readers.
        let (status, body) = call(
            &router,
            "GET",
            &format!("/routingcontrol/{}", enc(arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case delete {arn}: DescribeRoutingControl must answer 404, got {status} {body:?}"
        );
        let listed = list_routing_controls(&router, &panel_arn, None, None).await;
        assert!(
            !listed.contains(arn),
            "case delete {arn}: ListRoutingControls must not list it, got {listed:?}"
        );
    }
}

/// `DescribeControlPanel`'s `RoutingControlCount` for `panel_arn`.
async fn described_count(router: &MultiServiceRouter, panel_arn: &str) -> i64 {
    let (status, body) = call(
        router,
        "GET",
        &format!("/controlpanel/{}", enc(panel_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case describe {panel_arn}: expected 200, got {status} {body:?}"
    );
    body["ControlPanel"]["RoutingControlCount"]
        .as_i64()
        .unwrap_or(-1)
}

/// Every routing-control ARN `ListRoutingControls` returns for one page.
async fn list_routing_controls(
    router: &MultiServiceRouter,
    panel_arn: &str,
    max_results: Option<i32>,
    next_token: Option<&str>,
) -> Vec<String> {
    let (_, _, arns) = list_page(router, panel_arn, max_results, next_token).await;
    arns
}

/// One `ListRoutingControls` page: `(status, NextToken, the page's ARNs)`.
async fn list_page(
    router: &MultiServiceRouter,
    panel_arn: &str,
    max_results: Option<i32>,
    next_token: Option<&str>,
) -> (u16, Option<String>, Vec<String>) {
    let mut query: Vec<String> = Vec::new();
    if let Some(max) = max_results {
        query.push(format!("MaxResults={max}"));
    }
    if let Some(token) = next_token {
        query.push(format!("NextToken={}", enc(token)));
    }
    let suffix = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    let path = format!("/controlpanel/{}/routingcontrols{suffix}", enc(panel_arn));
    let (status, body) = call(router, "GET", &path, None).await;
    let arns = body["RoutingControls"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|rc| rc["RoutingControlArn"].as_str().unwrap_or_default().to_string())
        .collect();
    (
        status,
        body["NextToken"].as_str().map(ToString::to_string),
        arns,
    )
}

/// Paging through the stored routing controls yields each item exactly once, in a
/// stable order, for every page size from one to past the item count.
#[tokio::test]
async fn paging_yields_every_stored_control_exactly_once() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "paging" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "paged-panel" }),
    )
    .await;
    let panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    const TOTAL: usize = 5;
    let mut expected: Vec<String> = Vec::new();
    for n in 0..TOTAL {
        let control = create_routing_control(
            &router,
            json!({
                "ClusterArn": cluster_arn,
                "ControlPanelArn": panel_arn,
                "RoutingControlName": format!("paged-{n}"),
            }),
        )
        .await;
        expected.push(
            control["RoutingControlArn"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    // Page sizes spanning the class: one at a time, a size that does not divide the
    // total, the exact total, one past it, and the request that names no MaxResults.
    let cases: &[Option<i32>] = &[Some(1), Some(2), Some(3), Some(5), Some(6), None];
    for max_results in cases {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let (status, next, arns) =
                list_page(&router, &panel_arn, *max_results, token.as_deref()).await;
            assert_eq!(
                status, 200,
                "case MaxResults={max_results:?}: expected 200, got {status}"
            );
            if let Some(max) = max_results {
                assert!(
                    arns.len() <= *max as usize,
                    "case MaxResults={max_results:?}: a page holds at most {max} items, got {}",
                    arns.len()
                );
            }
            seen.extend(arns);
            pages += 1;
            assert!(
                pages <= TOTAL + 1,
                "case MaxResults={max_results:?}: paging did not terminate after {pages} pages"
            );
            match next {
                None => break,
                Some(next) => token = Some(next),
            }
        }
        assert_eq!(
            seen, expected,
            "case MaxResults={max_results:?}: paging must yield every stored control once, \
             in creation order, got {seen:?}"
        );
    }

    // A cursor that names nothing stored cannot be resumed from.
    let (status, body) = call(
        &router,
        "GET",
        &format!(
            "/controlpanel/{}/routingcontrols?NextToken={}",
            enc(&panel_arn),
            enc("arn:aws:route53-recovery-control::000000000000:controlpanel/x/routingcontrol/y")
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 400,
        "case unknown NextToken: expected 400, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case unknown NextToken: expected ValidationException, got {body:?}"
    );
}

/// A replayed `ClientToken` returns the resource the first call created; a token
/// replayed with different parameters is refused; two different tokens are two
/// resources.
#[tokio::test]
async fn a_replayed_client_token_returns_the_first_resource() {
    let router = router();

    let first = create_cluster(
        &router,
        json!({ "ClusterName": "idem", "ClientToken": "token-a" }),
    )
    .await;
    let replay = create_cluster(
        &router,
        json!({ "ClusterName": "idem", "ClientToken": "token-a" }),
    )
    .await;
    assert_eq!(
        replay["ClusterArn"], first["ClusterArn"],
        "case same token, same body: expected the first cluster back, got {:?} after {:?}",
        replay["ClusterArn"], first["ClusterArn"]
    );

    // Same token, different parameters: not a retry of the same request.
    let (status, body) = call(
        &router,
        "POST",
        "/cluster",
        Some(json!({ "ClusterName": "idem-other", "ClientToken": "token-a" })),
    )
    .await;
    assert_eq!(
        status, 409,
        "case same token, different body: expected 409, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ConflictException",
        "case same token, different body: expected ConflictException, got {body:?}"
    );

    // The same rule holds one level down, for a control panel.
    let cluster_arn = first["ClusterArn"].as_str().unwrap_or_default().to_string();
    let panel = create_control_panel(
        &router,
        json!({
            "ClusterArn": cluster_arn,
            "ControlPanelName": "idem-panel",
            "ClientToken": "token-b",
        }),
    )
    .await;
    let panel_replay = create_control_panel(
        &router,
        json!({
            "ClusterArn": cluster_arn,
            "ControlPanelName": "idem-panel",
            "ClientToken": "token-b",
        }),
    )
    .await;
    assert_eq!(
        panel_replay["ControlPanelArn"], panel["ControlPanelArn"],
        "case same token, same body (panel): expected the first panel back, got {:?}",
        panel_replay["ControlPanelArn"]
    );

    // A second cluster under a FRESH token is a second cluster.
    let other = create_cluster(
        &router,
        json!({ "ClusterName": "idem-two", "ClientToken": "token-c" }),
    )
    .await;
    assert_ne!(
        other["ClusterArn"], first["ClusterArn"],
        "case fresh token: expected a second cluster, got {:?}",
        other["ClusterArn"]
    );

    // Deleting the resource releases its token, so the ledger cannot be grown by a
    // create/delete loop and no token reports a resource that is gone.
    let other_arn = other["ClusterArn"].as_str().unwrap_or_default().to_string();
    let (status, body) = call(
        &router,
        "DELETE",
        &format!("/cluster/{}", enc(&other_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case delete the token's cluster: expected 200, got {status} {body:?}"
    );
    let recreated = create_cluster(
        &router,
        json!({ "ClusterName": "idem-two", "ClientToken": "token-c" }),
    )
    .await;
    assert_ne!(
        recreated["ClusterArn"], json!(other_arn),
        "case replay after delete: the released token must not name the deleted cluster, \
         got {:?}",
        recreated["ClusterArn"]
    );
}

/// A name identifies a resource within its parent, at the levels where this service
/// scopes names to one: a control panel inside its cluster, a routing control inside its
/// control panel. The refusal is of the request's input (400), not a transient collision.
///
/// Cluster names are NOT in this table — they are labels, not identities; see
/// `tests/all/rejection_error_classes.rs`.
#[tokio::test]
async fn a_duplicate_name_within_the_same_parent_is_refused() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "dup" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();
    let other = create_cluster(&router, json!({ "ClusterName": "dup-other" })).await;
    let other_arn = other["ClusterArn"].as_str().unwrap_or_default().to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "dup-panel" }),
    )
    .await;
    let panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    create_routing_control(
        &router,
        json!({
            "ClusterArn": cluster_arn,
            "ControlPanelArn": panel_arn,
            "RoutingControlName": "dup-rc",
        }),
    )
    .await;

    // One row per resource level that scopes names to a parent: the same name under the
    // same parent is refused.
    let conflicts: &[(&str, &str, Value)] = &[
        (
            "control panel",
            "/controlpanel",
            json!({ "ClusterArn": cluster_arn, "ControlPanelName": "dup-panel" }),
        ),
        (
            "routing control",
            "/routingcontrol",
            json!({
                "ClusterArn": cluster_arn,
                "ControlPanelArn": panel_arn,
                "RoutingControlName": "dup-rc",
            }),
        ),
    ];
    for (level, path, body) in conflicts {
        let (status, answer) = call(&router, "POST", path, Some(body.clone())).await;
        assert_eq!(
            status, 400,
            "case duplicate {level}: expected 400, got {status} {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "ValidationException",
            "case duplicate {level}: expected ValidationException, got {answer:?}"
        );
    }

    // The same name under a DIFFERENT parent is a different resource.
    let elsewhere = create_control_panel(
        &router,
        json!({ "ClusterArn": other_arn, "ControlPanelName": "dup-panel" }),
    )
    .await;
    assert_ne!(
        elsewhere["ControlPanelArn"], panel["ControlPanelArn"],
        "case same name, other cluster: expected a second panel, got {:?}",
        elsewhere["ControlPanelArn"]
    );
}

/// An ARN no resource is stored under is `ResourceNotFoundException` (404) on every read
/// and delete that ADDRESSES it.
///
/// A create that only names it as a parent is refused on its input instead — see
/// `tests/all/rejection_error_classes.rs`.
#[tokio::test]
async fn an_unknown_arn_is_not_found_everywhere() {
    let router = router();

    let missing_cluster = format!("{ARN_PREFIX}cluster/00000000-0000-0000-0000-000000000000");
    let missing_panel = format!("{ARN_PREFIX}controlpanel/00000000-0000-0000-0000-000000000000");
    let missing_control = format!("{missing_panel}/routingcontrol/11111111-1111-1111-1111-111111111111");

    // Rows span the operation kinds (read, delete, list) and all three resource levels.
    let cases: Vec<(String, &str, String, Option<Value>)> = vec![
        (
            "DescribeCluster".to_string(),
            "GET",
            format!("/cluster/{}", enc(&missing_cluster)),
            None,
        ),
        (
            "DeleteCluster".to_string(),
            "DELETE",
            format!("/cluster/{}", enc(&missing_cluster)),
            None,
        ),
        (
            "DescribeControlPanel".to_string(),
            "GET",
            format!("/controlpanel/{}", enc(&missing_panel)),
            None,
        ),
        (
            "DeleteControlPanel".to_string(),
            "DELETE",
            format!("/controlpanel/{}", enc(&missing_panel)),
            None,
        ),
        (
            "DescribeRoutingControl".to_string(),
            "GET",
            format!("/routingcontrol/{}", enc(&missing_control)),
            None,
        ),
        (
            "DeleteRoutingControl".to_string(),
            "DELETE",
            format!("/routingcontrol/{}", enc(&missing_control)),
            None,
        ),
        (
            "ListRoutingControls".to_string(),
            "GET",
            format!("/controlpanel/{}/routingcontrols", enc(&missing_panel)),
            None,
        ),
    ];

    for (case, method, path, body) in cases {
        let (status, answer) = call(&router, method, &path, body).await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "ResourceNotFoundException",
            "case {case}: expected ResourceNotFoundException, got {answer:?}"
        );
    }
}

/// A request outside a modelled constraint is refused with 400, under the class of the
/// layer that refused it — the request-body validator for a string constraint on a body
/// member, the modelled `ValidationException` for a query parameter outside its `@range`
/// — and a request on the published bounds is accepted.
#[tokio::test]
async fn constraint_violations_are_refused_and_the_bounds_are_accepted() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "bounds" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "bounds-panel" }),
    )
    .await;
    let panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // `ClusterName`/`ControlPanelName` are len 1..64 pattern ^\S+$, and
    // `MaxResults` is range 1..1000 (`smithy_query(query="operation", ...)`). The last
    // column is the class the refusal carries: the body members' string constraints are
    // the request-body validator's, `MaxResults` is a query parameter it never sees.
    // The message of each body refusal is pinned in
    // `tests/all/rejection_error_classes.rs`.
    let sixty_five = "a".repeat(65);
    let refusals: Vec<(&str, &str, String, Option<Value>, &str)> = vec![
        (
            "empty ClusterName",
            "POST",
            "/cluster".to_string(),
            Some(json!({ "ClusterName": "" })),
            "BadRequestException",
        ),
        (
            "ClusterName one past the length maximum",
            "POST",
            "/cluster".to_string(),
            Some(json!({ "ClusterName": sixty_five })),
            "BadRequestException",
        ),
        (
            "ClusterName with whitespace breaks ^\\S+$",
            "POST",
            "/cluster".to_string(),
            Some(json!({ "ClusterName": "has space" })),
            "BadRequestException",
        ),
        (
            "empty ControlPanelName",
            "POST",
            "/controlpanel".to_string(),
            Some(json!({ "ClusterArn": cluster_arn, "ControlPanelName": "" })),
            "BadRequestException",
        ),
        (
            "MaxResults one below the range minimum",
            "GET",
            format!("/controlpanel/{}/routingcontrols?MaxResults=0", enc(&panel_arn)),
            None,
            "ValidationException",
        ),
        (
            "MaxResults one above the range maximum",
            "GET",
            format!(
                "/controlpanel/{}/routingcontrols?MaxResults=1001",
                enc(&panel_arn)
            ),
            None,
            "ValidationException",
        ),
    ];
    for (case, method, path, body, expected) in refusals {
        let (status, answer) = call(&router, method, &path, body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            expected,
            "case {case}: expected {expected}, got {answer:?}"
        );
    }

    // Both published `MaxResults` bounds are inside the range and must be served.
    for max in [1, 1000] {
        let (status, _, _) = list_page(&router, &panel_arn, Some(max), None).await;
        assert_eq!(
            status, 200,
            "case MaxResults={max} (a published bound): expected 200, got {status}"
        );
    }

    // A 64-character name is on the length maximum and must be accepted.
    let sixty_four = "b".repeat(64);
    let accepted = create_cluster(&router, json!({ "ClusterName": sixty_four })).await;
    assert_eq!(
        accepted["Name"], json!(sixty_four),
        "case ClusterName on the length maximum: expected it stored, got {:?}",
        accepted["Name"]
    );
}

/// A routing control created without a `ControlPanelArn` lands in the cluster's default
/// control panel — the one the cluster was created with.
#[tokio::test]
async fn a_routing_control_without_a_panel_lands_in_the_default_panel() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "defaulting" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();

    let control = create_routing_control(
        &router,
        json!({ "ClusterArn": cluster_arn, "RoutingControlName": "implicit" }),
    )
    .await;
    let panel_arn = control["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        panel_arn.starts_with(&format!("{ARN_PREFIX}controlpanel/")),
        "case implicit panel: expected a control-panel ARN, got {panel_arn:?}"
    );

    let (status, body) = call(
        &router,
        "GET",
        &format!("/controlpanel/{}", enc(&panel_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case implicit panel: expected DescribeControlPanel 200, got {status} {body:?}"
    );
    let panel = body["ControlPanel"].clone();
    assert_eq!(
        panel["DefaultControlPanel"], json!(true),
        "case implicit panel: expected the cluster's DEFAULT panel, got {:?}",
        panel["DefaultControlPanel"]
    );
    assert_eq!(
        panel["ClusterArn"], json!(cluster_arn),
        "case implicit panel: expected the panel of this cluster, got {:?}",
        panel["ClusterArn"]
    );
    assert_eq!(
        panel["RoutingControlCount"], json!(1),
        "case implicit panel: expected the new control counted, got {:?}",
        panel["RoutingControlCount"]
    );

    // A control panel belonging to ANOTHER cluster is a contradictory request.
    let other = create_cluster(&router, json!({ "ClusterName": "defaulting-other" })).await;
    let other_arn = other["ClusterArn"].as_str().unwrap_or_default().to_string();
    let (status, answer) = call(
        &router,
        "POST",
        "/routingcontrol",
        Some(json!({
            "ClusterArn": other_arn,
            "ControlPanelArn": panel_arn,
            "RoutingControlName": "crossed",
        })),
    )
    .await;
    assert_eq!(
        status, 400,
        "case panel of another cluster: expected 400, got {status} {answer:?}"
    );
    assert_eq!(
        error_code(&answer),
        "ValidationException",
        "case panel of another cluster: expected ValidationException, got {answer:?}"
    );
}

/// The store is bounded: a caller cannot grow it without limit, and the create refuses
/// past the ceiling with the error the model declares for it.
#[tokio::test]
async fn a_full_store_refuses_another_create_with_the_modelled_quota_error() {
    let router = router();

    // The ceiling this service holds the cluster store to.
    let max_clusters =
        service_com_amazonaws_route53recoverycontrolconfig_route53recoverycontrolconfig::state::MAX_CLUSTERS;

    let mut arns: Vec<String> = Vec::new();
    for n in 0..max_clusters {
        let created = create_cluster(&router, json!({ "ClusterName": format!("bounded-{n}") })).await;
        arns.push(
            created["ClusterArn"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    let (status, body) = call(
        &router,
        "POST",
        "/cluster",
        Some(json!({ "ClusterName": "one-too-many" })),
    )
    .await;
    assert_eq!(
        status, 402,
        "case one past the ceiling of {max_clusters}: expected 402, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ServiceQuotaExceededException",
        "case one past the ceiling: expected ServiceQuotaExceededException, got {body:?}"
    );

    // Deleting one makes room again, so the bound is a ceiling and not a dead end.
    let freed = arns.first().expect("at least one cluster was created").clone();
    let (status, body) = call(&router, "DELETE", &format!("/cluster/{}", enc(&freed)), None).await;
    assert_eq!(
        status, 200,
        "case delete to make room: expected 200, got {status} {body:?}"
    );
    let (status, body) = call(
        &router,
        "POST",
        "/cluster",
        Some(json!({ "ClusterName": "one-too-many" })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case create after making room: expected 200, got {status} {body:?}"
    );
}

#[tokio::test]
async fn a_parent_with_children_cannot_be_deleted() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "teardown" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();
    let panel = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "teardown-panel" }),
    )
    .await;
    let panel_arn = panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let control = create_routing_control(
        &router,
        json!({
            "ClusterArn": cluster_arn,
            "ControlPanelArn": panel_arn,
            "RoutingControlName": "teardown-rc",
        }),
    )
    .await;
    let control_arn = control["RoutingControlArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    // The default control panel of this cluster, reached through a routing control
    // created without a panel.
    let implicit = create_routing_control(
        &router,
        json!({ "ClusterArn": cluster_arn, "RoutingControlName": "implicit" }),
    )
    .await;
    let default_panel_arn = implicit["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let implicit_arn = implicit["RoutingControlArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Rows: a cluster with a control panel, a control panel with a routing control,
    // and the default control panel, which lives and dies with its cluster.
    let conflicts: &[(&str, String)] = &[
        ("cluster with a control panel", format!("/cluster/{}", enc(&cluster_arn))),
        (
            "control panel with a routing control",
            format!("/controlpanel/{}", enc(&panel_arn)),
        ),
        (
            "default control panel",
            format!("/controlpanel/{}", enc(&default_panel_arn)),
        ),
    ];
    for (case, path) in conflicts {
        let (status, answer) = call(&router, "DELETE", path, None).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "ValidationException",
            "case {case}: expected ValidationException, got {answer:?}"
        );
    }

    // Tear down from the leaves up; each step then succeeds.
    for (case, path) in [
        (
            "routing control",
            format!("/routingcontrol/{}", enc(&control_arn)),
        ),
        (
            "implicit routing control",
            format!("/routingcontrol/{}", enc(&implicit_arn)),
        ),
        ("control panel", format!("/controlpanel/{}", enc(&panel_arn))),
        ("cluster", format!("/cluster/{}", enc(&cluster_arn))),
    ] {
        let (status, answer) = call(&router, "DELETE", &path, None).await;
        assert_eq!(
            status, 200,
            "case delete {case}: expected 200, got {status} {answer:?}"
        );
    }

    // The cluster, and the default control panel that belonged to it, are both gone.
    for (case, path) in [
        ("cluster", format!("/cluster/{}", enc(&cluster_arn))),
        (
            "default control panel",
            format!("/controlpanel/{}", enc(&default_panel_arn)),
        ),
    ] {
        let (status, answer) = call(&router, "GET", &path, None).await;
        assert_eq!(
            status, 404,
            "case read deleted {case}: expected 404, got {status} {answer:?}"
        );
    }

    // And the name is free again once the cluster is gone.
    let reused = create_cluster(&router, json!({ "ClusterName": "teardown" })).await;
    assert_ne!(
        reused["ClusterArn"], cluster["ClusterArn"],
        "case recreate after delete: expected a fresh ARN, got {:?}",
        reused["ClusterArn"]
    );
}

/// One `ListControlPanels` page: `(status, NextToken, the page's `ControlPanel` bodies)`.
///
/// `ClusterArn`, `MaxResults` and `NextToken` are all `httpQuery` members
/// (`smithy_query(query="operation", operation="ListControlPanels")`).
async fn list_control_panels_page(
    router: &MultiServiceRouter,
    cluster_arn: Option<&str>,
    max_results: Option<i32>,
    next_token: Option<&str>,
) -> (u16, Option<String>, Vec<Value>) {
    let mut query: Vec<String> = Vec::new();
    if let Some(cluster) = cluster_arn {
        query.push(format!("ClusterArn={}", enc(cluster)));
    }
    if let Some(max) = max_results {
        query.push(format!("MaxResults={max}"));
    }
    if let Some(token) = next_token {
        query.push(format!("NextToken={}", enc(token)));
    }
    let suffix = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    let (status, body) = call(router, "GET", &format!("/controlpanels{suffix}"), None).await;
    let panels = body["ControlPanels"].as_array().cloned().unwrap_or_default();
    (
        status,
        body["NextToken"].as_str().map(ToString::to_string),
        panels,
    )
}

/// Page right through `ListControlPanels` for one scope and return every
/// `ControlPanelArn` it yielded, asserting each page honours `MaxResults`.
async fn drain_control_panels(
    router: &MultiServiceRouter,
    cluster_arn: Option<&str>,
    max_results: Option<i32>,
    expected_total: usize,
) -> Vec<String> {
    let case = format!("ClusterArn={cluster_arn:?} MaxResults={max_results:?}");
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let (status, next, panels) =
            list_control_panels_page(router, cluster_arn, max_results, token.as_deref()).await;
        assert_eq!(status, 200, "case {case}: expected 200, got {status}");
        if let Some(max) = max_results {
            assert!(
                panels.len() <= max as usize,
                "case {case}: a page holds at most {max} panels, got {}",
                panels.len()
            );
        }
        seen.extend(
            panels
                .iter()
                .map(|p| p["ControlPanelArn"].as_str().unwrap_or_default().to_string()),
        );
        pages += 1;
        assert!(
            pages <= expected_total + 1,
            "case {case}: paging did not terminate after {pages} pages"
        );
        match next {
            None => break,
            Some(next) => token = Some(next),
        }
    }
    seen
}

/// `ListControlPanels` answers from the stored panels: every panel of the account, or
/// only those of the cluster `ClusterArn` names, in creation order, for every page size
/// from one to past the item count — and each listed body is the one
/// `DescribeControlPanel` answers with.
#[tokio::test]
async fn listing_control_panels_pages_through_every_stored_panel_once() {
    let router = router();

    // Two clusters, each created with its own default control panel, then
    // caller-created panels in a known order: the expected order is creation order.
    let first = create_cluster(&router, json!({ "ClusterName": "listing-first" })).await;
    let first_arn = first["ClusterArn"].as_str().unwrap_or_default().to_string();
    let second = create_cluster(&router, json!({ "ClusterName": "listing-second" })).await;
    let second_arn = second["ClusterArn"].as_str().unwrap_or_default().to_string();

    // A routing control created without a panel lands in its cluster's DEFAULT panel,
    // which is how the test learns that panel's ARN without listing.
    let default_of = |cluster: String| {
        let router = &router;
        async move {
            let control = create_routing_control(
                router,
                json!({ "ClusterArn": cluster, "RoutingControlName": "default-probe" }),
            )
            .await;
            control["ControlPanelArn"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        }
    };
    let default_first = default_of(first_arn.clone()).await;
    let default_second = default_of(second_arn.clone()).await;

    let mut created_first: Vec<String> = Vec::new();
    for name in ["first-a", "first-b"] {
        let panel = create_control_panel(
            &router,
            json!({ "ClusterArn": first_arn, "ControlPanelName": name }),
        )
        .await;
        created_first.push(
            panel["ControlPanelArn"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    let second_panel = create_control_panel(
        &router,
        json!({ "ClusterArn": second_arn, "ControlPanelName": "second-a" }),
    )
    .await;
    let second_panel_arn = second_panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let expected_first: Vec<String> = std::iter::once(default_first.clone())
        .chain(created_first.iter().cloned())
        .collect();
    let expected_second: Vec<String> =
        vec![default_second.clone(), second_panel_arn.clone()];
    // Account-wide: both default panels (minted with their clusters) precede the
    // caller-created ones, in the order the calls were made.
    let expected_all: Vec<String> = vec![
        default_first.clone(),
        default_second.clone(),
        created_first[0].clone(),
        created_first[1].clone(),
        second_panel_arn.clone(),
    ];

    // Rows span the scope (no filter, each cluster) and page sizes from one at a time,
    // through a size that does not divide the total, the exact total, one past it, and
    // the request that names no MaxResults.
    let scopes: &[(Option<&str>, &Vec<String>)] = &[
        (None, &expected_all),
        (Some(first_arn.as_str()), &expected_first),
        (Some(second_arn.as_str()), &expected_second),
    ];
    let page_sizes: &[Option<i32>] = &[Some(1), Some(2), Some(3), Some(5), Some(6), None];
    for (scope, expected) in scopes {
        for max_results in page_sizes {
            let seen = drain_control_panels(&router, *scope, *max_results, expected.len()).await;
            assert_eq!(
                &seen, *expected,
                "case ClusterArn={scope:?} MaxResults={max_results:?}: paging must yield every \
                 stored panel once, in creation order, got {seen:?}"
            );
        }
    }

    // Every listed body is the stored record, so it matches the single-resource reader
    // member for member — including the counted RoutingControlCount.
    let (_, _, listed) = list_control_panels_page(&router, None, None, None).await;
    for panel in &listed {
        let arn = panel["ControlPanelArn"].as_str().unwrap_or_default();
        let (status, body) = call(&router, "GET", &format!("/controlpanel/{}", enc(arn)), None).await;
        assert_eq!(
            status, 200,
            "case listed {arn}: expected DescribeControlPanel 200, got {status} {body:?}"
        );
        assert_eq!(
            &body["ControlPanel"], panel,
            "case listed {arn}: the list body must be the stored record, got {:?} after {panel:?}",
            body["ControlPanel"]
        );
    }
    // The default panels carry the probe control created above; the caller-created ones
    // carry none.
    for (arn, expected_count) in [
        (default_first.as_str(), 1),
        (default_second.as_str(), 1),
        (created_first[0].as_str(), 0),
        (second_panel_arn.as_str(), 0),
    ] {
        let listed_count = listed
            .iter()
            .find(|p| p["ControlPanelArn"].as_str() == Some(arn))
            .map(|p| p["RoutingControlCount"].clone())
            .unwrap_or(Value::Null);
        assert_eq!(
            listed_count, json!(expected_count),
            "case listed {arn}: expected RoutingControlCount {expected_count}, got {listed_count:?}"
        );
    }

    // A deleted panel leaves the listing, and the list stays a pure read: listing twice
    // yields the same page.
    let (status, body) = call(
        &router,
        "DELETE",
        &format!("/controlpanel/{}", enc(&second_panel_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case delete a listed panel: expected 200, got {status} {body:?}"
    );
    let after = drain_control_panels(&router, Some(&second_arn), None, 2).await;
    assert_eq!(
        after,
        vec![default_second.clone()],
        "case after delete: the deleted panel must be gone, got {after:?}"
    );
    let again = drain_control_panels(&router, Some(&second_arn), None, 2).await;
    assert_eq!(
        again, after,
        "case repeated read: listing must not change what a later caller sees, got {again:?}"
    );
}

/// `ListControlPanels` refuses what it cannot serve: a `ClusterArn` no cluster is stored
/// under is 404, a cursor outside the selected set is 400, and `MaxResults` outside its
/// modelled range is 400 — while both published bounds are served.
#[tokio::test]
async fn listing_control_panels_refuses_an_unknown_scope_or_cursor() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "refusing" })).await;
    let cluster_arn = cluster["ClusterArn"].as_str().unwrap_or_default().to_string();
    let other = create_cluster(&router, json!({ "ClusterName": "refusing-other" })).await;
    let other_arn = other["ClusterArn"].as_str().unwrap_or_default().to_string();
    let other_panel = create_control_panel(
        &router,
        json!({ "ClusterArn": other_arn, "ControlPanelName": "other-panel" }),
    )
    .await;
    let other_panel_arn = other_panel["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // A cluster ARN of the right shape that no create ever minted.
    let missing_cluster = format!("{ARN_PREFIX}cluster/00000000-0000-0000-0000-000000000000");
    let (status, body) = call(
        &router,
        "GET",
        &format!("/controlpanels?ClusterArn={}", enc(&missing_cluster)),
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "case ClusterArn naming no stored cluster: expected 404, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "case ClusterArn naming no stored cluster: expected ResourceNotFoundException, got {body:?}"
    );

    // Cursor rows: a token naming nothing stored, in both scopes, and a token naming a
    // panel that IS stored but lies outside the filtered set.
    let unknown_panel = format!("{ARN_PREFIX}controlpanel/00000000-0000-0000-0000-000000000000");
    let cursors: &[(&str, Option<&str>, &str)] = &[
        ("unknown cursor, account-wide", None, unknown_panel.as_str()),
        (
            "unknown cursor, filtered",
            Some(cluster_arn.as_str()),
            unknown_panel.as_str(),
        ),
        (
            "cursor naming a panel of another cluster",
            Some(cluster_arn.as_str()),
            other_panel_arn.as_str(),
        ),
    ];
    for (case, scope, token) in cursors {
        let (status, _, _) = list_control_panels_page(&router, *scope, None, Some(token)).await;
        assert_eq!(status, 400, "case {case}: expected 400, got {status}");
        let mut query = format!("NextToken={}", enc(token));
        if let Some(scope) = scope {
            query = format!("ClusterArn={}&{query}", enc(scope));
        }
        let (_, body) = call(&router, "GET", &format!("/controlpanels?{query}"), None).await;
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case}: expected ValidationException, got {body:?}"
        );
    }

    // `MaxResults` is modelled `range 1..1000`: one step outside each bound is refused,
    // and each bound itself is served.
    for (case, max) in [
        ("MaxResults one below the range minimum", 0),
        ("MaxResults one above the range maximum", 1001),
    ] {
        let (status, body) = call(
            &router,
            "GET",
            &format!("/controlpanels?MaxResults={max}"),
            None,
        )
        .await;
        assert_eq!(status, 400, "case {case}: expected 400, got {status} {body:?}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case}: expected ValidationException, got {body:?}"
        );
    }
    for max in [1, 1000] {
        let (status, _, _) = list_control_panels_page(&router, None, Some(max), None).await;
        assert_eq!(
            status, 200,
            "case MaxResults={max} (a published bound): expected 200, got {status}"
        );
    }
}

// --- the members the wire carries beside the modelled ones -----------------------------

// The names of those members come from the crate itself — the shapes that carry them —
// rather than from a second copy here, so a rename cannot leave the test asserting the
// old name.
use service_com_amazonaws_route53recoverycontrolconfig_route53recoverycontrolconfig::ops::types::{
    WIRE_ARN_MEMBER as GENERIC_ARN_MEMBER,
    WIRE_HEALTH_CHECK_SEQUENCE_MEMBER as HEALTH_CHECK_SEQUENCE_MEMBER,
};

/// One projection carries the generic ARN member, holding the resource's own ARN — the
/// value its typed member holds.
fn assert_generic_arn(case: &str, projection: &Value, typed_member: &str) {
    let typed = projection[typed_member].as_str();
    assert!(
        typed.is_some_and(|arn| arn.starts_with(ARN_PREFIX)),
        "case {case}: expected {typed_member} to be a recovery-control ARN, got {:?}",
        projection[typed_member]
    );
    let generic = projection[GENERIC_ARN_MEMBER].as_str();
    assert_eq!(
        generic, typed,
        "case {case}: expected {GENERIC_ARN_MEMBER} to equal {typed_member} ({typed:?}), got \
         {generic:?}"
    );
}

/// A routing control's health-check sequence number, as a positive integer.
fn sequence_number(case: &str, projection: &Value) -> i64 {
    let raw = &projection[HEALTH_CHECK_SEQUENCE_MEMBER];
    let number = raw.as_i64();
    assert!(
        number.is_some_and(|n| n >= 1),
        "case {case}: expected {HEALTH_CHECK_SEQUENCE_MEMBER} to be a positive number, got \
         {raw:?} in {projection:?}"
    );
    number.unwrap_or_default()
}

/// RULE: the endpoint reports a control panel's and a safety rule's ARN under the generic
/// member name as well as under the typed member (`ControlPanelArn` / `SafetyRuleArn`),
/// and the two hold the same value — in every operation that projects the resource:
/// create, describe, list and update.
///
/// Neither shape declares the generic member in the public model
/// (`smithy_query(query="shape",shape="ControlPanel")`, `shape="AssertionRule"` and
/// `shape="GatingRule"` list no `Arn`), so what is asserted here is the recorded wire
/// behaviour, over every projection and both kinds of safety rule rather than the one
/// read that exposed it.
#[tokio::test]
async fn control_panel_and_safety_rule_projections_carry_the_generic_arn_member() {
    let router = router();

    let cluster = create_cluster(&router, json!({ "ClusterName": "generic-arn-cluster" })).await;
    let cluster_arn = cluster["ClusterArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let created = create_control_panel(
        &router,
        json!({ "ClusterArn": cluster_arn, "ControlPanelName": "generic-arn-panel" }),
    )
    .await;
    assert_generic_arn("CreateControlPanel", &created, "ControlPanelArn");
    let panel_arn = created["ControlPanelArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Both panels of the cluster — the one just created and the default panel the cluster
    // was created with — through the list page and through a read of each.
    let (status, _, panels) =
        list_control_panels_page(&router, Some(&cluster_arn), None, None).await;
    assert_eq!(status, 200, "case ListControlPanels: expected 200, got {status}");
    assert_eq!(
        panels.len(),
        2,
        "case ListControlPanels: a cluster holds its default panel and the created one, got \
         {panels:?}"
    );
    for panel in &panels {
        let name = panel["Name"].as_str().unwrap_or_default().to_string();
        assert_generic_arn(&format!("ListControlPanels {name}"), panel, "ControlPanelArn");
        let arn = panel["ControlPanelArn"].as_str().unwrap_or_default();
        let (status, body) =
            call(&router, "GET", &format!("/controlpanel/{}", enc(arn)), None).await;
        assert_eq!(
            status, 200,
            "case DescribeControlPanel {name}: expected 200, got {status} {body:?}"
        );
        assert_generic_arn(
            &format!("DescribeControlPanel {name}"),
            &body["ControlPanel"],
            "ControlPanelArn",
        );
    }

    let (status, updated) = call(
        &router,
        "PUT",
        "/controlpanel",
        Some(json!({
            "ControlPanelArn": panel_arn,
            "ControlPanelName": "generic-arn-panel-renamed"
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case UpdateControlPanel: expected 200, got {status} {updated:?}"
    );
    assert_generic_arn(
        "UpdateControlPanel",
        &updated["ControlPanel"],
        "ControlPanelArn",
    );

    // Safety rules, of both kinds: created, read back, listed and updated.
    let (rules_panel, controls) = panel_with_controls(&router, "generic-arn", 2).await;
    let config = json!({ "Inverted": false, "Threshold": 1, "Type": "ATLEAST" });
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "AssertionRule",
            "ASSERTION",
            json!({
                "AssertionRule": {
                    "AssertedControls": [controls[0].clone()],
                    "ControlPanelArn": rules_panel,
                    "Name": "generic-arn-assertion",
                    "RuleConfig": config,
                    "WaitPeriodMs": 5000
                }
            }),
        ),
        (
            "GatingRule",
            "GATING",
            json!({
                "GatingRule": {
                    "ControlPanelArn": rules_panel,
                    "GatingControls": [controls[0].clone()],
                    "Name": "generic-arn-gating",
                    "RuleConfig": config,
                    "TargetControls": [controls[1].clone()],
                    "WaitPeriodMs": 5000
                }
            }),
        ),
    ];

    let mut rule_arns: Vec<(&str, &str, String)> = Vec::new();
    for (member, listed_member, body) in &rows {
        let (status, answer) = call(&router, "POST", "/safetyrule", Some(body.clone())).await;
        assert_eq!(
            status, 200,
            "case CreateSafetyRule {member}: expected 200, got {status} {answer:?}"
        );
        assert_generic_arn(
            &format!("CreateSafetyRule {member}"),
            &answer[*member],
            "SafetyRuleArn",
        );
        let arn = answer[*member]["SafetyRuleArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        // A safety rule's own id is sixteen hexadecimal characters, not the 8-4-4-4-12
        // form the other recovery-control resources carry: AWS lays the ARN out as
        // `…/controlpanel/<panel id>/safetyrule/<sixteen hexadecimal characters>`.
        let id = arn.rsplit_once('/').map_or("", |(_, id)| id);
        assert!(
            id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "case CreateSafetyRule {member}: expected a sixteen-character lowercase \
             hexadecimal rule id, got {id:?} in {arn:?}"
        );
        rule_arns.push((*member, *listed_member, arn));
    }

    for (member, _listed, arn) in &rule_arns {
        let (status, body) =
            call(&router, "GET", &format!("/safetyrule/{}", enc(arn)), None).await;
        assert_eq!(
            status, 200,
            "case DescribeSafetyRule {member}: expected 200, got {status} {body:?}"
        );
        assert_generic_arn(
            &format!("DescribeSafetyRule {member}"),
            &body[*member],
            "SafetyRuleArn",
        );

        // The member an update carries the change under is the rule's own kind plus
        // `Update`, so the key is built rather than written out per kind.
        let mut update = serde_json::Map::new();
        update.insert(
            format!("{member}Update"),
            json!({
                "Name": format!("{member}-renamed"),
                "SafetyRuleArn": arn,
                "WaitPeriodMs": 7500
            }),
        );
        let (status, updated) =
            call(&router, "PUT", "/safetyrule", Some(Value::Object(update))).await;
        assert_eq!(
            status, 200,
            "case UpdateSafetyRule {member}: expected 200, got {status} {updated:?}"
        );
        assert_generic_arn(
            &format!("UpdateSafetyRule {member}"),
            &updated[*member],
            "SafetyRuleArn",
        );
    }

    let (status, body) = call(
        &router,
        "GET",
        &format!("/controlpanel/{}/safetyrules", enc(&rules_panel)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case ListSafetyRules: expected 200, got {status} {body:?}"
    );
    let listed = body["SafetyRules"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        listed.len(),
        rule_arns.len(),
        "case ListSafetyRules: expected every created rule, got {listed:?}"
    );
    for rule in &listed {
        let (_, listed_member, _) = rule_arns
            .iter()
            .find(|(_, listed_member, _)| rule[*listed_member].is_object())
            .expect("a listed rule fills the member of its own kind");
        assert_generic_arn(
            &format!("ListSafetyRules {listed_member}"),
            &rule[*listed_member],
            "SafetyRuleArn",
        );
    }
}

/// RULE: a routing control reports the sequence number of the Route 53 health check state
/// that backs it, on every projection of the control — create, describe, list and update.
/// It is a positive number, each control carries its own, reading the control again
/// reports the same one, and it advances only when the control's configuration is
/// deployed again.
///
/// The model declares no such member (`smithy_query(query="shape",
/// shape="RoutingControl")` lists five), so what a client may rely on is that it is
/// there, that it is a number and that it is stable across reads — which is what the rows
/// below span: several controls, every operation that projects one, a rename and a
/// request that renames nothing.
#[tokio::test]
async fn a_routing_control_reports_its_health_check_sequence_number() {
    let router = router();
    let (panel_arn, controls) = panel_with_controls(&router, "sequence", 3).await;

    // Created: every control reports a number of its own, and reading one does not move
    // it.
    let mut created: Vec<(String, i64)> = Vec::new();
    for arn in &controls {
        let (status, body) =
            call(&router, "GET", &format!("/routingcontrol/{}", enc(arn)), None).await;
        assert_eq!(
            status, 200,
            "case DescribeRoutingControl {arn}: expected 200, got {status} {body:?}"
        );
        let number = sequence_number(
            &format!("DescribeRoutingControl {arn}"),
            &body["RoutingControl"],
        );
        // A read reports the stored number, so reading again reports the same one.
        let (_, again) =
            call(&router, "GET", &format!("/routingcontrol/{}", enc(arn)), None).await;
        assert_eq!(
            sequence_number("the same control read twice", &again["RoutingControl"]),
            number,
            "case {arn}: a read must not move the sequence number"
        );
        created.push((arn.clone(), number));
    }

    // The list projection carries it too, for every control of the panel.
    let (status, body) = call(
        &router,
        "GET",
        &format!("/controlpanel/{}/routingcontrols", enc(&panel_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case ListRoutingControls: expected 200, got {status} {body:?}"
    );
    let listed = body["RoutingControls"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        listed.len(),
        controls.len(),
        "case ListRoutingControls: expected every control, got {listed:?}"
    );
    for control in &listed {
        let arn = control["RoutingControlArn"].as_str().unwrap_or_default();
        let stored = created
            .iter()
            .find(|(created_arn, _)| created_arn == arn)
            .map(|(_, number)| *number)
            .unwrap_or_default();
        assert_eq!(
            sequence_number(&format!("ListRoutingControls {arn}"), control),
            stored,
            "case {arn}: the listed number is the stored one"
        );
    }

    // A rename deploys the control again, which advances the number with it; a request
    // naming the name the control already carries changes nothing, so it does not.
    let (arn, before) = created[0].clone();
    let rows: &[(&str, &str, bool)] = &[
        ("renamed", "sequence-control-renamed", true),
        ("renamed to the same name", "sequence-control-renamed", false),
    ];
    let mut expected = before;
    for (case, name, advances) in rows {
        let (status, updated) = call(
            &router,
            "PUT",
            "/routingcontrol",
            Some(json!({ "RoutingControlArn": arn, "RoutingControlName": name })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected UpdateRoutingControl 200, got {status} {updated:?}"
        );
        if *advances {
            expected += 1;
        }
        assert_eq!(
            sequence_number(case, &updated["RoutingControl"]),
            expected,
            "case {case}: expected the sequence number to {}",
            if *advances { "advance" } else { "stay" }
        );
        let (_, read) =
            call(&router, "GET", &format!("/routingcontrol/{}", enc(&arn)), None).await;
        assert_eq!(
            sequence_number(&format!("{case}, read back"), &read["RoutingControl"]),
            expected,
            "case {case}: a later read answers the stored number"
        );
    }

    // The other controls were not touched, so their numbers did not move.
    for (other_arn, number) in &created[1..] {
        let (_, read) = call(
            &router,
            "GET",
            &format!("/routingcontrol/{}", enc(other_arn)),
            None,
        )
        .await;
        assert_eq!(
            sequence_number(&format!("untouched {other_arn}"), &read["RoutingControl"]),
            *number,
            "case {other_arn}: deploying another control does not move this one's number"
        );
    }
}

// ---------------------------------------------------------------------------
// The Amazon Route 53 health checks that FOLLOW a routing control.
//
// A health check is a resource of Route 53, not of this service: a client creates one with
// `CreateHealthCheck` and makes it follow a routing control by naming that control in
// `HealthCheckConfig$RoutingControlArn`. So these suites mount Route 53 BESIDE this service
// on one shared engine and build every association through that service's own API, which is
// what a client does — the ids this service reports are then demonstrably the other
// service's, and not a list of its own.
// ---------------------------------------------------------------------------

/// The endpoint host a Route 53 request is addressed to, which is also the routing signal
/// the router resolves that service from.
const ROUTE53_ENDPOINT: &str = "route53.amazonaws.com";

/// The Route 53 API version every one of its routes is prefixed with.
const ROUTE53_PREFIX: &str = "/2013-04-01";

/// The `HealthCheckType` of a health check that follows a routing control: "RECOVERY_CONTROL
/// … the health check is associated with a Route 53 Application Recovery Controller routing
/// control" (`smithy_query(query="shape",shape="HealthCheckType")` on the Route 53 model).
const RECOVERY_CONTROL: &str = "RECOVERY_CONTROL";

/// A router with this service AND Amazon Route 53 mounted on ONE shared engine, so a health
/// check created through Route 53 is held in the store this service reads.
fn router_with_route53() -> MultiServiceRouter {
    let (mut router, engine) = super::common::router_with_engine();
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(engine),
    );
    router
}

/// Send one Route 53 request (restXml) and return `(status, body)`.
async fn route53_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("{ROUTE53_PREFIX}{path}"))
        .header(http::header::HOST, ROUTE53_ENDPOINT)
        .header(http::header::CONTENT_TYPE, "application/xml")
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The text of the first `<name>` element of an XML body.
fn xml_first(case: &str, body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let at = body
        .find(&open)
        .unwrap_or_else(|| panic!("case {case}: expected a {open} element, got {body}"))
        + open.len();
    let rest = &body[at..];
    let end = rest
        .find(&close)
        .unwrap_or_else(|| panic!("case {case}: expected a {close} element, got {body}"));
    rest[..end].to_string()
}

/// Create one Route 53 health check that follows `routing_control_arn`, and answer the id
/// Route 53 minted for it.
async fn follow_routing_control(
    router: &MultiServiceRouter,
    reference: &str,
    routing_control_arn: &str,
) -> String {
    let (status, body) = route53_call(
        router,
        "POST",
        "/healthcheck",
        &format!(
            "<CreateHealthCheckRequest \
             xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <CallerReference>{reference}</CallerReference>\
             <HealthCheckConfig><Type>{RECOVERY_CONTROL}</Type>\
             <RoutingControlArn>{routing_control_arn}</RoutingControlArn>\
             </HealthCheckConfig></CreateHealthCheckRequest>"
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "case CreateHealthCheck {reference}: expected 201, got {status} {body}"
    );
    xml_first(&format!("CreateHealthCheck {reference}"), &body, "Id")
}

/// `ListAssociatedRoute53HealthChecks` for `routing_control_arn`, with `query` appended to
/// the route; answers `(status, parsed body)`.
async fn associated(
    router: &MultiServiceRouter,
    routing_control_arn: &str,
    query: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!(
            "/routingcontrol/{}/associatedRoute53HealthChecks{query}",
            enc(routing_control_arn)
        ),
        None,
    )
    .await
}

/// The `HealthCheckIds` of a successful page.
fn health_check_ids(case: &str, body: &Value) -> Vec<String> {
    body["HealthCheckIds"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected a HealthCheckIds list, got {body:?}"))
        .iter()
        .map(|id| id.as_str().unwrap_or_default().to_string())
        .collect()
}

/// RULE: the operation reports the ids of exactly those Route 53 health checks whose
/// configuration names the routing control addressed — read from the store that service
/// holds them in, at the moment of the call — so a control nothing follows is reported with
/// the empty list and no control is reported another's checks.
///
/// The rows span what the rule generalises over: how many checks follow one control (none,
/// one, several), with every row read back through Route 53's own `GetHealthCheck` so the
/// ids reported are demonstrably that service's records and each really names the control it
/// was reported under.
#[tokio::test]
async fn associated_health_checks_are_the_route53_checks_that_follow_the_control() {
    let router = router_with_route53();
    let (_panel_arn, controls) = panel_with_controls(&router, "associated", 3).await;

    // (case, the control, how many health checks follow it)
    let rows: Vec<(&str, &str, usize)> = vec![
        ("a control nothing follows", controls[0].as_str(), 0),
        ("a control one check follows", controls[1].as_str(), 1),
        ("a control three checks follow", controls[2].as_str(), 3),
    ];

    let mut expected: Vec<(&str, &str, Vec<String>)> = Vec::new();
    for (case, control_arn, checks) in &rows {
        let mut ids = Vec::new();
        for n in 0..*checks {
            ids.push(follow_routing_control(&router, &format!("{case}-{n}"), control_arn).await);
        }
        ids.sort();
        expected.push((case, control_arn, ids));
    }

    for (case, control_arn, ids) in &expected {
        let (status, body) = associated(&router, control_arn, "").await;
        assert_eq!(
            status, 200,
            "case {case}: expected 200, got {status} {body:?}"
        );
        let reported = health_check_ids(case, &body);
        assert_eq!(
            &reported, ids,
            "case {case}: expected the ids of the checks following {control_arn}, got \
             {reported:?}"
        );
        assert!(
            body["NextToken"].is_null(),
            "case {case}: one page holds every id, so there is no NextToken, got {body:?}"
        );

        // Each reported id is a record of Route 53's, and its configuration names this
        // control: the association is the other service's, read rather than invented.
        for id in &reported {
            let (status, check) = route53_call(&router, "GET", &format!("/healthcheck/{id}"), "")
                .await;
            assert_eq!(
                status, 200,
                "case {case}: expected GetHealthCheck 200 for {id}, got {status} {check}"
            );
            let followed = xml_first(case, &check, "RoutingControlArn");
            assert_eq!(
                &followed, control_arn,
                "case {case}: health check {id} must follow the control it was reported \
                 under, got {followed:?}"
            );
        }
    }
}

/// RULE, on the page: a page carries at most `MaxResults` ids and the `NextToken` beside it
/// resumes at the next one, so walking the tokens visits every id exactly once in one
/// stable order.
///
/// The rows span the page size against the population: one id per page, a size that does
/// not divide it, the exact count, one past it, and the published maximum of `MaxResults`
/// (`smithy_query(query="operation",operation="ListAssociatedRoute53HealthChecks")` ->
/// range 1..1000).
#[tokio::test]
async fn a_page_is_max_results_long_and_the_next_token_resumes_after_it() {
    let router = router_with_route53();
    let (_panel_arn, controls) = panel_with_controls(&router, "paged", 1).await;
    let control_arn = controls[0].clone();

    let mut all: Vec<String> = Vec::new();
    for n in 0..5 {
        all.push(follow_routing_control(&router, &format!("paged-{n}"), &control_arn).await);
    }
    all.sort();

    // The whole set in one page, which is also the order every paged walk must reproduce.
    let (status, body) = associated(&router, &control_arn, "").await;
    assert_eq!(
        status, 200,
        "case the unpaged read: expected 200, got {status} {body:?}"
    );
    assert_eq!(
        health_check_ids("the unpaged read", &body),
        all,
        "case the unpaged read: expected every id of the control"
    );

    for page_size in [1_usize, 2, 5, 6, 1000] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &token {
                None => format!("?MaxResults={page_size}"),
                Some(token) => format!("?MaxResults={page_size}&NextToken={token}"),
            };
            let (status, body) = associated(&router, &control_arn, &query).await;
            assert_eq!(
                status, 200,
                "case MaxResults={page_size} page {pages}: expected 200, got {status} {body:?}"
            );
            let page = health_check_ids(&format!("MaxResults={page_size}"), &body);
            let remaining = all.len() - walked.len();
            assert_eq!(
                page.len(),
                page_size.min(remaining),
                "case MaxResults={page_size} page {pages}: expected {} ids, got {page:?}",
                page_size.min(remaining)
            );
            walked.extend(page);
            pages += 1;
            assert!(
                pages <= all.len() + 1,
                "case MaxResults={page_size}: the walk must end, got {pages} pages"
            );
            match body["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            walked, all,
            "case MaxResults={page_size}: the walk must visit every id once, in the one order"
        );
    }
}

/// RULE, on where the answer comes from: the ids are read from Route 53's store on every
/// call, so a health check created there appears and one deleted there stops being
/// reported, with no call to this service in between.
#[tokio::test]
async fn the_reported_ids_follow_route53s_own_store() {
    let router = router_with_route53();
    let (_panel_arn, controls) = panel_with_controls(&router, "tracking", 2).await;
    let (control_arn, other_arn) = (controls[0].clone(), controls[1].clone());

    // A check following the OTHER control, which must never be reported under this one.
    let other = follow_routing_control(&router, "tracking-other", &other_arn).await;

    let first = follow_routing_control(&router, "tracking-first", &control_arn).await;
    let second = follow_routing_control(&router, "tracking-second", &control_arn).await;
    let mut both = vec![first.clone(), second.clone()];
    both.sort();

    let mut expected = both.clone();
    let (status, body) = associated(&router, &control_arn, "").await;
    assert_eq!(status, 200, "case both created: got {status} {body:?}");
    assert_eq!(
        health_check_ids("both created", &body),
        expected,
        "case both created: expected both ids"
    );

    // Deleted in Route 53: the id is gone from this answer, without this service being
    // told.
    let (status, deleted) =
        route53_call(&router, "DELETE", &format!("/healthcheck/{first}"), "").await;
    assert_eq!(
        status, 200,
        "case DeleteHealthCheck {first}: expected 200, got {status} {deleted}"
    );
    expected.retain(|id| id != &first);
    let (status, body) = associated(&router, &control_arn, "").await;
    assert_eq!(status, 200, "case after the delete: got {status} {body:?}");
    assert_eq!(
        health_check_ids("after the delete", &body),
        expected,
        "case after the delete: expected only the check Route 53 still holds"
    );

    // And a third one created there appears, in the same one order.
    let third = follow_routing_control(&router, "tracking-third", &control_arn).await;
    expected.push(third);
    expected.sort();
    let (status, body) = associated(&router, &control_arn, "").await;
    assert_eq!(status, 200, "case after the third create: got {status} {body:?}");
    assert_eq!(
        health_check_ids("after the third create", &body),
        expected,
        "case after the third create: expected the checks Route 53 holds"
    );
    assert!(
        !health_check_ids("the other control's check", &body).contains(&other),
        "case the other control's check: {other} follows {other_arn} and must not be \
         reported under {control_arn}"
    );
}

/// RULE, on the refusals: the operation reads the ARN's SYNTAX first (400, naming the value
/// it could not read), then whose account it names (403), then whether this service holds a
/// routing control under it (404) — and a `NextToken` naming no id of the page set is
/// refused as input (400).
///
/// The rows span each class against several values: three ways a value can fail to be an
/// ARN, two foreign accounts, a well-formed routing-control ARN that was never minted, and
/// two tokens that name no associated check.
#[tokio::test]
async fn the_refusals_state_the_class_of_the_request() {
    let router = router_with_route53();
    let (_panel_arn, controls) = panel_with_controls(&router, "refusals", 1).await;
    let control_arn = controls[0].clone();
    follow_routing_control(&router, "refusals-check", &control_arn).await;

    // Not an ARN at all: refused as malformed input, echoing the value the request carried.
    for (case, value) in [
        ("no ARN fields at all", "not-an-arn"),
        (
            "five fields: no resource field",
            "arn:aws:route53-recovery-control::000000000000",
        ),
        ("an empty vendor", "arn:aws:::000000000000:controlpanel/abc"),
    ] {
        let (status, body) = associated(&router, value, "").await;
        let expected = format!("ARN format is invalid: {value}.");
        assert_eq!(
            (
                status,
                error_code(&body).as_str(),
                body["message"].as_str().unwrap_or_default()
            ),
            (400, "ValidationException", expected.as_str()),
            "case {case}: expected 400 ValidationException {expected:?}, got {status} {body:?}"
        );
    }

    // Another account's ARN: a denial, before anything is looked up.
    for account in ["123456789012", "999999999999"] {
        let foreign = control_arn.replace(ACCOUNT, account);
        let (status, body) = associated(&router, &foreign, "").await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (403, "AccessDeniedException"),
            "case an ARN of {account}: expected 403 AccessDeniedException, got {status} \
             {body:?}"
        );
    }

    // The caller's own, well formed, and no routing control stored under it.
    let unstored = format!(
        "{ARN_PREFIX}controlpanel/00000000-0000-0000-0000-000000000000/routingcontrol/\
         00000000-0000-0000-0000-000000000000"
    );
    let (status, body) = associated(&router, &unstored, "").await;
    let expected = format!("No record corresponding to routing control Arn({unstored})");
    assert_eq!(
        (
            status,
            error_code(&body).as_str(),
            body["message"].as_str().unwrap_or_default()
        ),
        (404, "ResourceNotFoundException", expected.as_str()),
        "case an unstored routing control: expected 404 ResourceNotFoundException \
         {expected:?}, got {status} {body:?}"
    );

    // A token that names no id of this control's page set.
    for (case, token) in [
        ("a token that is no health check id", "not-an-id"),
        (
            "a well-formed id nothing was minted for",
            "00000000-0000-0000-0000-000000000000",
        ),
    ] {
        let (status, body) = associated(&router, &control_arn, &format!("?NextToken={token}")).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case}: expected 400 ValidationException, got {status} {body:?}"
        );
    }
}
