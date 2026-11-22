// SPDX-License-Identifier: CC0-1.0

//! A CDK-synthesized custom resource whose PROVIDER does its work through another service's API:
//! the stack reaches a terminal state on the provider's own answer, and the work that provider
//! reported is work that service really did.
//!
//! RULE under test: **a stack's custom resource settles on the answer its provider sends to the
//! request's `ResponseURL`, and a provider written for a managed runtime answers SUCCESS when the
//! AWS SDK call its own code makes is one this emulator serves — so the resource reaches
//! CREATE_COMPLETE, the stack CREATE_COMPLETE, and the change the provider made is there in the
//! service that owns it, exactly as far as the provider's own code went. The provider is sent a
//! `Delete` when the stack comes down, and the stack reaches DELETE_COMPLETE only if the calls
//! that request's handler makes are served too.**
//!
//! That is what the no-response and the answered-failure sentences are the absence of: a provider
//! whose call cannot be made answers FAILED and the resource fails with the reason it sent, and a
//! provider that never answers at all leaves CloudFormation reporting "CloudFormation did not
//! receive a response from your Custom Resource" — which this suite asserts is nowhere in the
//! stack's own events.
//!
//! Every row drives the emulator through its own wire: the stack operations over CloudFormation's
//! awsQuery endpoint, the provider's deployment package staged in Amazon S3 the way `cdk deploy`
//! uploads one, the provider created in AWS Lambda by the stack itself, the answer sent by the
//! provider's own code, and the security group the provider's API calls changed read back through
//! Amazon EC2's own API — so a row whose provider answered without doing the work fails here.
//!
//! The provider code is the shape the CDK synthesizes for a `CustomResourceProvider`: an
//! `__entrypoint__.js` that invokes the user handler and PUTs the response document to the
//! request's `ResponseURL`, beside the `index.js` that makes the calls. The rows differ along
//! what the rule generalises over: the TYPE the custom resource is declared with, and WHICH of
//! the rules the group came with its provider is declared to restrict — so each row's expected
//! state is a different one.

use std::collections::BTreeSet;
use std::io::Write as _;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router_with_engine, settled_stack, xml_blocks, xml_text, xml_texts,
    REGION,
};
use crate::stack_lifecycle::{ec2, ACCOUNT_ID};
use crate::stack_resource_provisioning::s3;

/// The sentence CloudFormation words a resource's failure with when no provider answer ever
/// arrives — the fixed text, whose one variable part is the request id. No row may produce it.
const NO_RESPONSE: &str = "CloudFormation did not receive a response from your Custom Resource.";

/// The entry point a CDK `CustomResourceProvider` function declares as its `Handler`: it logs the
/// request, calls the user handler the package ships beside it, and PUTs the response document to
/// the `ResponseURL` the request carried — retrying a write that is refused.
const ENTRYPOINT: &str = r#"
'use strict';

const https = require('https');
const url = require('url');

const MISSING_PHYSICAL_ID = 'CustomResourceProviderFramework::MISSING_PHYSICAL_ID';
const CREATE_FAILED = 'CustomResourceProviderFramework::CREATE_FAILED';

exports.handler = async function (event, context) {
    const sanitized = { ...event, ResponseURL: '...' };
    console.log(JSON.stringify(sanitized, undefined, 2));
    try {
        const handler = require('./index').handler;
        const answered = (await handler(sanitized, context)) || {};
        await submit('SUCCESS', {
            ...event,
            ...answered,
            PhysicalResourceId: answered.PhysicalResourceId
                ?? event.PhysicalResourceId
                ?? event.RequestId,
        });
    } catch (error) {
        await submit('FAILED', {
            ...event,
            PhysicalResourceId: event.PhysicalResourceId || CREATE_FAILED,
        }, error.message);
    }
};

