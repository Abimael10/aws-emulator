// SPDX-License-Identifier: CC0-1.0

//! The two DOCUMENTS a record of an EC2 call carries — the parameters it was dispatched
//! with and the resources it acted on — in the form EC2's own API documents take.
//!
//! A CloudTrail record reports a call in the SERVING API's own document form rather than in
//! the SDK's: for EC2, whose wire protocol is a query/XML dialect, that form is the one the
//! model's `@xmlName`s spell — a member under the name the wire gives it, and a repeated
//! member as a single member holding its elements under
//! `service_common::management_events::WIRE_LIST_ELEMENTS_MEMBER`. A response is rendered
//! straight from the output's own schema in that form
//! (`service_common::management_events::RecordedDocumentForm::WireMembers`, driven from
//! [`super::management_events::hold_success`]); the REQUEST has no built shape to render —
//! an ec2Query request carries a flattened `application/x-www-form-urlencoded` parameter
//! list (`InstanceType`, `InstanceId.1`, `Filter.1.Name`) — so this module reads that list
//! back as the document form above.
//!
//! # How a parameter key becomes a path in the document
//!
//! Each key is read as the path it names: a name segment is a member, an INDEX segment the
//! (one-based) position of an element, and the wrapper segment a wrapped list writes
//! positions a value inside the member before it rather than naming one of its own. On top
//! of that reading, the document form puts a repeated member's elements under their own
//! member ([`RECORDED_LIST_MEMBERS`] names the member each repeated parameter is held in,
//! and a repeated parameter the table does not name is held in `<member>Set`), and names a
//! SCALAR element by the member the parameter itself was sent under — so
//! `InstanceId.1=i-…` is recorded as `instancesSet.items[0].instanceId`, which is how an
//! EC2 document carries the instances a call names.
//!
//! A few operations' documents group members the public request carries at the top level
//! ([`LAUNCH_SPECIFICATION_MEMBERS`]): EC2's own `RunInstances` document carries the launch
//! specification per instance, so the members that describe the instances to launch sit in
//! that per-instance element.
//!
//! # The resources a record names
//!
//! An event's `Resources` names each resource the call ACTED ON, by the resource type
//! CloudTrail publishes for it beside its id ([`CLOUDTRAIL_RESOURCE_TYPES`]). The ids come
//! out of the two documents the record already carries — what the call was dispatched with
//! and what it answered — read by the same prefix table the authorization gate takes a
//! request's ids through ([`super::addressed_resources::resource_type_of`]), so a resource
//! a call MINTED is named as well as one its request addressed. A REPORTING call acts on
//! nothing: it names no resource, exactly as it records no `responseElements`.

use std::borrow::Cow;

use aws_smithy_runtime_api::http::Request as RtRequest;

use service_common::management_events::{
    recorded_member_name, AuditedResource, HIDDEN_SENSITIVE_VALUE, WIRE_LIST_ELEMENTS_MEMBER,
};

/// The parameters an ec2Query request carries that are the PROTOCOL's rather than the
/// call's, and so are no part of the call's recorded parameters.
const ENVELOPE_PARAMETERS: &[&str] = &["Action", "Version"];

/// The most form parameters one record reports. The same bound, for the same reason, as the
/// authorization gate's walk over the same parameter list
/// (`super::addressed_resources`): the list is sized by the caller and every served call
/// appends a record, so one record's parameter count is bounded here rather than by what
/// was sent.
pub const MAX_RECORDED_PARAMETERS: usize = 4_096;

/// The separator between the segments of an ec2Query parameter key.
const PARAMETER_PATH_SEPARATOR: char = '.';

/// The segment a WRAPPED list writes between the member and the index of an element. It
/// positions a value within the member the segment before it named rather than naming a
/// member of its own, so a recorded path does not carry it.
const WRAPPED_LIST_SEGMENT: &str = "member";

