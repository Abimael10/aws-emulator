// SPDX-License-Identifier: CC0-1.0

//! The IAM role associations an `AWS::RDS::DBInstance` or an `AWS::RDS::DBCluster` declares:
//! the `AssociatedRoles` property, carried to Amazon RDS as the call that service associates
//! one role with one of those resources for one feature. One file of [`super`]'s per-type
//! arms, reached by the readiness arm of each type
//! ([`super::application_services::databases_and_networks::db_instance_readiness`],
//! [`super::rds_db_clusters::db_cluster_readiness`]).
//!
//! A declared property is carried to the owning service by the call that takes it, and these
//! associations are such a property:
//!
//! * `CreateDBInstance` takes no `AssociatedRoles` member at all, and `ModifyDBInstance` takes
//!   none either — `AddRoleToDBInstance(DBInstanceIdentifier, RoleArn, FeatureName)` is the
//!   call that associates one, so EVERY association a DB instance's declaration asks for is
//!   made through it;
//! * `CreateDBCluster` DOES take the list, so a cluster is created with the roles its first
//!   declaration asked for ([`super::rds_db_clusters::CREATE_DB_CLUSTER_MEMBERS`]), and
//!   `ModifyDBCluster` takes no such member — so an association a CHANGED declaration adds is
//!   made by `AddRoleToDBCluster` rather than dropped.
//!
//! Amazon RDS takes either call only on a resource that is available ("to add a role to a DB
//! instance, the status of the DB instance must be available"), which is why this is the
//! READINESS arm's work rather than the create's: the arm runs where that service has just
//! reported the resource up. That is also what makes the associations part of the resource's
//! creation — a stack reports the resource created only once the roles it declared are
//! associated with it, and an association that service REFUSES is the resource's own failure,
//! reported in the words that service answered.
//!
//! The associations the resource already holds are read back first, so the arm converges: the
//! readiness answer is asked again on every pass of the stabilisation walk and on a later
//! update of the same resource, and a role the resource already acts under is left alone
//! instead of being offered to Amazon RDS a second time (which that service refuses).

use super::*;

/// The property either type declares its role associations under, and the two members one
/// entry of it carries (`DBInstanceRole` / `DBClusterRole`: "the Amazon Resource Name (ARN) of
/// the IAM role that is associated with the DB instance" and "the name of the feature
/// associated with the ... IAM role"), which are the members of the call that makes the
/// association.
pub(crate) const ASSOCIATED_ROLES_PROPERTY: &str = "AssociatedRoles";
const ROLE_ARN_MEMBER: &str = "RoleArn";
const FEATURE_NAME_MEMBER: &str = "FeatureName";

/// How ONE of the two types' role associations are made and read: the calls differ only in the
/// member each addresses the resource by, the action names, the element the answer reports one
/// association entry under and the code that service answers for a resource it does not hold.
///
/// Each type states its own record beside that type's arms
/// ([`super::rds_db_instance_roles::DB_INSTANCE_ROLE_CALLS`],
/// [`super::rds_db_clusters::DB_CLUSTER_ROLE_CALLS`]), so what is here is the walk they share.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RoleAssociationCalls {
    /// The member every one of these calls addresses the resource by, which is also this
    /// resource's physical id.
    pub(crate) identifier_member: &'static str,
    /// The call that associates one role with the resource for one feature.
    pub(crate) associate: &'static str,
    /// The call the associations in place are read back off.
    pub(crate) describe: &'static str,
    /// The element that answer reports one `AssociatedRoles` entry under: the `@xmlName` the
    /// list's member fixes (`smithy_query(query="shape",
    /// shape="DBInstanceRoles,DBClusterRoles")`).
    pub(crate) entry_element: &'static str,
    /// The error code that service answers for a resource it does not hold.
    pub(crate) not_found: &'static str,
}

