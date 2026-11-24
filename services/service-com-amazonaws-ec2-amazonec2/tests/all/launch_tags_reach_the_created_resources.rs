// SPDX-License-Identifier: CC0-1.0

//! The tags a LAUNCH asks for on the resources it CREATES reach those resources — end to
//! end over the wire the service speaks (`ec2Query`, through the shared
//! [`super::ec2_query`] harness).
//!
//! The rule under test: **"the tags to apply to the resources that are created during
//! instance launch" (`RunInstancesRequest$TagSpecifications`, and the same member on the
//! launch-template version a launch reads) are applied PER RESOURCE TYPE to the resources of
//! that type the launch creates** — the instance, the EBS root volume it is given and the
//! network interfaces created for it — so each of those resources carries exactly its own
//! entries' tags, and every reader of that resource's tags reports them: its own describe and
//! the account's tag report. An interface the launch only ATTACHES is not a resource the
//! launch created, so its tags are left as they were.
//!
//! The rows span what the rule generalises over: which documented type an entry names, how
//! many entries one launch carries, how many tags each entry carries, an empty tag value, how
//! many instances the launch mints, whether the entries arrive on the request or on the
//! launch-template version the launch reads, and whether an interface is one the launch
//! created or one it attached. No row is the specification: every expectation is read back out
//! of the emulator's own state.
//!
//! [`super::run_instances_tag_specifications`] drives the same member from the INSTANCE's
//! side — which entries are served and which refused, and that no other type's tags reach the
//! instance; here the subject is the resources beside the instance.

use std::collections::BTreeMap;

use ec2::state::MAX_TAGS_PER_RESOURCE;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{
    call, element, elements, error_code, filters, indexed, tag_report, tag_specifications,
};
use super::wire::{child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The image every launch here names, in the long id form this service accepts.
const AMI: &str = "ami-0123456789abcdef0";

/// One `(name, value)` request parameter.
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

/// Launch `count` instances with `extra` beside the AMI, as the launch's body.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    count: usize,
    extra: &[(String, String)],
) -> String {
    let mut params = vec![
        p("ImageId", AMI),
        p("MinCount", &count.to_string()),
        p("MaxCount", &count.to_string()),
    ];
    params.extend_from_slice(extra);
    ok(router, case, "RunInstances", &params).await
}

/// The tags one reported resource carries: the `(key, value)` pairs of its own `tagSet`.
fn tags_of(item: roxmltree::Node<'_, '_>) -> BTreeMap<String, String> {
    set_items(item, "tagSet")
        .into_iter()
        .map(|tag| {
            (
                child_text(tag, "key").unwrap_or_default(),
                child_text(tag, "value").unwrap_or_default(),
            )
        })
        .collect()
}

/// The `(id -> tags)` map a report carries: the entries of its `<{set}>` list, each keyed by
/// the `<{id}>` member it reports.
///
/// Read off the TREE so each entry's own `tagSet` stays with it: a volume item carries an
/// attachment set of its own beside its tags.
fn reported_tag_sets(
    case: &str,
    body: &str,
    set: &str,
    id: &str,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let parsed = document(case, body);
    let Some(list) = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == set)
    else {
        return BTreeMap::new();
    };
    list.children()
        .filter(roxmltree::Node::is_element)
        .filter_map(|item| Some((child_text(item, id)?, tags_of(item))))
        .collect()
}

/// The `(volume id -> tags)` map `DescribeVolumes` reports for the volumes attached to
/// `instance_id`.
async fn volumes_of(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let body = ok(
        router,
        case,
        "DescribeVolumes",
        &filters(&[("attachment.instance-id", &[instance_id])]),
    )
    .await;
    reported_tag_sets(case, &body, "volumeSet", "volumeId")
}

