// SPDX-License-Identifier: CC0-1.0

//! The two parameter branches a `RunInstances` client pins a launch's NETWORK with, and the
//! sentences an absent launch template is reported in — driven over the wire the service
//! speaks (`ec2Query`) through the crate's shared [`super::ec2_query`] harness.
//!
//! THE RULES:
//!
//! 1. A launch that carries a network interface specification is SERVED. The model binds
//!    `RunInstancesRequest$NetworkInterfaces` as a list of
//!    `InstanceNetworkInterfaceSpecification` (`DeviceIndex`, `SubnetId`, `Groups`,
//!    `AssociatePublicIpAddress`, `PrivateIpAddress`, `Description`, `InterfaceType`,
//!    `NetworkInterfaceId`, …) and declares no error for naming one, so every entry becomes an
//!    interface of every instance the launch mints — created in the subnet the entry names
//!    with the groups, address and description it names, or, for an entry naming an interface
//!    by id, that existing interface attached where it is — each at its own device index, and
//!    the entry at device index 0 is the instance's primary interface. Interface and instance
//!    are one record, so `DescribeNetworkInterfaces` reports the same interface the launch
//!    did.
//! 2. A launch that pins a launch template VERSION the template does not hold is refused
//!    `InvalidLaunchTemplateId.VersionNotFound` in the sentence EC2 words for a version
//!    RESOLUTION: fixed text around the version the request named and the id of the template
//!    the account holds, closed by a period.
//! 3. Which sentence an ABSENT launch template is reported in follows the FORM of the member
//!    that named it: an operation whose member names ONE id states that id, and only the
//!    operation whose member takes a LIST of ids states the plural sentence, which names none
//!    of the request's values. The two are never interchanged — a client that reads the
//!    message can only tell "this template is gone" from "one of the ids I sent is gone" if
//!    each form keeps its own sentence.
//!
//! The rows span what each rule generalises over: rule 1 over how many entries a launch
//! names, how many instances it mints, which subnet an entry names, how many groups it
//! applies, whether it names its own address, description and type, whether it asks for a
//! public address, and whether the interface is created by the launch or named by id; rule 2
//! over how far past the template's versions the request reaches and over which operation
//! resolves the version; rule 3 over the operation and the member form that named the key. No
//! row is the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch names: `RunInstances` only has to accept the id, and which image an
/// instance was launched from has no bearing on its interfaces.
const AMI: &str = "ami-0123456789abcdef0";

/// The network this suite launches into, outside the `172.31.0.0/16` a default VPC has, so a
/// reported address says which subnet it came from.
const VPC_CIDR: &str = "10.90.0.0/16";
const FIRST_SUBNET_CIDR: &str = "10.90.1.0/24";
const SECOND_SUBNET_CIDR: &str = "10.90.2.0/24";

/// The first three octets of each of those subnets: an address the SERVICE chose for an
/// interface has to come out of the subnet the entry named.
const FIRST_SUBNET_PREFIX: &str = "10.90.1.";
const SECOND_SUBNET_PREFIX: &str = "10.90.2.";

/// EC2's sentence for the member that names a LIST of launch templates: it is FIXED and names
/// none of the request's values, a request being able to name several.
const TEMPLATES_ABSENT_SENTENCE: &str =
    "At least one of the launch templates specified in the request does not exist.";

/// EC2's sentence for an operation whose member names ONE launch template id: the id the
/// request sent is the one variable part.
fn single_template_absent_sentence(launch_template_id: &str) -> String {
    format!("The specified launch template, with template ID {launch_template_id}, does not exist.")
}

/// EC2's sentence for a version the named template does not hold: the version the request
/// named and the id of the template it was named on.
fn version_absent_sentence(version: &str, launch_template_id: &str) -> String {
    format!(
        "Could not find the specified version {version} for the launch template with ID \
         {launch_template_id}."
    )
}