/// Associate every IAM role the declaration asks for that the resource is not already acting
/// under.
///
/// `Ok(None)` is every role associated — including a declaration that asks for none, which
/// makes no call at all. `Ok(Some(refusal))` is Amazon RDS REFUSING one of the associations,
/// answered AS that refusal: what the resource's event reports for it is what it reports for
/// any other refused provider call, and only the refusal itself carries the code that says
/// which `HandlerErrorCode` class it falls in — so the refusal travels to the reporting site
/// ([`provisioning::Readiness::Refused`]) instead of being worded here. `Err` is the refusal of
/// the READ this arm takes first, which says nothing about the associations: the caller decides
/// what an unanswered read means, as it does for every other readiness answer.
pub(crate) async fn associate_declared_roles(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
    calls: RoleAssociationCalls,
) -> Result<Option<Refusal>, Refusal> {
    let asked = declared_roles(declared);
    if asked.is_empty() {
        return Ok(None);
    }
    let held = reported_roles(ctx, region, physical_resource_id, calls).await?;
    // One call per association the resource does not already hold, and the FIRST refusal ends
    // the walk — so a declaration asking for more associations than Amazon RDS allows on one
    // resource makes at most its published quota of calls plus the one that refused, however
    // long the declared list is.
    for (role_arn, feature_name) in asked {
        if held
            .iter()
            .any(|(arn, feature)| *arn == role_arn && *feature == feature_name)
        {
            continue;
        }
        let mut parameters = vec![
            (
                calls.identifier_member.to_string(),
                physical_resource_id.to_string(),
            ),
            (ROLE_ARN_MEMBER.to_string(), role_arn),
        ];
        // The member names the feature the role acts for, and an entry that states none is
        // sent as it was declared: whether the call takes it that way is the owning service's
        // to answer, not something this arm invents a value for.
        if let Some(feature_name) = feature_name {
            parameters.push((FEATURE_NAME_MEMBER.to_string(), feature_name));
        }
        if let Err(refusal) = call_query(
            ctx,
            RDS_SIGNING_NAME,
            region,
            RDS_API_VERSION,
            calls.associate,
            &parameters,
        )
        .await
        {
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

/// The `(RoleArn, FeatureName)` pairs the declaration asks for, in the order it states them.
///
/// A property the declaration states as ONE entry rather than as a list is the one-entry list
/// of itself, exactly as every other list property of this crate is read ([`push_list`]).
///
/// An entry is taken AS DECLARED: a `RoleArn` the declaration does not state is the empty
/// value, carried into the call and refused by the service that owns it, exactly as a missing
/// `FeatureName` is. Dropping such an entry would report a resource created without an
/// association its template asked for.
fn declared_roles(declared: Option<&Value>) -> Vec<(String, Option<String>)> {
    let Some(property) = declared.and_then(|declared| declared.get(ASSOCIATED_ROLES_PROPERTY))
    else {
        return Vec::new();
    };
    let entries: Vec<&Value> = match property {
        Value::Array(values) => values.iter().collect(),
        single => vec![single],
    };
    entries
        .into_iter()
        .map(|entry| {
            (
                entry
                    .get(ROLE_ARN_MEMBER)
                    .and_then(scalar_text)
                    .unwrap_or_default(),
                entry.get(FEATURE_NAME_MEMBER).and_then(scalar_text),
            )
        })
        .collect()
}

/// The `(RoleArn, FeatureName)` pairs Amazon RDS reports the resource already associated with,
/// read off its `AssociatedRoles`.
///
/// A resource that service no longer holds reports no association: the call this read guards
/// then names it and is refused by that service, which is the answer about the resource rather
/// than one this arm would have to word itself.
async fn reported_roles(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    calls: RoleAssociationCalls,
) -> Result<Vec<(String, Option<String>)>, Refusal> {
    let parameters = vec![(
        calls.identifier_member.to_string(),
        physical_resource_id.to_string(),
    )];
    let answered = match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        calls.describe,
        &parameters,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == calls.not_found => return Ok(Vec::new()),
        Err(refusal) => return Err(refusal),
    };
    Ok(
        xml_entry_structures(&answered, ASSOCIATED_ROLES_PROPERTY, calls.entry_element)
            .into_iter()
            .filter_map(|entry| {
                let role_arn = entry.get(ROLE_ARN_MEMBER).and_then(scalar_text)?;
                Some((
                    role_arn,
                    entry.get(FEATURE_NAME_MEMBER).and_then(scalar_text),
                ))
            })
            .collect(),
    )
}
