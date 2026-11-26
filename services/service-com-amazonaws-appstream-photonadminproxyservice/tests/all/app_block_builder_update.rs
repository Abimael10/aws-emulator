// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for updating an AppStream app block builder over the wire.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rule under test is the one `UpdateAppBlockBuilder`'s own documentation states — "If
//! the app block builder is in the STARTING or STOPPING state, you can't update it. If the
//! app block builder is in the RUNNING state, you can only update the DisplayName and
//! Description. If the app block builder is in the STOPPED state, you can update any
//! attribute except the Name" — asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * a stopped builder admits every attribute: each member the request carries replaces
//!   the stored field, each member it omits leaves that field alone, the `Name` addresses
//!   the builder rather than renaming it, and what the update answers is what a later
//!   `DescribeAppBlockBuilders` reports;
//! * `AttributesToDelete` clears the attributes the modelled `AppBlockBuilderAttribute`
//!   enum names, and setting an attribute's configuration while naming that attribute in
//!   the same request is refused `InvalidParameterCombinationException`;
//! * a running builder admits its `DisplayName` and `Description` alone: every other
//!   attribute is refused `ResourceInUseException` with AWS's sentence naming the builder
//!   and the state it is in, and the refused update leaves the stored builder untouched —
//!   so the identical update applies once the builder has been stopped;
//! * a builder coming up or going down admits no update at all, display members included;
//! * a name this account holds no builder under is refused `ResourceNotFoundException`
//!   with AWS's sentence for that refusal class, whatever form the name takes.

use serde_json::{json, Value};

use crate::app_block_builder_stop::{
    assert_only_the_state_moved, associate_app_block, await_state, create_builder, described,
    names, reported_state, start, stop, BUILDER_KIND, INSTANCE_TYPES, PLATFORM, STATE_RUNNING,
    STATE_STARTING, STATE_STOPPED, STATE_STOPPING,
};
use crate::not_found_refusal_status::REFUSAL_STATUS;
use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::Emulator;

/// The account every emulated resource belongs to, so an `IamRoleArn` row names a role of
/// the account that was searched rather than a value from one run.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The only `AccessEndpointType` the model declares (`ACCESS_ENDPOINT_TYPE_VALUES` in
/// `src/ops/types/shared_types.rs`).
const ENDPOINT_TYPE: &str = "STREAMING";

/// The members `UpdateAppBlockBuilder` can write that the modelled `AppBlockBuilder` also
/// carries: each is reported back exactly as the request gave it, and keeps the value the
/// create persisted when the request omits it. `Name` is among them because an update
/// ADDRESSES the builder by it: "you can update any attribute except the Name".
const ECHOED_MEMBERS: &[&str] = &[
    "Name",
    "Platform",
    "InstanceType",
    "VpcConfig",
    "Description",
    "DisplayName",
    "EnableDefaultInternetAccess",
    "IamRoleArn",
    "AccessEndpoints",
    "DisableIMDSV1",
];

/// The `AppBlockBuilderAttribute` values `AttributesToDelete` can name
/// (`smithy_query(query="shape", shape="AppBlockBuilderAttribute")`, kept as
/// `APP_BLOCK_BUILDER_ATTRIBUTE_VALUES` in `src/ops/types/shared_types.rs`).
const ATTRIBUTE_IAM_ROLE_ARN: &str = "IAM_ROLE_ARN";
const ATTRIBUTE_ACCESS_ENDPOINTS: &str = "ACCESS_ENDPOINTS";
const ATTRIBUTE_SECURITY_GROUP_IDS: &str = "VPC_CONFIGURATION_SECURITY_GROUP_IDS";

/// AWS's `ResourceInUseException` sentence for a write of attributes the builder's state
/// does not admit: fixed text apart from the builder's name, as the request addressed it,
/// and the state the stored builder is in — and it ends in a space, as AWS's own does.
fn in_use_sentence(name: &str, state: &str) -> String {
    format!("Cannot update the attributes for AppBlockBuilder {name} when it is in {state} state. ")
}

