// SPDX-License-Identifier: CC0-1.0

//! A stack operation that re-declares a resource the stack ALREADY has, with a changed
//! configuration, end to end over the awsQuery wire.
//!
//! The rule every case here holds the service to: **a deployment that re-declares an
//! existing resource with a different configuration applies that change through the owning
//! service's own update API — the resource keeps its identity and its handle, the owning
//! service really holds the new configuration, and the stack operation reaches its COMPLETED
//! status; a change this emulator cannot apply fails the operation naming the resource rather
//! than completing while the caller's change went nowhere.**
//!
//! `AWS::SNS::Topic` is the type the rows are driven through: a topic's configuration is its
//! ATTRIBUTES plus its tags, each with one Amazon SNS call that writes it on a live topic, so
//! the change reaches SNS rather than replacing the topic. The rows differ along what the
//! rule generalises over — a property added, a property re-valued, several at once, a value
//! written as a string and as an object, a tag added and a tag dropped, a property the
//! revision stops declaring (which goes back to that attribute's documented absent value),
//! the change this emulator cannot apply, and both operations that deploy a revision
//! (`UpdateStack` and `ExecuteChangeSet`) — and every expected value is computed from what
//! the row declares, never read back out of the answer being checked.
//!
//! The recorded traces this suite was written for are two rows of it: a change set created
//! over a revised one-topic template, executed, and polled for the stack's completed status;
//! and the no-op revision whose change set settles `FAILED` and is refused execution.

use crate::harness::{xml_blocks, xml_texts};
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, ec2, member, router, sns, wait_for_the_transition,
    xml_text,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, failed_action, resource_failure_summary, status, NO_CHANGES_REASON,
};
use smithy_server_runtime::MultiServiceRouter;

/// A one-topic template whose topic declares `properties` beside its name.
fn topic_template(topic_name: &str, properties: &str) -> String {
    format!(
        r#"{{"Resources":{{"Topic":{{"Type":"AWS::SNS::Topic","Properties":{{"TopicName":"{topic_name}"{properties}}}}}}}}}"#
    )
}

/// The `StackStatus` a stack reports right now.
async fn status_of(router: &MultiServiceRouter, stack: &str) -> String {
    let (code, body) = describe_stack(router, stack).await;
    assert_eq!(code, 200, "DescribeStacks {stack:?}: body {body}");
    xml_text(&body, "StackStatus").unwrap_or_default()
}

/// The `StackStatusReason` a stack reports right now.
async fn reason_of(router: &MultiServiceRouter, stack: &str) -> String {
    let (_, body) = describe_stack(router, stack).await;
    xml_text(&body, "StackStatusReason").unwrap_or_default()
}

/// The attributes Amazon SNS holds for the topic `arn` names.
async fn topic_attributes(case: &str, router: &MultiServiceRouter, arn: &str) -> Vec<(String, String)> {
    let (code, body) = sns(router, "GetTopicAttributes", &[("TopicArn", arn)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetTopicAttributes to answer for {arn:?}, got {code} {body}"
    );
    xml_blocks(&body, "entry")
        .iter()
        .filter_map(|entry| {
            Some((
                xml_text(entry, "key")?,
                xml_text(entry, "value").unwrap_or_default(),
            ))
        })
        .collect()
}

/// The tags Amazon SNS holds for the topic `arn` names.
async fn topic_tags(case: &str, router: &MultiServiceRouter, arn: &str) -> Vec<(String, String)> {
    let (code, body) = sns(router, "ListTagsForResource", &[("ResourceArn", arn)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected ListTagsForResource to answer for {arn:?}, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter_map(|entry| {
            Some((
                xml_text(entry, "Key")?,
                xml_text(entry, "Value").unwrap_or_default(),
            ))
        })
        .collect()
}

/// The ARN of the topic the stack published for its `Topic` resource — the handle a client
/// reaches it by, which an in-place update must not change.
async fn topic_arn_of(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResource",
        &[member("StackName", stack), member("LogicalResourceId", "Topic")],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackResource to answer, got {code} {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// One row of the in-place update table.
struct UpdateCase {
    case: &'static str,
    stack: &'static str,
    topic: &'static str,
    /// The properties the CREATE declares, beside the topic's name.
    created: &'static str,
    /// The properties the revision declares, beside the topic's name.
    revised: &'static str,
    /// The topic attributes Amazon SNS must hold once the revision is deployed.
    expected_attributes: &'static [(&'static str, &'static str)],
    /// The tags Amazon SNS must hold once the revision is deployed.
    expected_tags: &'static [(&'static str, &'static str)],
}

/// The class the rule covers: a revision that states a different configuration for a
/// resource the stack already holds.
fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            case: "a property the create did not declare at all",
            stack: "inplace-added",
            topic: "inplace-added-topic",
            created: "",
            revised: r#","DisplayName":"added""#,
            expected_attributes: &[("DisplayName", "added")],
            expected_tags: &[],
        },
        UpdateCase {
            case: "a property the create declared, re-valued",
            stack: "inplace-revalued",
            topic: "inplace-revalued-topic",
            created: r#","DisplayName":"before""#,
            revised: r#","DisplayName":"after""#,
            expected_attributes: &[("DisplayName", "after")],
            expected_tags: &[],
        },
        UpdateCase {
            case: "several attributes at once, one of them a closed value set",
            stack: "inplace-several",
            topic: "inplace-several-topic",
            created: r#","DisplayName":"before","TracingConfig":"PassThrough""#,
            revised: r#","DisplayName":"after","TracingConfig":"Active","SignatureVersion":"2""#,
            expected_attributes: &[
                ("DisplayName", "after"),
                ("TracingConfig", "Active"),
                ("SignatureVersion", "2"),
            ],
            expected_tags: &[],
        },
        UpdateCase {
            case: "an attribute whose declared value is an OBJECT, not a string",
            stack: "inplace-object",
            topic: "inplace-object-topic",
            created: r#","DisplayName":"policy-holder""#,
            revised: r#","DisplayName":"policy-holder","DeliveryPolicy":{"http":{"defaultHealthyRetryPolicy":{"numRetries":3}}}"#,
            expected_attributes: &[
                ("DisplayName", "policy-holder"),
                (
                    "DeliveryPolicy",
                    r#"{"http":{"defaultHealthyRetryPolicy":{"numRetries":3}}}"#,
                ),
            ],
            expected_tags: &[],
        },
        UpdateCase {
            case: "a tag added, a tag re-valued and a tag dropped",
            stack: "inplace-tags",
            topic: "inplace-tags-topic",
            created: r#","Tags":[{"Key":"kept","Value":"one"},{"Key":"dropped","Value":"two"}]"#,
            revised: r#","Tags":[{"Key":"kept","Value":"changed"},{"Key":"added","Value":"three"}]"#,
            expected_attributes: &[],
            expected_tags: &[("added", "three"), ("kept", "changed")],
        },
    ]
}

#[tokio::test]
async fn an_update_applies_a_changed_declaration_in_the_owning_service() {
    for case in update_cases() {
        let UpdateCase {
            case,
            stack,
            topic,
            created,
            revised,
            expected_attributes,
            expected_tags,
        } = case;
        let router = router();
        let (code, body) = create_stack(&router, stack, &topic_template(topic, created), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to succeed, got {code} {body}"
        );
        wait_for_the_transition().await;
        let reported = status_of(&router, stack).await;
        assert_eq!(
            reported,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {reported:?}"
        );
        let arn = topic_arn_of(case, &router, stack).await;

        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &topic_template(topic, revised)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected UpdateStack to succeed, got {code} {body}"
        );
        wait_for_the_transition().await;
        let reported = status_of(&router, stack).await;
        assert_eq!(
            reported,
            status::UPDATE_COMPLETE,
            "case {case:?}: expected the update to complete, got {reported:?} (reason {:?})",
            reason_of(&router, stack).await
        );

        // The resource kept its identity: the handle the stack publishes is the one it
        // published before the update, so nothing was replaced.
        let after = topic_arn_of(case, &router, stack).await;
        assert_eq!(
            after, arn,
            "case {case:?}: expected the topic to keep its ARN across an in-place update, \
             got {after:?}"
        );

        // ...and Amazon SNS really holds the declared configuration.
        let held = topic_attributes(case, &router, &arn).await;
        for (name, value) in expected_attributes {
            let found = held
                .iter()
                .find(|(held_name, _)| held_name == name)
                .map(|(_, held_value)| held_value.clone());
            assert_eq!(
                found.as_deref(),
                Some(*value),
                "case {case:?}: expected Amazon SNS to hold {name}={value:?}, got {found:?} \
                 (attributes {held:?})"
            );
        }
        if !expected_tags.is_empty() {
            let tags = topic_tags(case, &router, &arn).await;
            // The tags CloudFormation PROPAGATES to the resource are on the topic too; what
            // this row is about is the declaration's own, so the declared set is checked
            // pair by pair and a key the revision dropped must be gone.
            for (key, value) in expected_tags {
                let found = tags
                    .iter()
                    .find(|(held_key, _)| held_key == key)
                    .map(|(_, held_value)| held_value.clone());
                assert_eq!(
                    found.as_deref(),
                    Some(*value),
                    "case {case:?}: expected Amazon SNS to hold the tag {key}={value:?}, got \
                     {found:?} (tags {tags:?})"
                );
            }
            assert!(
                !tags.iter().any(|(key, _)| key == "dropped"),
                "case {case:?}: expected the tag the revision no longer declares to be \
                 removed, got {tags:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_dropped_property_goes_back_to_its_documented_absent_value() {
    // A property the revision no longer declares: the topic goes back to the value Amazon
    // SNS documents as that attribute's absent state, through the same call that writes it,
    // so the topic is configured as the revision declares it rather than keeping a property
    // the caller removed.
    let router = router();
    let template = topic_template("inplace-dropped-topic", r#","DisplayName":"before""#);
    let (code, body) = create_stack(&router, "inplace-dropped", &template, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    wait_for_the_transition().await;
    let arn = topic_arn_of("dropped", &router, "inplace-dropped").await;

    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "inplace-dropped"),
            member("TemplateBody", &topic_template("inplace-dropped-topic", "")),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    wait_for_the_transition().await;
    let reported = status_of(&router, "inplace-dropped").await;
    assert_eq!(
        reported,
        status::UPDATE_COMPLETE,
        "expected the update that drops a property to complete, got {reported:?} (reason {:?})",
        reason_of(&router, "inplace-dropped").await
    );
    let held = topic_attributes("dropped", &router, &arn).await;
    let display_name = held
        .iter()
        .find(|(name, _)| name == "DisplayName")
        .map(|(_, value)| value.clone())
        .unwrap_or_default();
    assert_eq!(
        display_name, "",
        "expected the dropped display name to go back to its absent value, got \
         {display_name:?} (attributes {held:?})"
    );
}

#[tokio::test]
async fn a_resource_whose_physical_name_cloudformation_generated_is_updated_in_place() {
    // The declaration names NO physical name, so CloudFormation generates one — and the
    // revision names none either, so the resource keeps the generated name and the change is
    // applied to the resource that exists. The row differs from the table above only in that
    // one respect, which is the one the generated-name path turns on.
    let router = router();
    let created = r#"{"Resources":{"Topic":{"Type":"AWS::SNS::Topic","Properties":{"DisplayName":"orig-display-name"}}}}"#;
    let (code, body) = create_stack(&router, "generated-name-inplace", created, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    wait_for_the_transition().await;
    let reported = status_of(&router, "generated-name-inplace").await;
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "expected the create to complete, got {reported:?} (reason {:?})",
        reason_of(&router, "generated-name-inplace").await
    );
    let arn = topic_arn_of("a generated name", &router, "generated-name-inplace").await;

    // A REFUSED submit in between: the body is truncated, so the request is refused and
    // nothing is written — the stack is still the one the create left, which is what the
    // valid revision below is applied to.
    let truncated = r#"{"Resources": {"Topic": {"Type": "AWS::SNS::Topic", "Properties": {"#;
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "generated-name-inplace"),
            member("TemplateBody", truncated),
        ],
    )
    .await;
    assert_eq!(
        code, 400,
        "expected the truncated body to be refused, got {code} {body}"
    );
    let reported = status_of(&router, "generated-name-inplace").await;
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "expected the refused update to leave the stack where it was, got {reported:?}"
    );

    let revised = r#"{"Resources":{"Topic":{"Type":"AWS::SNS::Topic","Properties":{"DisplayName":"updated-display-name"}}}}"#;
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "generated-name-inplace"),
            member("TemplateBody", revised),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    wait_for_the_transition().await;
    let reported = status_of(&router, "generated-name-inplace").await;
    assert_eq!(
        reported,
        status::UPDATE_COMPLETE,
        "expected the update to complete, got {reported:?} (reason {:?})",
        reason_of(&router, "generated-name-inplace").await
    );
    let after = topic_arn_of("a generated name", &router, "generated-name-inplace").await;
    assert_eq!(
        after, arn,
        "expected the topic to keep the generated name it was created with, got {after:?}"
    );
    let held = topic_attributes("a generated name", &router, &arn).await;
    assert!(
        held.iter()
            .any(|(name, value)| name == "DisplayName" && value == "updated-display-name"),
        "expected Amazon SNS to hold the revised display name, got {held:?}"
    );
}

