// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon Bedrock's own control plane: the APPLICATION
//! INFERENCE PROFILE an `AWS::Bedrock::ApplicationInferenceProfile` declares. One file of
//! [`super`]'s per-type arms, dispatched to by its `Provisioner`, beside the guardrail arms
//! of the same control plane ([`super::bedrock_guardrails`]).
//!
//! ## Why the profile is really created
//!
//! An application inference profile is the handle an application invokes a model through
//! when its usage and cost are to be tracked separately: `InvokeModel` and `Converse` take
//! its ARN as their `modelId`, and `GetInferenceProfile`, `ListInferenceProfiles`, the
//! tagging calls and `DeleteInferenceProfile` all address one by its
//! `inferenceProfileIdentifier`. A stack that reported the profile created while nothing
//! created it publishes an identity that resolves to nothing, so the first invocation a
//! client makes through the deployed stack fails.
//!
//! ## The identity is the service's
//!
//! `CreateInferenceProfile` takes NO identifier for the profile — the declaration states the
//! name, the description and the model source it copies from — and answers the
//! `inferenceProfileArn` Amazon Bedrock minted, whose resource segment carries an id of that
//! service's own (`InferenceProfileId`: `^[a-zA-Z0-9-:.]+$`). No name CloudFormation decides
//! is that ARN, so the identity in force is the one that service assigned: the ARN IS this
//! resource's physical id — the type's primary identifier, which its `Ref` returns — and the
//! stand-in the naming rules computed is only what a reference was resolved against until
//! the create answered.
//!
//! ## What the profile reports
//!
//! The create answers the ARN and the profile's status alone, so everything else the type
//! publishes — the `inferenceProfileId` that service minted, the status, the `type` it
//! classified the profile as and the two instants of its clock — is read off that service's
//! own report of the profile the create just made ([`reported_profile`]), which is also where
//! the drift read gets the profile's configuration from. None of it is computed here.
//!
//! ## Why there is no update arm
//!
//! Amazon Bedrock publishes no update of an inference profile at all: the calls on one are
//! its read, its delete and the tagging calls. So every property of the type is
//! create-only ([`crate::state::template::replacement_properties`]) and a changed
//! declaration replaces the resource rather than reaching an arm here.

use super::*;

use crate::state::template::{
    BEDROCK_INFERENCE_PROFILE_ARN_ATTRIBUTE, BEDROCK_INFERENCE_PROFILE_ID_ATTRIBUTE,
};

/// The profile collection `CreateInferenceProfile` is bound to (`POST /inference-profiles`);
/// one profile is addressed under it by its `inferenceProfileIdentifier`
/// (`GET`/`DELETE /inference-profiles/{inferenceProfileIdentifier}`).
const INFERENCE_PROFILES_URI: &str = "/inference-profiles";

/// The members `CreateInferenceProfile` takes this type's two scalar properties as: the name
/// the profile is created under and its description, each the property of the same name with
/// a lower-cased initial.
const PROFILE_MEMBERS: &[(&str, &str)] = &[
    ("InferenceProfileName", "inferenceProfileName"),
    ("Description", "description"),
];

/// The member `CreateInferenceProfile` takes the profile's name in, which is the one member
/// CloudFormation decides when the declaration states none.
const NAME_MEMBER: &str = "inferenceProfileName";

/// The property the model or system-defined profile the new profile copies from is declared
/// in, and the member the create takes it as. Its own member (`CopyFrom`) is the same name
/// with a lower-cased initial, which is what [`camel_cased`] does to it.
const MODEL_SOURCE_PROPERTY: &str = "ModelSource";
const MODEL_SOURCE_MEMBER: &str = "modelSource";

/// The member the declared `Tags` list reaches `CreateInferenceProfile` as — its own `tags`,
/// a `TagList` whose `Tag` spells its members `key` and `value` ([`LOWER_CASED_TAG`]).
const TAGS_MEMBER: &str = "tags";

/// The `@idempotencyToken` the create carries, and how many characters the token this
/// provisioner sends has.
///
/// Derived from the resource's own computed physical id, so the same declaration of the same
/// stack asks for the same profile rather than a second one. DERIVED rather than sent as that
/// id: the member admits at most 256 characters of `[-a-zA-Z0-9]`, which an ARN's `:` and `/`
/// are outside of.
const CLIENT_TOKEN_MEMBER: &str = "clientRequestToken";
const CLIENT_TOKEN_CHARS: usize = 32;

/// The member the created profile's ARN is answered in, which is also the member its own read
/// reports it in.
const PROFILE_ARN_MEMBER: &str = "inferenceProfileArn";

/// The members an answered profile reports, as (attribute, member) — the order
/// [`reported_attributes`] reads them in: the values this type publishes, read off Amazon
/// Bedrock's own report of the profile rather than recomputed here.
///
/// The two instants are `@timestampFormat("date-time")` members, so they arrive as text and
/// reach the attribute as that text ([`reported_attributes`] takes a scalar as it came).
const PROFILE_ATTRIBUTES: &[(&str, &str)] = &[
    (BEDROCK_INFERENCE_PROFILE_ARN_ATTRIBUTE, PROFILE_ARN_MEMBER),
    (BEDROCK_INFERENCE_PROFILE_ID_ATTRIBUTE, "inferenceProfileId"),
    ("Status", "status"),
    ("Type", "type"),
    ("CreatedAt", "createdAt"),
    ("UpdatedAt", "updatedAt"),
];

