// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Backup Gateway's own control plane: the hypervisor an
//! `AWS::BackupGateway::Hypervisor` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! ## Why the hypervisor is really imported
//!
//! A hypervisor is the on-premises host Backup Gateway reads virtual machines off, and
//! `ImportHypervisorConfiguration` is the one call that connects to one: it takes the host,
//! the name and the credentials, takes NO identifier at all, and answers with the
//! `HypervisorArn` that service minted. That ARN is the type's primary identifier — what its
//! `Ref` returns — so it becomes the resource's physical id, and it is the `ServerArn` every
//! later call on a hypervisor is addressed by (`GetHypervisor`, `UpdateHypervisor`,
//! `DeleteHypervisor`, `AssociateGatewayToServer`, `PutHypervisorPropertyMappings`,
//! `StartVirtualMachinesMetadataSync`). A stack that reported the resource created while
//! nothing imported the configuration publishes an ARN that resolves to nothing, so no
//! gateway can be pointed at the host and no virtual machine of it is ever discovered.
//!
//! ## The properties
//!
//! The type's properties ARE the owning API's own members, spelled identically on both sides,
//! split across the two calls that take them: [`IMPORT_MEMBERS`] are
//! `ImportHypervisorConfiguration`'s, and `LogGroupArn` is a member of `UpdateHypervisor`
//! alone — so a declaration that states a log group is imported and then updated, which is
//! the pair a client makes to reach the same state. The declared tags travel with the import
//! beside the ones CloudFormation gives the resource ([`tags_of`]), as the `Tags` list of
//! `Tag` structures that call's own member takes.
//!
//! A declared property neither call carries is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]): a hypervisor reported created with less than its
//! declaration asked for is the shallow success this arm exists to avoid.
//!
//! ## What a declaration that service refuses does
//!
//! Nothing here checks a declared value. A value outside the FORM one of those members
//! publishes — the hypervisor's `Name` over `^[a-zA-Z0-9-]*$`, its `Host` over `^.+$`, its
//! `KmsKeyArn` the KMS ARN pattern, its `LogGroupArn` the log-group one — is a value no handler
//! of the type could be given, so the stage that runs before any provider is invoked refuses it
//! and this arm is never reached at all
//! ([`crate::state::property_validation::patterned_properties`]). Every other constraint on
//! those members is that operation's own — their lengths among them — so a declaration outside
//! one of them is refused BY Backup Gateway, and that refusal is what fails the resource and
//! rolls the stack back. One check in one place, worded as the side that owns it words it.

use super::*;

/// Backup Gateway's signing name, from its model (`sigv4Name: backup-gateway`), which is also
/// the ARN namespace its resources carry ([`super::template::BACKUP_GATEWAY_ARN_NAMESPACE`]),
/// and the `X-Amz-Target` prefix its service shape's own name gives
/// (`BackupOnPremises_v20210101`): it speaks `awsJson1_0`, so its calls ride on
/// [`call_json_rpc`].
const BACKUP_GATEWAY_SIGNING_NAME: &str = "backup-gateway";
const BACKUP_GATEWAY_TARGET_PREFIX: &str = "BackupOnPremises_v20210101";

/// The operations this module calls, as Backup Gateway's own model names them.
const IMPORT_HYPERVISOR_CONFIGURATION: &str = "ImportHypervisorConfiguration";
const GET_HYPERVISOR: &str = "GetHypervisor";
const UPDATE_HYPERVISOR: &str = "UpdateHypervisor";
const DELETE_HYPERVISOR: &str = "DeleteHypervisor";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code Backup Gateway answers for a hypervisor it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its read, update and delete), which is
/// what says a delete has nothing left to remove.
const HYPERVISOR_NOT_FOUND: &str = "ResourceNotFoundException";

/// The properties of the type that are members of `ImportHypervisorConfiguration`, each
/// under the member of the same name.
///
/// The tag set is not among them: it is merged with the tags CloudFormation propagates before
/// it is sent ([`tags_of`]).
const IMPORT_MEMBERS: &[(&str, &str)] = &[
    ("Host", "Host"),
    ("KmsKeyArn", "KmsKeyArn"),
    ("Name", "Name"),
    ("Password", "Password"),
    ("Username", "Username"),
];

/// The properties `UpdateHypervisor` takes, each under the member of the same name: the host,
/// the credentials, the name and the log group that call alone carries.
///
/// The encryption key is NOT among them — `KmsKeyArn` is a member of the import and of no
/// member of the update — which is why it is this type's one replacement property
/// ([`super::template::replacement_properties`]) rather than a member of this request.
const UPDATE_MEMBERS: &[(&str, &str)] = &[
    ("Host", "Host"),
    ("LogGroupArn", "LogGroupArn"),
    ("Name", "Name"),
    ("Password", "Password"),
    ("Username", "Username"),
];