#[tokio::test]
async fn a_change_this_emulator_cannot_apply_fails_the_update_naming_the_resource() {
    // The inline `Subscription` list is not a property of the topic: each entry is a
    // subscription of Amazon SNS's own, addressed by an ARN this resource's record does not
    // carry, so a revision that changes the list fails the operation naming the resource
    // rather than completing while the caller's change went nowhere.
    let router = router();
    let subscribed = r#","Subscription":[{"Protocol":"email","Endpoint":"one@example.com"}]"#;
    let template = topic_template("inplace-subscribed-topic", subscribed);
    let (code, body) = create_stack(&router, "inplace-subscribed", &template, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    wait_for_the_transition().await;

    let revised = r#","Subscription":[{"Protocol":"email","Endpoint":"two@example.com"}]"#;
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "inplace-subscribed"),
            member(
                "TemplateBody",
                &topic_template("inplace-subscribed-topic", revised),
            ),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    // Polled for: the update's conclusion and the rollback it owes are each due at an
    // instant this service records.
    let settled =
        crate::stack_lifecycle::settled_stack(&router, "inplace-subscribed", "inplace-subscribed")
            .await;
    let reported = xml_text(&settled, "StackStatus").unwrap_or_default();
    // The update failed and is therefore UNDONE: rollback is at its default, so the stack
    // reaches the rolled-back terminal status rather than parking at UPDATE_FAILED.
    assert_eq!(
        reported,
        status::UPDATE_ROLLBACK_COMPLETE,
        "expected an update this emulator cannot apply to fail and roll back, got {reported:?}"
    );
    let reason = reason_of(&router, "inplace-subscribed").await;
    let expected = resource_failure_summary(failed_action::UPDATE, ["Topic"]);
    assert_eq!(
        reason, expected,
        "expected the failure to name the resource it could not update, got {reason:?}"
    );
}

