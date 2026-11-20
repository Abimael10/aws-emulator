// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of an Amazon Bedrock AgentCore GATEWAY and of one of its
//! TARGETS, and of the two SANDBOX types a template declares — a code interpreter and a
//! browser. One file of [`super`]'s per-type arms, beside the runtime's and the memory's
//! ([`super::bedrock_agentcore`]), whose shared readers every arm here uses.
//!
//! ## A target is created only once its gateway is READY
//!
//! `CreateGatewayTarget` is refused while the gateway it names is still `CREATING`: a target
//! is served BY the gateway, so the gateway has to be there to serve it. Nothing special is
//! needed for that here — a gateway's create answers it `CREATING`, so its own readiness arm
//! holds the gateway's creation open, and a target whose `GatewayIdentifier` READS the
//! gateway is withheld until that wait releases
//! ([`crate::state::provisioning::deferred`]), which is the order CloudFormation deploys a
//! stack's resources in.
//!
//! ## The sandbox types are created READY
//!
//! `CreateCodeInterpreter` and `CreateBrowser` each answer the sandbox `READY`, so neither
//! type has a readiness arm: the call that created the sandbox already said everything there
//! was to say about it. Neither service publishes an update of one either, so a changed
//! declaration of either type is reported as NOT applied rather than as a success that
//! dropped the caller's change — which is what a type with no [`super::Provisioner::update`]
//! arm reports.

use super::*;

// ---------------------------------------------------------------------------
// AWS::BedrockAgentCore::Gateway
// ---------------------------------------------------------------------------

/// The URI the gateway COLLECTION is bound to (`POST /gateways/`, trailing slash included, as
/// `CreateGateway`'s own `@http` binding spells it).
const GATEWAYS_URI: &str = "/gateways/";

/// The property an `AWS::BedrockAgentCore::Gateway` states the gateway's name in.
const GATEWAY_NAME_PROPERTY: &str = "Name";

/// The properties no member of the gateway's create carries from the declared document: the
/// tag map, which travels as that call's own `tags` member, and the name CloudFormation
/// decides.
const GATEWAY_OMITTED_PROPERTIES: &[&str] = &[TAGS_PROPERTY, GATEWAY_NAME_PROPERTY];

/// The members an answered gateway reports, as `(attribute, member)`: the two values this
/// type publishes as return values.
const GATEWAY_ATTRIBUTES: &[(&str, &str)] = &[
    ("GatewayId", "gatewayId"),
    ("GatewayArn", "gatewayArn"),
];

/// The attribute carrying the id that service minted, which is this resource's physical id.
const GATEWAY_ID_ATTRIBUTE: &str = "GatewayId";

/// The properties `GetGateway` reports back under the same names, for drift detection.
const GATEWAY_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    (GATEWAY_NAME_PROPERTY, "name"),
    ("Description", "description"),
    ("RoleArn", "roleArn"),
    ("ProtocolType", "protocolType"),
    ("AuthorizerType", "authorizerType"),
    ("ExceptionLevel", "exceptionLevel"),
];

/// The URI ONE gateway is addressed at (`/gateways/{gatewayIdentifier}/`).
fn gateway_uri(gateway_id: &str) -> String {
    format!("{GATEWAYS_URI}{}/", percent_encoded(gateway_id))
}

/// Create the gateway an `AWS::BedrockAgentCore::Gateway` declares, so a client that deployed
/// the stack reads it back through `GetGateway` by the `GatewayId` the stack published, and
/// so the targets the same template declares have a gateway to be created on.
pub(super) async fn create_agentcore_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = agentcore_members(&resource.properties, &[], GATEWAY_OMITTED_PROPERTIES)?;
    request.insert(
        "name".to_string(),
        Value::String(agentcore_name(
            &resource.properties,
            GATEWAY_NAME_PROPERTY,
            resource,
        )),
    );
    if let Some(tags) = agentcore_tags(&resource.properties, propagated) {
        request.insert("tags".to_string(), tags);
    }
    let answered = call_rest_json_named(
        ctx,
        AGENTCORE_SIGNING_NAME,
        region,
        "CreateGateway",
        "POST",
        GATEWAYS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let attributes = gateway_attributes(&answered)?;
    Ok(Created {
        physical_resource_id: attributes.get(GATEWAY_ID_ATTRIBUTE).cloned(),
        attributes,
    })
}

