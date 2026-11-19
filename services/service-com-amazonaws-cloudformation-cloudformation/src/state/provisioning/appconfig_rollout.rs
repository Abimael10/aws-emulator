// SPDX-License-Identifier: CC0-1.0

//! `AWS::AppConfig::HostedConfigurationVersion` and `AWS::AppConfig::Deployment`: the
//! configuration CONTENT a stack hosts in AWS AppConfig and the rollout of one version of it
//! into an environment, created in that service itself.
//!
//! This is the per-type half of provisioning for two resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives in
//! its own module beside [`super::appconfig_resources`] — which owns the four types of the
//! estate those two are declared INSIDE — for the reason that module records, and is reached
//! through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! A configuration version is what a deployment deploys: `StartDeployment$ConfigurationVersion`
//! is "the configuration version to deploy", and AppConfig refuses a version its profile holds
//! nothing under. The version NUMBER is AppConfig's own to mint —
//! `CreateHostedConfigurationVersion` takes the content and no number and answers the
//! `VersionNumber` it assigned, incrementing it per version of the profile
//! (`smithy_query(query="operation",operation="CreateHostedConfigurationVersion")` on that
//! service's model) — so a stack that reported a hosted version created while nothing created
//! it publishes a number no `GetHostedConfigurationVersion` resolves, and the deployment the
//! next resource of the same template declares is started against a version that does not
//! exist. A DEPLOYMENT is the same case one level on: `StartDeployment` takes no identifier and
//! answers the `DeploymentNumber` it allocated, and every later call on the deployment
//! (`GetDeployment`, `StopDeployment`) addresses it by the application, the environment and
//! that number. Both are therefore provisioned here rather than left declared-only, and the
//! identifier each create answers becomes part of the resource's physical id.
//!
//! ## What each type publishes
//!
//! Each type's primary identifier is the TRIPLE its own API addresses one resource by — the
//! application, the profile or the environment, and the number that service minted — so the
//! physical id a `Ref` resolves to is that triple joined the way CloudFormation joins the parts
//! of a composite identifier ([`super::super::template::appconfig_rollout_identity`]), and
//! `Fn::GetAtt` reads the part only that service can produce: a hosted version's
//! `VersionNumber`, and a deployment's `DeploymentNumber` beside the `State` AppConfig reports
//! the rollout in.
//!
//! A declared property none of the calls here carries is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    applicable_tags, call_rest_json, call_rest_payload_reporting, percent_encoded, Created,
    Readiness, Refusal, Removal,
};
use crate::state::provisioner::{attribute_text, tags_as_map, tags_of};
use crate::state::template::{
    appconfig_rollout_identity, appconfig_rollout_identity_parts, APPCONFIG_DEPLOYMENT,
    APPCONFIG_HOSTED_CONFIGURATION_VERSION,
};
use crate::state::StoredResource;

/// The SigV4 signing name of the service that owns these two types, which is what routes the
/// in-process call to AWS AppConfig's own handlers (that model's `sigv4Name`/`endpointPrefix`,
/// `smithy_query(query="service_info")`).
const APPCONFIG_SIGNING_NAME: &str = "appconfig";

/// The error code AppConfig answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled refusal of its reads, writes and deletes), which
/// is what says a delete has nothing left to remove and a read has nothing to report.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the same
/// one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property, or one
/// whose recorded identity is not the triple its type is addressed by.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// The code a create that answered 2xx without the number AppConfig assigns is reported under.
const UNIDENTIFIED_FAILURE: &str = "InternalFailure";

/// Which of the two types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::AppConfig::HostedConfigurationVersion`.
    HostedConfigurationVersion,
    /// `AWS::AppConfig::Deployment`.
    Deployment,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        APPCONFIG_HOSTED_CONFIGURATION_VERSION => Some(Kind::HostedConfigurationVersion),
        APPCONFIG_DEPLOYMENT => Some(Kind::Deployment),
        _ => None,
    }
}

