// SPDX-License-Identifier: CC0-1.0

//! The CloudFront resources whose create takes the DECLARATION'S OWN PROPERTIES as the
//! request — `AWS::CloudFront::KeyValueStore`, `AWS::CloudFront::Function`,
//! `AWS::CloudFront::ConnectionFunction` and `AWS::CloudFront::ConnectionGroup` — created in
//! Amazon CloudFront itself.
//!
//! ## Why they are really created
//!
//! Each of these types names a resource the rest of the CloudFront surface addresses by the
//! identity that service MINTS: a key value store by the ARN its data plane takes as every
//! route's `KvsARN`, a connection function by the id its publish, update and delete take, a
//! connection group by the id a distribution tenant is routed through. A stack that reported
//! one created while nothing created it publishes an identity that resolves to nothing — the
//! first read on a stack output is answered `EntityNotFound`, blaming the client for an id the
//! deployment itself produced — so the deployment calls that service's own create, which is
//! what makes the identity the stack publishes one CloudFront really holds.
//!
//! ## The shape these three share, and where they differ from [`super`]'s two
//!
//! A distribution, an origin access control and the policy and key resources beside them are
//! declared as ONE configuration structure that IS the create's payload. These three are not:
//! each type publishes its properties directly, and the create's payload is the operation's
//! own request shape (`CreateKeyValueStoreRequest`, `CreateConnectionFunctionRequest`,
//! `CreateConnectionGroupRequest`), so each property is rendered as the member of that request
//! it names. What is judged is still the one level a template and the API part company at —
//! a property the TYPE does not publish is REFUSED rather than dropped — and below it a
//! member travels under the name the template wrote, through the same rendering the catalogue
//! in [`super`] uses, so the `{Quantity, Items}` containers and the two spellings of a key
//! value store's import ARN are resolved in one place.
//!
//! Three things are this module's own:
//!
//! * a key value store is addressed by its NAME (`DescribeKeyValueStore`,
//!   `UpdateKeyValueStore` and `DeleteKeyValueStore` each bind `{Name}`) while the identity
//!   the type publishes is the store's ARN, so the name the declaration states is what the
//!   calls after the create take — and a FUNCTION is in that same position, addressed by name
//!   while the ARN a cache behaviour names it by is its identity;
//! * a function's CODE is a blob member, which travels base64-encoded on the restXml wire,
//!   and a declaration asking for the function to be published reaches the LIVE stage through
//!   that function's own publish — the step that makes it one a distribution can associate;
//! * "the specified CloudFront resource hasn't been disabled" is what a connection group's
//!   delete answers while the group is still enabled, so the delete disables it first, exactly
//!   as a distribution's does.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{Created, Refusal, Removal, StoredResource};

/// The resource types this module provisions.
pub(crate) const CLOUDFRONT_KEY_VALUE_STORE: &str =
    crate::state::template::CLOUDFRONT_KEY_VALUE_STORE;
pub(crate) const CLOUDFRONT_FUNCTION: &str = crate::state::template::CLOUDFRONT_FUNCTION;
pub(crate) const CLOUDFRONT_CONNECTION_FUNCTION: &str =
    crate::state::template::CLOUDFRONT_CONNECTION_FUNCTION;
pub(crate) const CLOUDFRONT_CONNECTION_GROUP: &str =
    crate::state::template::CLOUDFRONT_CONNECTION_GROUP;

/// Which of them a resource is. Resolved once, so every call below is about that same
/// resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `AWS::CloudFront::KeyValueStore`.
    KeyValueStore,
    /// `AWS::CloudFront::Function`.
    Function,
    /// `AWS::CloudFront::ConnectionFunction`.
    ConnectionFunction,
    /// `AWS::CloudFront::ConnectionGroup`.
    ConnectionGroup,
}

/// The kind of resource `resource_type` is, or `None` for a type this module does not own.
pub(crate) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        CLOUDFRONT_KEY_VALUE_STORE => Some(Kind::KeyValueStore),
        CLOUDFRONT_FUNCTION => Some(Kind::Function),
        CLOUDFRONT_CONNECTION_FUNCTION => Some(Kind::ConnectionFunction),
        CLOUDFRONT_CONNECTION_GROUP => Some(Kind::ConnectionGroup),
        _ => None,
    }
}

