// SPDX-License-Identifier: CC0-1.0

//! The two `AWS::StepFunctions::*` types a stack declares BESIDE a state machine — the
//! VERSION it publishes of one and the ALIAS that routes traffic to versions — created in
//! AWS Step Functions itself.
//!
//! This is the per-type half of provisioning for two resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::step_functions_state_machine`] for the reason that
//! module records, and is reached through the same seam ([`super::Owner`]).
//!
//! A client that deployed the stack and then calls `sfn list-state-machine-versions` reads
//! the version the stack published, and an execution started against the alias the stack
//! declared is routed by the weights the template stated — so a stack is not reported as
//! deployed while the version and the alias at the heart of a deployment exist nowhere.
//!
//! ## Step Functions decides whether either can exist
//!
//! Both creates are that service's own calls, so both are refused by that service's own
//! rules: `PublishStateMachineVersion` refuses a machine it does not hold and a `revisionId`
//! that is not the machine's current revision, and `CreateStateMachineAlias` refuses a
//! routing configuration whose entries name versions of different machines, name a version
//! nothing published, or carry weights that do not add up to the total that service requires.
//! Each refusal is the resource's failure — nothing was created — so a deployment declaring
//! one fails the resource and the stack rather than reporting a deploy nothing carried out.
//! Which is why this module exists at all: it is Step Functions, not CloudFormation, that
//! knows which versions a machine has and which routing configurations it will take.
//!
//! ## The identifiers are Step Functions'
//!
//! "Ref returns the ARN of the state machine version" and "Ref returns the ARN of the state
//! machine alias", and neither ARN can be computed from the template: a version's carries the
//! NUMBER that service allocates ("the ARN of the state machine version"), and an alias's is
//! built on the machine the routing configuration's versions belong to, which the declaration
//! names only through those versions. So each create reports the ARN the call ANSWERED as the
//! resource's physical id and as its `Arn` attribute, and [`super::reconcile`] re-points every
//! reference to the resource at it ([`super::repoint_references`]) — which is how the
//! `RoutingConfiguration` of an alias whose `StateMachineVersionArn` is `Fn::GetAtt` of a
//! version resource reaches Step Functions carrying the version that service really published.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    call_json_rpc, lower_camel, modelled_declaration, Created, ModelledScalar, Readiness,
    Refusal, Removal, AWS_JSON_1_0,
};
use crate::state::StoredResource;

/// The published VERSION of a state machine.
pub(in crate::state) const STEP_FUNCTIONS_STATE_MACHINE_VERSION: &str =
    "AWS::StepFunctions::StateMachineVersion";

/// The ALIAS that routes traffic to one or two versions of a state machine.
pub(in crate::state) const STEP_FUNCTIONS_STATE_MACHINE_ALIAS: &str =
    "AWS::StepFunctions::StateMachineAlias";

/// AWS Step Functions' signing name and `X-Amz-Target` prefix, from its model
/// (`sigv4Name: states`, service shape `AWSStepFunctions`), which speaks `awsJson1_0`.
const SFN_SIGNING_NAME: &str = "states";
/// See [`SFN_SIGNING_NAME`].
const SFN_TARGET_PREFIX: &str = "AWSStepFunctions";

/// The error code Step Functions answers for an alias nothing answers to (`ResourceNotFound`,
/// httpError 404), which is what says a delete has nothing left to remove.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFound";

/// The error code it answers for a state machine ARN nothing answers to
/// (`StateMachineDoesNotExist`), which says the same about a version of a machine that is
/// already gone.
const STATE_MACHINE_DOES_NOT_EXIST: &str = "StateMachineDoesNotExist";

/// Which of the two types a resource is. Travels with the owner, so the algorithm resolves
/// the type once and every call below is about that same resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::StepFunctions::StateMachineVersion`.
    Version,
    /// `AWS::StepFunctions::StateMachineAlias`.
    Alias,
}

