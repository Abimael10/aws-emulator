// SPDX-License-Identifier: CC0-1.0

//! WHEN a VPC link's provisioning status is decided, and what it is decided FROM.
//!
//! RULE: **a VPC link's terminal status is decided ONCE, by the asynchronous provisioning the
//! create starts, from whether Elastic Load Balancing holds the network load balancer every
//! entry of `targetArns` names at that moment — a target it holds none for settles the link
//! `FAILED` with the service's fixed description "NLB ARN not found", every target it holds
//! settles it `AVAILABLE` — and each later read reports that settled pair unchanged, whatever
//! the account's load balancers do afterwards.** `targetArns` is "The ARN of the network load
//! balancer of the VPC targeted by the VPC link. The network load balancer must be owned by
//! the same Amazon Web Services account of the API owner" (`VpcLink$targetArns`), and
//! `VpcLink$status` is the asynchronous `AVAILABLE | PENDING | DELETING | FAILED` the create
//! answers `PENDING` out of, described by `VpcLink$statusMessage`, "A description about the
//! VPC link status". So the question provisioning puts is about the RESOURCE a target names,
//! not about the ARN's text, and reading a link afterwards neither re-asks it nor changes the
//! answer.
//!
//! Only this test crate can assert it: API Gateway and Elastic Load Balancing are separate
//! service crates, so a link's targets exist only where both are mounted on one engine — as
//! `core/router/src/lib.rs` mounts them — and every load balancer below is created and removed
//! through Elastic Load Balancing's OWN API rather than written into a store by hand. The
//! emulator, the ELBv2 calls and the `CreateVpcLink`/`GetVpcLink` helpers are this suite's
//! sibling [`crate::apigateway_vpc_link_target_resolution`], which asserts the resolution
//! itself; what is asserted here is that the outcome is SETTLED — the one observable a client
//! polling a link to readiness depends on.
//!
//! The rows differ along what the rule generalises over: how many targets a link carries,
//! which of them is unresolvable (the only one, the middle of several), and the Region the
//! target's own ARN names — the request's, or another one, which is where the load balancer
//! has to be looked for.
//!
//! The changes made to the inventory AFTER the links settle are shown to be real changes by
//! two controls: a link created over the removed load balancer's ARN settles `FAILED`, and one
//! created over the newly added load balancer settles `AVAILABLE`. Without them "the pair did
//! not move" could be a change that never happened.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_target_resolution::{
    apigw, build_router, create_network_load_balancer, delete_load_balancer, member, settled,
    unheld_target, REGION,
};

/// A Region other than the one the API Gateway calls are addressed in, for the row whose
/// target names a load balancer living somewhere else: the account owns it, so the link over it
/// is provisioned.
const ANOTHER_REGION: &str = "eu-central-1";

/// The statuses a VPC link passes through and settles in (`VpcLinkStatus`).
const PENDING: &str = "PENDING";
const AVAILABLE: &str = "AVAILABLE";
const FAILED: &str = "FAILED";

/// The description a provisioned link reports, and the one a link whose target does not
/// resolve reports. Spelled out here rather than read from the service crate: these are the
/// sentences a client's readiness gate keys on, so the suite pins the wording independently of
/// the constants the handler carries. Both are FIXED text, with no variable part.
const AVAILABLE_SENTENCE: &str = "VPC link is ready to route traffic";
const NOT_FOUND_SENTENCE: &str = "NLB ARN not found";

/// One link the rule is asserted over.
struct Row {
    case: &'static str,
    /// The `targetArns` the create carries, in request order.
    targets: Vec<String>,
    /// The status its provisioning must settle it in, and the description that goes with it.
    settles: &'static str,
    describes: &'static str,
}

/// `CreateVpcLink` over `targets`, asserting the modelled 202 and the `PENDING` a create of an
/// asynchronous resource answers out of whatever its targets turn out to be, and answering the
/// link's id.
async fn created_link(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    targets: &[String],
) -> String {
    let created = apigw(
        router,
        "POST",
        "/vpclinks",
        Some(json!({ "name": name, "targetArns": targets })),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected 202 from CreateVpcLink, got {} with {}",
        created.status, created.body
    );
    assert_eq!(
        member(case, &created.body, "status"),
        PENDING,
        "case {case:?}: expected CreateVpcLink to report {PENDING}, got {}",
        created.body
    );
    member(case, &created.body, "id")
}