/// The properties naming the estate a resource of either type belongs to, which ADDRESS the
/// call rather than travelling in its body: both are `@httpLabel`s of
/// `CreateHostedConfigurationVersion` and of `StartDeployment` respectively.
const APPLICATION_ID: &str = "ApplicationId";
const CONFIGURATION_PROFILE_ID: &str = "ConfigurationProfileId";
const ENVIRONMENT_ID: &str = "EnvironmentId";

/// The property carrying a resource's tags, which reaches the create that takes them as a map.
const TAGS: &str = "Tags";

/// The attribute a hosted configuration version publishes: the version number AppConfig minted.
const VERSION_NUMBER: &str = "VersionNumber";

/// The attributes a deployment publishes: the number AppConfig allocated for it, and the state
/// that service reports the rollout in.
const DEPLOYMENT_NUMBER: &str = "DeploymentNumber";
const DEPLOYMENT_STATE: &str = "State";

/// Every property `AWS::AppConfig::HostedConfigurationVersion` publishes.
///
/// Each is a member of `CreateHostedConfigurationVersion` under the same name, which is how that
/// model spells them: the two ids that address the call, the `Content` it carries as its
/// `@httpPayload`, and the four it carries as headers.
const HOSTED_CONFIGURATION_VERSION_PROPERTIES: &[&str] = &[
    APPLICATION_ID,
    CONFIGURATION_PROFILE_ID,
    "Content",
    "ContentType",
    "Description",
    "LatestVersionNumber",
    "VersionLabel",
];

/// Every property `AWS::AppConfig::Deployment` publishes, each a member of `StartDeployment`
/// under the same name.
const DEPLOYMENT_PROPERTIES: &[&str] = &[
    APPLICATION_ID,
    CONFIGURATION_PROFILE_ID,
    "ConfigurationVersion",
    "DeploymentStrategyId",
    "Description",
    "DynamicExtensionParameters",
    ENVIRONMENT_ID,
    "KmsKeyIdentifier",
    TAGS,
];

/// The members of `StartDeployment` that travel in its BODY as text, under the names the type
/// spells its properties with.
const DEPLOYMENT_TEXT_MEMBERS: &[&str] = &[
    CONFIGURATION_PROFILE_ID,
    "ConfigurationVersion",
    "DeploymentStrategyId",
    "Description",
    "KmsKeyIdentifier",
];

/// Create the resource one of these two types declares, in AWS AppConfig itself — so the
/// version a stack publishes is one `GetHostedConfigurationVersion` resolves and the deployment
/// it reports is one `GetDeployment` answers for.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    match kind {
        // A hosted configuration version is not taggable: AppConfig publishes no call that
        // carries a tag to one and the type declares no `Tags` property at all, so
        // CloudFormation propagates none to it.
        Kind::HostedConfigurationVersion => {
            create_hosted_configuration_version(ctx, region, resource).await
        }
        Kind::Deployment => create_deployment(ctx, region, resource, propagated).await,
    }
}

