// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the EKS Fargate profile slice: `CreateFargateProfile`,
//! `DescribeFargateProfile` and `ListFargateProfiles`, driven over the wire through
//! the same `MultiServiceRouter` the production binary mounts.
//!
//! Four rules, each asserted over a TABLE of inputs rather than over one recorded
//! request:
//!
//! 1. a profile read back through `DescribeFargateProfile` is the record the create
//!    wrote — every member the request supplied, plus the server-generated ones —
//!    and only its clock-owed status may have moved;
//! 2. a Fargate profile declares which Pods run on Fargate through its selectors,
//!    so a request whose selectors cannot do that (none at all, more than the
//!    documented five, one without a namespace) is refused 400
//!    `InvalidParameterException` and nothing is written;
//! 3. a refusal decided from the REQUEST ALONE is answered before any gate that
//!    depends on the kind of cluster the request named: the same payload earns the
//!    parameter refusal whether the cluster is one Amazon EKS manages or one
//!    `RegisterCluster` connected, and only a request whose payload is sound is
//!    answered with the connected-cluster `ClientException`;
//! 4. `ListFargateProfiles` names exactly the profiles the store holds for the
//!    cluster the request addresses in the Region it is signed for, in profile-name
//!    order, at most `maxResults` to a page, with an opaque `nextToken` that resumes
//!    after the last name returned and is absent on the last page.
//!
//! The rows differ along what each rule generalises over — which optional members
//! are supplied, selector count and label shape, subnet-list length, tag map, the
//! region, the cluster, and which member of the payload is wrong — and every
//! assertion names its case and prints the actual value.
//!
//! The wire harness (`Answer`, `emulator`, `call`, `encode_label`) is the shared one
//! in `tests/all/wire.rs` and the cluster helpers are `cluster_lifecycle`'s: no
//! suite grows its own client.

use std::time::Duration;

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    cluster_name_refusal, cluster_not_found_message, encode_page_token, encode_scoped_page_token,
    max_results_range_message, CONNECTED_CLUSTER_REFUSAL_MESSAGE,
    FARGATE_PROFILE_ACTIVATION_DELAY, INVALID_NEXT_TOKEN_MESSAGE, MAX_FARGATE_PROFILES_PER_CLUSTER,
    MAX_LIST_MAX_RESULTS, MAX_SELECTORS_PER_FARGATE_PROFILE, MIN_LIST_MAX_RESULTS,
    NODEGROUPS_LISTING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::REGION;
use crate::wire::{call, emulator, encode_label, error_code, read_until, Answer};

/// A role of the account this emulator serves, so the pass-role authorization
/// (403, pinned by `pass_role_authorization`) is never what answers a row here.
fn pod_execution_role() -> String {
    format!(
        "arn:aws:iam::{}:role/eksFargatePodExecutionRole",
        engine::emulator_account_id()
    )
}

/// The sentence `CreateFargateProfile` refuses a `fargateProfileName` the cluster
/// already holds with: fixed text with NO variable part — it names neither the
/// profile nor the cluster, both of which are the caller's own request members.
pub(crate) const PROFILE_NAME_IN_USE: &str =
    "A Fargate Profile already exists with this name in this cluster.";

/// The `fargateProfile` document an answer carries.
fn profile(answer: &Answer) -> &serde_json::Value {
    &answer.body["fargateProfile"]
}

/// The same document with `status` dropped, which is the one member a read may
/// legitimately differ on: the clock owes the profile a `CREATING` -> `ACTIVE` move
/// between the create and a later read.
fn without_status(answer: &Answer) -> serde_json::Value {
    let mut document = profile(answer).clone();
    if let Some(object) = document.as_object_mut() {
        object.remove("status");
    }
    document
}

fn cluster_request(name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "roleArn": format!("arn:aws:iam::{}:role/eksClusterRole", engine::emulator_account_id()),
        "resourcesVpcConfig": { "subnetIds": ["subnet-aaaa1111", "subnet-bbbb2222"] },
    })
}

async fn create_cluster_in(router: &MultiServiceRouter, region: &str, name: &str) {
    let answer = call(
        router,
        region,
        "POST",
        "/clusters",
        &cluster_request(name).to_string(),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup: the cluster {name:?} in {region} must be created, got {answer:?}"
    );
}

async fn register_cluster(router: &MultiServiceRouter, name: &str) {
    let body = serde_json::json!({
        "name": name,
        "connectorConfig": {
            "roleArn": format!(
                "arn:aws:iam::{}:role/AmazonEKSConnectorRole",
                engine::emulator_account_id()
            ),
            "provider": "OTHER",
        },
    });
    let answer = call(
        router,
        REGION,
        "POST",
        "/cluster-registrations",
        &body.to_string(),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup: registering {name:?} must succeed, got {answer:?}"
    );
}

async fn create_profile_in(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    body: &serde_json::Value,
) -> Answer {
    call(
        router,
        region,
        "POST",
        &format!("/clusters/{}/fargate-profiles", encode_label(cluster)),
        &body.to_string(),
    )
    .await
}

async fn create_profile(
    router: &MultiServiceRouter,
    cluster: &str,
    body: &serde_json::Value,
) -> Answer {
    create_profile_in(router, REGION, cluster, body).await
}