/// One call signed for [`REGION`].
async fn api(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

// =============================================================================
// Rule 1: the interfaces a launch pins
// =============================================================================

/// The network one launch row pins: one VPC, two subnets of ONE zone (so a second interface
/// sits where the instance does), and two security groups of that VPC.
struct Network {
    router: MultiServiceRouter,
    vpc_id: String,
    first_subnet: String,
    second_subnet: String,
    group_a: String,
    group_b: String,
}

impl Network {
    async fn create() -> Self {
        let (router, _engine) = super::ec2_query::emulator();
        // One zone for both subnets: an interface of an instance sits in the instance's own
        // Availability Zone, so a row that names two subnets names two of one zone. The zone
        // comes from the Region's own catalogue rather than from a literal.
        let zone = ec2::state::availability_zones_in_region(REGION)
            .first()
            .map(|zone| zone.zone_name.clone())
            .expect("a Region reports Availability Zones");
        let body = ok(
            "the VPC",
            &router,
            "CreateVpc",
            &[param("CidrBlock", VPC_CIDR)],
        )
        .await;
        let vpc_id = minted("the VPC", &body, "vpcId");

        let mut subnets: Vec<String> = Vec::new();
        for cidr_block in [FIRST_SUBNET_CIDR, SECOND_SUBNET_CIDR] {
            let body = ok(
                "the subnet",
                &router,
                "CreateSubnet",
                &[
                    param("VpcId", &vpc_id),
                    param("CidrBlock", cidr_block),
                    param("AvailabilityZone", &zone),
                ],
            )
            .await;
            subnets.push(minted("the subnet", &body, "subnetId"));
        }

        let mut groups: Vec<String> = Vec::new();
        for name in ["pinned-launch-a", "pinned-launch-b"] {
            let body = ok(
                "the group",
                &router,
                "CreateSecurityGroup",
                &[
                    param("VpcId", &vpc_id),
                    param("GroupName", name),
                    param("GroupDescription", "a group this suite launches with"),
                ],
            )
            .await;
            groups.push(minted("the group", &body, "groupId"));
        }

        Self {
            router,
            vpc_id,
            first_subnet: subnets[0].clone(),
            second_subnet: subnets[1].clone(),
            group_a: groups[0].clone(),
            group_b: groups[1].clone(),
        }
    }

    /// One `RunInstances` that must succeed, as its body.
    async fn launch(&self, case: &str, count: u32, interfaces: &[Param]) -> String {
        let mut params = vec![
            param("ImageId", AMI),
            param("MinCount", &count.to_string()),
            param("MaxCount", &count.to_string()),
        ];
        params.extend_from_slice(interfaces);
        ok(case, &self.router, "RunInstances", &params).await
    }
}

/// The id a create reported, or a panic naming the create that did not report one: a setup
/// step that minted nothing has to fail where it happened, not as a mismatch rows later.
fn minted(case: &str, body: &str, name: &str) -> String {
    element(body, name)
        .unwrap_or_else(|| panic!("case {case:?}: the create must report its <{name}>: {body}"))
}

/// The form fields of one `NetworkInterfaces` entry: `NetworkInterface.N.<member>`, as
/// ec2Query flattens a list of structures.
fn interface(index: usize, members: &[(&str, &str)]) -> Vec<Param> {
    members
        .iter()
        .map(|(name, value)| param(&format!("NetworkInterface.{index}.{name}"), value))
        .collect()
}

/// One interface as the launch or a describe reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedInterface {
    network_interface_id: String,
    device_index: Option<String>,
    subnet_id: Option<String>,
    private_ip: Option<String>,
    description: Option<String>,
    interface_type: Option<String>,
    group_ids: Vec<String>,
    public_ip: Option<String>,
    attached_instance: Option<String>,
}

/// Read one `<networkInterface>`-shaped element. Groups are SORTED, so nothing asserted here
/// depends on the order a set was written in.
fn read_interface(node: roxmltree::Node<'_, '_>) -> ReportedInterface {
    let attachment = child(node, "attachment");
    let association = child(node, "association");
    let mut group_ids: Vec<String> = set_items(node, "groupSet")
        .into_iter()
        .filter_map(|group| child_text(group, "groupId"))
        .collect();
    group_ids.sort();
    ReportedInterface {
        network_interface_id: child_text(node, "networkInterfaceId").unwrap_or_default(),
        device_index: attachment.and_then(|held| child_text(held, "deviceIndex")),
        subnet_id: child_text(node, "subnetId"),
        private_ip: child_text(node, "privateIpAddress"),
        description: child_text(node, "description"),
        interface_type: child_text(node, "interfaceType"),
        group_ids,
        public_ip: association.and_then(|held| child_text(held, "publicIp")),
        attached_instance: attachment.and_then(|held| child_text(held, "instanceId")),
    }
}