/// Create the hosted configuration version an `AWS::AppConfig::HostedConfigurationVersion`
/// declares, under the application and the configuration profile its ids name.
///
/// The create's input is an `@httpPayload`: the `Content` the declaration states IS the request
/// body, under the `Content-Type` header the type's own `ContentType` property carries, with
/// `Description`, `LatestVersionNumber` and `VersionLabel` as headers beside it. What comes back
/// is read off the ANSWER'S headers — `Version-Number`, `Application-Id` and
/// `Configuration-Profile-Id` — because that operation's output binds every member it reports
/// that way and carries the content itself as its payload.
async fn create_hosted_configuration_version(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        resource,
        HOSTED_CONFIGURATION_VERSION_PROPERTIES,
    )?;
    let application_id = required(resource, &resource.properties, APPLICATION_ID)?;
    let profile_id = required(resource, &resource.properties, CONFIGURATION_PROFILE_ID)?;
    let content = required(resource, &resource.properties, "Content")?;
    let content_type = required(resource, &resource.properties, "ContentType")?;
    let mut sent = vec![(CONTENT_TYPE_HEADER, content_type)];
    for (property, header) in [
        ("Description", DESCRIPTION_HEADER),
        ("LatestVersionNumber", LATEST_VERSION_NUMBER_HEADER),
        ("VersionLabel", VERSION_LABEL_HEADER),
    ] {
        if let Some(value) = stated(&resource.properties, property) {
            sent.push((header, value));
        }
    }
    let answered = call_rest_payload_reporting(
        ctx,
        APPCONFIG_SIGNING_NAME,
        region,
        "POST",
        &hosted_versions_uri(&application_id, &profile_id),
        sent,
        content.into_bytes(),
        &[
            VERSION_NUMBER_HEADER,
            APPLICATION_ID_HEADER,
            CONFIGURATION_PROFILE_ID_HEADER,
        ],
    )
    .await?;
    // The ids the ANSWER carries are the ones AppConfig resolved the request's labels to, which
    // is what the version is addressed by from here on; the declaration's own values stand in
    // where that answer carries none.
    let version = minted(
        &answered,
        VERSION_NUMBER_HEADER,
        "CreateHostedConfigurationVersion",
        VERSION_NUMBER,
    )?;
    let mut attributes = BTreeMap::new();
    attributes.insert(VERSION_NUMBER.to_string(), version.clone());
    Ok(Created {
        physical_resource_id: Some(appconfig_rollout_identity(
            answered
                .get(APPLICATION_ID_HEADER)
                .unwrap_or(&application_id),
            answered
                .get(CONFIGURATION_PROFILE_ID_HEADER)
                .unwrap_or(&profile_id),
            &version,
        )),
        attributes,
    })
}

/// Start the deployment an `AWS::AppConfig::Deployment` declares, into the environment its
/// `EnvironmentId` names.
///
/// `StartDeployment` takes the application and the environment as its URI labels and everything
/// else in its body, and answers the `DeploymentNumber` AppConfig allocated beside the `State`
/// the rollout is in — which is the pair this type publishes.
async fn create_deployment(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, resource, DEPLOYMENT_PROPERTIES)?;
    let application_id = required(resource, &resource.properties, APPLICATION_ID)?;
    let environment_id = required(resource, &resource.properties, ENVIRONMENT_ID)?;
    let mut request = Map::new();
    for property in DEPLOYMENT_TEXT_MEMBERS {
        if let Some(value) = stated(&resource.properties, property) {
            request.insert((*property).to_string(), Value::String(value));
        }
    }
    // Every member the create requires beyond the two ids that address it, so a declaration
    // stating none of one is refused here — naming the property — rather than sent as a request
    // that service would reject for a member CloudFormation could have seen was missing.
    for property in [
        CONFIGURATION_PROFILE_ID,
        "ConfigurationVersion",
        "DeploymentStrategyId",
    ] {
        if !request.contains_key(property) {
            return Err(missing(resource, property));
        }
    }
    // "A map of dynamic extension parameter names to values": a map the declaration states
    // travels as it is written.
    if let Some(parameters) = resource
        .properties
        .get("DynamicExtensionParameters")
        .filter(|value| !value.is_null())
    {
        request.insert(
            "DynamicExtensionParameters".to_string(),
            parameters.clone(),
        );
    }
    let tags = tags_as_map(&tags_of(
        &resource.properties,
        &applicable_tags(propagated),
    ));
    if !tags.is_empty() {
        request.insert(TAGS.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json(
        ctx,
        APPCONFIG_SIGNING_NAME,
        region,
        "POST",
        &deployments_uri(&application_id, &environment_id),
        Some(&Value::Object(request)),
    )
    .await?;
    let number = answered
        .get(DEPLOYMENT_NUMBER)
        .and_then(scalar_text)
        .ok_or_else(|| Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!(
                "AppConfig answered StartDeployment without a {DEPLOYMENT_NUMBER}"
            ),
        })?;
    let mut attributes = BTreeMap::new();
    attributes.insert(DEPLOYMENT_NUMBER.to_string(), number.clone());
    if let Some(state) = answered.get(DEPLOYMENT_STATE).and_then(Value::as_str) {
        attributes.insert(DEPLOYMENT_STATE.to_string(), state.to_string());
    }
    Ok(Created {
        physical_resource_id: Some(appconfig_rollout_identity(
            answered
                .get(APPLICATION_ID)
                .and_then(Value::as_str)
                .unwrap_or(&application_id),
            answered
                .get(ENVIRONMENT_ID)
                .and_then(Value::as_str)
                .unwrap_or(&environment_id),
            &number,
        )),
        attributes,
    })
}