/// The `(interface id -> tags)` map `DescribeNetworkInterfaces` reports for the interfaces
/// attached to `instance_id`.
async fn interfaces_of(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let body = ok(
        router,
        case,
        "DescribeNetworkInterfaces",
        &filters(&[("attachment.instance-id", &[instance_id])]),
    )
    .await;
    reported_tag_sets(case, &body, "networkInterfaceSet", "networkInterfaceId")
}

/// The tags `DescribeInstances` reports for `instance_id`.
async fn instance_tags_of(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let body = ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await;
    reported_tag_sets(case, &body, "instancesSet", "instanceId")
}

/// The one entry of a `(id -> tags)` map, or a panic naming the case: every row below creates
/// exactly one resource of each kind per instance.
fn only_entry(
    case: &str,
    kind: &str,
    reported: &BTreeMap<String, BTreeMap<String, String>>,
) -> (String, BTreeMap<String, String>) {
    let [(id, tags)] = reported.iter().collect::<Vec<_>>()[..] else {
        panic!("case {case:?}: expected exactly one {kind}, got {reported:?}");
    };
    (id.clone(), tags.clone())
}

/// A declared tag set as the map the assertions compare against.
fn expected(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// One row of the table: the specifications a launch carries, how many instances it mints,
/// and the tags each kind of resource it creates must end up with.
struct Row {
    /// What makes this row different from the others.
    case: &'static str,
    /// How many instances the launch mints.
    count: usize,
    /// The `TagSpecifications` entries the launch carries, by resource type.
    entries: Vec<(&'static str, Vec<(&'static str, &'static str)>)>,
    /// The tags the instance must report.
    instance: Vec<(&'static str, &'static str)>,
    /// The tags its EBS root volume must report.
    volume: Vec<(&'static str, &'static str)>,
    /// The tags the interface created for it must report.
    interface: Vec<(&'static str, &'static str)>,
}

/// THE RULE: each resource a launch creates carries the tags of ITS OWN type's entries, and
/// every reader of that resource reports them.
#[tokio::test]
async fn each_resource_a_launch_creates_carries_its_own_tag_specification() {
    let rows = vec![
        Row {
            case: "no specification at all",
            count: 1,
            entries: Vec::new(),
            instance: Vec::new(),
            volume: Vec::new(),
            interface: Vec::new(),
        },
        Row {
            case: "a volume entry alone",
            count: 1,
            entries: vec![("volume", vec![("Name", "the-root-volume")])],
            instance: Vec::new(),
            volume: vec![("Name", "the-root-volume")],
            interface: Vec::new(),
        },
        Row {
            case: "a network-interface entry alone, with two tags",
            count: 1,
            entries: vec![(
                "network-interface",
                vec![("Name", "the-primary-eni"), ("tier", "web")],
            )],
            instance: Vec::new(),
            volume: Vec::new(),
            interface: vec![("Name", "the-primary-eni"), ("tier", "web")],
        },
        Row {
            case: "all three types at once, the volume entry carrying an empty value",
            count: 1,
            entries: vec![
                ("instance", vec![("Name", "the-instance")]),
                ("volume", vec![("Name", "the-volume"), ("owner", "")]),
                ("network-interface", vec![("Name", "the-eni")]),
            ],
            instance: vec![("Name", "the-instance")],
            volume: vec![("Name", "the-volume"), ("owner", "")],
            interface: vec![("Name", "the-eni")],
        },
        Row {
            case: "the volume entry after the instance entry, over three instances",
            count: 3,
            entries: vec![
                ("network-interface", vec![("Name", "every-eni")]),
                ("instance", vec![("Name", "every-instance")]),
                ("volume", vec![("Name", "every-volume")]),
            ],
            instance: vec![("Name", "every-instance")],
            volume: vec![("Name", "every-volume")],
            interface: vec![("Name", "every-eni")],
        },
        Row {
            case: "a Spot request entry beside a volume entry",
            count: 1,
            entries: vec![
                ("spot-instances-request", vec![("Name", "no-such-request")]),
                ("volume", vec![("Name", "still-the-volume")]),
            ],
            instance: Vec::new(),
            volume: vec![("Name", "still-the-volume")],
            interface: Vec::new(),
        },
    ];

    for row in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let case = row.case;
        let entries: Vec<(&str, &[(&str, &str)])> = row
            .entries
            .iter()
            .map(|(resource_type, tags)| (*resource_type, tags.as_slice()))
            .collect();
        let body = launch(&router, case, row.count, &tag_specifications(&entries)).await;
        let launched = elements(&body, "instanceId");
        assert_eq!(
            launched.len(),
            row.count,
            "case {case:?}: expected {} launched instances, got {launched:?}",
            row.count
        );

        for instance_id in &launched {
            let reported = instance_tags_of(&router, case, instance_id).await;
            let (_, instance_tags) = only_entry(case, "instance", &reported);
            assert_eq!(
                instance_tags,
                expected(&row.instance),
                "case {case:?}: expected {instance_id} to carry {:?}, got {instance_tags:?}",
                expected(&row.instance)
            );

            let volumes = volumes_of(&router, case, instance_id).await;
            let (volume_id, volume_tags) = only_entry(case, "root volume", &volumes);
            assert_eq!(
                volume_tags,
                expected(&row.volume),
                "case {case:?}: expected the volume {volume_id} of {instance_id} to carry \
                 {:?}, got {volume_tags:?}",
                expected(&row.volume)
            );

            let interfaces = interfaces_of(&router, case, instance_id).await;
            let (interface_id, interface_tags) = only_entry(case, "network interface", &interfaces);
            assert_eq!(
                interface_tags,
                expected(&row.interface),
                "case {case:?}: expected the interface {interface_id} of {instance_id} to \
                 carry {:?}, got {interface_tags:?}",
                expected(&row.interface)
            );

            // The account's own tag report is the other reader of those tags, and the one
            // the tagging index projects: it reports each resource under the kind it is
            // reported as, with the same pairs its describe carried.
            for (resource_id, resource_type, tags) in [
                (&volume_id, "volume", &volume_tags),
                (&interface_id, "network-interface", &interface_tags),
            ] {
                let reported: BTreeMap<String, String> =
                    tag_report(&router, REGION, case, resource_id)
                        .await
                        .into_iter()
                        .map(|(reported_id, reported_type, key, value)| {
                            assert_eq!(
                                (reported_id.as_str(), reported_type.as_str()),
                                (resource_id.as_str(), resource_type),
                                "case {case:?}: DescribeTags must report {resource_id} as a \
                                 {resource_type}, got {reported_id} as a {reported_type}"
                            );
                            (key, value)
                        })
                        .collect();
                assert_eq!(
                    &reported, tags,
                    "case {case:?}: expected DescribeTags to report {tags:?} for \
                     {resource_id}, got {reported:?}"
                );
            }
        }
    }
}

/// THE RULE's bound: a launch may ask for as many tags per resource type as a resource holds
/// ([`MAX_TAGS_PER_RESOURCE`], "Each resource can have up to 50 tags"), and one past that is
/// refused with EC2's own tag-limit code, having launched nothing.
#[tokio::test]
async fn a_launch_is_served_at_the_tag_count_bound_and_refused_one_past_it() {
    // The bound comes from the crate's own constant, not from a number written here, so a
    // change to it is covered rather than contradicted.
    let at_the_bound: Vec<(String, String)> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|nth| (format!("key-{nth}"), format!("value-{nth}")))
        .collect();
    let one_past: Vec<(String, String)> = (0..=MAX_TAGS_PER_RESOURCE)
        .map(|nth| (format!("key-{nth}"), format!("value-{nth}")))
        .collect();

    for (case, tags, served) in [
        (
            format!("{MAX_TAGS_PER_RESOURCE} volume tags"),
            at_the_bound,
            true,
        ),
        (
            format!("{} volume tags", MAX_TAGS_PER_RESOURCE + 1),
            one_past,
            false,
        ),
    ] {
        let (router, _engine) = super::ec2_query::emulator();
        let borrowed: Vec<(&str, &str)> = tags
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let params = tag_specifications(&[("volume", &borrowed)]);
        let mut request = vec![
            p("ImageId", AMI),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ];
        request.extend(params);
        let (status, body) = call(&router, REGION, "RunInstances", &request).await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            let instances = elements(&body, "instanceId");
            let [instance_id] = instances.as_slice() else {
                panic!("case {case:?}: expected one launched instance, got {instances:?}");
            };
            let (_, volume_tags) = only_entry(
                &case,
                "root volume",
                &volumes_of(&router, &case, instance_id).await,
            );
            assert_eq!(
                volume_tags.len(),
                MAX_TAGS_PER_RESOURCE,
                "case {case:?}: expected the volume to carry every tag the launch asked for, \
                 got {volume_tags:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the launch to be refused, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "TagLimitExceeded",
                "case {case:?}: expected EC2's tag-limit code, got {body}"
            );
            let stored = ok(&router, &case, "DescribeInstances", &[]).await;
            assert!(
                elements(&stored, "instanceId").is_empty(),
                "case {case:?}: a refused launch must store nothing, got {stored}"
            );
        }
    }
}