#[tokio::test]
async fn a_change_set_executed_over_a_revised_topic_completes_and_a_no_op_set_is_refused() {
    let router = router();
    let template = topic_template("changeset-topic", "");
    let (code, body) = create_stack(&router, "changeset-inplace", &template, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    wait_for_the_transition().await;

    let revised = topic_template("changeset-topic", r#","DisplayName":"by-change-set""#);
    let (code, body) = call(
        &router,
        "CreateChangeSet",
        &[
            member("StackName", "changeset-inplace"),
            member("ChangeSetName", "revision"),
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &revised),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected CreateChangeSet to succeed, got {code} {body}");
    let change_set_id = xml_text(&body, "Id").unwrap_or_default();
    wait_for_the_transition().await;
    let (_, described) = call(
        &router,
        "DescribeChangeSet",
        &[member("ChangeSetName", &change_set_id)],
    )
    .await;
    assert_eq!(
        xml_text(&described, "Status").as_deref(),
        Some(change_set_status::CREATE_COMPLETE),
        "expected the change set with a real diff to settle CREATE_COMPLETE, got {described}"
    );

    let (code, body) = call(
        &router,
        "ExecuteChangeSet",
        &[member("ChangeSetName", &change_set_id)],
    )
    .await;
    assert_eq!(code, 200, "expected ExecuteChangeSet to succeed, got {code} {body}");
    wait_for_the_transition().await;
    let reported = status_of(&router, "changeset-inplace").await;
    assert_eq!(
        reported,
        status::UPDATE_COMPLETE,
        "expected the executed change set to complete the stack, got {reported:?} (reason {:?})",
        reason_of(&router, "changeset-inplace").await
    );
    let arn = topic_arn_of("change set", &router, "changeset-inplace").await;
    let held = topic_attributes("change set", &router, &arn).await;
    assert!(
        held.iter()
            .any(|(name, value)| name == "DisplayName" && value == "by-change-set"),
        "expected Amazon SNS to hold the display name the executed change set declared, got \
         {held:?}"
    );

    // The same revision again changes nothing, so its change set settles FAILED with the
    // reason CloudFormation gives — and executing it is refused with the change set's own
    // ARN and current status.
    let (code, body) = call(
        &router,
        "CreateChangeSet",
        &[
            member("StackName", "changeset-inplace"),
            member("ChangeSetName", "no-op"),
            member("ChangeSetType", "UPDATE"),
            member("TemplateBody", &revised),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected CreateChangeSet to succeed, got {code} {body}");
    let no_op_id = xml_text(&body, "Id").unwrap_or_default();
    wait_for_the_transition().await;
    let (_, described) = call(
        &router,
        "DescribeChangeSet",
        &[member("ChangeSetName", &no_op_id)],
    )
    .await;
    assert_eq!(
        xml_text(&described, "Status").as_deref(),
        Some(change_set_status::FAILED),
        "expected the no-op change set to settle FAILED, got {described}"
    );
    assert_eq!(
        xml_text(&described, "StatusReason").as_deref(),
        Some(NO_CHANGES_REASON),
        "expected the no-op change set to carry the reason CloudFormation gives, got \
         {described}"
    );
    let (code, body) = call(
        &router,
        "ExecuteChangeSet",
        &[member("ChangeSetName", &no_op_id)],
    )
    .await;
    assert_eq!(
        code, 400,
        "expected executing a FAILED change set to be refused, got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "Code").as_deref(),
        Some("InvalidChangeSetStatus"),
        "expected the modelled InvalidChangeSetStatus code, got {body}"
    );
    assert_eq!(
        xml_text(&body, "Message"),
        Some(format!(
            "ChangeSet [{no_op_id}] cannot be executed in its current status of [{}]",
            change_set_status::FAILED
        )),
        "expected the refusal to name the change set and its current status, got {body}"
    );
    // The refused execution left the stack exactly where it was.
    let reported = status_of(&router, "changeset-inplace").await;
    assert_eq!(
        reported,
        status::UPDATE_COMPLETE,
        "expected the refused execution to leave the stack alone, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// The same rule over the Amazon EC2 estate a deploy-then-operate template declares.
//
// Every property AWS publishes as updatable on one of these types has a call Amazon EC2
// serves for changing it on the resource that EXISTS — a VPC's and a subnet's switches
// through `Modify…Attribute`, a gateway attachment's gateway through the detach and attach
// pair, a route's target through `ReplaceRoute`, a security group's rules through the
// authorize and revoke for their direction, every taggable resource's tags through
// `CreateTags`/`DeleteTags` — so a revision that states one differently is applied to that
// resource: its physical id does not move, the stack reaches `UPDATE_COMPLETE`, and EC2 holds
// what the revision declared. A property NO such call carries is create-only, and a revision
// restating one REPLACES the resource instead.
// ---------------------------------------------------------------------------

/// The values one revision of the Amazon EC2 estate below declares: each one a property of a
/// different type, and each one a property AWS publishes as updatable with no interruption.
#[derive(Debug, Clone, Copy)]
struct Estate {
    /// The VPC's `EnableDnsHostnames`, a VPC ATTRIBUTE `ModifyVpcAttribute` writes.
    dns_hostnames: bool,
    /// The subnet's `MapPublicIpOnLaunch`, a subnet attribute `ModifySubnetAttribute` writes.
    map_public_ip: bool,
    /// The value of the `estate` tag every taggable resource of the template carries.
    tag: &'static str,
    /// The logical id of the INTERNET GATEWAY the `AWS::EC2::VPCGatewayAttachment` puts on
    /// the VPC: swapping it is a detach of the one in force and an attach of the one declared,
    /// on the attachment that exists — whose identity is the KIND of gateway attached and the
    /// VPC it is attached to, so a swap between two gateways of one kind keeps it.
    gateway: &'static str,
    /// The logical id of the resource the `AWS::EC2::Route` points at — an internet gateway
    /// (the route's `GatewayId`) or an elastic network interface (its `NetworkInterfaceId`).
    route_target: &'static str,
}

/// The estate one revision declares: a VPC with both DNS switches, a subnet, an internet
/// gateway and a virtual private gateway with the attachment that puts ONE of them on the VPC,
/// a route table with a default route, and the network interface that route can point at
/// instead.
fn estate_template(estate: &Estate) -> String {
    let Estate {
        dns_hostnames,
        map_public_ip,
        tag,
        gateway,
        route_target,
    } = *estate;
    let attached = format!(r#""InternetGatewayId":{{"Ref":"{gateway}"}}"#);
    let target = if route_target == "Igw" {
        r#""GatewayId":{"Ref":"Igw"}"#.to_string()
    } else {
        r#""NetworkInterfaceId":{"Ref":"Eni"}"#.to_string()
    };
    format!(
        r#"{{"Resources":{{
          "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.20.0.0/16",
            "EnableDnsSupport":true,"EnableDnsHostnames":{dns_hostnames},
            "Tags":[{{"Key":"estate","Value":"{tag}"}}]}}}},
          "Subnet":{{"Type":"AWS::EC2::Subnet","Properties":{{"VpcId":{{"Ref":"Vpc"}},
            "CidrBlock":"10.20.1.0/24","MapPublicIpOnLaunch":{map_public_ip},
            "Tags":[{{"Key":"estate","Value":"{tag}"}}]}}}},
          "Igw":{{"Type":"AWS::EC2::InternetGateway","Properties":{{
            "Tags":[{{"Key":"estate","Value":"{tag}"}}]}}}},
          "IgwTwo":{{"Type":"AWS::EC2::InternetGateway","Properties":{{
            "Tags":[{{"Key":"estate","Value":"{tag}"}}]}}}},
          "Vgw":{{"Type":"AWS::EC2::VPNGateway","Properties":{{"Type":"ipsec.1",
            "Tags":[{{"Key":"estate","Value":"{tag}"}}]}}}},
          "Attach":{{"Type":"AWS::EC2::VPCGatewayAttachment","Properties":{{
            "VpcId":{{"Ref":"Vpc"}},{attached}}}}},
          "Eni":{{"Type":"AWS::EC2::NetworkInterface","Properties":{{
            "SubnetId":{{"Ref":"Subnet"}},"Description":"the estate interface"}}}},
          "RouteTable":{{"Type":"AWS::EC2::RouteTable","Properties":{{"VpcId":{{"Ref":"Vpc"}},
            "Tags":[{{"Key":"estate","Value":"{tag}"}}]}}}},
          "Route":{{"Type":"AWS::EC2::Route","Properties":{{
            "RouteTableId":{{"Ref":"RouteTable"}},"DestinationCidrBlock":"0.0.0.0/0",
            {target}}},"DependsOn":"Attach"}}
        }},
        "Outputs":{{
          "RouteRef":{{"Value":{{"Ref":"Route"}}}},
          "RouteCidr":{{"Value":{{"Fn::GetAtt":["Route","CidrBlock"]}}}},
          "AttachRef":{{"Value":{{"Ref":"Attach"}}}},
          "TableRef":{{"Value":{{"Ref":"RouteTable"}}}},
          "VpcRef":{{"Value":{{"Ref":"Vpc"}}}}
        }}}}"#
    )
}