/// Whether AppConfig reports the created resource ready.
///
/// A hosted configuration VERSION exists from the moment its create answers: that answer carries
/// the content every later read reports, a deployment may name the version at once, and the
/// service publishes no status a fresh version settles out of. A DEPLOYMENT does not —
/// `StartDeployment` answers it `DEPLOYING` at 0% of its targets for any strategy that paces a
/// stage, and `Deployment$PercentageComplete` is "the percentage of targets for which the
/// deployment is available" — so the resource's creation is held open until that service reports
/// the rollout in a state it no longer leaves on its own
/// ([`deployment_readiness`]).
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    match kind {
        Kind::HostedConfigurationVersion => Ok(Readiness::Ready),
        Kind::Deployment => deployment_readiness(ctx, region, physical_resource_id).await,
    }
}

/// The `DeploymentState` values AppConfig reports a rollout it is still carrying out in, which
/// are the ones a wait is held on: "VALIDATING", "DEPLOYING" and "BAKING" are the stages of a
/// deployment in flight, and "ROLLING_BACK" the unwinding of one
/// (`smithy_query(query="shape",shape="DeploymentState")`).
const DEPLOYMENT_IN_FLIGHT: &[&str] = &["VALIDATING", "DEPLOYING", "BAKING", "ROLLING_BACK"];

/// The state a deployment that carried its configuration all the way out reaches, which is the
/// one that ENDS the wait: "COMPLETE".
const DEPLOYMENT_COMPLETE: &str = "COMPLETE";

/// Whether AppConfig has finished rolling the declared configuration out, read off
/// `GetDeployment`'s own report of the deployment.
///
/// A deployment that service no longer holds is not one a wait brings up, and one it reports
/// `ROLLED_BACK` or `REVERTED` carried the configuration out and took it away again — neither is
/// a rollout the stack can report as created, so each fails the resource rather than waiting the
/// window out.
async fn deployment_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_deployment(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "AppConfig holds no deployment {physical_resource_id}"
        )));
    };
    let state = reported
        .get(DEPLOYMENT_STATE)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if state == DEPLOYMENT_COMPLETE {
        return Ok(Readiness::Ready);
    }
    if DEPLOYMENT_IN_FLIGHT.contains(&state.as_str()) {
        return Ok(Readiness::Pending(state));
    }
    Ok(Readiness::Failed(format!(
        "AppConfig reports the deployment {physical_resource_id} as {state}"
    )))
}