/// How deep into a request's parameters one record goes — the same bound, for the same
/// reason, as the shared recorder's walk over a built input.
const MAX_PARAMETER_DEPTH: usize = 16;

/// What an EC2 document names a repeated parameter's member, where that name is not the
/// parameter's own with `Set` after it.
///
/// Keyed by the parameter's path — the name segments of its key, without the indices
/// (`TagSpecification.Tag`) — because the same parameter name is held differently at
/// different depths. The names are EC2's own for a list of the same thing: the model
/// carries each one as the `@xmlName` of the member a RESPONSE reports that list in
/// (`smithy_query(query="shape",shape="Reservation,DescribeImagesResult,DescribeVolumesResult,DescribeSubnetsResult,DescribeVpcsResult,DescribeSnapshotsResult,Instance")`
/// -> `instancesSet`, `imagesSet`, `volumeSet`, `subnetSet`, `vpcSet`, `snapshotSet`,
/// `blockDeviceMapping`), and the two rows with no response counterpart are the ones AWS's
/// published records of `CreateTags` (`resourcesSet`) and of a create carrying tag
/// specifications (`tags`, held directly rather than under a member of its own) show.
///
/// `WRAPPED` is whether the elements sit under [`WIRE_LIST_ELEMENTS_MEMBER`] inside that
/// member, which all but the last of these do.
pub const RECORDED_LIST_MEMBERS: &[(&str, &str, bool)] = &[
    ("BlockDeviceMapping", "blockDeviceMapping", true),
    ("ImageId", "imagesSet", true),
    ("InstanceId", "instancesSet", true),
    ("ResourceId", "resourcesSet", true),
    ("SnapshotId", "snapshotSet", true),
    ("SubnetId", "subnetSet", true),
    ("TagSpecification.Tag", "tags", false),
    ("VolumeId", "volumeSet", true),
    ("VpcId", "vpcSet", true),
];

/// What a repeated parameter the table above does not name is held in: its own member name
/// with `Set` after it (`Filter.1.Name` -> `filterSet.items[0].name`), which is how EC2's
/// documents name a repeated member.
const RECORDED_LIST_MEMBER_SUFFIX: &str = "Set";

/// The operation whose document carries a per-instance LAUNCH SPECIFICATION, and the
/// members of the public request that sit inside it.
///
/// EC2's own `RunInstances` document describes the instances to launch as a list — which is
/// why a record of a launch carries `instancesSet.items[0].imageId` where the request sent
/// `ImageId` — so the members that describe those instances are recorded there rather than
/// beside the launch-wide ones (`instanceType`, `subnetId`, the tag specifications). The
/// member list is the one AWS's published `RunInstances` records carry inside that element.
const LAUNCH_SPECIFICATION_MEMBERS: &[&str] = &[
    "AdditionalInfo",
    "ImageId",
    "KernelId",
    "KeyName",
    "MaxCount",
    "MinCount",
    "RamdiskId",
    "UserData",
];

/// The operation whose request is recorded with a launch specification
/// ([`LAUNCH_SPECIFICATION_MEMBERS`]).
const LAUNCH_OPERATION: &str = "RunInstances";

/// The member an EC2 document holds a launch specification in, and the member the
/// instances a launch answers are reported in: one name for the request's list and the
/// response's, as EC2's documents spell it (`Reservation$Instances`'s `@xmlName`).
const LAUNCH_SPECIFICATION_MEMBER: &str = "instancesSet";