/// Whether the control plane has finished bringing up the gateway a stack created.
///
/// `CreateGateway` answers `202 Accepted` with the gateway `CREATING` — it serves no tool
/// call and accepts no target in that status — so the resource's creation is held open until
/// that service reports it `READY`.
pub(super) async fn agentcore_gateway_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(gateway) = read_agentcore_gateway(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "the Amazon Bedrock AgentCore control plane holds no gateway {physical_resource_id}"
        )));
    };
    Ok(agentcore_readiness(
        &format!("gateway {physical_resource_id}"),
        &reported_text(&gateway, STATUS_MEMBER).unwrap_or_default(),
        AGENTCORE_READY,
        AGENTCORE_SETTLING,
        status_reasons(&gateway),
    ))
}

/// Apply a changed declaration to the gateway the stack created, through `UpdateGateway` —
/// the one call that changes a gateway that exists. It takes the same members the create
/// does, the gateway's name included, so a changed declaration is applied to that gateway
/// rather than replacing it; its tags travel on the create alone.
pub(super) async fn update_agentcore_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let mut request = agentcore_members(declared, &[], GATEWAY_OMITTED_PROPERTIES)?;
    request.insert(
        "name".to_string(),
        Value::String(agentcore_name(declared, GATEWAY_NAME_PROPERTY, resource)),
    );
    let answered = call_rest_json_named(
        ctx,
        AGENTCORE_SIGNING_NAME,
        region,
        "UpdateGateway",
        "PUT",
        &gateway_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Created::attributes(gateway_attributes(&answered)?))
}

/// Delete the gateway a stack created, by the id that service minted for it.
pub(super) async fn delete_agentcore_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_agentcore(
        ctx,
        region,
        "DeleteGateway",
        &gateway_uri(physical_resource_id),
    )
    .await
}

/// Whether the control plane has finished removing the gateway a stack deleted.
pub(super) async fn agentcore_gateway_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    agentcore_removal(
        ctx,
        region,
        "GetGateway",
        &gateway_uri(physical_resource_id),
    )
    .await
}

/// The gateway's configuration as the control plane reports it, for drift detection.
pub(super) async fn describe_agentcore_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_agentcore_gateway(ctx, region, physical_resource_id)
        .await?
        .map(|gateway| reported_as_properties(&gateway, GATEWAY_REPORTED_PROPERTIES)))
}

/// The gateway the control plane holds under `physical_resource_id`, or `None` for one it
/// does not.
async fn read_agentcore_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    read_agentcore(
        ctx,
        region,
        "GetGateway",
        &gateway_uri(physical_resource_id),
    )
    .await
}

/// The attributes one answered gateway reports ([`GATEWAY_ATTRIBUTES`]).
fn gateway_attributes(answered: &Value) -> Result<BTreeMap<String, String>, Refusal> {
    let attributes = reported_attributes(answered, GATEWAY_ATTRIBUTES);
    if !attributes.contains_key(GATEWAY_ID_ATTRIBUTE) {
        return Err(no_identifier_answered("a gateway"));
    }
    Ok(attributes)
}

// ---------------------------------------------------------------------------
// AWS::BedrockAgentCore::GatewayTarget
// ---------------------------------------------------------------------------

/// The property an `AWS::BedrockAgentCore::GatewayTarget` states the gateway the target
/// belongs to in, which is the `gatewayIdentifier` every call on a target binds as its URI
/// label.
pub(in crate::state) const GATEWAY_TARGET_GATEWAY_PROPERTY: &str = "GatewayIdentifier";

