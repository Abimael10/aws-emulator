// SPDX-License-Identifier: CC0-1.0

//! Amazon EC2's own wire facts, which this crate reaches for the reason Amazon RDS's and
//! Amazon CloudFront's are reached: a caller names an operation of it through the AWS SDK
//! integration FAMILY, and nothing else.
//!
//! Amazon EC2 publishes no optimized integration of its own, so
//! `arn:aws:states:::ec2:<action>` names no `Task` state's work — only the family's own
//! `aws-sdk:ec2:<operation>` does. Its entry is therefore kept out of [`super::APIS`],
//! the table the optimized family is looked up in, and lives here beside the per-operation
//! bindings its protocol needs, so the one concern sits in one file.
//!
//! Its protocol is `ec2Query` (`smithy_query(query="service_info",
//! model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/
//! com-amazonaws-ec2-amazonec2.json")`), which is a FORM protocol like `awsQuery` and
//! differs from it in three things this module answers for:
//!
//! * a LIST is numbered from one under the field the member itself goes under, with no
//!   wrapper field between them (`InstanceId.1`), rather than under `awsQuery`'s wrapped
//!   form;
//! * the field a member goes under is the member's own `ec2QueryName`, else its `xmlName`
//!   with the first letter upper-cased, else the member's name upper-cased the same way —
//!   so a SCALAR member's field is the name a `Task` state's parameters already write
//!   (`MaxResults`, `DryRun`, `ImageId`), while a COLLECTION member's is the singular its
//!   model gives it (`InstanceIds` -> `InstanceId`, `Filters` -> `Filter`). Those names are
//!   facts of the invoked model, which this crate cannot read, so they are STATED per
//!   operation ([`EC2_OPERATIONS`]) and a collection member no entry names is one no call
//!   is made for, exactly as a form member of any other service is;
//! * a 2xx answer carries the operation's members directly inside
//!   `<{Operation}Response>` — there is no `{Operation}Result` wrapper — beside the
//!   `<requestId>` the call is reported under, and a list is the entries of a wrapper
//!   element under the name the list's own member carries, which this model spells `item`
//!   throughout (`smithy_query(query="shape",shape="ReservationList,TagList")`).
//!
//! Its model declares NO error shape at all — every operation's `errors` list is empty
//! (`smithy_query(query="operation",operation="DescribeInstances")`) — while the service
//! refuses calls with codes of its own (`InvalidInstanceID.NotFound`,
//! `IncorrectInstanceState`, `UnauthorizedOperation`). No client therefore has a class for
//! any of those codes, so each refusal resolves to the base exception its client raises
//! and a `Task` state reports it as `Ec2.Ec2Exception` ([`super::Api::models_errors`]).

use super::*;

/// Amazon EC2's ARN namespace, its signing name and its endpoint prefix, which are the
/// same label.
pub(super) const EC2_NAMESPACE: &str = "ec2";

/// Amazon EC2, for the calls this crate makes to it: a `Task` state whose `Resource`
/// names one of its operations through the AWS SDK integration family.
///
/// `services/service-com-amazonaws-ec2-amazonec2/smithy-model/
/// com-amazonaws-ec2-amazonec2.json` (`smithy_query(query="service_info")`): ec2Query,
/// sigv4Name `ec2`, endpointPrefix `ec2`, arnNamespace `ec2`, sdkId `EC2`, version
/// `2016-11-15`.
pub(super) static EC2_API: Api = Api {
    namespace: EC2_NAMESPACE,
    action_prefix: None,
    signing_name: EC2_NAMESPACE,
    endpoint_prefix: EC2_NAMESPACE,
    wire: Wire::Ec2Query {
        version: "2016-11-15",
    },
    sdk_id: "EC2",
    error_prefix: "EC2",
    // The AWS SDK names the service `Ec2`, which its own `Ec2` client carries — the SDK
    // id with the capitalisation that SDK gives it.
    sdk_error_prefix: "Ec2",
    // The client class `Ec2Client`, whose base exception is `Ec2Exception`.
    sdk_client: "Ec2",
    sdk_exception: "Ec2Exception",
    // "Checks whether you have the required permissions for the operation ... If you have
    // the required permissions, the error response is DryRunOperation. Otherwise, it is
    // UnauthorizedOperation" (`DescribeInstancesRequest$DryRun` of that model): the code
    // this service's front door answers a call its caller may not make with, under the
    // Forbidden such an answer carries.
    access_denied_status: TARGET_FORBIDDEN_STATUS,
    access_denied_code: "UnauthorizedOperation",
    lowercases_refused_action: false,
    // `DescribeInstances`' input: `InstanceIds`, `Filters`, `MaxResults`.
    member_case: MemberCase::Pascal,
    // Its model declares the `ec2Query` protocol itself rather than the compatibility
    // modifier, so an error of it is named by its own code and there is no second name to
    // ask for.
    query_compatible: false,
    // Its model declares no error shape at all (see this module's own docs), so every
    // refusal it answers resolves to the base exception of its client.
    models_errors: false,
};