/// AWS's `InvalidParameterCombinationException` sentence for a request that both sets an
/// attribute's configuration and names that attribute in `AttributesToDelete`; the
/// apostrophe is the right single quotation mark (U+2019) AWS's own sentence carries.
fn set_and_deleted_sentence(configuration: &str) -> String {
    format!("You can’t update and delete {configuration} at the same time.")
}

/// `AccessEndpoints` of `count` entries — 1 to 4 is the modelled length range of
/// `UpdateAppBlockBuilder.AccessEndpoints`.
fn access_endpoints(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({ "EndpointType": ENDPOINT_TYPE, "VpceId": format!("vpce-1{index}") }))
            .collect(),
    )
}

/// The create every row updates: a builder carrying a value for every member an update can
/// write, so a row that replaces one shows the replacement and a row that omits one shows
/// the stored value surviving.
fn fully_specified_create(name: &str) -> Value {
    json!({
        "Name": name,
        "Platform": PLATFORM,
        "InstanceType": INSTANCE_TYPES[0],
        "VpcConfig": {
            "SubnetIds": ["subnet-a1", "subnet-b2"],
            "SecurityGroupIds": ["sg-1", "sg-2"],
        },
        "Description": "the builder as created",
        "DisplayName": "As created",
        "EnableDefaultInternetAccess": false,
        "DisableIMDSV1": false,
        "IamRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/created"),
        "AccessEndpoints": access_endpoints(1),
    })
}

/// One update row: the members it carries beyond the `Name`, and whether those members
/// reach beyond the display members a running builder admits.
struct WriteRow {
    case: String,
    members: Value,
    beyond_display: bool,
}

/// Updates spanning the class: the rows differ along WHICH member is written (every member
/// an update can write, the display members on their own and every attribute at once), the
/// VALUE written (each instance type the model documents, both values of each boolean, a
/// VPC configuration with and without security groups, and every admissible number of
/// access endpoints), and whether the row reaches beyond the display members.
fn write_rows() -> Vec<WriteRow> {
    let mut rows = vec![
        WriteRow {
            case: "the display name alone".to_string(),
            members: json!({ "DisplayName": "Renamed" }),
            beyond_display: false,
        },
        WriteRow {
            case: "the description alone".to_string(),
            members: json!({ "Description": "a description an update wrote" }),
            beyond_display: false,
        },
        WriteRow {
            // `Description` and `DisplayName` are length 0..256 / 0..100, so the empty
            // string is a value an update accepts and must report back.
            case: "both display members, emptied".to_string(),
            members: json!({ "DisplayName": "", "Description": "" }),
            beyond_display: false,
        },
        WriteRow {
            case: "the platform".to_string(),
            members: json!({ "Platform": PLATFORM }),
            beyond_display: true,
        },
        WriteRow {
            case: "a VPC configuration naming subnets and security groups".to_string(),
            members: json!({
                "VpcConfig": {
                    "SubnetIds": ["subnet-y8", "subnet-z9"],
                    "SecurityGroupIds": ["sg-9"],
                },
            }),
            beyond_display: true,
        },
        WriteRow {
            case: "a VPC configuration naming subnets alone".to_string(),
            members: json!({ "VpcConfig": { "SubnetIds": ["subnet-y8", "subnet-z9"] } }),
            beyond_display: true,
        },
        WriteRow {
            case: "default internet access enabled".to_string(),
            members: json!({ "EnableDefaultInternetAccess": true }),
            beyond_display: true,
        },
        WriteRow {
            case: "IMDSv1 disabled".to_string(),
            members: json!({ "DisableIMDSV1": true }),
            beyond_display: true,
        },
        WriteRow {
            case: "another IAM role".to_string(),
            members: json!({ "IamRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/updated") }),
            beyond_display: true,
        },
        WriteRow {
            case: "every attribute at once".to_string(),
            members: json!({
                "DisplayName": "Everything",
                "Description": "every attribute written at once",
                "Platform": PLATFORM,
                "InstanceType": INSTANCE_TYPES[4],
                "VpcConfig": {
                    "SubnetIds": ["subnet-c3", "subnet-d4", "subnet-e5"],
                    "SecurityGroupIds": ["sg-7", "sg-8"],
                },
                "EnableDefaultInternetAccess": true,
                "DisableIMDSV1": true,
                "IamRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/everything"),
                "AccessEndpoints": access_endpoints(2),
            }),
            beyond_display: true,
        },
    ];

    // One row per instance type the model documents as available, so the catalog the rule
    // generalises over is covered rather than the one value a recording carried.
    for instance_type in INSTANCE_TYPES {
        rows.push(WriteRow {
            case: format!("the instance type {instance_type}"),
            members: json!({ "InstanceType": instance_type }),
            beyond_display: true,
        });
    }
    // One row per admissible `AccessEndpoints` length, so the count is covered at both
    // published bounds.
    for count in 1..=4_usize {
        rows.push(WriteRow {
            case: format!("{count} access endpoint(s)"),
            members: json!({ "AccessEndpoints": access_endpoints(count) }),
            beyond_display: true,
        });
    }
    rows
}

