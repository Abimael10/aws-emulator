// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of `AWS::MSK::Configuration`: the `server.properties`
//! document an Amazon MSK cluster can be given, created in Amazon MSK itself.
//!
//! One file of [`super::super`]'s per-type arms, dispatched to by its `Provisioner`,
//! beside the two `AWS::MSK::*` CLUSTER arms in [`super::databases_and_networks`] — a
//! concern of its own because a configuration is a resource of a different kind from a
//! cluster: it is created by `CreateConfiguration` (`POST /v1/configurations`), its body
//! travels as a BLOB, and what it publishes is a REVISION rather than brokers.

use super::*;

/// The URI `CreateConfiguration` is bound to, and the collection a configuration's own URI
/// hangs off: `POST /v1/configurations` creates one, `GET /v1/configurations/{Arn}` reads
/// one and `DELETE /v1/configurations/{Arn}` deletes one
/// (`smithy_query(query="operation", operation="CreateConfiguration,DescribeConfiguration,\
/// DeleteConfiguration")` on that service's model).
pub(crate) const MSK_CONFIGURATIONS_URI: &str = "/v1/configurations";

/// The operation this arm's create calls, as Amazon MSK's own model names it — stated so
/// the refusal a failed resource reports is the one that service's SDK would raise for THIS
/// call ([`super::super::provisioning::call_rest_json_named`]).
pub(crate) const MSK_CREATE_CONFIGURATION: &str = "CreateConfiguration";

/// The longest configuration name Amazon MSK registers, which is what a GENERATED name for
/// a declaration that states none is held to ([`super::super::template::naming_rules`]).
///
/// Read from the Amazon MSK crate's own bound on that name
/// (`MAX_CONFIGURATION_NAME_LENGTH`: a 128-character name is created and read back, 129 is
/// refused), so the two cannot drift apart.
pub(crate) const MSK_CONFIGURATION_NAME_LENGTH: usize = 128;

/// The members `CreateConfiguration` takes, as (property, member) pairs: the type's
/// `Description` and `Name` are that call's own `description` and `name`, while the list of
/// Apache Kafka versions the configuration may be applied to is published as
/// `KafkaVersionsList` where the call takes `kafkaVersions`
/// (`smithy_query(query="operation", operation="CreateConfiguration")`) — a per-member
/// correspondence rather than a case change, which is why this type's properties are mapped
/// by name instead of walked.
///
/// `ServerProperties` is not among them: the property is the document's TEXT while the
/// member is a blob, which the restJson1 wire carries base64-encoded
/// ([`msk_server_properties`]).
pub(crate) const MSK_CONFIGURATION_MEMBERS: &[(&str, &str)] = &[
    ("Description", "description"),
    ("KafkaVersionsList", "kafkaVersions"),
];

/// Create the configuration an `AWS::MSK::Configuration` declares, and report the `Arn`
/// Amazon MSK ASSIGNED it beside the revision that service created.
///
/// Every declared property is a `CreateConfiguration` member
/// ([`MSK_CONFIGURATION_MEMBERS`]) except the one the wire spells differently: the
/// `ServerProperties` the type publishes as the document's text is that call's
/// `serverProperties` BLOB, so it travels base64-encoded ([`msk_server_properties`]).
/// VALUES are otherwise untouched: the Apache Kafka versions are the ones the template
/// declared, and Amazon MSK's own admission of them is what judges them — a version that
/// service does not offer leaves the resource `CREATE_FAILED` and the stack rolled back
/// rather than a configuration nothing could be applied to.
///
/// The configuration's NAME is the one value CloudFormation decides ([`declared_name`]), so
/// it is set on the way out; its identity is not CloudFormation's at all ("Ref returns the
/// ARN of the configuration", whose resource path carries an identifier and an ordinal only
/// Amazon MSK mints), so the ARN the create answers with is reported as the resource's
/// physical id ([`msk_configuration_created`]).
pub(crate) async fn create_msk_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let mapped = mapped_members(&resource.properties, MSK_CONFIGURATION_MEMBERS);
    let mut request = mapped.as_object().cloned().unwrap_or_default();
    request.insert("name".to_string(), Value::String(declared_name(resource)));
    request.insert(
        "serverProperties".to_string(),
        Value::String(msk_server_properties(resource)?),
    );
    let reported = call_rest_json_named(
        ctx,
        MSK_SIGNING_NAME,
        region,
        MSK_CREATE_CONFIGURATION,
        "POST",
        MSK_CONFIGURATIONS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    msk_configuration_created(resource, &reported)
}