/// The property such a declaration states the target's own name in.
const GATEWAY_TARGET_NAME_PROPERTY: &str = "Name";

/// The attribute carrying the id the control plane mints for a target, which is the second
/// part of the identity the type is addressed by.
pub(in crate::state) const GATEWAY_TARGET_ID_ATTRIBUTE: &str = "TargetId";

/// The attribute carrying the ARN of the gateway the target belongs to, the other value this
/// type publishes as a return value.
pub(in crate::state) const GATEWAY_TARGET_GATEWAY_ARN_ATTRIBUTE: &str = "GatewayArn";

/// The two values an `AWS::BedrockAgentCore::GatewayTarget` publishes as return values, for
/// the return-value catalogue ([`super::super::template::return_values`]).
pub(in crate::state) const GATEWAY_TARGET_ATTRIBUTE_NAMES: &[&str] = &[
    GATEWAY_TARGET_GATEWAY_ARN_ATTRIBUTE,
    GATEWAY_TARGET_ID_ATTRIBUTE,
];

/// The properties no member of the target's create or update carries from the declared
/// document: the gateway, which the URI carries, and the name CloudFormation decides.
const GATEWAY_TARGET_OMITTED_PROPERTIES: &[&str] = &[
    GATEWAY_TARGET_GATEWAY_PROPERTY,
    GATEWAY_TARGET_NAME_PROPERTY,
];

/// The members an answered target reports, as `(attribute, member)`.
const GATEWAY_TARGET_ATTRIBUTES: &[(&str, &str)] = &[
    (GATEWAY_TARGET_ID_ATTRIBUTE, "targetId"),
    (GATEWAY_TARGET_GATEWAY_ARN_ATTRIBUTE, "gatewayArn"),
];

/// The properties `GetGatewayTarget` reports back under the same names, for drift detection.
const GATEWAY_TARGET_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    (GATEWAY_TARGET_NAME_PROPERTY, "name"),
    ("Description", "description"),
];

/// The URI the TARGET COLLECTION of one gateway is bound to
/// (`POST /gateways/{gatewayIdentifier}/targets/`).
fn gateway_targets_uri(gateway_id: &str) -> String {
    format!("{}targets/", gateway_uri(gateway_id))
}

/// The URI ONE target is addressed at: its collection plus the `targetId` label.
fn gateway_target_uri(gateway_id: &str, target_id: &str) -> String {
    format!(
        "{}{}/",
        gateway_targets_uri(gateway_id),
        percent_encoded(target_id)
    )
}

/// The NAME the target is created under: the `Name` the declaration states, or — for a
/// declaration that states none, which that type's own contract marks required — the TARGET
/// half of the identity CloudFormation's naming rules computed for the resource.
///
/// That half, not the whole physical id and not a label split off it: this type's identity is
/// the PAIR `<gatewayId>|<targetId>`, so the generic label fallback would take a slice of the
/// GATEWAY's id — the same one for every unnamed target of one gateway. The target half is
/// per resource and satisfies `TargetName` (`^([0-9a-zA-Z][-]?){1,100}$`), so the create
/// carries a name that member accepts rather than an empty or a shared one. The same reading
/// a Bedrock data source's name falls back on ([`super::bedrock_data_sources`]).
fn gateway_target_name(declared: &Value, resource: &StoredResource) -> String {
    let stated = declared_scalar(declared, GATEWAY_TARGET_NAME_PROPERTY);
    if !stated.is_empty() {
        return stated;
    }
    template::agentcore_gateway_target_identity_parts(&resource.physical_resource_id)
        .map(|(_, target_id)| target_id.to_string())
        .filter(|target_id| !target_id.is_empty())
        .unwrap_or_else(|| {
            resource
                .attributes
                .get(GATEWAY_TARGET_ID_ATTRIBUTE)
                .cloned()
                .unwrap_or_default()
        })
}