/// The request one row sends against the builder named `name`.
fn update_request(name: &str, members: &Value) -> Value {
    let mut request = members.clone();
    request["Name"] = json!(name);
    request
}

/// The `AppBlockBuilder` an update reported.
fn updated(case: &str, payload: &Value) -> Value {
    payload
        .get("AppBlockBuilder")
        .unwrap_or_else(|| panic!("case {case:?}: expected an AppBlockBuilder, got {payload}"))
        .clone()
}

/// Assert that `reported` carries every member the update wrote and, for every member it
/// did not write, the value the create persisted — and that the derived members the update
/// never writes are untouched.
fn assert_applies_update(case: &str, created: &Value, members: &Value, reported: &Value) {
    for member in ECHOED_MEMBERS {
        let expected = members.get(member).or_else(|| created.get(member));
        let actual = reported.get(member);
        assert_eq!(
            actual, expected,
            "case {case:?}: {member} must be reported as the update left it \
             ({expected:?}), got {actual:?}"
        );
    }
    for member in ["Arn", "State", "CreatedTime"] {
        let expected = created.get(member);
        let actual = reported.get(member);
        assert_eq!(
            actual, expected,
            "case {case:?}: an update must not move {member} ({expected:?}), got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_stopped_builder_admits_every_attribute_but_the_name() {
    // Rows: the Region the request's credential scope names — the Region whose ARN the
    // updated builder keeps — and, within each Region, the member and value written.
    for region in ["us-east-1", "eu-west-2"] {
        let emulator = Emulator::in_region(region);

        for (index, row) in write_rows().into_iter().enumerate() {
            let case = format!("{region} {}", row.case);
            let name = format!("builder-{index:02}");
            let created = create_builder(&emulator, &case, fully_specified_create(&name)).await;
            assert_eq!(
                created.get("State").and_then(Value::as_str),
                Some(STATE_STOPPED),
                "case {case:?}: a created builder must be {STATE_STOPPED}, got {created}"
            );

            let reported = updated(
                &case,
                &emulator
                    .ok(
                        &case,
                        "UpdateAppBlockBuilder",
                        update_request(&name, &row.members),
                    )
                    .await,
            );
            assert_applies_update(&case, &created, &row.members, &reported);

            // The update answers the stored record, so a read reports the same builder.
            let read = described(&emulator, &case, &name).await;
            assert_eq!(
                read, reported,
                "case {case:?}: the read must report the record the update wrote \
                 ({reported}), got {read}"
            );
        }
    }
}

#[tokio::test]
async fn attributes_to_delete_clears_the_attributes_it_names() {
    let emulator = Emulator::new();

    // Rows: each attribute the modelled enum names, alone and all three together — and,
    // for the security groups, a VPC configuration written in the same request that names
    // subnets alone, which asks nothing of the security groups being cleared.
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "the IAM role",
            json!({ "AttributesToDelete": [ATTRIBUTE_IAM_ROLE_ARN] }),
            vec![ATTRIBUTE_IAM_ROLE_ARN],
        ),
        (
            "the access endpoints",
            json!({ "AttributesToDelete": [ATTRIBUTE_ACCESS_ENDPOINTS] }),
            vec![ATTRIBUTE_ACCESS_ENDPOINTS],
        ),
        (
            "the VPC configuration's security groups",
            json!({ "AttributesToDelete": [ATTRIBUTE_SECURITY_GROUP_IDS] }),
            vec![ATTRIBUTE_SECURITY_GROUP_IDS],
        ),
        (
            "every attribute at once",
            json!({
                "AttributesToDelete": [
                    ATTRIBUTE_IAM_ROLE_ARN,
                    ATTRIBUTE_ACCESS_ENDPOINTS,
                    ATTRIBUTE_SECURITY_GROUP_IDS,
                ],
            }),
            vec![
                ATTRIBUTE_IAM_ROLE_ARN,
                ATTRIBUTE_ACCESS_ENDPOINTS,
                ATTRIBUTE_SECURITY_GROUP_IDS,
            ],
        ),
        (
            "the security groups beside a VPC configuration naming subnets alone",
            json!({
                "VpcConfig": { "SubnetIds": ["subnet-y8", "subnet-z9"] },
                "AttributesToDelete": [ATTRIBUTE_SECURITY_GROUP_IDS],
            }),
            vec![ATTRIBUTE_SECURITY_GROUP_IDS],
        ),
    ];

    for (index, (case, members, cleared)) in rows.into_iter().enumerate() {
        let name = format!("cleared-{index}");
        let created = create_builder(&emulator, case, fully_specified_create(&name)).await;
        let reported = updated(
            case,
            &emulator
                .ok(case, "UpdateAppBlockBuilder", update_request(&name, &members))
                .await,
        );

        for attribute in &cleared {
            match *attribute {
                ATTRIBUTE_IAM_ROLE_ARN => assert!(
                    reported.get("IamRoleArn").is_none(),
                    "case {case:?}: a cleared IAM role must be absent, got {reported}"
                ),
                ATTRIBUTE_ACCESS_ENDPOINTS => assert!(
                    reported.get("AccessEndpoints").is_none(),
                    "case {case:?}: cleared access endpoints must be absent, got {reported}"
                ),
                _ => {
                    let vpc_config = reported.get("VpcConfig");
                    // The subnets the builder runs in are the rest of that configuration
                    // and are NOT what the attribute names, so they survive the clearing:
                    // whichever subnets the request wrote, or the stored ones when it
                    // wrote none.
                    let subnets = members
                        .pointer("/VpcConfig/SubnetIds")
                        .or_else(|| created.pointer("/VpcConfig/SubnetIds"));
                    assert_eq!(
                        vpc_config,
                        Some(&json!({ "SubnetIds": subnets })),
                        "case {case:?}: only the security groups may be cleared, \
                         got {vpc_config:?}"
                    );
                }
            }
        }
        // Clearing one attribute leaves every other one alone.
        if !cleared.contains(&ATTRIBUTE_IAM_ROLE_ARN) {
            assert_eq!(
                reported.get("IamRoleArn"),
                created.get("IamRoleArn"),
                "case {case:?}: the IAM role must be left alone, got {reported}"
            );
        }
        if !cleared.contains(&ATTRIBUTE_ACCESS_ENDPOINTS) {
            assert_eq!(
                reported.get("AccessEndpoints"),
                created.get("AccessEndpoints"),
                "case {case:?}: the access endpoints must be left alone, got {reported}"
            );
        }

        let read = described(&emulator, case, &name).await;
        assert_eq!(
            read, reported,
            "case {case:?}: the read must report the record the update wrote ({reported}), \
             got {read}"
        );
    }
}