/// Every property `AWS::CloudFront::KeyValueStore` publishes, plus the `Tags` its create
/// takes. A declared property this list does not carry is one no call here applies.
const KEY_VALUE_STORE_PROPERTIES: &[&str] = &["Name", "Comment", "ImportSource", "Tags"];

/// Every property `AWS::CloudFront::ConnectionFunction` publishes, plus the `Tags` its create
/// takes.
const CONNECTION_FUNCTION_PROPERTIES: &[&str] = &[
    "Name",
    "ConnectionFunctionConfig",
    "ConnectionFunctionCode",
    "AutoPublish",
    "Tags",
];

/// Every property `AWS::CloudFront::Function` publishes beyond the read-only metadata it
/// reports, plus the `Tags` its create takes.
const FUNCTION_PROPERTIES: &[&str] = &[
    "Name",
    "FunctionConfig",
    "FunctionCode",
    "AutoPublish",
    "Tags",
];

/// Every property `AWS::CloudFront::ConnectionGroup` publishes.
const CONNECTION_GROUP_PROPERTIES: &[&str] = &[
    "Name",
    "Ipv6Enabled",
    "AnycastIpListId",
    "Enabled",
    "Tags",
];

/// The members `FunctionConfig` publishes, in the order that shape declares them
/// (`smithy_query(query="shape",shape="FunctionConfig")`). The template spells each one
/// identically.
const FUNCTION_CONFIG_MEMBERS: &[&str] = &["Comment", "Runtime", "KeyValueStoreAssociations"];

/// The error code CloudFront answers for a resource of these kinds that this account does not
/// hold, which is what says a delete has nothing left to remove: "The entity was not found."
const ENTITY_NOT_FOUND: &str = "EntityNotFound";

/// The code every FUNCTION operation answers for a function this account does not hold, which
/// is that surface's own wording of the same thing.
const NO_SUCH_FUNCTION_EXISTS: &str = "NoSuchFunctionExists";

/// The code a connection group's delete answers while the group is still enabled: "The
/// specified CloudFront resource hasn't been disabled yet."
const RESOURCE_NOT_DISABLED: &str = "ResourceNotDisabled";

/// Create the resource one of these declarations describes, in Amazon CloudFront itself.
///
/// `Err` is CloudFront's own refusal of the create: a name another resource of the kind
/// already has, a function runtime it does not serve, an Anycast static IP list the group
/// names that does not exist.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    match kind {
        Kind::KeyValueStore => create_key_value_store(ctx, region, resource, propagated).await,
        Kind::Function => create_function(ctx, region, resource, propagated).await,
        Kind::ConnectionFunction => {
            create_connection_function(ctx, region, resource, propagated).await
        }
        Kind::ConnectionGroup => create_connection_group(ctx, region, resource, propagated).await,
    }
}