/// THE RULE over EVERY volume a launch creates: a launch whose own block device mapping names
/// a second device creates a second volume, and the `volume` entries tag that one too.
#[tokio::test]
async fn the_volume_entries_tag_every_volume_the_launch_creates() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "a launch whose mapping names a data device beside the root one";
    let mut params = vec![
        p("BlockDeviceMapping.1.DeviceName", "/dev/sdb"),
        p("BlockDeviceMapping.1.Ebs.VolumeSize", "4"),
    ];
    params.extend(tag_specifications(&[(
        "volume",
        &[("Name", "every-volume-of-the-launch")],
    )]));
    let body = launch(&router, case, 1, &params).await;
    let instances = elements(&body, "instanceId");
    let [instance_id] = instances.as_slice() else {
        panic!("case {case:?}: expected exactly one launched instance, got {instances:?}");
    };

    let volumes = volumes_of(&router, case, instance_id).await;
    assert_eq!(
        volumes.len(),
        2,
        "case {case:?}: expected {instance_id} to hold the root volume and the data volume, \
         got {volumes:?}"
    );
    for (volume_id, tags) in &volumes {
        assert_eq!(
            tags,
            &expected(&[("Name", "every-volume-of-the-launch")]),
            "case {case:?}: expected the volume {volume_id} to carry the launch's volume \
             tags, got {tags:?}"
        );
    }
}