/// Every logical id the estate declares, which is the set whose physical ids an in-place
/// update must leave exactly where they are.
const ESTATE_RESOURCES: &[&str] = &[
    "Vpc",
    "Subnet",
    "Igw",
    "IgwTwo",
    "Vgw",
    "Attach",
    "Eni",
    "RouteTable",
    "Route",
];

/// The `PhysicalResourceId` the stack reports for one of its resources.
async fn estate_physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackResources({logical_id}) to answer, got {code} \
         {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// Read every resource of the estate back out of Amazon EC2 and hold the deployment to what
/// `estate` declares: the value of each switch, the tag set, the gateway on the VPC and the
/// route's target.
async fn assert_estate_in_amazon_ec2(
    router: &MultiServiceRouter,
    case: &str,
    ids: &[(String, String)],
    estate: &Estate,
) {
    let identifier = |logical_id: &str| -> String {
        ids.iter()
            .find(|(held, _)| held == logical_id)
            .map(|(_, id)| id.clone())
            .unwrap_or_default()
    };

    // The VPC's DNS-hostnames attribute, which `DescribeVpcAttribute` reports as that
    // attribute's own value.
    let (code, body) = ec2(
        router,
        "DescribeVpcAttribute",
        &[
            ("VpcId", &identifier("Vpc")),
            ("Attribute", "enableDnsHostnames"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeVpcAttribute to answer, got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "value").as_deref(),
        Some(estate.dns_hostnames.to_string().as_str()),
        "case {case:?}: expected the VPC to hold EnableDnsHostnames={}, got {body}",
        estate.dns_hostnames
    );

    // The subnet's launch switch and its tag, both reported on the described subnet.
    let (code, body) = ec2(
        router,
        "DescribeSubnets",
        &[("SubnetId.1", &identifier("Subnet"))],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeSubnets to answer, got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "mapPublicIpOnLaunch").as_deref(),
        Some(estate.map_public_ip.to_string().as_str()),
        "case {case:?}: expected the subnet to hold MapPublicIpOnLaunch={}, got {body}",
        estate.map_public_ip
    );
    assert_eq!(
        crate::harness::ec2_tag_value(&body, "estate").as_deref(),
        Some(estate.tag),
        "case {case:?}: expected the subnet to carry the declared estate tag, got {body}"
    );

    // The tag every other taggable resource of the estate carries, each read through the
    // describe of its own type.
    for (logical_id, action, member_name) in [
        ("Igw", "DescribeInternetGateways", "InternetGatewayId.1"),
        ("IgwTwo", "DescribeInternetGateways", "InternetGatewayId.1"),
        ("Vgw", "DescribeVpnGateways", "VpnGatewayId.1"),
        ("RouteTable", "DescribeRouteTables", "RouteTableId.1"),
    ] {
        let (code, body) = ec2(router, action, &[(member_name, &identifier(logical_id))]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected {action} to answer, got {code} {body}"
        );
        assert_eq!(
            crate::harness::ec2_tag_value(&body, "estate").as_deref(),
            Some(estate.tag),
            "case {case:?}: expected {logical_id} to carry the declared estate tag, got {body}"
        );
    }

    // The attachment puts exactly the declared gateway on the VPC, and the other gateway is
    // attached to nothing: a swap is a detach of the one in force and an attach of the one
    // declared, never two gateways on one VPC.
    for (logical_id, action, member_name) in [
        ("Igw", "DescribeInternetGateways", "InternetGatewayId.1"),
        ("IgwTwo", "DescribeInternetGateways", "InternetGatewayId.1"),
    ] {
        let (code, body) = ec2(router, action, &[(member_name, &identifier(logical_id))]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected {action} to answer, got {code} {body}"
        );
        let attached = xml_texts(&body, "vpcId").contains(&identifier("Vpc"));
        assert_eq!(
            attached,
            logical_id == estate.gateway,
            "case {case:?}: expected {logical_id} to be attached to the VPC only when the \
             declaration names it ({}), got {body}",
            estate.gateway
        );
    }

    // The route points at exactly the declared target, and the table still holds ONE route for
    // that destination.
    let (code, body) = ec2(
        router,
        "DescribeRouteTables",
        &[("RouteTableId.1", &identifier("RouteTable"))],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeRouteTables to answer, got {code} {body}"
    );
    let routes: Vec<String> = xml_blocks(&body, "item")
        .into_iter()
        .filter(|item| xml_text(item, "destinationCidrBlock").as_deref() == Some("0.0.0.0/0"))
        .collect();
    assert_eq!(
        routes.len(),
        1,
        "case {case:?}: expected the table to hold one default route, got {body}"
    );
    let target = identifier(estate.route_target);
    assert!(
        routes[0].contains(&target),
        "case {case:?}: expected the default route to point at {}, got {}",
        target,
        routes[0]
    );
    // ...and the route's STATE is what its target's availability makes it: "the blackhole
    // state indicates that the route's target isn't available" (`Route$State`), which an
    // elastic network interface attached to nothing is.
    let expected_state = if estate.route_target == "Eni" {
        "blackhole"
    } else {
        "active"
    };
    assert_eq!(
        xml_text(&routes[0], "state").as_deref(),
        Some(expected_state),
        "case {case:?}: expected the default route to be reported {expected_state}, got {}",
        routes[0]
    );
}

/// Deploy one revision of the estate and answer the physical id the stack reports for each of
/// its resources.
async fn deploy_estate(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    estate: &Estate,
    first: bool,
) -> Vec<(String, String)> {
    let template = estate_template(estate);
    let (code, body) = if first {
        create_stack(router, stack, &template, &[]).await
    } else {
        call(
            router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &template),
            ],
        )
        .await
    };
    assert_eq!(
        code, 200,
        "case {case:?}: expected the deployment to be accepted, got {code} {body}"
    );
    let settled = crate::stack_lifecycle::settled_stack(router, case, stack).await;
    let reported = xml_text(&settled, "StackStatus").unwrap_or_default();
    let expected = if first {
        status::CREATE_COMPLETE
    } else {
        status::UPDATE_COMPLETE
    };
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the deployment to reach {expected}, got {reported:?} (reason \
         {:?})",
        reason_of(router, stack).await
    );
    let mut ids = Vec::new();
    for logical_id in ESTATE_RESOURCES {
        ids.push((
            (*logical_id).to_string(),
            estate_physical_id(router, case, stack, logical_id).await,
        ));
    }
    ids
}

#[tokio::test]
async fn an_in_place_update_applies_each_ec2_change_and_keeps_every_identity() {
    // The rows differ along what the rule generalises over: which direction each switch is
    // flipped in, which gateway the attachment carries, which KIND of target the route points
    // at, and the tag value every taggable resource ends up with. The recorded case the issue
    // was filed about is the first row's VPC and internet gateway.
    let rows = [
        (
            "an estate whose switches, tags, gateway and route target all move forward",
            "ec2-inplace-forward",
            Estate {
                dns_hostnames: false,
                map_public_ip: false,
                tag: "one",
                gateway: "Igw",
                route_target: "Igw",
            },
            Estate {
                dns_hostnames: true,
                map_public_ip: true,
                tag: "two",
                gateway: "IgwTwo",
                route_target: "Eni",
            },
        ),
        (
            "an estate whose switches, tags, gateway and route target all move back",
            "ec2-inplace-back",
            Estate {
                dns_hostnames: true,
                map_public_ip: true,
                tag: "two",
                gateway: "IgwTwo",
                route_target: "Eni",
            },
            Estate {
                dns_hostnames: false,
                map_public_ip: false,
                tag: "one",
                gateway: "Igw",
                route_target: "Igw",
            },
        ),
    ];
    for (case, stack, created, revised) in rows {
        let router = router();
        let before = deploy_estate(&router, case, stack, &created, true).await;
        assert_estate_in_amazon_ec2(&router, case, &before, &created).await;

        let after = deploy_estate(&router, case, stack, &revised, false).await;
        // Every resource kept the identity its owning service assigned: an in-place update
        // changes the resource that exists rather than creating another one, so nothing that
        // references one of them is left pointing at a resource nothing holds.
        assert_eq!(
            after, before,
            "case {case:?}: expected every resource to keep its physical id across the \
             in-place update"
        );
        assert_estate_in_amazon_ec2(&router, case, &after, &revised).await;

        // ...and what the stack PUBLISHES for the two types Amazon EC2 mints no identifier
        // for is the pair that identifies each of them: a route is its table and the
        // destination it matches on, an attachment the kind of gateway it carries and the VPC
        // it is on, both joined the way CloudFormation joins the parts of a composite
        // identifier — and the route's one published attribute is that destination.
        let published = outputs_of(&router, stack).await;
        let table = output_of(&published, "TableRef");
        let vpc = output_of(&published, "VpcRef");
        assert_eq!(
            output_of(&published, "RouteRef"),
            format!("{table}|0.0.0.0/0"),
            "case {case:?}: expected the route's Ref to be its table and its destination, got \
             {published:?}"
        );
        assert_eq!(
            output_of(&published, "RouteCidr"),
            "0.0.0.0/0",
            "case {case:?}: expected the route to publish the destination it matches on, got \
             {published:?}"
        );
        assert_eq!(
            output_of(&published, "AttachRef"),
            format!("IGW|{vpc}"),
            "case {case:?}: expected the attachment's Ref to be its gateway kind and its VPC, \
             got {published:?}"
        );
    }
}