/// Apply a CHANGED declaration to the resource the stack already has, through CloudFront's own
/// update — the call that takes the resource's identity, so it keeps it and nothing is
/// replaced.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    match kind {
        // "The comment of the key value store" is what an update of one carries; the name and
        // the import source are what the store was created from.
        Kind::KeyValueStore => {
            refuse_unapplied(declared, resource, KEY_VALUE_STORE_PROPERTIES)?;
            let name = required(declared, "Name", resource)?;
            let path = key_value_store_path(&name);
            let (_, version) = super::read_version(ctx, region, &path).await?;
            let comment = declared_text(declared, "Comment").unwrap_or_default();
            let body = request_document(
                "UpdateKeyValueStoreRequest",
                &super::xml_leaf("Comment", &comment),
            );
            let answered =
                super::call_cloudfront(ctx, region, "PUT", &path, Some(&body), version.as_deref())
                    .await?;
            Ok(Some(Created::attributes(key_value_store_attributes(
                &answered,
            ))))
        }
        // A FUNCTION's update takes the whole configuration and the code again, addressed by
        // the name it was created under, and a declaration asking for the function to be
        // published reaches the LIVE stage after it.
        Kind::Function => {
            refuse_unapplied(declared, resource, FUNCTION_PROPERTIES)?;
            let name = required(declared, "Name", resource)?;
            let (_, version) =
                super::read_version(ctx, region, &function_describe_path(&name)).await?;
            let body = request_document(
                "UpdateFunctionRequest",
                &format!(
                    "{}{}",
                    config_element(declared, resource, "FunctionConfig")?,
                    code_element(declared, resource, "FunctionCode")?
                ),
            );
            let (answered, headers) = super::call_cloudfront_versioned(
                ctx,
                region,
                "PUT",
                &function_path(&name),
                Some(&body),
                version.as_deref(),
            )
            .await?;
            let published_summary =
                published(ctx, region, declared, &function_path(&name), &headers).await?;
            Ok(Some(Created::attributes(with_published_stage(
                function_attributes(&answered),
                published_summary.as_deref(),
            ))))
        }
        Kind::ConnectionFunction => {
            refuse_unapplied(declared, resource, CONNECTION_FUNCTION_PROPERTIES)?;
            let id = function_id(resource)?;
            let path = connection_function_path(&id);
            let (_, version) =
                super::read_version(ctx, region, &connection_function_describe_path(&id)).await?;
            let body = request_document(
                "UpdateConnectionFunctionRequest",
                &format!(
                    "{}{}",
                    config_element(declared, resource, "ConnectionFunctionConfig")?,
                    code_element(declared, resource, "ConnectionFunctionCode")?
                ),
            );
            let (answered, headers) = super::call_cloudfront_versioned(
                ctx,
                region,
                "PUT",
                &path,
                Some(&body),
                version.as_deref(),
            )
            .await?;
            let published_summary = published(ctx, region, declared, &path, &headers).await?;
            Ok(Some(Created::attributes(with_published_connection_stage(
                connection_function_attributes(&answered),
                published_summary.as_deref(),
            ))))
        }
        // `UpdateConnectionGroup` takes the three members a declaration may change; the
        // group's name is what it was created under.
        Kind::ConnectionGroup => {
            refuse_unapplied(declared, resource, CONNECTION_GROUP_PROPERTIES)?;
            let id = &resource.physical_resource_id;
            let path = connection_group_path(id);
            let (_, version) = super::read_version(ctx, region, &path).await?;
            let body = request_document(
                "UpdateConnectionGroupRequest",
                &rendered_properties(declared, &["Ipv6Enabled", "AnycastIpListId", "Enabled"]),
            );
            let answered =
                super::call_cloudfront(ctx, region, "PUT", &path, Some(&body), version.as_deref())
                    .await?;
            Ok(Some(Created::attributes(connection_group_attributes(
                &answered,
            ))))
        }
    }
}

/// Delete the resource a stack created, addressed by the identity CloudFront minted for it.
///
/// A resource CloudFront no longer holds is already in the state the delete asks for, judged
/// on the CODE and never on a message. Each of these deletes is conditional on the version
/// that resource's own read reports, so the version is read first.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    match kind {
        Kind::KeyValueStore => {
            let name = required(&resource.properties, "Name", resource)?;
            let path = key_value_store_path(&name);
            let Some(version) = version_of(ctx, region, &path).await? else {
                return Ok(());
            };
            removed(
                super::call_cloudfront(ctx, region, "DELETE", &path, None, version.as_deref())
                    .await,
            )
        }
        Kind::Function => {
            let name = required(&resource.properties, "Name", resource)?;
            let Some(version) = version_of(ctx, region, &function_describe_path(&name)).await?
            else {
                return Ok(());
            };
            removed(
                super::call_cloudfront(
                    ctx,
                    region,
                    "DELETE",
                    &function_path(&name),
                    None,
                    version.as_deref(),
                )
                .await,
            )
        }
        Kind::ConnectionFunction => {
            let id = function_id(resource)?;
            let Some(version) =
                version_of(ctx, region, &connection_function_describe_path(&id)).await?
            else {
                return Ok(());
            };
            removed(
                super::call_cloudfront(
                    ctx,
                    region,
                    "DELETE",
                    &connection_function_path(&id),
                    None,
                    version.as_deref(),
                )
                .await,
            )
        }
        Kind::ConnectionGroup => {
            remove_connection_group(ctx, region, &resource.physical_resource_id).await
        }
    }
}

