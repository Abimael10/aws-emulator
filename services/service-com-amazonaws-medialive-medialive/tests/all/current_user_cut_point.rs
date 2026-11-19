// SPDX-License-Identifier: CC0-1.0

//! The ONE cut point this service's derived "currently using" lists apply, end
//! to end over the restJson1 wire.
//!
//! ACCEPTING a resource's delete is what releases everything it held, so from
//! that instant it stops being reported as a current user of what it referenced
//! — in the derived member itself, and in the `State`, the output type and the
//! in-use refusal derived from that member — and it stays out once the clock has
//! settled it into its terminal deleted state.
//!
//! Every test drives the service the way a client does — through
//! `MultiServiceRouter::dispatch` with real HTTP requests — so what is asserted
//! is the emulator's wire behaviour, not an internal function's return value.
//!
//! The suites are tables: the issue's input-security-group case is ONE row of
//! each. The wire harness (`medialive`, `call`) is the crate's shared one in
//! `wire`, the resource setup is `support`'s and the clock wait is the channel
//! suite's — reused, not copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_lifecycle::wait_for_the_clock;
use crate::wire::{call, medialive};

/// The input device the device row claims. A device id is the Link unit's own,
/// which is why `ClaimDevice` takes it rather than minting one.
const DEVICE_ID: &str = "hd-cutpoint00001";

/// The kind of resource the row deletes: the one that USES the resource under
/// test.
#[derive(Clone, Copy, Debug)]
enum User {
    Input,
    Channel,
}

/// The resource whose derived list of current users the row reads.
#[derive(Clone, Copy, Debug)]
enum Referenced {
    SecurityGroup,
    SdiSource,
    InputDevice,
}

