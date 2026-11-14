// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Amazon EventBridge EVENT BUS: the bus an
//! `AWS::Events::EventBus` declares, and the resource-policy statement an
//! `AWS::Events::EventBusPolicy` installs on one. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! ## Why a bus needs an arm at all
//!
//! A resource another resource of the same stack NAMES has to exist in the service that owns
//! it by the time that second resource is created: CloudFormation creates a stack's resources
//! in dependency order, so a rule whose `EventBusName` is a `Ref` of a declared bus is created
//! after the bus and resolves that `Ref` to the bus's name. With no arm for the bus, the name
//! resolves to a bus EventBridge never made, and every resource that names it fails with that
//! service's own `ResourceNotFoundException`.
//!
//! ## `AWS::Events::EventBus`
//!
//! Its properties are `CreateEventBus`'s own members, spelled identically
//! ([`EVENT_BUS_MEMBERS`]), with three decided here: the bus's `Name`, which is this
//! resource's physical id and what `Ref` returns; the `Tags`, which are the declaration's own
//! beside the ones CloudFormation propagates; and the `Policy`, which is not a member of that
//! call at all — a bus's resource policy is installed by `PutPermission`, the same second call
//! a client makes.
//!
//! `UpdateEventBus` is what applies a changed declaration ([`EVENT_BUS_UPDATED_MEMBERS`]):
//! the bus's name and its `EventSourceName` are create-only on that API — neither is a member
//! of it — so a declaration restating either asks for a different bus, which CloudFormation
//! replaces rather than updates
//! ([`super::template::replacement_properties`], [`super::template::name_is_create_only`]).
//!
//! ## `AWS::Events::EventBusPolicy`
//!
//! This resource is one GRANT on a bus's resource policy, addressed by its `StatementId`:
//! "each StatementId must be unique", `RemovePermission` revokes the statement that id names,
//! and both the bus and the id are create-only properties of the type. So the arm installs the
//! ONE statement its declaration describes and leaves every other statement the bus holds
//! alone — a second policy resource on the same bus adds its own grant rather than displacing
//! the first.
//!
//! EventBridge takes such a grant in either of two forms, and the declaration decides which:
//! the members that spell out one grant (`Action`, `Principal`, `StatementId`, `Condition`),
//! which `PutPermission` takes under those very names, or the whole statement as a document
//! (`Statement`, which that call takes as `Policy`: "you can include a Policy parameter in
//! the request instead of using the StatementId, Action, Principal, or Condition
//! parameters"). The document form replaces the bus's whole policy, so the statement the
//! declaration describes is merged into the document the bus holds now
//! ([`merged_policy_document`]) and that merged document is what is put — which is what makes
//! the per-statement revoke the delete performs coherent.

use super::*;

/// The `AWS::Events::EventBus` properties `CreateEventBus` takes under the SAME member name.
///
/// Source: AWS's CloudFormation resource reference for the type against
/// `smithy_query(operation="CreateEventBus")` — `Description`, `EventSourceName`,
/// `KmsKeyIdentifier`, `DeadLetterConfig` and `LogConfig` are spelled identically on both
/// sides. The three properties NOT here are the three that are not passed straight through:
/// `Name`, which CloudFormation decides and which is this resource's physical id, `Tags`,
/// which this arm rebuilds from the declaration's own tags and the ones CloudFormation
/// propagates, and `Policy`, which is no member of that call.
pub(super) const EVENT_BUS_MEMBERS: &[&str] = &[
    "DeadLetterConfig",
    "Description",
    "EventSourceName",
    "KmsKeyIdentifier",
    "LogConfig",
];

/// Those of them `UpdateEventBus` takes (`smithy_query(operation="UpdateEventBus")`): every
/// one but `EventSourceName`, the partner source a bus is created for, which that call takes
/// no member for.
pub(super) const EVENT_BUS_UPDATED_MEMBERS: &[&str] = &[
    "DeadLetterConfig",
    "Description",
    "KmsKeyIdentifier",
    "LogConfig",
];

/// The `Version` an event-bus resource policy document carries: the IAM policy language
/// version, which is what a policy assembled here states when the document it is merged into
/// states none.
const POLICY_VERSION: &str = "2012-10-17";