/// Which of these types `resource_type` is, or `None` for any other type.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        STEP_FUNCTIONS_STATE_MACHINE_VERSION => Some(Kind::Version),
        STEP_FUNCTIONS_STATE_MACHINE_ALIAS => Some(Kind::Alias),
        _ => None,
    }
}

/// The property of a version naming the state machine it is published FROM, which that type
/// requires ("StateMachineArn … Required: Yes").
const STATE_MACHINE_ARN_PROPERTY: &str = "StateMachineArn";

/// The property stating WHICH revision of that machine the version may be published from,
/// which is `PublishStateMachineVersion`'s own `revisionId`.
const STATE_MACHINE_REVISION_ID_PROPERTY: &str = "StateMachineRevisionId";

/// The property every description is declared under, which is a member of the version's
/// publish and of the alias's create under the same name, lower-cased.
const DESCRIPTION_PROPERTY: &str = "Description";

/// The property naming the alias itself, which is `CreateStateMachineAlias`' required `name`.
const NAME_PROPERTY: &str = "Name";

/// The property stating where an alias sends traffic, which is that call's required
/// `routingConfiguration`.
const ROUTING_CONFIGURATION_PROPERTY: &str = "RoutingConfiguration";

/// The member of one routing-configuration entry that the owning model types as a NUMBER
/// ("Weight … the percentage of traffic you want to route to a state machine version"), so a
/// template that quoted it reaches that service as the integer it models.
const WEIGHT_MEMBER: &str = "Weight";

/// Every property `AWS::StepFunctions::StateMachineVersion` publishes. A declared property
/// this list does not carry is REFUSED rather than dropped, so a version is never reported
/// published with less than its declaration asked for.
///
/// Source: the type's own property set, which is `PublishStateMachineVersion`'s member list
/// (`smithy_query(query="operation",operation="PublishStateMachineVersion")`) with that call's
/// `revisionId` under the type's own name for it.
const VERSION_PROPERTIES: &[&str] = &[
    DESCRIPTION_PROPERTY,
    STATE_MACHINE_ARN_PROPERTY,
    STATE_MACHINE_REVISION_ID_PROPERTY,
];

/// Every property `AWS::StepFunctions::StateMachineAlias` this module applies: the members of
/// `CreateStateMachineAlias` (`smithy_query(query="operation",operation=\
/// "CreateStateMachineAlias")`) under the type's own spelling of each.
///
/// `DeploymentPreference` is deliberately NOT one of them: it is a GRADUAL deployment, which
/// shifts an alias's traffic over an interval and rolls it back on an alarm, and no call of
/// that service carries one — so a declaration stating it is refused rather than reported as
/// created with a rollout nothing is performing.
const ALIAS_PROPERTIES: &[&str] = &[
    DESCRIPTION_PROPERTY,
    NAME_PROPERTY,
    ROUTING_CONFIGURATION_PROPERTY,
];

/// Publish the version — or create the alias — one of these resources declares, in AWS Step
/// Functions itself.
///
/// `Err` is that service's own refusal of the call, which is what the resource fails with.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(kind, &resource.resource_type, &resource.properties)?;
    match kind {
        Kind::Version => create_version(ctx, region, resource).await,
        Kind::Alias => create_alias(ctx, region, resource).await,
    }
}

/// A version exists, and an alias routes traffic, from the moment its own create answers:
/// `DescribeStateMachineAlias` answers for the alias and an execution may be started against
/// either ARN from then on, Step Functions reporting no state either is still coming up in.
pub(super) fn ready() -> Readiness {
    Readiness::Ready
}

