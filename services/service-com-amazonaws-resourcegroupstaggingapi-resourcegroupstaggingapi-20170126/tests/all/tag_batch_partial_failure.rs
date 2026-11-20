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
//!
//! The second half of the suite spans the other dimension of the same batch: WHICH SERVICE
//! owns each ARN. A `FailedResourcesMap` entry carries the refusal of the service that
//! hosts that one resource, so a batch addressing several owning services at once answers
//! one entry per unresolvable ARN, each with its own owner's status, code and sentence, and
//! none for the ARNs that resolved.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::elb_tagged_resource_index::{
    account, action, elb, elb_arn, elb_ok, elb_tags, emulator, expected, p, page_arns, page_tags,
    require, resources_for, tag_fields, tagging, REGION,
};
use crate::lambda_tagged_resource_index::failures;
use crate::sns_tagged_resource_index::{emulator as sns_emulator, sns_tags, topic};

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

// ---------------------------------------------------------------------------
// The same batch contract across OWNING SERVICES: every entry is composed by the
// service that hosts the one resource its key names.
// ---------------------------------------------------------------------------

/// An ARN in a namespace no owning crate is linked against, so it needs no live resource
/// behind it: the tagging store is its tag home, and a batch that names it must tag it.
const UNOWNED: &str = "arn:aws:cloudwatch:us-east-1:000000000000:alarm/batch-owner-control";

/// What Amazon SNS says about a topic ARN it holds no topic for. The WHOLE sentence is
/// fixed text with no variable part, so a test pins it as the wire carries it rather than
/// as the implementation spells it.
const SNS_ABSENT_MESSAGE: &str = "Resource does not exist";

/// One row of the owner catalogue: an ARN well formed for this endpoint's partition,
/// Region and account that the service owning its namespace holds no resource for, beside
/// the `StatusCode` and `ErrorCode` THAT service's own refusal carries — and, where the
/// owning crate publishes its wording as a constant, the sentence too.
struct Absent {
    case: &'static str,
    arn: String,
    status_code: i64,
    error_code: &'static str,
    message: Option<&'static str>,
}

/// One unresolvable ARN per owning service this API resolves against, spanning the owners
/// whose refusals differ in STATUS as well as in code, so an entry's fields are shown to
/// come from the ARN's own owner rather than from the call.
///
/// Each pair is the error the owning service's own tagging operation refuses an
/// unresolvable resource with, read from that service's model —
/// `smithy_query(query="shape", shape="<Error>")` on its `smithy-model/` reports the
/// `httpError` and, for a service publishing an awsQuery wire, the `awsQueryError` code
/// the wire carries instead of the shape name:
///
/// * Amazon SNS `ResourceNotFoundException`: 404, awsQuery code `ResourceNotFound`;
/// * AWS Lambda `ResourceNotFoundException`: 404;
/// * Amazon SQS `QueueDoesNotExist`: 400, awsQuery code
///   `AWS.SimpleQueueService.NonExistentQueue`;
/// * Elastic Load Balancing `TargetGroupNotFoundException`: 400, awsQuery code
///   `TargetGroupNotFound`;
/// * Amazon DynamoDB, Amazon EventBridge and AWS Secrets Manager
///   `ResourceNotFoundException`: 400;
/// * Amazon ECS `ClusterNotFoundException`: 400.
///
/// The last row is a second resource of an owner already in the table whose NAME carries
/// upper case and an underscore, which the catalogue's other rows do not: the entry is
/// keyed by the request's ARN as written, so a name no case folding would survive is the
/// row that catches a normalised key.
fn absent_per_owner() -> Vec<Absent> {
    let account = account();
    vec![
        Absent {
            case: "an Amazon SNS topic that was never created",
            arn: format!("arn:aws:sns:{REGION}:{account}:batch-owner-ghost-topic"),
            status_code: 404,
            error_code: "ResourceNotFound",
            message: Some(SNS_ABSENT_MESSAGE),
        },
        Absent {
            case: "an AWS Lambda function that was never created",
            arn: format!("arn:aws:lambda:{REGION}:{account}:function:batch-owner-ghost-function"),
            status_code: 404,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an Amazon SQS queue that was never created",
            arn: format!("arn:aws:sqs:{REGION}:{account}:batch-owner-ghost-queue"),
            status_code: 400,
            error_code: "AWS.SimpleQueueService.NonExistentQueue",
            message: None,
        },
        Absent {
            case: "an Elastic Load Balancing target group that was never created",
            arn: elb_arn("targetgroup/batch-owner-ghost/0123456789abcdef"),
            status_code: 400,
            error_code: "TargetGroupNotFound",
            message: None,
        },
        Absent {
            case: "an Amazon DynamoDB table that was never created",
            arn: format!("arn:aws:dynamodb:{REGION}:{account}:table/batch-owner-ghost-table"),
            status_code: 400,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an Amazon EventBridge rule that was never created",
            arn: format!("arn:aws:events:{REGION}:{account}:rule/batch-owner-ghost-rule"),
            status_code: 400,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an AWS Secrets Manager secret that was never created",
            arn: format!("arn:aws:secretsmanager:{REGION}:{account}:secret:batch-owner-ghost-AbCdEf"),
            status_code: 400,
            error_code: "ResourceNotFoundException",
            message: None,
        },
        Absent {
            case: "an Amazon ECS cluster that was never created",
            arn: format!("arn:aws:ecs:{REGION}:{account}:cluster/batch-owner-ghost"),
            status_code: 400,
            error_code: "ClusterNotFoundException",
            message: None,
        },
        Absent {
            case: "an Amazon SNS topic whose name carries upper case and an underscore",
            arn: format!("arn:aws:sns:{REGION}:{account}:Batch_Owner_GHOST"),
            status_code: 404,
            error_code: "ResourceNotFound",
            message: Some(SNS_ABSENT_MESSAGE),
        },
    ]
}