/// Every property of the type this arm carries onto the owning API, so a declaration stating
/// one outside the set is REFUSED rather than reported as created without it
/// ([`refuse_unmapped_properties`]): the two calls' members above, and the tag set.
const MAPPED_PROPERTIES: &[&str] = &[
    "Host",
    "KmsKeyArn",
    "LogGroupArn",
    "Name",
    "Password",
    "Tags",
    "Username",
];

/// The property the log group a hypervisor reports into is declared in, which only
/// `UpdateHypervisor` takes: a declaration stating it is imported and then updated.
const LOG_GROUP_ARN_PROPERTY: &str = "LogGroupArn";

/// The member Backup Gateway answers a hypervisor's ARN in — on the import, on the update and
/// on the delete alike — which is also the member each of those calls addresses one by, and
/// the one `Fn::GetAtt` attribute this type publishes.
const HYPERVISOR_ARN_MEMBER: &str = "HypervisorArn";

/// The member `GetHypervisor` answers the hypervisor's details in.
const HYPERVISOR_MEMBER: &str = "Hypervisor";

/// The member a hypervisor's state is reported in, and the two values that end a wait for a
/// freshly imported one: "The possible states are `PENDING`, `ONLINE`, `OFFLINE`, or `ERROR`"
/// (`HypervisorDetails$State`).
const STATE_MEMBER: &str = "State";
const HYPERVISOR_ONLINE: &str = "ONLINE";
const HYPERVISOR_ERROR: &str = "ERROR";

/// The member the TAGGING calls address a resource of that service by (`TagResource` and
/// `UntagResource` take `ResourceARN`), and the member its tag READ takes (`ListTagsForResource`
/// takes `ResourceArn`) — spelled as each of those operations spells it.
const TAG_WRITE_RESOURCE_ARN_MEMBER: &str = "ResourceARN";
const TAG_READ_RESOURCE_ARN_MEMBER: &str = "ResourceArn";

/// The member a tag set travels as on the import and on `TagResource` alike: a list of `Tag`
/// structures, whose members are spelled `Key`/`Value` ([`UPPER_CASED_TAG`]).
const TAGS_MEMBER: &str = "Tags";

/// The member `UntagResource` takes the keys to drop in.
const TAG_KEYS_MEMBER: &str = "TagKeys";

/// The scalar properties `GetHypervisor` reports back under the names the type declares, for
/// drift detection.
///
/// The members it reports that are NOT properties of the type — the hypervisor's state, the
/// metadata sync it last ran and the instant that sync succeeded — are left out: they are
/// that service's to report and no declaration states them. The credentials are left out for
/// the opposite reason: they are `@sensitive` write-only members no read of that service
/// carries at all.
const REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("Host", "Host"),
    ("KmsKeyArn", "KmsKeyArn"),
    ("LogGroupArn", "LogGroupArn"),
    ("Name", "Name"),
];

/// Import the hypervisor an `AWS::BackupGateway::Hypervisor` declares, in Backup Gateway
/// itself — so a client that deployed the stack reads the hypervisor back through that
/// service's own `GetHypervisor` and `ListHypervisors` for the ARN the stack published, and a
/// gateway can be associated with the host the template named.
///
/// The declaration IS the request ([`IMPORT_MEMBERS`]), with the declared tags beside the ones
/// CloudFormation gives the resource ([`tags_of`]) as the `Tags` list that call's own member
/// takes. A declared `LogGroupArn` is applied afterwards through `UpdateHypervisor`, the one
/// call that carries it; where that second call is refused, the hypervisor the import minted is
/// deleted again, so a failed resource leaves nothing behind in that service.
///
/// The identity is Backup Gateway's: the `HypervisorArn` it minted becomes this resource's
/// physical id — the type's primary identifier, which its `Ref` returns — and is the one
/// attribute the type publishes.
pub(super) async fn create_backup_gateway_hypervisor(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, IMPORT_MEMBERS, &mut request);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }
    let answered = call_backup_gateway(
        ctx,
        region,
        IMPORT_HYPERVISOR_CONFIGURATION,
        &Value::Object(request),
    )
    .await?;
    let arn = reported_text(&answered, HYPERVISOR_ARN_MEMBER).unwrap_or_default();
    if arn.is_empty() {
        // There would be no handle a client could call Backup Gateway with, and nothing for
        // the stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Backup Gateway control plane answered an imported hypervisor \
                      without an ARN"
                .to_string(),
        });
    }
    // The log group is the one declared property the import does not carry, so the hypervisor
    // the stack asked for is only the one the template declared once that second call has
    // landed.
    if let Some(log_group) = properties.get(LOG_GROUP_ARN_PROPERTY) {
        let mut request = Map::new();
        request.insert(HYPERVISOR_ARN_MEMBER.to_string(), Value::String(arn.clone()));
        request.insert(LOG_GROUP_ARN_PROPERTY.to_string(), log_group.clone());
        if let Err(refused) =
            call_backup_gateway(ctx, region, UPDATE_HYPERVISOR, &Value::Object(request)).await
        {
            // The resource FAILED, so the stack publishes no identity for it and nothing would
            // ever delete the hypervisor the import had already minted: it is taken away here
            // instead of being left in Backup Gateway for nobody. A cleanup that service also
            // refuses does not displace the refusal that failed the resource, which is the one
            // reported.
            let _ = delete_backup_gateway_hypervisor(ctx, region, &arn).await;
            return Err(refused);
        }
    }
    Ok(Created {
        physical_resource_id: Some(arn.clone()),
        attributes: hypervisor_attributes(&arn),
    })
}

