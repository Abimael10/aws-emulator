// SPDX-License-Identifier: CC0-1.0

//! The app block builder's single app block slot, pinned as an INVARIANT over randomized
//! walks of associate and disassociate requests rather than over one recorded order.
//!
//! The rule under test: an app block builder holds AT MOST ONE associated app block, and
//! the check is on what the store holds right now — so while a builder's slot is filled,
//! a request naming a DIFFERENT app block is refused `OperationNotPermittedException`
//! (HTTP 400, `smithy_query(query="operation",
//! operation="AssociateAppBlockBuilderAppBlock")`) with AWS's fixed sentence and writes
//! nothing, the app block already in the slot is answered as the association it already
//! is, and disassociating the app block in the slot frees it so the very request that was
//! refused then succeeds.
//!
//! A disassociation removes exactly the pair it names: both endpoints have to exist —
//! `DisassociateAppBlockBuilderAppBlock` models `ResourceNotFoundException` and no error for
//! an association that is absent (`smithy_query(query="operation",
//! operation="DisassociateAppBlockBuilderAppBlock")`, whose result carries no members) — so
//! removing an edge the store does not hold is answered as the removal it already is and
//! changes nothing, which is the half of the rule `disassociating_frees_the_builders_slot`
//! in `app_block_builder_app_block_associations.rs` states for a named pair and the walk
//! below exercises in every order.
//!
//! The walk below keeps a MODEL of that one rule — each builder's slot, holding either
//! nothing or one app block ARN — and after every request asserts both the answer and
//! every read of the store against it. A walk therefore covers the orders a table cannot
//! enumerate: re-associating what is held, offering a third app block to a filled slot,
//! removing an edge the store never held, refilling a freed slot with the app block that
//! was just removed, and the same app block filling several builders' slots at once.
//! Every case names the seed it was generated from, so a failure is reproducible without
//! printing anything.

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};

use crate::app_block_builder_app_block_associations::{
    associated, create_associable_app_block, create_builder, slot_taken_sentence,
    NOT_PERMITTED_CODE, REGION,
};
use crate::refusal_messages::refusal;
use crate::stack_fleet_lifecycle::Emulator;

/// The seeds the walk is driven with — a bounded, fixed set, so the suite is deterministic
/// and costs the same on every run.
const SEEDS: u64 = 4;

/// How many requests one walk makes. Enough that a builder's slot is filled, offered
/// another app block, freed and refilled many times over, and small enough to stay well
/// inside a test's budget.
const STEPS: usize = 14;

/// How many builders and app blocks a walk draws from. More app blocks than the one slot
/// of each builder is what makes the slot rule observable; more than one builder is what
/// shows the slot is the BUILDER's.
const BUILDERS: usize = 3;
const APP_BLOCKS: usize = 3;

/// How often a step removes an edge rather than adding one. Below one half, so a walk
/// spends most of its steps with slots filled, where the rule bites.
const DISASSOCIATE_SHARE: f64 = 0.4;

/// The sentence AWS states when an app block builder's one app block slot is already taken,
/// spelled out here so the wording itself is pinned rather than only compared with the crate's
/// own constant. It is entirely fixed text: it takes no name, ARN or value from the request or
/// from state.
const SLOT_TAKEN_SENTENCE: &str =
    "The selected App block builder can not be associated with additional App block";

/// The edge a `(builder, app block)` pair reads back as.
fn edge(builder: &str, app_block_arn: &str) -> Value {
    json!({ "AppBlockBuilderName": builder, "AppBlockArn": app_block_arn })
}

/// The associations the model expects, in the order the store reports them: by builder
/// name, each builder contributing its one slot if it is filled.
fn expected_edges(slots: &BTreeMap<String, Option<String>>) -> Vec<Value> {
    slots
        .iter()
        .filter_map(|(builder, held)| held.as_ref().map(|arn| edge(builder, arn)))
        .collect()
}

