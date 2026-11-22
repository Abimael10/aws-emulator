// SPDX-License-Identifier: CC0-1.0

//! The Amazon EC2 half of the SDK seam a RUNNING handler calls another service through: the
//! SECURITY GROUP a handler reads, and the rules it revokes from and authorizes on one.
//!
//! TIGHTENING A SECURITY GROUP'S OWN RULES is the shape a handler that polices a network
//! takes, and the shape the provider of a CloudFormation custom resource over a VPC takes:
//! the resource is declared for a group, and the provider's `Create` revokes the rules that
//! group came with while its `Delete` authorizes them again — which is the whole of what such
//! a resource DOES, so a provider that cannot reach this service answers its request with a
//! failure and the stack that declared the resource rolls back. READING WHAT A GROUP ALLOWS is
//! the other half of the same job: a handler that reports or audits a group's rules asks this
//! service for them.
//!
//! Most services a handler reaches bind their requests with an AWS JSON protocol, where the
//! request body IS the operation's input document, so one code path serves every operation of
//! every one of them ([`super::aws_api`]). This service does not: its model declares
//! `ec2Query` (`smithy_query(query="service_info")`), so a request to it is a form-encoded
//! `Action` with the input flattened into the parameters beside it, and the answer is an XML
//! document. That binding is not written out here: the request is built, signed, dispatched
//! and parsed by the CLIENT this crate holds (`aws_sdk_ec2`), over the same in-process
//! callback transport every other call of this seam uses, so the wire details are the
//! client's and the answer is the emulated Amazon EC2 service's own.
//!
//! What a guest can and cannot do here:
//!
//! * The destination is ALWAYS this emulator: the client is given the callback's own transport
//!   and the service's own regional endpoint, so no host name is resolved and no socket is
//!   opened.
//! * The call is signed as the identity the client was built with — the session of the
//!   function's execution role, or the credentials the handler passed that client
//!   ([`super::aws_api::AwsAccess::signing_as`]) — so Amazon EC2 judges it against the
//!   policies in force on that identity exactly as it judges any other signed request.
//! * Only the operations in [`BOUND_OPERATIONS`] are bound, and only the input members each
//!   entry names. An operation or a member this seam does not bind is reported as a call that
//!   was NOT MADE, naming it, rather than sent with a value dropped — a dropped member would
//!   make a call that did something other than what the handler asked for.
//! * The answer's own status and document are what Amazon EC2 answered: a success carries the
//!   output members this module reads off it, and a refusal is that service's own error code,
//!   message and status, which the SDK the guest calls raises as `ClientError`
//!   ([`super::aws_sdk_guest`]).
//!
//! How one call's input is read, what the answer carried and how a refusal is handed back are
//! the same for every service reached through an SDK client, so they are [`super::sdk_call`]'s
//! rather than this module's.

use ::aws_sdk_ec2::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{
    insert_flag, insert_number, insert_structures, insert_text, some_list, Input, Observed,
    PROVIDER_NAME,
};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "Amazon EC2";

/// The operations of Amazon EC2 this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the SECURITY GROUP RULE surface — the
/// inbound and outbound rules a handler authorizes on a group and revokes from one, and the
/// read that reports what a group allows. A handler naming any other operation of the service
/// is told the call was not made and why, because a request this module cannot bind is one it
/// would otherwise be guessing at.
pub const BOUND_OPERATIONS: &[&str] = &[
    "DescribeSecurityGroups",
    "AuthorizeSecurityGroupIngress",
    "AuthorizeSecurityGroupEgress",
    "RevokeSecurityGroupIngress",
    "RevokeSecurityGroupEgress",
];