/// Whether CloudFront has finished removing a deleted resource.
///
/// Each of these resources is gone as its delete answers: a connection group's delete is the
/// one that waits, and what it waits for — the group being disabled — is carried out by the
/// delete itself ([`remove_connection_group`]) rather than left to a later pass.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Create the key value store an `AWS::CloudFront::KeyValueStore` declares.
///
/// The store's identity is its ARN: that is what the key value store DATA plane takes as the
/// `KvsARN` of every one of its routes, and what a function's `KeyValueStoreAssociation`
/// names, so it is the identifier the stack publishes.
async fn create_key_value_store(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied(&resource.properties, resource, KEY_VALUE_STORE_PROPERTIES)?;
    let name = required(&resource.properties, "Name", resource)?;
    let members = format!(
        "{}{}{}{}",
        super::xml_leaf("Name", &name),
        declared_text(&resource.properties, "Comment")
            .map(|comment| super::xml_leaf("Comment", &comment))
            .unwrap_or_default(),
        rendered_properties(&resource.properties, &["ImportSource"]),
        tags_element(&resource.properties, propagated),
    );
    let body = request_document("CreateKeyValueStoreRequest", &members);
    let answered = super::call_cloudfront(
        ctx,
        region,
        "POST",
        "/key-value-store",
        Some(&body),
        None,
    )
    .await?;
    let attributes = key_value_store_attributes(&answered);
    let arn = attributes
        .get("Arn")
        .cloned()
        .ok_or_else(|| Refusal {
            code: "UnidentifiedFailure".to_string(),
            message: "CreateKeyValueStore answered no KeyValueStore.ARN".to_string(),
        })?;
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Create the function an `AWS::CloudFront::Function` declares, and publish it where the
/// declaration asks for that.
///
/// "Ref returns the function ARN", which is also what "a distribution's cache behavior" names
/// a published function by — so the ARN CloudFront built for the function is the identity the
/// stack publishes, while the NAME the declaration states is what every call on the function
/// takes.
async fn create_function(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied(&resource.properties, resource, FUNCTION_PROPERTIES)?;
    let name = required(&resource.properties, "Name", resource)?;
    let members = format!(
        "{}{}{}{}",
        super::xml_leaf("Name", &name),
        config_element(&resource.properties, resource, "FunctionConfig")?,
        code_element(&resource.properties, resource, "FunctionCode")?,
        tags_element(&resource.properties, propagated),
    );
    let body = request_document("CreateFunctionRequest", &members);
    let (answered, headers) =
        super::call_cloudfront_versioned(ctx, region, "POST", "/function", Some(&body), None)
            .await?;
    let attributes = function_attributes(&answered);
    let arn = attributes.get("FunctionARN").cloned().ok_or_else(|| Refusal {
        code: "UnidentifiedFailure".to_string(),
        message: "CreateFunction answered no FunctionMetadata.FunctionARN".to_string(),
    })?;
    let published_summary = published(
        ctx,
        region,
        &resource.properties,
        &function_path(&name),
        &headers,
    )
    .await?;
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes: with_published_stage(attributes, published_summary.as_deref()),
    })
}

/// The attributes a function reports once a publish that was asked for has been carried out:
/// the stage it is in NOW, read off what that publish answered, over the ones its create left.
fn with_published_stage(
    created: BTreeMap<String, String>,
    published: Option<&str>,
) -> BTreeMap<String, String> {
    let Some(published) = published else {
        return created;
    };
    let mut attributes = created;
    for (attribute, value) in function_attributes(published) {
        attributes.insert(attribute, value);
    }
    attributes
}