/// The resource type CloudTrail names each kind of EC2 resource by in an event's
/// `Resources`, keyed by the resource type this service's own ARNs carry
/// (`super::addressed_resources::RESOURCE_TYPE_BY_ID_PREFIX`).
///
/// The names AWS publishes for these resources (`AWS::EC2::<resource>` — the spelling
/// CloudTrail and CloudFormation both use). A kind this table does not carry names no
/// resource in a record rather than one under a type nothing publishes, which is the same
/// direction every other gap in the reading of a request's ids takes
/// (`super::addressed_resources`).
pub const CLOUDTRAIL_RESOURCE_TYPES: &[(&str, &str)] = &[
    ("capacity-reservation", "AWS::EC2::CapacityReservation"),
    ("dhcp-options", "AWS::EC2::DHCPOptions"),
    ("elastic-ip", "AWS::EC2::EIP"),
    ("instance", "AWS::EC2::Instance"),
    ("internet-gateway", "AWS::EC2::InternetGateway"),
    ("launch-template", "AWS::EC2::LaunchTemplate"),
    ("natgateway", "AWS::EC2::NatGateway"),
    ("network-acl", "AWS::EC2::NetworkAcl"),
    ("network-interface", "AWS::EC2::NetworkInterface"),
    ("route-table", "AWS::EC2::RouteTable"),
    ("security-group", "AWS::EC2::SecurityGroup"),
    ("snapshot", "AWS::EC2::Snapshot"),
    ("subnet", "AWS::EC2::Subnet"),
    ("transit-gateway", "AWS::EC2::TransitGateway"),
    ("volume", "AWS::EC2::Volume"),
    ("vpc", "AWS::EC2::VPC"),
    ("vpc-endpoint", "AWS::EC2::VPCEndpoint"),
    ("vpc-peering-connection", "AWS::EC2::VPCPeeringConnection"),
];

/// How many resources one record names. An EMULATOR ceiling, for the same reason the
/// parameter count is bounded: a request's id list is sized by the caller and every served
/// call appends a record.
const MAX_AUDITED_RESOURCES: usize = 64;

/// How deep into a record's own documents the reading of its resources goes — one level per
/// member or element, past the depth either document can reach ([`MAX_PARAMETER_DEPTH`] for
/// the parameters, the shared recorder's own ceiling for the response), so the walk is
/// bounded by this rather than by what a caller nested.
const MAX_DOCUMENT_DEPTH: usize = 32;

/// The parameters `req` carried, as the record's `requestParameters` for `operation`.
///
/// The protocol's own parameters ([`ENVELOPE_PARAMETERS`]) are no part of the call and are
/// left out, and a value held at a member this service withholds
/// (`super::management_events::SENSITIVE_MEMBERS`) carries the redaction literal instead —
/// matched per segment, so a withheld member is withheld wherever in a request it occurs.
#[must_use]
pub fn request_document(operation: &str, req: &RtRequest) -> serde_json::Value {
    let mut document = serde_json::Value::Object(serde_json::Map::new());
    // The one walk this front door takes over a request's parameter list
    // (`super::request_parameters`), under the bound a RECORD takes rather than the one a
    // gate's decision takes.
    let fields = super::request_parameters::request_parameters(req, MAX_RECORDED_PARAMETERS);
    for (key, value) in fields {
        if ENVELOPE_PARAMETERS.contains(&key.as_ref()) {
            continue;
        }
        let segments: Vec<&str> = key
            .split(PARAMETER_PATH_SEPARATOR)
            .filter(|segment| *segment != WRAPPED_LIST_SEGMENT)
            .collect();
        if segments.is_empty() || segments.len() > MAX_PARAMETER_DEPTH {
            continue;
        }
        let withheld = segments.iter().any(|segment| is_withheld(segment));
        let recorded = if withheld {
            HIDDEN_SENSITIVE_VALUE.to_string()
        } else {
            value.into_owned()
        };
        put_at(
            &mut document,
            &recorded_path(operation, &segments),
            serde_json::Value::String(recorded),
        );
    }
    document
}

/// One step of a path through a recorded document.
#[derive(Debug, PartialEq, Eq)]
enum Step<'a> {
    /// A member of a structure, named as the document names it.
    Member(Cow<'a, str>),
    /// The (zero-based) position of an element of a list.
    Element(usize),
}

