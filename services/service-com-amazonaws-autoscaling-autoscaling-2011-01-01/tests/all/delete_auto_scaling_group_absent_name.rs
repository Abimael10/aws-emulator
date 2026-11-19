// SPDX-License-Identifier: CC0-1.0

//! What `DeleteAutoScalingGroup` answers for an `AutoScalingGroupName` the account holds no
//! group under, over the wire.
//!
//! RULE under test: `AutoScalingGroupName` names the group the call removes, so the name is
//! resolved against the groups the account HOLDS and a name no group answers to is refused —
//! the Query protocol's `ValidationError` at HTTP 400, carrying the fixed
//! `AutoScalingGroup name not found - ` text and, as this operation's own detail, the name it
//! was asked for in quotes. A delete is therefore not idempotent in its name: a group this
//! account has already deleted is a name that names nothing, and the second delete is refused
//! exactly as a name nothing was ever created under is.
//!
//! The rows span what the rule generalises over: HOW the name comes to name nothing (never
//! created, created and deleted, created and deleted with `ForceDelete`), whether the second
//! delete repeats the first call's `ForceDelete`, and the FORM of the name (a plain name, a
//! name with the separators a group name admits, the longest name this suite writes). A group
//! that IS held stands beside every row and is deleted successfully, so no row passes because
//! the operation refuses everything.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_launch_templates, tag_value, tag_values,
    xml_unescape, REGION,
};

/// The launch template the groups of this suite launch from.
const TEMPLATE: &str = "lt-delete-absent";

/// The fixed text every refusal of a name this service holds no group under starts with.
/// Written out here rather than read off the code under test: the WORDING is the contract.
const ABSENT_GROUP_PREFIX: &str = "AutoScalingGroup name not found - ";

/// The whole sentence this operation answers, whose one variable part is the name the request
/// gave, quoted.
fn absent_group_message(name: &str) -> String {
    format!("{ABSENT_GROUP_PREFIX}AutoScalingGroup '{name}' not found")
}

/// One emulator with Auto Scaling and the account's own Amazon EC2 store behind it, and the
/// launch template this suite's groups launch from already in it.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = new_engine();
    seed_launch_templates(&engine, &[TEMPLATE]);
    (router_with(Arc::clone(&engine)), engine)
}

/// Create the group `name`, with no instances, and assert the create was served.
async fn create(router: &MultiServiceRouter, name: &str) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
        p("AvailabilityZones.member.1", format!("{REGION}a")),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the group must be created, got {status}: {body}"
    );
}

/// Ask for the delete of `name`, forcing it when `force`.
async fn delete(router: &MultiServiceRouter, name: &str, force: bool) -> (u16, String) {
    let mut params = vec![("AutoScalingGroupName", name)];
    if force {
        params.push(("ForceDelete", "true"));
    }
    query(router, "DeleteAutoScalingGroup", &params).await
}

/// Assert `body` is this operation's absent-group refusal for `name`.
fn assert_absent_group(case: &str, status: u16, body: &str, name: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status}: {body}"
    );
    assert_eq!(
        tag_value(body, "Code"),
        "ValidationError",
        "case {case:?}: expected a ValidationError, got {body}"
    );
    assert_eq!(
        xml_unescape(&tag_value(body, "Message")),
        absent_group_message(name),
        "case {case:?}: expected the absent-group sentence naming {name:?}, got {body}"
    );
}

/// Whether the account holds a group under `name`, read back through the service's own report.
async fn group_exists(router: &MultiServiceRouter, name: &str) -> bool {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups must answer 200, got {status}: {body}"
    );
    !tag_values(&body, "AutoScalingGroupName").is_empty()
}

/// RULE: a name the account holds no group under is refused, however it came to name nothing —
/// never created, or created and then deleted.
#[tokio::test]
async fn a_delete_of_a_name_no_group_answers_to_is_refused() {
    // (case, the name, whether a group is created and deleted first, whether the first delete
    // forces, whether the asserted delete forces)
    let rows: &[(&str, &str, bool, bool, bool)] = &[
        ("a name nothing was created under", "asg-never", false, false, false),
        (
            "a name nothing was created under, forced",
            "asg-never-forced",
            false,
            false,
            true,
        ),
        (
            "a name with the separators a group name admits",
            "asg.probe_name-0",
            false,
            false,
            false,
        ),
        (
            "the name of a group this account deleted",
            "asg-deleted",
            true,
            false,
            false,
        ),
        (
            "the name of a group this account force-deleted",
            "asg-force-deleted",
            true,
            true,
            false,
        ),
        (
            "the name of a deleted group, asked again with ForceDelete",
            "asg-deleted-then-forced",
            true,
            false,
            true,
        ),
    ];

    let (router, _engine) = emulator();
    // The control: a group that stays held, so a refusal below is about the name it carries.
    create(&router, "asg-held").await;

    for (case, name, created_first, force_first, force_second) in rows {
        if *created_first {
            create(&router, name).await;
            let (status, body) = delete(&router, name, *force_first).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the first delete removes the group, got {status}: {body}"
            );
            assert!(
                !group_exists(&router, name).await,
                "case {case:?}: the group must be gone after the first delete"
            );
        }
        let (status, body) = delete(&router, name, *force_second).await;
        assert_absent_group(case, status, &body, name);
    }

    // The control is still held and its own delete is served, so the refusals above are the
    // name resolving to nothing rather than the operation refusing every call.
    assert!(
        group_exists(&router, "asg-held").await,
        "case \"the held group\": expected the refusals to remove nothing"
    );
    let (status, body) = delete(&router, "asg-held", false).await;
    assert_eq!(
        status, 200,
        "case \"the held group\": its own delete must be served, got {status}: {body}"
    );
    assert!(
        !group_exists(&router, "asg-held").await,
        "case \"the held group\": expected it to be gone once its delete was served"
    );
    // ... and asking for it again is the same refusal every other absent name earns.
    let (status, body) = delete(&router, "asg-held", false).await;
    assert_absent_group("the held group, deleted twice", status, &body, "asg-held");
}