#[tokio::test]
async fn setting_an_attribute_and_deleting_it_in_one_request_is_refused() {
    let emulator = Emulator::new();

    // Rows: each attribute whose configuration an update member sets, with the sentence
    // AWS states for the configuration both halves name.
    let rows: Vec<(&str, Value, String)> = vec![
        (
            ATTRIBUTE_IAM_ROLE_ARN,
            json!({
                "IamRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/conflicting"),
                "AttributesToDelete": [ATTRIBUTE_IAM_ROLE_ARN],
            }),
            set_and_deleted_sentence("iam role arn"),
        ),
        (
            ATTRIBUTE_ACCESS_ENDPOINTS,
            json!({
                "AccessEndpoints": access_endpoints(2),
                "AttributesToDelete": [ATTRIBUTE_ACCESS_ENDPOINTS],
            }),
            set_and_deleted_sentence("vpc endpoints"),
        ),
        (
            ATTRIBUTE_SECURITY_GROUP_IDS,
            json!({
                "VpcConfig": { "SubnetIds": ["subnet-y8"], "SecurityGroupIds": ["sg-9"] },
                "AttributesToDelete": [ATTRIBUTE_SECURITY_GROUP_IDS],
            }),
            set_and_deleted_sentence("vpc configuration security group ids"),
        ),
    ];

    let name = "conflicted";
    let created = create_builder(&emulator, "setup", fully_specified_create(name)).await;

    for (case, members, expected) in rows {
        let message = refusal(
            &emulator,
            case,
            "UpdateAppBlockBuilder",
            update_request(name, &members),
            REFUSAL_STATUS,
            "InvalidParameterCombinationException",
        )
        .await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );

        // A refused update writes nothing: neither half of the combination was applied.
        let read = described(&emulator, case, name).await;
        assert_eq!(
            read, created,
            "case {case:?}: the stored builder must be untouched ({created}), got {read}"
        );
    }
}