/// The attributes AppConfig publishes for a resource it now reports up.
///
/// A hosted version's `VersionNumber` came back with its own create, so there is nothing left to
/// read for one. A DEPLOYMENT publishes the `State` of the rollout, which the stages it went
/// through since its create have left behind, so that one is read again — the value a stack
/// reports for it is the state that service reports now, not the one the call that started it
/// answered.
pub(super) async fn ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    if kind == Kind::HostedConfigurationVersion {
        return Ok(BTreeMap::new());
    }
    let Some(reported) = read_deployment(ctx, region, physical_resource_id).await? else {
        return Ok(BTreeMap::new());
    };
    let mut attributes = BTreeMap::new();
    if let Some(state) = reported.get(DEPLOYMENT_STATE).and_then(Value::as_str) {
        attributes.insert(DEPLOYMENT_STATE.to_string(), state.to_string());
    }
    if let Some(number) = reported.get(DEPLOYMENT_NUMBER).and_then(scalar_text) {
        attributes.insert(DEPLOYMENT_NUMBER.to_string(), number);
    }
    Ok(attributes)
}

/// Whether AppConfig has finished removing a deleted resource.
///
/// Nothing is asked for either type: `DeleteHostedConfigurationVersion` takes the version away as
/// it answers, and a deployment's removal is the stop of a rollout still in flight
/// ([`delete`]), which that service reports no state a removed resource is still being deleted
/// in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has.
///
/// `Ok(None)` for both types, which is a change no call can make: a hosted configuration version
/// IS its content under the number AppConfig minted — that service publishes no call that edits
/// one — and a deployment IS one rollout of one version, `StartDeployment` being the only call
/// that makes one. Every property of either type is therefore create-only
/// ([`crate::state::template::replacement_properties`] names them), so a deployment restating one
/// replaces the resource rather than reaching this arm at all; a declaration that restates
/// nothing leaves the resource exactly as it is.
pub(super) async fn update(
    _ctx: &smithy_server_runtime::OperationContext<'_>,
    _region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, resource, properties_of(kind))?;
    for property in properties_of(kind) {
        if declared.get(*property) != resource.properties.get(*property) {
            return Ok(None);
        }
    }
    Ok(Some(Created::attributes(BTreeMap::new())))
}

/// Delete the resource a stack created.
///
/// A hosted configuration VERSION is removed by `DeleteHostedConfigurationVersion`, addressed by
/// the triple its create was recorded under. A DEPLOYMENT cannot be: AppConfig publishes no call
/// that removes one — the only call that acts on an existing deployment is `StopDeployment`,
/// which "stops a deployment" that is still being carried out — so a rollout still in flight is
/// STOPPED and a terminal one is left as the record of what was deployed. Judged on the state
/// that service reports, never on a message.
///
/// A resource AppConfig no longer holds is already in the state the delete asks for, judged on
/// the not-found CODE — which is also what makes deleting a version whose PROFILE has already
/// gone a completed deletion rather than a failure.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let (first, second, number) = addressed(resource)?;
    match kind {
        Kind::HostedConfigurationVersion => {
            let uri = format!(
                "{}/{}",
                hosted_versions_uri(&first, &second),
                percent_encoded(&number)
            );
            match call_appconfig(ctx, region, "DELETE", &uri, None).await {
                Ok(_) => Ok(()),
                Err(refusal) if refusal.code == NOT_FOUND => Ok(()),
                Err(refusal) => Err(refusal),
            }
        }
        Kind::Deployment => {
            let Some(reported) =
                read_deployment(ctx, region, &resource.physical_resource_id).await?
            else {
                return Ok(());
            };
            let state = reported
                .get(DEPLOYMENT_STATE)
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !DEPLOYMENT_IN_FLIGHT.contains(&state) {
                return Ok(());
            }
            let uri = format!(
                "{}/{}",
                deployments_uri(&first, &second),
                percent_encoded(&number)
            );
            match call_appconfig(ctx, region, "DELETE", &uri, None).await {
                Ok(_) => Ok(()),
                Err(refusal) if refusal.code == NOT_FOUND => Ok(()),
                Err(refusal) => Err(refusal),
            }
        }
    }
}