/// Create the target an `AWS::BedrockAgentCore::GatewayTarget` declares on the gateway its
/// declaration names, so a client that deployed the stack finds it in `ListGatewayTargets`
/// and reads it back through `GetGatewayTarget` by the pair the stack published — and so the
/// gateway really serves the tools the template described.
///
/// The identity is the control plane's: the `targetId` it minted, joined with the gateway the
/// target was created on, becomes this resource's physical id — the primary identifier of the
/// type.
pub(super) async fn create_agentcore_gateway_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    // The gateway is read off the DECLARATION, not off the identity: the declaration is the
    // resolved request — a reference to a gateway of the same stack carries the id that
    // service minted by now — while the identity the naming rules computed is only a
    // stand-in until this create answers.
    let gateway_id = declared_scalar(&resource.properties, GATEWAY_TARGET_GATEWAY_PROPERTY);
    let mut request = agentcore_members(
        &resource.properties,
        &[],
        GATEWAY_TARGET_OMITTED_PROPERTIES,
    )?;
    request.insert(
        "name".to_string(),
        Value::String(gateway_target_name(&resource.properties, resource)),
    );
    let answered = call_rest_json_named(
        ctx,
        AGENTCORE_SIGNING_NAME,
        region,
        "CreateGatewayTarget",
        "POST",
        &gateway_targets_uri(&gateway_id),
        Some(&Value::Object(request)),
    )
    .await?;
    let attributes = gateway_target_attributes(&answered)?;
    Ok(Created {
        physical_resource_id: Some(template::agentcore_gateway_target_identity(
            &gateway_id,
            attributes
                .get(GATEWAY_TARGET_ID_ATTRIBUTE)
                .map(String::as_str)
                .unwrap_or_default(),
        )),
        attributes,
    })
}

/// Whether the control plane has finished bringing up the target a stack created.
///
/// `CreateGatewayTarget` answers `202 Accepted` with the target `CREATING`, and a target
/// whose tools the gateway reads from the endpoint it names walks `SYNCHRONIZING` before it
/// is usable, so the resource's creation is held open until that service reports it `READY`.
pub(super) async fn agentcore_gateway_target_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(target) = read_agentcore_gateway_target(ctx, region, physical_resource_id).await?
    else {
        return Ok(Readiness::Failed(format!(
            "the Amazon Bedrock AgentCore control plane holds no gateway target \
             {physical_resource_id}"
        )));
    };
    Ok(agentcore_readiness(
        &format!("gateway target {physical_resource_id}"),
        &reported_text(&target, STATUS_MEMBER).unwrap_or_default(),
        AGENTCORE_READY,
        TARGET_SETTLING,
        status_reasons(&target),
    ))
}

/// Apply a changed declaration to the target the stack created, through
/// `UpdateGatewayTarget` — the one call that changes a target that exists. It takes the same
/// members the create does by the gateway-and-target pair, so a member the declaration no
/// longer states leaves the target without it.
///
/// `Ok(None)` is a change that call cannot make: a declaration that re-points the target at
/// ANOTHER gateway, which no call moves a target between — that is a different target, under
/// a different identity.
pub(super) async fn update_agentcore_gateway_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let Some((gateway_id, target_id)) =
        template::agentcore_gateway_target_identity_parts(&resource.physical_resource_id)
    else {
        return Ok(None);
    };
    if declared_scalar(declared, GATEWAY_TARGET_GATEWAY_PROPERTY) != gateway_id {
        return Ok(None);
    }
    let mut request = agentcore_members(declared, &[], GATEWAY_TARGET_OMITTED_PROPERTIES)?;
    request.insert(
        "name".to_string(),
        Value::String(gateway_target_name(declared, resource)),
    );
    let answered = call_rest_json_named(
        ctx,
        AGENTCORE_SIGNING_NAME,
        region,
        "UpdateGatewayTarget",
        "PUT",
        &gateway_target_uri(gateway_id, target_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Some(Created::attributes(gateway_target_attributes(
        &answered,
    )?)))
}

