// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon Bedrock Agent's build-time control plane: the
//! FLOW an `AWS::Bedrock::Flow` declares. One file of [`super`]'s per-type arms, dispatched
//! to by its `Provisioner`, beside the knowledge-base arms of the same owning service
//! ([`super::bedrock_knowledge_bases`]) and the guardrail arms of Amazon Bedrock's own
//! control plane ([`super::bedrock_guardrails`]).
//!
//! ## Why the flow is really created
//!
//! A flow is the one Bedrock resource with a DATA PLANE of its own: `InvokeFlow` runs the
//! flow a `flowIdentifier` names, and a stack is how a client deploys one. The ARN and the
//! id the stack publishes are the handles every later call takes — `GetFlow`, `PrepareFlow`,
//! `CreateFlowVersion`, `CreateFlowAlias`, `UpdateFlow`, `DeleteFlow` and the invocation
//! itself all address a flow by one of them. A stack that reported the flow created while
//! nothing created it publishes handles that resolve to nothing, so the first call a client
//! makes on the deployed stack fails. Creating the flow where it belongs is what makes the
//! identity the stack publishes one the owning service answers for.
//!
//! ## Why the flow is PREPARED
//!
//! A flow is invocable only once it has been prepared: preparation is what packages the
//! `DRAFT` definition into something `InvokeFlow` can run, and `CreateFlow` leaves the flow
//! `NotPrepared`. A deployed flow a client cannot invoke is not the resource the type
//! publishes — the recorded service reports the flow `Prepared` as soon as the stack
//! completes — so this arm asks the owning service to prepare the flow it just created and
//! holds the resource open until that service reports the preparation finished
//! ([`flow_readiness`]).
//!
//! ## The identifier is the service's
//!
//! `CreateFlow` takes the flow's NAME and answers with the `id` it minted (modelled
//! `^[0-9a-zA-Z]{10}$`) and the `flow/<id>` ARN built around it. No name CloudFormation
//! decides, generated or declared, can satisfy that pattern, so the identity in force is the
//! one that service assigned: the ARN IS this resource's physical id — the type's primary
//! identifier, which its `Ref` returns — and the ten-character stand-in the naming rules
//! computed is only what a reference was resolved against until the create answered.
//!
//! ## What the declaration is
//!
//! Every property of this type is the member of the same name with a lower-cased initial all
//! the way down (`ExecutionRoleArn` is `executionRoleArn`, `Definition.Nodes[].Type` is
//! `definition.nodes[].type`), so the declared document IS the request once its member names
//! are re-cased ([`lower_camel_members`]) — the same relation the knowledge-base arms beside
//! this one rely on, except that a flow NODE's `Configuration` is a modelled union rather
//! than a map of caller data, so its members are re-cased too. The declared `Tags` are a MAP
//! for this type, which is the shape `CreateFlow`'s own `tags` member takes, so they travel
//! beside the ones CloudFormation gives the resource ([`tags_of`]).
//!
//! `TestAliasTags` is left out: it tags the flow's TEST ALIAS, not the flow, and is no
//! member of `CreateFlow`. A declaration that states the definition anywhere but in
//! `Definition` is REFUSED rather than created without one ([`DEFINITION_ELSEWHERE`]): a
//! flow created without the definition its template described is not that flow, and its
//! invocation would run nothing.
//!
//! No `clientToken` is sent. That member is an `@idempotencyToken`, so a token derived from
//! the resource would make the create of a REPLACEMENT flow replay the flow the stack
//! already had instead of minting a new one.

use super::*;

/// The flow COLLECTION `CreateFlow` is bound to (`POST /flows/`, trailing slash included, as
/// that operation's own `@http` binding spells it); one flow is addressed under it by its
/// `flowIdentifier` (`GET`/`PUT`/`DELETE /flows/{flowIdentifier}/`).
const FLOWS_URI: &str = "/flows/";

/// The member `CreateFlow` and `UpdateFlow` take the flow's name in, which is the one member
/// CloudFormation decides.
const NAME_MEMBER: &str = "name";

/// The member the declared `Tags` map reaches `CreateFlow` as — its own `tags`, which
/// `UpdateFlow` does not take.
const TAGS_MEMBER: &str = "tags";