/// `GetDeployment`'s own report of the deployment `identity` names, or `None` for one AppConfig
/// no longer holds.
async fn read_deployment(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identity: &str,
) -> Result<Option<Value>, Refusal> {
    let (application_id, environment_id, number) = identity_parts(identity)?;
    let uri = format!(
        "{}/{}",
        deployments_uri(application_id, environment_id),
        percent_encoded(number)
    );
    match call_appconfig(ctx, region, "GET", &uri, None).await {
        Ok(reported) => Ok(Some(reported)),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The collection every hosted configuration version of ONE profile lives in, which
/// `CreateHostedConfigurationVersion` is bound to
/// (`/applications/{ApplicationId}/configurationprofiles/{ConfigurationProfileId}/hostedconfigurationversions`).
fn hosted_versions_uri(application_id: &str, profile_id: &str) -> String {
    format!(
        "/applications/{}/configurationprofiles/{}/hostedconfigurationversions",
        percent_encoded(application_id),
        percent_encoded(profile_id)
    )
}

/// The collection every deployment of ONE environment lives in, which `StartDeployment` is bound
/// to (`/applications/{ApplicationId}/environments/{EnvironmentId}/deployments`) and under which
/// `GetDeployment` and `StopDeployment` address one by its number.
fn deployments_uri(application_id: &str, environment_id: &str) -> String {
    format!(
        "/applications/{}/environments/{}/deployments",
        percent_encoded(application_id),
        percent_encoded(environment_id)
    )
}

/// The request headers `CreateHostedConfigurationVersion` binds its members to, as that
/// operation's own `@httpHeader` traits name them, and the response headers its answer reports
/// under.
const CONTENT_TYPE_HEADER: &str = "content-type";
const DESCRIPTION_HEADER: &str = "Description";
const LATEST_VERSION_NUMBER_HEADER: &str = "Latest-Version-Number";
const VERSION_LABEL_HEADER: &str = "VersionLabel";
const VERSION_NUMBER_HEADER: &str = "Version-Number";
const APPLICATION_ID_HEADER: &str = "Application-Id";
const CONFIGURATION_PROFILE_ID_HEADER: &str = "Configuration-Profile-Id";

/// The three parts of the resource's recorded identity, as the calls that read, stop or delete it
/// address it by.
fn addressed(resource: &StoredResource) -> Result<(String, String, String), Refusal> {
    let (first, second, number) = identity_parts(&resource.physical_resource_id).map_err(
        |_| Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!(
                "{} is identified by the triple its own API addresses one resource by, and {} is \
                 not one",
                resource.resource_type, resource.physical_resource_id
            ),
        },
    )?;
    Ok((
        first.to_string(),
        second.to_string(),
        number.to_string(),
    ))
}

/// [`addressed`] for a caller that has the identity itself rather than the record.
fn identity_parts(identity: &str) -> Result<(&str, &str, &str), Refusal> {
    appconfig_rollout_identity_parts(identity).ok_or_else(|| Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!(
            "an AWS AppConfig rollout resource is identified by the triple its own API addresses \
             one resource by, and {identity} is not one"
        ),
    })
}

/// Every property the type of `kind` publishes, for the one check that refuses a declared
/// property no call this module makes carries — and, since every one of them is fixed when the
/// resource is made, the set a changed declaration is judged against.
fn properties_of(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::HostedConfigurationVersion => HOSTED_CONFIGURATION_VERSION_PROPERTIES,
        Kind::Deployment => DEPLOYMENT_PROPERTIES,
    }
}

/// One property the declaration STATES, as the text the member it travels in carries; `None` for
/// a property the declaration leaves out, which is left off the request rather than sent empty —
/// so AppConfig's own validation judges the request it would judge for a client's own call.
fn stated(properties: &Value, property: &str) -> Option<String> {
    properties
        .get(property)
        .filter(|value| !value.is_null())
        .map(attribute_text)
        .filter(|value| !value.is_empty())
}