/// Delete the target a stack created, by the gateway and the id it was created under — the
/// two parts of this resource's physical id.
pub(super) async fn delete_agentcore_gateway_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let Some((gateway_id, target_id)) =
        template::agentcore_gateway_target_identity_parts(physical_resource_id)
    else {
        return Ok(());
    };
    delete_agentcore(
        ctx,
        region,
        "DeleteGatewayTarget",
        &gateway_target_uri(gateway_id, target_id),
    )
    .await
}

/// Whether the control plane has finished removing the target a stack deleted.
pub(super) async fn agentcore_gateway_target_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match read_agentcore_gateway_target(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(target) => Ok(Removal::Pending(
            reported_text(&target, STATUS_MEMBER).unwrap_or_else(|| "DELETING".to_string()),
        )),
    }
}

/// The target's configuration as the control plane reports it, for drift detection.
pub(super) async fn describe_agentcore_gateway_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(
        read_agentcore_gateway_target(ctx, region, physical_resource_id)
            .await?
            .map(|target| reported_as_properties(&target, GATEWAY_TARGET_REPORTED_PROPERTIES)),
    )
}

/// The target the control plane holds under the pair `physical_resource_id` carries, or `None`
/// where it holds none — a gateway it no longer holds answers the same not-found code for a
/// target of it, which is the same answer.
async fn read_agentcore_gateway_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some((gateway_id, target_id)) =
        template::agentcore_gateway_target_identity_parts(physical_resource_id)
    else {
        return Ok(None);
    };
    read_agentcore(
        ctx,
        region,
        "GetGatewayTarget",
        &gateway_target_uri(gateway_id, target_id),
    )
    .await
}

/// The attributes one answered target reports ([`GATEWAY_TARGET_ATTRIBUTES`]).
fn gateway_target_attributes(answered: &Value) -> Result<BTreeMap<String, String>, Refusal> {
    let attributes = reported_attributes(answered, GATEWAY_TARGET_ATTRIBUTES);
    if !attributes.contains_key(GATEWAY_TARGET_ID_ATTRIBUTE) {
        return Err(no_identifier_answered("a gateway target"));
    }
    Ok(attributes)
}

// ---------------------------------------------------------------------------
// AWS::BedrockAgentCore::CodeInterpreter and AWS::BedrockAgentCore::Browser
// ---------------------------------------------------------------------------

/// Which of the two SANDBOX types one declaration is: the two differ only in the URI their
/// calls are bound to and in the names of the values they publish, so one set of arms serves
/// both and each arm is told which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Sandbox {
    /// `AWS::BedrockAgentCore::CodeInterpreter`, whose calls are bound under
    /// `/code-interpreters`.
    CodeInterpreter,
    /// `AWS::BedrockAgentCore::Browser`, whose calls are bound under `/browsers`.
    Browser,
}

impl Sandbox {
    /// The URI this sandbox's COLLECTION is bound to, as its create's `@http` binding spells
    /// it (`PUT /code-interpreters`, `PUT /browsers` — neither carries a trailing slash).
    fn collection_uri(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "/code-interpreters",
            Self::Browser => "/browsers",
        }
    }

    /// The URI ONE sandbox of this kind is addressed at.
    fn uri(self, sandbox_id: &str) -> String {
        format!("{}/{}", self.collection_uri(), percent_encoded(sandbox_id))
    }

    /// The operation each of the three calls this type makes is named by, for the diagnostic
    /// a refused call reports ([`call_rest_json_named`]).
    fn operations(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::CodeInterpreter => (
                "CreateCodeInterpreter",
                "GetCodeInterpreter",
                "DeleteCodeInterpreter",
            ),
            Self::Browser => ("CreateBrowser", "GetBrowser", "DeleteBrowser"),
        }
    }

    /// The `(attribute, member)` pairs this type publishes as return values: the id the
    /// control plane minted and the ARN it built on it.
    fn attributes(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::CodeInterpreter => &[
                ("CodeInterpreterId", "codeInterpreterId"),
                ("CodeInterpreterArn", "codeInterpreterArn"),
            ],
            Self::Browser => &[("BrowserId", "browserId"), ("BrowserArn", "browserArn")],
        }
    }

    /// The attribute carrying the id, which is this resource's physical id.
    fn id_attribute(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "CodeInterpreterId",
            Self::Browser => "BrowserId",
        }
    }

    /// What a refusal and a failed read call this kind of sandbox.
    fn described(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "a code interpreter",
            Self::Browser => "a browser",
        }
    }
}