/// Make ONE Amazon EC2 call: `operation`, with `body` as the operation's input document (the
/// keyword arguments the handler passed its client method), in `region`.
pub async fn call(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    operation: &str,
    body: &str,
    chain: Option<&str>,
) -> Result<Answer, Refusal> {
    let input = Input::read(SERVICE, body)?;
    let observed = Observed::of(SERVICE);
    let client = client(callback, identity, region, observed.clone(), chain);
    match operation {
        "DescribeSecurityGroups" => describe_security_groups(&client, &input, &observed).await,
        "AuthorizeSecurityGroupIngress" => {
            authorize_security_group_ingress(&client, &input, &observed).await
        }
        "AuthorizeSecurityGroupEgress" => {
            authorize_security_group_egress(&client, &input, &observed).await
        }
        "RevokeSecurityGroupIngress" => {
            revoke_security_group_ingress(&client, &input, &observed).await
        }
        "RevokeSecurityGroupEgress" => {
            revoke_security_group_egress(&client, &input, &observed).await
        }
        other => Err(Refusal::NotMade(format!(
            "the AWS SDK this runtime supplies binds no {other} call on {SERVICE}; the \
             operations it binds are {}",
            BOUND_OPERATIONS.join(", ")
        ))),
    }
}

/// An Amazon EC2 client over the in-process callback into this same emulator, signing as the
/// identity the SDK client the guest built was given.
///
/// The transport is in-process, so everything a network client needs and this one cannot have —
/// retries, timeouts, identity caching and stalled-stream protection, each of which needs a
/// sleep implementation the callback transport is not given — is switched off.
///
/// Every request it makes carries the tracing header of the chain this invocation belongs to,
/// where the invocation has one ([`::service_common::xray_trace::ChainHop`]): a call the
/// handler makes is one HOP of the chain that reached the handler.
fn client(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    observed: Observed,
    chain: Option<&str>,
) -> Client {
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .http_client(callback.http_client())
        .interceptor(observed)
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
        );
    let config = match identity {
        Some(identity) => config.credentials_provider(::aws_sdk_ec2::config::Credentials::new(
            identity.access_key_id.clone(),
            identity.secret_access_key.clone(),
            identity.token().map(str::to_string),
            None,
            PROVIDER_NAME,
        )),
        None => config.credentials_provider(callback.aws_credentials_provider()),
    };
    // A call the handler makes is one HOP of the chain that reached the handler.
    let config = match chain {
        Some(chain) => config.interceptor(::service_common::xray_trace::ChainHop::carrying(chain)),
        None => config,
    };
    Client::from_conf(config.build())
}

/// `DescribeSecurityGroups`: the groups the call names, or every group of this account in the
/// Region.
async fn describe_security_groups(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "GroupIds",
        "GroupNames",
        "Filters",
        "MaxResults",
        "NextToken",
        "DryRun",
    ])?;
    let described = client
        .describe_security_groups()
        .set_group_ids(some_list(input.text_list("GroupIds")?))
        .set_group_names(some_list(input.text_list("GroupNames")?))
        .set_filters(some_list(filters(input)?))
        .set_max_results(input.integer("MaxResults")?)
        .set_next_token(input.text("NextToken")?)
        .set_dry_run(input.boolean("DryRun")?)
        .send()
        .await;
    let described = match described {
        Ok(described) => described,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_structures(
        &mut document,
        "SecurityGroups",
        described.security_groups(),
        security_group_document,
    );
    insert_text(&mut document, "NextToken", described.next_token());
    observed.answer(document)
}