async fn describe_profile_in(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    name: &str,
) -> Answer {
    call(
        router,
        region,
        "GET",
        &format!(
            "/clusters/{}/fargate-profiles/{}",
            encode_label(cluster),
            encode_label(name)
        ),
        "",
    )
    .await
}

async fn describe_profile(router: &MultiServiceRouter, cluster: &str, name: &str) -> Answer {
    describe_profile_in(router, REGION, cluster, name).await
}

/// The smallest request `CreateFargateProfile` accepts: the three required members
/// plus the one selector a profile cannot work without.
fn minimal_request(name: &str) -> serde_json::Value {
    serde_json::json!({
        "fargateProfileName": name,
        "podExecutionRoleArn": pod_execution_role(),
        "selectors": [{ "namespace": "default" }],
    })
}

// ---------------------------------------------------------------------------
// RULE 1: a Fargate profile read back through `DescribeFargateProfile` is the
// record `CreateFargateProfile` wrote — the members the request supplied plus the
// server-generated ARN, `createdAt`, health and status.
// ---------------------------------------------------------------------------

/// The table spans what the rule generalises over: how many selectors the profile
/// carries (both ends of the documented 1..5 range), whether a selector carries
/// labels, how long the subnet list is (including absent), and whether tags and an
/// idempotency token are supplied at all.
fn roundtrip_rows() -> Vec<(&'static str, serde_json::Value)> {
    let role = pod_execution_role();
    let five_selectors: Vec<serde_json::Value> = (0..MAX_SELECTORS_PER_FARGATE_PROFILE)
        .map(|index| serde_json::json!({ "namespace": format!("team-{index}") }))
        .collect();
    vec![
        (
            "only the required members and one selector",
            serde_json::json!({
                "fargateProfileName": "fp-minimal",
                "podExecutionRoleArn": role,
                "selectors": [{ "namespace": "default" }],
            }),
        ),
        (
            "a selector with labels",
            serde_json::json!({
                "fargateProfileName": "fp-labels",
                "podExecutionRoleArn": role,
                "selectors": [{
                    "namespace": "payments",
                    "labels": { "tier": "web", "env": "prod" },
                }],
            }),
        ),
        (
            "the documented ceiling of five selectors",
            serde_json::json!({
                "fargateProfileName": "fp-five-selectors",
                "podExecutionRoleArn": role,
                "selectors": five_selectors,
            }),
        ),
        (
            "one subnet",
            serde_json::json!({
                "fargateProfileName": "fp-one-subnet",
                "podExecutionRoleArn": role,
                "selectors": [{ "namespace": "default" }],
                "subnets": ["subnet-aaaa1111"],
            }),
        ),
        (
            "three subnets, tags and an idempotency token",
            serde_json::json!({
                "fargateProfileName": "fp-everything",
                "podExecutionRoleArn": role,
                "selectors": [
                    { "namespace": "default", "labels": { "app": "api" } },
                    { "namespace": "kube-system" },
                ],
                "subnets": ["subnet-aaaa1111", "subnet-bbbb2222", "subnet-cccc3333"],
                "tags": { "owner": "platform", "cost-centre": "42" },
                "clientRequestToken": "token-everything",
            }),
        ),
    ]
}