#[tokio::test]
async fn one_slot_per_builder_holds_over_every_order_of_requests() {
    // The crate states the sentence once and every suite reads it from there, so pin that
    // wording against AWS's own text — a property of the service, not of any one walk.
    assert_eq!(
        slot_taken_sentence(),
        SLOT_TAKEN_SENTENCE,
        "the service must state AWS's sentence for a taken slot, got {:?}",
        slot_taken_sentence()
    );
    for seed in 0..SEEDS {
        let case = format!("seed {seed}");
        let mut rng = StdRng::seed_from_u64(seed);
        let emulator = Emulator::in_region(REGION);

        let mut builders: Vec<String> = Vec::new();
        for index in 0..BUILDERS {
            let name = format!("walk-builder-{index}");
            create_builder(&emulator, &case, &name).await;
            builders.push(name);
        }
        let mut app_blocks: Vec<String> = Vec::new();
        for index in 0..APP_BLOCKS {
            app_blocks.push(
                create_associable_app_block(&emulator, &case, &format!("walk-block-{index}")).await,
            );
        }

        // The model: one slot per builder, every one of them empty to start with.
        let mut slots: BTreeMap<String, Option<String>> = builders
            .iter()
            .map(|builder| (builder.clone(), None))
            .collect();

        for step in 0..STEPS {
            let builder = builders[rng.random_range(0..builders.len())].clone();
            let app_block = app_blocks[rng.random_range(0..app_blocks.len())].clone();
            let step_case = format!("{case}, step {step}: builder {builder:?}, {app_block}");
            let request = edge(&builder, &app_block);
            let held = slots
                .get(&builder)
                .cloned()
                .expect("the model holds every builder the walk created");

            if rng.random_bool(DISASSOCIATE_SHARE) {
                // Removing an edge removes exactly the pair it names: the slot is freed
                // only when the app block it holds is the one named, and removing a pair
                // the store does not hold changes nothing.
                let answer = emulator
                    .ok(
                        &step_case,
                        "DisassociateAppBlockBuilderAppBlock",
                        request.clone(),
                    )
                    .await;
                assert_eq!(
                    answer,
                    json!({}),
                    "case {step_case:?}: a disassociation carries no members, got {answer}"
                );
                if held.as_deref() == Some(app_block.as_str()) {
                    slots.insert(builder.clone(), None);
                }
            } else if held.as_deref().is_some_and(|slot| slot != app_block) {
                // The slot is filled by a DIFFERENT app block, so this is an additional
                // one: refused, with nothing written.
                let message = refusal(
                    &emulator,
                    &step_case,
                    "AssociateAppBlockBuilderAppBlock",
                    request.clone(),
                    400,
                    NOT_PERMITTED_CODE,
                )
                .await;
                assert_eq!(
                    message,
                    SLOT_TAKEN_SENTENCE,
                    "case {step_case:?}: the refusal must state that the builder takes no \
                     additional app block, got {message:?}"
                );
                // The sentence states the rule about the builder that was selected, so it
                // echoes nothing of the request or of the store.
                assert!(
                    !message.contains(builder.as_str()) && !message.contains(app_block.as_str()),
                    "case {step_case:?}: the refusal must echo neither the builder's name nor \
                     the ARN, got {message:?}"
                );
            } else {
                // Either the slot is free, or it already holds exactly this app block —
                // which is no additional app block, so the same pair is answered as the
                // association it already is.
                let answer = emulator
                    .ok(
                        &step_case,
                        "AssociateAppBlockBuilderAppBlock",
                        request.clone(),
                    )
                    .await;
                assert_eq!(
                    answer,
                    json!({ "AppBlockBuilderAppBlockAssociation": request }),
                    "case {step_case:?}: the association must be answered for the pair the \
                     request named, got {answer}"
                );
                slots.insert(builder.clone(), Some(app_block.clone()));
            }

            // Every read agrees with the model after every step: the whole store, each
            // builder's slot, and each app block's side (which takes any number of edges,
            // because the slot is the builder's).
            let reported = associated(&emulator, &step_case, json!({})).await;
            let expected = expected_edges(&slots);
            assert_eq!(
                reported, expected,
                "case {step_case:?}: the store must report {expected:?}, got {reported:?}"
            );
            for (name, slot) in &slots {
                let reported =
                    associated(&emulator, &step_case, json!({ "AppBlockBuilderName": name })).await;
                assert!(
                    reported.len() <= 1,
                    "case {step_case:?}: builder {name:?} may hold at most one app block, got \
                     {reported:?}"
                );
                let expected: Vec<Value> = slot.iter().map(|arn| edge(name, arn)).collect();
                assert_eq!(
                    reported, expected,
                    "case {step_case:?}: builder {name:?} must report {expected:?}, got \
                     {reported:?}"
                );
            }
            for arn in &app_blocks {
                let reported =
                    associated(&emulator, &step_case, json!({ "AppBlockArn": arn })).await;
                let expected: Vec<Value> = slots
                    .iter()
                    .filter(|(_, held)| held.as_deref() == Some(arn.as_str()))
                    .map(|(name, _)| edge(name, arn))
                    .collect();
                assert_eq!(
                    reported, expected,
                    "case {step_case:?}: the app block {arn} must report {expected:?}, got \
                     {reported:?}"
                );
            }
        }
    }
}