/// The whole `FailedResourcesMap` of one answer, read the way the sibling suite reads it
/// ([`failures`]), checked against the catalogue: every unresolvable ARN of `rows` is named
/// with the status and code its owner refuses it with, and no other ARN is named at all.
///
/// A reference answer has to be checked before another answer is compared to it: a map that
/// named NOTHING — the symptom this suite exists for — would otherwise agree with every
/// other empty map.
fn checked_failures(
    case: &str,
    body: &Value,
    rows: &[&Absent],
) -> BTreeMap<String, (i64, String, String)> {
    let got = failures(body);
    let named: BTreeSet<&String> = got.keys().collect();
    let want: BTreeSet<&String> = rows.iter().map(|row| &row.arn).collect();
    assert_eq!(
        named, want,
        "case {case:?}: expected exactly the unresolvable ARNs of the batch to be named, got \
         {named:?}"
    );
    for row in rows {
        let (status_code, error_code, message) = got
            .get(&row.arn)
            .unwrap_or_else(|| panic!("case {case:?}: no entry for {}", row.arn));
        assert_eq!(
            (*status_code, error_code.as_str()),
            (row.status_code, row.error_code),
            "case {case:?} / {}: expected ({}, {}) for {}, got ({status_code}, {error_code})",
            row.case,
            row.status_code,
            row.error_code,
            row.arn
        );
        match row.message {
            Some(expected) => assert_eq!(
                message, expected,
                "case {case:?} / {}: expected the owning service's sentence {expected:?} for {}, \
                 got {message:?}",
                row.case, row.arn
            ),
            None => assert!(
                !message.is_empty(),
                "case {case:?} / {}: expected a sentence from the owning service for {}, got \
                 {message:?}",
                row.case,
                row.arn
            ),
        }
    }
    got
}