/// A one-group template whose security group declares `ingress` inbound, `egress` outbound and
/// the `posture` tag `tag`, under the description `description` — which is create-only, so a
/// revision that states another one REPLACES the group.
fn described_group_template(
    description: &str,
    ingress: &str,
    egress: &str,
    tag: &str,
) -> String {
    format!(
        r#"{{"Resources":{{
          "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.30.0.0/16"}}}},
          "Group":{{"Type":"AWS::EC2::SecurityGroup","UpdateReplacePolicy":"Retain",
            "Properties":{{
            "GroupDescription":"{description}","VpcId":{{"Ref":"Vpc"}},
            "SecurityGroupIngress":[{ingress}],"SecurityGroupEgress":[{egress}],
            "Tags":[{{"Key":"posture","Value":"{tag}"}}]}}}}
        }}}}"#
    )
}

/// A one-group template whose security group declares `ingress` inbound, `egress` outbound and
/// the `posture` tag `tag`.
fn group_template(ingress: &str, egress: &str, tag: &str) -> String {
    described_group_template("the posture group", ingress, egress, tag)
}

#[tokio::test]
async fn an_in_place_update_leaves_a_security_group_with_exactly_the_declared_rules() {
    // A security group's inbound and outbound rules are each one permission of the authorize
    // call for their direction, and the revoke for that direction takes the same permission —
    // so a revision that re-states them leaves the group carrying exactly the declared set,
    // under the `sg-` id Amazon EC2 assigned it. The rows differ along what changed: a rule's
    // description, its port range, a rule added beside the one that holds, and the tag.
    let first_ingress = r#"{"IpProtocol":"tcp","FromPort":8443,"ToPort":8443,
        "CidrIp":"192.0.2.0/24","Description":"tcp from the documentation range"}"#;
    let revised_ingress = r#"{"IpProtocol":"tcp","FromPort":8443,"ToPort":8443,
        "CidrIp":"192.0.2.0/24","Description":"tcp from the documentation range, revised"},
        {"IpProtocol":"tcp","FromPort":9443,"ToPort":9443,"CidrIp":"198.51.100.0/24"}"#;
    let first_egress = r#"{"IpProtocol":"tcp","FromPort":443,"ToPort":443,
        "CidrIp":"0.0.0.0/0"}"#;
    let revised_egress = r#"{"IpProtocol":"tcp","FromPort":8080,"ToPort":8080,
        "CidrIp":"0.0.0.0/0"}"#;

    let router = router();
    let created = group_template(first_ingress, first_egress, "one");
    let (code, body) = create_stack(&router, "ec2-inplace-group", &created, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    let settled = crate::stack_lifecycle::settled_stack(&router, "group", "ec2-inplace-group").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the create to complete, got {settled}"
    );
    let group_id = estate_physical_id(&router, "group", "ec2-inplace-group", "Group").await;

    // What the CREATE left in Amazon EC2: exactly the declared posture in both directions —
    // the all-traffic outbound rule a new group comes with is replaced by the one the
    // declaration states — and the tag set CloudFormation gives every resource it creates,
    // which is the declaration's own tag beside the three that name the stack and this
    // resource's place in it.
    let (code, body) = ec2(
        &router,
        "DescribeSecurityGroups",
        &[("GroupId.1", &group_id)],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected DescribeSecurityGroups to answer, got {code} {body}"
    );
    let outbound = xml_blocks(&body, "ipPermissionsEgress");
    assert_eq!(
        xml_texts(outbound.first().map_or("", String::as_str), "ipProtocol"),
        vec!["tcp".to_string()],
        "expected the created group to carry exactly the declared outbound rule, got {body}"
    );
    for key in [
        "posture",
        "aws:cloudformation:stack-name",
        "aws:cloudformation:stack-id",
        "aws:cloudformation:logical-id",
    ] {
        assert!(
            crate::harness::ec2_tag_value(&body, key).is_some(),
            "expected the created group to carry the {key} tag, got {body}"
        );
    }

    let revised = group_template(revised_ingress, revised_egress, "two");
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "ec2-inplace-group"),
            member("TemplateBody", &revised),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    let settled = crate::stack_lifecycle::settled_stack(&router, "group", "ec2-inplace-group").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "expected the in-place update to complete, got {settled} (reason {:?})",
        reason_of(&router, "ec2-inplace-group").await
    );
    let after = estate_physical_id(&router, "group", "ec2-inplace-group", "Group").await;
    assert_eq!(
        after, group_id,
        "expected the group to keep the id Amazon EC2 assigned it"
    );

    // Amazon EC2 holds exactly the declared posture: the two inbound rules the revision
    // states, the one outbound rule it states — and no trace of the rules it no longer does.
    let (code, body) = ec2(
        &router,
        "DescribeSecurityGroups",
        &[("GroupId.1", &group_id)],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected DescribeSecurityGroups to answer, got {code} {body}"
    );
    let inbound = xml_blocks(&body, "ipPermissions");
    let outbound = xml_blocks(&body, "ipPermissionsEgress");
    let inbound_ports = xml_texts(inbound.first().map_or("", String::as_str), "fromPort");
    assert_eq!(
        inbound_ports,
        vec!["8443".to_string(), "9443".to_string()],
        "expected the group to hold exactly the declared inbound rules, got {body}"
    );
    let outbound_ports = xml_texts(outbound.first().map_or("", String::as_str), "fromPort");
    assert_eq!(
        outbound_ports,
        vec!["8080".to_string()],
        "expected the group to hold exactly the declared outbound rule, got {body}"
    );
    assert!(
        body.contains("tcp from the documentation range, revised"),
        "expected the group to carry the revised rule description, got {body}"
    );
    assert_eq!(
        crate::harness::ec2_tag_value(&body, "posture").as_deref(),
        Some("two"),
        "expected the group to carry the tag the revision declares, got {body}"
    );
}

