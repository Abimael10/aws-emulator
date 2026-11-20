// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Bedrock Agent RUNTIME plane: the SESSION an
//! `AWS::Bedrock::Session` declares. One file of [`super`]'s per-type arms, dispatched to by
//! its `Provisioner`, beside the build-time plane's flow and knowledge-base arms
//! ([`super::bedrock_flows`], [`super::bedrock_knowledge_bases`]) and the guardrail arms of
//! Amazon Bedrock's own control plane ([`super::bedrock_guardrails`]).
//!
//! ## Why the session is really created
//!
//! A session is the store a generative-AI application checkpoints a conversation into:
//! `CreateInvocation`, `PutInvocationStep`, `GetInvocationStep` and `ListInvocations` all
//! address one by its `sessionIdentifier`, as do `GetSession`, `UpdateSession`, `EndSession`
//! and `DeleteSession`. A stack that reported the session created while nothing created it
//! publishes an identity that resolves to nothing, so the first call a client makes on the
//! deployed stack fails. Minting the session where it belongs is what makes the identity the
//! stack publishes one the owning service answers for.
//!
//! ## The identifier is the service's
//!
//! `CreateSession` takes NO identifier at all — the declaration states the encryption key,
//! the metadata and the tags — and answers the `sessionId` that service drew (modelled as a
//! `Uuid`) with the `session/<uuid>` ARN built around it. No name CloudFormation decides
//! satisfies either form, so the identity in force is the one that service assigned: the ARN
//! IS this resource's physical id — the type's primary identifier, which its `Ref` returns —
//! and the uuid stand-in the naming rules computed is only what a reference was resolved
//! against until the create answered.
//!
//! ## What the declaration is
//!
//! Three properties, each the `CreateSession` member of the same name with a lower-cased
//! initial: the key the session's data is encrypted with, the metadata map to persist across
//! it, and the tags. They are mapped one by one rather than re-cased in bulk, because
//! `SessionMetadata` is a map of the CALLER's own keys — re-casing those would store metadata
//! the declaration did not describe. The declared `Tags` are a MAP for this type, which is
//! the shape `CreateSession`'s own `tags` member takes, so they travel beside the ones
//! CloudFormation gives the resource ([`tags_of`]).
//!
//! ## Why the delete ENDS the session first
//!
//! `DeleteSession` is documented "Deletes a session that you ended. You can't delete a
//! session with an `ACTIVE` status. To delete an active session, you must first end it with
//! the `EndSession` API operation" — and a created session is `ACTIVE`. So the delete makes
//! the pair of calls a client makes: it ends the session, then deletes it. A stack whose
//! delete skipped the first call would leave the session behind and report the resource
//! deleted.

use super::*;

use crate::state::template::{BEDROCK_SESSION_ARN_ATTRIBUTE, BEDROCK_SESSION_ID_ATTRIBUTE};

/// The operations this provider calls on a session, as the Bedrock Agent runtime's own model
/// names them: the name travels with each call ([`call_rest_json_named`]) because the SDK
/// diagnostic a refused call is reported with names the operation — and because
/// [`BEDROCK_SIGNING_NAME`] is a signing name several services share, so the operation is also
/// what says WHICH of them was called (`super::provisioning::SDK_SHARED_NAME_SERVICES`). That
/// plane serves its own path space (`sessions`, here) over `restJson1`, so every call rides on
/// [`call_rest_json_named`] on the URI that operation's own `@http` binding declares.
const CREATE_SESSION: &str = "CreateSession";
const GET_SESSION: &str = "GetSession";
const UPDATE_SESSION: &str = "UpdateSession";
const END_SESSION: &str = "EndSession";
const DELETE_SESSION: &str = "DeleteSession";

/// The session COLLECTION `CreateSession` is bound to (`PUT /sessions/`, trailing slash
/// included, as that operation's own `@http` binding spells it); one session is addressed
/// under it by its `sessionIdentifier` (`GET`/`PUT`/`DELETE /sessions/{sessionIdentifier}/`,
/// and `PATCH /sessions/{sessionIdentifier}` for the end, which binds no trailing slash).
const SESSIONS_URI: &str = "/sessions/";

