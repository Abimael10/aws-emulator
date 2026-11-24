// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of `AWS::AmazonMQ::Configuration`: the engine
//! configuration document a broker of Amazon MQ can be given, created in Amazon MQ itself.
//!
//! One file of [`super::super`]'s per-type arms, dispatched to by its `Provisioner`, beside
//! the `AWS::AmazonMQ::Broker` arm in [`super::databases_and_networks`] — a concern of its
//! own because a configuration is a resource of a different kind from a broker: it is
//! created by `CreateConfiguration` (`POST /v1/configurations`), the DOCUMENT it carries
//! reaches it as a REVISION of its own (`UpdateConfiguration`, `PUT
//! /v1/configurations/{ConfigurationId}`), and what it publishes is that revision's number
//! rather than a host and a port.

use super::*;

/// The URI `CreateConfiguration` is bound to, and the collection a configuration's own URI
/// hangs off: `POST /v1/configurations` creates one, `GET /v1/configurations/{ConfigurationId}`
/// reads one, `PUT` of the same URI adds a revision to one and `DELETE` of it removes one
/// (`smithy_query(query="operation", operation="CreateConfiguration,DescribeConfiguration,\
/// UpdateConfiguration,DeleteConfiguration")` on Amazon MQ's own model).
pub(crate) const MQ_CONFIGURATIONS_URI: &str = "/v1/configurations";

/// The longest configuration name Amazon MQ registers, which is what a GENERATED name for a
/// declaration that states none is held to ([`super::super::template::naming_rules`]).
///
/// Read from `CreateConfigurationRequest$Name`'s own documented rule in that service's model
/// ("This value must be 1-150 characters long"), which is also the bound the Amazon MQ crate
/// holds a create to.
pub(crate) const MQ_CONFIGURATION_NAME_LENGTH: usize = 150;

/// The members `CreateConfiguration` takes whose value goes through unchanged, as
/// (property, member) pairs: the type's `AuthenticationStrategy`, `EngineType` and
/// `EngineVersion` are that call's own members with their initial letter lower-cased
/// (`smithy_query(query="operation", operation="CreateConfiguration")`).
///
/// `Name` is not among them: it is the one value CloudFormation decides
/// ([`declared_name`]), so it is set on the way out. `Tags` is not either — the type
/// publishes the `[{Key, Value}]` list where the create takes a tag MAP ([`tags_as_map`]) —
/// and neither are `Data` and `Description`: `CreateConfiguration` "creates a configuration
/// from the engine's default settings" and takes no document at all, so both travel on the
/// revision the create is followed by ([`create_mq_configuration`]).
pub(crate) const MQ_CONFIGURATION_MEMBERS: &[(&str, &str)] = &[
    ("AuthenticationStrategy", "authenticationStrategy"),
    ("EngineType", "engineType"),
    ("EngineVersion", "engineVersion"),
];

/// Create the configuration an `AWS::AmazonMQ::Configuration` declares, and report the `Id`
/// Amazon MQ ASSIGNED it beside the ARN it built and the revision its declared document is
/// stored as — so a client that reads the stack's outputs can call that service with them
/// (`DescribeConfiguration`, `DescribeConfigurationRevision`, a broker's `Configuration`)
/// rather than being handed identifiers nothing minted.
///
/// TWO calls, because the type's document is not a member of the create: that operation
/// "creates a configuration from the engine's default settings", so the declared `Data` —
/// "the base64-encoded XML configuration" for ActiveMQ, the base64-encoded Cuttlefish one
/// for RabbitMQ — reaches the configuration through `UpdateConfiguration`, which is the one
/// operation of that service that takes a document, and that call also carries the
/// declaration's `Description` (the revision's own). Amazon MQ's own admission is what
/// judges both: an engine type or version it does not offer, a name it will not register and
/// a document it cannot read each leave the resource `CREATE_FAILED` and the stack rolled
/// back rather than a configuration no broker could be given.
///
/// The configuration's NAME is the one value CloudFormation decides ([`declared_name`]); its
/// identity is not CloudFormation's at all ("Ref returns the Amazon MQ configuration ID", a
/// value only that service mints), so the `Id` the create answers with is reported as the
/// resource's physical id and the `Arn` it built around that id as the resource's ARN.
pub(crate) async fn create_mq_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mapped = mapped_members(&resource.properties, MQ_CONFIGURATION_MEMBERS);
    let mut request = mapped.as_object().cloned().unwrap_or_default();
    request.insert("name".to_string(), Value::String(declared_name(resource)));
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert("tags".to_string(), Value::Object(tags));
    }
    // The document is read off the declaration BEFORE the create, so a declaration carrying
    // none is refused with nothing created rather than leaving a configuration behind that
    // no revision of the declared document was ever added to.
    let data = mq_configuration_data(resource)?;
    let created = call_rest_json(
        ctx,
        MQ_SIGNING_NAME,
        region,
        "POST",
        MQ_CONFIGURATIONS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let (Some(configuration_id), Some(configuration_arn)) = (
        created
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty()),
        created
            .get("arn")
            .and_then(Value::as_str)
            .filter(|arn| !arn.is_empty()),
    ) else {
        // The create answered 2xx without the two values its output models. Nothing
        // addresses a configuration without that id, so this is reported as a failure rather
        // than recorded as a resource with a physical id nothing minted.
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: "Amazon MQ answered CreateConfiguration without an id and arn".to_string(),
        });
    };
    let configuration_id = configuration_id.to_string();
    let configuration_arn = configuration_arn.to_string();
    let mut revision = Map::new();
    revision.insert("data".to_string(), Value::String(data));
    if let Some(description) = property(resource, "Description") {
        revision.insert("description".to_string(), Value::String(description));
    }
    let revised = match call_rest_json(
        ctx,
        MQ_SIGNING_NAME,
        region,
        "PUT",
        &mq_configuration_uri(&configuration_id),
        Some(&Value::Object(revision)),
    )
    .await
    {
        Ok(revised) => revised,
        // A configuration whose declared document Amazon MQ will not store is not a
        // configuration this declaration created: the create is ONE step, so the
        // configuration the first call made is taken away again before that service's
        // refusal is reported. Nothing is left behind for a client to find under the
        // declared name, and the id this call minted is not reported anywhere — which is
        // also why the resource could not be rolled back by its physical id. A delete that
        // is itself refused changes nothing about the refusal the caller gets.
        Err(refusal) => {
            let _removed = delete_mq_configuration(ctx, region, &configuration_id).await;
            return Err(refusal);
        }
    };
    let Some(number) = revised
        .get("latestRevision")
        .and_then(|revision| revision.get("revision"))
        .and_then(scalar_text)
    else {
        // The revision was stored and answered for without the number that identifies it.
        // `Revision` is one of the three values this type publishes, and the only value that
        // could stand in its place is one no read of the configuration would agree with, so
        // the resource is reported as failed — and the configuration taken away with it, for
        // the reason the arm above takes one away.
        let _removed = delete_mq_configuration(ctx, region, &configuration_id).await;
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: "Amazon MQ answered UpdateConfiguration without a latestRevision revision"
                .to_string(),
        });
    };
    let mut attributes: BTreeMap<String, String> =
        super::super::template::arn_attributes(&resource.resource_type, &configuration_arn)
            .into_iter()
            .collect();
    attributes.insert(
        super::super::template::MQ_CONFIGURATION_REVISION_ATTRIBUTE.to_string(),
        number,
    );
    Ok(Created {
        physical_resource_id: Some(configuration_id),
        attributes,
    })
}