#[tokio::test]
async fn a_created_fargate_profile_reads_back_as_it_was_written() {
    let router = emulator();
    let cluster = "fp-roundtrip";
    create_cluster_in(&router, REGION, cluster).await;

    for (case, body) in roundtrip_rows() {
        let name = body["fargateProfileName"].as_str().expect("a name").to_owned();
        let created = create_profile(&router, cluster, &body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the profile must be created, got {created:?} ({})",
            created.message()
        );

        let document = profile(&created);
        assert_eq!(
            document["fargateProfileName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the answer must name the profile, got {document}"
        );
        assert_eq!(
            document["clusterName"].as_str(),
            Some(cluster),
            "case {case:?}: the answer must name the cluster, got {document}"
        );
        assert_eq!(
            document["selectors"], body["selectors"],
            "case {case:?}: the selectors must be the ones the request gave, got {}",
            document["selectors"]
        );
        assert_eq!(
            document["podExecutionRoleArn"].as_str(),
            Some(pod_execution_role().as_str()),
            "case {case:?}: the Pod execution role must be the one the request gave, got {document}"
        );
        assert_eq!(
            document["subnets"],
            body.get("subnets")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
            "case {case:?}: the subnets must be the ones the request gave, got {}",
            document["subnets"]
        );
        assert_eq!(
            document["tags"],
            body.get("tags")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            "case {case:?}: the tags must be the ones the request gave, got {}",
            document["tags"]
        );
        assert_eq!(
            document["status"].as_str(),
            Some("CREATING"),
            "case {case:?}: a new profile is CREATING, got {document}"
        );
        assert_eq!(
            document["health"],
            serde_json::json!({ "issues": [] }),
            "case {case:?}: a healthy profile reports an empty issue list, got {}",
            document["health"]
        );
        assert!(
            document["createdAt"].is_number(),
            "case {case:?}: the profile must report when it was created, got {document}"
        );
        let arn = document["fargateProfileArn"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let expected_prefix = format!(
            "arn:aws:eks:{REGION}:{}:fargateprofile/{cluster}/{name}/",
            engine::emulator_account_id()
        );
        assert!(
            arn.starts_with(&expected_prefix),
            "case {case:?}: expected an ARN under {expected_prefix:?}, got {arn:?}"
        );
        assert!(
            arn.len() > expected_prefix.len(),
            "case {case:?}: the ARN must carry the profile's own id, got {arn:?}"
        );

        // The read: the same document, status aside.
        let read = describe_profile(&router, cluster, &name).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the stored profile must read back, got {read:?}"
        );
        assert_eq!(
            without_status(&read),
            without_status(&created),
            "case {case:?}: the read must be the record the create wrote, got {:?}",
            profile(&read)
        );
    }
}

/// The clock owes a new profile the `CREATING` -> `ACTIVE` move, and nothing else
/// about the record changes with it.
#[tokio::test]
async fn a_fargate_profile_becomes_active_once_the_clock_owes_it() {
    let router = emulator();
    let cluster = "fp-activation";
    create_cluster_in(&router, REGION, cluster).await;

    let created = create_profile(&router, cluster, &minimal_request("fp-clock")).await;
    assert_eq!(
        created.status, 200,
        "the profile must be created, got {created:?}"
    );
    // The span the clock owes is never zero, so the create itself answers the
    // transient status. It is asserted on the create's OWN answer rather than on a
    // second wire call, which a loaded machine could issue after the span has already
    // passed.
    assert_eq!(
        profile(&created)["status"].as_str(),
        Some("CREATING"),
        "a caller sees the transient status on the create itself, got {created:?}"
    );

    tokio::time::sleep(FARGATE_PROFILE_ACTIVATION_DELAY + Duration::from_millis(250)).await;
    let later = describe_profile(&router, cluster, "fp-clock").await;
    assert_eq!(
        later.body["fargateProfile"]["status"].as_str(),
        Some("ACTIVE"),
        "once the span the clock owed has passed the profile is ACTIVE, got {later:?}"
    );
    assert_eq!(
        without_status(&later),
        without_status(&created),
        "the move must change nothing but the status, got {:?}",
        profile(&later)
    );
    // And it stays there: a second read does not move it again.
    let again = describe_profile(&router, cluster, "fp-clock").await;
    assert_eq!(
        profile(&again),
        profile(&later),
        "a repeated read must answer identically, got {again:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE 2: the selectors are what a Fargate profile declares, so a request that
// declares nothing usable is refused as the bad parameter it is, and nothing is
// written.
// ---------------------------------------------------------------------------

/// The rows differ along WHICH way the selectors are unusable and along their
/// count: absent, present but empty, one past the documented ceiling of five, and a
/// selector carrying no namespace at all or a blank one.
fn invalid_selector_rows() -> Vec<(&'static str, serde_json::Value)> {
    let role = pod_execution_role();
    let too_many: Vec<serde_json::Value> = (0..=MAX_SELECTORS_PER_FARGATE_PROFILE)
        .map(|index| serde_json::json!({ "namespace": format!("team-{index}") }))
        .collect();
    vec![
        (
            "no selectors member at all",
            serde_json::json!({
                "fargateProfileName": "fp-no-selectors",
                "podExecutionRoleArn": role,
            }),
        ),
        (
            "an empty selectors list",
            serde_json::json!({
                "fargateProfileName": "fp-empty-selectors",
                "podExecutionRoleArn": role,
                "selectors": [],
            }),
        ),
        (
            "one selector past the documented ceiling",
            serde_json::json!({
                "fargateProfileName": "fp-too-many-selectors",
                "podExecutionRoleArn": role,
                "selectors": too_many,
            }),
        ),
        (
            "a selector with no namespace",
            serde_json::json!({
                "fargateProfileName": "fp-no-namespace",
                "podExecutionRoleArn": role,
                "selectors": [{ "labels": { "app": "api" } }],
            }),
        ),
        (
            "a selector whose namespace is blank",
            serde_json::json!({
                "fargateProfileName": "fp-blank-namespace",
                "podExecutionRoleArn": role,
                "selectors": [{ "namespace": "   " }],
            }),
        ),
        (
            "one sound selector beside one without a namespace",
            serde_json::json!({
                "fargateProfileName": "fp-mixed-selectors",
                "podExecutionRoleArn": role,
                "selectors": [{ "namespace": "default" }, { "labels": { "app": "api" } }],
            }),
        ),
    ]
}

#[tokio::test]
async fn selectors_a_profile_cannot_declare_with_are_refused_and_nothing_is_written() {
    let router = emulator();
    let cluster = "fp-selectors";
    create_cluster_in(&router, REGION, cluster).await;

    for (case, body) in invalid_selector_rows() {
        let name = body["fargateProfileName"].as_str().expect("a name").to_owned();
        let answer = create_profile(&router, cluster, &body).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected the parameter refusal, got {answer:?}"
        );
        assert_eq!(
            error_code(&answer),
            "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {answer:?}"
        );
        assert_eq!(
            answer.body["clusterName"].as_str(),
            Some(cluster),
            "case {case:?}: the payload must name the cluster, got {answer:?}"
        );
        assert_eq!(
            answer.body["fargateProfileName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the payload must name the profile at fault, got {answer:?}"
        );
        assert!(
            !answer.message().is_empty(),
            "case {case:?}: the refusal must say what is wrong, got {answer:?}"
        );

        let read = describe_profile(&router, cluster, &name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused create must have written nothing, got {read:?}"
        );
    }

    // The CONTROL: the counts either side of the refusals are accepted, so the rule
    // is a bound and not a blanket refusal — one selector and the documented five.
    for count in [1, MAX_SELECTORS_PER_FARGATE_PROFILE] {
        let selectors: Vec<serde_json::Value> = (0..count)
            .map(|index| serde_json::json!({ "namespace": format!("ns-{index}") }))
            .collect();
        let name = format!("fp-accepted-{count}");
        let body = serde_json::json!({
            "fargateProfileName": name,
            "podExecutionRoleArn": pod_execution_role(),
            "selectors": selectors,
        });
        let answer = create_profile(&router, cluster, &body).await;
        assert_eq!(
            answer.status, 200,
            "case {count} selectors: within the documented range the create is served, got \
             {answer:?} ({})",
            answer.message()
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 3: a refusal decided from the request alone precedes every gate that
// depends on the cluster the request named — and only a sound payload reaches
// those gates.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_payload_is_judged_before_the_kind_of_cluster_it_names() {
    let router = emulator();
    let managed = "fp-order-managed";
    let connected = "fp-order-connected";
    create_cluster_in(&router, REGION, managed).await;
    register_cluster(&router, connected).await;

    for (case, body) in invalid_selector_rows() {
        // The SAME body against both kinds of cluster: the answer is the parameter
        // refusal either way, because nothing about the cluster can make these
        // selectors usable.
        for cluster in [managed, connected] {
            let answer = create_profile(&router, cluster, &body).await;
            assert_eq!(
                (answer.status, error_code(&answer).as_str()),
                (400, "InvalidParameterException"),
                "case {case:?} on {cluster}: the payload is judged before the cluster's own \
                 facts, got {answer:?}"
            );
            assert_ne!(
                answer.message(),
                CONNECTED_CLUSTER_REFUSAL_MESSAGE,
                "case {case:?} on {cluster}: a bad payload must not be reported as the \
                 connected-cluster refusal, got {answer:?}"
            );
        }
    }

    // A SOUND payload reaches the gate, and that is what the connected cluster
    // answers — so the ordering above did not cost the gate.
    let refused = create_profile(&router, connected, &minimal_request("fp-order-sound")).await;
    assert_eq!(
        (refused.status, error_code(&refused).as_str()),
        (400, "ClientException"),
        "a sound payload on a connected cluster earns the Amazon EKS-managed-plane refusal, \
         got {refused:?}"
    );
    assert_eq!(
        refused.message(),
        CONNECTED_CLUSTER_REFUSAL_MESSAGE,
        "expected the connected-cluster refusal sentence, got {:?}",
        refused.message()
    );
    // And the same payload on the cluster Amazon EKS manages is created.
    let created = create_profile(&router, managed, &minimal_request("fp-order-sound")).await;
    assert_eq!(
        created.status, 200,
        "the same payload on a managed cluster is created, got {created:?}"
    );

    // A cluster this region does not hold is still the 404 that names only the
    // cluster, whatever the payload says.
    for (case, body) in invalid_selector_rows()
        .into_iter()
        .chain(std::iter::once(("a sound payload", minimal_request("fp-ghost"))))
    {
        let answer = create_profile(&router, "fp-no-such-cluster", &body).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: an unresolvable cluster is reported before the body, got {answer:?}"
        );
        assert_eq!(
            answer.body["clusterName"].as_str(),
            Some("fp-no-such-cluster"),
            "case {case:?}: the payload must name the cluster that was not found, got {answer:?}"
        );
        assert_eq!(
            answer.body["fargateProfileName"],
            serde_json::Value::Null,
            "case {case:?}: the not-found payload must not echo the profile name, got {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A profile name is unique WITHIN its cluster, and an idempotent retry of the
// create that made it is absorbed rather than conflicting.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_profile_name_is_unique_within_its_cluster() {
    let router = emulator();
    let other_region = "eu-west-1";
    let first = "fp-unique-first";
    let second = "fp-unique-second";
    create_cluster_in(&router, REGION, first).await;
    create_cluster_in(&router, REGION, second).await;
    create_cluster_in(&router, other_region, first).await;

    let name = "fp-shared-name";
    let created = create_profile(&router, first, &minimal_request(name)).await;
    assert_eq!(
        created.status, 200,
        "the first create must be served, got {created:?}"
    );

    // The same name in the same cluster: refused as the conflict it is, and the
    // stored profile is untouched. The sentence is this operation's own fixed text,
    // which names neither the profile nor the cluster.
    let conflict = create_profile(&router, first, &minimal_request(name)).await;
    assert_eq!(
        (conflict.status, error_code(&conflict).as_str()),
        (409, "ResourceInUseException"),
        "a second create under a name the cluster holds is refused, got {conflict:?}"
    );
    assert_eq!(
        conflict.message(),
        PROFILE_NAME_IN_USE,
        "the refusal must state the taken-name sentence, got {:?}",
        conflict.message()
    );

    // The SAME name is a different profile in another cluster, and in another region.
    let sibling = create_profile(&router, second, &minimal_request(name)).await;
    assert_eq!(
        sibling.status, 200,
        "the same name in another cluster is a different profile, got {sibling:?}"
    );
    let across_region = create_profile_in(&router, other_region, first, &minimal_request(name)).await;
    assert_eq!(
        across_region.status, 200,
        "the same name in another region is a different profile, got {across_region:?}"
    );
    assert_ne!(
        profile(&across_region)["fargateProfileArn"],
        profile(&created)["fargateProfileArn"],
        "two profiles of the same name in different regions are different resources, got {:?}",
        profile(&across_region)
    );
    // Each region reads back its own.
    let read_here = describe_profile(&router, first, name).await;
    let read_there = describe_profile_in(&router, other_region, first, name).await;
    assert_eq!(
        (read_here.status, read_there.status),
        (200, 200),
        "both profiles must read back, got {read_here:?} and {read_there:?}"
    );
    assert_eq!(
        profile(&read_here)["fargateProfileArn"],
        profile(&created)["fargateProfileArn"],
        "the read in {REGION} must answer the profile created there, got {read_here:?}"
    );

    // An idempotent retry — the same `clientRequestToken` — is absorbed and answered
    // with the profile that call created.
    let body = serde_json::json!({
        "fargateProfileName": "fp-idempotent",
        "podExecutionRoleArn": pod_execution_role(),
        "selectors": [{ "namespace": "default" }],
        "clientRequestToken": "retry-token-1",
    });
    let once = create_profile(&router, first, &body).await;
    assert_eq!(once.status, 200, "the create must be served, got {once:?}");
    let retried = create_profile(&router, first, &body).await;
    assert_eq!(
        retried.status, 200,
        "a retry carrying the same idempotency token is absorbed, got {retried:?}"
    );
    assert_eq!(
        without_status(&retried),
        without_status(&once),
        "the retry must answer the profile the first call created, got {:?}",
        profile(&retried)
    );

    // A DIFFERENT token under the same name is a new request, so it conflicts.
    let mut other_token = body.clone();
    other_token["clientRequestToken"] = serde_json::json!("retry-token-2");
    let conflict = create_profile(&router, first, &other_token).await;
    assert_eq!(
        (conflict.status, error_code(&conflict).as_str()),
        (409, "ResourceInUseException"),
        "a different token under a name the cluster holds still conflicts, got {conflict:?}"
    );
}

// ---------------------------------------------------------------------------
// A Fargate profile is a child of its cluster: the quota is per cluster, the
// cluster cannot be deleted while a profile is attached to it, and once they are
// gone a cluster created again under the same name inherits none of them.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn profiles_are_bounded_per_cluster_and_go_with_the_cluster() {
    let router = emulator();
    let cluster = "fp-quota";
    create_cluster_in(&router, REGION, cluster).await;

    for index in 0..MAX_FARGATE_PROFILES_PER_CLUSTER {
        let answer = create_profile(&router, cluster, &minimal_request(&format!("fp-{index}"))).await;
        assert_eq!(
            answer.status, 200,
            "profile {index} is within the quota, got {answer:?} ({})",
            answer.message()
        );
    }
    let past_quota = create_profile(&router, cluster, &minimal_request("fp-past-quota")).await;
    assert_eq!(
        (past_quota.status, error_code(&past_quota).as_str()),
        (400, "ResourceLimitExceededException"),
        "the {MAX_FARGATE_PROFILES_PER_CLUSTER}-profile quota must be reported as a limit, got \
         {past_quota:?}"
    );

    // Another cluster has its own quota, so the bound is per cluster.
    let neighbour = "fp-quota-neighbour";
    create_cluster_in(&router, REGION, neighbour).await;
    let answer = create_profile(&router, neighbour, &minimal_request("fp-0")).await;
    assert_eq!(
        answer.status, 200,
        "the quota is per cluster, so a sibling cluster still accepts a profile, got {answer:?}"
    );

    let delete_cluster = || async {
        call(
            &router,
            REGION,
            "DELETE",
            &format!("/clusters/{}", encode_label(cluster)),
            "",
        )
        .await
    };

    // The cluster keeps every profile attached to it, and those profiles keep the
    // cluster: "If you have managed node groups or Fargate profiles attached to the
    // cluster, you must delete them first".
    let refused = delete_cluster().await;
    assert_eq!(
        (refused.status, error_code(&refused).as_str()),
        (409, "ResourceInUseException"),
        "a cluster with profiles attached must not delete, got {refused:?} ({})",
        refused.message()
    );
    let still_there = describe_profile(&router, cluster, "fp-0").await;
    assert_eq!(
        still_there.status, 200,
        "a refused delete leaves the cluster and its profiles alone, got {still_there:?}"
    );

    // Deleted first, the profiles no longer hold the cluster — and a cluster created
    // again under the same name inherits none of them.
    for index in 0..MAX_FARGATE_PROFILES_PER_CLUSTER {
        let removed = call(
            &router,
            REGION,
            "DELETE",
            &format!(
                "/clusters/{}/fargate-profiles/{}",
                encode_label(cluster),
                encode_label(&format!("fp-{index}"))
            ),
            "",
        )
        .await;
        assert_eq!(
            removed.status, 200,
            "setup: profile fp-{index} must delete, got {removed:?} ({})",
            removed.message()
        );
    }
    let deleted = delete_cluster().await;
    assert_eq!(
        deleted.status, 200,
        "setup: the cluster must delete once nothing is attached, got {deleted:?} ({})",
        deleted.message()
    );
    // The accepted deletion runs for a window in which the cluster still holds its
    // name, so the name is created again once the deletion has FINISHED — waited for
    // by looking again ([`crate::wire::read_until`]).
    let cluster_uri = format!("/clusters/{}", encode_label(cluster));
    read_until(
        "the cluster is created again",
        "the deleted cluster became absent",
        || call(&router, REGION, "GET", &cluster_uri, ""),
        |read| read.status == 404,
    )
    .await;
    create_cluster_in(&router, REGION, cluster).await;
    let read = describe_profile(&router, cluster, "fp-0").await;
    assert_eq!(
        read.status, 404,
        "a cluster created again under the same name inherits no profile, got {read:?}"
    );
    let recreated = create_profile(&router, cluster, &minimal_request("fp-0")).await;
    assert_eq!(
        recreated.status, 200,
        "and the name is free again, got {recreated:?} ({})",
        recreated.message()
    );
}

// ---------------------------------------------------------------------------
// RULE 4: `ListFargateProfiles` names exactly the profiles the store holds for the
// cluster the request addresses in the Region it is signed for, in profile-name
// order, at most `maxResults` to a page, with an opaque `nextToken` that resumes
// after the last name returned and is absent on the last page.
// ---------------------------------------------------------------------------

async fn list_profiles_in(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    query: &str,
) -> Answer {
    let collection = format!("/clusters/{}/fargate-profiles", encode_label(cluster));
    let uri = if query.is_empty() {
        collection
    } else {
        format!("{collection}?{query}")
    };
    call(router, region, "GET", &uri, "").await
}

async fn list_profiles(router: &MultiServiceRouter, cluster: &str, query: &str) -> Answer {
    list_profiles_in(router, REGION, cluster, query).await
}

/// The profile names one answer carries, as a client reads them.
fn listed_names(answer: &Answer) -> Vec<String> {
    answer.body["fargateProfileNames"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The `nextToken` one answer carries, or `None` on a last page.
fn page_token(answer: &Answer) -> Option<String> {
    answer.body["nextToken"].as_str().map(str::to_owned)
}

/// Create `names` as profiles of `cluster` in `region`, in the order given — which
/// is deliberately NOT the order the listing must answer them in.
async fn create_profiles_in(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    names: &[&str],
) {
    for name in names {
        let answer = create_profile_in(router, region, cluster, &minimal_request(name)).await;
        assert_eq!(
            answer.status, 200,
            "setup: the profile {name:?} of {cluster:?} in {region} must be created, got \
             {answer:?} ({})",
            answer.message()
        );
    }
}

/// [`create_profiles_in`] in the Region the rest of this suite works in.
async fn create_profiles(router: &MultiServiceRouter, cluster: &str, names: &[&str]) {
    create_profiles_in(router, REGION, cluster, names).await
}

/// Walk EVERY page of the listing of `cluster` at `page_size` (absent for the
/// default page), answering the names in the order the pages gave them.
///
/// The walk follows the token the previous page returned, which is the only thing a
/// client has to resume from, and stops on the page that returns none.
async fn walk_pages(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    page_size: Option<i64>,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("maxResults={size}"));
        }
        if let Some(carried) = &token {
            query.push(format!("nextToken={carried}"));
        }
        let answer = list_profiles(router, cluster, &query.join("&")).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: every page must be served, got {answer:?} ({})",
            answer.message()
        );
        let names = listed_names(&answer);
        if let Some(size) = page_size {
            assert!(
                names.len() <= size as usize,
                "case {case:?}: a page holds at most {size} names, got {names:?}"
            );
        }
        let next = page_token(&answer);
        assert!(
            next.is_none() || !names.is_empty(),
            "case {case:?}: a page that resumes must have returned something, got {answer:?}"
        );
        seen.extend(names);
        // A page count bounded by the store: the cluster cannot hold more than its
        // quota, so a walk that asks for more pages than that is not terminating.
        assert!(
            seen.len() <= MAX_FARGATE_PROFILES_PER_CLUSTER,
            "case {case:?}: the walk must terminate within the cluster's quota, got {seen:?}"
        );
        match next {
            Some(carried) => token = Some(carried),
            None => return seen,
        }
    }
}

/// The table spans what the rule generalises over: how many profiles a cluster
/// holds (none, one, several), which cluster of the Region holds them, and which
/// Region holds a cluster of the SAME name.
#[tokio::test]
async fn the_listing_names_the_profiles_of_its_own_cluster_and_region_in_order() {
    let router = emulator();
    let other_region = "eu-west-1";
    let cluster = "fp-listing";
    let neighbour = "fp-listing-neighbour";
    create_cluster_in(&router, REGION, cluster).await;
    create_cluster_in(&router, REGION, neighbour).await;
    create_cluster_in(&router, other_region, cluster).await;

    // A cluster that holds none: an empty listing, not a refusal and not a token.
    for (case, region, named) in [
        ("the addressed cluster", REGION, cluster),
        ("a sibling cluster of the Region", REGION, neighbour),
        ("the same name in another Region", other_region, cluster),
    ] {
        let answer = list_profiles_in(&router, region, named, "").await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a cluster with no profile still has a listing, got {answer:?}"
        );
        assert_eq!(
            listed_names(&answer),
            Vec::<String>::new(),
            "case {case:?}: expected no names, got {answer:?}"
        );
        assert_eq!(
            page_token(&answer),
            None,
            "case {case:?}: nothing is left to resume from, got {answer:?}"
        );
    }

    // Created in an order that is NOT the order the listing answers them in, so the
    // ordering asserted below is the listing's and not the creates'.
    create_profiles(&router, cluster, &["zeta", "alpha", "mu", "beta"]).await;
    create_profiles(&router, neighbour, &["neighbour-only"]).await;
    create_profiles_in(&router, other_region, cluster, &["other-region-only"]).await;

    for (case, region, named, expected) in [
        (
            "the addressed cluster",
            REGION,
            cluster,
            vec!["alpha", "beta", "mu", "zeta"],
        ),
        (
            "a sibling cluster of the Region",
            REGION,
            neighbour,
            vec!["neighbour-only"],
        ),
        (
            "the same name in another Region",
            other_region,
            cluster,
            vec!["other-region-only"],
        ),
    ] {
        let answer = list_profiles_in(&router, region, named, "").await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the listing must be served, got {answer:?}"
        );
        assert_eq!(
            listed_names(&answer),
            expected,
            "case {case:?}: expected exactly {expected:?} in profile-name order, got {answer:?}"
        );
        assert_eq!(
            page_token(&answer),
            None,
            "case {case:?}: a whole page leaves nothing to resume from, got {answer:?}"
        );
    }

    // Every name the listing answers reads back as a profile of THAT cluster, so the
    // listing is the store's and not a list of labels.
    for name in listed_names(&list_profiles(&router, cluster, "").await) {
        let read = describe_profile(&router, cluster, &name).await;
        assert_eq!(
            read.status, 200,
            "case {name:?}: a listed name must read back, got {read:?}"
        );
        assert_eq!(
            profile(&read)["clusterName"].as_str(),
            Some(cluster),
            "case {name:?}: the listed profile belongs to the cluster listed, got {read:?}"
        );
    }
}

/// The table spans the page sizes the rule generalises over: the default (no
/// `maxResults` at all), one, a size that divides the store, one that does not, the
/// exact count, more than the count, and both published bounds of the member.
#[tokio::test]
async fn a_page_holds_at_most_max_results_and_the_token_resumes_after_it() {
    let router = emulator();
    let cluster = "fp-paging";
    create_cluster_in(&router, REGION, cluster).await;
    let stored: Vec<String> = (0..7).map(|index| format!("fp-page-{index}")).collect();
    let created: Vec<&str> = stored.iter().rev().map(String::as_str).collect();
    create_profiles(&router, cluster, &created).await;

    for page_size in [
        None,
        Some(1),
        Some(2),
        Some(3),
        Some(7),
        Some(8),
        Some(MIN_LIST_MAX_RESULTS),
        Some(MAX_LIST_MAX_RESULTS),
    ] {
        let case = match page_size {
            Some(size) => format!("maxResults={size}"),
            None => "no maxResults".to_string(),
        };
        let seen = walk_pages(&router, &case, cluster, page_size).await;
        assert_eq!(
            seen, stored,
            "case {case:?}: paging must yield every profile name once, in the listing's \
             order; got {seen:?}"
        );
    }

    // One past each published bound is refused with the page-size sentence, which
    // names no resource — the page the caller asked for is wrong on its own.
    for size in [MIN_LIST_MAX_RESULTS - 1, MAX_LIST_MAX_RESULTS + 1] {
        let case = format!("maxResults={size}");
        let answer = list_profiles(&router, cluster, &case).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, "InvalidParameterException"),
            "case {case:?}: a page size outside its bounds is refused, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            max_results_range_message(),
            "case {case:?}: expected the page-size sentence, got {:?}",
            answer.message()
        );
    }
}