/// The members `CreateSession` takes the declared properties as, as (property, member): the
/// key the session is encrypted under and the metadata persisted across it. Mapped one by one
/// because the metadata's KEYS are the caller's own data.
const SESSION_MEMBERS: &[(&str, &str)] = &[
    ("EncryptionKeyArn", "encryptionKeyArn"),
    ("SessionMetadata", "sessionMetadata"),
];

/// The member the declared `Tags` map reaches `CreateSession` as — its own `tags`, which
/// `UpdateSession` does not take.
const TAGS_MEMBER: &str = "tags";

/// The member `UpdateSession` takes a session's metadata in, which is the one member of the
/// declaration that call can change.
const SESSION_METADATA_MEMBER: &str = "sessionMetadata";

/// The members an answered session reports, as (attribute, member) — the order
/// [`reported_attributes`] reads them in: the values this type publishes, read off the answer
/// rather than recomputed here.
const SESSION_ATTRIBUTES: &[(&str, &str)] = &[
    (BEDROCK_SESSION_ARN_ATTRIBUTE, "sessionArn"),
    (BEDROCK_SESSION_ID_ATTRIBUTE, "sessionId"),
    ("SessionStatus", "sessionStatus"),
    ("CreatedAt", "createdAt"),
    ("LastUpdatedAt", "lastUpdatedAt"),
];

/// The properties a session's own read reports back under the same names, for drift
/// detection, as (property, member): the two declared values `GetSession` answers.
///
/// Its tags are left out — `GetSession` reports none, and a property the read does not report
/// is not evidence of anything ([`super::drift`]).
const SESSION_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("EncryptionKeyArn", "encryptionKeyArn"),
    ("SessionMetadata", "sessionMetadata"),
];

/// Create the session an `AWS::Bedrock::Session` declares, so a client that deployed the
/// stack and calls `GetSession` with the identity the stack publishes reads the session back
/// — and so every hop off it (opening an invocation, putting a step, ending the session)
/// addresses a session that exists.
///
/// The identity is the Bedrock Agent runtime's: the ARN it minted becomes this resource's
/// physical id — the primary identifier of the type — and the ARN, the uuid, the status and
/// the instants it answered are the resource's attributes.
pub(super) async fn create_bedrock_session(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = declared_session_members(&resource.properties);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&tags)));
    }
    let answered = call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        CREATE_SESSION,
        "PUT",
        SESSIONS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let (session_arn, attributes) = identified_session(&answered)?;
    Ok(Created {
        // That service assigned the session's identity, so the ARN it answered is what `Ref`
        // resolves to and what every call on the session can be addressed by.
        physical_resource_id: Some(session_arn),
        attributes,
    })
}

/// Apply a changed declaration to the session the stack created, through `UpdateSession` —
/// the one call that changes a session that exists.
///
/// It takes the session's identity together with the metadata it is to carry, so a key the
/// declaration no longer states leaves the session without it rather than keeping the one an
/// earlier deployment gave it. The session keeps the uuid and the ARN that service minted, so
/// the handles the stack publishes are unchanged.
///
/// `Ok(None)` is a change that call cannot make, so the deployment reports it as not applied
/// rather than as a success that dropped it: a changed `Tags` map, which `UpdateSession` does
/// not carry — a session's tags are that plane's own tagging calls. (A changed
/// `EncryptionKeyArn` never reaches here: it is one of this type's replacement properties,
/// `CreateSession` being the only call that takes a key at all.)
pub(super) async fn update_bedrock_session(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    if declared.get("Tags") != resource.properties.get("Tags") {
        return Ok(None);
    }
    let mut request = Map::new();
    // The whole declared metadata, so a key the declaration dropped is dropped on the
    // session; an absent property leaves the member absent, which is that service's own
    // decision about a session whose metadata the declaration no longer states.
    if let Some(metadata) = declared.get("SessionMetadata") {
        request.insert(SESSION_METADATA_MEMBER.to_string(), metadata.clone());
    }
    let answered = call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        UPDATE_SESSION,
        "PUT",
        &session_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Some(Created::attributes(
        identified_session(&answered)?.1,
    )))
}