/// Create the connection function an `AWS::CloudFront::ConnectionFunction` declares, and
/// publish it where the declaration asks for that.
///
/// The function's identity is the ARN CloudFront built around the id it minted, which is what
/// a distribution's `ConnectionFunctionAssociation` and every tag call on one name; the id
/// itself travels beside it as the attribute the type publishes, since that is what the
/// function's own update, publish and delete take.
async fn create_connection_function(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied(&resource.properties, resource, CONNECTION_FUNCTION_PROPERTIES)?;
    let name = declared_text(&resource.properties, "Name")
        .unwrap_or_else(|| resource.physical_resource_id.clone());
    let members = format!(
        "{}{}{}{}",
        super::xml_leaf("Name", &name),
        config_element(&resource.properties, resource, "ConnectionFunctionConfig")?,
        code_element(&resource.properties, resource, "ConnectionFunctionCode")?,
        tags_element(&resource.properties, propagated),
    );
    let body = request_document("CreateConnectionFunctionRequest", &members);
    let (answered, headers) = super::call_cloudfront_versioned(
        ctx,
        region,
        "POST",
        "/connection-function",
        Some(&body),
        None,
    )
    .await?;
    let attributes = connection_function_attributes(&answered);
    let (Some(arn), Some(id)) = (
        attributes.get("ConnectionFunctionArn").cloned(),
        attributes.get("Id").cloned(),
    ) else {
        return Err(Refusal {
            code: "UnidentifiedFailure".to_string(),
            message: "CreateConnectionFunction answered no ConnectionFunctionSummary.Id"
                .to_string(),
        });
    };
    let published_summary = published(
        ctx,
        region,
        &resource.properties,
        &connection_function_path(&id),
        &headers,
    )
    .await?;
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes: with_published_connection_stage(attributes, published_summary.as_deref()),
    })
}

/// [`with_published_stage`] for a connection function, whose answer carries its own summary
/// element.
fn with_published_connection_stage(
    created: BTreeMap<String, String>,
    published: Option<&str>,
) -> BTreeMap<String, String> {
    let Some(published) = published else {
        return created;
    };
    let mut attributes = created;
    for (attribute, value) in connection_function_attributes(published) {
        attributes.insert(attribute, value);
    }
    attributes
}

/// Create the connection group an `AWS::CloudFront::ConnectionGroup` declares.
async fn create_connection_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied(&resource.properties, resource, CONNECTION_GROUP_PROPERTIES)?;
    let name = declared_text(&resource.properties, "Name")
        .unwrap_or_else(|| resource.physical_resource_id.clone());
    let members = format!(
        "{}{}{}",
        super::xml_leaf("Name", &name),
        rendered_properties(
            &resource.properties,
            &["Ipv6Enabled", "AnycastIpListId", "Enabled"]
        ),
        tags_element(&resource.properties, propagated),
    );
    let body = request_document("CreateConnectionGroupRequest", &members);
    let answered = super::call_cloudfront(
        ctx,
        region,
        "POST",
        "/connection-group",
        Some(&body),
        None,
    )
    .await?;
    let attributes = connection_group_attributes(&answered);
    let id = attributes.get("Id").cloned().ok_or_else(|| Refusal {
        code: "UnidentifiedFailure".to_string(),
        message: "CreateConnectionGroup answered no ConnectionGroup.Id".to_string(),
    })?;
    Ok(Created {
        physical_resource_id: Some(id),
        attributes,
    })
}

/// Ask CloudFront to take the connection group away.
///
/// "The specified CloudFront resource hasn't been disabled yet" is what the delete answers
/// while the group is still enabled, so a group CloudFront still has enabled is disabled with
/// its own update first — the pattern a distribution's deletion follows, and the one thing
/// standing between a stack's teardown and a group nothing can remove.
async fn remove_connection_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    id: &str,
) -> Result<(), Refusal> {
    let path = connection_group_path(id);
    let Some(mut version) = version_of(ctx, region, &path).await? else {
        return Ok(());
    };
    let body = request_document(
        "UpdateConnectionGroupRequest",
        &super::xml_leaf("Enabled", "false"),
    );
    match super::call_cloudfront_versioned(
        ctx,
        region,
        "PUT",
        &path,
        Some(&body),
        version.as_deref(),
    )
    .await
    {
        Ok((_, headers)) => version = headers.get(super::ETAG_HEADER).cloned(),
        Err(refusal) if refusal.code == ENTITY_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    }
    match super::call_cloudfront(ctx, region, "DELETE", &path, None, version.as_deref()).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == ENTITY_NOT_FOUND => Ok(()),
        // The group is still enabled at CloudFront although the update above was accepted,
        // which is that service's own account of the resource rather than this walk's: it is
        // reported as the refusal of the delete.
        Err(refusal) if refusal.code == RESOURCE_NOT_DISABLED => Err(refusal),
        Err(refusal) => Err(refusal),
    }
}