/// One `TagResources` batch that must answer HTTP 200, and the body it answered.
async fn tag_batch(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arns: &[String],
) -> Value {
    let (status, body) = tagging(
        router,
        "TagResources",
        json!({ "ResourceARNList": arns, "Tags": { WRITTEN.0: WRITTEN.1 } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a per-resource failure is still HTTP 200, got {status} with body {body}"
    );
    body
}

/// RULE: every ARN of a batch that the service owning its namespace cannot resolve earns
/// its OWN `FailedResourcesMap` entry, keyed by that ARN exactly as the request wrote it
/// and carrying that owner's status, error code and sentence; the ARNs that did resolve
/// earn no entry and are tagged by the same call.
#[tokio::test]
async fn a_batch_spanning_owning_services_keys_each_failure_to_its_own_owner() {
    let router = sns_emulator();
    // Two resolvable ARNs of different homes: a topic the owning service really holds, and
    // an ARN whose namespace no owning crate serves, whose tags this API itself keeps.
    let live = topic(&router, "batch-owner-live").await;
    let rows = absent_per_owner();

    let mut arns: Vec<String> = vec![live.clone(), UNOWNED.to_string()];
    arns.extend(rows.iter().map(|row| row.arn.clone()));
    let case = "every owner at once";
    let body = tag_batch(&router, case, &arns).await;

    // Exactly the unresolvable ARNs are named, each with its own owner's status, code and
    // sentence; the resolvable ones are named nowhere.
    checked_failures(case, &body, &rows.iter().collect::<Vec<&Absent>>());

    for row in &rows {
        // A per-resource failure brings nothing into being: the ARN is in no answer of
        // this API afterwards.
        let page = resources_for(&router, &row.arn).await;
        let reported = page_arns(&page);
        assert!(
            reported.is_empty(),
            "case {:?}: expected GetResources to report nothing for {}, got {reported:?}",
            row.case,
            row.arn
        );
    }

    // And the resolvable ARNs of that same call were tagged — the topic in the owning
    // service's own records, read through ITS reader, and the unowned ARN in this API's.
    let tagged = expected(&[WRITTEN]);
    let held = sns_tags(&router, &live).await;
    assert_eq!(
        held, tagged,
        "case the resolvable topic: expected the owning service to hold {tagged:?} for \
         {live}, got {held:?}"
    );
    let page = resources_for(&router, UNOWNED).await;
    let got = page_tags(&page, UNOWNED);
    assert_eq!(
        got, tagged,
        "case the resolvable unowned ARN: expected GetResources to report {tagged:?} for \
         {UNOWNED}, got {got:?}"
    );
}

/// RULE: the entry is composed per RESOURCE, so the failure one ARN earns does not depend
/// on the batch it travelled in — its position, the other owners beside it, or whether any
/// ARN of the call resolved at all.
#[tokio::test]
async fn an_absent_arn_earns_the_same_failure_wherever_it_sits_in_a_batch() {
    let router = sns_emulator();
    let live = topic(&router, "batch-owner-position-live").await;
    let rows = absent_per_owner();
    let every: Vec<&Absent> = rows.iter().collect();
    let all: Vec<String> = rows.iter().map(|row| row.arn.clone()).collect();

    // The reference answer: every unresolvable ARN of the catalogue, behind the resolvable
    // one. It is checked against the catalogue before any other answer is compared to it,
    // so an answer naming nothing cannot pass as agreement.
    let mut first = vec![live.clone()];
    first.extend(all.iter().cloned());
    let case = "the resolvable ARN first";
    let baseline = checked_failures(case, &tag_batch(&router, case, &first).await, &every);

    // The shapes the same ARNs are presented in: the resolvable one last, the
    // unresolvable ones reversed around it, the batch where nothing resolves, and each
    // unresolvable ARN alone beside the resolvable one. Each row carries the rows of the
    // catalogue its call addresses, so the check knows which entries to expect.
    let mut shapes: Vec<(String, Vec<String>, Vec<&Absent>)> = Vec::new();
    let mut last = all.clone();
    last.push(live.clone());
    shapes.push(("the resolvable ARN last".to_string(), last, every.clone()));
    let mut reversed: Vec<String> = all.iter().rev().cloned().collect();
    reversed.insert(all.len() / 2, live.clone());
    shapes.push((
        "the unresolvable ARNs reversed around the resolvable one".to_string(),
        reversed,
        every.clone(),
    ));
    shapes.push((
        "no ARN of the batch resolves".to_string(),
        all.clone(),
        every.clone(),
    ));
    for row in &rows {
        shapes.push((
            format!("{} alone beside the resolvable ARN", row.case),
            vec![row.arn.clone(), live.clone()],
            vec![row],
        ));
    }

    for (case, arns, addressed) in shapes {
        let got = checked_failures(&case, &tag_batch(&router, &case, &arns).await, &addressed);
        let want: BTreeMap<String, (i64, String, String)> = baseline
            .iter()
            .filter(|(arn, _)| arns.contains(arn))
            .map(|(arn, failure)| (arn.clone(), failure.clone()))
            .collect();
        assert_eq!(
            got, want,
            "case {case:?}: expected the same per-resource failures {want:?}, got {got:?}"
        );
    }
}