/// `DeleteStateMachineVersion` and `DeleteStateMachineAlias` each take the resource away as
/// they answer, so there is nothing to wait for.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has.
///
/// An alias is changed by `UpdateStateMachineAlias`, the call a client makes for the same
/// change: it takes the alias's `description` and its `routingConfiguration`, so a declaration
/// that re-points the alias at other versions or at other weights reaches the alias in place.
/// Its NAME is its identity, and a declaration that states a different one is a different
/// resource: [`crate::state::template::replacing_properties`] names it, so the deployment
/// replaces the alias rather than reaching here.
///
/// A published VERSION cannot be changed at all — it is the snapshot the publish took, and
/// Step Functions publishes no call that edits one — so every property of that type is a
/// replacement property and a change to one is deployed as a new version rather than reaching
/// here.
///
/// `Ok(None)` is left for a change neither call can make, so the deployment reports it as not
/// applied rather than as a success that dropped it: a declaration that DROPS the alias's
/// description, which `UpdateStateMachineAlias` leaves as it was — that call carries no member
/// that resets one.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unmapped_properties(kind, &resource.resource_type, declared)?;
    let Kind::Alias = kind else {
        return Ok(None);
    };
    let mut request = Map::new();
    request.insert(
        "stateMachineAliasArn".to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    match text_property(declared, DESCRIPTION_PROPERTY) {
        Some(description) => {
            request.insert("description".to_string(), Value::String(description));
        }
        // A description the alias HAD and this declaration no longer states is a change no
        // member of this call makes.
        None if text_property(&resource.properties, DESCRIPTION_PROPERTY).is_some() => {
            return Ok(None)
        }
        None => {}
    }
    if let Some(routing) = property(declared, ROUTING_CONFIGURATION_PROPERTY) {
        request.insert("routingConfiguration".to_string(), sent_routing(routing));
    }
    // "Requests with the same … parameters are considered equivalent", and the call requires
    // at least one of the two members it carries: a declaration that changed neither has
    // nothing for this call to apply, so nothing is asked of that service.
    if request.len() == 1 {
        return Ok(Some(Created::default()));
    }
    call_sfn(ctx, region, "UpdateStateMachineAlias", &Value::Object(request)).await?;
    // The alias keeps its ARN across an update, so the handle the stack publishes — and every
    // attribute read off it — is unchanged.
    Ok(Some(Created::default()))
}

/// Delete the version a stack published, or the alias it created, addressed by the ARN its own
/// create answered — this resource's physical id, which is what `Ref` returns for the type.
///
/// One Step Functions no longer holds is already in the state the delete asks for, judged on
/// the CODE and never on a message. A physical id that is NO such ARN names nothing to remove
/// — the stand-in a resource whose create never answered still carries — so nothing is asked
/// of that service for it.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let arn = &resource.physical_resource_id;
    if !arn.starts_with(ARN_PREFIX) {
        return Ok(());
    }
    let (operation, member) = match kind {
        Kind::Version => ("DeleteStateMachineVersion", "stateMachineVersionArn"),
        Kind::Alias => ("DeleteStateMachineAlias", "stateMachineAliasArn"),
    };
    let request = Value::Object(
        [(member.to_string(), Value::String(arn.clone()))]
            .into_iter()
            .collect(),
    );
    match call_sfn(ctx, region, operation, &request).await {
        Ok(_) => Ok(()),
        Err(refusal)
            if refusal.code == RESOURCE_NOT_FOUND
                || refusal.code == STATE_MACHINE_DOES_NOT_EXIST =>
        {
            Ok(())
        }
        Err(refusal) => Err(refusal),
    }
}

/// The alias's configuration as Step Functions reports it, for drift detection. One it no
/// longer holds was deleted.
///
/// The properties its read reports are the ones this type declares under a name
/// `DescribeStateMachineAlias` answers for AND that the declaration states as they are stored:
/// the alias's `Name` and its `Description`. The ROUTING CONFIGURATION is not among them, and
/// a property the read does not report is not evidence of anything
/// ([`super::super::drift`]): a declaration states the versions it routes to through
/// references to the version resources of the same stack, which the deployment re-points at
/// the ARNs Step Functions minted ([`super::repoint_references`]), so comparing that read
/// against the declaration as it was written would report every stack-published alias as
/// drifted when nothing drifted.
pub(super) async fn alias_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let arn = &resource.physical_resource_id;
    if !arn.starts_with(ARN_PREFIX) {
        return Ok(None);
    }
    let request = serde_json::json!({ "stateMachineAliasArn": arn });
    let answered = match call_sfn(ctx, region, "DescribeStateMachineAlias", &request).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == RESOURCE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let mut reported = Map::new();
    for (property, member) in [(NAME_PROPERTY, "name"), (DESCRIPTION_PROPERTY, "description")] {
        if let Some(value) = answered.get(member).filter(|value| !value.is_null()) {
            reported.insert(property.to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(reported)))
}

