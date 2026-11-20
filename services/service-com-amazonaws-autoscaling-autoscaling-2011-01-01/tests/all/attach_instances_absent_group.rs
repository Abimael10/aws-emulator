// SPDX-License-Identifier: CC0-1.0

//! How `AttachInstances` reports a group the account holds nothing for, over the awsQuery
//! wire.
//!
//! The rule under test: the instances an attach names are taken INTO the group the request
//! names, so that group is resolved first — a name the account holds no group for is
//! refused `ValidationError` (HTTP 400) with this operation's own absent-group sentence,
//! whose detail labels the resource and carries the name THE REQUEST asked for, read from
//! the request and nothing else. The detail is never a placeholder and never another
//! operation's clause: two attaches naming two different missing groups answer two
//! different sentences.
//!
//! The rows therefore span what the rule generalises over — the name's length, letter
//! case, character set, separators and spelling (including a name spelled like a part of
//! the sentence itself and one spelled like the token an unfilled detail would travel as),
//! how the name comes to hold no group (never created, created and deleted, a live group's
//! name in another letter case, a live group's name extended), and how many instance ids
//! the call carries (none, one, and the most one call may name) — and each asserts the
//! WHOLE message, so no row can pass by refusing for another reason. A positive control
//! attaches against a group that EXISTS, so a blanket refusal cannot pass either.

use smithy_server_runtime::MultiServiceRouter;

use crate::attach_instances::absent_group_message;
use crate::support::{
    borrowed, emulator, new_engine, p, query, router_with, seed_subnets, tag_value, xml_unescape,
};

