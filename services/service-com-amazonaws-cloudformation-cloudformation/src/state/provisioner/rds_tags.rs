// SPDX-License-Identifier: CC0-1.0

//! The TAGS Amazon RDS holds on a resource a stack already created, brought to the set the
//! deployment in flight gives it. One file of [`super`]'s per-type arms, reached by
//! [`super::Provisioner::retag`].
//!
//! Amazon RDS takes a tag list on every create of these types (`CreateDBInstance$Tags` and
//! its siblings), and that list applies to the resource THAT CALL creates: no call that
//! changes a resource which already exists carries a tag at all — `ModifyDBInstance`,
//! `ModifyDBCluster` and the rest publish no `Tags` member — so the tags of such a resource
//! are `AddTagsToResource` ("adds metadata tags to an Amazon RDS resource") and
//! `RemoveTagsFromResource` ("removes metadata tags from an Amazon RDS resource"). Both
//! address the resource by its ARN, through `ResourceName` ("This value is an Amazon Resource
//! Name (ARN)"), and `ListTagsForResource` reads the set that is in force — which is what
//! makes this reconciliation a delta rather than a blind write.
//!
//! One arm for every Amazon RDS type whose tags that service holds, because the arithmetic
//! and the three calls are the same for all of them: only the member of the record that
//! carries the resource's ARN is per type ([`reported_arn_member`]).

use super::*;

/// Bring the tags of the Amazon RDS resource `resource` names to the set this deployment
/// gives it, through that service's own `AddTagsToResource` and `RemoveTagsFromResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this
/// deployment no longer gives the resource and is removed, and one in it with another value
/// is re-valued. Where the two sets already agree nothing is called at all, which is what
/// makes this safe to ask on every deployment.
///
/// The resource is addressed by the ARN AMAZON RDS reports for it, read off that service's
/// own record of the resource ([`super::Provisioner::actual_configuration`], the same read
/// drift detection takes): the tagging calls resolve a resource BY that ARN, so an ARN
/// computed here rather than reported would address something that service does not hold. A
/// resource it no longer holds, and one whose record carries no ARN, have no tags to
/// reconcile.
pub(super) async fn retag_rds_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    provisioner: Provisioner,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let Some(member) = reported_arn_member(provisioner) else {
        return Ok(());
    };
    let Some(reported) =
        Box::pin(provisioner.actual_configuration(ctx, region, resource)).await?
    else {
        return Ok(());
    };
    let Some(arn) = reported
        .get(member)
        .and_then(scalar_text)
        .filter(|arn| !arn.is_empty())
    else {
        return Ok(());
    };
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&listed_tags(ctx, region, &arn).await?, &desired);
    // Neither call can carry a key in a namespace Amazon RDS reserves
    // ([`reserved_by_rds`]), so the halves it applies are the keys a tagging call may name.
    let added: Vec<(String, String)> = added
        .into_iter()
        .filter(|(key, _)| !reserved_by_rds(key))
        .collect();
    let removed: Vec<String> = removed
        .into_iter()
        .filter(|key| !reserved_by_rds(key))
        .collect();
    if !added.is_empty() {
        let mut parameters = vec![(RESOURCE_NAME_MEMBER.to_string(), arn.clone())];
        push_tag_list(&mut parameters, "Tags", "Tag", &added);
        call_query(
            ctx,
            RDS_SIGNING_NAME,
            region,
            RDS_API_VERSION,
            "AddTagsToResource",
            &parameters,
        )
        .await?;
    }
    if !removed.is_empty() {
        let mut parameters = vec![(RESOURCE_NAME_MEMBER.to_string(), arn.clone())];
        // `TagKeys` is a list of plain strings whose member fixes no name of its own
        // (`RemoveTagsFromResource$TagKeys`, targeting `KeyList`), so each key is indexed
        // from 1 under the protocol's default element.
        for (index, key) in removed.iter().enumerate() {
            parameters.push((format!("TagKeys.member.{}", index + 1), key.clone()));
        }
        call_query(
            ctx,
            RDS_SIGNING_NAME,
            region,
            RDS_API_VERSION,
            "RemoveTagsFromResource",
            &parameters,
        )
        .await?;
    }
    Ok(())
}

/// The member every one of these three calls names its resource by: `ResourceName`, the ARN
/// member each of their inputs carries (`smithy_query(query="operation",
/// operation="AddTagsToResource,ListTagsForResource,RemoveTagsFromResource")`).
const RESOURCE_NAME_MEMBER: &str = "ResourceName";

