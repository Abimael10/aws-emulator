// SPDX-License-Identifier: CC0-1.0

//! The SET of legacy target-group attachments an Auto Scaling group holds, over a run of
//! `AttachLoadBalancerTargetGroups` and `DetachLoadBalancerTargetGroups` calls.
//!
//! The invariant under test: whatever sequence of calls a client makes, the target groups a
//! group holds are the insertion-ordered SET of the ARNs attached to it — an attach is
//! additive and holds each one once ("this operation is additive and does not detach existing
//! target groups or Classic Load Balancers from the Auto Scaling group",
//! `AttachLoadBalancerTargetGroups`), a detach removes exactly the ones it names and only
//! when the group holds every one of them, and a refused call changes nothing. BOTH
//! implemented readers of that set report the same sequence: the group's own
//! `TargetGroupARNs` through `DescribeAutoScalingGroups`, and
//! `DescribeLoadBalancerTargetGroups`, which reports "target groups that were attached by
//! using AttachLoadBalancerTargetGroups" with the state each attachment is in.
//!
//! This is a PROPERTY-based suite rather than a table: the invariant is about a run of calls,
//! so each case is a generated sequence of attaches and detaches over a pool of target groups
//! the account holds, replayed against a model of that set held in the test. The sequences are
//! derived from a seed with SHA-256 — the maintained digest this workspace already hashes
//! with — so a failing case is reproduced from the seed its assertion names, the case count is
//! bounded, and nothing is written to disk.

use std::collections::VecDeque;

use sha2::{Digest, Sha256};
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_target_group, emulator, list_members, p, query, seed_subnets, tag_value,
    vpc_of, xml_unescape,
};

/// The launch source every group here is created from — a launch TEMPLATE, because no account
/// can create a launch configuration (see `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The port the target groups of this suite listen on.
const TARGET_GROUP_PORT: i32 = 80;

/// How many target groups the generated sequences draw from. Small on purpose: the invariant
/// is about the ORDER and the de-duplication of a set, and a pool this size already lets one
/// sequence attach, detach and re-attach the same ARN several times over.
const POOL: usize = 4;

/// How many sequences are replayed, and how many calls each makes: bounded, so the suite is
/// quick, and wide enough that every step kind appears many times over.
const SEQUENCES: u64 = 6;
const STEPS_PER_SEQUENCE: usize = 10;

/// The most entries one generated call names. The documented per-call maximum is ten
/// (`AttachLoadBalancerTargetGroupsType$TargetGroupARNs`: "You can specify up to 10 target
/// groups"), which `load_balancer_target_registration.rs` and `legacy_target_group_detach.rs`
/// hold the two operations to; these sequences stay inside it.
const MAX_ENTRIES_PER_CALL: usize = 3;

/// The states a target-group attachment is reported in, from
/// `smithy_query(query="doc",shape="LoadBalancerTargetGroupState",member="State")`: `Adding`,
/// `Added`, `InService`, `Removing` and `Removed`. An attachment the group HOLDS is reported
/// in one of them, whichever the registrations behind it have reached.
const ATTACHMENT_STATES: &[&str] = &["Adding", "Added", "InService", "Removing", "Removed"];

/// The error code every refusal in this suite travels under.
const VALIDATION_ERROR: &str = "ValidationError";

/// A deterministic byte stream derived from one seed, so a generated sequence is reproduced
/// from the seed the failing assertion names. The bytes come from SHA-256 over the seed and a
/// block counter rather than from a hash written here.
struct SeededBytes {
    seed: u64,
    block: u64,
    buffered: VecDeque<u8>,
}

impl SeededBytes {
    fn new(seed: u64) -> Self {
        Self {
            seed,
            block: 0,
            buffered: VecDeque::new(),
        }
    }

    fn next_byte(&mut self) -> u8 {
        if self.buffered.is_empty() {
            let mut digest = Sha256::new();
            digest.update(self.seed.to_be_bytes());
            digest.update(self.block.to_be_bytes());
            self.block += 1;
            self.buffered.extend(digest.finalize().iter().copied());
        }
        self.buffered
            .pop_front()
            .expect("the refilled stream holds bytes")
    }

    /// One value in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        assert!(bound > 0, "a generated choice needs a positive bound");
        usize::from(self.next_byte()) % bound
    }
}