/// `AuthorizeSecurityGroupIngress`: the inbound rules the call adds to one group.
async fn authorize_security_group_ingress(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(INGRESS_MEMBERS)?;
    let authorized = client
        .authorize_security_group_ingress()
        .set_group_id(input.text("GroupId")?)
        .set_group_name(input.text("GroupName")?)
        .set_ip_permissions(some_list(ip_permissions(input)?))
        .set_cidr_ip(input.text("CidrIp")?)
        .set_ip_protocol(input.text("IpProtocol")?)
        .set_from_port(input.integer("FromPort")?)
        .set_to_port(input.integer("ToPort")?)
        .set_source_security_group_name(input.text("SourceSecurityGroupName")?)
        .set_source_security_group_owner_id(input.text("SourceSecurityGroupOwnerId")?)
        .set_dry_run(input.boolean("DryRun")?)
        .send()
        .await;
    let authorized = match authorized {
        Ok(authorized) => authorized,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_flag(&mut document, "Return", authorized.r#return());
    insert_structures(
        &mut document,
        "SecurityGroupRules",
        authorized.security_group_rules(),
        security_group_rule_document,
    );
    observed.answer(document)
}

/// `AuthorizeSecurityGroupEgress`: the outbound rules the call adds to one group.
async fn authorize_security_group_egress(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(EGRESS_MEMBERS)?;
    let authorized = client
        .authorize_security_group_egress()
        .set_group_id(input.text("GroupId")?)
        .set_ip_permissions(some_list(ip_permissions(input)?))
        .set_cidr_ip(input.text("CidrIp")?)
        .set_ip_protocol(input.text("IpProtocol")?)
        .set_from_port(input.integer("FromPort")?)
        .set_to_port(input.integer("ToPort")?)
        .set_source_security_group_name(input.text("SourceSecurityGroupName")?)
        .set_source_security_group_owner_id(input.text("SourceSecurityGroupOwnerId")?)
        .set_dry_run(input.boolean("DryRun")?)
        .send()
        .await;
    let authorized = match authorized {
        Ok(authorized) => authorized,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_flag(&mut document, "Return", authorized.r#return());
    insert_structures(
        &mut document,
        "SecurityGroupRules",
        authorized.security_group_rules(),
        security_group_rule_document,
    );
    observed.answer(document)
}

/// `RevokeSecurityGroupIngress`: the inbound rules the call removes from one group.
async fn revoke_security_group_ingress(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(REVOKE_INGRESS_MEMBERS)?;
    let revoked = client
        .revoke_security_group_ingress()
        .set_group_id(input.text("GroupId")?)
        .set_group_name(input.text("GroupName")?)
        .set_ip_permissions(some_list(ip_permissions(input)?))
        .set_cidr_ip(input.text("CidrIp")?)
        .set_ip_protocol(input.text("IpProtocol")?)
        .set_from_port(input.integer("FromPort")?)
        .set_to_port(input.integer("ToPort")?)
        .set_source_security_group_name(input.text("SourceSecurityGroupName")?)
        .set_source_security_group_owner_id(input.text("SourceSecurityGroupOwnerId")?)
        .set_security_group_rule_ids(some_list(input.text_list("SecurityGroupRuleIds")?))
        .set_dry_run(input.boolean("DryRun")?)
        .send()
        .await;
    let revoked = match revoked {
        Ok(revoked) => revoked,
        Err(error) => return observed.refused(&error),
    };
    observed.answer(revocation_document(
        revoked.r#return(),
        revoked.unknown_ip_permissions(),
        revoked.revoked_security_group_rules(),
    ))
}

/// `RevokeSecurityGroupEgress`: the outbound rules the call removes from one group.
async fn revoke_security_group_egress(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(REVOKE_EGRESS_MEMBERS)?;
    let revoked = client
        .revoke_security_group_egress()
        .set_group_id(input.text("GroupId")?)
        .set_ip_permissions(some_list(ip_permissions(input)?))
        .set_cidr_ip(input.text("CidrIp")?)
        .set_ip_protocol(input.text("IpProtocol")?)
        .set_from_port(input.integer("FromPort")?)
        .set_to_port(input.integer("ToPort")?)
        .set_source_security_group_name(input.text("SourceSecurityGroupName")?)
        .set_source_security_group_owner_id(input.text("SourceSecurityGroupOwnerId")?)
        .set_security_group_rule_ids(some_list(input.text_list("SecurityGroupRuleIds")?))
        .set_dry_run(input.boolean("DryRun")?)
        .send()
        .await;
    let revoked = match revoked {
        Ok(revoked) => revoked,
        Err(error) => return observed.refused(&error),
    };
    observed.answer(revocation_document(
        revoked.r#return(),
        revoked.unknown_ip_permissions(),
        revoked.revoked_security_group_rules(),
    ))
}

/// The input members an `AuthorizeSecurityGroupEgress` call carries, as that operation's own
/// input shape names them (`smithy_query(operation="AuthorizeSecurityGroupEgress")`). The
/// rule-tag specifications are not among them: a rule TAG is not part of what a rule allows,
/// and a call naming one is reported as a call that was not made rather than sent without it.
const EGRESS_MEMBERS: &[&str] = &[
    "GroupId",
    "IpPermissions",
    "CidrIp",
    "IpProtocol",
    "FromPort",
    "ToPort",
    "SourceSecurityGroupName",
    "SourceSecurityGroupOwnerId",
    "DryRun",
];

/// The same for `AuthorizeSecurityGroupIngress`, whose group may also be named by NAME: "for a
/// security group in a nondefault VPC, use the security group ID" — the id is the other
/// spelling, and both are the operation's own.
const INGRESS_MEMBERS: &[&str] = &[
    "GroupId",
    "GroupName",
    "IpPermissions",
    "CidrIp",
    "IpProtocol",
    "FromPort",
    "ToPort",
    "SourceSecurityGroupName",
    "SourceSecurityGroupOwnerId",
    "DryRun",
];

/// The same for `RevokeSecurityGroupEgress`, which also names rules by their own ids.
const REVOKE_EGRESS_MEMBERS: &[&str] = &[
    "GroupId",
    "IpPermissions",
    "CidrIp",
    "IpProtocol",
    "FromPort",
    "ToPort",
    "SourceSecurityGroupName",
    "SourceSecurityGroupOwnerId",
    "SecurityGroupRuleIds",
    "DryRun",
];

/// The same for `RevokeSecurityGroupIngress`.
const REVOKE_INGRESS_MEMBERS: &[&str] = &[
    "GroupId",
    "GroupName",
    "IpPermissions",
    "CidrIp",
    "IpProtocol",
    "FromPort",
    "ToPort",
    "SourceSecurityGroupName",
    "SourceSecurityGroupOwnerId",
    "SecurityGroupRuleIds",
    "DryRun",
];

/// What both revocations answer: whether the request was carried out, the permissions the group
/// did not have, and the rules the call removed.
fn revocation_document(
    returned: Option<bool>,
    unknown: &[::aws_sdk_ec2::types::IpPermission],
    revoked: &[::aws_sdk_ec2::types::RevokedSecurityGroupRule],
) -> Map<String, Value> {
    let mut document = Map::new();
    insert_flag(&mut document, "Return", returned);
    insert_structures(
        &mut document,
        "UnknownIpPermissions",
        unknown,
        ip_permission_document,
    );
    insert_structures(
        &mut document,
        "RevokedSecurityGroupRules",
        revoked,
        revoked_security_group_rule_document,
    );
    document
}

/// The `IpPermissions` the call carries, each built from the members the `IpPermission` shape
/// names; an empty list where the call carries none.
fn ip_permissions(input: &Input) -> Result<Vec<::aws_sdk_ec2::types::IpPermission>, Refusal> {
    let mut permissions = Vec::new();
    for stated in input.structures("IpPermissions")? {
        stated.only(&[
            "IpProtocol",
            "FromPort",
            "ToPort",
            "IpRanges",
            "Ipv6Ranges",
            "PrefixListIds",
            "UserIdGroupPairs",
        ])?;
        let mut ranges = Vec::new();
        for range in stated.structures("IpRanges")? {
            range.only(&["CidrIp", "Description"])?;
            ranges.push(
                ::aws_sdk_ec2::types::IpRange::builder()
                    .set_cidr_ip(range.text("CidrIp")?)
                    .set_description(range.text("Description")?)
                    .build(),
            );
        }
        let mut ipv6 = Vec::new();
        for range in stated.structures("Ipv6Ranges")? {
            range.only(&["CidrIpv6", "Description"])?;
            ipv6.push(
                ::aws_sdk_ec2::types::Ipv6Range::builder()
                    .set_cidr_ipv6(range.text("CidrIpv6")?)
                    .set_description(range.text("Description")?)
                    .build(),
            );
        }
        let mut prefixes = Vec::new();
        for prefix in stated.structures("PrefixListIds")? {
            prefix.only(&["PrefixListId", "Description"])?;
            prefixes.push(
                ::aws_sdk_ec2::types::PrefixListId::builder()
                    .set_prefix_list_id(prefix.text("PrefixListId")?)
                    .set_description(prefix.text("Description")?)
                    .build(),
            );
        }
        let mut pairs = Vec::new();
        for pair in stated.structures("UserIdGroupPairs")? {
            pair.only(&[
                "GroupId",
                "GroupName",
                "UserId",
                "Description",
                "VpcId",
                "VpcPeeringConnectionId",
                "PeeringStatus",
            ])?;
            pairs.push(
                ::aws_sdk_ec2::types::UserIdGroupPair::builder()
                    .set_group_id(pair.text("GroupId")?)
                    .set_group_name(pair.text("GroupName")?)
                    .set_user_id(pair.text("UserId")?)
                    .set_description(pair.text("Description")?)
                    .set_vpc_id(pair.text("VpcId")?)
                    .set_vpc_peering_connection_id(pair.text("VpcPeeringConnectionId")?)
                    .set_peering_status(pair.text("PeeringStatus")?)
                    .build(),
            );
        }
        permissions.push(
            ::aws_sdk_ec2::types::IpPermission::builder()
                .set_ip_protocol(stated.text("IpProtocol")?)
                .set_from_port(stated.integer("FromPort")?)
                .set_to_port(stated.integer("ToPort")?)
                .set_ip_ranges(some_list(ranges))
                .set_ipv6_ranges(some_list(ipv6))
                .set_prefix_list_ids(some_list(prefixes))
                .set_user_id_group_pairs(some_list(pairs))
                .build(),
        );
    }
    Ok(permissions)
}

/// The `Filters` a read narrows its answer by, each a name and the values it matches.
fn filters(input: &Input) -> Result<Vec<::aws_sdk_ec2::types::Filter>, Refusal> {
    let mut filters = Vec::new();
    for stated in input.structures("Filters")? {
        stated.only(&["Name", "Values"])?;
        filters.push(
            ::aws_sdk_ec2::types::Filter::builder()
                .set_name(stated.text("Name")?)
                .set_values(some_list(stated.text_list("Values")?))
                .build(),
        );
    }
    Ok(filters)
}

/// One SECURITY GROUP as the answer reported it: the members the `SecurityGroup` shape names,
/// each read off that answer.
fn security_group_document(group: &::aws_sdk_ec2::types::SecurityGroup) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "GroupId", group.group_id());
    insert_text(&mut held, "GroupName", group.group_name());
    insert_text(&mut held, "Description", group.description());
    insert_text(&mut held, "VpcId", group.vpc_id());
    insert_text(&mut held, "OwnerId", group.owner_id());
    insert_text(&mut held, "SecurityGroupArn", group.security_group_arn());
    insert_structures(
        &mut held,
        "IpPermissions",
        group.ip_permissions(),
        ip_permission_document,
    );
    insert_structures(
        &mut held,
        "IpPermissionsEgress",
        group.ip_permissions_egress(),
        ip_permission_document,
    );
    insert_structures(&mut held, "Tags", group.tags(), tag_document);
    Value::Object(held)
}

/// One PERMISSION as the answer reported it: what it allows, and the sources or destinations it
/// allows it from or to.
fn ip_permission_document(permission: &::aws_sdk_ec2::types::IpPermission) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "IpProtocol", permission.ip_protocol());
    insert_number(
        &mut held,
        "FromPort",
        permission.from_port().map(i64::from),
    );
    insert_number(&mut held, "ToPort", permission.to_port().map(i64::from));
    insert_structures(&mut held, "IpRanges", permission.ip_ranges(), |range| {
        let mut reported = Map::new();
        insert_text(&mut reported, "CidrIp", range.cidr_ip());
        insert_text(&mut reported, "Description", range.description());
        Value::Object(reported)
    });
    insert_structures(&mut held, "Ipv6Ranges", permission.ipv6_ranges(), |range| {
        let mut reported = Map::new();
        insert_text(&mut reported, "CidrIpv6", range.cidr_ipv6());
        insert_text(&mut reported, "Description", range.description());
        Value::Object(reported)
    });
    insert_structures(
        &mut held,
        "PrefixListIds",
        permission.prefix_list_ids(),
        |prefix| {
            let mut reported = Map::new();
            insert_text(&mut reported, "PrefixListId", prefix.prefix_list_id());
            insert_text(&mut reported, "Description", prefix.description());
            Value::Object(reported)
        },
    );
    insert_structures(
        &mut held,
        "UserIdGroupPairs",
        permission.user_id_group_pairs(),
        |pair| {
            let mut reported = Map::new();
            insert_text(&mut reported, "GroupId", pair.group_id());
            insert_text(&mut reported, "GroupName", pair.group_name());
            insert_text(&mut reported, "UserId", pair.user_id());
            insert_text(&mut reported, "Description", pair.description());
            insert_text(&mut reported, "VpcId", pair.vpc_id());
            insert_text(
                &mut reported,
                "VpcPeeringConnectionId",
                pair.vpc_peering_connection_id(),
            );
            insert_text(&mut reported, "PeeringStatus", pair.peering_status());
            Value::Object(reported)
        },
    );
    Value::Object(held)
}

