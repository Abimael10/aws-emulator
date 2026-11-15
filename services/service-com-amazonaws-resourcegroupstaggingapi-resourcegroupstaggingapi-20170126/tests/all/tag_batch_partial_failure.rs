// SPDX-License-Identifier: CC0-1.0

//! The BATCH contract of `TagResources` and `UntagResources`: both answer HTTP 200 and
//! report the outcome PER RESOURCE: a request mixing ARNs an owning service resolves with
//! ARNs it does not names exactly the unresolvable ones in `FailedResourcesMap` — each
//! carrying that service's own `ErrorCode`, `ErrorMessage` and `StatusCode` — while every
//! resolvable ARN of the SAME request is written.
//!
//! The sibling suite [`crate::elb_tagged_resource_index`] spans the resource KINDS an
//! Elastic Load Balancing ARN can name; this one spans the shape of the BATCH — how many
//! ARNs it carries, how many of them resolve, and where in the list those sit — because
//! the selection a caller reads out of `FailedResourcesMap` is keyed by ARN, not by
//! position, and an entry that is absent from the map is a claim that the write landed.
//!
//! The harness (one engine carrying this API and Elastic Load Balancing, the awsJson and
//! awsQuery wires, the owning service's own tag reader) is that suite's, reused rather
//! than copied.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::elb_tagged_resource_index::{
    action, elb, elb_arn, elb_ok, elb_tags, emulator, expected, p, page_arns, page_tags,
    require, resources_for, tag_fields, tagging,
};

/// The most ARNs one call may carry: `ResourceARNList` of `TagResources` and
/// `UntagResources` targets `ResourceARNListForTagUntag`, `@length(1, 20)`
/// (`smithy_query(query="operation", operation="TagResources,UntagResources")`).
const MAX_ARNS_PER_CALL: usize = 20;

/// The tag every batch below writes, and whose key the untag batch removes.
const WRITTEN: (&str, &str) = ("CostCentre", "cc-42");

/// The tag each live resource is CREATED with, so a batch write is shown to merge into
/// the set the owning service already held rather than to replace it.
const BASELINE: (&str, &str) = ("estate", "live");

/// What one slot of a batch's `ResourceARNList` holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A target group this emulator really created.
    Live,
    /// A well-formed target group ARN no target group answers to.
    GhostTargetGroup,
    /// A well-formed load balancer ARN no load balancer answers to — a second kind, so
    /// the failure each slot earns is shown to come from the ARN rather than from the
    /// batch.
    GhostLoadBalancer,
}

impl Slot {
    /// The wire error code the owning service refuses this slot's ARN with, or `None`
    /// when the slot resolves and therefore earns no entry in `FailedResourcesMap`.
    fn error_code(self) -> Option<&'static str> {
        match self {
            Slot::Live => None,
            Slot::GhostTargetGroup => Some("TargetGroupNotFound"),
            Slot::GhostLoadBalancer => Some("LoadBalancerNotFound"),
        }
    }
}

/// One batch, as the caller writes it: the ARNs of the slots in the order the request
/// carries them, beside the slot each ARN came from.
struct Batch {
    arns: Vec<String>,
    slots: Vec<Slot>,
}

impl Batch {
    /// The ARNs of every slot that resolves.
    fn live(&self) -> Vec<&String> {
        self.pick(|slot| slot == Slot::Live)
    }

    /// The ARNs of every slot that does not.
    fn ghosts(&self) -> Vec<&String> {
        self.pick(|slot| slot != Slot::Live)
    }

    fn pick(&self, keep: impl Fn(Slot) -> bool) -> Vec<&String> {
        self.slots
            .iter()
            .zip(self.arns.iter())
            .filter(|(slot, _)| keep(**slot))
            .map(|(_, arn)| arn)
            .collect()
    }

    /// The failure each slot earns, keyed by ARN: the whole `FailedResourcesMap` this
    /// batch must answer with, so an ARN missing from it and an extra ARN in it are both
    /// caught.
    fn expected_codes(&self) -> BTreeMap<&String, &'static str> {
        self.slots
            .iter()
            .zip(self.arns.iter())
            .filter_map(|(slot, arn)| slot.error_code().map(|code| (arn, code)))
            .collect()
    }
}

/// Create one target group per [`Slot::Live`] and fabricate an ARN of the right kind for
/// every other slot, in the order the slots are given.
///
/// Each live target group carries [`BASELINE`] from its create; each ghost ARN is
/// well-formed in this endpoint's partition, Region and account (so the call is not
/// refused at request level) and names a resource no create ever minted.
async fn batch(router: &smithy_server_runtime::MultiServiceRouter, slots: &[Slot]) -> Batch {
    let mut arns = Vec::with_capacity(slots.len());
    for (index, slot) in slots.iter().enumerate() {
        arns.push(match slot {
            Slot::Live => {
                let mut form = action("CreateTargetGroup");
                form.push(p("Name", &format!("batch-live-{index}")));
                form.push(p("Protocol", "HTTP"));
                form.push(p("Port", "80"));
                form.push(p("VpcId", "vpc-12345678"));
                form.extend(tag_fields(&[BASELINE]));
                require(&elb_ok(router, &form).await, "TargetGroupArn")
            }
            Slot::GhostTargetGroup => {
                elb_arn(&format!("targetgroup/batch-ghost-{index}/0123456789abcdef"))
            }
            Slot::GhostLoadBalancer => {
                elb_arn(&format!("loadbalancer/app/batch-ghost-{index}/0123456789abcdef"))
            }
        });
    }
    Batch {
        arns,
        slots: slots.to_vec(),
    }
}