/// Create the event bus an `AWS::Events::EventBus` declares, so a rule, a policy or a rule
/// target of the same stack that names the bus resolves it — and a client reading the stack's
/// outputs and then calling `events DescribeEventBus` with that name finds the bus there
/// rather than a `ResourceNotFoundException`.
///
/// `Ref` of this type returns the bus NAME, which is this resource's physical id, so the bus
/// is created under exactly the name the stack reports for it; the `EventBusArn` the create
/// answers is the bus's real identity and replaces the computed one.
///
/// A declared `Policy` is installed after the bus exists, through the call that installs one
/// ([`put_bus_policy_document`]). A bus whose policy could not be installed is not the
/// resource the declaration describes, so the bus this call just made is removed before that
/// refusal is reported: nothing is left behind under a name the stack does not record.
pub(super) async fn create_event_bus(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let name = &resource.physical_resource_id;
    let mut request = declared_members(&resource.properties, EVENT_BUS_MEMBERS);
    request.insert("Name".to_string(), Value::String(name.clone()));
    let tags = tags_of(&resource.properties, propagated);
    // `CreateEventBus$Tags` is "tags to associate with the event bus": a bus with no tag at
    // all carries the member absent rather than an empty list.
    if !tags.is_empty() {
        request.insert("Tags".to_string(), tags_as_list(&tags, UPPER_CASED_TAG));
    }
    let reported = call_events(ctx, region, "CreateEventBus", &Value::Object(request)).await?;
    let mut attributes = BTreeMap::new();
    if let Some(arn) = reported.get("EventBusArn").and_then(Value::as_str) {
        attributes.insert("Arn".to_string(), arn.to_string());
    }
    attributes.insert("Name".to_string(), name.clone());
    if let Some(document) = declared_bus_policy(&resource.properties) {
        if let Err(refusal) = put_bus_policy_document(ctx, region, name, &document).await {
            // Best effort: the refusal that stopped the create is what the stack reports, so
            // a failure to undo it must not displace that reason.
            let _ = delete_event_bus(ctx, region, name).await;
            return Err(refusal);
        }
    }
    Ok(Created::attributes(attributes))
}

/// Apply a CHANGED `AWS::Events::EventBus` declaration to the bus the stack already has,
/// through that service's own `UpdateEventBus`.
///
/// The members that call takes are put under the bus's name ([`EVENT_BUS_UPDATED_MEMBERS`]);
/// the declared `Policy` is applied the way the create applies one, and a declaration that no
/// longer states one leaves the bus with no policy at all — `RemovePermission` with
/// `RemoveAllPermissions`, which is the state a bus nothing has been granted on is in.
pub(super) async fn update_event_bus(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let name = &resource.physical_resource_id;
    let mut request = declared_members(declared, EVENT_BUS_UPDATED_MEMBERS);
    request.insert("Name".to_string(), Value::String(name.clone()));
    let reported = call_events(ctx, region, "UpdateEventBus", &Value::Object(request)).await?;
    let mut attributes = BTreeMap::new();
    if let Some(arn) = reported.get("Arn").and_then(Value::as_str) {
        attributes.insert("Arn".to_string(), arn.to_string());
    }
    attributes.insert("Name".to_string(), name.clone());
    match declared_bus_policy(declared) {
        Some(document) => put_bus_policy_document(ctx, region, name, &document).await?,
        None if declared_bus_policy(&resource.properties).is_some() => {
            revoke_every_grant(ctx, region, name).await?;
        }
        None => {}
    }
    Ok(Some(Created::attributes(attributes)))
}