/// THE RULE, reached from the other source of a launch's specifications: a launch-template
/// version's entries tag the resources the launch made from it creates.
#[tokio::test]
async fn a_launch_template_versions_entries_tag_the_resources_its_launch_creates() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "a launch from a template that tags its volume and its interface";
    let mut params = vec![
        p("LaunchTemplateName", "tags-the-resources-it-creates"),
        p("LaunchTemplateData.ImageId", AMI),
    ];
    for (name, value) in tag_specifications(&[
        ("instance", &[("Name", "from-the-template")]),
        ("volume", &[("Name", "the-templates-volume")]),
        ("network-interface", &[("Name", "the-templates-eni")]),
    ]) {
        params.push(p(&format!("LaunchTemplateData.{name}"), &value));
    }
    let created = ok(&router, case, "CreateLaunchTemplate", &params).await;
    let template_id = element(&created, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: no <launchTemplateId> in {created}"));

    let body = launch(
        &router,
        case,
        1,
        &[p("LaunchTemplate.LaunchTemplateId", &template_id)],
    )
    .await;
    let instances = elements(&body, "instanceId");
    let [instance_id] = instances.as_slice() else {
        panic!("case {case:?}: expected exactly one launched instance, got {instances:?}");
    };

    let (_, volume_tags) = only_entry(
        case,
        "root volume",
        &volumes_of(&router, case, instance_id).await,
    );
    assert_eq!(
        volume_tags,
        expected(&[("Name", "the-templates-volume")]),
        "case {case:?}: expected the volume of {instance_id} to carry the template's volume \
         tag, got {volume_tags:?}"
    );
    let (_, interface_tags) = only_entry(
        case,
        "network interface",
        &interfaces_of(&router, case, instance_id).await,
    );
    assert_eq!(
        interface_tags,
        expected(&[("Name", "the-templates-eni")]),
        "case {case:?}: expected the interface of {instance_id} to carry the template's \
         interface tag, got {interface_tags:?}"
    );
}