/// One row's resources and expectations, as its setup left them.
struct Scenario {
    /// The id of the resource whose derived list is under test.
    id: String,
    /// The read that reports that list.
    uri: String,
    /// The output member the read wraps its resource in, where it wraps one
    /// (`DescribeInputSecurityGroup`'s output is flat, `DescribeSdiSource`'s
    /// wraps its resource).
    wrapper: Option<&'static str>,
    /// The derived member under test.
    member: &'static str,
    /// What that member reports while the user is one this account holds and may
    /// still use.
    entry: Value,
    /// What it reports once no such user is left.
    empty: Value,
    /// The member derived FROM that list: its name, its value while such a user
    /// is left and its value once none is.
    derived: (&'static str, Value, Value),
    /// The id of the resource that uses it, as the refusal naming it spells it.
    user_id: String,
    /// The user's own path — the DELETE that takes it out of the account, and
    /// the GET that reports the state that delete left it in.
    user_uri: String,
}

/// An input created from `body`, with the resources its configuration names put
/// in the account first (`crate::support`).
async fn create_input(router: &MultiServiceRouter, case: &str, body: Value) -> Value {
    let body = crate::support::resolved_input_body(router, body).await;
    let (status, created) = call(router, "POST", "/prod/inputs", Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateInput: expected 201, got {status}: {created}"
    );
    created["input"].clone()
}

/// A channel created from `body` and left in `IDLE`, the only state a delete
/// accepts.
async fn idle_channel(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let body = crate::support::resolved_channel_body(router, body).await;
    let (status, created) = call(router, "POST", "/prod/channels", Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateChannel: expected 201, got {status}: {created}"
    );
    let id = created["channel"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    wait_for_the_clock();
    let state = resource_state(router, case, &format!("/prod/channels/{id}")).await;
    assert_eq!(
        state, "IDLE",
        "case {case:?}: expected the created channel to settle to IDLE, got {state:?}"
    );
    id
}

/// The `State` the resource at `uri` reports right now.
async fn resource_state(router: &MultiServiceRouter, case: &str, uri: &str) -> String {
    let (status, read) = call(router, "GET", uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GET {uri}: expected 200, got {status}: {read}"
    );
    read["state"].as_str().unwrap_or_default().to_string()
}

/// The resource whose derived list is under test, as it reads right now.
async fn referenced_resource(router: &MultiServiceRouter, case: &str, scenario: &Scenario) -> Value {
    let (status, read) = call(router, "GET", &scenario.uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GET {}: expected 200, got {status}: {read}",
        scenario.uri
    );
    scenario
        .wrapper
        .map_or(read.clone(), |name| read[name].clone())
}

/// Put one row's pair of resources in the account: the resource under test and
/// the resource that uses it.
async fn scenario(
    router: &MultiServiceRouter,
    case: &str,
    referenced: Referenced,
    user: User,
) -> Scenario {
    match referenced {
        // `InputSecurityGroup$Inputs` is "The list of inputs currently using
        // this Input Security Group" and `$Channels` "The list of channels
        // currently using this Input Security Group as their channel security
        // group", and `$State` is derived from the two.
        Referenced::SecurityGroup => {
            let id = crate::support::input_security_group(router).await;
            let (member, user_id, user_uri) = match user {
                User::Input => {
                    let input = create_input(
                        router,
                        case,
                        json!({
                            "name": "group-user",
                            "type": "RTP_PUSH",
                            "inputSecurityGroups": [id.clone()],
                        }),
                    )
                    .await;
                    let input_id = input["id"].as_str().unwrap_or_default().to_string();
                    let uri = format!("/prod/inputs/{input_id}");
                    ("inputs", input_id, uri)
                }
                User::Channel => {
                    let channel_id = idle_channel(
                        router,
                        case,
                        json!({
                            "name": "group-user",
                            "channelSecurityGroups": [id.clone()],
                        }),
                    )
                    .await;
                    let uri = format!("/prod/channels/{channel_id}");
                    ("channels", channel_id, uri)
                }
            };
            Scenario {
                uri: format!("/prod/inputSecurityGroups/{id}"),
                id,
                wrapper: None,
                member,
                entry: json!([user_id.clone()]),
                empty: json!([]),
                derived: ("state", json!("IN_USE"), json!("IDLE")),
                user_id,
                user_uri,
            }
        }
        // `SdiSource$Inputs` is "The list of inputs that are currently using
        // this SDI source", and `$State` is "whether the SDI source is attached
        // to an SDI input (IN_USE) or not (IDLE)".
        Referenced::SdiSource => {
            let id = crate::support::sdi_source(router, "cut-point-source").await;
            let input = create_input(
                router,
                case,
                json!({
                    "name": "source-user",
                    "type": "SDI",
                    "sdiSources": [id.clone()],
                }),
            )
            .await;
            let user_id = input["id"].as_str().unwrap_or_default().to_string();
            Scenario {
                uri: format!("/prod/sdiSources/{id}"),
                id,
                wrapper: Some("sdiSource"),
                member: "inputs",
                entry: json!([user_id.clone()]),
                empty: json!([]),
                derived: ("state", json!("IN_USE"), json!("IDLE")),
                user_uri: format!("/prod/inputs/{user_id}"),
                user_id,
            }
        }
        // `InputDevice$MedialiveInputArns` is "An array of the ARNs for the
        // MediaLive inputs attached to the device", and `$OutputType` is
        // "The output attachment type of the input device", MEDIALIVE_INPUT
        // exactly while such an input is attached.
        Referenced::InputDevice => {
            let id = crate::support::claimed_device(router, DEVICE_ID).await;
            let input = create_input(
                router,
                case,
                json!({
                    "name": "device-user",
                    "type": "INPUT_DEVICE",
                    "inputDevices": [{ "id": id.clone() }],
                }),
            )
            .await;
            let user_id = input["id"].as_str().unwrap_or_default().to_string();
            let arn = input["arn"].clone();
            Scenario {
                uri: format!("/prod/inputDevices/{id}"),
                id,
                wrapper: None,
                member: "medialiveInputArns",
                entry: json!([arn]),
                // A device no MediaLive input is attached to reports no ARNs at
                // all, because the member belongs to the MEDIALIVE_INPUT
                // output type it no longer has.
                empty: Value::Null,
                derived: (
                    "outputType",
                    json!("MEDIALIVE_INPUT"),
                    json!("NONE"),
                ),
                user_uri: format!("/prod/inputs/{user_id}"),
                user_id,
            }
        }
    }
}

/// `BatchDelete` with `body`, answering its two result lists as
/// `(successful, failed)`.
async fn batch_delete(
    router: &MultiServiceRouter,
    case: &str,
    body: Value,
) -> (Vec<Value>, Vec<Value>) {
    let (status, response) = call(router, "POST", "/prod/batch/delete", Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: BatchDelete: expected 200, got {status}: {response}"
    );
    (
        response["successful"].as_array().cloned().unwrap_or_default(),
        response["failed"].as_array().cloned().unwrap_or_default(),
    )
}

/// Rule: a resource is a CURRENT user of what it references only while its
/// delete has not been accepted, so it leaves the derived list of users — and
/// the member derived from that list — the instant the delete is accepted, in
/// the transient `DELETING` step as much as in the terminal state the clock
/// settles it into.
///
/// The rows span what the rule generalises over: the kind of resource that does
/// the using (an input, a channel) and the kind of resource whose derived list
/// is read (an input security group, through both of its user members; an SDI
/// source; a claimed input device). Each row reads the list in all three phases
/// — while the user is live, inside the `DELETING` window and once the clock has
/// settled it — because the rule is about WHERE between them the cut falls.
#[tokio::test]
async fn a_user_leaves_a_derived_list_once_its_delete_is_accepted() {
    // (case, the resource whose derived list is read, the resource using it)
    let table: [(&str, Referenced, User); 4] = [
        (
            "an input using an input security group",
            Referenced::SecurityGroup,
            User::Input,
        ),
        (
            "a channel using an input security group",
            Referenced::SecurityGroup,
            User::Channel,
        ),
        ("an input using an sdi source", Referenced::SdiSource, User::Input),
        (
            "an input attached to an input device",
            Referenced::InputDevice,
            User::Input,
        ),
    ];

    for (case, referenced, user) in table {
        let router = medialive();
        let scenario = scenario(&router, case, referenced, user).await;
        let (derived, in_use, unused) = scenario.derived.clone();

        let before = referenced_resource(&router, case, &scenario).await;
        assert_eq!(
            before[scenario.member], scenario.entry,
            "case {case:?}: expected {} to report the live user, got {:?}",
            scenario.member, before[scenario.member]
        );
        assert_eq!(
            before[derived], in_use,
            "case {case:?}: expected {derived} to report the live user, got {:?}",
            before[derived]
        );

        let (status, deleted) = call(&router, "DELETE", &scenario.user_uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DELETE {}: expected 200, got {status}: {deleted}",
            scenario.user_uri
        );
        let deleting = resource_state(&router, case, &scenario.user_uri).await;
        assert_eq!(
            deleting, "DELETING",
            "case {case:?}: expected the accepted delete to open the transient step, got {deleting:?}"
        );

        // The delete has been accepted, so the user has already released what it
        // referenced: the derived list drops it inside the `DELETING` window.
        let during = referenced_resource(&router, case, &scenario).await;
        assert_eq!(
            during[scenario.member], scenario.empty,
            "case {case:?}: expected {} to drop a user whose delete was accepted, got {:?}",
            scenario.member, during[scenario.member]
        );
        assert_eq!(
            during[derived], unused,
            "case {case:?}: expected {derived} to report no user once the delete was accepted, \
             got {:?}",
            during[derived]
        );

        wait_for_the_clock();
        let settled = resource_state(&router, case, &scenario.user_uri).await;
        assert_eq!(
            settled, "DELETED",
            "case {case:?}: expected the clock to settle the user into its terminal state, got {settled:?}"
        );
        let after = referenced_resource(&router, case, &scenario).await;
        assert_eq!(
            after[scenario.member], scenario.empty,
            "case {case:?}: expected {} to keep a settled user out, got {:?}",
            scenario.member, after[scenario.member]
        );
        assert_eq!(
            after[derived], unused,
            "case {case:?}: expected {derived} to report no user left, got {:?}",
            after[derived]
        );
        assert_eq!(
            scenario.id,
            referenced_resource(&router, case, &scenario).await["id"]
                .as_str()
                .unwrap_or_default(),
            "case {case:?}: expected the reads to be of the resource the setup created"
        );
    }
}

/// Rule: `BatchDelete`'s per-group delete is judged on the same pair of derived
/// lists `DescribeInputSecurityGroup` reports, so a group a LIVE resource holds
/// is refused with the operation's per-resource `BadRequestException` naming
/// that user, and the delete is accepted as soon as that user's own delete has
/// been accepted — inside the transient `DELETING` window, without waiting for
/// the clock.
///
/// The rows span the two kinds of user a group has — an input, through
/// `InputSecurityGroup$Inputs`, and a channel, through `$Channels` — because
/// one situation cannot have two answers depending on which kind the last user
/// is.
#[tokio::test]
async fn a_group_is_refused_while_a_live_user_holds_it_for_either_kind() {
    // (case, the kind of resource using the group)
    let table: [(&str, User); 2] = [
        ("an input using the group", User::Input),
        ("a channel using the group", User::Channel),
    ];

    for (case, user) in table {
        let router = medialive();
        let scenario = scenario(&router, case, Referenced::SecurityGroup, user).await;
        let group_id = scenario.id.clone();

        let (successful, failed) = batch_delete(
            &router,
            case,
            json!({ "inputSecurityGroupIds": [group_id.clone()] }),
        )
        .await;
        assert!(
            successful.is_empty(),
            "case {case:?}: expected no accepted delete while a live user holds the group, got {successful:?}"
        );
        assert_eq!(
            failed.len(),
            1,
            "case {case:?}: expected exactly one refusal, got {failed:?}"
        );
        assert_eq!(
            failed[0]["code"],
            json!("BadRequestException"),
            "case {case:?}: expected BadRequestException, got {:?}",
            failed[0]
        );
        assert_eq!(
            failed[0]["message"],
            json!(format!(
                "Input security group {group_id} is in use by {} and cannot be deleted",
                scenario.user_id
            )),
            "case {case:?}: expected the refusal to name the group and its user, got {:?}",
            failed[0]
        );
        let kept = resource_state(&router, case, &scenario.uri).await;
        assert_eq!(
            kept, "IN_USE",
            "case {case:?}: expected the refused delete to leave the group in use, got {kept:?}"
        );

        // Accepting the user's delete releases the group, so the same request is
        // accepted while that user is still only `DELETING`.
        let (status, deleted) = call(&router, "DELETE", &scenario.user_uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DELETE {}: expected 200, got {status}: {deleted}",
            scenario.user_uri
        );
        let deleting = resource_state(&router, case, &scenario.user_uri).await;
        assert_eq!(
            deleting, "DELETING",
            "case {case:?}: expected the accepted delete to open the transient step, got {deleting:?}"
        );

        let (successful, failed) = batch_delete(
            &router,
            case,
            json!({ "inputSecurityGroupIds": [group_id.clone()] }),
        )
        .await;
        assert!(
            failed.is_empty(),
            "case {case:?}: expected no refusal once the user's delete was accepted, got {failed:?}"
        );
        assert_eq!(
            successful.len(),
            1,
            "case {case:?}: expected the delete to be accepted, got {successful:?}"
        );
        assert_eq!(
            successful[0]["state"],
            json!("DELETED"),
            "case {case:?}: expected the accepted delete to report DELETED, got {:?}",
            successful[0]
        );
    }
}

/// Rule: `DeleteInputSecurityGroup` answers with the group's USAGE state — the
/// one derived from the same pair of lists `DescribeInputSecurityGroup` reports
/// — so a group whose last user's delete has already been accepted is deleted as
/// an unused group: the response names no user at all and reports `IDLE`, from
/// the instant that user's delete was accepted rather than one transient span
/// later. This is the teardown order a client cleaning up runs: delete what used
/// the group, then delete the group.
///
/// The rows span the two kinds of user a group has (an input, through
/// `InputSecurityGroup$Inputs`; a channel, through `$Channels`) and both phases
/// of the user's accepted delete, because the answer cannot depend on either.
#[tokio::test]
async fn deleting_a_group_right_after_its_last_user_reports_no_user_and_idle() {
    // (case, the kind of resource using the group, whether the clock settles the
    //  user's delete before the group is deleted)
    let table: [(&str, User, bool); 4] = [
        ("an input, inside the DELETING window", User::Input, false),
        ("an input, once the delete settled", User::Input, true),
        ("a channel, inside the DELETING window", User::Channel, false),
        ("a channel, once the delete settled", User::Channel, true),
    ];

    for (case, user, settle) in table {
        let router = medialive();
        let scenario = scenario(&router, case, Referenced::SecurityGroup, user).await;

        let (status, deleted_user) = call(&router, "DELETE", &scenario.user_uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DELETE {}: expected 200, got {status}: {deleted_user}",
            scenario.user_uri
        );
        if settle {
            wait_for_the_clock();
        }

        let (status, deleted) = crate::input_security_group::delete_group(&router, &scenario.id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteInputSecurityGroup: expected 200, got {status}: {deleted}"
        );
        for member in ["inputs", "channels"] {
            assert_eq!(
                deleted[member],
                json!([]),
                "case {case:?}: expected the deleted group to name no {member}, got {:?}",
                deleted[member]
            );
        }
        assert_eq!(
            deleted["state"],
            json!("IDLE"),
            "case {case:?}: expected the group to be deleted as an unused group, got {:?}",
            deleted["state"]
        );
    }
}

/// The ids one derived member of a group projection carries, sorted so a
/// comparison is about membership rather than about the store's iteration order.
fn member_ids(case: &str, phase: &str, reader: &str, member: &str, group: &Value) -> Vec<String> {
    let entries = group[member].as_array().unwrap_or_else(|| {
        panic!("case {case:?} / {phase}: expected {reader} to report a {member} list, got {group}")
    });
    let mut ids: Vec<String> = entries
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_string())
        .collect();
    ids.sort();
    ids
}

/// The same ids, sorted, for the side the test built up itself.
fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// The group one id names, as `ListInputSecurityGroups` reports it (GET
/// /prod/inputSecurityGroups, `ListInputSecurityGroupsResponse$InputSecurityGroups`,
/// jsonName `inputSecurityGroups`).
async fn listed_group(router: &MultiServiceRouter, case: &str, phase: &str, id: &str) -> Value {
    let (status, listing) = call(router, "GET", "/prod/inputSecurityGroups", None).await;
    assert_eq!(
        status, 200,
        "case {case:?} / {phase}: ListInputSecurityGroups: expected 200, got {status}: {listing}"
    );
    listing["inputSecurityGroups"]
        .as_array()
        .and_then(|groups| {
            groups
                .iter()
                .find(|group| group["id"] == json!(id))
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!("case {case:?} / {phase}: expected the listing to report the group, got {listing}")
        })
}

/// What a group reports as its current users right now — its `inputs`, its
/// `channels` and the `state` derived from the two — read through BOTH
/// serializers that emit those members and asserted to agree, since one
/// derivation backs them.
async fn membership(
    router: &MultiServiceRouter,
    case: &str,
    phase: &str,
    id: &str,
) -> (Vec<String>, Vec<String>, String) {
    let (status, described) = call(
        router,
        "GET",
        &format!("/prod/inputSecurityGroups/{id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?} / {phase}: DescribeInputSecurityGroup: expected 200, got {status}: {described}"
    );
    let listed = listed_group(router, case, phase, id).await;

    let inputs = member_ids(case, phase, "DescribeInputSecurityGroup", "inputs", &described);
    let channels = member_ids(
        case,
        phase,
        "DescribeInputSecurityGroup",
        "channels",
        &described,
    );
    let state = described["state"].as_str().unwrap_or_default().to_string();
    assert_eq!(
        member_ids(case, phase, "ListInputSecurityGroups", "inputs", &listed),
        inputs,
        "case {case:?} / {phase}: expected both readers to report the same inputs, \
         got {listed} beside {described}"
    );
    assert_eq!(
        member_ids(case, phase, "ListInputSecurityGroups", "channels", &listed),
        channels,
        "case {case:?} / {phase}: expected both readers to report the same channels, \
         got {listed} beside {described}"
    );
    assert_eq!(
        listed["state"].as_str().unwrap_or_default(),
        state,
        "case {case:?} / {phase}: expected both readers to report the same state, \
         got {listed} beside {described}"
    );
    (inputs, channels, state)
}

/// Rule: a group's membership is derived from the resources that CURRENTLY use
/// it, so each accepted delete takes exactly that one resource out of the member
/// that lists it and the `State` derived from the pair is `IN_USE` exactly while
/// either list is non-empty — IDLE once the last user's delete is accepted,
/// whatever reader emits the projection and however many users the group had.
///
/// The rows differ along what the rule generalises over: HOW MANY resources use
/// the group (one, two, three), and WHICH KINDS use it — inputs alone, or an
/// input beside a channel, where deleting the input empties `inputs` while
/// `channels` keeps the group `IN_USE`. The projection is read through both
/// serializers that emit those members, because one derivation backs them. No
/// read waits on the clock: the cut falls where the delete is ACCEPTED, so a
/// deleted user is out of the membership before its terminal state is due.
#[tokio::test]
async fn a_group_reports_the_users_left_after_each_accepted_delete() {
    // (case, how many inputs name the group, whether a channel also names it)
    let table: [(&str, usize, bool); 4] = [
        ("one input", 1, false),
        ("two inputs", 2, false),
        ("three inputs", 3, false),
        ("one input beside a channel", 1, true),
    ];

    for (case, input_count, with_channel) in table {
        let router = medialive();
        let group_id = crate::support::input_security_group(&router).await;

        let mut input_ids: Vec<String> = Vec::new();
        for index in 0..input_count {
            let input = create_input(
                &router,
                case,
                json!({
                    "name": format!("group-user-{index}"),
                    "type": "RTP_PUSH",
                    "inputSecurityGroups": [group_id.clone()],
                }),
            )
            .await;
            input_ids.push(input["id"].as_str().unwrap_or_default().to_string());
        }
        // The channel row's channel is created through the same helper the rows
        // above use, so it is settled and its own delete is accepted when the
        // teardown reaches it.
        let mut channel_ids: Vec<String> = if with_channel {
            vec![
                idle_channel(
                    &router,
                    case,
                    json!({
                        "name": "group-user-channel",
                        "channelSecurityGroups": [group_id.clone()],
                    }),
                )
                .await,
            ]
        } else {
            Vec::new()
        };

        let mut live = sorted(input_ids.clone());
        let (inputs, channels, state) = membership(&router, case, "every user live", &group_id).await;
        assert_eq!(
            inputs, live,
            "case {case:?}: expected the group to report every live input, got {inputs:?}"
        );
        assert_eq!(
            channels,
            sorted(channel_ids.clone()),
            "case {case:?}: expected the group to report every live channel, got {channels:?}"
        );
        assert_eq!(
            state, "IN_USE",
            "case {case:?}: expected IN_USE while a user holds the group, got {state:?}"
        );

        // One accepted delete at a time: the group drops that input and keeps
        // every other user, and its state follows the two lists.
        for (index, input_id) in input_ids.iter().enumerate() {
            let (status, deleted) = call(
                &router,
                "DELETE",
                &format!("/prod/inputs/{input_id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteInput: expected 200, got {status}: {deleted}"
            );
            live.retain(|id| id != input_id);

            let phase = format!("after {} of {input_count} input delete(s)", index + 1);
            let (inputs, channels, state) = membership(&router, case, &phase, &group_id).await;
            assert_eq!(
                inputs, live,
                "case {case:?} / {phase}: expected only the inputs left to be reported, \
                 got {inputs:?}"
            );
            assert_eq!(
                channels,
                sorted(channel_ids.clone()),
                "case {case:?} / {phase}: expected an input delete to leave the channels alone, \
                 got {channels:?}"
            );
            let expected = if live.is_empty() && channel_ids.is_empty() {
                "IDLE"
            } else {
                "IN_USE"
            };
            assert_eq!(
                state, expected,
                "case {case:?} / {phase}: expected {expected} with {} input(s) and {} channel(s) \
                 left, got {state:?}",
                live.len(),
                channel_ids.len()
            );
        }

        // The other kind of user goes the same way: deleting the channel empties
        // the second list too, so the group reports no user at all.
        for channel_id in channel_ids.clone() {
            let (status, deleted) = call(
                &router,
                "DELETE",
                &format!("/prod/channels/{channel_id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteChannel: expected 200, got {status}: {deleted}"
            );
            channel_ids.retain(|id| id != &channel_id);

            let phase = "after the channel delete";
            let (inputs, channels, state) = membership(&router, case, phase, &group_id).await;
            assert_eq!(
                channels,
                sorted(channel_ids.clone()),
                "case {case:?} / {phase}: expected only the channels left to be reported, \
                 got {channels:?}"
            );
            assert_eq!(
                inputs, live,
                "case {case:?} / {phase}: expected a channel delete to leave the inputs alone, \
                 got {inputs:?}"
            );
            let expected = if live.is_empty() && channel_ids.is_empty() {
                "IDLE"
            } else {
                "IN_USE"
            };
            assert_eq!(
                state, expected,
                "case {case:?} / {phase}: expected {expected} with {} input(s) and {} channel(s) \
                 left, got {state:?}",
                live.len(),
                channel_ids.len()
            );
        }

        // The teardown this unblocks, reached by every row: once no user of
        // either kind is left the group is released, and the delete reports it as
        // the unused group it has become.
        let (status, deleted) = crate::input_security_group::delete_group(&router, &group_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteInputSecurityGroup: expected 200, got {status}: {deleted}"
        );
        for member in ["inputs", "channels"] {
            assert_eq!(
                deleted[member],
                json!([]),
                "case {case:?}: expected the released group to name no {member}, got {:?}",
                deleted[member]
            );
        }
        assert_eq!(
            deleted["state"],
            json!("IDLE"),
            "case {case:?}: expected the released group to report IDLE, got {:?}",
            deleted["state"]
        );
    }
}