/// The properties the profile's own read reports back under the same names, for drift
/// detection, as (property, member): the two declared values `GetInferenceProfile` answers.
///
/// The model source is left out — that read reports the `models` the profile resolved rather
/// than the source it was copied from — and so are the tags, which that service's own tag
/// read answers: a property the read does not report is not evidence of anything
/// ([`super::drift`]).
const PROFILE_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("InferenceProfileName", "inferenceProfileName"),
    ("Description", "description"),
];

/// Create the application inference profile an `AWS::Bedrock::ApplicationInferenceProfile`
/// declares, so a client that read the ARN out of the stack's outputs finds the profile in
/// Amazon Bedrock's own `GetInferenceProfile` — and so an invocation addressed to that ARN
/// reaches a profile that exists.
///
/// The identity is Amazon Bedrock's: the ARN it minted becomes this resource's physical id —
/// the primary identifier of the type, and an `inferenceProfileIdentifier` every call on a
/// profile takes — and the id, the status, the type and the instants it reports for the
/// profile are the resource's attributes.
pub(super) async fn create_bedrock_inference_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, PROFILE_MEMBERS, &mut request);
    // A declaration that states no name is deployed under the name CloudFormation computed
    // for it, which is this resource's physical id until that service assigns one.
    if !request.contains_key(NAME_MEMBER) {
        request.insert(
            NAME_MEMBER.to_string(),
            Value::String(resource.physical_resource_id.clone()),
        );
    }
    if let Some(source) = properties
        .get(MODEL_SOURCE_PROPERTY)
        .filter(|value| !value.is_null())
    {
        request.insert(MODEL_SOURCE_MEMBER.to_string(), camel_cased(source, 0)?);
    }
    request.insert(
        CLIENT_TOKEN_MEMBER.to_string(),
        Value::String(client_token(resource)),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(&tags, LOWER_CASED_TAG),
        );
    }
    let answered = call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "POST",
        INFERENCE_PROFILES_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let Some(arn) = reported_text(&answered, PROFILE_ARN_MEMBER) else {
        // There would be no handle a client could invoke the profile through, and nothing
        // for the stack to publish, so this is a failed create rather than a created
        // resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Amazon Bedrock control plane answered a created inference profile \
                      without an ARN"
                .to_string(),
        });
    };
    // What the profile reports beside its ARN is that service's: read off its own report of
    // the profile the create just made, never recomputed here.
    let mut attributes = match reported_profile(ctx, region, &arn).await? {
        Some(profile) => reported_attributes(&profile, PROFILE_ATTRIBUTES),
        None => BTreeMap::new(),
    };
    attributes.insert(
        BEDROCK_INFERENCE_PROFILE_ARN_ATTRIBUTE.to_string(),
        arn.clone(),
    );
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Delete the application inference profile a stack created, by the identity it was created
/// under — this resource's physical id.
///
/// A profile that service no longer holds (`ResourceNotFoundException`, judged on the CODE
/// and never on a message that carries the request's own values) is already in the state the
/// delete asks for.
pub(super) async fn delete_bedrock_inference_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "DELETE",
        &profile_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The profile's configuration as Amazon Bedrock reports it, for drift detection:
/// `GetInferenceProfile`'s own answer, read by the identity that is this resource's physical
/// id ([`PROFILE_REPORTED_PROPERTIES`]). One it no longer holds was deleted, which is what
/// `None` reports.
pub(super) async fn describe_bedrock_inference_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(reported_profile(ctx, region, physical_resource_id)
        .await?
        .map(|profile| reported_as_properties(&profile, PROFILE_REPORTED_PROPERTIES)))
}

/// Amazon Bedrock's own report of ONE inference profile, by the identifier it is addressed
/// by, or `None` where that service holds none.
///
/// `GetInferenceProfile` answers the profile itself rather than a profile inside a member, so
/// the answer IS the report.
async fn reported_profile(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    identifier: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        "GET",
        &profile_uri(identifier),
        None,
    )
    .await
    {
        Ok(answer) => Ok(Some(answer)),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI one profile is addressed at by the read and the delete: the collection plus the
/// `inferenceProfileIdentifier` `@httpLabel`, escaped as a single path segment
/// ([`percent_encoded`]) so an ARN's own `:` and `/` address one profile rather than another
/// path.
fn profile_uri(identifier: &str) -> String {
    format!("{INFERENCE_PROFILES_URI}/{}", percent_encoded(identifier))
}

/// The `@idempotencyToken` the create of this resource carries: [`CLIENT_TOKEN_CHARS`]
/// characters derived from the resource's own computed physical id, which is a function of
/// the stack and the resource's logical id — so the same declaration of the same stack asks
/// for the same profile rather than a second one.
fn client_token(resource: &StoredResource) -> String {
    crate::state::stable_identifier_digits(&resource.physical_resource_id, CLIENT_TOKEN_CHARS)
}