/// One property a call cannot be addressed without, as the text it carries, or the refusal for a
/// declaration that states none.
fn required(
    resource: &StoredResource,
    properties: &Value,
    property: &str,
) -> Result<String, Refusal> {
    stated(properties, property).ok_or_else(|| missing(resource, property))
}

/// The refusal for a declaration that states none of a property its type requires.
fn missing(resource: &StoredResource, property: &str) -> Refusal {
    Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!("{} requires the {property} property", resource.resource_type),
    }
}

/// The identifier a create answered in the response header `header`, which is the number
/// AppConfig minted for the resource.
///
/// A create that answered 2xx WITHOUT it identifies no resource, so the resource is reported as
/// failed rather than recorded under a number nothing answers to.
fn minted(
    answered: &BTreeMap<String, String>,
    header: &str,
    operation: &str,
    member: &str,
) -> Result<String, Refusal> {
    answered
        .get(header)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!("AppConfig answered {operation} without a {member}"),
        })
}

/// One scalar an answer carries, as the text the value spells — a number as the digits it is
/// written with, which is what a later call takes it as.
fn scalar_text(value: &Value) -> Option<String> {
    crate::state::scalar_text(value).filter(|text| !text.is_empty())
}

/// The refusal for a declared property no call this module makes carries, naming which property
/// it was: refusing is what keeps a resource from being reported as created with less than its
/// declaration asked for. `properties` is the document the deployment DECLARES, which on an
/// update is not the one the record carries.
fn refuse_unapplied_properties(
    properties: &Value,
    resource: &StoredResource,
    published: &[&str],
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if published.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    Ok(())
}