/// The listing follows the STORE: a profile appears when it is created, is still
/// named while the removal its delete accepted is running, and is gone once the
/// clock has finished it.
#[tokio::test]
async fn the_listing_follows_the_store_as_profiles_come_and_go() {
    let router = emulator();
    let cluster = "fp-listing-lifecycle";
    create_cluster_in(&router, REGION, cluster).await;
    create_profiles(&router, cluster, &["fp-keep", "fp-go", "fp-stay"]).await;

    let listed = list_profiles(&router, cluster, "").await;
    assert_eq!(
        listed_names(&listed),
        vec!["fp-go", "fp-keep", "fp-stay"],
        "every created profile is named, got {listed:?}"
    );

    let deleted = call(
        &router,
        REGION,
        "DELETE",
        &format!(
            "/clusters/{}/fargate-profiles/{}",
            encode_label(cluster),
            encode_label("fp-go")
        ),
        "",
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "the delete must be accepted, got {deleted:?} ({})",
        deleted.message()
    );
    assert_eq!(
        profile(&deleted)["status"].as_str(),
        Some("DELETING"),
        "the delete answers the profile on its way out, got {deleted:?}"
    );
    // WHILE THE REMOVAL RUNS the profile is still a profile of its cluster, so the
    // listing still names it.
    let during = list_profiles(&router, cluster, "").await;
    assert!(
        listed_names(&during).contains(&"fp-go".to_string()),
        "a profile whose removal is still running is still named, got {during:?}"
    );

    // ONCE THE CLOCK HAS FINISHED IT the listing drops it — waited for by LOOKING
    // AGAIN, never by waiting out the span.
    let after = read_until(
        "a finished removal",
        "the listing drops the removed profile",
        || list_profiles(&router, cluster, ""),
        |answer| !listed_names(answer).contains(&"fp-go".to_string()),
    )
    .await;
    assert_eq!(
        listed_names(&after),
        vec!["fp-keep", "fp-stay"],
        "the other profiles are untouched by the removal, got {after:?}"
    );
}