/// Whether Backup Gateway has finished connecting to the hypervisor a stack imported.
///
/// An import leaves the hypervisor `PENDING` — the first of the states
/// `HypervisorDetails$State` documents, in which that service has not reached the host yet, so
/// nothing of the host's inventory can be read and no metadata sync of it would report
/// anything — and `ONLINE` is the state it serves in. `ERROR` is a state no wait brings a
/// hypervisor out of, and so is one that service no longer holds, nothing being left to import
/// it; every other state is one it is still settling in.
pub(super) async fn backup_gateway_hypervisor_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(hypervisor) = read_hypervisor(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Backup Gateway holds no hypervisor {physical_resource_id}"
        )));
    };
    let state = reported_text(&hypervisor, STATE_MEMBER).unwrap_or_default();
    if state == HYPERVISOR_ONLINE {
        return Ok(Readiness::Ready);
    }
    if state == HYPERVISOR_ERROR {
        return Ok(Readiness::Failed(format!(
            "Backup Gateway reports hypervisor {physical_resource_id} as {HYPERVISOR_ERROR}"
        )));
    }
    Ok(Readiness::Pending(state))
}

/// Apply a changed declaration to the hypervisor the stack imported, through
/// `UpdateHypervisor` — the one call that reconfigures a hypervisor that exists.
///
/// It takes the host, the credentials, the name and the log group ([`UPDATE_MEMBERS`]),
/// addressed by the ARN Backup Gateway minted; the encryption key is no member of it, which is
/// why a declaration restating that replaces the hypervisor instead of reaching here
/// ([`super::template::replacement_properties`]). The tags travel separately
/// ([`retag_backup_gateway_hypervisor`]).
pub(super) async fn update_backup_gateway_hypervisor(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    if let Some(property) = unmapped_property(&properties, MAPPED_PROPERTIES) {
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    let mut request = Map::new();
    request.insert(
        HYPERVISOR_ARN_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    copy_members(&properties, UPDATE_MEMBERS, &mut request);
    call_backup_gateway(ctx, region, UPDATE_HYPERVISOR, &Value::Object(request)).await?;
    Ok(Created::attributes(hypervisor_attributes(
        &resource.physical_resource_id,
    )))
}

/// Bring the tags of the hypervisor a resource record names to the ones this deployment gives
/// it, through Backup Gateway's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the import applied — so a key outside it is one this deployment
/// no longer gives the resource and is removed, and one in it with another value is re-valued.
/// Where the two already agree nothing is called at all, which is what makes this safe to ask
/// on every deployment; a hypervisor that service no longer holds has no tags to reconcile.
///
/// Those calls address a resource of that service by its ARN, which is this resource's
/// physical id.
pub(super) async fn retag_backup_gateway_hypervisor(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let arn = Value::String(resource.physical_resource_id.clone());
    let read = Value::Object(
        [(TAG_READ_RESOURCE_ARN_MEMBER.to_string(), arn.clone())]
            .into_iter()
            .collect(),
    );
    let answered = match call_backup_gateway(ctx, region, LIST_TAGS_FOR_RESOURCE, &read).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == HYPERVISOR_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    // That service answers a resource's tags as the `[{Key, Value}]` list a declaration's
    // `Tags` property is also written as, so one reader serves both sides.
    let held = declared_tags(&answered);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        let request = Value::Object(
            [
                (TAG_WRITE_RESOURCE_ARN_MEMBER.to_string(), arn.clone()),
                (
                    TAGS_MEMBER.to_string(),
                    tags_as_list(&added, UPPER_CASED_TAG),
                ),
            ]
            .into_iter()
            .collect(),
        );
        call_backup_gateway(ctx, region, TAG_RESOURCE, &request).await?;
    }
    if !removed.is_empty() {
        let request = Value::Object(
            [
                (TAG_WRITE_RESOURCE_ARN_MEMBER.to_string(), arn),
                (TAG_KEYS_MEMBER.to_string(), tag_keys_as_list(&removed)),
            ]
            .into_iter()
            .collect(),
        );
        call_backup_gateway(ctx, region, UNTAG_RESOURCE, &request).await?;
    }
    Ok(())
}

/// Delete the hypervisor a stack imported, by the ARN it was imported under — this resource's
/// physical id. One Backup Gateway no longer holds (`ResourceNotFoundException`, judged on the
/// CODE and never on a message) is already in the state the delete asks for.
pub(super) async fn delete_backup_gateway_hypervisor(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_backup_gateway(
        ctx,
        region,
        DELETE_HYPERVISOR,
        &addressed_hypervisor(physical_resource_id),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == HYPERVISOR_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The hypervisor's configuration as Backup Gateway reports it, for drift detection:
/// `GetHypervisor`'s own record, read by the ARN the stack published, projected onto the
/// property names the type declares ([`REPORTED_PROPERTIES`]). One that service no longer
/// holds was deleted, which is what `None` reports.
pub(super) async fn describe_backup_gateway_hypervisor(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_hypervisor(ctx, region, physical_resource_id)
        .await?
        .map(|hypervisor| reported_as_properties(&hypervisor, REPORTED_PROPERTIES)))
}

/// The hypervisor record that service holds under `physical_resource_id`, or `None` when it
/// holds none (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_hypervisor(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_backup_gateway(
        ctx,
        region,
        GET_HYPERVISOR,
        &addressed_hypervisor(physical_resource_id),
    )
    .await
    {
        Ok(answered) => Ok(answered.get(HYPERVISOR_MEMBER).cloned()),
        Err(refusal) if refusal.code == HYPERVISOR_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The request that addresses ONE hypervisor: the `HypervisorArn` its read, its update and its
/// delete each take.
fn addressed_hypervisor(physical_resource_id: &str) -> Value {
    Value::Object(
        [(
            HYPERVISOR_ARN_MEMBER.to_string(),
            Value::String(physical_resource_id.to_string()),
        )]
        .into_iter()
        .collect(),
    )
}

/// The attributes a hypervisor gives the resource: the ARN Backup Gateway minted, which is the
/// one return value this type publishes beside its `Ref`.
fn hypervisor_attributes(arn: &str) -> BTreeMap<String, String> {
    [(HYPERVISOR_ARN_MEMBER.to_string(), arn.to_string())]
        .into_iter()
        .collect()
}

/// Call one Backup Gateway operation on the awsJson1_0 wire its model declares.
async fn call_backup_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        BACKUP_GATEWAY_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{BACKUP_GATEWAY_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every member of the two calls is a property the type publishes, and every property is
    /// carried by one of them: the two catalogues and the mapped set cannot drift apart, which
    /// is what keeps a declaration from being reported as applied while a member went nowhere.
    #[test]
    fn every_member_of_the_two_calls_is_a_mapped_property() {
        for (property, _) in IMPORT_MEMBERS.iter().chain(UPDATE_MEMBERS.iter()) {
            assert!(
                MAPPED_PROPERTIES.contains(property),
                "case {property:?}: expected a member of the owning API to be a property this \
                 arm applies, it is not in {MAPPED_PROPERTIES:?}"
            );
        }
        for property in MAPPED_PROPERTIES {
            let carried = *property == TAGS_MEMBER
                || IMPORT_MEMBERS
                    .iter()
                    .chain(UPDATE_MEMBERS.iter())
                    .any(|(declared, _)| declared == property);
            assert!(
                carried,
                "case {property:?}: expected the property to be carried by the import, the \
                 update or the tag calls, it is carried by none"
            );
        }
    }

    /// The drift read reports the properties a declaration states and nothing that service
    /// alone decides: a hypervisor's state, its metadata sync and the credentials no read
    /// carries are not properties to compare a declaration against.
    #[test]
    fn the_drift_read_reports_only_declared_properties() {
        for (property, _) in REPORTED_PROPERTIES {
            assert!(
                MAPPED_PROPERTIES.contains(property),
                "case {property:?}: expected a reported property to be one the type publishes, \
                 it is not in {MAPPED_PROPERTIES:?}"
            );
        }
        for reported in ["State", "LatestMetadataSyncStatus", "Password", "Username"] {
            assert!(
                !REPORTED_PROPERTIES
                    .iter()
                    .any(|(property, _)| *property == reported),
                "case {reported:?}: expected it to be left out of the drift read, it is reported"
            );
        }
    }
}