#[tokio::test]
async fn a_replaced_security_group_is_created_beside_the_one_it_replaces() {
    // A group's DESCRIPTION is create-only — Amazon EC2 publishes no call that changes it — so
    // a revision stating another one REPLACES the group. The group CloudFormation created
    // carries a name CloudFormation generated, and the group being replaced still holds that
    // name while the replacement is created (the more so under `Retain`, which keeps it for
    // good), so the replacement is created under a name of its OWN and the stack completes with
    // the new group holding the declared posture.
    let ingress = r#"{"IpProtocol":"tcp","FromPort":8443,"ToPort":8443,
        "CidrIp":"192.0.2.0/24","Description":"tcp from the documentation range"}"#;
    let egress = r#"{"IpProtocol":"tcp","FromPort":443,"ToPort":443,"CidrIp":"0.0.0.0/0"}"#;

    let router = router();
    let created = described_group_template("the first posture", ingress, egress, "one");
    let (code, body) = create_stack(&router, "ec2-group-replace", &created, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    let settled = crate::stack_lifecycle::settled_stack(&router, "replace", "ec2-group-replace").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the create to complete, got {settled}"
    );
    let first = estate_physical_id(&router, "replace", "ec2-group-replace", "Group").await;

    let revised = described_group_template("the second posture", ingress, egress, "two");
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "ec2-group-replace"),
            member("TemplateBody", &revised),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    let settled = crate::stack_lifecycle::settled_stack(&router, "replace", "ec2-group-replace").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "expected the replacement to complete, got {settled} (reason {:?})",
        reason_of(&router, "ec2-group-replace").await
    );
    let replacement = estate_physical_id(&router, "replace", "ec2-group-replace", "Group").await;
    assert_ne!(
        replacement, first,
        "expected the replacement to be a group of its own"
    );

    // The group the stack now publishes holds exactly what the revision declared...
    let (code, body) = ec2(
        &router,
        "DescribeSecurityGroups",
        &[("GroupId.1", &replacement)],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected DescribeSecurityGroups to answer for the replacement, got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "groupDescription").as_deref(),
        Some("the second posture"),
        "expected the replacement to carry the declared description, got {body}"
    );
    assert_eq!(
        crate::harness::ec2_tag_value(&body, "posture").as_deref(),
        Some("two"),
        "expected the replacement to carry the declared tag, got {body}"
    );
    assert!(
        xml_texts(&body, "fromPort").contains(&"8443".to_string()),
        "expected the replacement to carry the declared inbound rule, got {body}"
    );

    // ...and the group it replaced is still there, which is what `Retain` asks for.
    let (code, body) = ec2(&router, "DescribeSecurityGroups", &[("GroupId.1", &first)]).await;
    assert_eq!(
        code, 200,
        "expected the retained group to still be held by Amazon EC2, got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "groupDescription").as_deref(),
        Some("the first posture"),
        "expected the retained group to be left exactly as it was, got {body}"
    );
}

/// A one-association template whose `AWS::EC2::SubnetRouteTableAssociation` puts the subnet on
/// the route table `table` names.
fn association_template(table: &str) -> String {
    format!(
        r#"{{"Resources":{{
          "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.40.0.0/16"}}}},
          "Subnet":{{"Type":"AWS::EC2::Subnet","Properties":{{"VpcId":{{"Ref":"Vpc"}},
            "CidrBlock":"10.40.1.0/24"}}}},
          "First":{{"Type":"AWS::EC2::RouteTable","Properties":{{"VpcId":{{"Ref":"Vpc"}}}}}},
          "Second":{{"Type":"AWS::EC2::RouteTable","Properties":{{"VpcId":{{"Ref":"Vpc"}}}}}},
          "Assoc":{{"Type":"AWS::EC2::SubnetRouteTableAssociation",
            "UpdateReplacePolicy":"Retain","Properties":{{
            "SubnetId":{{"Ref":"Subnet"}},"RouteTableId":{{"Ref":"{table}"}}}}}}
        }}}}"#
    )
}

#[tokio::test]
async fn replacing_a_subnet_route_table_association_deletes_the_old_one_first() {
    // A subnet holds exactly ONE route-table association, so the owning service refuses a
    // second one for it: a replacement of this type is DELETE-BEFORE-CREATE, and the old
    // association is taken down whatever its `UpdateReplacePolicy` says — `Retain` included,
    // since there is no window in which both exist.
    let router = router();
    let (code, body) = create_stack(
        &router,
        "ec2-assoc-replace",
        &association_template("First"),
        &[],
    )
    .await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    let settled = crate::stack_lifecycle::settled_stack(&router, "assoc", "ec2-assoc-replace").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the create to complete, got {settled}"
    );
    let created = estate_physical_id(&router, "assoc", "ec2-assoc-replace", "Assoc").await;

    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "ec2-assoc-replace"),
            member("TemplateBody", &association_template("Second")),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    let settled = crate::stack_lifecycle::settled_stack(&router, "assoc", "ec2-assoc-replace").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "expected the replacement to complete, got {settled} (reason {:?})",
        reason_of(&router, "ec2-assoc-replace").await
    );
    let replacement = estate_physical_id(&router, "assoc", "ec2-assoc-replace", "Assoc").await;
    assert_ne!(
        replacement, created,
        "expected the replacement to be an association of its own"
    );

    // The association's own events, oldest first: the old association is DELETED before the
    // replacement's creation opens.
    let (code, body) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", "ec2-assoc-replace")],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected DescribeStackEvents to answer, got {code} {body}"
    );
    let mut statuses: Vec<String> = xml_blocks(&body, "member")
        .into_iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some("Assoc"))
        .filter_map(|event| xml_text(&event, "ResourceStatus"))
        .collect();
    statuses.reverse();
    let deleted = statuses
        .iter()
        .position(|status| status == "DELETE_COMPLETE")
        .unwrap_or_else(|| panic!("expected the old association to be deleted, got {statuses:?}"));
    let created_again = statuses
        .iter()
        .rposition(|status| status.ends_with("_IN_PROGRESS"))
        .unwrap_or_else(|| panic!("expected the replacement to be created, got {statuses:?}"));
    assert!(
        deleted < created_again,
        "expected the old association to be deleted before its replacement is created, got \
         {statuses:?}"
    );

    // ...and Amazon EC2 holds exactly one association for the subnet, on the table the
    // revision names.
    let (code, body) = ec2(
        &router,
        "DescribeRouteTables",
        &[(
            "RouteTableId.1",
            &estate_physical_id(&router, "assoc", "ec2-assoc-replace", "Second").await,
        )],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected DescribeRouteTables to answer, got {code} {body}"
    );
    assert!(
        xml_texts(&body, "routeTableAssociationId").contains(&replacement),
        "expected the second table to hold the replacement association, got {body}"
    );
}

/// A one-instance template whose `AWS::EC2::Instance` puts itself on `subnet_id` through a
/// NETWORK INTERFACE of its own, in the security group `group_id`, with a public IPv4 address
/// — the standard way a VPC-scoped instance template states where an instance goes.
fn interface_instance_template(image_id: &str, subnet_id: &str, group_id: &str) -> String {
    format!(
        r#"{{"Resources":{{"Instance":{{"Type":"AWS::EC2::Instance","Properties":{{
            "ImageId":"{image_id}","InstanceType":"t3.micro",
            "NetworkInterfaces":[{{"DeviceIndex":"0","SubnetId":"{subnet_id}",
              "GroupSet":["{group_id}"],"AssociatePublicIpAddress":true,
              "DeleteOnTermination":true,"Description":"the declared interface"}}],
            "Tags":[{{"Key":"estate","Value":"one"}}]}}}}}},
          "Outputs":{{
            "InstanceId":{{"Value":{{"Ref":"Instance"}}}},
            "VpcId":{{"Value":{{"Fn::GetAtt":["Instance","VpcId"]}}}},
            "PrivateIp":{{"Value":{{"Fn::GetAtt":["Instance","PrivateIp"]}}}}}}}}"#
    )
}