/// A `nextToken` this listing did not issue is a bad parameter, not an empty page.
/// The rows differ along HOW the token is foreign: outside this encoding's
/// alphabet, an odd length, a well-formed encoding carrying no provenance tag, and
/// the two a scope-less check would wave through — tokens other listings of this
/// same service mint.
#[tokio::test]
async fn a_next_token_this_listing_did_not_issue_is_refused() {
    let router = emulator();
    let cluster = "fp-tokens";
    create_cluster_in(&router, REGION, cluster).await;
    create_profiles(&router, cluster, &["fp-token-a", "fp-token-b"]).await;

    let untagged: String = "fp-token-a".bytes().map(|byte| format!("{byte:02x}")).collect();
    let clusters_listing = encode_page_token(cluster);
    let nodegroups_listing = encode_scoped_page_token(NODEGROUPS_LISTING, "fp-token-a");
    for case in [
        "not-hex",
        "zz",
        "abc",
        "ffffffffff",
        untagged.as_str(),
        clusters_listing.as_str(),
        nodegroups_listing.as_str(),
    ] {
        let answer = list_profiles(&router, cluster, &format!("nextToken={case}")).await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, "InvalidParameterException"),
            "case {case:?}: a foreign nextToken is refused, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            INVALID_NEXT_TOKEN_MESSAGE,
            "case {case:?}: expected this plane's page-token sentence, got {:?}",
            answer.message()
        );
        assert_eq!(
            answer.body["clusterName"].as_str(),
            Some(cluster),
            "case {case:?}: the refusal names the cluster the listing was addressed to, \
             got {answer:?}"
        );
    }

    // The CONTROL: the token THIS listing issued is honoured, so the rows above are
    // the provenance check and not a blanket refusal.
    let first = list_profiles(&router, cluster, "maxResults=1").await;
    let token = page_token(&first).expect("a first page of two leaves a token");
    let second = list_profiles(&router, cluster, &format!("maxResults=1&nextToken={token}")).await;
    assert_eq!(
        (second.status, listed_names(&second)),
        (200, vec!["fp-token-b".to_string()]),
        "the token this listing issued resumes after the name it was minted from, got \
         {second:?}"
    );
}