/// The CONFIGURATION element a create or an update of a function sends: the declared
/// configuration under the property the type publishes it as, whose members the API spells
/// identically (`FunctionConfig` for a function, `ConnectionFunctionConfig` for a connection
/// function — both the model's own `FunctionConfig` shape).
fn config_element(
    properties: &Value,
    resource: &StoredResource,
    property: &str,
) -> Result<String, Refusal> {
    let declared = properties
        .get(property)
        .and_then(Value::as_object)
        .ok_or_else(|| missing(property, resource))?;
    super::refuse_unapplied_members(declared, property, FUNCTION_CONFIG_MEMBERS)?;
    super::refuse_unrenderable_members(properties, property, 0)?;
    let mut rendered = String::new();
    for member in FUNCTION_CONFIG_MEMBERS {
        match declared.get(*member).filter(|value| !value.is_null()) {
            Some(value) => {
                rendered.push_str(&super::rendered_member(member, value, declared, property))
            }
            // `FunctionConfig$Comment` is required by the API and optional in a template, so a
            // declaration that states none sends the empty comment rather than leaving the
            // member out of a document the service would refuse.
            None if *member == "Comment" => {
                rendered.push_str(&super::xml_leaf("Comment", ""));
            }
            None => {}
        }
    }
    Ok(format!("<{property}>{rendered}</{property}>"))
}

/// The CODE element a create or an update sends: the function's SOURCE as the declaration
/// states it, base64-encoded.
///
/// The member is a blob (`FunctionBlob`), and a blob travels base64-encoded on the restXml
/// wire, while a template states the function's source as text — so the encoding is
/// CloudFormation's to apply, by the same encoder the runtime decodes it with.
fn code_element(
    properties: &Value,
    resource: &StoredResource,
    property: &str,
) -> Result<String, Refusal> {
    let code = declared_text(properties, property).ok_or_else(|| missing(property, resource))?;
    Ok(super::xml_leaf(
        property,
        &::aws_smithy_types::base64::encode(code.as_bytes()),
    ))
}

/// Publish the function to the LIVE stage where the declaration asks for it, and report what
/// that publish answered.
///
/// `AutoPublish` is "a flag that determines whether to automatically publish the function to
/// the LIVE stage": a declaration that does not state it asks for no publish, and the publish
/// is conditional on the version the call that just wrote the function answered — which is the
/// version that call reported in its `ETag`. `path` is the function's own resource path, which
/// the publish hangs off for either kind of function.
///
/// What it answers is the summary of the stage it WROTE, so a function the stack published
/// reports the stage it is in now rather than the one its create left it in.
async fn published(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
    path: &str,
    headers: &BTreeMap<String, String>,
) -> Result<Option<String>, Refusal> {
    let publish = declared_text(properties, "AutoPublish")
        .is_some_and(|flag| flag.trim().eq_ignore_ascii_case("true"));
    if !publish {
        return Ok(None);
    }
    let version = headers.get(super::ETAG_HEADER).cloned();
    super::call_cloudfront(
        ctx,
        region,
        "POST",
        &format!("{path}/publish"),
        None,
        version.as_deref(),
    )
    .await
    .map(Some)
}

