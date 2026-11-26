// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon AppFlow's own control plane: the connector profile
//! an `AWS::AppFlow::ConnectorProfile` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! A connector profile is "an instance of a connector. This includes the provided name,
//! credentials ARN, connection-mode, and so on", and it is NAMED: `CreateConnectorProfile`
//! takes a `connectorProfileName` whose "name is unique for each `ConnectorProfile` in the
//! Amazon Web Services account", and that name is the handle every later call on the profile
//! takes — `UpdateConnectorProfile`, `DeleteConnectorProfile` and the
//! `DescribeConnectorProfiles` filter each address one by it. So the type's `Ref` returns the
//! name the conventional `<last segment>Name` reading of [`super::template`]'s `physical_name`
//! already finds, and the profile is created in the owning service under exactly it.
//!
//! What the type PUBLISHES beyond that name is Amazon AppFlow's own: the ARN it minted for the
//! profile and the handle it holds the profile's credentials under. Both are read off that
//! service's own report of the profile ([`reported_connector_profile_attributes`]) rather than
//! recomputed here, which is why the stand-ins [`super::template`]'s return values compute for
//! them are re-pointed by the create.
//!
//! A connector profile is finished when its create answers: `CreateConnectorProfile` answers
//! the ARN of a profile that already exists, and that service reports no provisioning status
//! for one, so the resource needs no readiness wait.

use super::*;

/// Amazon AppFlow's signing name, from its model (`smithy_query(query="service_info")`:
/// `sigv4Name: appflow`), which speaks `restJson1` — so its calls ride on
/// [`call_rest_json_named`], each on the URI its own operation's `@http` binding declares,
/// naming the operation so a refusal is reported the way that service's own SDK raises it.
const APPFLOW_SIGNING_NAME: &str = "appflow";

/// The `AWS::<Namespace>::<Resource>` type of an Amazon AppFlow CONNECTOR PROFILE, as the "AWS
/// resource and property types reference" publishes it.
pub(crate) const APPFLOW_CONNECTOR_PROFILE: &str = "AWS::AppFlow::ConnectorProfile";

/// The operations this module calls, as Amazon AppFlow's own model names them, with the URI
/// each is bound to (`POST` for every one of them).
const CREATE_CONNECTOR_PROFILE: &str = "CreateConnectorProfile";
const CREATE_CONNECTOR_PROFILE_URI: &str = "/create-connector-profile";
const UPDATE_CONNECTOR_PROFILE: &str = "UpdateConnectorProfile";
const UPDATE_CONNECTOR_PROFILE_URI: &str = "/update-connector-profile";
const DELETE_CONNECTOR_PROFILE: &str = "DeleteConnectorProfile";
const DELETE_CONNECTOR_PROFILE_URI: &str = "/delete-connector-profile";
const DESCRIBE_CONNECTOR_PROFILES: &str = "DescribeConnectorProfiles";
const DESCRIBE_CONNECTOR_PROFILES_URI: &str = "/describe-connector-profiles";

/// The error code Amazon AppFlow answers for a connector profile it does not hold — the
/// modelled 404 of `UpdateConnectorProfile` and `DeleteConnectorProfile`
/// (`smithy_query(query="operation", operation="UpdateConnectorProfile,DeleteConnectorProfile")`).
/// Judged on the CODE and never on a message, which carries the request's own name.
const APPFLOW_NOT_FOUND: &str = "ResourceNotFoundException";

/// The members a call of that service carries the profile's identity and configuration in, as
/// its own model names them.
const CONNECTOR_PROFILE_NAME_MEMBER: &str = "connectorProfileName";
const CONNECTOR_PROFILE_NAMES_MEMBER: &str = "connectorProfileNames";
const CONNECTOR_PROFILE_DETAILS_MEMBER: &str = "connectorProfileDetails";
const CONNECTOR_PROFILE_CONFIG_MEMBER: &str = "connectorProfileConfig";

/// The TEXT properties of [`APPFLOW_CONNECTOR_PROFILE`] this arm carries onto
/// `CreateConnectorProfile`, with the member each travels as — the type's property names being
/// its members' own, spelled with an upper-case initial, with `KMSArn` the one whose published
/// spelling is not the member's initial-capped name.
const CONNECTOR_PROFILE_TEXT_MEMBERS: &[(&str, &str)] = &[
    ("ConnectionMode", "connectionMode"),
    ("ConnectorLabel", "connectorLabel"),
    ("ConnectorType", "connectorType"),
    ("KMSArn", "kmsArn"),
];