/// One instance as the launch reports it, with its interfaces in device-index order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedInstance {
    instance_id: String,
    subnet_id: Option<String>,
    vpc_id: Option<String>,
    public_ip: Option<String>,
    interfaces: Vec<ReportedInterface>,
}

/// Every instance a body reports.
fn reported_instances(case: &str, body: &str) -> Vec<ReportedInstance> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .map(|node| {
            let mut interfaces: Vec<ReportedInterface> = set_items(node, "networkInterfaceSet")
                .into_iter()
                .map(read_interface)
                .collect();
            interfaces.sort_by(|left, right| left.device_index.cmp(&right.device_index));
            ReportedInstance {
                instance_id: child_text(node, "instanceId").unwrap_or_default(),
                subnet_id: child_text(node, "subnetId"),
                vpc_id: child_text(node, "vpcId"),
                // `Instance$PublicIpAddress` reaches the wire as `<ipAddress>`.
                public_ip: child_text(node, "ipAddress"),
                interfaces,
            }
        })
        .collect()
}

/// `DescribeNetworkInterfaces` narrowed to one interface, as that interface.
async fn described_interface(
    case: &str,
    router: &MultiServiceRouter,
    network_interface_id: &str,
) -> ReportedInterface {
    let body = ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &indexed("NetworkInterfaceId", &[network_interface_id]),
    )
    .await;
    let parsed = document(case, &body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())
        .unwrap_or_else(|| {
            panic!("case {case:?}: no interface for {network_interface_id} in {body}")
        });
    read_interface(node)
}

/// What one row expects of one interface of every instance the launch minted.
struct ExpectedInterface {
    device_index: &'static str,
    subnet_id: String,
    /// The first three octets the interface's address must carry: an address the service chose
    /// comes out of the subnet the entry named.
    address_prefix: &'static str,
    /// The address the entry NAMED, where it named one.
    private_ip: Option<&'static str>,
    description: &'static str,
    interface_type: &'static str,
    group_ids: Vec<String>,
    public_address: bool,
    /// The interface the entry named by ID, for the row that attaches one that already exists.
    network_interface_id: Option<String>,
}

/// One launch row: the entries it names, how many instances it mints, and what each instance
/// must then report.
struct Row {
    case: &'static str,
    count: usize,
    fields: Vec<Param>,
    expected: Vec<ExpectedInterface>,
}