/// How ONE operation of this service binds to its protocol.
///
/// A `Task` state's parameters are the invoked operation's input and its result is that
/// operation's output, and this states the two names that are the invoked MODEL's own
/// rather than the protocol's: the FIELD a member of the request goes on the wire under,
/// and the MEMBER of the output an element of the answer stands for. Each is stated only
/// where it differs from the name the other side already carries, because the protocol
/// derives the rest ([`Ec2Form::field`], [`Ec2Form::member`]).
pub(super) struct Ec2Form {
    /// The operation, as this service's model names it.
    operation: &'static str,
    /// `(member, field)` for each member of the request whose form field is not the
    /// member's own name: its `ec2QueryName`, else its `xmlName` upper-cased — which for
    /// this model is the SINGULAR of a collection member. A member written at any depth
    /// of the request is read here, because one member name is one shape of this model
    /// throughout an operation's input.
    fields: &'static [(&'static str, &'static str)],
    /// `(element, member)` for each element of the answer whose output member is not the
    /// element's own name.
    members: &'static [(&'static str, &'static str)],
}

impl Ec2Form {
    /// The form field the member `member` of this request goes under, and whether this
    /// binding STATES it.
    ///
    /// A member no entry names goes under its own name, which is the field every member
    /// this model declares no rename for carries; whether it was stated is what decides
    /// a COLLECTION member, whose field names are the invoked shape's own
    /// ([`ec2_fields`]).
    fn stated_field<'a>(&self, member: &'a str) -> (&'a str, bool) {
        self.fields
            .iter()
            .find(|(named, _)| *named == member)
            .map_or((member, false), |(_, field)| (*field, true))
    }

    /// The output member the answer element `element` stands for: the one this binding
    /// states, and the element's own name otherwise — which the result is then reported
    /// under the integration's own spelling of ([`super::task_result`]).
    fn member<'a>(&self, element: &'a str) -> &'a str {
        self.members
            .iter()
            .find(|(named, _)| *named == element)
            .map_or(element, |(_, member)| *member)
    }
}