/// The members `UpdateConnectorProfile` takes of those: that call carries the connection mode
/// and the whole configuration, addressed by the name the profile was created under
/// (`smithy_query(query="operation", operation="UpdateConnectorProfile")`). The connector type,
/// its label and the encryption key are members of the CREATE alone, which is why a declaration
/// restating one replaces the resource ([`super::template::replacement_properties`]) instead of
/// reaching the update.
const CONNECTOR_PROFILE_UPDATABLE_TEXT_MEMBERS: &[(&str, &str)] =
    &[("ConnectionMode", "connectionMode")];

/// Every property of the type this arm carries, so a declaration stating one outside the set is
/// REFUSED rather than reported as created without it ([`refuse_unmapped_properties`]).
///
/// The type publishes no `Tags` property at all — Amazon AppFlow holds a connector profile's
/// tags through its own `TagResource` and `CreateConnectorProfile` takes none — so no tags are
/// propagated to a profile a stack creates.
const CONNECTOR_PROFILE_MAPPED_PROPERTIES: &[&str] = &[
    "ConnectionMode",
    "ConnectorLabel",
    "ConnectorProfileConfig",
    "ConnectorProfileName",
    "ConnectorType",
    "KMSArn",
];

/// Every `Fn::GetAtt` attribute an [`APPFLOW_CONNECTOR_PROFILE`] publishes, with the member of
/// that service's own `ConnectorProfile` each is read off
/// (`smithy_query(query="shape", shape="ConnectorProfile")`).
///
/// Source: the type's own "Return values" section — the ARN Amazon AppFlow minted for the
/// profile and the ARN of the credentials it holds for it. Both are that service's to produce,
/// so each is a STAND-IN until [`create_appflow_connector_profile`] has reported what that
/// service really answered.
const CONNECTOR_PROFILE_ATTRIBUTES: &[(&str, &str)] = &[
    ("ConnectorProfileArn", "connectorProfileArn"),
    ("CredentialsArn", "credentialsArn"),
];

/// The attribute NAMES the type publishes, which is the other half of the catalogue above —
/// what the stand-in table reads ([`super::template::published_attribute_names`]).
///
/// Stated in the SORTED order a resource's own attribute map holds them in, because that is the
/// order the stand-in table computes them in. Held to the catalogue above by
/// `the_published_names_are_the_catalogued_attributes`, so the two halves cannot drift apart.
pub(crate) const CONNECTOR_PROFILE_ATTRIBUTE_NAMES: &[&str] =
    &["ConnectorProfileArn", "CredentialsArn"];

/// The scalar properties a read of the profile reports back under the names the type declares,
/// for drift detection: the name it is held under, the connector it is an instance of, that
/// connector's label and the connection mode its flows reach the application over. A property
/// that is a STRUCTURE on the wire (the configuration) is left out, which is the position every
/// arm whose property is not a scalar is in.
const CONNECTOR_PROFILE_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("ConnectionMode", "connectionMode"),
    ("ConnectorLabel", "connectorLabel"),
    ("ConnectorProfileName", CONNECTOR_PROFILE_NAME_MEMBER),
    ("ConnectorType", "connectorType"),
];