#[tokio::test]
async fn a_running_builder_admits_its_display_members_alone() {
    let emulator = Emulator::new();
    let rows = write_rows();

    // Every builder is brought up before any update is sent, so the span the clock owes
    // them runs once; each row gets a builder of its own so a refused update is shown
    // against the builder it was sent for.
    let mut running: Vec<(String, String, Value, Value, bool)> = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let case = format!("running {}", row.case);
        let name = format!("running-{index:02}");
        create_builder(&emulator, &case, fully_specified_create(&name)).await;
        associate_app_block(&emulator, &case, &name, index).await;
        start(&emulator, &case, &name).await;
        running.push((case, name, row.members, Value::Null, row.beyond_display));
    }
    for (case, name, _, stored, _) in running.iter_mut() {
        await_state(&emulator, case, name, STATE_RUNNING).await;
        *stored = described(&emulator, case, name).await;
    }

    for (case, name, members, stored, beyond_display) in &running {
        if *beyond_display {
            let message = refusal(
                &emulator,
                case,
                "UpdateAppBlockBuilder",
                update_request(name, members),
                REFUSAL_STATUS,
                "ResourceInUseException",
            )
            .await;
            let expected = in_use_sentence(name, STATE_RUNNING);
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
            let read = described(&emulator, case, name).await;
            assert_eq!(
                &read, stored,
                "case {case:?}: a refused update must leave the stored builder untouched \
                 ({stored}), got {read}"
            );
        } else {
            let reported = updated(
                case,
                &emulator
                    .ok(case, "UpdateAppBlockBuilder", update_request(name, members))
                    .await,
            );
            assert_applies_update(case, stored, members, &reported);
            let read = described(&emulator, case, name).await;
            assert_eq!(
                read, reported,
                "case {case:?}: the read must report the record the update wrote \
                 ({reported}), got {read}"
            );
        }
    }

    // `AttributesToDelete` reaches beyond the display members too: every attribute it can
    // name is one of the instance the builder runs on.
    for (index, attribute) in [
        ATTRIBUTE_IAM_ROLE_ARN,
        ATTRIBUTE_ACCESS_ENDPOINTS,
        ATTRIBUTE_SECURITY_GROUP_IDS,
    ]
    .into_iter()
    .enumerate()
    {
        let (case, name, _, stored, _) = &running[index];
        let message = refusal(
            &emulator,
            case,
            "UpdateAppBlockBuilder",
            update_request(name, &json!({ "AttributesToDelete": [attribute] })),
            REFUSAL_STATUS,
            "ResourceInUseException",
        )
        .await;
        let expected = in_use_sentence(name, STATE_RUNNING);
        assert_eq!(
            message, expected,
            "case {case:?}: clearing {attribute} must be refused with {expected:?}, \
             got {message:?}"
        );
        let read = described(&emulator, case, name).await;
        assert_eq!(
            read.get("IamRoleArn"),
            stored.get("IamRoleArn"),
            "case {case:?}: a refused clearing must leave the IAM role alone, got {read}"
        );
    }
}