async function submit(status, event, reason) {
    const document = JSON.stringify({
        Status: status,
        Reason: reason ?? status,
        StackId: event.StackId,
        RequestId: event.RequestId,
        PhysicalResourceId: event.PhysicalResourceId || MISSING_PHYSICAL_ID,
        LogicalResourceId: event.LogicalResourceId,
        NoEcho: event.NoEcho,
        Data: event.Data,
    });
    const parsed = url.parse(event.ResponseURL);
    const request = {
        hostname: parsed.hostname,
        path: parsed.path,
        method: 'PUT',
        headers: {
            'content-type': '',
            'content-length': Buffer.byteLength(document, 'utf8'),
        },
    };
    await withRetries({ attempts: 3 }, send)(request, document);
}

function send(options, body) {
    return new Promise((resolve, reject) => {
        try {
            const request = https.request(options, (response) => {
                response.resume();
                if (!response.statusCode || response.statusCode >= 400) {
                    reject(new Error('Unsuccessful HTTP response: ' + response.statusCode));
                } else {
                    resolve();
                }
            });
            request.on('error', reject);
            request.write(body);
            request.end();
        } catch (error) {
            reject(error);
        }
    });
}

function withRetries(options, fn) {
    return async (...xs) => {
        let attempts = options.attempts;
        for (;;) {
            try {
                return await fn(...xs);
            } catch (error) {
                if (attempts-- <= 0) {
                    throw error;
                }
            }
        }
    };
}
"#;

/// The user handler beside it: the shape the CDK ships for a resource that restricts a VPC's
/// default security group — the `Create` revokes the rules the group came with, the `Delete`
/// authorizes them again, and an `Update` moves the restriction when the group changed. WHICH of
/// the two rules it acts on is what the resource's own properties declare.
const RESTRICT_HANDLER: &str = r#"
'use strict';

const {
    EC2Client,
    AuthorizeSecurityGroupEgressCommand,
    AuthorizeSecurityGroupIngressCommand,
    RevokeSecurityGroupEgressCommand,
    RevokeSecurityGroupIngressCommand,
} = require('@aws-sdk/client-ec2');

const ec2 = new EC2Client({});

exports.handler = async function (event) {
    const groupId = event.ResourceProperties.DefaultSecurityGroupId;
    const account = event.ResourceProperties.Account;
    const directions = event.ResourceProperties.Directions;
    switch (event.RequestType) {
        case 'Create':
            return revoke(groupId, account, directions);
        case 'Update':
            return onUpdate(event);
        case 'Delete':
            return authorize(groupId, account, directions);
    }
};

function outbound() {
    return [{ IpProtocol: '-1', IpRanges: [{ CidrIp: '0.0.0.0/0' }] }];
}

function inbound(groupId, account) {
    return [{
        IpProtocol: '-1',
        UserIdGroupPairs: [{ GroupId: groupId, UserId: account }],
    }];
}

async function onUpdate(event) {
    const oldGroup = event.OldResourceProperties.DefaultSecurityGroupId;
    const newGroup = event.ResourceProperties.DefaultSecurityGroupId;
    const account = event.ResourceProperties.Account;
    const directions = event.ResourceProperties.Directions;
    if (oldGroup !== newGroup) {
        await authorize(oldGroup, account, directions);
        await revoke(newGroup, account, directions);
    }
    return { Data: { GroupId: newGroup } };
}

async function revoke(groupId, account, directions) {
    if (directions.includes('Egress')) {
        await ec2.send(new RevokeSecurityGroupEgressCommand({
            GroupId: groupId,
            IpPermissions: outbound(),
        }));
    }
    if (directions.includes('Ingress')) {
        await ec2.send(new RevokeSecurityGroupIngressCommand({
            GroupId: groupId,
            IpPermissions: inbound(groupId, account),
        }));
    }
    return { Data: { GroupId: groupId } };
}