#[tokio::test]
async fn an_instance_launches_with_the_network_interfaces_its_declaration_states() {
    // `NetworkInterfaces` is `RunInstances`'s own list, so a declaration naming one is sent as
    // that member rather than refused: the instance comes up on the subnet the entry names, in
    // the groups it names, and the attributes the launch assigns — the VPC it runs in among
    // them — are the ones the stack publishes.
    let router = router();
    let (code, body) = ec2(&router, "CreateVpc", &[("CidrBlock", "10.50.0.0/16")]).await;
    assert_eq!(code, 200, "expected CreateVpc to answer, got {code} {body}");
    let vpc_id = xml_text(&body, "vpcId").unwrap_or_default();
    let (code, body) = ec2(
        &router,
        "CreateSubnet",
        &[("VpcId", &vpc_id), ("CidrBlock", "10.50.1.0/24")],
    )
    .await;
    assert_eq!(code, 200, "expected CreateSubnet to answer, got {code} {body}");
    let subnet_id = xml_text(&body, "subnetId").unwrap_or_default();
    let (code, body) = ec2(
        &router,
        "CreateSecurityGroup",
        &[
            ("GroupName", "the-interface-group"),
            ("GroupDescription", "the group the interface is in"),
            ("VpcId", &vpc_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected CreateSecurityGroup to answer, got {code} {body}"
    );
    let group_id = xml_text(&body, "groupId").unwrap_or_default();
    // The AMI is read out of Amazon EC2 itself: a template cannot invent an image id.
    let (code, body) = ec2(&router, "DescribeImages", &[]).await;
    assert_eq!(code, 200, "expected DescribeImages to answer, got {code} {body}");
    let image_id = xml_texts(&body, "imageId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a fresh account must hold an image: {body}"));

    let template = interface_instance_template(&image_id, &subnet_id, &group_id);
    let (code, body) = create_stack(&router, "ec2-instance-interfaces", &template, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    let settled =
        crate::stack_lifecycle::settled_stack(&router, "interfaces", "ec2-instance-interfaces")
            .await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the create to complete, got {settled} (reason {:?})",
        reason_of(&router, "ec2-instance-interfaces").await
    );

    // The outputs the stack publishes are the values Amazon EC2 assigned, not stand-ins: the
    // instance's own id, the VPC its interface put it in, and the private address of that
    // interface.
    let instance_id =
        estate_physical_id(&router, "interfaces", "ec2-instance-interfaces", "Instance").await;
    let published = outputs_of(&router, "ec2-instance-interfaces").await;
    assert_eq!(
        output_of(&published, "InstanceId"),
        instance_id,
        "expected the stack to publish the instance Amazon EC2 launched, got {published:?}"
    );
    assert_eq!(
        output_of(&published, "VpcId"),
        vpc_id,
        "expected the stack to publish the VPC the instance runs in, got {published:?}"
    );

    // ...and Amazon EC2 holds the instance on the subnet and in the group the entry named,
    // with the public address it asked for.
    let (code, body) = ec2(&router, "DescribeInstances", &[("InstanceId.1", &instance_id)]).await;
    assert_eq!(
        code, 200,
        "expected DescribeInstances to answer, got {code} {body}"
    );
    assert_eq!(
        xml_text(&body, "subnetId").as_deref(),
        Some(subnet_id.as_str()),
        "expected the instance to be on the subnet its interface named, got {body}"
    );
    assert!(
        xml_texts(&body, "groupId").contains(&group_id),
        "expected the instance to be in the group its interface named, got {body}"
    );
    assert_eq!(
        xml_text(&body, "privateIpAddress").as_deref(),
        Some(output_of(&published, "PrivateIp").as_str()),
        "expected the published private address to be the one Amazon EC2 assigned, got {body}"
    );
    assert!(
        !xml_text(&body, "ipAddress").unwrap_or_default().is_empty(),
        "expected the instance to carry the public address its interface asked for, got {body}"
    );
}

/// A one-interface template whose `AWS::EC2::NetworkInterface` is in the security groups
/// `groups` names (a JSON list body).
fn interface_template(groups: &str) -> String {
    format!(
        r#"{{"Resources":{{
          "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.60.0.0/16"}}}},
          "Subnet":{{"Type":"AWS::EC2::Subnet","Properties":{{"VpcId":{{"Ref":"Vpc"}},
            "CidrBlock":"10.60.1.0/24"}}}},
          "First":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
            "GroupDescription":"the first group","VpcId":{{"Ref":"Vpc"}}}}}},
          "Second":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
            "GroupDescription":"the second group","VpcId":{{"Ref":"Vpc"}}}}}},
          "Eni":{{"Type":"AWS::EC2::NetworkInterface","Properties":{{
            "SubnetId":{{"Ref":"Subnet"}},"GroupSet":{groups}}}}}
        }}}}"#
    )
}

#[tokio::test]
async fn an_interface_takes_a_new_group_set_in_place_and_refuses_an_empty_one() {
    // "Changes the security groups for the network interface" is the one call that re-points
    // them, so a revision naming other groups is applied to the interface that exists. A
    // revision naming NONE states a set that call cannot carry — an interface is always in at
    // least one group — so the deployment reports the change as not applied and fails the
    // resource rather than completing while the caller's change went nowhere.
    let router = router();
    let created = interface_template(r#"[{"Ref":"First"}]"#);
    let (code, body) = create_stack(&router, "ec2-interface-groups", &created, &[]).await;
    assert_eq!(code, 200, "expected CreateStack to succeed, got {code} {body}");
    let settled =
        crate::stack_lifecycle::settled_stack(&router, "groups", "ec2-interface-groups").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the create to complete, got {settled}"
    );
    let interface = estate_physical_id(&router, "groups", "ec2-interface-groups", "Eni").await;
    let second = estate_physical_id(&router, "groups", "ec2-interface-groups", "Second").await;

    // The group set the revision names reaches the interface, which keeps its `eni-` id.
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "ec2-interface-groups"),
            member("TemplateBody", &interface_template(r#"[{"Ref":"Second"}]"#)),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    let settled =
        crate::stack_lifecycle::settled_stack(&router, "groups", "ec2-interface-groups").await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "expected the in-place update to complete, got {settled} (reason {:?})",
        reason_of(&router, "ec2-interface-groups").await
    );
    assert_eq!(
        estate_physical_id(&router, "groups", "ec2-interface-groups", "Eni").await,
        interface,
        "expected the interface to keep the id Amazon EC2 assigned it"
    );
    let (code, body) = ec2(
        &router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", &interface)],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected DescribeNetworkInterfaces to answer, got {code} {body}"
    );
    assert!(
        xml_texts(&body, "groupId").contains(&second),
        "expected the interface to be in the group the revision names, got {body}"
    );

    // ...and the revision that names no group at all fails the resource.
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", "ec2-interface-groups"),
            member("TemplateBody", &interface_template("[]")),
        ],
    )
    .await;
    assert_eq!(code, 200, "expected UpdateStack to succeed, got {code} {body}");
    let settled =
        crate::stack_lifecycle::settled_stack(&router, "groups", "ec2-interface-groups").await;
    let reported = xml_text(&settled, "StackStatus").unwrap_or_default();
    assert!(
        reported.starts_with("UPDATE_ROLLBACK"),
        "expected the change the owning service cannot carry to fail the resource, got \
         {reported:?} (reason {:?})",
        reason_of(&router, "ec2-interface-groups").await
    );
    // The interface is still in the group the deployment in force put it in: nothing was
    // dropped on the way.
    let (code, body) = ec2(
        &router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", &interface)],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected DescribeNetworkInterfaces to answer, got {code} {body}"
    );
    assert!(
        xml_texts(&body, "groupId").contains(&second),
        "expected the interface to keep the groups it is in, got {body}"
    );
}

/// The stack's `Outputs` as the key-to-value pairs a client reads them as.
async fn outputs_of(router: &MultiServiceRouter, stack: &str) -> Vec<(String, String)> {
    let (code, body) = describe_stack(router, stack).await;
    assert_eq!(code, 200, "expected DescribeStacks({stack}) to answer, got {code} {body}");
    xml_blocks(&body, "member")
        .iter()
        .filter_map(|output| {
            Some((
                xml_text(output, "OutputKey")?,
                xml_text(output, "OutputValue").unwrap_or_default(),
            ))
        })
        .collect()
}

