// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for stopping an AppStream app block builder over the wire.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * `StopAppBlockBuilder` terminates the instance of the builder it names: the stored
//!   builder moves to `STOPPING`, and the clock owes it `STOPPED` once the stop's span has
//!   passed — so a caller reading straight after the call still sees the transient state,
//!   and a later read sees a stopped builder;
//! * the stopped builder is reported FROM the stored record: what the stop answers is what
//!   a later `DescribeAppBlockBuilders` reports, and every other member of the builder is
//!   left exactly as the create persisted it;
//! * a stop of a builder that already has no instance, or one already on its way to having
//!   none, asks for nothing new: it is answered with the builder as it stands and starts no
//!   second span;
//! * a builder still coming up is stopped too — the instance the start was bringing up is
//!   the one the stop terminates;
//! * a name this account holds no builder under is refused `ResourceNotFoundException` with
//!   AWS's sentence for that refusal class, whatever form the name takes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::not_found_refusal_status::REFUSAL_STATUS;
use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::Emulator;

/// The only `AppBlockBuilderPlatformType` the model declares
/// (`APP_BLOCK_BUILDER_PLATFORM_TYPE_VALUES` in `src/ops/types/shared_types.rs`).
pub(crate) const PLATFORM: &str = "WINDOWS_SERVER_2019";

/// The word AWS's refusal sentences use for an app block builder.
pub(crate) const BUILDER_KIND: &str = "appblockbuilder";

/// The `PackagingType` whose application package an app block builder produces, which is
/// the only packaging a builder can be associated with.
const BUILDER_PACKAGED: &str = "APPSTREAM2";

/// The `AppBlockBuilderState` values this suite drives a builder through, read from the
/// crate so the test tracks the constants rather than copies of their values.
pub(crate) const STATE_STOPPED: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STATE_STOPPED;
pub(crate) const STATE_STARTING: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STATE_STARTING;
pub(crate) const STATE_RUNNING: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STATE_RUNNING;
pub(crate) const STATE_STOPPING: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STATE_STOPPING;

/// The spans the clock owes a starting and a stopping builder, read from the crate for the
/// same reason.
const START_SPAN: Duration =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_START_SPAN;
const STOP_SPAN: Duration =
    service_com_amazonaws_appstream_photonadminproxyservice::state::APP_BLOCK_BUILDER_STOP_SPAN;

/// The instance types `CreateAppBlockBuilder.InstanceType` documents as available
/// ("stream.standard.small stream.standard.medium stream.standard.large
/// stream.standard.xlarge stream.standard.2xlarge" —
/// `smithy_query(query="doc", shape="CreateAppBlockBuilderRequest", member="InstanceType")`).
/// The rows below span the whole catalog, not only the smallest.
pub(crate) const INSTANCE_TYPES: &[&str] = &[
    "stream.standard.small",
    "stream.standard.medium",
    "stream.standard.large",
    "stream.standard.xlarge",
    "stream.standard.2xlarge",
];

/// Names spanning the class: the rows differ along length, character classes and case. The
/// last is the longest name a create admits — `Name`'s pattern is
/// `^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`
/// (`smithy_query(query="operation", operation="StopAppBlockBuilder")`), so 101
/// characters.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`), so every suite spans the same class.
pub(crate) fn names() -> Vec<String> {
    vec![
        "builder".to_string(),
        "Mixed.Case_builder-9".to_string(),
        "0123456789".to_string(),
        format!("b{}", "5".repeat(100)),
    ]
}

/// Create an app block builder from `request`, which carries its `Name` and whatever else
/// the row gives it.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) async fn create_builder(emulator: &Emulator, case: &str, request: Value) -> Value {
    let created = emulator.ok(case, "CreateAppBlockBuilder", request).await;
    created
        .get("AppBlockBuilder")
        .unwrap_or_else(|| panic!("case {case:?}: expected an AppBlockBuilder, got {created}"))
        .clone()
}

/// The create request for a builder named `name` on `instance_type`, with the two subnets
/// in different availability zones a builder requires.
pub(crate) fn create_request(name: &str, instance_type: &str) -> Value {
    json!({
        "Name": name,
        "Platform": PLATFORM,
        "InstanceType": instance_type,
        "VpcConfig": { "SubnetIds": ["subnet-a1", "subnet-b2"] },
    })
}

/// Fill the builder's single app block slot, which is what a start requires of it: an app
/// block of its own named after `slot`, associated with the builder.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) async fn associate_app_block(
    emulator: &Emulator,
    case: &str,
    builder: &str,
    slot: usize,
) {
    let created = emulator
        .ok(
            case,
            "CreateAppBlock",
            json!({
                "Name": format!("block-{slot}"),
                "PackagingType": BUILDER_PACKAGED,
                "SourceS3Location": { "S3Bucket": "app-packages" },
            }),
        )
        .await;
    let arn = created
        .pointer("/AppBlock/Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the app block's ARN, got {created}"))
        .to_string();
    emulator
        .ok(
            case,
            "AssociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn }),
        )
        .await;
}