/// One RULE as an authorization reported it: the rule's own id, what it allows and where it
/// allows it from or to.
fn security_group_rule_document(rule: &::aws_sdk_ec2::types::SecurityGroupRule) -> Value {
    let mut held = Map::new();
    insert_text(
        &mut held,
        "SecurityGroupRuleId",
        rule.security_group_rule_id(),
    );
    insert_text(&mut held, "GroupId", rule.group_id());
    insert_text(&mut held, "GroupOwnerId", rule.group_owner_id());
    insert_flag(&mut held, "IsEgress", rule.is_egress());
    insert_text(&mut held, "IpProtocol", rule.ip_protocol());
    insert_number(&mut held, "FromPort", rule.from_port().map(i64::from));
    insert_number(&mut held, "ToPort", rule.to_port().map(i64::from));
    insert_text(&mut held, "CidrIpv4", rule.cidr_ipv4());
    insert_text(&mut held, "CidrIpv6", rule.cidr_ipv6());
    insert_text(&mut held, "PrefixListId", rule.prefix_list_id());
    if let Some(referenced) = rule.referenced_group_info() {
        let mut reported = Map::new();
        insert_text(&mut reported, "GroupId", referenced.group_id());
        insert_text(&mut reported, "PeeringStatus", referenced.peering_status());
        insert_text(&mut reported, "UserId", referenced.user_id());
        insert_text(&mut reported, "VpcId", referenced.vpc_id());
        insert_text(
            &mut reported,
            "VpcPeeringConnectionId",
            referenced.vpc_peering_connection_id(),
        );
        held.insert("ReferencedGroupInfo".to_string(), Value::Object(reported));
    }
    insert_text(&mut held, "Description", rule.description());
    insert_text(&mut held, "SecurityGroupRuleArn", rule.security_group_rule_arn());
    insert_structures(&mut held, "Tags", rule.tags(), tag_document);
    Value::Object(held)
}