/// RULE 1: a launch that names its interfaces is served with exactly those interfaces, and the
/// interface plane reports the same records.
#[tokio::test]
async fn a_launch_that_names_its_interfaces_is_served_with_those_interfaces() {
    let network = Network::create().await;

    // One interface created BEFORE any launch, for the row that attaches an existing one
    // rather than letting the launch create it.
    let existing = {
        let body = ok(
            "the interface created before the launch",
            &network.router,
            "CreateNetworkInterface",
            &[
                param("SubnetId", &network.first_subnet),
                param("PrivateIpAddress", "10.90.1.40"),
                param("Description", "an interface created before the launch"),
                param("SecurityGroupId.1", &network.group_b),
            ],
        )
        .await;
        minted(
            "the interface created before the launch",
            &body,
            "networkInterfaceId",
        )
    };

    let mut rows: Vec<Row> = Vec::new();
    rows.push(Row {
        case: "one eth0 naming a subnet, a group and a public address",
        count: 1,
        fields: interface(
            1,
            &[
                ("DeviceIndex", "0"),
                ("SubnetId", &network.first_subnet),
                ("SecurityGroupId.1", &network.group_a),
                ("AssociatePublicIpAddress", "true"),
                ("DeleteOnTermination", "true"),
            ],
        ),
        expected: vec![ExpectedInterface {
            device_index: "0",
            subnet_id: network.first_subnet.clone(),
            address_prefix: FIRST_SUBNET_PREFIX,
            private_ip: None,
            description: "",
            interface_type: "interface",
            group_ids: vec![network.group_a.clone()],
            public_address: true,
            network_interface_id: None,
        }],
    });
    rows.push(Row {
        case: "one eth0 naming the address and the description it wants, and no public address",
        count: 1,
        fields: interface(
            1,
            &[
                ("DeviceIndex", "0"),
                ("SubnetId", &network.first_subnet),
                ("SecurityGroupId.1", &network.group_a),
                ("PrivateIpAddress", "10.90.1.25"),
                ("Description", "the primary of the launch"),
                ("AssociatePublicIpAddress", "false"),
            ],
        ),
        expected: vec![ExpectedInterface {
            device_index: "0",
            subnet_id: network.first_subnet.clone(),
            address_prefix: FIRST_SUBNET_PREFIX,
            private_ip: Some("10.90.1.25"),
            description: "the primary of the launch",
            interface_type: "interface",
            group_ids: vec![network.group_a.clone()],
            public_address: false,
            network_interface_id: None,
        }],
    });
    rows.push(Row {
        case: "two interfaces, eth0 and eth1, each with its own group and subnet",
        count: 1,
        fields: [
            interface(
                1,
                &[
                    ("DeviceIndex", "0"),
                    ("SubnetId", &network.first_subnet),
                    ("SecurityGroupId.1", &network.group_a),
                ],
            ),
            interface(
                2,
                &[
                    ("DeviceIndex", "1"),
                    ("SubnetId", &network.second_subnet),
                    ("SecurityGroupId.1", &network.group_b),
                    ("Description", "the second of the launch"),
                ],
            ),
        ]
        .concat(),
        expected: vec![
            ExpectedInterface {
                device_index: "0",
                subnet_id: network.first_subnet.clone(),
                address_prefix: FIRST_SUBNET_PREFIX,
                private_ip: None,
                description: "",
                interface_type: "interface",
                group_ids: vec![network.group_a.clone()],
                public_address: false,
                network_interface_id: None,
            },
            ExpectedInterface {
                device_index: "1",
                subnet_id: network.second_subnet.clone(),
                address_prefix: SECOND_SUBNET_PREFIX,
                private_ip: None,
                description: "the second of the launch",
                interface_type: "interface",
                group_ids: vec![network.group_b.clone()],
                public_address: false,
                network_interface_id: None,
            },
        ],
    });
    rows.push(Row {
        case: "an eth0 in the second subnet with two groups and a named type, for two instances",
        count: 2,
        fields: interface(
            1,
            &[
                ("DeviceIndex", "0"),
                ("SubnetId", &network.second_subnet),
                ("SecurityGroupId.1", &network.group_a),
                ("SecurityGroupId.2", &network.group_b),
                // "Valid values: interface | efa | efa-only"
                // (`InstanceNetworkInterfaceSpecification$InterfaceType`).
                ("InterfaceType", "interface"),
            ],
        ),
        expected: vec![ExpectedInterface {
            device_index: "0",
            subnet_id: network.second_subnet.clone(),
            address_prefix: SECOND_SUBNET_PREFIX,
            private_ip: None,
            description: "",
            interface_type: "interface",
            group_ids: {
                let mut both = vec![network.group_a.clone(), network.group_b.clone()];
                both.sort();
                both
            },
            public_address: false,
            network_interface_id: None,
        }],
    });
    rows.push(Row {
        case: "an eth0 naming an interface that already exists",
        count: 1,
        fields: interface(
            1,
            &[("DeviceIndex", "0"), ("NetworkInterfaceId", &existing)],
        ),
        expected: vec![ExpectedInterface {
            device_index: "0",
            subnet_id: network.first_subnet.clone(),
            address_prefix: FIRST_SUBNET_PREFIX,
            private_ip: Some("10.90.1.40"),
            description: "an interface created before the launch",
            interface_type: "interface",
            group_ids: vec![network.group_b.clone()],
            public_address: false,
            network_interface_id: Some(existing.clone()),
        }],
    });

    for row in &rows {
        let case = row.case;
        let body = network
            .launch(
                case,
                u32::try_from(row.count).expect("a row launches a handful of instances"),
                &row.fields,
            )
            .await;
        let instances = reported_instances(case, &body);
        assert_eq!(
            instances.len(),
            row.count,
            "case {case:?}: expected {} instance(s), got {instances:?} from {body}",
            row.count
        );

        let mut seen_ids: Vec<String> = Vec::new();
        for instance in &instances {
            assert_eq!(
                instance.interfaces.len(),
                row.expected.len(),
                "case {case:?}: expected {} interface(s) on {}, got {instance:?}",
                row.expected.len(),
                instance.instance_id
            );
            for (expected, reported) in row.expected.iter().zip(&instance.interfaces) {
                let at = format!("{case}: device index {}", expected.device_index);
                assert_eq!(
                    reported.device_index.as_deref(),
                    Some(expected.device_index),
                    "case {at:?}: the entry is attached at the index it named, got {reported:?}"
                );
                assert_eq!(
                    reported.subnet_id.as_deref(),
                    Some(expected.subnet_id.as_str()),
                    "case {at:?}: expected the subnet the entry named, got {reported:?}"
                );
                assert_eq!(
                    reported.group_ids, expected.group_ids,
                    "case {at:?}: expected the groups the entry named, got {reported:?}"
                );
                assert_eq!(
                    reported.description.as_deref(),
                    Some(expected.description),
                    "case {at:?}: expected the description the entry named, got {reported:?}"
                );
                assert_eq!(
                    reported.interface_type.as_deref(),
                    Some(expected.interface_type),
                    "case {at:?}: expected the interface type, got {reported:?}"
                );
                let address = reported.private_ip.clone().unwrap_or_default();
                if let Some(named) = expected.private_ip {
                    assert_eq!(
                        address, named,
                        "case {at:?}: expected the address the entry named, got {reported:?}"
                    );
                }
                assert!(
                    address.starts_with(expected.address_prefix),
                    "case {at:?}: expected an address of {}, got {address:?}",
                    expected.address_prefix
                );
                assert_eq!(
                    reported.public_ip.is_some(),
                    expected.public_address,
                    "case {at:?}: expected a public address: {}, got {reported:?}",
                    expected.public_address
                );
                if let Some(named) = expected.network_interface_id.as_deref() {
                    assert_eq!(
                        reported.network_interface_id, named,
                        "case {at:?}: expected the interface the entry named by id, got \
                         {reported:?}"
                    );
                }
                assert!(
                    !seen_ids.contains(&reported.network_interface_id),
                    "case {at:?}: every attachment of a launch is its own interface, but \
                     {} was reported twice",
                    reported.network_interface_id
                );
                seen_ids.push(reported.network_interface_id.clone());

                // One record: the interface plane reports the interface the launch did, and
                // reports it attached to the instance that launched it — the launch's own
                // answer carries no instance id inside an attachment of the instance it is
                // already reporting.
                let read_back =
                    described_interface(&at, &network.router, &reported.network_interface_id)
                        .await;
                assert_eq!(
                    read_back.attached_instance.as_deref(),
                    Some(instance.instance_id.as_str()),
                    "case {at:?}: the interface must be attached to the instance that \
                     launched it, got {read_back:?}"
                );
                let apart_from_the_attachment = ReportedInterface {
                    attached_instance: reported.attached_instance.clone(),
                    ..read_back.clone()
                };
                assert_eq!(
                    &apart_from_the_attachment, reported,
                    "case {at:?}: the describe of the interface reports another record: \
                     {read_back:?}"
                );
            }

            // The entry at device index 0 is the instance's PRIMARY interface, so the
            // instance's own network members are that interface's.
            let primary = &instance.interfaces[0];
            assert_eq!(
                instance.subnet_id, primary.subnet_id,
                "case {case:?}: expected the instance's subnet to be eth0's, got {instance:?}"
            );
            assert_eq!(
                instance.vpc_id.as_deref(),
                Some(network.vpc_id.as_str()),
                "case {case:?}: expected the instance in the VPC of its subnet, got {instance:?}"
            );
            assert_eq!(
                instance.public_ip, primary.public_ip,
                "case {case:?}: expected the instance's public address to be eth0's, got \
                 {instance:?}"
            );
        }
    }
}