/// The builder one `DescribeAppBlockBuilders` reports for `name`.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) async fn described(emulator: &Emulator, case: &str, name: &str) -> Value {
    let payload = emulator
        .ok(case, "DescribeAppBlockBuilders", json!({ "Names": [name] }))
        .await;
    payload
        .pointer("/AppBlockBuilders/0")
        .unwrap_or_else(|| panic!("case {case:?}: expected the builder, got {payload}"))
        .clone()
}

/// The `State` a read reports for `name`.
pub(crate) async fn reported_state(emulator: &Emulator, case: &str, name: &str) -> String {
    let builder = described(emulator, case, name).await;
    builder
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the builder's State, got {builder}"))
        .to_string()
}

/// Wait for the state the clock owes a builder. The transition is due on the clock; nothing
/// here hurries it, and the deadline is far past either span so a loaded host does not
/// decide the outcome.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) async fn await_state(emulator: &Emulator, case: &str, name: &str, expected: &str) {
    let longest = if START_SPAN > STOP_SPAN {
        START_SPAN
    } else {
        STOP_SPAN
    };
    let deadline = Instant::now() + longest * 20;
    loop {
        let state = reported_state(emulator, case, name).await;
        if state == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the builder must reach {expected} within the span the call owes \
             it, still {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Start `name` and answer the `AppBlockBuilder` the call reported.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) async fn start(emulator: &Emulator, case: &str, name: &str) -> Value {
    operate(emulator, case, "StartAppBlockBuilder", name).await
}

/// Stop `name` and answer the `AppBlockBuilder` the call reported.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) async fn stop(emulator: &Emulator, case: &str, name: &str) -> Value {
    operate(emulator, case, "StopAppBlockBuilder", name).await
}

/// One of the lifecycle operations that takes a builder's `Name` alone, and the builder it
/// reported.
async fn operate(emulator: &Emulator, case: &str, operation: &str, name: &str) -> Value {
    let payload = emulator.ok(case, operation, json!({ "Name": name })).await;
    payload
        .get("AppBlockBuilder")
        .unwrap_or_else(|| panic!("case {case:?}: expected an AppBlockBuilder, got {payload}"))
        .clone()
}

/// A builder with its app block slot filled and its instance up, which is the state a stop
/// takes a builder out of.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) async fn running_builder(
    emulator: &Emulator,
    case: &str,
    name: &str,
    instance_type: &str,
    slot: usize,
) -> Value {
    create_builder(emulator, case, create_request(name, instance_type)).await;
    associate_app_block(emulator, case, name, slot).await;
    start(emulator, case, name).await;
    await_state(emulator, case, name, STATE_RUNNING).await;
    described(emulator, case, name).await
}

/// Assert that `reported` carries every member of `stored` but `State`, which is the one
/// thing a start or a stop moves.
///
/// Shared with the other suites in this target
/// (`tests/all/app_block_builder_update.rs`).
pub(crate) fn assert_only_the_state_moved(case: &str, stored: &Value, reported: &Value) {
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
            "case {case:?}: {member} must be left as it was ({value}), got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_stop_moves_the_builder_to_stopping_and_the_clock_owes_it_stopped() {
    // Rows: the Region the request's credential scope names, and — within each Region — the
    // name's form and the instance type the builder runs on. Every builder of a Region is
    // stopped before any is awaited, so the span the clock owes them runs once.
    for region in ["us-east-1", "eu-west-2"] {
        let emulator = Emulator::in_region(region);
        let mut stopping: Vec<(String, String, Value)> = Vec::new();

        for (index, name) in names().into_iter().enumerate() {
            let case = format!("{region} stop of {name:?}");
            let instance_type = INSTANCE_TYPES[index % INSTANCE_TYPES.len()];
            let running = running_builder(&emulator, &case, &name, instance_type, index).await;
            assert_eq!(
                running.get("State").and_then(Value::as_str),
                Some(STATE_RUNNING),
                "case {case:?}: the builder must be {STATE_RUNNING} before the stop, \
                 got {running}"
            );

            let reported = stop(&emulator, &case, &name).await;
            assert_eq!(
                reported.get("State").and_then(Value::as_str),
                Some(STATE_STOPPING),
                "case {case:?}: a caller reading straight after the stop must still see \
                 {STATE_STOPPING}, got {reported}"
            );
            assert_only_the_state_moved(&case, &running, &reported);

            // The stop answers the stored record, so a read straight after it reports the
            // same builder.
            let read = described(&emulator, &case, &name).await;
            assert_eq!(
                read, reported,
                "case {case:?}: the read must report the record the stop wrote \
                 ({reported}), got {read}"
            );
            stopping.push((case, name, running));
        }

        for (case, name, running) in stopping {
            await_state(&emulator, &case, &name, STATE_STOPPED).await;
            let stopped = described(&emulator, &case, &name).await;
            assert_only_the_state_moved(&case, &running, &stopped);

            // The transition is applied once: a later read reports the same stopped
            // builder rather than moving it on again.
            let again = described(&emulator, &case, &name).await;
            assert_eq!(
                again, stopped,
                "case {case:?}: a second read must answer identically, got {again}"
            );
        }
    }
}