/// One RULE a revocation removed, as that answer reported it.
fn revoked_security_group_rule_document(
    rule: &::aws_sdk_ec2::types::RevokedSecurityGroupRule,
) -> Value {
    let mut held = Map::new();
    insert_text(
        &mut held,
        "SecurityGroupRuleId",
        rule.security_group_rule_id(),
    );
    insert_text(&mut held, "GroupId", rule.group_id());
    insert_flag(&mut held, "IsEgress", rule.is_egress());
    insert_text(&mut held, "IpProtocol", rule.ip_protocol());
    insert_number(&mut held, "FromPort", rule.from_port().map(i64::from));
    insert_number(&mut held, "ToPort", rule.to_port().map(i64::from));
    insert_text(&mut held, "CidrIpv4", rule.cidr_ipv4());
    insert_text(&mut held, "CidrIpv6", rule.cidr_ipv6());
    insert_text(&mut held, "PrefixListId", rule.prefix_list_id());
    insert_text(&mut held, "ReferencedGroupId", rule.referenced_group_id());
    insert_text(&mut held, "Description", rule.description());
    Value::Object(held)
}

/// One TAG as the answer reported it.
fn tag_document(tag: &::aws_sdk_ec2::types::Tag) -> Value {
    let mut held = Map::new();
    insert_text(&mut held, "Key", tag.key());
    insert_text(&mut held, "Value", tag.value());
    Value::Object(held)
}

#[cfg(test)]
mod tests {
    use super::BOUND_OPERATIONS;

    /// RULE: the table names each bound operation once, so a handler naming one reaches one
    /// arm of the call above.
    #[test]
    fn the_table_names_each_bound_operation_once() {
        let mut names = BOUND_OPERATIONS.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            BOUND_OPERATIONS.len(),
            "case {BOUND_OPERATIONS:?}: expected each bound operation once, got {names:?}"
        );
        for bound in BOUND_OPERATIONS {
            assert!(
                bound
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_ascii_uppercase()),
                "case {bound:?}: expected the service's own spelling of the operation"
            );
        }
    }
}