/// Delete the event bus a stack created, addressed by the name it was created under — this
/// resource's physical id.
///
/// `DeleteEventBus` is idempotent ("deletes the specified custom event bus"; its model
/// declares no error that reports a missing resource), so a bus EventBridge no longer holds
/// is already in the state the delete asks for.
pub(super) async fn delete_event_bus(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert(
        "Name".to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    call_events(ctx, region, "DeleteEventBus", &Value::Object(request)).await?;
    Ok(())
}

/// The bus's configuration as EventBridge reports it, for drift detection. A bus it no longer
/// holds (`ResourceNotFoundException`) was deleted.
///
/// `DescribeEventBus` answers the members the type declares under the very names it declares
/// them with, so its answer IS the actual configuration document. The bus's `Policy` is
/// deliberately left out: that document holds every statement installed on the bus — an
/// `AWS::Events::EventBusPolicy` resource's grant as much as a `Policy` this declaration
/// states — so what the bus reports is not this declaration's property, and a property the
/// read does not report is not evidence of anything ([`super::drift`]).
pub(super) async fn describe_event_bus(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_event_bus(ctx, region, physical_resource_id)
        .await?
        .map(|bus| {
            reported_as_properties(
                &bus,
                &[
                    ("DeadLetterConfig", "DeadLetterConfig"),
                    ("Description", "Description"),
                    ("KmsKeyIdentifier", "KmsKeyIdentifier"),
                    ("LogConfig", "LogConfig"),
                    ("Name", "Name"),
                ],
            )
        }))
}

/// Install the grant an `AWS::Events::EventBusPolicy` declares on the bus it names, so a
/// principal that bus's policy now admits can publish onto it.
///
/// The declaration states the grant in one of the two forms `PutPermission` takes — see this
/// module's docs — and the statement id is the one member both forms carry, since it is what
/// identifies the grant and what the delete revokes.
pub(super) async fn put_event_bus_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    install_grant(ctx, region, &resource.properties).await?;
    Ok(Created::attributes(BTreeMap::new()))
}

/// Apply a CHANGED `AWS::Events::EventBusPolicy` declaration to the grant the stack already
/// has: the same install the create performs, under the same statement id — "each StatementId
/// must be unique", so putting the grant again under an id already installed changes that one
/// statement and leaves every other alone.
///
/// The bus and the statement id are create-only properties of the type, so a declaration that
/// restates either is a replacement rather than a change reaching here.
pub(super) async fn update_event_bus_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    install_grant(ctx, region, declared).await?;
    Ok(Some(Created::attributes(BTreeMap::new())))
}

/// Revoke the grant a stack installed: `RemovePermission` for the statement id the
/// declaration names, on the bus it names — the two members that identify one grant, which
/// only the resource's own record carries.
///
/// A grant EventBridge no longer holds — the statement id names no statement, or the bus
/// itself is gone — is already in the state the delete asks for, judged on that service's
/// not-found CODE and never on a message.
pub(super) async fn remove_event_bus_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let Some(sid) = declared_statement_id(&resource.properties) else {
        return Ok(());
    };
    let mut request = Map::new();
    request.insert("StatementId".to_string(), Value::String(sid));
    if let Some(bus) = declared_policy_bus(&resource.properties) {
        request.insert("EventBusName".to_string(), Value::String(bus));
    }
    match call_events(ctx, region, "RemovePermission", &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether the grant a stack installed is still on the bus, for drift detection: the bus's
/// own policy is read back and the statement the declaration's `StatementId` names looked for.
/// A statement the bus no longer carries — or a bus that is gone — is the `DELETED` drift
/// status.
///
/// What is reported is the pair that identifies the grant, under the property names the
/// declaration uses. The grant's own members are not: EventBridge reports the bus's whole
/// policy document rather than one grant's declared members, so there is nothing of them to
/// compare against — and a property the read does not report is not evidence of anything
/// ([`super::drift`]).
pub(super) async fn describe_event_bus_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(sid) = declared_statement_id(&resource.properties) else {
        return Ok(None);
    };
    let bus = declared_policy_bus(&resource.properties);
    let held = match &bus {
        Some(bus) => read_event_bus(ctx, region, bus).await?,
        None => read_event_bus(ctx, region, template::DEFAULT_EVENT_BUS).await?,
    };
    let carried = held
        .as_ref()
        .and_then(|bus| bus.get("Policy"))
        .and_then(Value::as_str)
        .is_some_and(|document| statement_installed(document, &sid));
    if !carried {
        return Ok(None);
    }
    let mut reported = Map::new();
    if let Some(bus) = resource.properties.get("EventBusName") {
        reported.insert("EventBusName".to_string(), bus.clone());
    }
    reported.insert("StatementId".to_string(), Value::String(sid));
    Ok(Some(Value::Object(reported)))
}