#[tokio::test]
async fn a_stop_of_a_builder_that_is_already_stopped_or_stopping_asks_for_nothing_new() {
    let emulator = Emulator::new();

    // Rows: how far along the builder already is when the stop arrives — never started at
    // all, and already on its way down.
    for (index, name) in names().into_iter().enumerate() {
        let case = format!("repeat stop of {name:?}");
        let instance_type = INSTANCE_TYPES[index % INSTANCE_TYPES.len()];
        let created =
            create_builder(&emulator, &case, create_request(&name, instance_type)).await;

        let stopped = stop(&emulator, &case, &name).await;
        assert_eq!(
            stopped, created,
            "case {case:?}: a stop of a builder that has no instance must answer it \
             unchanged ({created}), got {stopped}"
        );
        // No span was started, so the builder stays stopped rather than reporting a
        // transient state.
        let state = reported_state(&emulator, &case, &name).await;
        assert_eq!(
            state, STATE_STOPPED,
            "case {case:?}: expected {STATE_STOPPED}, got {state:?}"
        );

        associate_app_block(&emulator, &case, &name, index).await;
        start(&emulator, &case, &name).await;
        await_state(&emulator, &case, &name, STATE_RUNNING).await;

        let first = stop(&emulator, &case, &name).await;
        let second = stop(&emulator, &case, &name).await;
        assert_eq!(
            second, first,
            "case {case:?}: a stop of a builder that is already stopping must answer the \
             builder it already is ({first}), got {second}"
        );
        await_state(&emulator, &case, &name, STATE_STOPPED).await;
    }
}

#[tokio::test]
async fn a_stop_of_a_builder_that_is_still_coming_up_terminates_the_instance_it_was_bringing_up() {
    let emulator = Emulator::new();

    for (index, name) in names().into_iter().enumerate() {
        let case = format!("stop while starting {name:?}");
        let instance_type = INSTANCE_TYPES[index % INSTANCE_TYPES.len()];
        create_builder(&emulator, &case, create_request(&name, instance_type)).await;
        associate_app_block(&emulator, &case, &name, index).await;

        let starting = start(&emulator, &case, &name).await;
        assert_eq!(
            starting.get("State").and_then(Value::as_str),
            Some(STATE_STARTING),
            "case {case:?}: a builder whose instance is coming up must be {STATE_STARTING}, \
             got {starting}"
        );

        let reported = stop(&emulator, &case, &name).await;
        assert_eq!(
            reported.get("State").and_then(Value::as_str),
            Some(STATE_STOPPING),
            "case {case:?}: the stop must terminate the instance the start was bringing up, \
             got {reported}"
        );

        // What the clock owes the builder now is the state the stop asked for, not the one
        // the start did.
        await_state(&emulator, &case, &name, STATE_STOPPED).await;
        let state = reported_state(&emulator, &case, &name).await;
        assert_eq!(
            state, STATE_STOPPED,
            "case {case:?}: expected {STATE_STOPPED}, got {state:?}"
        );
    }
}

#[tokio::test]
async fn a_stop_of_a_name_the_account_holds_no_builder_under_is_refused() {
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
    // One builder the account does hold, so the refusals below are about the name asked
    // for rather than about an empty store.
    let stored = create_builder(
        &emulator,
        "setup",
        create_request("stored-builder", INSTANCE_TYPES[1]),
    )
    .await;

    for name in names() {
        let case = format!("absent {name:?}");
        let message = refusal(
            &emulator,
            &case,
            "StopAppBlockBuilder",
            json!({ "Name": name }),
            REFUSAL_STATUS,
            "ResourceNotFoundException",
        )
        .await;
        let expected = not_found_sentence(BUILDER_KIND, &name);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
    }

    // The refused stops wrote nothing: the one builder the account holds is as it was.
    let read = described(&emulator, "stored-builder", "stored-builder").await;
    assert_eq!(
        read, stored,
        "case \"stored-builder\": the refused stops must leave the stored builder \
         untouched ({stored}), got {read}"
    );
}