/// The attributes a `FunctionSummary` answer reports for the resource: the ARN CloudFront
/// built for the function and the stage it is in, which `AWS::CloudFront::Function` publishes
/// as `FunctionARN`, `FunctionMetadata.FunctionARN` and `Stage` — the metadata's spelling
/// beside the function's own, since the type publishes the ARN under both.
fn function_attributes(answered: &str) -> BTreeMap<String, String> {
    reported_attributes(
        answered,
        "FunctionMetadata",
        &[
            ("FunctionARN", "FunctionARN"),
            ("FunctionMetadata.FunctionARN", "FunctionARN"),
            ("Stage", "Stage"),
        ],
    )
}

/// The attributes a `KeyValueStore` answer reports for the resource: the ARN that identifies
/// it, the id inside that ARN, and the status CloudFront reports the store in.
fn key_value_store_attributes(answered: &str) -> BTreeMap<String, String> {
    // `KeyValueStore$ARN` is the member CloudFront's model spells the ARN with; `Arn` is what
    // the resource type publishes it as.
    reported_attributes(
        answered,
        "KeyValueStore",
        &[("Arn", "ARN"), ("Id", "Id"), ("Status", "Status")],
    )
}

/// The attributes a `ConnectionGroup` answer reports for the resource.
fn connection_group_attributes(answered: &str) -> BTreeMap<String, String> {
    reported_attributes(
        answered,
        "ConnectionGroup",
        &[
            ("Arn", "Arn"),
            ("Id", "Id"),
            ("RoutingEndpoint", "RoutingEndpoint"),
            ("Status", "Status"),
            ("CreatedTime", "CreatedTime"),
            ("LastModifiedTime", "LastModifiedTime"),
            ("IsDefault", "IsDefault"),
        ],
    )
}

/// The attributes a `ConnectionFunctionSummary` answer reports for the resource.
fn connection_function_attributes(answered: &str) -> BTreeMap<String, String> {
    reported_attributes(
        answered,
        "ConnectionFunctionSummary",
        &[
            ("ConnectionFunctionArn", "ConnectionFunctionArn"),
            ("Id", "Id"),
            ("Status", "Status"),
            ("Stage", "Stage"),
            ("CreatedTime", "CreatedTime"),
            ("LastModifiedTime", "LastModifiedTime"),
        ],
    )
}

/// The attributes one answer reports, as `(attribute the type publishes, member the model
/// spells it with)`: a member the answer does not carry, or carries empty, contributes no
/// attribute rather than an empty one.
fn reported_attributes(
    answered: &str,
    element: &str,
    names: &[(&str, &str)],
) -> BTreeMap<String, String> {
    let reported = super::xml_scalar_children(answered, element);
    let mut attributes = BTreeMap::new();
    for (attribute, member) in names {
        if let Some(value) = reported
            .get(*member)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            attributes.insert(
                (*attribute).to_string(),
                super::published_attribute(attribute, value),
            );
        }
    }
    attributes
}

/// The id of the connection function a stack holds: the one its own create answered, which
/// travels on the record as the attribute the type publishes beside the ARN its identity is.
///
/// Read from the record rather than off the ARN's text, so the identifier the calls take is
/// the one that service reported. A record carrying neither is one no create answered for.
fn function_id(resource: &StoredResource) -> Result<String, Refusal> {
    if let Some(id) = resource.attributes.get("Id") {
        return Ok(id.clone());
    }
    Err(Refusal {
        code: super::MISSING_PROPERTY.to_string(),
        message: format!(
            "the connection function {} reports no Id to address it by",
            resource.logical_id
        ),
    })
}

/// The declared properties `members` names, rendered as the request members they are.
fn rendered_properties(properties: &Value, members: &[&str]) -> String {
    let Some(declared) = properties.as_object() else {
        return String::new();
    };
    let empty = Map::new();
    members
        .iter()
        .filter_map(|member| {
            let value = declared.get(*member).filter(|value| !value.is_null())?;
            Some(super::rendered_member(member, value, &empty, member))
        })
        .collect()
}