/// The property that tags the flow's TEST ALIAS, which is no member of either call: the
/// alias is a resource of its own, and `CreateFlow` carries tags for the flow alone.
const TEST_ALIAS_TAGS_MEMBER: &str = "testAliasTags";

/// The PROPERTIES that state a flow's definition as something other than the `Definition`
/// document — a JSON string, and an Amazon S3 object, each optionally with the
/// `DefinitionSubstitutions` filled into it — none of which `CreateFlow` takes a member for.
const DEFINITION_ELSEWHERE: [&str; 3] = [
    "DefinitionString",
    "DefinitionS3Location",
    "DefinitionSubstitutions",
];

/// The `FlowStatus` a flow is invocable in: "Prepared – The flow is prepared and can be
/// invoked" (`smithy_query(query="shape",shape="FlowStatus")` -> Failed, Prepared, Preparing,
/// NotPrepared).
const FLOW_PREPARED: &str = "Prepared";

/// The transient `FlowStatus` a flow being packaged reports: "Preparing – The flow is being
/// prepared".
const FLOW_PREPARING: &str = "Preparing";

/// The members an answered flow reports, as (member, attribute): the values this type
/// publishes as return values, read off the answer rather than recomputed here.
///
/// `validations` is not among them — it is a LIST, and the attribute map is string-valued,
/// so reporting one entry as the whole of it would be worse than reporting none.
const FLOW_ATTRIBUTES: &[(&str, &str)] = &[
    ("arn", "Arn"),
    ("id", "Id"),
    ("status", "Status"),
    ("createdAt", "CreatedAt"),
    ("updatedAt", "UpdatedAt"),
    ("version", "Version"),
];

/// The properties a flow's own read reports back under the same names, for drift detection,
/// as (property, member): the scalars `GetFlow` answers that a declaration also states.
///
/// The definition document is left out: what the read reports are that service's own member
/// names, and a property the read does not report is not evidence of anything
/// ([`super::drift`]).
const FLOW_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("Name", "name"),
    ("Description", "description"),
    ("ExecutionRoleArn", "executionRoleArn"),
    ("CustomerEncryptionKeyArn", "customerEncryptionKeyArn"),
];

/// Create the flow an `AWS::Bedrock::Flow` declares, so a client that deployed the stack and
/// calls `GetFlow` with the identity the stack publishes reads the flow back — and so every
/// hop off it (preparing it, cutting a version, aliasing one, invoking the flow through the
/// runtime data plane) addresses a flow that exists.
///
/// The identity is Amazon Bedrock Agent's: the ARN it minted becomes this resource's physical
/// id — the primary identifier of the type — and the ARN, the id, the status, the version and
/// the two instants it answered are the resource's attributes.
pub(super) async fn create_bedrock_flow(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = declared_flow_members(&resource.properties)?;
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(flow_name(resource)),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&tags)));
    }
    let answered = call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "POST",
        FLOWS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let attributes = flow_attributes(&answered)?;
    let Some(flow_identifier) = attributes.get("Arn").cloned() else {
        return Err(no_flow_answered());
    };
    // The create leaves the flow `NotPrepared`, in which no invocation runs it, so the
    // preparation that makes it invocable is asked for here and waited out by
    // [`flow_readiness`]: what the stack publishes is a flow a client can invoke.
    prepare_flow(ctx, region, &flow_identifier).await?;
    Ok(Created {
        // Amazon Bedrock Agent assigned the flow's identity, so the ARN it answered is what
        // `Ref` resolves to and what every call on the flow can be addressed by.
        physical_resource_id: Some(flow_identifier),
        attributes,
    })
}

/// Ask the owning service to prepare the flow the stack created, through `PrepareFlow` — the
/// one call that turns a stored definition into something the runtime plane can invoke.
///
/// It answers `202 Accepted` with the flow `Preparing`, so the outcome is read off the flow
/// itself ([`flow_readiness`]) rather than from this answer.
async fn prepare_flow(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    flow_identifier: &str,
) -> Result<(), Refusal> {
    call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "POST",
        &flow_uri(flow_identifier),
        None,
    )
    .await
    .map(|_| ())
}