/// Call one AWS AppConfig operation on the `restJson1` wire its model declares, on the URI that
/// operation's own `@http` binding states.
async fn call_appconfig(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json(ctx, APPCONFIG_SIGNING_NAME, region, method, uri, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `StoredResource` of one of these types, as a deployment hands it to the arms above.
    fn declared(resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Subject".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: "abc1234|def5678|1".to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// The two type names this module owns are exactly the ones it claims, and nothing else: a
    /// near miss of one is another type's, which this module must not answer for.
    #[test]
    fn the_owned_types_are_the_two_appconfig_rollout_types() {
        let cases: &[(&str, Option<Kind>)] = &[
            (
                APPCONFIG_HOSTED_CONFIGURATION_VERSION,
                Some(Kind::HostedConfigurationVersion),
            ),
            (APPCONFIG_DEPLOYMENT, Some(Kind::Deployment)),
            ("AWS::AppConfig::Application", None),
            ("AWS::AppConfig::DeploymentStrategy", None),
            ("AWS::AppConfig::ConfigurationProfile", None),
            ("AWS::AppConfig::HostedConfiguration", None),
            ("AWS::SQS::Queue", None),
            ("", None),
        ];
        for (resource_type, expected) in cases {
            let actual = kind_of(resource_type);
            assert_eq!(
                actual, *expected,
                "case {resource_type:?}: expected kind_of {expected:?}, got {actual:?}"
            );
        }
    }

    /// Each type's collection URI is the one its own operation's `@http` binding declares, with
    /// every part of the identity percent-encoded as one value.
    #[test]
    fn a_resource_is_addressed_as_its_own_operation_binds_it() {
        let cases: &[(&str, String, &str)] = &[
            (
                "a hosted configuration version collection",
                hosted_versions_uri("abc1234", "def5678"),
                "/applications/abc1234/configurationprofiles/def5678/hostedconfigurationversions",
            ),
            (
                "a deployment collection",
                deployments_uri("abc1234", "xyz9012"),
                "/applications/abc1234/environments/xyz9012/deployments",
            ),
            (
                "an id carrying a separator",
                deployments_uri("a/b", "c d"),
                "/applications/a%2Fb/environments/c%20d/deployments",
            ),
        ];
        for (case, actual, expected) in cases {
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the URI {expected:?}, got {actual:?}"
            );
        }
    }

    /// A declaration that states none of a property its create cannot be made without is refused
    /// here, naming that property and its type, rather than sent as a request the owning service
    /// would reject.
    #[test]
    fn a_declaration_missing_a_required_property_is_refused() {
        let cases: &[(&str, &str, Value, &str)] = &[
            (
                "a hosted version without an application",
                APPCONFIG_HOSTED_CONFIGURATION_VERSION,
                serde_json::json!({ "ConfigurationProfileId": "def5678" }),
                APPLICATION_ID,
            ),
            (
                "a hosted version without content",
                APPCONFIG_HOSTED_CONFIGURATION_VERSION,
                serde_json::json!({
                    "ApplicationId": "abc1234",
                    "ConfigurationProfileId": "def5678",
                    "ContentType": "application/json"
                }),
                "Content",
            ),
            (
                "a deployment without an environment",
                APPCONFIG_DEPLOYMENT,
                serde_json::json!({ "ApplicationId": "abc1234" }),
                ENVIRONMENT_ID,
            ),
        ];
        for (case, resource_type, properties, property) in cases {
            let resource = declared(resource_type, properties.clone());
            let refusal = required(&resource, properties, property)
                .expect_err(&format!("case {case:?}: expected a refusal"));
            assert!(
                refusal.message.contains(property) && refusal.message.contains(resource_type),
                "case {case:?}: expected the refusal to name the property and the type, got \
                 {:?}",
                refusal.message
            );
        }
    }

    /// The recorded identity is read back as the three parts the owning API addresses one
    /// resource by, and a value that is not such a triple is refused rather than used to address
    /// something else.
    #[test]
    fn an_identity_is_read_back_as_the_triple_it_is() {
        let identity = appconfig_rollout_identity("abc1234", "def5678", "3");
        let (application, second, number) = identity_parts(&identity)
            .unwrap_or_else(|refusal| panic!("case \"a triple\": expected parts, got {refusal}"));
        assert_eq!(
            (application, second, number),
            ("abc1234", "def5678", "3"),
            "case \"a triple\": expected the three parts of {identity:?}, got \
             {application:?}/{second:?}/{number:?}"
        );
        for (case, malformed) in [
            ("a bare name", "a-name"),
            ("a pair", "abc1234|def5678"),
            ("four parts", "abc1234|def5678|1|2"),
        ] {
            let refusal = identity_parts(malformed)
                .expect_err(&format!("case {case:?}: expected a refusal"));
            assert!(
                refusal.message.contains(malformed),
                "case {case:?}: expected the refusal to name the value, got {:?}",
                refusal.message
            );
        }
    }

    /// A property no call this module makes carries is refused, naming it, so nothing is reported
    /// created with less than the declaration asked for — and every property each type publishes
    /// is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        let resource = declared(APPCONFIG_DEPLOYMENT, Value::Null);
        let published = serde_json::json!({
            "ApplicationId": "abc1234",
            "ConfigurationProfileId": "def5678",
            "ConfigurationVersion": "1",
            "DeploymentStrategyId": "ghi9012",
            "Description": "the rollout",
            "DynamicExtensionParameters": {},
            "EnvironmentId": "xyz9012",
            "KmsKeyIdentifier": "a-key",
            "Tags": []
        });
        assert!(
            refuse_unapplied_properties(&published, &resource, DEPLOYMENT_PROPERTIES).is_ok(),
            "case \"every published property\": expected it to be applied, got a refusal"
        );
        let unknown = serde_json::json!({ "ApplicationId": "abc1234", "LatestDeploymentNumber": 1 });
        let refusal = refuse_unapplied_properties(&unknown, &resource, DEPLOYMENT_PROPERTIES)
            .expect_err("case \"a property no call carries\": expected a refusal");
        assert!(
            refusal.message.contains("LatestDeploymentNumber"),
            "case \"a property no call carries\": expected the refusal to name it, got {:?}",
            refusal.message
        );
    }
}