/// Install the grant `properties` describes, in whichever of `PutPermission`'s two forms the
/// declaration states it in (see this module's docs). The one place a declaration of this type
/// becomes a request, so its create and its update cannot install two different grants out of
/// one declaration.
///
/// The statement id is required of either form: it is what identifies the grant — "each
/// StatementId must be unique", it is the key the document form merges under, and it is what
/// the delete revokes — so a declaration carrying none describes no grant this provider could
/// install or ever remove. CloudFormation refuses such a declaration before any call is made
/// ([`super::super::property_validation::REQUIRED_PROPERTIES`]); this is the provider's own
/// guard, so no statement can reach a bus without the id that addresses it.
async fn install_grant(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    properties: &Value,
) -> Result<(), Refusal> {
    let bus = declared_policy_bus(properties);
    let Some(sid) = declared_statement_id(properties) else {
        return Err(Refusal {
            code: provisioning::HANDLER_ERROR_INVALID_REQUEST.to_string(),
            message: MISSING_STATEMENT_ID.to_string(),
        });
    };
    if let Some(statement) = declared_statement(properties) {
        let name = bus.unwrap_or_else(|| template::DEFAULT_EVENT_BUS.to_string());
        let held = read_event_bus(ctx, region, &name).await?;
        let document = merged_policy_document(
            held.as_ref()
                .and_then(|bus| bus.get("Policy"))
                .and_then(Value::as_str),
            &sid,
            &statement,
        );
        return put_bus_policy_document(ctx, region, &name, &document).await;
    }
    let mut request = Map::new();
    for member in ["Action", "Principal", "Condition"] {
        if let Some(value) = properties.get(member) {
            request.insert(member.to_string(), value.clone());
        }
    }
    request.insert("StatementId".to_string(), Value::String(sid));
    if let Some(bus) = bus {
        request.insert("EventBusName".to_string(), Value::String(bus));
    }
    call_events(ctx, region, "PutPermission", &Value::Object(request)).await?;
    Ok(())
}

/// What this provider reports for a declaration that states no statement id, in the words
/// EventBridge itself refuses a `PutPermission` carrying no `StatementId` with.
const MISSING_STATEMENT_ID: &str = "Parameter(s) StatementId must be specified.";

/// `PutPermission` with `document` as the bus's whole resource policy: the form that call
/// takes a policy in rather than a grant's members ("a JSON string that describes the
/// permission policy statement").
async fn put_bus_policy_document(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    bus: &str,
    document: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert("Policy".to_string(), Value::String(document.to_string()));
    request.insert("EventBusName".to_string(), Value::String(bus.to_string()));
    call_events(ctx, region, "PutPermission", &Value::Object(request)).await?;
    Ok(())
}