/// One output's value, or the empty string where the stack published no such output.
fn output_of(outputs: &[(String, String)], key: &str) -> String {
    outputs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The attributes a revision REPUBLISHES
// ---------------------------------------------------------------------------

/// Which call deploys a revision: both are deployments of the same stack, and an attribute
/// follows the declaration either way.
#[derive(Clone, Copy, Debug)]
enum Deployed {
    UpdateStack,
    ExecuteChangeSet,
}

/// One row of the republished-attribute table: the stack, the template it deploys (with
/// `STACK` standing for the stack's name and `VALUE` for the row's declared value), how the
/// row's value reaches the template, and what the subject must then publish.
///
/// The attribute under test is `AWS::SSM::Parameter`'s `Value` — "Fn::GetAtt … Value: the
/// parameter value", one of the two return values that type publishes
/// (`smithy_query(query="doc", shape="PutParameter", member="Value")` for the member the
/// declaration states, and the type's own Return values section for the attribute).
struct Republished {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    /// The template `Parameter` the row's value is supplied through, where it reaches the
    /// template that way rather than being written into the body.
    supplied_through: Option<&'static str>,
    /// `(the value the revision states, the value the attribute must then publish)`.
    first: (&'static str, &'static str),
    revised: (&'static str, &'static str),
    deployed_by: Deployed,
}

/// A template whose subject publishes the row's value as an attribute, read on two surfaces:
/// an OUTPUT of the stack, and the PROPERTY of another resource the stack really creates in
/// the service that owns it.
const LITERAL_VALUE: &str = r#"{"Resources":{
    "Subject":{"Type":"AWS::SSM::Parameter","Properties":{"Type":"String","Value":"VALUE"}},
    "Reader":{"Type":"AWS::SNS::Topic","Properties":{"TopicName":"STACK-reader",
      "DisplayName":{"Fn::GetAtt":["Subject","Value"]}}}},
    "Outputs":{"Published":{"Value":{"Fn::GetAtt":["Subject","Value"]}},
      "Identity":{"Value":{"Ref":"Subject"}},
      "ReaderArn":{"Value":{"Ref":"Reader"}}}}"#;

/// The same, with the subject's value COMPOSED by an intrinsic over a template parameter the
/// revision re-values: the template text is identical across the two deployments, and only
/// the supplied value differs.
const COMPOSED_VALUE: &str = r#"{"Parameters":{"Supplied":{"Type":"String"}},
    "Resources":{
    "Subject":{"Type":"AWS::SSM::Parameter","Properties":{"Type":"String",
      "Value":{"Fn::Join":["-",["head",{"Ref":"Supplied"}]]}}},
    "Reader":{"Type":"AWS::SNS::Topic","Properties":{"TopicName":"STACK-reader",
      "DisplayName":{"Fn::GetAtt":["Subject","Value"]}}}},
    "Outputs":{"Published":{"Value":{"Fn::GetAtt":["Subject","Value"]}},
      "Identity":{"Value":{"Ref":"Subject"}},
      "ReaderArn":{"Value":{"Ref":"Reader"}}}}"#;

/// The class the rule covers: the rows differ along HOW the declared value is stated (a
/// literal in the body, an intrinsic over a supplied parameter), the number of characters and
/// the characters it carries, and WHICH call deploys the revision.
fn republished_rows() -> Vec<Republished> {
    vec![
        Republished {
            case: "a literal value re-stated by UpdateStack",
            stack: "republished-literal",
            template: LITERAL_VALUE,
            supplied_through: None,
            first: ("first-value", "first-value"),
            revised: ("second-value", "second-value"),
            deployed_by: Deployed::UpdateStack,
        },
        Republished {
            case: "a literal value re-stated by executing a change set",
            stack: "republished-change-set",
            template: LITERAL_VALUE,
            supplied_through: None,
            first: ("1", "1"),
            revised: ("a much longer value 2", "a much longer value 2"),
            deployed_by: Deployed::ExecuteChangeSet,
        },
        Republished {
            case: "a value composed by an intrinsic over a parameter the revision re-values",
            stack: "republished-composed",
            template: COMPOSED_VALUE,
            supplied_through: Some("Supplied"),
            first: ("one", "head-one"),
            revised: ("two", "head-two"),
            deployed_by: Deployed::UpdateStack,
        },
    ]
}

/// Deploy one revision of a row's stack and wait past the instant its operation settles.
async fn deploy_republished(
    router: &MultiServiceRouter,
    row: &Republished,
    value: &str,
    creating: bool,
) {
    let template = row
        .template
        .replace("STACK", row.stack)
        .replace("VALUE", value);
    let mut extra = Vec::new();
    if let Some(key) = row.supplied_through {
        extra.push(member("Parameters.member.1.ParameterKey", key));
        extra.push(member("Parameters.member.1.ParameterValue", value));
    }
    let case = row.case;
    if creating {
        let (code, body) = create_stack(router, row.stack, &template, &extra).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({}) must be accepted, got {code} {body}",
            row.stack
        );
        wait_for_the_transition().await;
        return;
    }
    match row.deployed_by {
        Deployed::UpdateStack => {
            let mut members = vec![
                member("StackName", row.stack),
                member("TemplateBody", &template),
            ];
            members.extend(extra);
            let (code, body) = call(router, "UpdateStack", &members).await;
            assert_eq!(
                code, 200,
                "case {case:?}: UpdateStack({}) must be accepted, got {code} {body}",
                row.stack
            );
        }
        Deployed::ExecuteChangeSet => {
            let change_set = format!("{}-revision", row.stack);
            let mut members = vec![
                member("StackName", row.stack),
                member("ChangeSetName", &change_set),
                member("TemplateBody", &template),
            ];
            members.extend(extra);
            let (code, body) = call(router, "CreateChangeSet", &members).await;
            assert_eq!(
                code, 200,
                "case {case:?}: CreateChangeSet({}) must be accepted, got {code} {body}",
                row.stack
            );
            wait_for_the_transition().await;
            let (code, body) = call(
                router,
                "ExecuteChangeSet",
                &[
                    member("StackName", row.stack),
                    member("ChangeSetName", &change_set),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: ExecuteChangeSet({}) must be accepted, got {code} {body}",
                row.stack
            );
        }
    }
    wait_for_the_transition().await;
}

/// What the subject publishes right now, on both surfaces: the stack's own output, and the
/// property the reader was really created with in the service that owns it.
async fn published_on_both_surfaces(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
) -> (String, String, String) {
    let outputs = outputs_of(router, stack).await;
    let reader = output_of(&outputs, "ReaderArn");
    let display_name = topic_attributes(case, router, &reader)
        .await
        .into_iter()
        .find(|(key, _)| key == "DisplayName")
        .map(|(_, value)| value)
        .unwrap_or_default();
    (
        output_of(&outputs, "Published"),
        display_name,
        output_of(&outputs, "Identity"),
    )
}

/// RULE: an attribute a resource publishes STRAIGHT FROM one of its own declared properties
/// is a function of the template being deployed, so a revision that states that property
/// differently republishes it — every `Fn::GetAtt` reading it, in an output of the stack or in
/// another resource's property, carries the value the revision stated, while the resource
/// keeps the identity it already had.
#[tokio::test]
async fn a_revision_republishes_the_attributes_its_declaration_states() {
    for row in republished_rows() {
        let case = row.case;
        let router = router();
        deploy_republished(&router, &row, row.first.0, true).await;
        assert_eq!(
            status_of(&router, row.stack).await,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, reason {:?}",
            reason_of(&router, row.stack).await
        );
        let (published, consumed, identity) =
            published_on_both_surfaces(case, &router, row.stack).await;
        assert_eq!(
            published, row.first.1,
            "case {case:?}: expected the created stack to publish {:?}, got {published:?}",
            row.first.1
        );
        assert_eq!(
            consumed, row.first.1,
            "case {case:?}: expected the reader to be created with {:?}, got {consumed:?}",
            row.first.1
        );

        deploy_republished(&router, &row, row.revised.0, false).await;
        assert_eq!(
            status_of(&router, row.stack).await,
            status::UPDATE_COMPLETE,
            "case {case:?}: expected the revision to complete, reason {:?}",
            reason_of(&router, row.stack).await
        );
        let (republished, reconsumed, same_identity) =
            published_on_both_surfaces(case, &router, row.stack).await;
        assert_eq!(
            republished, row.revised.1,
            "case {case:?}: expected the revision to publish {:?}, got {republished:?}",
            row.revised.1
        );
        assert_eq!(
            reconsumed, row.revised.1,
            "case {case:?}: expected the reader to hold {:?} in the owning service, got \
             {reconsumed:?}",
            row.revised.1
        );
        // The resource was updated in place: what it publishes moved, its identity did not.
        assert_eq!(
            same_identity, identity,
            "case {case:?}: expected the subject to keep the identity {identity:?}, got \
             {same_identity:?}"
        );
    }
}