/// The path the parameter key whose name/index `segments` are given is recorded at, in the
/// document form of the serving API (see the module docs).
fn recorded_path<'a>(operation: &str, segments: &[&'a str]) -> Vec<Step<'a>> {
    let mut path = Vec::new();
    // The name segments seen so far, which is what [`RECORDED_LIST_MEMBERS`] is keyed by.
    let mut named: Vec<&str> = Vec::new();
    let mut at = 0;
    while let Some(segment) = segments.get(at) {
        if let Some(index) = list_index(segment) {
            // An index with no member before it positions a value in nothing; recorded as
            // the element it names so the path stays the one the key wrote.
            path.push(Step::Element(index));
            at += 1;
            continue;
        }
        named.push(segment);
        match segments.get(at + 1).and_then(|next| list_index(next)) {
            // A member the request sent one value for.
            None => {
                if named.len() == 1 && in_launch_specification(operation, segment) {
                    path.push(Step::Member(Cow::Borrowed(LAUNCH_SPECIFICATION_MEMBER)));
                    path.push(Step::Member(Cow::Borrowed(WIRE_LIST_ELEMENTS_MEMBER)));
                    path.push(Step::Element(0));
                }
                path.push(Step::Member(Cow::Owned(recorded_member_name(segment))));
                at += 1;
            }
            // A REPEATED member: the member its elements are held in, then the element.
            Some(index) => {
                let (member, wrapped) = recorded_list_member(&named);
                path.push(Step::Member(member));
                if wrapped {
                    path.push(Step::Member(Cow::Borrowed(WIRE_LIST_ELEMENTS_MEMBER)));
                }
                path.push(Step::Element(index));
                // A SCALAR element — the key ends at the index — is named by the member the
                // parameter itself was sent under, which is how an EC2 document carries a
                // list of ids or of values.
                if at + 2 == segments.len() {
                    path.push(Step::Member(Cow::Owned(recorded_member_name(segment))));
                }
                at += 2;
            }
        }
    }
    path
}

/// The member an EC2 document holds the repeated parameter at the path `named` in, and
/// whether its elements sit under [`WIRE_LIST_ELEMENTS_MEMBER`] inside it.
fn recorded_list_member<'a>(named: &[&str]) -> (Cow<'a, str>, bool) {
    let path = named.join(&PARAMETER_PATH_SEPARATOR.to_string());
    if let Some((_, member, wrapped)) = RECORDED_LIST_MEMBERS
        .iter()
        .find(|(parameter, _, _)| *parameter == path)
    {
        return (Cow::Borrowed(*member), *wrapped);
    }
    let own = named.last().copied().unwrap_or_default();
    (
        Cow::Owned(format!(
            "{}{RECORDED_LIST_MEMBER_SUFFIX}",
            recorded_member_name(own)
        )),
        true,
    )
}

/// Is `member` of `operation`'s request recorded inside the per-instance launch
/// specification ([`LAUNCH_SPECIFICATION_MEMBERS`])?
fn in_launch_specification(operation: &str, member: &str) -> bool {
    operation == LAUNCH_OPERATION && LAUNCH_SPECIFICATION_MEMBERS.contains(&member)
}

/// Whether a record withholds the value a parameter key's `segment` names
/// (`super::management_events::SENSITIVE_MEMBERS`).
///
/// Compared without regard to ASCII case: the set is the MODEL's spelling of each member,
/// an ec2Query key is that name with its first letter capitalised, and a client's spelling
/// of a KEY must not be what decides whether a secret reaches a record.
fn is_withheld(segment: &str) -> bool {
    super::management_events::SENSITIVE_MEMBERS
        .iter()
        .any(|member| member.eq_ignore_ascii_case(segment))
}