/// Whether Amazon Bedrock Agent has finished preparing the flow a stack created.
///
/// `PrepareFlow` answers with the flow `Preparing` — in which it cannot be invoked — so
/// `Prepared` is the state the resource's creation is finished in, `Preparing` the state it is
/// still coming up in, and every other status is one no wait will bring it out of: a flow
/// whose preparation failed, and one left `NotPrepared` because nothing prepared it. A flow
/// that service no longer holds is in the same position, nothing being left to create it. The
/// findings it publishes for a failure are the diagnosis of one, so they travel with it.
pub(super) async fn flow_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(flow) = read_flow(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon Bedrock holds no flow {physical_resource_id}"
        )));
    };
    let status = reported_text(&flow, "status").unwrap_or_default();
    if status == FLOW_PREPARED {
        return Ok(Readiness::Ready);
    }
    if status == FLOW_PREPARING {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(flow_failed_reason(
        physical_resource_id,
        &status,
        &flow,
    )))
}

/// The attributes the owning service publishes for a flow it now reports prepared: its own
/// read of the flow, whose `status` and `updatedAt` are not the ones the create answered with
/// ([`FLOW_ATTRIBUTES`]).
pub(super) async fn flow_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    Ok(read_flow(ctx, region, physical_resource_id)
        .await?
        .map(|flow| reported_flow_attributes(&flow))
        .unwrap_or_default())
}

/// Why a flow this stack created cannot come up: the status that service reports it in, with
/// the validation findings it publishes for it where there are any (`Flow$validations`:
/// "a list of validation error messages related to the last failed operation on the flow").
fn flow_failed_reason(flow_identifier: &str, status: &str, flow: &Value) -> String {
    let findings: Vec<String> = flow
        .get("validations")
        .and_then(Value::as_array)
        .map(|findings| {
            findings
                .iter()
                .filter_map(|finding| reported_text(finding, "message"))
                .collect()
        })
        .unwrap_or_default();
    let reported = format!("Amazon Bedrock reports flow {flow_identifier} as {status}");
    if findings.is_empty() {
        return reported;
    }
    format!("{reported}: {}", findings.join("; "))
}

/// Apply a changed declaration to the flow the stack created, through `UpdateFlow` — the one
/// call that changes a flow that exists.
///
/// It takes the flow's identity together with the whole declared configuration, so a member
/// the declaration no longer states leaves the flow without it rather than keeping the one an
/// earlier deployment gave it. The flow keeps the ARN and the id that service minted, so the
/// handles the stack publishes are unchanged.
///
/// `Ok(None)` is a change that call cannot make, so the deployment reports it as not applied
/// rather than as a success that dropped it: a changed `Tags` map, which `UpdateFlow` does
/// not carry — a flow's tags are that service's own tagging calls.
pub(super) async fn update_bedrock_flow(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if declared.get("Tags") != resource.properties.get("Tags") {
        return Ok(None);
    }
    let mut request = declared_flow_members(declared)?;
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(declared_flow_name_or(declared, resource)),
    );
    let answered = call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "PUT",
        &flow_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    // `UpdateFlow` leaves the flow carrying the new definition and NOT prepared, so the
    // preparation is asked for again for the same reason the create asks for it: what the
    // stack publishes is a flow a client can invoke, and the deployment waits the preparation
    // out ([`flow_readiness`]).
    prepare_flow(ctx, region, &resource.physical_resource_id).await?;
    Ok(Some(Created::attributes(flow_attributes(&answered)?)))
}

/// Delete the flow a stack created, by the identity it was created under — this resource's
/// physical id. One the service no longer holds (`ResourceNotFoundException`, judged on the
/// CODE and never on a message that carries the request's own values) is already in the state
/// the delete asks for.
pub(super) async fn delete_bedrock_flow(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "DELETE",
        &flow_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The flow's configuration as Amazon Bedrock Agent reports it, for drift detection:
/// `GetFlow`'s own answer, read by the identity that is this resource's physical id
/// ([`FLOW_REPORTED_PROPERTIES`]). One it no longer holds was deleted.
pub(super) async fn describe_bedrock_flow(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_flow(ctx, region, physical_resource_id)
        .await?
        .map(|flow| reported_as_properties(&flow, FLOW_REPORTED_PROPERTIES)))
}