async function authorize(groupId, account, directions) {
    if (directions.includes('Ingress')) {
        await ec2.send(new AuthorizeSecurityGroupIngressCommand({
            GroupId: groupId,
            IpPermissions: inbound(groupId, account),
        }));
    }
    if (directions.includes('Egress')) {
        await ec2.send(new AuthorizeSecurityGroupEgressCommand({
            GroupId: groupId,
            IpPermissions: outbound(),
        }));
    }
    return { Data: { GroupId: groupId } };
}
"#;

/// The logical ids the CDK gives the constructs of an app whose VPC restricts its default
/// security group: the VPC, the provider's role and function, and the custom resource itself.
const VPC_LOGICAL_ID: &str = "EC2VPCB7E9CE57";
const ROLE_LOGICAL_ID: &str = "CustomVpcRestrictDefaultSGCustomResourceProviderRole26592FE0";
const PROVIDER_LOGICAL_ID: &str =
    "CustomVpcRestrictDefaultSGCustomResourceProviderHandlerDC833E5E";
const RESOURCE_LOGICAL_ID: &str = "EC2VPCRestrictDefaultSecurityGroupCustomResourceF7FE4A4E";

/// The bucket a CDK app stages its file assets in, as `cdk bootstrap` names it:
/// `cdk-<qualifier>-assets-<account>-<region>`.
fn assets_bucket() -> String {
    format!("cdk-hnb659fds-assets-{ACCOUNT_ID}-{REGION}")
}

/// One row: the stack one CDK-shaped app deploys, the type its custom resource is declared with,
/// and which of the group's own rules that resource's provider restricts.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The `Type` the custom resource carries.
    resource_type: &'static str,
    /// The directions the provider revokes on `Create` and authorizes again on `Delete`.
    directions: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        // The resource CDK synthesizes for every VPC with its default
        // `restrictDefaultSecurityGroup` behaviour: both rules the group came with, revoked.
        Row {
            case: "a Custom::VpcRestrictDefaultSG resource restricting both directions",
            stack_name: "cdk-restrict-default-sg",
            resource_type: "Custom::VpcRestrictDefaultSG",
            directions: &["Egress", "Ingress"],
        },
        // The same provider under the other type a custom resource is declared with, restricting
        // the outbound rule alone — so the inbound rule the group came with is still there
        // afterwards.
        Row {
            case: "an AWS::CloudFormation::CustomResource restricting the outbound rule alone",
            stack_name: "cdk-restrict-default-sg-egress",
            resource_type: "AWS::CloudFormation::CustomResource",
            directions: &["Egress"],
        },
    ]
}

/// A .zip archive holding each `(path, source)` entry, the way a client uploads a deployment
/// package.
fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (path, source) in entries {
            writer
                .start_file(*path, options)
                .expect("the archive entry starts");
            writer
                .write_all(source.as_bytes())
                .expect("the archive entry's bytes are written");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Stage one row's provider asset in the bootstrap bucket, as `cdk deploy` uploads it, and answer
/// the key it was stored under.
async fn stage_asset(router: &MultiServiceRouter, row: &Row) -> String {
    let bucket = assets_bucket();
    let created = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert!(
        (200..300).contains(&created.status),
        "case {:?}: the assets bucket must be created first, got {} {}",
        row.case,
        created.status,
        created.body
    );
    let key = format!("assets/{}.zip", row.stack_name);
    let staged = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        &archive(&[
            ("__entrypoint__.js", ENTRYPOINT),
            ("index.js", RESTRICT_HANDLER),
        ]),
    )
    .await;
    assert!(
        (200..300).contains(&staged.status),
        "case {:?}: the asset {key} must be staged first, got {} {}",
        row.case,
        staged.status,
        staged.body
    );
    key
}