/// One generated call, as positions in the pool of target groups (repeats included, because a
/// call may name the same ARN twice).
#[derive(Debug, Clone)]
enum Step {
    /// An attach: every position is served, held or not.
    Attach(Vec<usize>),
    /// A detach naming only positions the group holds: served.
    Detach(Vec<usize>),
    /// A detach naming at least one position the group does NOT hold: refused whole.
    DetachUnheld(Vec<usize>),
}

/// The next call of a sequence, given what the group holds at that point.
fn next_step(bytes: &mut SeededBytes, held: &[usize]) -> Step {
    let entries = 1 + bytes.below(MAX_ENTRIES_PER_CALL);
    let unheld: Vec<usize> = (0..POOL).filter(|slot| !held.contains(slot)).collect();
    match bytes.below(4) {
        // A detach of what the group holds, which the group must serve.
        0 | 1 if !held.is_empty() => {
            let picked = (0..entries.min(held.len() + 1))
                .map(|_| held[bytes.below(held.len())])
                .collect();
            Step::Detach(picked)
        }
        // A detach naming one the group does not hold, which it must refuse whole: the
        // unheld entry is placed at a generated position among held ones.
        2 if !unheld.is_empty() => {
            let mut picked: Vec<usize> = (0..entries - 1)
                .map(|_| {
                    if held.is_empty() {
                        unheld[bytes.below(unheld.len())]
                    } else {
                        held[bytes.below(held.len())]
                    }
                })
                .collect();
            let at = bytes.below(picked.len() + 1);
            picked.insert(at, unheld[bytes.below(unheld.len())]);
            Step::DetachUnheld(picked)
        }
        _ => Step::Attach((0..entries).map(|_| bytes.below(POOL)).collect()),
    }
}

/// Create one group with no capacity — nothing is launched, so a sequence is about the
/// attachments themselves — attached to nothing.
async fn create_group(router: &MultiServiceRouter, name: &str, subnet_id: &str) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", subnet_id),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Post one target-group call and answer its status, wire error code and message.
async fn target_group_call(
    router: &MultiServiceRouter,
    action: &str,
    name: &str,
    arns: &[String],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The target groups the group reports through its OWN `TargetGroupARNs`, in the order it
/// holds them.
async fn group_target_group_arns(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {name:?} back expected 200, got {status} with {body:?}"
    );
    list_members(&body, "TargetGroupARNs")
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The attachments `DescribeLoadBalancerTargetGroups` reports, as `(ARN, state)` in the order
/// reported.
async fn reported_attachments(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeLoadBalancerTargetGroups",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the attachments of {name:?} expected 200, got {status} with \
         {body:?}"
    );
    list_members(&body, "LoadBalancerTargetGroups")
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "LoadBalancerTargetGroupARN"),
                tag_value(member, "State"),
            )
        })
        .collect()
}

/// Assert both readers report exactly `expected`, in that order, and that every reported
/// attachment carries one of the documented states.
async fn assert_both_readers(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &[String],
) {
    let own = group_target_group_arns(router, case, name).await;
    assert_eq!(
        own, expected,
        "case {case:?}: expected the group's own TargetGroupARNs to be {expected:?}, got \
         {own:?}"
    );
    let reported = reported_attachments(router, case, name).await;
    let arns: Vec<String> = reported.iter().map(|(arn, _)| arn.clone()).collect();
    assert_eq!(
        arns, expected,
        "case {case:?}: expected DescribeLoadBalancerTargetGroups to report {expected:?}, got \
         {reported:?}"
    );
    for (arn, state) in &reported {
        assert!(
            ATTACHMENT_STATES.contains(&state.as_str()),
            "case {case:?}: expected {arn:?} to be in one of the documented states \
             {ATTACHMENT_STATES:?}, got {state:?}"
        );
    }
}

/// The model of the set: an attach pushes every named ARN the model does not already hold, in
/// request order, and leaves the position of one it holds alone.
fn attach_into(model: &mut Vec<usize>, named: &[usize]) {
    for slot in named {
        if !model.contains(slot) {
            model.push(*slot);
        }
    }
}