/// Create the connector profile an `AWS::AppFlow::ConnectorProfile` declares, in Amazon AppFlow
/// — so a client that deployed the stack reads it back through that service's own
/// `DescribeConnectorProfiles` under the name the stack published, and can define a flow over
/// it rather than getting a `ResourceNotFoundException` for a profile nothing created.
///
/// The declared properties ARE `CreateConnectorProfile`'s members, each spelled with the initial
/// its own member takes ([`CONNECTOR_PROFILE_TEXT_MEMBERS`],
/// [`connector_profile_config_request`]), with the name the naming rules settled pushed as
/// `connectorProfileName` — this resource's physical id, which is what the type's `Ref` returns.
///
/// What the resource PUBLISHES is read off that service's own report of the profile: the create
/// answers only the ARN, while the type also publishes the handle AppFlow holds the profile's
/// credentials under, so both come from one read of the profile the create has just stored.
pub(super) async fn create_appflow_connector_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, CONNECTOR_PROFILE_MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, CONNECTOR_PROFILE_TEXT_MEMBERS, &mut request);
    push_connector_profile_config(&properties, &mut request)?;
    request.insert(
        CONNECTOR_PROFILE_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    call_appflow(
        ctx,
        region,
        CREATE_CONNECTOR_PROFILE,
        CREATE_CONNECTOR_PROFILE_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let reported = read_connector_profile(ctx, region, &resource.physical_resource_id).await?;
    Ok(Created::attributes(
        reported_connector_profile_attributes(reported.as_ref()),
    ))
}

/// Apply a CHANGED declaration of the type to the connector profile the stack already has,
/// through `UpdateConnectorProfile` — the one call that re-configures a profile that exists.
///
/// It takes the connection mode and the whole `connectorProfileConfig`, addressed by the name
/// the profile was created under; every other property is fixed when the profile is made, so a
/// declaration restating one replaces the resource rather than reaching here.
pub(super) async fn update_appflow_connector_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared.as_object().cloned().unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(
        &properties,
        CONNECTOR_PROFILE_UPDATABLE_TEXT_MEMBERS,
        &mut request,
    );
    push_connector_profile_config(&properties, &mut request)?;
    request.insert(
        CONNECTOR_PROFILE_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    call_appflow(
        ctx,
        region,
        UPDATE_CONNECTOR_PROFILE,
        UPDATE_CONNECTOR_PROFILE_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    // What the resource now reports is read off that service's own report of the profile, the
    // one reading the create uses — so the two cannot differ by which call last looked at it.
    let reported = read_connector_profile(ctx, region, &resource.physical_resource_id).await?;
    Ok(Created::attributes(
        reported_connector_profile_attributes(reported.as_ref()),
    ))
}

/// Delete the connector profile a stack created, by the name it was created under — this
/// resource's physical id. One Amazon AppFlow no longer holds ([`APPFLOW_NOT_FOUND`], judged on
/// the CODE) is already in the state the delete asks for.
///
/// `forceDelete` is not sent: it "[i]ndicates whether Amazon AppFlow should delete the profile,
/// even if it is currently in use in one or more flows", and a flow that uses the profile is a
/// resource that DEPENDS on it, which CloudFormation tears down first — so the unforced delete
/// is the one this stack asks for, and a profile some other stack's flow still uses is refused
/// rather than silently breaking that flow.
pub(super) async fn delete_appflow_connector_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_appflow(
        ctx,
        region,
        DELETE_CONNECTOR_PROFILE,
        DELETE_CONNECTOR_PROFILE_URI,
        Some(&serde_json::json!({
            CONNECTOR_PROFILE_NAME_MEMBER: physical_resource_id,
        })),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == APPFLOW_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The profile's configuration as Amazon AppFlow reports it, for drift detection: that service's
/// own read of the profile, by the name the stack published. One it no longer holds was deleted.
pub(super) async fn describe_appflow_connector_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_connector_profile(ctx, region, physical_resource_id)
        .await?
        .map(|profile| {
            reported_as_properties(&profile, CONNECTOR_PROFILE_REPORTED_PROPERTIES)
        }))
}

/// The connector profile record that service holds under `name`, or `None` when it holds none.
///
/// Amazon AppFlow publishes no single-profile read: `DescribeConnectorProfiles` "[r]eturns a
/// list of connector-profile details matching the provided connector-profile names … If there
/// is no match, this operation returns an empty list", so the read is that listing filtered to
/// the one name and an empty answer is the profile being absent.
async fn read_connector_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    let answered = call_appflow(
        ctx,
        region,
        DESCRIBE_CONNECTOR_PROFILES,
        DESCRIBE_CONNECTOR_PROFILES_URI,
        Some(&serde_json::json!({
            CONNECTOR_PROFILE_NAMES_MEMBER: [name],
        })),
    )
    .await?;
    Ok(answered
        .get(CONNECTOR_PROFILE_DETAILS_MEMBER)
        .and_then(Value::as_array)
        .and_then(|details| details.first())
        .cloned())
}

/// The attributes one reported profile gives the resource, read the one way every arm reads a
/// reported attribute ([`reported_attributes`]). A read that answered no profile gives none, so
/// the resource keeps what the naming rules computed.
fn reported_connector_profile_attributes(profile: Option<&Value>) -> BTreeMap<String, String> {
    profile
        .map(|profile| reported_attributes(profile, CONNECTOR_PROFILE_ATTRIBUTES))
        .unwrap_or_default()
}

/// Put the declared `ConnectorProfileConfig` onto `request` as the member that call takes, when
/// the declaration states one.
fn push_connector_profile_config(
    properties: &Map<String, Value>,
    request: &mut Map<String, Value>,
) -> Result<(), Refusal> {
    if let Some(declared) = properties.get("ConnectorProfileConfig") {
        request.insert(
            CONNECTOR_PROFILE_CONFIG_MEMBER.to_string(),
            connector_profile_config_request(declared)?,
        );
    }
    Ok(())
}