/// The template one row deploys: the VPC, the provider's execution role with the grant its own
/// calls need, the provider function whose code is the staged asset, and the custom resource that
/// provider backs — whose properties read the VPC's own default security group.
fn template(row: &Row, key: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            VPC_LOGICAL_ID: {
                "Type": "AWS::EC2::VPC",
                "Properties": {
                    "CidrBlock": "10.0.0.0/16",
                    "EnableDnsHostnames": true,
                    "EnableDnsSupport": true,
                },
            },
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "Inline",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Action": [
                                    "ec2:AuthorizeSecurityGroupIngress",
                                    "ec2:AuthorizeSecurityGroupEgress",
                                    "ec2:RevokeSecurityGroupIngress",
                                    "ec2:RevokeSecurityGroupEgress",
                                ],
                                "Effect": "Allow",
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            PROVIDER_LOGICAL_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"S3Bucket": assets_bucket(), "S3Key": key},
                    "Handler": "__entrypoint__.handler",
                    "Role": {"Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"]},
                    "Runtime": "nodejs22.x",
                    "Timeout": 900,
                },
                "DependsOn": [ROLE_LOGICAL_ID],
            },
            RESOURCE_LOGICAL_ID: {
                "Type": row.resource_type,
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER_LOGICAL_ID, "Arn"]},
                    "DefaultSecurityGroupId": {
                        "Fn::GetAtt": [VPC_LOGICAL_ID, "DefaultSecurityGroup"],
                    },
                    "Account": {"Ref": "AWS::AccountId"},
                    "Directions": row.directions,
                },
                "UpdateReplacePolicy": "Delete",
                "DeletionPolicy": "Delete",
            },
        },
        "Outputs": {
            "DefaultSecurityGroup": {
                "Value": {"Fn::GetAtt": [VPC_LOGICAL_ID, "DefaultSecurityGroup"]},
            },
            "RestrictedGroup": {
                "Value": {"Fn::GetAtt": [RESOURCE_LOGICAL_ID, "GroupId"]},
            },
        },
    })
    .to_string()
}

/// A stack's reported outputs, as the key/value map a client reads.
fn outputs_of(xml: &str) -> std::collections::BTreeMap<String, String> {
    let Some(block) = xml_blocks(xml, "Outputs").into_iter().next() else {
        return std::collections::BTreeMap::new();
    };
    xml_texts(&block, "OutputKey")
        .into_iter()
        .zip(xml_texts(&block, "OutputValue"))
        .collect()
}

/// Every `ResourceStatusReason` the stack's own events carry, so a row can assert that the
/// no-response sentence is not among them.
async fn event_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (status_code, body) =
        call(router, "DescribeStackEvents", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status_code, 200,
        "expected DescribeStackEvents to answer for {stack_name:?}, got {status_code} with {body}"
    );
    xml_texts(&body, "ResourceStatusReason")
}