/// THE RULE's boundary: an interface the launch CREATED carries the launch's interface tags,
/// and one the launch only ATTACHED keeps the tags it already had.
#[tokio::test]
async fn a_launch_tags_the_interface_it_created_and_not_the_one_it_attached() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "a launch that attaches one interface and creates another";
    let vpc = ok(
        &router,
        case,
        "CreateVpc",
        &[p("CidrBlock", "10.71.0.0/16")],
    )
    .await;
    let vpc_id = element(&vpc, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId"));
    let subnet = ok(
        &router,
        case,
        "CreateSubnet",
        &[
            p("VpcId", &vpc_id),
            p("CidrBlock", "10.71.1.0/24"),
            p("AvailabilityZone", &format!("{REGION}a")),
        ],
    )
    .await;
    let subnet_id =
        element(&subnet, "subnetId").unwrap_or_else(|| panic!("case {case:?}: no subnetId"));

    // An interface that exists BEFORE the launch, carrying a tag of its own.
    let mut interface_params = vec![p("SubnetId", &subnet_id)];
    interface_params.extend(tag_specifications(&[(
        "network-interface",
        &[("Role", "created-before-the-launch")],
    )]));
    let created = ok(
        &router,
        case,
        "CreateNetworkInterface",
        &interface_params,
    )
    .await;
    let attached_id = element(&created, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: no networkInterfaceId in {created}"));

    let mut params = vec![
        p("NetworkInterface.1.DeviceIndex", "0"),
        p("NetworkInterface.1.NetworkInterfaceId", &attached_id),
        p("NetworkInterface.2.DeviceIndex", "1"),
        p("NetworkInterface.2.SubnetId", &subnet_id),
    ];
    params.extend(tag_specifications(&[(
        "network-interface",
        &[("Role", "created-by-the-launch")],
    )]));
    let body = launch(&router, case, 1, &params).await;
    let instances = elements(&body, "instanceId");
    let [instance_id] = instances.as_slice() else {
        panic!("case {case:?}: expected exactly one launched instance, got {instances:?}");
    };

    let interfaces = interfaces_of(&router, case, instance_id).await;
    assert_eq!(
        interfaces.len(),
        2,
        "case {case:?}: expected {instance_id} to hold both interfaces, got {interfaces:?}"
    );
    let attached = interfaces
        .get(&attached_id)
        .unwrap_or_else(|| panic!("case {case:?}: {attached_id} must be attached: {interfaces:?}"));
    assert_eq!(
        attached,
        &expected(&[("Role", "created-before-the-launch")]),
        "case {case:?}: the launch must leave the tags of the interface it only attached \
         alone, got {attached:?}"
    );
    for (interface_id, tags) in &interfaces {
        if interface_id == &attached_id {
            continue;
        }
        assert_eq!(
            tags,
            &expected(&[("Role", "created-by-the-launch")]),
            "case {case:?}: expected the interface {interface_id} the launch created to carry \
             the launch's interface tags, got {tags:?}"
        );
    }
}