/// The `ErrorCode` of every entry of a `FailedResourcesMap`, keyed by ARN.
fn failed_codes(body: &Value) -> BTreeMap<String, String> {
    body["FailedResourcesMap"]
        .as_object()
        .unwrap_or_else(|| panic!("a body with no FailedResourcesMap object: {body}"))
        .iter()
        .map(|(arn, failure)| {
            (
                arn.clone(),
                failure["ErrorCode"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a failure with no ErrorCode: {failure}"))
                    .to_string(),
            )
        })
        .collect()
}

/// The batch shapes every test below runs: the rows differ in how many ARNs the call
/// carries, how many of them resolve, WHERE the unresolvable ones sit, and which kind
/// they name. The recorded instance — one absent target group beside one live one — is
/// the first row.
fn shapes() -> Vec<(String, Vec<Slot>)> {
    let mut shapes: Vec<(String, Vec<Slot>)> = vec![
        (
            "the absent ARN first, the live one last".to_string(),
            vec![Slot::GhostTargetGroup, Slot::Live],
        ),
        (
            "the live ARN first, the absent one last".to_string(),
            vec![Slot::Live, Slot::GhostTargetGroup],
        ),
        (
            "an absent ARN between two live ones".to_string(),
            vec![Slot::Live, Slot::GhostTargetGroup, Slot::Live],
        ),
        (
            "two absent ARNs of different kinds among three live ones".to_string(),
            vec![
                Slot::Live,
                Slot::GhostTargetGroup,
                Slot::Live,
                Slot::GhostLoadBalancer,
                Slot::Live,
            ],
        ),
        (
            "every ARN of the batch resolves".to_string(),
            vec![Slot::Live, Slot::Live],
        ),
        (
            "no ARN of the batch resolves".to_string(),
            vec![Slot::GhostTargetGroup, Slot::GhostLoadBalancer],
        ),
        (
            "a single absent ARN, the shortest list the model allows".to_string(),
            vec![Slot::GhostTargetGroup],
        ),
    ];

    // The longest list the model allows, alternating, so the selection is read out of a
    // full batch rather than out of a pair.
    let full: Vec<Slot> = (0..MAX_ARNS_PER_CALL)
        .map(|index| {
            if index % 2 == 0 {
                Slot::Live
            } else {
                Slot::GhostTargetGroup
            }
        })
        .collect();
    shapes.push((
        format!("the longest list the model allows ({MAX_ARNS_PER_CALL} ARNs), half of them absent"),
        full,
    ));
    shapes
}

/// RULE: `TagResources` reports its outcome per resource — HTTP 200 with exactly the
/// ARNs the owning service could not resolve in `FailedResourcesMap`, each under that
/// service's own error code and `StatusCode`, and the tags applied to every ARN of the
/// same request that did resolve.
#[tokio::test]
async fn tag_resources_fails_exactly_the_unresolvable_arns_of_a_batch() {
    for (case, slots) in shapes() {
        // A router per row, so one row's writes are not another row's starting point.
        let router = emulator();
        let batch = batch(&router, &slots).await;

        let (status, body) = tagging(
            &router,
            "TagResources",
            json!({
                "ResourceARNList": &batch.arns,
                "Tags": { WRITTEN.0: WRITTEN.1 },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-resource failure is still HTTP 200, got {status} with \
             body {body}"
        );

        let want: BTreeMap<String, String> = batch
            .expected_codes()
            .into_iter()
            .map(|(arn, code)| (arn.clone(), code.to_string()))
            .collect();
        let got = failed_codes(&body);
        assert_eq!(
            got, want,
            "case {case:?}: expected FailedResourcesMap to name {want:?}, got {got:?}"
        );
        for arn in batch.ghosts() {
            let failure = &body["FailedResourcesMap"][arn.as_str()];
            assert_eq!(
                failure["StatusCode"],
                json!(400),
                "case {case:?}: expected StatusCode 400 for {arn}, got {failure}"
            );
            assert!(
                failure["ErrorMessage"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {case:?}: expected a message from the owning service for {arn}, got \
                 {failure}"
            );
        }

        // The write landed on every ARN the map did NOT name, in the owning service's own
        // records and therefore in this API's view of them.
        let tagged = expected(&[BASELINE, WRITTEN]);
        for arn in batch.live() {
            let got = elb_tags(&router, arn).await;
            assert_eq!(
                got, tagged,
                "case {case:?}: expected the owning service to hold {tagged:?} for {arn}, \
                 got {got:?}"
            );
            let page = resources_for(&router, arn).await;
            let got = page_tags(&page, arn);
            assert_eq!(
                got, tagged,
                "case {case:?}: expected GetResources to report {tagged:?} for {arn}, got \
                 {got:?}"
            );
        }
    }
}

/// RULE: an ARN the owning service does not resolve is not tagged at all — a failed
/// entry of the batch leaves no tag anywhere, so nothing reports it afterwards.
#[tokio::test]
async fn an_unresolvable_arn_of_a_batch_is_left_untagged() {
    for (case, slots) in shapes() {
        let router = emulator();
        let batch = batch(&router, &slots).await;
        let (status, body) = tagging(
            &router,
            "TagResources",
            json!({
                "ResourceARNList": &batch.arns,
                "Tags": { WRITTEN.0: WRITTEN.1 },
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: TagResources body {body}");

        for arn in batch.ghosts() {
            // This API's own index does not report it: a resource nothing holds is not a
            // resource this view can carry tags for.
            let page = resources_for(&router, arn).await;
            let reported = page_arns(&page);
            assert!(
                reported.is_empty(),
                "case {case:?}: expected GetResources to report nothing for {arn}, got \
                 {reported:?}"
            );
            // And neither does the owning service, which refuses the read outright.
            let mut form = action("DescribeTags");
            form.push(p("ResourceArns.member.1", arn));
            let (status, own) = elb(&router, &form).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the owning service to refuse DescribeTags({arn}), \
                 got {status} with body {own}"
            );
        }
    }
}

/// RULE: `UntagResources` reports its outcome per resource the same way — HTTP 200,
/// exactly the unresolvable ARNs in `FailedResourcesMap`, and the named keys removed from
/// every ARN of the same request that did resolve.
#[tokio::test]
async fn untag_resources_fails_exactly_the_unresolvable_arns_of_a_batch() {
    for (case, slots) in shapes() {
        let router = emulator();
        let batch = batch(&router, &slots).await;

        // Put the key on every resolvable ARN first, so the untag has something to remove
        // from each of them.
        if !batch.live().is_empty() {
            let live: Vec<&String> = batch.live();
            let (status, body) = tagging(
                &router,
                "TagResources",
                json!({ "ResourceARNList": live, "Tags": { WRITTEN.0: WRITTEN.1 } }),
            )
            .await;
            assert_eq!(status, 200, "case {case:?}: TagResources body {body}");
        }

        let (status, body) = tagging(
            &router,
            "UntagResources",
            json!({ "ResourceARNList": &batch.arns, "TagKeys": [WRITTEN.0] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a per-resource failure is still HTTP 200, got {status} with \
             body {body}"
        );
        let want: BTreeMap<String, String> = batch
            .expected_codes()
            .into_iter()
            .map(|(arn, code)| (arn.clone(), code.to_string()))
            .collect();
        let got = failed_codes(&body);
        assert_eq!(
            got, want,
            "case {case:?}: expected FailedResourcesMap to name {want:?}, got {got:?}"
        );

        let kept = expected(&[BASELINE]);
        for arn in batch.live() {
            let got = elb_tags(&router, arn).await;
            assert_eq!(
                got, kept,
                "case {case:?}: expected the untag to leave {kept:?} on {arn}, got {got:?}"
            );
        }
    }
}

/// RULE: the selection is keyed by ARN, so the same batch written twice answers the same
/// `FailedResourcesMap` — a per-resource failure neither creates the resource nor
/// consumes anything.
#[tokio::test]
async fn the_same_batch_answers_the_same_per_resource_selection_twice() {
    let router = emulator();
    let batch = batch(
        &router,
        &[Slot::GhostTargetGroup, Slot::Live, Slot::GhostLoadBalancer],
    )
    .await;
    let mut answers: Vec<BTreeMap<String, String>> = Vec::new();
    for attempt in 1..=2 {
        let (status, body) = tagging(
            &router,
            "TagResources",
            json!({
                "ResourceARNList": &batch.arns,
                "Tags": { WRITTEN.0: WRITTEN.1 },
            }),
        )
        .await;
        assert_eq!(status, 200, "attempt {attempt}: TagResources body {body}");
        answers.push(failed_codes(&body));
    }
    assert_eq!(
        answers[0], answers[1],
        "expected the second write of the same batch to answer {:?}, got {:?}",
        answers[0], answers[1]
    );
    let ghosts: BTreeSet<String> = batch.ghosts().into_iter().cloned().collect();
    let named: BTreeSet<String> = answers[0].keys().cloned().collect();
    assert_eq!(
        named, ghosts,
        "expected the unresolvable ARNs {ghosts:?} to be the whole selection, got {named:?}"
    );
}