/// The property either sandbox type states its name in.
const SANDBOX_NAME_PROPERTY: &str = "Name";

/// The properties no member of either create carries from the declared document.
const SANDBOX_OMITTED_PROPERTIES: &[&str] = &[TAGS_PROPERTY, SANDBOX_NAME_PROPERTY];

/// The properties either read reports back under the same names, for drift detection.
const SANDBOX_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    (SANDBOX_NAME_PROPERTY, "name"),
    ("Description", "description"),
    ("ExecutionRoleArn", "executionRoleArn"),
];

/// Create the sandbox an `AWS::BedrockAgentCore::CodeInterpreter` or
/// `AWS::BedrockAgentCore::Browser` declares, so a client that deployed the stack reads it
/// back through that type's own read by the id the stack published — and so a session can be
/// started on the sandbox the stack reported created.
pub(super) async fn create_agentcore_sandbox(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    sandbox: Sandbox,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = agentcore_members(&resource.properties, &[], SANDBOX_OMITTED_PROPERTIES)?;
    request.insert(
        "name".to_string(),
        Value::String(agentcore_name(
            &resource.properties,
            SANDBOX_NAME_PROPERTY,
            resource,
        )),
    );
    if let Some(tags) = agentcore_tags(&resource.properties, propagated) {
        request.insert("tags".to_string(), tags);
    }
    let answered = call_rest_json_named(
        ctx,
        AGENTCORE_SIGNING_NAME,
        region,
        sandbox.operations().0,
        "PUT",
        sandbox.collection_uri(),
        Some(&Value::Object(request)),
    )
    .await?;
    let attributes = sandbox_attributes(sandbox, &answered)?;
    Ok(Created {
        physical_resource_id: attributes.get(sandbox.id_attribute()).cloned(),
        attributes,
    })
}

/// Delete the sandbox a stack created, by the id that service minted for it.
pub(super) async fn delete_agentcore_sandbox(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    sandbox: Sandbox,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    delete_agentcore(
        ctx,
        region,
        sandbox.operations().2,
        &sandbox.uri(physical_resource_id),
    )
    .await
}

/// Whether the control plane has finished removing the sandbox a stack deleted: either delete
/// answers `202 Accepted`, so the sandbox is still reported while that service takes it away.
pub(super) async fn agentcore_sandbox_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    sandbox: Sandbox,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    agentcore_removal(
        ctx,
        region,
        sandbox.operations().1,
        &sandbox.uri(physical_resource_id),
    )
    .await
}

/// The sandbox's configuration as the control plane reports it, for drift detection.
pub(super) async fn describe_agentcore_sandbox(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    sandbox: Sandbox,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_agentcore(
        ctx,
        region,
        sandbox.operations().1,
        &sandbox.uri(physical_resource_id),
    )
    .await?
    .map(|answered| reported_as_properties(&answered, SANDBOX_REPORTED_PROPERTIES)))
}

/// The attributes one answered sandbox reports ([`Sandbox::attributes`]).
fn sandbox_attributes(
    sandbox: Sandbox,
    answered: &Value,
) -> Result<BTreeMap<String, String>, Refusal> {
    let attributes = reported_attributes(answered, sandbox.attributes());
    if !attributes.contains_key(sandbox.id_attribute()) {
        return Err(no_identifier_answered(sandbox.described()));
    }
    Ok(attributes)
}