/// The document the declaration states, as the `data` the revision takes.
///
/// `Data` is already "the base64-encoded XML configuration" (or the base64-encoded
/// Cuttlefish one) where `UpdateConfigurationRequest$Data` is that same base64 text, so the
/// value travels as the declaration wrote it and Amazon MQ's own reading of it is what
/// judges it.
///
/// A declaration carrying no document describes no configuration this type could be created
/// from — the property is REQUIRED — so it is refused here rather than sent as a request
/// that service would have to reject.
pub(crate) fn mq_configuration_data(resource: &StoredResource) -> Result<String, Refusal> {
    property(resource, "Data").ok_or_else(|| Refusal {
        code: MISSING_PROPERTY_CODE.to_string(),
        message: format!("{} declares no Data", resource.resource_type),
    })
}

/// The URI that addresses ONE configuration: the `ConfigurationId` `@httpLabel` every
/// operation on a configuration binds, percent-encoded as a non-greedy label is sent — the
/// same discipline [`mq_broker_uri`] applies to a broker id.
pub(crate) fn mq_configuration_uri(configuration_id: &str) -> String {
    format!(
        "{MQ_CONFIGURATIONS_URI}/{}",
        percent_encoded(configuration_id)
    )
}

/// The configuration as Amazon MQ reports it, for drift detection: `DescribeConfiguration`'s
/// own record, read by the id that service minted. One it no longer holds
/// (`NotFoundException`, judged on the CODE and never on a message that carries the
/// request's own identifier) was deleted.
///
/// Only the `Name` is compared. The read reports the configuration's engine type and
/// authentication strategy in Amazon MQ's own ANSWER spellings (`ActiveMQ` for the
/// `ACTIVEMQ` a declaration sends, `simple` for `SIMPLE`), so comparing either against the
/// declaration's own token would report drift on every configuration rather than on a
/// changed one; the declared `Data` is not reported by this read at all — a revision's
/// document is `DescribeConfigurationRevision`'s to answer — and `Tags` is left out for the
/// reason it is everywhere here (the tags a resource carries change independently of its
/// declaration).
pub(crate) async fn describe_mq_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let uri = mq_configuration_uri(physical_resource_id);
    match call_rest_json(ctx, MQ_SIGNING_NAME, region, "GET", &uri, None).await {
        Ok(answer) => Ok(Some(reported_as_properties(
            &answer,
            MQ_CONFIGURATION_REPORTED_PROPERTIES,
        ))),
        Err(refusal) if refusal.code == MQ_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The members `DescribeConfiguration` reports under this type's own property names, as
/// `(property, member)` pairs — see [`describe_mq_configuration`] for why the others are not
/// among them.
pub(crate) const MQ_CONFIGURATION_REPORTED_PROPERTIES: &[(&str, &str)] = &[("Name", "name")];

/// Delete the configuration a stack created, addressed by the `Id` Amazon MQ minted for it —
/// this resource's physical id, which is what `Ref` returns for the type. A configuration
/// that service no longer holds (`NotFoundException`, judged on the CODE and never on the
/// message, which carries the request's own id) is already in the state the delete asks for.
pub(crate) async fn delete_mq_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let uri = mq_configuration_uri(physical_resource_id);
    match call_rest_json(ctx, MQ_SIGNING_NAME, region, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == MQ_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}
