// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for starting an AppStream app block builder over the wire.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * `StartAppBlockBuilder` starts the instance of the builder it names: the stored builder
//!   moves to `STARTING`, and the clock owes it `RUNNING` once the start's span has passed —
//!   so a caller reading straight after the call still sees the transient state, and a
//!   later read sees a running builder;
//! * the started builder is reported FROM the stored record: what the start answers is what
//!   a later `DescribeAppBlockBuilders` reports, and every other member of the builder is
//!   left exactly as the create persisted it;
//! * a builder that is associated with no app block cannot be started: the call is refused
//!   `OperationNotPermittedException` (400) carrying the sentence for that rule, and the
//!   builder keeps the state it had — and the gate is live, so associating an app block
//!   opens it and disassociating closes it again;
//! * starting a builder that is already starting or already running asks for nothing new:
//!   it is answered with the builder's current state and starts no second span;
//! * a name this account holds no builder under is refused `ResourceNotFoundException`
//!   (400) with AWS's sentence for that refusal class, whatever form the name takes and
//!   whatever other resource family holds it.

use std::time::Duration;

use serde_json::{json, Value};

use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::Emulator;

/// The only `AppBlockBuilderPlatformType` the model declares
/// (`APP_BLOCK_BUILDER_PLATFORM_TYPE_VALUES` in `src/ops/types/shared_types.rs`).
const PLATFORM: &str = "WINDOWS_SERVER_2019";

/// The word AWS's refusal sentences use for an app block builder.
const BUILDER_KIND: &str = "appblockbuilder";

/// The `PackagingType` whose application package an app block builder produces, which is
/// the only packaging a builder can be associated with.
const BUILDER_PACKAGED: &str = "APPSTREAM2";

/// The `AppBlockBuilderState` values this suite drives a builder through, read from the
/// crate so the test tracks the constants rather than copies of their values.
const STATE_STOPPED: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STATE_CREATED;
const STATE_STARTING: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STATE_STARTING;
const STATE_RUNNING: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STATE_RUNNING;

/// The span the clock owes a starting builder before its instance is up, read from the
/// crate for the same reason.
const START_SPAN: Duration =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_START_SPAN;

/// The instance types `CreateAppBlockBuilder.InstanceType` documents as available
/// ("stream.standard.small stream.standard.medium stream.standard.large
/// stream.standard.xlarge stream.standard.2xlarge" —
/// `smithy_query(query="doc", shape="CreateAppBlockBuilderRequest", member="InstanceType")`).
/// The rows below span the whole catalog, not only the smallest.
const INSTANCE_TYPES: &[&str] = &[
    "stream.standard.small",
    "stream.standard.medium",
    "stream.standard.large",
    "stream.standard.xlarge",
    "stream.standard.2xlarge",
];

/// The sentence the service refuses the start of a builder with an empty app block slot
/// with, read from the crate so the test tracks the wording rather than a copy of it.
fn not_associated_sentence(name: &str) -> String {
    service_com_amazonaws_appstream_photonadminproxyservice::state::app_block_builder_not_associated_message(name)
}

/// Names spanning the class: the rows differ along length, character classes and case. The
/// last is the longest name a create admits — `Name`'s pattern is
/// `^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`
/// (`smithy_query(query="operation", operation="StartAppBlockBuilder")`), so 101
/// characters.
fn names() -> Vec<String> {
    vec![
        "builder".to_string(),
        "Mixed.Case_builder-9".to_string(),
        "0123456789".to_string(),
        format!("b{}", "4".repeat(100)),
    ]
}

/// Create an app block builder named `name`, on the instance type the row picked.
async fn create_builder(emulator: &Emulator, case: &str, name: &str, instance_type: &str) {
    emulator
        .ok(
            case,
            "CreateAppBlockBuilder",
            json!({
                "Name": name,
                "Platform": PLATFORM,
                "InstanceType": instance_type,
                "VpcConfig": { "SubnetIds": ["subnet-a1", "subnet-b2"] },
            }),
        )
        .await;
}

/// Create an app block a builder can package applications into, and answer its ARN.
async fn create_app_block(emulator: &Emulator, case: &str, name: &str) -> String {
    let created = emulator
        .ok(
            case,
            "CreateAppBlock",
            json!({
                "Name": name,
                "PackagingType": BUILDER_PACKAGED,
                "SourceS3Location": { "S3Bucket": "app-packages" },
            }),
        )
        .await;
    created
        .pointer("/AppBlock/Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the app block's ARN, got {created}"))
        .to_string()
}

/// Fill the builder's single app block slot, which is what a start requires of it.
async fn associate(emulator: &Emulator, case: &str, builder: &str, app_block_arn: &str) {
    emulator
        .ok(
            case,
            "AssociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": builder, "AppBlockArn": app_block_arn }),
        )
        .await;
}