/// What every ARN opens with, which is what says a physical id IS one of that service's own
/// identifiers rather than the stand-in a resource carries before its create answers.
const ARN_PREFIX: &str = "arn:";

/// `PublishStateMachineVersion` for the version a declaration states.
async fn create_version(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let mut request = Map::new();
    request.insert(
        "stateMachineArn".to_string(),
        Value::String(state_machine_arn(resource)?),
    );
    if let Some(description) = text_property(&resource.properties, DESCRIPTION_PROPERTY) {
        request.insert("description".to_string(), Value::String(description));
    }
    if let Some(revision) =
        text_property(&resource.properties, STATE_MACHINE_REVISION_ID_PROPERTY)
    {
        request.insert("revisionId".to_string(), Value::String(revision));
    }
    let answered =
        call_sfn(ctx, region, "PublishStateMachineVersion", &Value::Object(request)).await?;
    let arn = answered_arn(&answered, "stateMachineVersionArn")?;
    Ok(created_with(&resource.resource_type, arn))
}

/// `CreateStateMachineAlias` for the alias a declaration states.
async fn create_alias(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let mut request = Map::new();
    // The alias's name is CloudFormation's to decide where the declaration states none: the
    // name the naming rules computed for the resource is the one it is created under, read off
    // the record the same way every other arm in this position reads it.
    request.insert(
        "name".to_string(),
        Value::String(crate::state::provisioner::declared_name(resource)),
    );
    if let Some(description) = text_property(&resource.properties, DESCRIPTION_PROPERTY) {
        request.insert("description".to_string(), Value::String(description));
    }
    // Sent whether or not the declaration states one: the member is required, so an alias
    // declared without a routing configuration is refused by that service's own validation
    // rather than by a rule invented here.
    let routing = property(&resource.properties, ROUTING_CONFIGURATION_PROPERTY)
        .map_or_else(|| Value::Array(Vec::new()), sent_routing);
    request.insert("routingConfiguration".to_string(), routing);
    let answered =
        call_sfn(ctx, region, "CreateStateMachineAlias", &Value::Object(request)).await?;
    let arn = answered_arn(&answered, "stateMachineAliasArn")?;
    Ok(created_with(&resource.resource_type, arn))
}

/// What a create of either type reports: the ARN that service minted as the resource's
/// physical id, and that same ARN as every `Arn` name the type publishes
/// ([`super::super::template::arn_attributes`]).
fn created_with(resource_type: &str, arn: String) -> Created {
    let attributes: BTreeMap<String, String> =
        super::super::template::arn_attributes(resource_type, &arn)
            .into_iter()
            .collect();
    Created {
        physical_resource_id: Some(arn),
        attributes,
    }
}

/// The ARN one of these creates answered with, read off the member that call reports it in.
fn answered_arn(answered: &Value, member: &str) -> Result<String, Refusal> {
    answered
        .get(member)
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| Refusal {
            code: "InternalFailure".to_string(),
            message: format!(
                "AWS Step Functions answered without a {member}, which is what a reference to \
                 this resource resolves to"
            ),
        })
}