/// The flow Amazon Bedrock Agent holds under `flow_identifier`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_flow(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    flow_identifier: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "GET",
        &flow_uri(flow_identifier),
        None,
    )
    .await
    {
        // `GetFlow` answers the flow itself, not a flow inside a member.
        Ok(answer) => Ok(Some(answer)),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI one flow is addressed at: the collection plus the `flowIdentifier` `@httpLabel`
/// and the trailing slash those bindings carry, the identifier escaped as a single path
/// segment ([`percent_encoded`]) so a value carrying URI structure addresses one flow rather
/// than another path.
fn flow_uri(flow_identifier: &str) -> String {
    format!("{FLOWS_URI}{}/", percent_encoded(flow_identifier))
}

/// One declared property document as the request members Amazon Bedrock Agent takes: every
/// member name re-cased ([`lower_camel_members`], which re-cases the members of a node's
/// `Configuration` union too), with the tags left out — the create adds them as the map that
/// call takes, and the update does not take them at all — so one reader serves both.
///
/// A declaration that carries its definition anywhere but in `Definition` is refused here:
/// the properties that state it otherwise are no members of either call, so a flow created
/// from such a declaration would carry no definition at all.
fn declared_flow_members(declared: &Value) -> Result<Map<String, Value>, Refusal> {
    for property in DEFINITION_ELSEWHERE {
        if declared.get(property).is_some_and(|stated| !stated.is_null()) {
            return Err(definition_elsewhere(property));
        }
    }
    let mut request = match lower_camel_members(declared, 0)? {
        Value::Object(members) => members,
        // A declaration that is not an object declares no member of the request.
        _ => Map::new(),
    };
    request.remove(TAGS_MEMBER);
    request.remove(TEST_ALIAS_TAGS_MEMBER);
    // A flow whose `definition` is absent altogether is the owning service's own decision:
    // that member is optional on both calls, so the request carries whatever the declaration
    // states and that service answers for the rest.
    Ok(request)
}

/// The refusal a declaration whose definition this emulator cannot assemble earns: the
/// property that states it, and that the flow was left uncreated rather than created without
/// the definition the template described.
fn definition_elsewhere(property: &str) -> Refusal {
    Refusal {
        code: NOT_IMPLEMENTED.to_string(),
        message: format!(
            "this emulator creates the flow of an {BEDROCK_FLOW} from the Definition the \
             declaration states; it does not assemble one from {property}, so nothing was \
             created."
        ),
    }
}

/// The NAME the flow is created under: the `Name` the template asked for, or the
/// `<stack>-<logical id>-<suffix>` name CloudFormation generated for a declaration that
/// states none — which is this resource's physical id until that service assigns one.
fn flow_name(resource: &StoredResource) -> String {
    declared_flow_name_or(&resource.properties, resource)
}

/// The name one declared document states, or the one CloudFormation decided for the resource
/// ([`flow_name`]).
///
/// The fallback is the resource's own `Name` attribute where the record carries one — the
/// physical id being the ARN that service minted once the create has answered — and the
/// physical id itself before then.
fn declared_flow_name_or(declared: &Value, resource: &StoredResource) -> String {
    if let Some(name) = declared
        .get("Name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        return name.to_string();
    }
    resource
        .attributes
        .get("Name")
        .cloned()
        .unwrap_or_else(|| resource.physical_resource_id.clone())
}

/// The attributes one answered flow reports ([`FLOW_ATTRIBUTES`]).
///
/// An answer carrying no ARN is reported as a refusal rather than recorded as a created
/// resource: there would be no handle a client could call Amazon Bedrock Agent with.
fn flow_attributes(answered: &Value) -> Result<BTreeMap<String, String>, Refusal> {
    let attributes = reported_flow_attributes(answered);
    if !attributes.contains_key("Arn") {
        return Err(no_flow_answered());
    }
    Ok(attributes)
}

/// The refusal an answer carrying no identified flow is reported as.
fn no_flow_answered() -> Refusal {
    Refusal {
        code: "InternalFailure".to_string(),
        message: "Amazon Bedrock Agent answered a flow without an ARN".to_string(),
    }
}

/// The attributes one reported flow structure carries ([`FLOW_ATTRIBUTES`]).
fn reported_flow_attributes(flow: &Value) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    for (member, attribute) in FLOW_ATTRIBUTES {
        if let Some(value) = reported_text(flow, member) {
            attributes.insert((*attribute).to_string(), value);
        }
    }
    attributes
}