/// The builder one `DescribeAppBlockBuilders` reports for `name`.
async fn described(emulator: &Emulator, case: &str, name: &str) -> Value {
    let payload = emulator
        .ok(case, "DescribeAppBlockBuilders", json!({ "Names": [name] }))
        .await;
    payload
        .pointer("/AppBlockBuilders/0")
        .unwrap_or_else(|| panic!("case {case:?}: expected the builder, got {payload}"))
        .clone()
}

/// The `State` a read reports for `name`.
async fn reported_state(emulator: &Emulator, case: &str, name: &str) -> String {
    let builder = described(emulator, case, name).await;
    builder
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the builder's State, got {builder}"))
        .to_string()
}

/// Start `name` and answer the `AppBlockBuilder` the call reported.
async fn start(emulator: &Emulator, case: &str, name: &str) -> Value {
    let payload = emulator
        .ok(case, "StartAppBlockBuilder", json!({ "Name": name }))
        .await;
    payload
        .get("AppBlockBuilder")
        .unwrap_or_else(|| panic!("case {case:?}: expected an AppBlockBuilder, got {payload}"))
        .clone()
}

/// Wait for the state the clock owes a started builder. The transition is due on the
/// clock; nothing here hurries it, and the deadline is far past the span so a loaded host
/// does not decide the outcome.
async fn await_running(emulator: &Emulator, case: &str, name: &str) {
    let deadline = std::time::Instant::now() + START_SPAN * 20;
    loop {
        let state = reported_state(emulator, case, name).await;
        if state == STATE_RUNNING {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the builder must reach {STATE_RUNNING} within the span the start \
             owes it, still {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A builder with its app block slot filled, ready to be started. `slot` distinguishes the
/// app block of one builder from another's: an app block's `Name` carries the same pattern
/// as a builder's, so it cannot simply be derived from the longest builder name.
async fn startable_builder(
    emulator: &Emulator,
    case: &str,
    name: &str,
    instance_type: &str,
    slot: usize,
) {
    create_builder(emulator, case, name, instance_type).await;
    let app_block_arn = create_app_block(emulator, case, &format!("block-{slot}")).await;
    associate(emulator, case, name, &app_block_arn).await;
}

/// Assert that `reported` carries every member of `stored` but `State`, which is the one
/// thing a start moves.
fn assert_only_the_state_moved(case: &str, stored: &Value, reported: &Value) {
    let members = stored
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a builder object, got {stored}"));
    for (member, value) in members {
        if member == "State" {
            continue;
        }
        let actual = reported.get(member);
        assert_eq!(
            actual,
            Some(value),
            "case {case:?}: {member} must be left as the create persisted it ({value}), \
             got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_start_moves_the_builder_to_starting_and_the_clock_owes_it_running() {
    // Rows: the Region the request's credential scope names, and — within each Region — the
    // name's form and the instance type the builder runs on. Every builder of a Region is
    // started before any is awaited, so the span the clock owes them runs once.
    for region in ["us-east-1", "eu-west-2"] {
        let emulator = Emulator::in_region(region);
        let mut started: Vec<(String, String, Value)> = Vec::new();

        for (index, name) in names().into_iter().enumerate() {
            let case = format!("{region} start of {name:?}");
            let instance_type = INSTANCE_TYPES[index % INSTANCE_TYPES.len()];
            startable_builder(&emulator, &case, &name, instance_type, index).await;

            let stored = described(&emulator, &case, &name).await;
            assert_eq!(
                stored.get("State").and_then(Value::as_str),
                Some(STATE_STOPPED),
                "case {case:?}: a builder nobody has started must be {STATE_STOPPED}, \
                 got {stored}"
            );

            let reported = start(&emulator, &case, &name).await;
            assert_eq!(
                reported.get("State").and_then(Value::as_str),
                Some(STATE_STARTING),
                "case {case:?}: a caller reading straight after the start must still see \
                 {STATE_STARTING}, got {reported}"
            );
            assert_only_the_state_moved(&case, &stored, &reported);

            // The start answers the stored record, so a read straight after it reports the
            // same builder.
            let read = described(&emulator, &case, &name).await;
            assert_eq!(
                read, reported,
                "case {case:?}: the read must report the record the start wrote \
                 ({reported}), got {read}"
            );
            started.push((case, name, stored));
        }

        for (case, name, stored) in started {
            await_running(&emulator, &case, &name).await;
            let running = described(&emulator, &case, &name).await;
            assert_only_the_state_moved(&case, &stored, &running);

            // The transition is applied once: a later read reports the same running
            // builder rather than moving it on again.
            let again = described(&emulator, &case, &name).await;
            assert_eq!(
                again, running,
                "case {case:?}: a second read must answer identically, got {again}"
            );
        }
    }
}

#[tokio::test]
async fn a_start_of_a_builder_that_is_already_starting_or_running_asks_for_nothing_new() {
    let emulator = Emulator::new();

    // Rows: how far along the builder already is when the second start arrives — still
    // coming up, and already up.
    for (index, name) in names().into_iter().enumerate() {
        let case = format!("repeat start of {name:?}");
        startable_builder(
            &emulator,
            &case,
            &name,
            INSTANCE_TYPES[index % INSTANCE_TYPES.len()],
            index,
        )
        .await;

        let first = start(&emulator, &case, &name).await;
        let second = start(&emulator, &case, &name).await;
        assert_eq!(
            second, first,
            "case {case:?}: a start of a builder that is already starting must answer the \
             builder it already is ({first}), got {second}"
        );

        await_running(&emulator, &case, &name).await;
        let running = described(&emulator, &case, &name).await;
        let third = start(&emulator, &case, &name).await;
        assert_eq!(
            third, running,
            "case {case:?}: a start of a running builder must answer it unchanged \
             ({running}), got {third}"
        );
        // No second span was started, so the builder stays running rather than reporting
        // the transient state again.
        let after = reported_state(&emulator, &case, &name).await;
        assert_eq!(
            after, STATE_RUNNING,
            "case {case:?}: expected {STATE_RUNNING}, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_builder_associated_with_no_app_block_cannot_be_started() {
    // Rows: the name's form, and — for each — a builder that never had an app block and one
    // whose app block was disassociated again, so the gate is shown to read the edges the
    // store holds right now.
    for (index, name) in names().into_iter().enumerate() {
        let emulator = Emulator::new();
        let case = format!("unassociated {name:?}");
        let instance_type = INSTANCE_TYPES[index % INSTANCE_TYPES.len()];
        create_builder(&emulator, &case, &name, instance_type).await;

        let message = refusal(
            &emulator,
            &case,
            "StartAppBlockBuilder",
            json!({ "Name": name }),
            400,
            "OperationNotPermittedException",
        )
        .await;
        let expected = not_associated_sentence(&name);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
        let state = reported_state(&emulator, &case, &name).await;
        assert_eq!(
            state, STATE_STOPPED,
            "case {case:?}: a refused start must leave the builder {STATE_STOPPED}, \
             got {state:?}"
        );

        // Filling the slot opens the gate: the same request now starts the builder.
        let app_block_arn = create_app_block(&emulator, &case, &format!("block-{index}")).await;
        associate(&emulator, &case, &name, &app_block_arn).await;
        let reported = start(&emulator, &case, &name).await;
        assert_eq!(
            reported.get("State").and_then(Value::as_str),
            Some(STATE_STARTING),
            "case {case:?}: a builder with an app block must start, got {reported}"
        );

        // Emptying it again closes the gate: a builder with no app block is refused
        // whatever state it is in.
        let case = format!("disassociated {name:?}");
        emulator
            .ok(
                &case,
                "DisassociateAppBlockBuilderAppBlock",
                json!({ "AppBlockBuilderName": name, "AppBlockArn": app_block_arn }),
            )
            .await;
        let message = refusal(
            &emulator,
            &case,
            "StartAppBlockBuilder",
            json!({ "Name": name }),
            400,
            "OperationNotPermittedException",
        )
        .await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_name_the_account_holds_no_builder_under_is_refused() {
    let emulator = Emulator::new();
    // A fleet and an app block of one of the names below: a builder is a resource family of
    // its own, so a name another family holds still names no builder.
    emulator
        .ok(
            "setup",
            "CreateFleet",
            json!({ "Name": "builder", "InstanceType": INSTANCE_TYPES[0] }),
        )
        .await;
    create_app_block(&emulator, "setup", "0123456789").await;
    // One builder the account does hold, so the refusals below are about the name asked
    // for rather than about an empty store.
    startable_builder(&emulator, "setup", "stored-builder", INSTANCE_TYPES[1], 1).await;

    for name in names() {
        let case = format!("absent {name:?}");
        let message = refusal(
            &emulator,
            &case,
            "StartAppBlockBuilder",
            json!({ "Name": name }),
            400,
            "ResourceNotFoundException",
        )
        .await;
        let expected = not_found_sentence(BUILDER_KIND, &name);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
    }

    // The refused starts wrote nothing: the one builder the account holds is still stopped.
    let state = reported_state(&emulator, "stored-builder", "stored-builder").await;
    assert_eq!(
        state, STATE_STOPPED,
        "case \"stored-builder\": expected {STATE_STOPPED}, got {state:?}"
    );
}