/// Delete the session a stack created, by the identity it was created under — this resource's
/// physical id.
///
/// Two calls, as a client makes them: `EndSession` first, because `DeleteSession` "can't
/// delete a session with an `ACTIVE` status" and a created session is `ACTIVE`, then the
/// delete itself. A session that service no longer holds (`ResourceNotFoundException`, judged
/// on the CODE and never on a message that carries the request's own values) is already in
/// the state the delete asks for, at either step.
pub(super) async fn delete_bedrock_session(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        END_SESSION,
        "PATCH",
        &ended_session_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => {}
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    }
    match call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        DELETE_SESSION,
        "DELETE",
        &session_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The session's configuration as the Bedrock Agent runtime reports it, for drift detection:
/// `GetSession`'s own answer, read by the identity that is this resource's physical id
/// ([`SESSION_REPORTED_PROPERTIES`]). One it no longer holds was deleted.
pub(super) async fn describe_bedrock_session(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        BEDROCK_SIGNING_NAME,
        region,
        GET_SESSION,
        "GET",
        &session_uri(physical_resource_id),
        None,
    )
    .await
    {
        // `GetSession` answers the session itself, not a session inside a member.
        Ok(answer) => Ok(Some(reported_as_properties(
            &answer,
            SESSION_REPORTED_PROPERTIES,
        ))),
        Err(refusal) if refusal.code == BEDROCK_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI one session is addressed at by the reads, the update and the delete: the
/// collection plus the `sessionIdentifier` `@httpLabel` and the trailing slash those bindings
/// carry, the identifier escaped as a single path segment ([`percent_encoded`]) so a value
/// carrying URI structure addresses one session rather than another path.
fn session_uri(session_identifier: &str) -> String {
    format!("{SESSIONS_URI}{}/", percent_encoded(session_identifier))
}

/// The URI `EndSession` is bound to, which carries NO trailing slash
/// (`PATCH /sessions/{sessionIdentifier}`) where every other call on one session does.
fn ended_session_uri(session_identifier: &str) -> String {
    format!("{SESSIONS_URI}{}", percent_encoded(session_identifier))
}

/// One declared property document as the request members `CreateSession` takes
/// ([`SESSION_MEMBERS`]), with the tags left out — the create adds them as the map that call
/// takes, and the update does not take them at all.
///
/// A property the declaration does not state contributes no member, so what that service
/// decides for an absent one is what the session carries.
fn declared_session_members(declared: &Value) -> Map<String, Value> {
    let mut request = Map::new();
    for (property, member) in SESSION_MEMBERS {
        if let Some(stated) = declared.get(*property).filter(|value| !value.is_null()) {
            request.insert((*member).to_string(), stated.clone());
        }
    }
    request
}

/// The attributes one answered session reports ([`SESSION_ATTRIBUTES`]), beside the ARN that
/// service identified it by — the one value a resource of this type cannot be recorded without.
///
/// An answer carrying no ARN is reported as a refusal rather than recorded as a created
/// resource: there would be no handle a client could call that service with. ONE statement of
/// that rule, so the create's identity and the update's attributes cannot disagree about it.
fn identified_session(answered: &Value) -> Result<(String, BTreeMap<String, String>), Refusal> {
    let attributes = reported_attributes(answered, SESSION_ATTRIBUTES);
    let Some(session_arn) = attributes.get(BEDROCK_SESSION_ARN_ATTRIBUTE).cloned() else {
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Bedrock Agent runtime answered a session without an ARN".to_string(),
        });
    };
    Ok((session_arn, attributes))
}