// =============================================================================
// Rules 2 and 3: the sentences an absent template and an absent version carry
// =============================================================================

/// How one operation names the launch template key it is about, and the other members it needs
/// before it reaches that key.
type NamesKey = fn(&str) -> Vec<Param>;

/// Create a launch template named `name` whose first version names an AMI, and answer its id.
async fn create_template(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateLaunchTemplate",
        &[
            param("LaunchTemplateName", name),
            param("LaunchTemplateData.ImageId", AMI),
        ],
    )
    .await;
    minted(case, &body, "launchTemplateId")
}

/// How many instances the account holds, read back through `DescribeInstances`.
async fn instance_count(router: &MultiServiceRouter, case: &str) -> usize {
    let body = ok(case, router, "DescribeInstances", &[]).await;
    document(case, &body)
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "instanceId")
        .count()
}

/// RULE 3 at its SOURCE, where interchanging two forms' sentences can actually happen: each
/// form's wording lives in its own constructor of [`ec2::state`], and for the SAME template the
/// three constructors produce three DISTINCT sentences. A form can only be made to answer
/// another's sentence by pointing its call site at the other's constructor — which the tables
/// below catch on the wire — or by wording two constructors alike, which this catches here.
#[test]
fn each_member_form_carries_its_own_absence_sentence() {
    // One id and one version, so the three sentences are compared about the same template: a
    // difference between them is then a difference of WORDING and not of values.
    let launch_template_id = "lt-00000000000000123";
    let version = "7";
    // (the member form, the sentence AWS words for it, the constructor every operation of that
    // form answers from)
    let forms: [(&str, String, String); 3] = [
        (
            "a member that names ONE launch template id",
            single_template_absent_sentence(launch_template_id),
            ec2::state::specified_launch_template_id_not_found_message(launch_template_id),
        ),
        (
            "a member that names a LIST of launch template ids",
            TEMPLATES_ABSENT_SENTENCE.to_string(),
            ec2::state::LAUNCH_TEMPLATES_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "a member that names a VERSION of a launch template",
            version_absent_sentence(version, launch_template_id),
            ec2::state::specified_launch_template_version_not_found_message(
                launch_template_id,
                version,
            ),
        ),
    ];
    for (case, as_aws_words_it, as_the_crate_words_it) in &forms {
        assert_eq!(
            as_the_crate_words_it, as_aws_words_it,
            "case {case:?}: expected the sentence AWS words for this form, got \
             {as_the_crate_words_it:?}"
        );
    }
    for left in 0..forms.len() {
        for right in (left + 1)..forms.len() {
            assert_ne!(
                forms[left].1, forms[right].1,
                "case {:?} vs {:?}: two member forms must not carry one sentence, or a client \
                 cannot tell which of its values was not found",
                forms[left].0, forms[right].0
            );
        }
    }
}