#[tokio::test]
async fn the_update_a_running_builder_refuses_applies_once_it_is_stopped() {
    let emulator = Emulator::new();

    // Rows: the instance type the builder runs on and the one the update resizes it to,
    // walking the whole catalog the model documents rather than the one pair a recording
    // carried.
    let mut rows: Vec<(String, String, String, Value)> = Vec::new();
    for (index, instance_type) in INSTANCE_TYPES.iter().enumerate() {
        let resized = INSTANCE_TYPES[(index + 1) % INSTANCE_TYPES.len()];
        let case = format!("{instance_type} resized to {resized}");
        let name = format!("resized-{index}");
        let mut create = fully_specified_create(&name);
        create["InstanceType"] = json!(instance_type);
        create_builder(&emulator, &case, create).await;
        associate_app_block(&emulator, &case, &name, index).await;
        start(&emulator, &case, &name).await;
        rows.push((case, name, (*resized).to_string(), Value::Null));
    }
    for (case, name, _, stored) in rows.iter_mut() {
        await_state(&emulator, case, name, STATE_RUNNING).await;
        *stored = described(&emulator, case, name).await;
    }

    // While the builder is running the resize is refused, and the builder keeps the
    // instance type it was created with.
    for (case, name, resized, stored) in &rows {
        let members = json!({ "InstanceType": resized });
        let message = refusal(
            &emulator,
            case,
            "UpdateAppBlockBuilder",
            update_request(name, &members),
            REFUSAL_STATUS,
            "ResourceInUseException",
        )
        .await;
        let expected = in_use_sentence(name, STATE_RUNNING);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
        let read = described(&emulator, case, name).await;
        assert_eq!(
            read.get("InstanceType"),
            stored.get("InstanceType"),
            "case {case:?}: the refused update must leave the instance type at its prior \
             value, got {read}"
        );
    }

    // Stopped, the identical update applies and reads back.
    for (case, name, _, _) in &rows {
        stop(&emulator, case, name).await;
    }
    for (case, name, _, _) in &rows {
        await_state(&emulator, case, name, STATE_STOPPED).await;
    }
    for (case, name, resized, stored) in &rows {
        let members = json!({ "InstanceType": resized });
        let reported = updated(
            case,
            &emulator
                .ok(case, "UpdateAppBlockBuilder", update_request(name, &members))
                .await,
        );
        assert_eq!(
            reported.get("InstanceType").and_then(Value::as_str),
            Some(resized.as_str()),
            "case {case:?}: the update must report the new instance type, got {reported}"
        );
        // Only the instance type moved: the stop moved the state, and the update moved
        // nothing else.
        let mut expected = stored.clone();
        expected["InstanceType"] = json!(resized);
        expected["State"] = json!(STATE_STOPPED);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected}, got {reported}"
        );

        let read = described(&emulator, case, name).await;
        assert_eq!(
            read, reported,
            "case {case:?}: the read must report the new instance type ({reported}), \
             got {read}"
        );
    }
}