/// The state machine the version is published from, as the declaration names it.
///
/// A declaration carrying no `StateMachineArn` names no machine to publish from and is refused
/// rather than sent to a call that cannot be addressed.
fn state_machine_arn(resource: &StoredResource) -> Result<String, Refusal> {
    text_property(&resource.properties, STATE_MACHINE_ARN_PROPERTY).ok_or_else(|| Refusal {
        code: "InvalidRequest".to_string(),
        message: format!(
            "{} requires the {STATE_MACHINE_ARN_PROPERTY} property",
            resource.resource_type
        ),
    })
}

/// One declared `RoutingConfiguration` as `routingConfiguration` travels: every member name of
/// every entry in the spelling the API models it under, and each `Weight` read as the number
/// that model types it as ([`modelled_declaration`]).
///
/// A value that is no list of objects is sent as the declaration wrote it: that service's own
/// validation is what judges it, so nothing is refused here that it would word better.
fn sent_routing(declared: &Value) -> Value {
    let Some(entries) = declared.as_array() else {
        return declared.clone();
    };
    Value::Array(
        entries
            .iter()
            .map(|entry| match entry.as_object() {
                Some(members) => Value::Object(
                    members
                        .iter()
                        .map(|(name, value)| {
                            (
                                lower_camel(name),
                                modelled_declaration(name, value, &modelled_routing_member),
                            )
                        })
                        .collect(),
                ),
                None => entry.clone(),
            })
            .collect(),
    )
}

/// What the owning model types one member of a routing-configuration entry as: the weight is
/// an integer (`RoutingConfigurationListItem$weight`), and the version ARN beside it is text.
fn modelled_routing_member(member: &str) -> ModelledScalar {
    if member == WEIGHT_MEMBER {
        ModelledScalar::Number
    } else {
        ModelledScalar::AsWritten
    }
}

/// One declared property, as the template evaluated it.
fn property<'a>(properties: &'a Value, name: &str) -> Option<&'a Value> {
    properties.get(name).filter(|value| !value.is_null())
}

/// One declared property read as TEXT, absent where the declaration states it as nothing.
fn text_property(properties: &Value, name: &str) -> Option<String> {
    property(properties, name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a version from being reported as published — and an alias from
/// being reported as created — with less than its declaration asked for.
fn refuse_unmapped_properties(
    kind: Kind,
    resource_type: &str,
    properties: &Value,
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    let applied = match kind {
        Kind::Version => VERSION_PROPERTIES,
        Kind::Alias => ALIAS_PROPERTIES,
    };
    for property in properties.keys() {
        if applied.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: "NotImplemented".to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        });
    }
    Ok(())
}