/// The `Tags` element a create carries: the tags the declaration asks for and the ones
/// CloudFormation propagates to the resource, in the container the model wraps a tag list in.
fn tags_element(properties: &Value, propagated: &[(String, String)]) -> String {
    let tags = super::tags_of(properties, propagated);
    if tags.is_empty() {
        return String::new();
    }
    let items: String = tags
        .iter()
        .map(|(key, value)| {
            format!(
                "<Tag>{}{}</Tag>",
                super::xml_leaf("Key", key),
                super::xml_leaf("Value", value)
            )
        })
        .collect();
    format!("<Tags><Items>{items}</Items></Tags>")
}

/// One request document: the operation's own request shape as the root element, under this
/// service's XML namespace.
fn request_document(root: &str, members: &str) -> String {
    let namespace = super::CLOUDFRONT_XML_NAMESPACE;
    format!("<{root} xmlns=\"{namespace}\">{members}</{root}>")
}

/// The refusal for a declared property no call here applies, naming which property it was.
fn refuse_unapplied(
    properties: &Value,
    resource: &StoredResource,
    published: &[&str],
) -> Result<(), Refusal> {
    super::refuse_unapplied_properties(properties, &resource.resource_type, published)?;
    super::refuse_unrenderable_members(properties, &resource.resource_type, 0)
}

/// A declared property's value as the text the request carries, or `None` where the
/// declaration states none.
fn declared_text(properties: &Value, member: &str) -> Option<String> {
    properties
        .get(member)
        .filter(|value| !value.is_null())
        .and_then(super::scalar_text)
}

/// [`declared_text`] where the API requires the property, which a declaration stating none is
/// refused for.
fn required(
    properties: &Value,
    member: &str,
    resource: &StoredResource,
) -> Result<String, Refusal> {
    declared_text(properties, member).ok_or_else(|| missing(member, resource))
}

/// The refusal for a declaration that states none of a required property.
fn missing(member: &str, resource: &StoredResource) -> Refusal {
    Refusal {
        code: super::MISSING_PROPERTY.to_string(),
        message: format!(
            "Property {member} cannot be empty for resource of type {}",
            resource.resource_type
        ),
    }
}

/// The version CloudFront reports a resource under, or `None` where that service no longer
/// holds it — which is what says a delete has nothing left to remove.
async fn version_of(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    path: &str,
) -> Result<Option<Option<String>>, Refusal> {
    match super::read_version(ctx, region, path).await {
        Ok((_, version)) => Ok(Some(version)),
        Err(refusal) if is_gone(&refusal) => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// One delete's answer as this module reports it: a resource CloudFront no longer holds is
/// already in the state the delete asks for.
fn removed(answered: Result<String, Refusal>) -> Result<(), Refusal> {
    match answered {
        Ok(_) => Ok(()),
        Err(refusal) if is_gone(&refusal) => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether a refusal says CloudFront does not hold the resource, judged on the CODE and never
/// on a message: the newer surfaces here answer `EntityNotFound` and the function surface
/// words the same thing as `NoSuchFunctionExists`.
fn is_gone(refusal: &Refusal) -> bool {
    refusal.code == ENTITY_NOT_FOUND || refusal.code == NO_SUCH_FUNCTION_EXISTS
}

/// The URI each per-resource operation is bound to, with the identifier written as ONE path
/// segment so one outside the modelled form is refused by that operation's validation instead
/// of addressing a different resource.
fn key_value_store_path(name: &str) -> String {
    format!("/key-value-store/{}", super::percent_encoded(name))
}

fn connection_function_path(id: &str) -> String {
    format!("/connection-function/{}", super::percent_encoded(id))
}

/// A function is addressed by the NAME it was created under
/// (`PUT /2020-05-31/function/{Name}`).
fn function_path(name: &str) -> String {
    format!("/function/{}", super::percent_encoded(name))
}

/// `DescribeConnectionFunction` is the read that reports a connection function's VERSION,
/// which every mutation of one is conditional on.
fn connection_function_describe_path(id: &str) -> String {
    format!("{}/describe", connection_function_path(id))
}

/// `DescribeFunction` is the same read for a function.
fn function_describe_path(name: &str) -> String {
    format!("{}/describe", function_path(name))
}

fn connection_group_path(id: &str) -> String {
    format!("/connection-group/{}", super::percent_encoded(id))
}