/// RULES 2 and 3 on the wire: each refusal carries the sentence of the FORM the request used.
#[tokio::test]
async fn an_absent_template_or_version_is_refused_in_the_sentence_of_the_members_form() {
    let (router, _engine) = super::ec2_query::emulator();

    // One template that is kept, so every refusal below is about the key the row names rather
    // than about an account holding no template at all. It holds versions 1 and 2.
    let kept = create_template(&router, "the template that is kept", "sentences.kept").await;
    let body = ok(
        "the template that is kept",
        &router,
        "CreateLaunchTemplateVersion",
        &[
            param("LaunchTemplateId", &kept),
            param("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        element(&body, "versionNumber").as_deref(),
        Some("2"),
        "versions are numbered in the order they are created, got {body}"
    );

    // An id this service really issued whose template the account no longer holds.
    let gone = create_template(&router, "the template that is taken away", "sentences.gone").await;
    let deleted = ok(
        "the template that is taken away",
        &router,
        "DeleteLaunchTemplate",
        &[param("LaunchTemplateId", &gone)],
    )
    .await;
    assert_eq!(
        element(&deleted, "launchTemplateId").as_deref(),
        Some(gone.as_str()),
        "the delete must report the template it took away: {deleted}"
    );

    // RULE 3, the single-key half: every operation whose member names ONE id. The rows differ
    // in the operation and in what else it has to carry to reach the key.
    let single: Vec<(&str, NamesKey)> = vec![
        ("DescribeLaunchTemplateVersions", |id| {
            vec![param("LaunchTemplateId", id)]
        }),
        // A second delete of a template already taken away names the same one id.
        ("DeleteLaunchTemplate", |id| {
            vec![param("LaunchTemplateId", id)]
        }),
        ("DeleteLaunchTemplateVersions", |id| {
            vec![
                param("LaunchTemplateId", id),
                param("LaunchTemplateVersion.1", "1"),
            ]
        }),
        ("CreateLaunchTemplateVersion", |id| {
            vec![
                param("LaunchTemplateId", id),
                param("LaunchTemplateData.InstanceType", "t3.micro"),
            ]
        }),
        ("ModifyLaunchTemplate", |id| {
            vec![param("LaunchTemplateId", id), param("SetDefaultVersion", "1")]
        }),
        ("RunInstances", |id| {
            vec![
                param("MinCount", "1"),
                param("MaxCount", "1"),
                param("LaunchTemplate.LaunchTemplateId", id),
            ]
        }),
    ];
    let expected = single_template_absent_sentence(&gone);
    for (action, names_key) in single {
        let case = format!("{action}: an id whose template is gone");
        let (status, body) = api(&router, action, &names_key(&gone)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidLaunchTemplateId.NotFound",
            "case {case:?}: expected the id's own not-found code, got body {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the sentence that names the one id the request sent, got \
             {message:?} in {body}"
        );
    }

    // RULE 3, the id-LIST half: the operation whose member takes a list states the plural
    // sentence and names none of the request's values, however many ids it was sent.
    for (case, ids) in [
        ("DescribeLaunchTemplates: one gone id", vec![gone.as_str()]),
        (
            "DescribeLaunchTemplates: a held id beside the gone one",
            vec![kept.as_str(), gone.as_str()],
        ),
    ] {
        let (status, body) = api(
            &router,
            "DescribeLaunchTemplates",
            &indexed("LaunchTemplateId", &ids),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidLaunchTemplateId.NotFound",
            "case {case:?}: expected the id's own not-found code, got body {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, TEMPLATES_ABSENT_SENTENCE,
            "case {case:?}: expected the fixed sentence for a member that names a list, got \
             {message:?} in {body}"
        );
        assert!(
            !message.contains(&gone) && !message.contains(&kept),
            "case {case:?}: the list form's sentence names none of the request's values, got \
             {message:?}"
        );
    }

    // RULE 2: a version the template does not hold, over how far past its versions the
    // request reaches and over the two operations that RESOLVE a version — a read of it and a
    // launch from it. The template is named by its id and by its name, and the sentence names
    // the template's ID either way: the id comes from the template the account holds, the
    // version from the request.
    for version in ["3", "99", "0", "-1"] {
        let expected = version_absent_sentence(version, &kept);
        let probes: Vec<(String, Vec<Param>, &str)> = vec![
            (
                format!("RunInstances: version {version} by the template's id"),
                vec![
                    param("MinCount", "1"),
                    param("MaxCount", "1"),
                    param("LaunchTemplate.LaunchTemplateId", &kept),
                    param("LaunchTemplate.Version", version),
                ],
                "RunInstances",
            ),
            (
                format!("RunInstances: version {version} by the template's name"),
                vec![
                    param("MinCount", "1"),
                    param("MaxCount", "1"),
                    param("LaunchTemplate.LaunchTemplateName", "sentences.kept"),
                    param("LaunchTemplate.Version", version),
                ],
                "RunInstances",
            ),
            (
                format!("DescribeLaunchTemplateVersions: version {version}"),
                vec![
                    param("LaunchTemplateId", &kept),
                    param("LaunchTemplateVersion.1", version),
                ],
                "DescribeLaunchTemplateVersions",
            ),
        ];
        for (case, params, action) in probes {
            let (status, body) = api(&router, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidLaunchTemplateId.VersionNotFound",
                "case {case:?}: expected the version's own not-found code, got body {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the sentence a version resolution carries, got \
                 {message:?} in {body}"
            );
        }
    }

    // Every refusal only REPORTED: the template that was never deleted still resolves, by
    // either key and at the versions it holds, and no refused launch minted an instance.
    for key in [
        param("LaunchTemplateId", &kept),
        param("LaunchTemplateName", "sentences.kept"),
    ] {
        let body = ok(
            "the template that is kept",
            &router,
            "DescribeLaunchTemplateVersions",
            &[key],
        )
        .await;
        assert_eq!(
            element(&body, "launchTemplateId").as_deref(),
            Some(kept.as_str()),
            "the kept template must still resolve by either key, got {body}"
        );
    }
    assert_eq!(
        instance_count(&router, "the template that is kept").await,
        0,
        "a refused launch must mint no instance"
    );
}