/// Call one AWS Step Functions operation on the awsJson1_0 wire its model declares.
async fn call_sfn(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        SFN_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{SFN_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource record of one of these types, as the naming rules leave one.
    fn declared(resource_type: &str, physical_resource_id: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "MyVersion".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: physical_resource_id.to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// Each type's own `kind_of` answer, and nothing for another type.
    #[test]
    fn each_type_resolves_to_its_own_kind() {
        let cases: &[(&str, Option<Kind>)] = &[
            (STEP_FUNCTIONS_STATE_MACHINE_VERSION, Some(Kind::Version)),
            (STEP_FUNCTIONS_STATE_MACHINE_ALIAS, Some(Kind::Alias)),
            ("AWS::StepFunctions::StateMachine", None),
            ("AWS::NotAService::NotAType", None),
        ];
        for (resource_type, expected) in cases {
            let resolved = kind_of(resource_type);
            assert_eq!(
                resolved, *expected,
                "case {resource_type:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }

    /// A declaration naming no state machine is refused rather than sent to a call that
    /// cannot be addressed, and one naming a machine is addressed by it.
    #[test]
    fn the_declared_state_machine_addresses_the_publish() {
        let machine = "arn:aws:states:us-west-2:000000000000:stateMachine:orders";
        let cases: &[(&str, Value, Option<&str>)] = &[
            (
                "a machine named by ARN",
                serde_json::json!({ "StateMachineArn": machine }),
                Some(machine),
            ),
            (
                "no machine at all",
                serde_json::json!({ "Description": "the first cut" }),
                None,
            ),
            (
                "a machine stated as nothing",
                serde_json::json!({ "StateMachineArn": "" }),
                None,
            ),
        ];
        for (case, properties, expected) in cases {
            let resource = declared(
                STEP_FUNCTIONS_STATE_MACHINE_VERSION,
                "stack-MyVersion-ABC123",
                properties.clone(),
            );
            let named = state_machine_arn(&resource);
            match expected {
                Some(expected) => assert_eq!(
                    named.as_deref().ok(),
                    Some(*expected),
                    "case {case:?}: expected the publish to address {expected:?}, got {named:?}"
                ),
                None => assert!(
                    named.is_err(),
                    "case {case:?}: expected a refusal naming the required property, got \
                     {named:?}"
                ),
            }
        }
    }

    /// A routing configuration travels as the API models it: every member name lower-camelled,
    /// and every weight the number the model types it as however the template wrote it.
    #[test]
    fn a_routing_configuration_travels_as_the_api_models_it() {
        let version = "arn:aws:states:us-west-2:000000000000:stateMachine:orders:2";
        let cases: &[(&str, Value, Value)] = &[
            (
                "weights written as numbers",
                serde_json::json!([{ "StateMachineVersionArn": version, "Weight": 100 }]),
                serde_json::json!([{ "stateMachineVersionArn": version, "weight": 100 }]),
            ),
            (
                "a weight a template quoted",
                serde_json::json!([{ "StateMachineVersionArn": version, "Weight": "60" }]),
                serde_json::json!([{ "stateMachineVersionArn": version, "weight": 60 }]),
            ),
            (
                "two entries, as the model's length maximum allows",
                serde_json::json!([
                    { "StateMachineVersionArn": version, "Weight": 70 },
                    { "StateMachineVersionArn": version, "Weight": 30 },
                ]),
                serde_json::json!([
                    { "stateMachineVersionArn": version, "weight": 70 },
                    { "stateMachineVersionArn": version, "weight": 30 },
                ]),
            ),
            (
                "a value that is no list at all",
                serde_json::json!("not a routing configuration"),
                serde_json::json!("not a routing configuration"),
            ),
        ];
        for (case, declared_routing, expected) in cases {
            let sent = sent_routing(declared_routing);
            assert_eq!(
                sent, *expected,
                "case {case:?}: expected {expected}, got {sent}"
            );
        }
    }

    /// A property neither type applies is refused rather than dropped — a gradual deployment
    /// among them, which no call of that service carries.
    #[test]
    fn an_unapplied_property_is_refused() {
        let cases: &[(&str, Kind, &str, Value)] = &[
            (
                "a gradual deployment on an alias",
                Kind::Alias,
                STEP_FUNCTIONS_STATE_MACHINE_ALIAS,
                serde_json::json!({ "Name": "live", "DeploymentPreference": { "Type": "LINEAR" } }),
            ),
            (
                "a property of another type on a version",
                Kind::Version,
                STEP_FUNCTIONS_STATE_MACHINE_VERSION,
                serde_json::json!({ "StateMachineArn": "arn:aws:states:::sm", "Tags": [] }),
            ),
        ];
        for (case, kind, resource_type, properties) in cases {
            let refused = refuse_unmapped_properties(*kind, resource_type, properties);
            assert!(
                refused.is_err(),
                "case {case:?}: expected a refusal naming the unapplied property, got {refused:?}"
            );
        }
        // ...and a declaration stating only applied properties is not refused.
        let applied = serde_json::json!({
            "Name": "live",
            "Description": "the live alias",
            "RoutingConfiguration": [],
        });
        let accepted = refuse_unmapped_properties(
            Kind::Alias,
            STEP_FUNCTIONS_STATE_MACHINE_ALIAS,
            &applied,
        );
        assert!(
            accepted.is_ok(),
            "case \"every property applied\": expected no refusal, got {accepted:?}"
        );
    }
}