/// The rules the security group `group_id` holds right now, read through Amazon EC2's own
/// `DescribeSecurityGroups`: one entry per direction and source the group allows.
async fn held_rules(router: &MultiServiceRouter, case: &str, group_id: &str) -> BTreeSet<String> {
    let (status_code, body) =
        ec2(router, "DescribeSecurityGroups", &[("GroupId.1", group_id)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected EC2 to report {group_id}, got {status_code} with {body}"
    );
    let mut held = BTreeSet::new();
    for (direction, tag) in [
        ("Ingress", "ipPermissions"),
        ("Egress", "ipPermissionsEgress"),
    ] {
        for block in xml_blocks(&body, tag) {
            for item in xml_blocks(&block, "item") {
                let protocol = xml_text(&item, "ipProtocol").unwrap_or_default();
                let sources: Vec<String> = xml_texts(&item, "cidrIp")
                    .into_iter()
                    .chain(xml_texts(&item, "cidrIpv6"))
                    .chain(
                        xml_texts(&item, "groupId")
                            .into_iter()
                            .map(|group| format!("group:{group}")),
                    )
                    .collect();
                for source in sources {
                    held.insert(format!("{direction} {protocol} {source}"));
                }
            }
        }
    }
    held
}

/// The two rules a VPC's default security group comes with, keyed by the direction a provider
/// names: "allow all inbound traffic from network interfaces … assigned to the same security
/// group" and "allow all outbound … traffic" (Amazon VPC User Guide, "Default security groups
/// for your VPCs"), as [`held_rules`] renders them.
fn rule_of(direction: &str, group_id: &str) -> String {
    match direction {
        "Ingress" => format!("Ingress -1 group:{group_id}"),
        _ => "Egress -1 0.0.0.0/0".to_string(),
    }
}

/// What the group must hold once one row's provider has carried its `Create` out: the rules it
/// came with, less the ones that row's provider revokes.
fn expected_after_create(row: &Row, group_id: &str) -> BTreeSet<String> {
    ["Ingress", "Egress"]
        .into_iter()
        .filter(|direction| !row.directions.contains(direction))
        .map(|direction| rule_of(direction, group_id))
        .collect()
}

#[tokio::test]
async fn a_cdk_shaped_custom_resource_settles_on_its_providers_own_work() {
    for row in rows() {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let key = stage_asset(&router, &row).await;
        let (status_code, created) = create_stack(
            &router,
            row.stack_name,
            &template(&row, &key),
            &[member("Capabilities.member.1", "CAPABILITY_NAMED_IAM")],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status_code} with {created}"
        );

        let settled = settled_stack(case, &router, REGION, row.stack_name).await;
        let reasons = event_reasons(&router, row.stack_name).await;
        assert!(
            !reasons.iter().any(|reason| reason.contains(NO_RESPONSE)),
            "case {case:?}: no event may report the no-response sentence, got {reasons:?}"
        );
        assert_eq!(
            xml_text(&settled, "StackStatus").unwrap_or_default(),
            "CREATE_COMPLETE",
            "case {case:?}: expected the stack to complete on its provider's answer, got \
             {settled} with {reasons:?}"
        );

        // The provider's answer IS the resource: the `Data` it published is what `Fn::GetAtt`
        // resolves against, so the group the resource reports is the one the VPC published.
        let outputs = outputs_of(&settled);
        let group_id = outputs
            .get("DefaultSecurityGroup")
            .cloned()
            .unwrap_or_default();
        assert!(
            group_id.starts_with("sg-"),
            "case {case:?}: expected the VPC to publish its default security group, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("RestrictedGroup"),
            Some(&group_id),
            "case {case:?}: expected the resource to publish the group its provider acted on, \
             got {outputs:?}"
        );

        // ...and the WORK: the rules the provider revoked are gone from the group Amazon EC2
        // itself reports, and the ones its code left alone are still there.
        let after_create = held_rules(&router, case, &group_id).await;
        let expected = expected_after_create(&row, &group_id);
        assert_eq!(
            after_create, expected,
            "case {case:?}: expected the group to hold {expected:?} once its provider had \
             revoked {:?}, got {after_create:?}",
            row.directions
        );

        // The DELETE is sent to the provider too, and the stack reaches DELETE_COMPLETE only
        // because that request's own calls were served: a provider whose `Delete` could not
        // authorize the rules again answers FAILED, and the resource's removal fails with it.
        let (deleted_code, deleted) =
            call(&router, "DeleteStack", &[member("StackName", row.stack_name)]).await;
        assert_eq!(
            deleted_code, 200,
            "case {case:?}: expected DeleteStack to be accepted, got {deleted_code} with {deleted}"
        );
        let after = settled_stack(case, &router, REGION, row.stack_name).await;
        let status_after = if after.is_empty() {
            // A completed deletion is a stack this service no longer reports by name.
            "DELETE_COMPLETE".to_string()
        } else {
            xml_text(&after, "StackStatus").unwrap_or_default()
        };
        assert_eq!(
            status_after, "DELETE_COMPLETE",
            "case {case:?}: expected the delete to settle on the provider's answer, got \
             {status_after:?} with {after} and {:?}",
            event_reasons(&router, row.stack_name).await
        );
    }
}
