// SPDX-License-Identifier: CC0-1.0

//! The platform word an INSTANCE reports, and the filter that selects on it.
//!
//! The rule under test: "the platform. This value is `windows` for Windows instances;
//! otherwise, it is empty" (`Instance$Platform`). The value an instance reports for the
//! member is the LOWER-CASE word that sentence publishes — the same word the operation's own
//! filter documents, "platform - the platform. To list only Windows instances, use
//! `windows`" (`DescribeInstancesRequest$Filters`) — whatever spelling the AMI the instance
//! was launched from carries for its own member (the modelled `PlatformValues` spells its
//! enum member `Windows`). An instance launched from an AMI with no platform at all reports
//! no platform, which is the "otherwise" half.
//!
//! The rows are taken from the catalog the service itself publishes: the suite asks
//! `DescribeImages` which images carry a platform and which do not, so neither the AMI ids
//! nor the platform words are written into the table.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{IMAGE_PLATFORM_WINDOWS, INSTANCE_PLATFORM_WINDOWS};

use super::ec2_query::{call, element, filters, indexed};
use super::wire::{child, child_text, document};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One call that must be served, as its body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// An image the catalog publishes WITH the platform member, and one published without it:
/// `(the Windows image's id, an image that carries no platform)`.
async fn published_images(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let body = ok(router, case, "DescribeImages", &[]).await;
    let parsed = document(case, &body);
    let images = parsed
        .root_element()
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "imagesSet")
        .map(|set| {
            set.children()
                .filter(roxmltree::Node::is_element)
                .filter_map(|image| {
                    let image_id = child_text(image, "imageId")?;
                    Some((image_id, child_text(image, "platform")))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let windows = images
        .iter()
        .find(|(_, platform)| platform.as_deref() == Some(IMAGE_PLATFORM_WINDOWS))
        .map(|(image_id, _)| image_id.clone())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the catalog must publish a Windows image, got {body}")
        });
    let other = images
        .iter()
        .find(|(_, platform)| platform.is_none())
        .map(|(image_id, _)| image_id.clone())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the catalog must publish an image with no platform, got {body}")
        });
    (windows, other)
}

/// Launch one instance from `image_id` and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, image_id: &str) -> String {
    let body = ok(
        router,
        case,
        "RunInstances",
        &[
            p("ImageId", image_id),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ],
    )
    .await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: expected an instanceId in {body}"))
}

/// The platform `DescribeInstances` reports for `instance_id` — `None` when the instance
/// reports no such member.
async fn reported_platform(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Option<String> {
    let body = ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no instance in {body}"));
    child_text(instance, "platform")
}

/// The instance ids `DescribeInstances` reports for `params`.
async fn selected(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> Vec<String> {
    let body = ok(router, case, "DescribeInstances", params).await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .filter_map(|instance| child_text(instance, "instanceId"))
        .collect()
}

/// An instance reports the platform word its member publishes, and only for an AMI that
/// carries a platform at all.
#[tokio::test]
async fn an_instance_reports_the_platform_word_its_member_publishes() {
    let case = "the published catalog";
    let (router, _engine) = super::ec2_query::emulator();
    let (windows_image, other_image) = published_images(&router, case).await;

    // (case, the AMI the launch names, the platform the instance must report)
    let rows: [(&str, &str, Option<&str>); 2] = [
        (
            "an instance launched from a Windows AMI",
            &windows_image,
            Some(INSTANCE_PLATFORM_WINDOWS),
        ),
        (
            "an instance launched from an AMI carrying no platform",
            &other_image,
            None,
        ),
    ];
    let mut launched: Vec<(&str, String)> = Vec::new();
    for (case, image_id, expected) in rows {
        let instance_id = launch(&router, case, image_id).await;
        let reported = reported_platform(&router, case, &instance_id).await;
        assert_eq!(
            reported.as_deref(),
            expected,
            "case {case:?}: expected the platform {expected:?}, got {reported:?}"
        );
        launched.push((case, instance_id));
    }

    // The filter selects on the value the instance REPORTS, and the spelling the modelled
    // enum carries selects it too — the way `DescribeImages` answers the same filter — so a
    // caller that took the word off an image's own report still finds its instances.
    let windows_instance = launched[0].1.clone();
    for spelling in [INSTANCE_PLATFORM_WINDOWS, IMAGE_PLATFORM_WINDOWS] {
        let case = format!("the platform filter spelled {spelling:?}");
        let found = selected(&router, &case, &filters(&[("platform", &[spelling])])).await;
        assert_eq!(
            found,
            vec![windows_instance.clone()],
            "case {case:?}: expected only the Windows instance, got {found:?}"
        );
    }

    // And a value no instance's platform matches selects none, which is a normal answer
    // rather than a refusal.
    let case = "a platform no instance runs";
    let found = selected(&router, case, &filters(&[("platform", &["linux"])])).await;
    assert!(
        found.is_empty(),
        "case {case:?}: expected no instance, got {found:?}"
    );
}