/// Whatever the run of calls, both readers report the insertion-ordered set of the group's
/// attachments — and a refused detach leaves it exactly as it was.
#[tokio::test]
async fn attach_and_detach_maintain_the_insertion_ordered_attachment_set() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;

    // The target groups a sequence draws from are Elastic Load Balancing v2's own records,
    // created over that service's wire: an attach resolves the ARN it names there.
    let mut pool = Vec::with_capacity(POOL);
    for index in 0..POOL {
        pool.push(
            create_target_group(
                &router,
                &format!("tg-set-{index}"),
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                &vpc_id,
            )
            .await,
        );
    }

    for seed in 0..SEQUENCES {
        let name = format!("asg-set-{seed}");
        create_group(&router, &name, &subnets[0].id).await;
        let mut bytes = SeededBytes::new(seed);
        let mut model: Vec<usize> = Vec::new();
        assert_both_readers(&router, &format!("seed {seed}, before any call"), &name, &[]).await;

        for step_index in 0..STEPS_PER_SEQUENCE {
            let step = next_step(&mut bytes, &model);
            let case = format!("seed {seed}, step {step_index}: {step:?}");
            let named: Vec<String> = match &step {
                Step::Attach(slots) | Step::Detach(slots) | Step::DetachUnheld(slots) => {
                    slots.iter().map(|slot| pool[*slot].clone()).collect()
                }
            };
            match &step {
                Step::Attach(slots) => {
                    let (status, code, message) = target_group_call(
                        &router,
                        "AttachLoadBalancerTargetGroups",
                        &name,
                        &named,
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
                    );
                    attach_into(&mut model, slots);
                }
                Step::Detach(slots) => {
                    let (status, code, message) = target_group_call(
                        &router,
                        "DetachLoadBalancerTargetGroups",
                        &name,
                        &named,
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
                    );
                    model.retain(|slot| !slots.contains(slot));
                }
                Step::DetachUnheld(_) => {
                    let (status, code, message) = target_group_call(
                        &router,
                        "DetachLoadBalancerTargetGroups",
                        &name,
                        &named,
                    )
                    .await;
                    assert_eq!(
                        status, 400,
                        "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
                    );
                    assert_eq!(
                        code, VALIDATION_ERROR,
                        "case {case:?}: expected the {VALIDATION_ERROR} code, got {code:?} \
                         {message:?}"
                    );
                }
            }
            let expected: Vec<String> = model.iter().map(|slot| pool[*slot].clone()).collect();
            assert_both_readers(&router, &case, &name, &expected).await;
        }
    }
}

/// Re-attaching a target group a detach removed puts it back at the END of the set: the group
/// came to hold it again with that call, so the order the readers report is the order it was
/// attached in, not the order it once had.
///
/// The property above generates this case many times over; it is pinned here on its own
/// because it is the one consequence of "insertion-ordered" a reader can see and a
/// de-duplication that kept the old position would get wrong.
#[tokio::test]
async fn a_reattached_target_group_takes_the_end_of_the_set() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;

    let mut pool = Vec::with_capacity(3);
    for index in 0..3 {
        pool.push(
            create_target_group(
                &router,
                &format!("tg-reattach-{index}"),
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                &vpc_id,
            )
            .await,
        );
    }
    let name = "asg-reattach";
    create_group(&router, name, &subnets[0].id).await;

    // Each row: the call to make, the entries it names as pool positions, and the set the
    // group must then hold.
    let rows: Vec<(&str, &str, Vec<usize>, Vec<usize>)> = vec![
        (
            "the three attached in one call",
            "AttachLoadBalancerTargetGroups",
            vec![0, 1, 2],
            vec![0, 1, 2],
        ),
        (
            "the first detached",
            "DetachLoadBalancerTargetGroups",
            vec![0],
            vec![1, 2],
        ),
        (
            "the detached one attached again: last, not first",
            "AttachLoadBalancerTargetGroups",
            vec![0],
            vec![1, 2, 0],
        ),
        (
            "the middle of the three detached",
            "DetachLoadBalancerTargetGroups",
            vec![2],
            vec![1, 0],
        ),
        (
            "every one of them named again, two already held",
            "AttachLoadBalancerTargetGroups",
            vec![0, 1, 2],
            vec![1, 0, 2],
        ),
    ];
    for (case, action, named_slots, held_slots) in rows {
        let named: Vec<String> = named_slots.iter().map(|slot| pool[*slot].clone()).collect();
        let (status, code, message) = target_group_call(&router, action, name, &named).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
        );
        let expected: Vec<String> = held_slots.iter().map(|slot| pool[*slot].clone()).collect();
        assert_both_readers(&router, case, name, &expected).await;
    }
}