/// The `server.properties` document the declaration states, as the blob the create takes.
///
/// `ServerProperties` is the document's own TEXT ("contents of the server.properties file"),
/// and `CreateConfigurationRequest$ServerProperties` is a `__blob`, which a restJson1
/// request carries base64-encoded — through `aws_smithy_types::base64`, the standard-alphabet
/// codec every other blob in this crate is rendered with, so one encoder serves them all.
///
/// A declaration carrying no document describes no configuration that could be created — the
/// member is REQUIRED — so it is refused here rather than sent as a request Amazon MSK would
/// have to reject.
pub(crate) fn msk_server_properties(resource: &StoredResource) -> Result<String, Refusal> {
    let text = resource
        .properties
        .get("ServerProperties")
        .and_then(scalar_text)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY_CODE.to_string(),
            message: format!("{} declares no ServerProperties", resource.resource_type),
        })?;
    Ok(::aws_smithy_types::base64::encode(text.as_bytes()))
}

/// What a configuration create answered, as the resource's identity and its published return
/// values: the `Arn` Amazon MSK minted and the revision it created.
///
/// "Ref returns the ARN of the configuration", so that ARN is reported as the physical id
/// ([`Created::physical_resource_id`]), which
/// [`super::super::provisioning::reconcile`] adopts, and under every conventional ARN name a
/// resource answers ([`super::super::template::arn_attributes`]) — one resource must not
/// report two different ARNs. The LATEST REVISION is the other half of what the type
/// publishes, read off the same answer: its number, and the instant Amazon MSK created it,
/// re-rendered to the millisecond precision CloudFormation publishes an instant in
/// ([`crate::state::provisioning::published_instant`]) so a client comparing the stack's
/// output against `DescribeConfiguration`'s own read compares two spellings of one instant.
/// A revision DESCRIPTION is reported only where that service reported one, the member being
/// optional on a revision.
pub(crate) fn msk_configuration_created(
    resource: &StoredResource,
    reported: &Value,
) -> Result<Created, Refusal> {
    let Some(arn) = reported.get("arn").and_then(Value::as_str) else {
        // The create answered 2xx without the ARN its output models. Nothing addresses an
        // MSK configuration without one, so this is reported as a failure rather than
        // recorded as a resource with a physical id nothing minted.
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: "Amazon MSK answered no arn for the configuration it created".to_string(),
        });
    };
    let mut attributes: BTreeMap<String, String> =
        super::super::template::arn_attributes(&resource.resource_type, arn)
            .into_iter()
            .collect();
    let revision = reported.get("latestRevision");
    if let Some(number) = revision
        .and_then(|revision| revision.get("revision"))
        .and_then(scalar_text)
    {
        attributes.insert(MSK_CONFIGURATION_REVISION_NUMBER.to_string(), number);
    }
    if let Some(created_at) = revision
        .and_then(|revision| revision.get("creationTime"))
        .and_then(Value::as_str)
    {
        attributes.insert(
            MSK_CONFIGURATION_REVISION_CREATION_TIME.to_string(),
            crate::state::provisioning::published_instant(created_at),
        );
    }
    if let Some(description) = revision
        .and_then(|revision| revision.get("description"))
        .and_then(Value::as_str)
    {
        attributes.insert(
            MSK_CONFIGURATION_REVISION_DESCRIPTION.to_string(),
            description.to_string(),
        );
    }
    Ok(Created {
        physical_resource_id: Some(arn.to_string()),
        attributes,
    })
}