/// The status/description pair a link settles in, asserted to be `settles`/`describes`.
async fn settled_pair(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    settles: &str,
    describes: &str,
) -> (String, String) {
    let read = settled(router, case, id).await;
    let status = member(case, &read, "status");
    // `statusMessage` is a member of the ANSWER, not an optional extra: a client reading a
    // link to explain its state has to find it there.
    let message = member(case, &read, "statusMessage");
    assert_eq!(
        status, settles,
        "case {case:?}: expected the link to settle {settles}, got {read}"
    );
    assert_eq!(
        message, describes,
        "case {case:?}: expected the description {describes:?}, got {message:?}"
    );
    (status, message)
}

#[tokio::test]
async fn a_settled_vpc_link_reports_the_pair_its_provisioning_decided_however_the_account_changes()
{
    let router = build_router();

    // The load balancers the rows are provisioned over, each created through Elastic Load
    // Balancing's own API: two in the Region the creates are addressed in, one in another.
    let in_region = create_network_load_balancer(&router, REGION, "settled-in-region").await;
    let alongside = create_network_load_balancer(&router, REGION, "settled-alongside").await;
    let elsewhere =
        create_network_load_balancer(&router, ANOTHER_REGION, "settled-elsewhere").await;

    let rows: Vec<Row> = vec![
        Row {
            case: "one target the account holds in the Region the create is addressed in",
            targets: vec![in_region.clone()],
            settles: AVAILABLE,
            describes: AVAILABLE_SENTENCE,
        },
        Row {
            case: "one target the account holds in another Region",
            targets: vec![elsewhere.clone()],
            settles: AVAILABLE,
            describes: AVAILABLE_SENTENCE,
        },
        Row {
            case: "one target no load balancer answers to",
            targets: vec![unheld_target(REGION, "settled-absent")],
            settles: FAILED,
            describes: NOT_FOUND_SENTENCE,
        },
        Row {
            case: "three targets, the MIDDLE of which is unheld",
            targets: vec![
                in_region.clone(),
                unheld_target(REGION, "settled-absent-middle"),
                alongside.clone(),
            ],
            settles: FAILED,
            describes: NOT_FOUND_SENTENCE,
        },
    ];

    // Every link is created first, so they are all being provisioned over the one inventory,
    // and then read to the status that provisioning settled them in.
    let mut links: Vec<(Row, String)> = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let id = created_link(
            &router,
            row.case,
            &format!("settled-link-{index}"),
            &row.targets,
        )
        .await;
        links.push((row, id));
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (row, id) in &links {
        pairs.push(settled_pair(&router, row.case, id, row.settles, row.describes).await);
    }

    // Now the account's load balancers CHANGE under every settled link: the one two links were
    // provisioned over is removed, the one another was provisioned over in its own Region is
    // removed, and a load balancer nothing targets is added.
    delete_load_balancer(&router, REGION, &in_region).await;
    delete_load_balancer(&router, ANOTHER_REGION, &elsewhere).await;
    let added = create_network_load_balancer(&router, REGION, "settled-added").await;

    // The settled pair is what every later read reports — twice over, so a read that consumed
    // or recomputed it would show — and so are the targets the link was created with.
    for round in 0..2 {
        for ((row, id), (status, message)) in links.iter().zip(&pairs) {
            let case = row.case;
            let read = apigw(&router, "GET", &format!("/vpclinks/{id}"), None).await;
            assert_eq!(
                read.status, 200,
                "case {case:?} (round {round}): expected 200 from GetVpcLink({id}), got {} with {}",
                read.status, read.body
            );
            assert_eq!(
                (
                    member(case, &read.body, "status"),
                    member(case, &read.body, "statusMessage")
                ),
                (status.clone(), message.clone()),
                "case {case:?} (round {round}): expected the settled pair \
                 ({status:?}, {message:?}), got {}",
                read.body
            );
            assert_eq!(
                read.body.get("targetArns").and_then(Value::as_array).cloned(),
                Some(
                    row.targets
                        .iter()
                        .map(|target| Value::String(target.clone()))
                        .collect::<Vec<Value>>()
                ),
                "case {case:?} (round {round}): expected the targets it was created with, got {}",
                read.body
            );
        }
    }

    // The controls: the inventory really did change, so what the reads above reported is a
    // SETTLED outcome and not a change that never happened. A link created now over the ARN
    // of the removed load balancer cannot be provisioned, and one created over the added load
    // balancer can.
    for (index, (case, targets, settles, describes)) in [
        (
            "a link created after its target was removed",
            vec![in_region.clone()],
            FAILED,
            NOT_FOUND_SENTENCE,
        ),
        (
            "a link created over a load balancer added afterwards",
            vec![added.clone()],
            AVAILABLE,
            AVAILABLE_SENTENCE,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = created_link(
            &router,
            case,
            &format!("settled-control-link-{index}"),
            &targets,
        )
        .await;
        settled_pair(&router, case, &id, settles, describes).await;
    }
}