/// The listing resolves the cluster it is addressed to exactly as the read of one
/// profile does: a label no cluster of this API could carry is the bad parameter it
/// is, a cluster this Region does not hold is the 404 that names it, and a cluster
/// `RegisterCluster` connected has no Amazon EKS-managed Fargate plane to list.
#[tokio::test]
async fn the_listing_resolves_the_cluster_before_it_lists_anything() {
    let router = emulator();
    let other_region = "eu-west-1";
    let managed = "fp-listing-managed";
    let connected = "fp-listing-connected";
    let elsewhere = "fp-listing-elsewhere";
    create_cluster_in(&router, REGION, managed).await;
    register_cluster(&router, connected).await;
    create_cluster_in(&router, other_region, elsewhere).await;

    for (case, label) in [
        ("a cluster ARN where a bare name belongs", {
            format!(
                "arn:aws:eks:{REGION}:{}:cluster/{managed}",
                engine::emulator_account_id()
            )
        }),
        ("a space and a bang", "sim2real probe bad!".to_string()),
        ("a leading dash", "-leading-dash".to_string()),
        ("one character past the maximum", "c".repeat(101)),
    ] {
        let answer = list_profiles(&router, &label, "").await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (400, "InvalidParameterException"),
            "case {case:?}: a label no cluster could carry is refused, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            cluster_name_refusal("clusterName", &label),
            "case {case:?}: expected the cluster-name sentence, got {:?}",
            answer.message()
        );
    }

    for (case, region, named) in [
        ("a name no cluster of the Region carries", REGION, "fp-ghost"),
        ("a cluster only another Region holds", REGION, elsewhere),
    ] {
        let answer = list_profiles_in(&router, region, named, "").await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: an unresolvable cluster has no listing, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            cluster_not_found_message(named),
            "case {case:?}: expected the cluster-not-found sentence, got {:?}",
            answer.message()
        );
        assert_eq!(
            answer.body["clusterName"].as_str(),
            Some(named),
            "case {case:?}: the refusal names the cluster that was not found, got {answer:?}"
        );
    }

    let refused = list_profiles(&router, connected, "").await;
    assert_eq!(
        (refused.status, error_code(&refused).as_str()),
        (400, "ClientException"),
        "a connected cluster has no Amazon EKS-managed Fargate plane to list, got {refused:?}"
    );
    assert_eq!(
        refused.message(),
        CONNECTED_CLUSTER_REFUSAL_MESSAGE,
        "expected the connected-cluster refusal sentence, got {:?}",
        refused.message()
    );

    // The CONTROL: the cluster Amazon EKS manages is listed.
    let served = list_profiles(&router, managed, "").await;
    assert_eq!(
        served.status, 200,
        "the managed cluster is listed, got {served:?}"
    );
}