/// Revoke every grant on the bus `name`: `RemovePermission` with `RemoveAllPermissions`, which
/// leaves the bus with no resource policy at all. A bus EventBridge no longer holds is already
/// in that state.
async fn revoke_every_grant(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<(), Refusal> {
    let mut request = Map::new();
    request.insert("EventBusName".to_string(), Value::String(name.to_string()));
    request.insert("RemoveAllPermissions".to_string(), Value::Bool(true));
    match call_events(ctx, region, "RemovePermission", &Value::Object(request)).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// `DescribeEventBus` for one bus, or `None` for a bus that service does not hold
/// (`ResourceNotFoundException`, judged on the CODE). The one read behind the drift
/// comparison and the policy merge, so neither asks for a bus differently.
async fn read_event_bus(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    let mut request = Map::new();
    request.insert("Name".to_string(), Value::String(name.to_string()));
    match call_events(ctx, region, "DescribeEventBus", &Value::Object(request)).await {
        Ok(bus) => Ok(Some(bus)),
        Err(refusal) if refusal.code == RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The resource policy document that installs `statement` under the statement id `sid` into
/// the document `held` — the one the bus carries now, where it carries one.
///
/// A statement is identified by its `Sid`, so the declared statement is written with `sid` as
/// its `Sid` and REPLACES the held statement carrying that id; a bus holding no statement
/// under it gains one, and every other statement the bus holds is carried over untouched and
/// in order. The `Version` is the held document's where it states one, since replacing the
/// policy must not restate the policy language version the bus was using.
fn merged_policy_document(held: Option<&str>, sid: &str, statement: &Value) -> String {
    let parsed: Option<Value> = held.and_then(|text| serde_json::from_str(text).ok());
    let version = parsed
        .as_ref()
        .and_then(|document| document.get("Version"))
        .and_then(Value::as_str)
        .unwrap_or(POLICY_VERSION)
        .to_string();
    let mut statements = held_statements(parsed.as_ref());
    let mut installed = match statement {
        Value::Object(members) => members.clone(),
        _ => Map::new(),
    };
    installed.insert("Sid".to_string(), Value::String(sid.to_string()));
    let installed = Value::Object(installed);
    match statements
        .iter()
        .position(|statement| statement_id_of(statement) == Some(sid))
    {
        Some(at) => statements[at] = installed,
        None => statements.push(installed),
    }
    let mut document = Map::new();
    document.insert("Version".to_string(), Value::String(version));
    document.insert("Statement".to_string(), Value::Array(statements));
    Value::Object(document).to_string()
}

/// Whether the policy `document` carries a statement under the statement id `sid`.
fn statement_installed(document: &str, sid: &str) -> bool {
    let parsed: Option<Value> = serde_json::from_str(document).ok();
    held_statements(parsed.as_ref())
        .iter()
        .any(|statement| statement_id_of(statement) == Some(sid))
}

/// The statements of a parsed policy document, in the order it states them: "a policy document
/// carries a Statement member", either one statement object or a list of them.
fn held_statements(document: Option<&Value>) -> Vec<Value> {
    match document.and_then(|document| document.get("Statement")) {
        Some(Value::Array(items)) => items.clone(),
        Some(statement @ Value::Object(_)) => vec![statement.clone()],
        _ => Vec::new(),
    }
}

/// The `Sid` one policy statement carries, which is the statement id a grant is addressed by.
fn statement_id_of(statement: &Value) -> Option<&str> {
    statement.get("Sid").and_then(Value::as_str)
}

/// The members of `properties` a call takes under their own names, as that call's request.
fn declared_members(properties: &Value, members: &[&str]) -> Map<String, Value> {
    let mut request = Map::new();
    if let Some(declared) = properties.as_object() {
        for member in members {
            if let Some(value) = declared.get(*member) {
                request.insert((*member).to_string(), value.clone());
            }
        }
    }
    request
}

/// The resource policy an `AWS::Events::EventBus` declaration states, as the JSON text
/// `PutPermission$Policy` takes — the type declaring the document as a template object or as
/// the text of one, which is the spelling rule every policy property reaches its owning
/// service under ([`document_text`]).
fn declared_bus_policy(properties: &Value) -> Option<String> {
    properties
        .get("Policy")
        .and_then(document_text)
        .filter(|document| !document.trim().is_empty())
}

/// The bus an `AWS::Events::EventBusPolicy` declaration names, or `None` for one that names
/// none — which is the account's default bus, the member's own documented default, so nothing
/// is sent for it.
///
/// `PutPermission$EventBusName` takes a bare NAME (`^[\.\-_A-Za-z0-9]+$`), so a declaration
/// that names the bus by its ARN is reduced to the name that ARN carries
/// ([`super::template::event_bus_name`]).
fn declared_policy_bus(properties: &Value) -> Option<String> {
    properties
        .get("EventBusName")
        .and_then(scalar_text)
        .map(|declared| template::event_bus_name(&declared))
        .filter(|bus| !bus.is_empty())
}

/// The statement id an `AWS::Events::EventBusPolicy` declaration states, which identifies the
/// grant it installs.
fn declared_statement_id(properties: &Value) -> Option<String> {
    properties
        .get("StatementId")
        .and_then(scalar_text)
        .filter(|sid| !sid.is_empty())
}

/// The whole policy STATEMENT a declaration states, where it states one: the type declares
/// `Statement` as a template document, which is an object here, or as the JSON text of one.
fn declared_statement(properties: &Value) -> Option<Value> {
    match properties.get("Statement")? {
        Value::String(text) => serde_json::from_str(text).ok(),
        statement @ Value::Object(_) => Some(statement.clone()),
        _ => None,
    }
}