/// The tag set Amazon RDS reports for the resource `arn` names, as the ordered pairs the
/// reconciliation compares ([`tag_delta`]).
///
/// `ListTagsForResource` answers its `TagList` as a list of `Tag` structures whose member
/// fixes the element name `Tag` (`TagList`'s `@xmlName`), so the entries are read under that
/// name. A tag with no key is not a tag and contributes nothing; a value the answer leaves
/// empty is the empty string, which is the value that resource carries.
async fn listed_tags(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    arn: &str,
) -> Result<Vec<(String, String)>, Refusal> {
    let parameters = vec![(RESOURCE_NAME_MEMBER.to_string(), arn.to_string())];
    let answered = call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "ListTagsForResource",
        &parameters,
    )
    .await?;
    Ok(xml_entry_structures(&answered, "TagList", "Tag")
        .into_iter()
        .filter_map(|tag| {
            let key = tag
                .get("Key")
                .and_then(scalar_text)
                .filter(|key| !key.is_empty())?;
            Some((
                key,
                tag.get("Value").and_then(scalar_text).unwrap_or_default(),
            ))
        })
        .collect())
}

/// The member of the record Amazon RDS reports a resource of this type under that carries
/// the resource's ARN, for each type whose tags reach that service through its own tagging
/// calls — and `None` for every other type, which is what decides whether
/// [`retag_rds_resource`] is the arm at all.
///
/// Each shape names it `<record>Arn` (`smithy_query(query="shape", shape="DBInstance")` and
/// the sibling shapes beside it), and the records themselves are the ones that service's own
/// read of the resource answers with ([`super::Provisioner::actual_configuration`]).
///
/// `AWS::RDS::DBClusterEndpoint` and `AWS::RDS::DBProxyTargetGroup` are left out: this
/// emulator's Amazon RDS holds no tag list for either, so no tagging call of it addresses
/// one, and their tags are reconciled by no arm yet exactly as
/// [`super::Provisioner::retag`]'s own default leaves every type without an arm.
pub(super) fn reported_arn_member(provisioner: Provisioner) -> Option<&'static str> {
    Some(match provisioner {
        Provisioner::RdsDbInstance => "DBInstanceArn",
        Provisioner::RdsDbCluster => "DBClusterArn",
        Provisioner::RdsDbSubnetGroup => "DBSubnetGroupArn",
        Provisioner::RdsDbParameterGroup => "DBParameterGroupArn",
        Provisioner::RdsDbClusterParameterGroup => "DBClusterParameterGroupArn",
        Provisioner::RdsOptionGroup => "OptionGroupArn",
        Provisioner::RdsDbSnapshot => "DBSnapshotArn",
        Provisioner::RdsClusterSnapshot => "DBClusterSnapshotArn",
        Provisioner::RdsEventSubscription => "EventSubscriptionArn",
        Provisioner::RdsGlobalCluster => "GlobalClusterArn",
        Provisioner::RdsDbProxy => "DBProxyArn",
        _ => return None,
    })
}

/// The prefix Amazon RDS reserves for the tags IT assigns, beside the one AWS reserves for
/// its own ([`is_reserved_tag_key`]): `Tag$Key` is "the required name of the tag … can't be
/// prefixed with `aws:` or `rds:`", and `Tag$Value` documents the same two.
const RDS_RESERVED_TAG_KEY_PREFIX: &str = "rds:";

/// Whether `key` is in a namespace Amazon RDS's own `Tag$Key` reserves, so neither tagging
/// call may write or drop it.
///
/// The tags in those namespaces are set from INSIDE the services that own them — the three
/// `aws:cloudformation:*` records a resource carries travel with the create that made it —
/// and they are not a deployment's to change: a resource's identity, the stack it belongs to
/// and its logical id in that stack are fixed for as long as the resource is the same
/// resource. So the reconciliation covers the keys a tagging call can carry, and a stack is
/// not failed over a tag CloudFormation adds of its own accord.
///
/// Matched case-insensitively, as the services that refuse such a key match it.
fn reserved_by_rds(key: &str) -> bool {
    is_reserved_tag_key(key)
        || key
            .get(..RDS_RESERVED_TAG_KEY_PREFIX.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(RDS_RESERVED_TAG_KEY_PREFIX))
}