/// One declared `ConnectorProfileConfig` as the member `CreateConnectorProfile` and
/// `UpdateConnectorProfile` take.
///
/// The usual re-casing ([`camel_cased`]) does not serve this property on its own, because the
/// document has THREE kinds of key in it:
///
/// 1. the two SECTIONS of the configuration (`ConnectorProfileProperties`,
///    `ConnectorProfileCredentials`), which are members of `ConnectorProfileConfig` and so
///    travel with a lower-case initial;
/// 2. inside each section, one key per CONNECTOR (`Redshift`, `Salesforce`, `CustomConnector`),
///    which `ConnectorProfileProperties` and `ConnectorProfileCredentials` spell with an
///    UPPER-case initial on the wire as well (`smithy_query(query="shape",
///    shape="ConnectorProfileProperties,ConnectorProfileCredentials")`) — so those keys are
///    carried through verbatim, where lower-casing them would reach members that do not exist;
/// 3. inside one connector's settings, that connector's own members (`BucketName`, `RoleArn`,
///    `DataApiRoleArn`), which travel with a lower-case initial like every other member.
fn connector_profile_config_request(declared: &Value) -> Result<Value, Refusal> {
    let Some(sections) = declared.as_object() else {
        return Ok(declared.clone());
    };
    let mut config = Map::new();
    for (section, connectors) in sections {
        let Some(connectors) = connectors.as_object() else {
            // A section that is not a document at all is carried as it came: the owning
            // operation's own validation is what refuses it, rather than this dropping it.
            config.insert(lower_initial(section), connectors.clone());
            continue;
        };
        let mut converted = Map::new();
        for (connector, settings) in connectors {
            converted.insert(connector.clone(), camel_cased(settings, 0)?);
        }
        config.insert(lower_initial(section), Value::Object(converted));
    }
    Ok(Value::Object(config))
}

/// Call one Amazon AppFlow operation in process, on the REST-JSON wire that service publishes.
async fn call_appflow(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json_named(
        ctx,
        APPFLOW_SIGNING_NAME,
        region,
        operation,
        "POST",
        uri,
        body,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published attribute NAMES of the type are exactly the attributes the catalogue above
    /// reads off Amazon AppFlow's own record of a profile, in the SORTED order the stand-in
    /// table computes them in.
    ///
    /// The two halves are read by different callers — the names by the stand-in table, the
    /// pairs by the arms here — so a name missing from one half or invented in the other would
    /// make what a stack publishes for a resource disagree with what the type publishes.
    #[test]
    fn the_published_names_are_the_catalogued_attributes() {
        let catalogued: ::std::collections::BTreeSet<&str> = CONNECTOR_PROFILE_ATTRIBUTES
            .iter()
            .map(|(attribute, _)| *attribute)
            .collect();
        let expected: Vec<&str> = catalogued.into_iter().collect();
        assert_eq!(
            CONNECTOR_PROFILE_ATTRIBUTE_NAMES.to_vec(),
            expected,
            "case {APPFLOW_CONNECTOR_PROFILE:?}: expected the published names to be the \
             catalogued attributes in sorted order, got {CONNECTOR_PROFILE_ATTRIBUTE_NAMES:?}"
        );
    }

    /// The declared configuration reaches the owning API with each of its three kinds of key in
    /// the spelling that key takes on the wire: the sections lower-initialled, the connector
    /// keys verbatim and one connector's own settings lower-initialled.
    #[test]
    fn the_configuration_keeps_the_connector_keys_and_re_cases_the_rest() {
        let declared = serde_json::json!({
            "ConnectorProfileProperties": {
                "Redshift": {
                    "BucketName": "records",
                    "RoleArn": "arn:aws:iam::000000000000:role/redshift",
                    "DataApiRoleArn": "arn:aws:iam::000000000000:role/data-api",
                    "IsRedshiftServerless": true,
                    "WorkgroupName": "analytics",
                    "DatabaseName": "dev",
                },
            },
            "ConnectorProfileCredentials": {
                "Redshift": { "Username": "admin", "Password": "secret" },
            },
        });
        let expected = serde_json::json!({
            "connectorProfileProperties": {
                "Redshift": {
                    "bucketName": "records",
                    "roleArn": "arn:aws:iam::000000000000:role/redshift",
                    "dataApiRoleArn": "arn:aws:iam::000000000000:role/data-api",
                    "isRedshiftServerless": true,
                    "workgroupName": "analytics",
                    "databaseName": "dev",
                },
            },
            "connectorProfileCredentials": {
                "Redshift": { "username": "admin", "password": "secret" },
            },
        });
        let converted = connector_profile_config_request(&declared)
            .expect("the declared configuration is shallow enough to convert");
        assert_eq!(
            converted, expected,
            "case {APPFLOW_CONNECTOR_PROFILE:?}: expected the request members, got {converted:?}"
        );
    }
}