#[tokio::test]
async fn a_builder_coming_up_or_going_down_admits_no_update() {
    // Rows: the transient state the builder is in — coming up and going down — and, for
    // each, an update of the display members a RUNNING builder would admit beside one that
    // reaches further. Each row gets a builder of its own, driven into its transient state
    // immediately before the update, so the window the clock owes it is the one under test.
    let transient_rows: Vec<(&str, Value)> = vec![
        ("the display members", json!({ "DisplayName": "Renamed" })),
        (
            "an attribute of the instance",
            json!({ "InstanceType": INSTANCE_TYPES[2] }),
        ),
    ];

    for state in [STATE_STARTING, STATE_STOPPING] {
        for (index, (members_case, members)) in transient_rows.iter().enumerate() {
            let emulator = Emulator::new();
            let case = format!("{state} builder, {members_case}");
            let name = format!("transient-{index}");
            create_builder(&emulator, &case, fully_specified_create(&name)).await;
            associate_app_block(&emulator, &case, &name, index).await;

            // Driven into the state under test: a start leaves the builder coming up, and
            // a stop of a running builder leaves it going down.
            if state == STATE_STARTING {
                start(&emulator, &case, &name).await;
            } else {
                start(&emulator, &case, &name).await;
                await_state(&emulator, &case, &name, STATE_RUNNING).await;
                stop(&emulator, &case, &name).await;
            }
            let stored = described(&emulator, &case, &name).await;
            assert_eq!(
                stored.get("State").and_then(Value::as_str),
                Some(state),
                "case {case:?}: the builder must be {state} for this row, got {stored}"
            );

            let message = refusal(
                &emulator,
                &case,
                "UpdateAppBlockBuilder",
                update_request(&name, members),
                REFUSAL_STATUS,
                "ResourceInUseException",
            )
            .await;
            let expected = in_use_sentence(&name, state);
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
            let read = described(&emulator, &case, &name).await;
            assert_only_the_state_moved(&case, &stored, &read);

            // Once the clock has settled the builder the same update applies, so what
            // refused it was the transient state and nothing else.
            let settled = if state == STATE_STARTING {
                STATE_RUNNING
            } else {
                STATE_STOPPED
            };
            await_state(&emulator, &case, &name, settled).await;
            if settled == STATE_STOPPED || members_case == &"the display members" {
                let applied = emulator
                    .ok(
                        &case,
                        "UpdateAppBlockBuilder",
                        update_request(&name, members),
                    )
                    .await;
                let applied = updated(&case, &applied);
                let (member, value) = members
                    .as_object()
                    .and_then(|members| members.iter().next())
                    .expect("the row writes one member");
                assert_eq!(
                    applied.get(member),
                    Some(value),
                    "case {case:?}: a settled builder must admit {member}, got {applied}"
                );
            }
            let state_after = reported_state(&emulator, &case, &name).await;
            assert_eq!(
                state_after, settled,
                "case {case:?}: expected {settled}, got {state_after:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_of_a_name_the_account_holds_no_builder_under_is_refused() {
    let emulator = Emulator::new();
    // One builder the account does hold, so the refusals below are about the name asked
    // for rather than about an empty store, and a fleet of one of the names below: a
    // builder is a resource family of its own.
    let stored = create_builder(
        &emulator,
        "setup",
        fully_specified_create("stored-builder"),
    )
    .await;
    emulator
        .ok(
            "setup",
            "CreateFleet",
            json!({ "Name": "builder", "InstanceType": INSTANCE_TYPES[0] }),
        )
        .await;

    for name in names() {
        // Rows: the name's form, and what the refused update asked to write — a display
        // member, an attribute of the instance, and a clearing.
        for members in [
            json!({ "DisplayName": "nobody applies this" }),
            json!({ "InstanceType": INSTANCE_TYPES[3] }),
            json!({ "AttributesToDelete": [ATTRIBUTE_IAM_ROLE_ARN] }),
        ] {
            let case = format!("absent {name:?} with {members}");
            let message = refusal(
                &emulator,
                &case,
                "UpdateAppBlockBuilder",
                update_request(&name, &members),
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
    }

    // The refused updates wrote nothing: the one builder the account holds is as it was.
    let read = described(&emulator, "stored-builder", "stored-builder").await;
    assert_eq!(
        read, stored,
        "case \"stored-builder\": expected {stored}, got {read}"
    );
}