/// The configuration as Amazon MSK reports it, for drift detection: the record
/// `DescribeConfiguration` answers with, read by the ARN that service minted.
///
/// An ARN Amazon MSK holds no configuration for is the resource having been deleted — judged
/// on the error CODE, never on a message. That service reports an unresolvable configuration
/// reference as a `BadRequestException` rather than as the `NotFoundException` its other
/// resource families answer, so BOTH are read as the configuration being gone
/// ([`MSK_CONFIGURATION_GONE_CODES`]), and only another refusal leaves the check unmade. The
/// record's members are Amazon MSK's own (lower camel), so a declared property spelled
/// identically on both sides takes part in the comparison and one spelled differently leaves
/// the configuration reported as existing rather than as modified — exactly what
/// [`super::super::drift`] documents for every arm there.
pub(crate) async fn describe_msk_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let uri = format!(
        "{MSK_CONFIGURATIONS_URI}/{}",
        percent_encoded(physical_resource_id)
    );
    match call_rest_json(ctx, MSK_SIGNING_NAME, region, "GET", &uri, None).await {
        Ok(answer) => Ok(Some(answer)),
        Err(refusal) if MSK_CONFIGURATION_GONE_CODES.contains(&refusal.code.as_str()) => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The error codes a READ of a configuration Amazon MSK no longer holds answers under: the
/// `NotFoundException` the model declares for `DescribeConfiguration`, and the
/// `BadRequestException` that service answers a configuration ARN that resolves to nothing —
/// which is what a configuration this emulator's Amazon MSK has removed earns.
pub(crate) const MSK_CONFIGURATION_GONE_CODES: &[&str] = &[MSK_NOT_FOUND, "BadRequestException"];

/// Whether Amazon MSK has finished removing the configuration a stack deleted.
///
/// `DeleteConfiguration` answers the configuration [`MSK_CONFIGURATION_DELETING`] — that
/// service takes a deleted configuration away asynchronously, so its own reads still resolve
/// the ARN while they do — and the deletion is finished once it no longer holds it
/// ([`describe_msk_configuration`]). This is what CloudFormation holds the stack operation
/// open on, so a completed delete means the configuration really is gone: a client's own
/// `DescribeConfiguration` on the ARN the stack published then answers that the ARN resolves
/// to nothing rather than reporting a configuration mid-deletion.
pub(crate) async fn msk_configuration_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match describe_msk_configuration(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(read) => Ok(Removal::Pending(
            read.get("state")
                .and_then(Value::as_str)
                .unwrap_or(MSK_CONFIGURATION_DELETING)
                .to_string(),
        )),
    }
}

/// The state Amazon MSK reports a configuration it has been asked to delete in, as its
/// `ConfigurationState` enum spells it (`smithy_query(query="shape",
/// shape="ConfigurationState")`): the state the configuration is reported in while that
/// service is still removing it.
pub(crate) const MSK_CONFIGURATION_DELETING: &str = "DELETING";

/// Delete the configuration a stack created, addressed by the ARN Amazon MSK minted for it —
/// this resource's physical id, which is what `Ref` returns for the type.
///
/// A configuration that service holds none of under that ARN is already in the state the
/// delete asks for: the `NotFoundException` the model declares for `DeleteConfiguration` and
/// the `BadRequestException` Amazon MSK answers a configuration ARN that resolves to nothing
/// are both that ([`MSK_CONFIGURATION_GONE_CODES`]). Judged on the error CODE, never on the
/// message, which carries the request's own ARN. The ARN goes into the URI percent-encoded,
/// as a non-greedy `@httpLabel` is sent, so its `:` and `/` cannot address a different
/// operation.
pub(crate) async fn delete_msk_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let uri = format!(
        "{MSK_CONFIGURATIONS_URI}/{}",
        percent_encoded(physical_resource_id)
    );
    match call_rest_json(ctx, MSK_SIGNING_NAME, region, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if MSK_CONFIGURATION_GONE_CODES.contains(&refusal.code.as_str()) => Ok(()),
        Err(refusal) => Err(refusal),
    }
}