/// The operations of this service a `Task` state's call is made for, each with the names
/// of its own that the protocol does not derive ([`Ec2Form`]).
///
/// Every name below is read from this service's model
/// (`smithy_query(query="operation",operation="<Op>",
/// model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/
/// com-amazonaws-ec2-amazonec2.json")`, and `query="shape"` for the structures its
/// requests nest): a list member's `xmlName` is the singular its field is named by, and an
/// output member's `xmlName` is the element its answer carries it in. An operation with no
/// entry is one no call is made for — where each of its members goes on the wire is that
/// operation's own fact — exactly as an operation of a REST service with no binding is.
const EC2_OPERATIONS: &[Ec2Form] = &[
    Ec2Form {
        operation: "DescribeInstances",
        // `InstanceIds` (xmlName `InstanceId`), `Filters` (xmlName `Filter`), and the
        // `Values` of one `Filter` (xmlName `Value`).
        fields: &[
            ("InstanceIds", "InstanceId"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `Reservations` (xmlName `reservationSet`).
        members: &[("reservationSet", "Reservations")],
    },
    Ec2Form {
        operation: "DescribeInstanceStatus",
        fields: &[
            ("InstanceIds", "InstanceId"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `InstanceStatuses` (xmlName `instanceStatusSet`).
        members: &[("instanceStatusSet", "InstanceStatuses")],
    },
    Ec2Form {
        operation: "RunInstances",
        fields: &[
            ("BlockDeviceMappings", "BlockDeviceMapping"),
            ("Ipv6Addresses", "Ipv6Address"),
            ("LicenseSpecifications", "LicenseSpecification"),
            ("NetworkInterfaces", "NetworkInterface"),
            ("SecurityGroupIds", "SecurityGroupId"),
            ("SecurityGroups", "SecurityGroup"),
            ("TagSpecifications", "TagSpecification"),
            ("Tags", "Tag"),
        ],
        // Its output is the `Reservation` shape: `Groups` (xmlName `groupSet`) and
        // `Instances` (xmlName `instancesSet`).
        members: &[("groupSet", "Groups"), ("instancesSet", "Instances")],
    },
    Ec2Form {
        operation: "StartInstances",
        fields: &[("InstanceIds", "InstanceId")],
        // `StartingInstances` (xmlName `instancesSet`).
        members: &[("instancesSet", "StartingInstances")],
    },
    Ec2Form {
        operation: "StopInstances",
        fields: &[("InstanceIds", "InstanceId")],
        // `StoppingInstances` (xmlName `instancesSet`).
        members: &[("instancesSet", "StoppingInstances")],
    },
    Ec2Form {
        operation: "RebootInstances",
        fields: &[("InstanceIds", "InstanceId")],
        // Its output carries no member at all.
        members: &[],
    },
    Ec2Form {
        operation: "TerminateInstances",
        fields: &[("InstanceIds", "InstanceId")],
        // `TerminatingInstances` (xmlName `instancesSet`).
        members: &[("instancesSet", "TerminatingInstances")],
    },
    Ec2Form {
        operation: "CreateTags",
        // `Resources` (xmlName `ResourceId`) and `Tags` (xmlName `Tag`).
        fields: &[("Resources", "ResourceId"), ("Tags", "Tag")],
        members: &[],
    },
    Ec2Form {
        operation: "DeleteTags",
        // `Resources` (ec2QueryName `ResourceId`) and `Tags` (ec2QueryName `Tag`).
        fields: &[("Resources", "ResourceId"), ("Tags", "Tag")],
        members: &[],
    },
    Ec2Form {
        operation: "DescribeTags",
        fields: &[("Filters", "Filter"), ("Values", "Value")],
        // `Tags` (xmlName `tagSet`).
        members: &[("tagSet", "Tags")],
    },
    Ec2Form {
        operation: "CreateSnapshot",
        fields: &[("TagSpecifications", "TagSpecification"), ("Tags", "Tag")],
        // Its output is the `Snapshot` shape: `Tags` (xmlName `tagSet`), `State`
        // (xmlName `status`) and `StateMessage` (xmlName `statusMessage`).
        members: &[
            ("tagSet", "Tags"),
            ("status", "State"),
            ("statusMessage", "StateMessage"),
        ],
    },
    Ec2Form {
        operation: "DescribeSnapshots",
        fields: &[
            ("OwnerIds", "Owner"),
            ("RestorableByUserIds", "RestorableBy"),
            ("SnapshotIds", "SnapshotId"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `Snapshots` (xmlName `snapshotSet`).
        members: &[("snapshotSet", "Snapshots")],
    },
    Ec2Form {
        operation: "DescribeVolumes",
        fields: &[
            ("VolumeIds", "VolumeId"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `Volumes` (xmlName `volumeSet`).
        members: &[("volumeSet", "Volumes")],
    },
    Ec2Form {
        operation: "DescribeImages",
        fields: &[
            ("ExecutableUsers", "ExecutableBy"),
            ("ImageIds", "ImageId"),
            ("Owners", "Owner"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `Images` (xmlName `imagesSet`).
        members: &[("imagesSet", "Images")],
    },
    Ec2Form {
        operation: "DescribeSecurityGroups",
        fields: &[
            ("GroupIds", "GroupId"),
            ("GroupNames", "GroupName"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `SecurityGroups` (xmlName `securityGroupInfo`).
        members: &[("securityGroupInfo", "SecurityGroups")],
    },
    Ec2Form {
        operation: "DescribeSubnets",
        fields: &[
            ("SubnetIds", "SubnetId"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `Subnets` (xmlName `subnetSet`).
        members: &[("subnetSet", "Subnets")],
    },
    Ec2Form {
        operation: "DescribeVpcs",
        fields: &[
            ("VpcIds", "VpcId"),
            ("Filters", "Filter"),
            ("Values", "Value"),
        ],
        // `Vpcs` (xmlName `vpcSet`).
        members: &[("vpcSet", "Vpcs")],
    },
];

/// The binding of the operation `operation` of this service, or `None` for an operation
/// no call is made for ([`EC2_OPERATIONS`]).
pub(super) fn form(operation: &str) -> Option<&'static Ec2Form> {
    EC2_OPERATIONS
        .iter()
        .find(|form| form.operation == operation)
}

/// The `application/x-www-form-urlencoded` body one call to this service carries:
/// `Action` naming the operation, `Version` naming the service's API version, and one
/// field per LEAF of the operation's input.
///
/// Each field name and each value is percent-encoded by the maintained encoder over RFC
/// 3986's unreserved set, exactly as the other form protocol's body is
/// ([`super::form_body`]), so a value carrying a delimiter is one field rather than
/// several.
pub(super) fn ec2_form_body(
    form: &Ec2Form,
    version: &str,
    document: &Value,
) -> Result<String, String> {
    let mut body = format!("Action={}&Version={version}", form.operation);
    for (member, value) in document.as_object().into_iter().flatten() {
        let (field, stated) = form.stated_field(member);
        let mut fields = Vec::new();
        ec2_fields(form, &encoded(field), value, stated, 1, &mut fields)?;
        for (field, value) in fields {
            body.push('&');
            body.push_str(&field);
            body.push('=');
            body.push_str(&encoded(&value));
        }
    }
    Ok(body)
}

/// The fields the node `name` of such a request goes on the wire as, appended to `fields`
/// as (already-encoded name, raw value) pairs.
///
/// A LIST is the one place `stated` is read: this protocol numbers its entries from one
/// under the field the member itself goes under, and that field is the singular name the
/// invoked model gives the member — a fact this crate cannot read, so a list whose field
/// no binding states is NOT bound and no call is made for it, rather than one sent under a
/// field the service does not carry. Every other node needs no name of the model's: a
/// structure's members are one field each below it, and a scalar is the value itself.
///
/// The descent is bounded by [`super::super::asl::MAX_PAYLOAD_DEPTH`] rather than by the
/// thread's stack, as every other walk of a document a caller shaped is: the document is
/// one a `Task` state's payload template built, so its nesting is the caller's.
fn ec2_fields(
    form: &Ec2Form,
    name: &str,
    value: &Value,
    stated: bool,
    depth: usize,
    fields: &mut Vec<(String, String)>,
) -> Result<(), String> {
    if depth > super::super::asl::MAX_PAYLOAD_DEPTH {
        return Err(format!(
            "this task's '{name}' nests more than the {} levels this service binds to that \
             service's form-field protocol",
            super::super::asl::MAX_PAYLOAD_DEPTH
        ));
    }
    match value {
        // A member the request did not carry is no field at all.
        Value::Null => Ok(()),
        Value::String(text) => {
            fields.push((name.to_string(), text.clone()));
            Ok(())
        }
        Value::Bool(_) | Value::Number(_) => {
            fields.push((name.to_string(), value.to_string()));
            Ok(())
        }
        Value::Array(items) => {
            if !stated {
                return Err(super::unbound_form_member(name, "list"));
            }
            for (index, item) in items.iter().enumerate() {
                // An entry of a list is numbered from ONE, and it is the same node as
                // the list it is in: a list of structures carries each structure's
                // members below its own number.
                ec2_fields(
                    form,
                    &format!("{name}.{}", index + 1),
                    item,
                    stated,
                    depth + 1,
                    fields,
                )?;
            }
            Ok(())
        }
        Value::Object(members) => {
            for (member, held) in members {
                let (field, stated) = form.stated_field(member);
                ec2_fields(
                    form,
                    &format!("{name}.{}", encoded(field)),
                    held,
                    stated,
                    depth + 1,
                    fields,
                )?;
            }
            Ok(())
        }
    }
}

/// What a 2xx answer of this service carries, as the invoked operation's own output
/// document.
///
/// The members are the children of the `<{Operation}Response>` element — this protocol
/// wraps them in no result element — each under the output member it stands for
/// ([`Ec2Form::member`]). The `<requestId>` beside them is the call's own identifier
/// rather than a member of the output, so it is not part of the result, exactly as the
/// other form protocol's `<ResponseMetadata>` is not.
pub(super) fn ec2_result(operation: &str, payload: &[u8]) -> Answer {
    let unreadable = |why: String| Failed {
        error: super::super::asl::RUNTIME_ERROR.to_string(),
        cause: format!("the answer to the '{operation}' call could not be read: {why}"),
    };
    let form = form(operation).ok_or_else(|| {
        unreadable(format!("'{operation}' is not an operation this emulator calls"))
    })?;
    let root = crate::state::xml_answer::read(payload).map_err(unreadable)?;
    let mut reported = Map::new();
    for child in &root.children {
        if child.name == REQUEST_ID_ELEMENT {
            continue;
        }
        reported.insert(form.member(&child.name).to_string(), ec2_value(child));
    }
    Ok(Value::Object(reported))
}

/// The element such an answer names the call's own identifier by, which this protocol
/// carries beside the operation's members rather than in a metadata element.
const REQUEST_ID_ELEMENT: &str = "requestId";

/// The element the entries of a wrapped LIST are carried under, which is the name the
/// list's own member carries in this model — `item` for every list of it
/// (`smithy_query(query="shape",shape="ReservationList,InstanceStateChangeList,TagList")`).
const LIST_ITEM_ELEMENT: &str = "item";

/// `element` as the JSON document its tree carries, with a wrapped LIST read as the array
/// it is.
///
/// Everything else is read as this crate reads any XML answer
/// ([`crate::state::xml_answer::as_value`]): an element with no children is the text it
/// holds, one with children is the object of those children, and every value is a STRING
/// because that is what an XML document carries — which of them the invoked operation
/// models as a number or a boolean is a fact of that operation's own model, which this
/// crate cannot read.
///
/// The descent is bounded by the PARSE that built the tree, which refuses a document
/// nested deeper than the levels it reads ([`crate::state::xml_answer::read`]), so an
/// answer cannot be read into the thread's stack.
fn ec2_value(element: &crate::state::xml_answer::Element) -> Value {
    if element.children.is_empty() {
        return Value::String(element.text.clone());
    }
    if element
        .children
        .iter()
        .all(|child| child.name == LIST_ITEM_ELEMENT)
    {
        return Value::Array(element.children.iter().map(ec2_value).collect());
    }
    let mut members = Map::new();
    for child in &element.children {
        let value = ec2_value(child);
        match members.get_mut(&child.name) {
            // A name the element repeats outside the wrapped form above is the array
            // repeating it means, exactly as it is for any other XML answer.
            Some(Value::Array(values)) => values.push(value),
            Some(held) => {
                let first = held.clone();
                members.insert(child.name.clone(), Value::Array(vec![first, value]));
            }
            None => {
                members.insert(child.name.clone(), value);
            }
        }
    }
    Value::Object(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request one call to this service sends: `Action` and `Version` naming the
    /// call, a scalar as the field its own member name is, and a list numbered from one
    /// under the singular field the model gives that member.
    #[test]
    fn a_request_is_the_form_this_protocol_spreads_the_input_over() {
        // (case, the operation, the state's parameters, the fields the body must carry)
        let rows: &[(&str, &str, Value, &[&str])] = &[
            (
                "a list of one, under the singular field its member is named by",
                "DescribeInstances",
                ::serde_json::json!({ "InstanceIds": ["i-0abc"] }),
                &["Action=DescribeInstances", "Version=2016-11-15", "InstanceId.1=i-0abc"],
            ),
            (
                "a list of three, numbered from one in the order it was written",
                "TerminateInstances",
                ::serde_json::json!({ "InstanceIds": ["i-1", "i-2", "i-3"] }),
                &["InstanceId.1=i-1", "InstanceId.2=i-2", "InstanceId.3=i-3"],
            ),
            (
                "a scalar member, whose field is the member's own name",
                "DescribeInstances",
                ::serde_json::json!({ "MaxResults": 5, "DryRun": false }),
                &["MaxResults=5", "DryRun=false"],
            ),
            (
                "a list of structures, each carrying a list of its own",
                "DescribeInstances",
                ::serde_json::json!({
                    "Filters": [{ "Name": "instance-state-name", "Values": ["running", "stopped"] }],
                }),
                &[
                    "Filter.1.Name=instance-state-name",
                    "Filter.1.Value.1=running",
                    "Filter.1.Value.2=stopped",
                ],
            ),
            (
                "a value carrying a character a form field escapes",
                "CreateTags",
                ::serde_json::json!({
                    "Resources": ["i-1"],
                    "Tags": [{ "Key": "owner", "Value": "a&b c" }],
                }),
                &["ResourceId.1=i-1", "Tag.1.Key=owner", "Tag.1.Value=a%26b%20c"],
            ),
        ];
        for (case, operation, parameters, expected) in rows {
            let form = form(operation).unwrap_or_else(|| panic!("case {case:?}: {operation} binds"));
            let body = ec2_form_body(form, "2016-11-15", parameters)
                .unwrap_or_else(|why| panic!("case {case:?}: the body builds, got {why}"));
            for field in *expected {
                assert!(
                    body.split('&').any(|carried| carried == *field),
                    "case {case:?}: expected the field {field:?}, got {body:?}"
                );
            }
        }
    }

    /// A COLLECTION member no binding states the field names of is one no call is made
    /// for: those names are the invoked shape's own.
    #[test]
    fn a_list_whose_field_is_not_stated_is_not_bound() {
        let form = form("DescribeInstances").expect("DescribeInstances binds");
        let built = ec2_form_body(
            form,
            "2016-11-15",
            &::serde_json::json!({ "InstanceIdentifiers": ["i-1"] }),
        );
        let why = built.expect_err("an unstated list binds to no field");
        assert!(
            why.contains("InstanceIdentifiers") && why.contains("list"),
            "expected the unbound member and its kind, got {why:?}"
        );
    }

    /// A 2xx answer is read as the operation's own output: the member each element stands
    /// for, the entries of a wrapped list as the array they are, and the call's own
    /// identifier left out of the result.
    #[test]
    fn an_answer_is_read_as_the_operations_own_output() {
        let payload = br#"<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15">
            <requestId>r-1</requestId>
            <reservationSet>
                <item>
                    <reservationId>r-0a</reservationId>
                    <instancesSet>
                        <item><instanceId>i-1</instanceId></item>
                        <item><instanceId>i-2</instanceId></item>
                    </instancesSet>
                </item>
            </reservationSet>
            <nextToken>t-2</nextToken>
        </DescribeInstancesResponse>"#;
        let read = ec2_result("DescribeInstances", payload).expect("the answer reads");
        assert_eq!(
            read,
            ::serde_json::json!({
                "Reservations": [{
                    "reservationId": "r-0a",
                    "instancesSet": [
                        { "instanceId": "i-1" },
                        { "instanceId": "i-2" },
                    ],
                }],
                "nextToken": "t-2",
            }),
            "expected the output members this operation declares, got {read}"
        );
    }

    /// Every entry of the table names an operation of this service once, and every field
    /// and member it states is a rename rather than the name the protocol already
    /// derives.
    #[test]
    fn each_binding_states_only_the_names_the_protocol_does_not_derive() {
        let mut seen: Vec<&str> = Vec::new();
        for form in EC2_OPERATIONS {
            assert!(
                !seen.contains(&form.operation),
                "case {:?}: one entry per operation, got it twice",
                form.operation
            );
            seen.push(form.operation);
            assert!(
                form.operation.starts_with(char::is_uppercase),
                "case {:?}: an operation of this model is named in PascalCase",
                form.operation
            );
            for (member, field) in form.fields {
                assert_ne!(
                    member, field,
                    "case {:?}: a member whose field is its own name is not stated",
                    form.operation
                );
            }
            for (element, member) in form.members {
                assert_ne!(
                    MemberCase::Pascal.spell(element).as_str(),
                    *member,
                    "case {:?}: an element whose member is its own name is not stated",
                    form.operation
                );
            }
        }
    }
}