/// The zero-based position of the list element `segment` names, or `None` when it names a
/// member instead.
///
/// ec2Query numbers the elements of a list FROM ONE (`InstanceId.1`), so the index is one
/// less than the segment.
fn list_index(segment: &str) -> Option<usize> {
    let position = segment.parse::<usize>().ok()?;
    position.checked_sub(1)
}

/// Put `value` at `path` in `document`, growing the structures and lists it leads through.
///
/// A list grows only by the element the NEXT position names, so the document holds one
/// value per field the request sent and nothing a field did not fill: a request numbering
/// an element past the end of the list it is in states a position the fields before it
/// never filled, and that field is left out rather than reserving the positions between.
/// That is what keeps the document's size the size of the parameter list the request really
/// carried — already bounded by [`MAX_RECORDED_PARAMETERS`] — rather than a number a
/// caller wrote in a key.
///
/// A container already holding another kind of value is REPLACED rather than merged: a key
/// that names a path through a value another key stated as a leaf is the caller's own
/// inconsistency, and a record reports the last reading of it rather than both.
fn put_at(document: &mut serde_json::Value, path: &[Step<'_>], value: serde_json::Value) {
    let Some((step, rest)) = path.split_first() else {
        *document = value;
        return;
    };
    match step {
        Step::Element(index) => {
            if !document.is_array() {
                *document = serde_json::Value::Array(Vec::new());
            }
            let Some(elements) = document.as_array_mut() else {
                return;
            };
            if *index > elements.len() {
                return;
            }
            if *index == elements.len() {
                elements.push(serde_json::Value::Null);
            }
            // By position rather than by index expression: a record is never worth a panic
            // on a path a request reaches.
            if let Some(element) = elements.get_mut(*index) {
                put_at(element, rest, value);
            }
        }
        Step::Member(name) => {
            if !document.is_object() {
                *document = serde_json::Value::Object(serde_json::Map::new());
            }
            let Some(members) = document.as_object_mut() else {
                return;
            };
            let member = members
                .entry(name.as_ref())
                .or_insert(serde_json::Value::Null);
            put_at(member, rest, value);
        }
    }
}

/// The resources the call these documents report ACTED ON, each by the resource type
/// CloudTrail publishes for it beside its own id.
///
/// `documents` are the record's own two documents, read in the order they are given: what
/// the call was dispatched with, then what it answered — so a resource the request
/// addressed is named before one the call minted. Each resource is named once.
#[must_use]
pub fn audited_resources(documents: &[&serde_json::Value]) -> Vec<AuditedResource> {
    let mut resources: Vec<AuditedResource> = Vec::new();
    for document in documents {
        collect_resources(document, 0, &mut resources);
    }
    resources
}

/// Add every resource an id in `document` names to `resources`, at most
/// [`MAX_AUDITED_RESOURCES`] of them and each named once.
///
/// `depth` bounds the walk at [`MAX_DOCUMENT_DEPTH`]: the recursion is bounded here rather
/// than by how deeply a caller's own parameter list nested.
fn collect_resources(
    document: &serde_json::Value,
    depth: usize,
    resources: &mut Vec<AuditedResource>,
) {
    if resources.len() >= MAX_AUDITED_RESOURCES || depth > MAX_DOCUMENT_DEPTH {
        return;
    }
    match document {
        serde_json::Value::String(text) => {
            let Some(resource_type) = cloudtrail_resource_type(text) else {
                return;
            };
            let resource = AuditedResource::new(resource_type, text);
            if !resources.contains(&resource) {
                resources.push(resource);
            }
        }
        serde_json::Value::Array(elements) => {
            for element in elements {
                collect_resources(element, depth + 1, resources);
            }
        }
        serde_json::Value::Object(members) => {
            for member in members.values() {
                collect_resources(member, depth + 1, resources);
            }
        }
        _ => {}
    }
}

/// The CloudTrail resource type `value` is the id of, or `None` when it is no EC2 id of a
/// kind [`CLOUDTRAIL_RESOURCE_TYPES`] carries.
#[must_use]
pub fn cloudtrail_resource_type(value: &str) -> Option<&'static str> {
    let resource_type = super::addressed_resources::resource_type_of(value)?;
    CLOUDTRAIL_RESOURCE_TYPES
        .iter()
        .find(|(known, _)| *known == resource_type)
        .map(|(_, published)| *published)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the parameter-path table: the case, the operation, the fields the request
    /// carried, and the document they are recorded as.
    type ParameterCase<'a> = (
        &'a str,
        &'a str,
        &'a [(&'a str, &'a str)],
        serde_json::Value,
    );

    /// The document the fields of one request are recorded as, driven exactly as
    /// [`request_document`] drives it.
    fn recorded(operation: &str, fields: &[(&str, &str)]) -> serde_json::Value {
        let mut document = serde_json::Value::Object(serde_json::Map::new());
        for (key, value) in fields {
            let segments: Vec<&str> = key
                .split(PARAMETER_PATH_SEPARATOR)
                .filter(|segment| *segment != WRAPPED_LIST_SEGMENT)
                .collect();
            let withheld = segments.iter().any(|segment| is_withheld(segment));
            let recorded = if withheld { HIDDEN_SENSITIVE_VALUE } else { value };
            put_at(
                &mut document,
                &recorded_path(operation, &segments),
                serde_json::Value::String(recorded.to_string()),
            );
        }
        document
    }

    /// RULE: a parameter key is read back as the path it names, in the document form the
    /// serving API uses — a repeated member holding its elements under `items`, a scalar
    /// element named by the member it was sent under — and a position no field filled is
    /// left out.
    #[test]
    fn a_parameter_key_is_read_back_in_the_document_form_of_the_serving_api() {
        let rows: &[ParameterCase<'_>] = &[
            (
                "a scalar member",
                "RunInstances",
                &[("InstanceType", "t3.micro")],
                serde_json::json!({"instanceType": "t3.micro"}),
            ),
            (
                "a list of ids, held in the member EC2's documents name it",
                "TerminateInstances",
                &[("InstanceId.1", "i-0123abcd"), ("InstanceId.2", "i-4567ef01")],
                serde_json::json!({
                    "instancesSet": {"items": [
                        {"instanceId": "i-0123abcd"},
                        {"instanceId": "i-4567ef01"},
                    ]},
                }),
            ),
            (
                "a wrapped list, whose wrapper segment names no member",
                "TerminateInstances",
                &[("InstanceId.member.1", "i-0123abcd")],
                serde_json::json!({
                    "instancesSet": {"items": [{"instanceId": "i-0123abcd"}]},
                }),
            ),
            (
                // A row the issue did not cite: a repeated member the table does not name
                // is held in its own name with `Set` after it, at both depths.
                "a list of structures, each with a list of its own",
                "DescribeInstances",
                &[
                    ("Filter.1.Name", "instance-state-name"),
                    ("Filter.1.Value.1", "running"),
                    ("Filter.1.Value.2", "pending"),
                ],
                serde_json::json!({
                    "filterSet": {"items": [{
                        "name": "instance-state-name",
                        "valueSet": {"items": [{"value": "running"}, {"value": "pending"}]},
                    }]},
                }),
            ),
            (
                // Another row the issue did not cite: the tag write's own two lists, one
                // named by the table and one by the default rule.
                "a tag write, whose resources and tags are both repeated",
                "CreateTags",
                &[
                    ("ResourceId.1", "i-0123abcd"),
                    ("Tag.1.Key", "Name"),
                    ("Tag.1.Value", "audited"),
                ],
                serde_json::json!({
                    "resourcesSet": {"items": [{"resourceId": "i-0123abcd"}]},
                    "tagSet": {"items": [{"key": "Name", "value": "audited"}]},
                }),
            ),
            (
                "a launch, whose per-instance members sit in its launch specification",
                "RunInstances",
                &[
                    ("ImageId", "ami-0123456789abcdef0"),
                    ("MinCount", "1"),
                    ("MaxCount", "2"),
                    ("InstanceType", "t3.micro"),
                    ("SubnetId", "subnet-0123456789abcdef0"),
                    ("TagSpecification.1.ResourceType", "instance"),
                    ("TagSpecification.1.Tag.1.Key", "Name"),
                    ("TagSpecification.1.Tag.1.Value", "audited"),
                ],
                serde_json::json!({
                    "instancesSet": {"items": [{
                        "imageId": "ami-0123456789abcdef0",
                        "minCount": "1",
                        "maxCount": "2",
                    }]},
                    "instanceType": "t3.micro",
                    "subnetId": "subnet-0123456789abcdef0",
                    "tagSpecificationSet": {"items": [{
                        "resourceType": "instance",
                        "tags": [{"key": "Name", "value": "audited"}],
                    }]},
                }),
            ),
            (
                // The same member names a resource of its own OUTSIDE a launch, where no
                // launch specification holds it.
                "an image member of an operation that carries no launch specification",
                "DescribeImageAttribute",
                &[("ImageId", "ami-0123456789abcdef0")],
                serde_json::json!({"imageId": "ami-0123456789abcdef0"}),
            ),
            (
                "an element numbered past the end of its list, which no field filled",
                "TerminateInstances",
                &[("InstanceId.1", "i-0123abcd"), ("InstanceId.9999", "i-4567ef01")],
                serde_json::json!({
                    "instancesSet": {"items": [{"instanceId": "i-0123abcd"}]},
                }),
            ),
            (
                "a member whose value this service withholds",
                "RunInstances",
                &[("UserData", "a script nobody should read back")],
                serde_json::json!({
                    "instancesSet": {"items": [{"userData": HIDDEN_SENSITIVE_VALUE}]},
                }),
            ),
        ];
        for (case, operation, fields, expected) in rows {
            let document = recorded(operation, fields);
            assert_eq!(
                &document, expected,
                "case {case:?}: expected {expected}, got {document}"
            );
        }
    }

    /// RULE: the resources a record names are the EC2 resources the call's own documents
    /// carry the ids of, each under the resource type CloudTrail publishes for it, named
    /// once, and a value that is no id of a kind the table carries names none.
    #[test]
    fn the_named_resources_are_the_ids_the_documents_carry() {
        let parameters = serde_json::json!({
            "instancesSet": {"items": [{"instanceId": "i-0123456789abcdef0"}]},
            "subnetId": "subnet-0123456789abcdef0",
            // Neither a resource of a kind the table carries nor an id at all.
            "imageId": "ami-0123456789abcdef0",
            "instanceType": "t3.micro",
        });
        let response = serde_json::json!({
            "instancesSet": {"items": [{
                // The same instance the request named, and a volume the call minted.
                "instanceId": "i-0123456789abcdef0",
                "blockDeviceMapping": {"items": [{"ebs": {"volumeId": "vol-0123456789abcdef0"}}]},
            }]},
            "reservationId": "r-0123456789abcdef0",
        });
        let named = audited_resources(&[&parameters, &response]);
        assert_eq!(
            named,
            vec![
                AuditedResource::new("AWS::EC2::Instance", "i-0123456789abcdef0"),
                AuditedResource::new("AWS::EC2::Subnet", "subnet-0123456789abcdef0"),
                AuditedResource::new("AWS::EC2::Volume", "vol-0123456789abcdef0"),
            ],
            "case \"a launch\": expected the instance, its subnet and its volume once each, \
             got {named:?}"
        );

        let empty = audited_resources(&[&serde_json::json!({"cidrBlock": "10.0.0.0/16"})]);
        assert!(
            empty.is_empty(),
            "case \"a document carrying no id\": expected no resource, got {empty:?}"
        );
    }
}