/// The launch source every group here is created from — a launch TEMPLATE the account
/// already holds, because no account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Post one `AttachInstances` and answer the status, the error type, the wire code and the
/// unescaped message.
async fn attach(
    router: &MultiServiceRouter,
    group: &str,
    ids: &[String],
) -> (u16, String, String, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, id) in ids.iter().enumerate() {
        params.push(p(&format!("InstanceIds.member.{}", index + 1), id.clone()));
    }
    let (status, body) = query(router, "AttachInstances", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Type"),
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// A well-formed 17-hexadecimal-digit instance id no instance of the account has, built
/// from `seed` so each row's ids are its own.
fn fabricated_id(seed: usize) -> String {
    format!("i-0fff{seed:013x}")
}

/// The instance-id sets one call may carry, from none at all to the most a single call may
/// name (`InstanceIds` — "You can specify up to 20 instances",
/// `smithy_query(query="operation",operation="AttachInstances")`), so no row trips the
/// per-call bound instead of the group lookup.
fn id_sets() -> Vec<(&'static str, Vec<String>)> {
    vec![
        ("no instance id at all", Vec::new()),
        ("one instance id", vec![fabricated_id(1)]),
        (
            "three instance ids",
            (2..5).map(fabricated_id).collect::<Vec<_>>(),
        ),
        (
            "the most instance ids one call may name",
            (10..30).map(fabricated_id).collect::<Vec<_>>(),
        ),
    ]
}

/// Group names a client can submit, differing in length, character set, case, separators
/// and spelling. `AutoScalingGroupName` is `XmlStringMaxLen255` (len 1..255, pattern
/// `^[\u{0020}-\u{D7FF}\u{E000}-\u{FFFD}\u{D800}\u{DC00}-\u{DBFF}\u{DFFF}\r\n\t]*$`,
/// `smithy_query(query="operation",operation="AttachInstances")`), so the boundary accepts
/// every row here and only the handler can refuse it.
fn absent_group_names() -> Vec<(&'static str, String)> {
    let mut names: Vec<(&'static str, String)> = [
        ("the shortest name the model allows", "a"),
        ("a name of the shape a probe uses", "probe-asg-0123456789ab"),
        ("an upper-case name", "ASG-UPPER-CASE"),
        (
            "a name carrying every separator the model allows",
            "asg.name_with-separators 1",
        ),
        (
            "a name spelled like the token an unfilled detail travels as",
            "null",
        ),
        (
            "a name spelled like a part of the detail sentence",
            "AutoScalingGroup",
        ),
        (
            "a name carrying XML metacharacters",
            "<asg & 'name' \"quoted\">",
        ),
        ("a non-ASCII name", "グループ-ünïcode"),
        ("a name that is only digits", "1234567890123456"),
    ]
    .into_iter()
    .map(|(case, name)| (case, name.to_string()))
    .collect();
    // The longest name the member's length constraint admits.
    names.push(("the longest name the model allows", "z".repeat(255)));
    names
}

/// Every name, crossed with every instance-id set: HTTP 400, a `Sender`
/// `ValidationError`, and this operation's absent-group sentence carrying the name the
/// request asked for.
#[tokio::test]
async fn an_attach_of_an_absent_group_names_the_group_the_request_asked_for() {
    let router = router_with(new_engine());

    for (name_case, name) in absent_group_names() {
        for (ids_case, ids) in id_sets() {
            let case = format!("{name_case} with {ids_case}");
            let (status, error_type, code, message) = attach(&router, &name, &ids).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {message:?}"
            );
            assert_eq!(
                (error_type.as_str(), code.as_str()),
                ("Sender", "ValidationError"),
                "case {case:?}: expected a Sender ValidationError, got {error_type:?} {code:?} \
                 with {message:?}"
            );
            // The WHOLE message, not a substring: a name spelled like a part of the
            // sentence would satisfy a `contains` check even against a handler that had
            // stopped naming the group.
            let expected = absent_group_message(&name);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
    }
}

/// Two attaches naming two different missing groups answer two DIFFERENT sentences: the
/// name is read from each request rather than being fixed text of the message.
#[tokio::test]
async fn two_missing_groups_are_reported_by_their_own_names() {
    let router = router_with(new_engine());

    let first = "asg-missing-one";
    let second = "asg-missing-two";
    let (_, _, _, first_message) = attach(&router, first, &[fabricated_id(7)]).await;
    let (_, _, _, second_message) = attach(&router, second, &[fabricated_id(7)]).await;
    assert_ne!(
        first_message, second_message,
        "case \"two missing groups\": the two refusals must differ, both read \
         {first_message:?}"
    );
    assert_eq!(
        (first_message.as_str(), second_message.as_str()),
        (
            absent_group_message(first).as_str(),
            absent_group_message(second).as_str()
        ),
        "case \"two missing groups\": each refusal must name its own group, got \
         {first_message:?} and {second_message:?}"
    );
}

/// A group that EXISTS is resolved — the refusal did not swallow the attach path, and a
/// request against it is refused for its INSTANCES rather than for the group — while a
/// name that only looks like it, and the name once the group is deleted, are refused by
/// the name the request carried.
#[tokio::test]
async fn a_stored_group_is_resolved_and_a_name_it_does_not_answer_to_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let home = subnets.first().expect("one seeded subnet").clone();

    let live = "asg-attach-live";
    let params = vec![
        p("AutoScalingGroupName", live),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "2"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", home.zone.clone()),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {live:?}: expected HTTP 200, got {status} with {body:?}"
    );

    // The positive control: the group is there, so the refusal is about the INSTANCE the
    // request named and not about the group.
    let unknown = fabricated_id(42);
    let (status, _, code, message) = attach(&router, live, std::slice::from_ref(&unknown)).await;
    assert_eq!(
        status, 400,
        "case \"a stored group named with an unknown instance\": expected HTTP 400, got \
         {status} with {message:?}"
    );
    assert_eq!(
        code, "ValidationError",
        "case \"a stored group named with an unknown instance\": expected the \
         ValidationError code, got {code:?} with {message:?}"
    );
    assert!(
        message.contains(&unknown) && !message.starts_with("AutoScalingGroup name not found"),
        "case \"a stored group named with an unknown instance\": the refusal must be about \
         {unknown:?}, got {message:?}"
    );

    // Names the live group does not answer to: a group is resolved by its WHOLE name.
    for (case, name) in [
        ("the live group's name in another letter case", live.to_uppercase()),
        ("the live group's name extended", format!("{live}-extra")),
        ("the live group's name with trailing space", format!("{live} ")),
    ] {
        let (status, _, code, message) = attach(&router, &name, &[fabricated_id(43)]).await;
        let expected = absent_group_message(&name);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "ValidationError", expected.as_str()),
            "case {case:?}: expected HTTP 400 ValidationError {expected:?}, got {status} \
             {code:?} {message:?}"
        );
    }

    // And once the group is deleted its own name is refused the same way, naming it.
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", live), ("ForceDelete", "true")],
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting {live:?} must succeed, got {status} with {body:?}"
    );
    let (status, _, code, message) = attach(&router, live, &[fabricated_id(44)]).await;
    let expected = absent_group_message(live);
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, "ValidationError", expected.as_str()),
        "case \"the deleted group's own name\": expected HTTP 400 ValidationError \
         {expected:?}, got {status} {code:?} {message:?}"
    );
}
