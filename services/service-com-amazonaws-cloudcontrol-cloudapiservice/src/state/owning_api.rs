// SPDX-License-Identifier: CC0-1.0

//! The resource types whose owning service is reached through its OWN API.
//!
//! [`crate::state::native`] resolves a type whose resources live in another service's STORE
//! of this emulator by writing that store directly. Some types cannot be served that way:
//! the owning service's create does work of its own — it provisions further resources in yet
//! more services, and reports a lifecycle a client drives with calls of its own — so the only
//! thing that creates such a resource is that service's own create API, and the only thing
//! that knows where the resource has got to is that service's own read.
//!
//! `AWS::CloudFormation::Stack` is such a type ([`stacks`]): a `CreateResource` of it has to
//! create a REAL CloudFormation stack, whose resources are created in the services that own
//! them, whose own reads report it while it is `CREATE_IN_PROGRESS`, and which a
//! `cloudformation:SignalResource` against a gate its template declares is what releases.
//!
//! Two rules follow, and this module is where they live.
//!
//! * **The work is a call on that service's own API.** A resource operation request of such a
//!   type is accepted exactly like any other — it is recorded owing its work, which falls due
//!   one [`crate::state::RESOURCE_OPERATION_SPAN`] later — but the work is applied HERE, by
//!   [`settle`], which whichever operation of this service is called next runs BEFORE it locks
//!   the store. It has to be: a call on another service is asynchronous and this store's guard
//!   is not held across one.
//! * **The request's status is that service's lifecycle.** Once the owning service has taken
//!   the work, the request goes on owing [`crate::state::ResourceEffect::Awaiting`]: it stays
//!   `IN_PROGRESS` for as long as that service reports the resource still changing, and
//!   reaches `SUCCESS` or `FAILED` when that service's own lifecycle does
//!   ([`stacks::progress`]). So a client that polls `GetResourceRequestStatus` while driving
//!   the stack through CloudFormation's own API is answered from one truth, and a request
//!   never reports a create that nothing has finished.
//!
//! Reads of such a type go the same way: `GetResource` and `ListResources` are answered by the
//! owning service's own read ([`Resolution`], [`page`]), taken before this store is locked, so
//! a resource that service holds is reported whichever API created it and one it no longer
//! holds reads as not found.

pub mod stacks;

/// `AWS::ApiGatewayV2::Api`, the other type an owning service's own API answers for.
pub mod apis;

use ::std::time::SystemTime;

use ::serde_json::Value;

use super::native::{
    NativeResources, HANDLER_ERROR_GENERAL_SERVICE_EXCEPTION, HANDLER_ERROR_INVALID_REQUEST,
    HANDLER_ERROR_SERVICE_LIMIT_EXCEEDED,
};
use super::{
    ComAmazonawsCloudcontrolCloudapiserviceState, RequestRecord, ResolvedResource, ResourceEffect,
    HANDLER_ERROR_ALREADY_EXISTS, HANDLER_ERROR_NOT_FOUND, STATUS_FAILED, STATUS_SUCCESS,
};

/// Why an owning service reached through its OWN API would not do what a resource operation
/// asked of it.
///
/// Each variant is that service's verdict, which is what the resource operation request
/// reports: the `HandlerErrorCode` it maps to ([`Self::code`]) and the diagnostic
/// ([`Self::detail`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// That service holds no such resource — the one verdict this service states in its own
    /// terms, which is what the handler that owns a type reports for a resource it cannot
    /// find ([`crate::state::handler_not_found_detail`]).
    NotFound,
    /// That service would not carry the work out, in its OWN words, under the
    /// `HandlerErrorCode` its answer maps to.
    Refused {
        /// The handler error code the refusal is reported under — a value of the model's own
        /// `HandlerErrorCode` enum.
        code: &'static str,
        /// That service's own statement of what happened, passed on rather than re-worded.
        detail: String,
    },
}

impl Refusal {
    /// The owning service would not take the resource model the request carried: a fault in
    /// the model, which that service is what judges.
    pub fn invalid(detail: String) -> Self {
        Self::Refused {
            code: HANDLER_ERROR_INVALID_REQUEST,
            detail,
        }
    }

    /// That service holds as many of this resource as it may, and states the quota in its
    /// own terms.
    pub fn at_capacity(detail: String) -> Self {
        Self::Refused {
            code: HANDLER_ERROR_SERVICE_LIMIT_EXCEEDED,
            detail,
        }
    }

    /// That service already holds the resource the work would create.
    pub fn already_exists(detail: String) -> Self {
        Self::Refused {
            code: HANDLER_ERROR_ALREADY_EXISTS,
            detail,
        }
    }

    /// That service raised an error of its own that the type's handler maps to no other
    /// verdict — which is what that code is for.
    pub fn rejected(detail: String) -> Self {
        Self::Refused {
            code: HANDLER_ERROR_GENERAL_SERVICE_EXCEPTION,
            detail,
        }
    }

    /// The `HandlerErrorCode` the request fails with.
    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => HANDLER_ERROR_NOT_FOUND,
            Self::Refused { code, .. } => code,
        }
    }
}

/// Whether the resources of `type_name` are the OWNING service's, reached through that
/// service's own API.
pub fn backs(type_name: &str) -> bool {
    identifier_property(type_name).is_some()
}

// ---------------------------------------------------------------------------
// What every provider of this module reads out of a resource model, and out of an answer
// the owning service refused. One copy, because the shape of a model's value and of a
// refused call is the REGISTRY's rather than any one type's.
// ---------------------------------------------------------------------------

/// How many pages of an owning service's own read ONE listing of this service walks.
///
/// That read is bounded per page and the resources of an account are bounded by the owning
/// service's own quota, so this is the ceiling on the work one listing can drive — well
/// above the pages that quota can fill, and a number rather than "until they run out".
pub(super) const MAX_OWNING_PAGES: usize = 64;

/// The property every type's schema carries its tag set under, as a list of
/// `{ Key, Value }` pairs, and the two members of one of its entries.
pub(super) const TAGS: &str = "Tags";
pub(super) const TAG_KEY: &str = "Key";
pub(super) const TAG_VALUE: &str = "Value";

/// The text `value` carries, or why it is not the text `property` takes.
pub(super) fn text(property: &str, value: &Value) -> Result<String, Refusal> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Refusal::invalid(format!("{property} must be a string")))
}

/// The count `value` carries, or why it is not the number `property` takes.
pub(super) fn count(property: &str, value: &Value) -> Result<i32, Refusal> {
    value
        .as_i64()
        .and_then(|count| i32::try_from(count).ok())
        .ok_or_else(|| Refusal::invalid(format!("{property} must be a number")))
}

/// The flag `value` carries, or why it is not the boolean `property` takes.
pub(super) fn flag(property: &str, value: &Value) -> Result<bool, Refusal> {
    value
        .as_bool()
        .ok_or_else(|| Refusal::invalid(format!("{property} must be a boolean")))
}

/// The strings `value` carries, or why it is not the list of strings `property` takes.
pub(super) fn text_list(property: &str, value: &Value) -> Result<Vec<String>, Refusal> {
    let Some(entries) = value.as_array() else {
        return Err(Refusal::invalid(format!(
            "{property} must be a list of strings"
        )));
    };
    entries.iter().map(|entry| text(property, entry)).collect()
}

/// The tag entries `value` carries, in the order the list carried them — or why it is not a
/// tag set at all.
pub(super) fn tag_entries(value: &Value) -> Result<Vec<(String, String)>, Refusal> {
    let Some(entries) = value.as_array() else {
        return Err(Refusal::invalid(format!(
            "{TAGS} must be a list of {{ {TAG_KEY}, {TAG_VALUE} }} pairs"
        )));
    };
    let mut tags = Vec::with_capacity(entries.len());
    for entry in entries {
        let key = entry.get(TAG_KEY).and_then(Value::as_str);
        let tag_value = entry.get(TAG_VALUE).and_then(Value::as_str);
        let (Some(key), Some(tag_value)) = (key, tag_value) else {
            return Err(Refusal::invalid(format!(
                "every {TAGS} entry must carry {TAG_KEY} and {TAG_VALUE}"
            )));
        };
        tags.push((key.to_string(), tag_value.to_string()));
    }
    Ok(tags)
}

/// What an owning service's refused call SAID: `(the error code it named, the message it
/// worded)`, with the client's own rendering of the error standing in for a message it did
/// not word.
///
/// The one reader of a refused call's answer, because which `HandlerErrorCode` a code maps
/// to is the TYPE's business while reading the code and the message is not.
pub(super) fn answered_error<E>(
    error: ::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> (String, String)
where
    ::aws_smithy_runtime_api::client::result::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >: ::aws_smithy_types::error::metadata::ProvideErrorMetadata + ::std::fmt::Display,
{
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
    let code = error.code().unwrap_or_default().to_string();
    let detail = error
        .message()
        .map_or_else(|| error.to_string(), ToString::to_string);
    (code, detail)
}

/// The property a resource of such a type carries its primary identifier in — the one the
/// type's schema points `primaryIdentifier` at — or `None` for every other type, whose
/// resources live in a store this emulator holds.
pub fn identifier_property(type_name: &str) -> Option<&'static str> {
    match type_name {
        stacks::TYPE_NAME => Some(stacks::IDENTIFIER_PROPERTY),
        apis::TYPE_NAME => Some(apis::IDENTIFIER_PROPERTY),
        _ => None,
    }
}

/// Where the owning service's own lifecycle for one resource has got to.
#[derive(Debug)]
pub enum Progress {
    /// That service reports the resource still changing, so the request is still running.
    Running,
    /// The operation the request started has been carried out; carries the resource model
    /// the request leaves behind, and `None` for a delete, which leaves no resource to
    /// report one of.
    Settled(Option<Value>),
    /// That service's lifecycle reached a state the operation did not ask for, in its own
    /// terms.
    Failed(Refusal),
}

/// How one request resolves the resource it names.
#[derive(Debug)]
pub enum Resolution {
    /// The type's resources live in a store this emulator holds — this service's own, or
    /// the owning service's through a provider handle ([`crate::state::native`]).
    Stored,
    /// The type's resources are the OWNING service's, reached through that service's own
    /// API: this is what its read answered, taken before this store was locked.
    Owned(Option<ResolvedResource>),
}

impl Resolution {
    /// What the owning service's own read answers about `identifier` of `type_name`, for a
    /// type that service answers for; [`Self::Stored`] for every other type.
    ///
    /// Called by each operation that takes an `Identifier`, BEFORE it locks this store: the
    /// read is a call on another service, and this store's guard is never held across one.
    pub async fn of(
        ctx: &smithy_server_runtime::OperationContext<'_>,
        region: &str,
        type_name: &str,
        identifier: &str,
    ) -> Self {
        let read = match type_name {
            stacks::TYPE_NAME => stacks::read(ctx, region, identifier).await,
            apis::TYPE_NAME => apis::read(ctx, region, identifier).await,
            _ => return Self::Stored,
        };
        let Some(identifier_property) = identifier_property(type_name) else {
            return Self::Stored;
        };
        Self::Owned(read.map(|properties| resolved(type_name, identifier_property, properties)))
    }
}

/// The page of `type_name`'s resources the OWNING service's own read answers — at most
/// `limit` of them, and only those whose identifier sorts after `after` — or `None` for a
/// type whose resources live in a store this emulator holds.
pub async fn page(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    type_name: &str,
    after: Option<&str>,
    limit: usize,
) -> Option<Vec<ResolvedResource>> {
    let listed = match type_name {
        stacks::TYPE_NAME => stacks::list(ctx, region, after, limit).await,
        apis::TYPE_NAME => apis::list(ctx, region, after, limit).await,
        _ => return None,
    };
    let identifier_property = identifier_property(type_name)?;
    Some(
        listed
            .into_iter()
            .map(|(_, properties)| resolved(type_name, identifier_property, properties))
            .collect(),
    )
}

/// One resource of such a type, as every answer of this service reports one: under the
/// primary identifier it HOLDS, read off the model's own identifier property.
fn resolved(type_name: &str, identifier_property: &str, properties: Value) -> ResolvedResource {
    let identifier = super::model_identifier(type_name, &properties, identifier_property)
        .unwrap_or_default();
    ResolvedResource {
        type_name: type_name.to_string(),
        identifier,
        identifier_property: identifier_property.to_string(),
        properties,
    }
}

/// One request whose owed work is a call on an owning service's own API, as [`settle`]
/// needs it with no lock held.
#[derive(Debug, Clone)]
pub struct OwedWork {
    /// The request that owes it.
    pub request_token: String,
    /// The type whose owning service is to be called.
    pub type_name: String,
    /// `CREATE`, `UPDATE` or `DELETE`.
    pub operation: String,
    /// The Region the request was accepted in, which is the Region the call is made about.
    pub region: String,
    /// The resource the work is owed on — `None` for a create whose identifier the owning
    /// service has not assigned yet.
    pub identifier: Option<String>,
    /// The resource model a create or an update is to apply.
    pub properties: Option<Value>,
    /// Whether the owning service has already TAKEN the work, so what is owed now is
    /// following its lifecycle rather than starting it.
    pub started: bool,
}

/// Whether the work `effect` owes on a resource of `type_name` is owed to an owning service
/// reached through its own API — the work [`settle`] carries out, which no synchronous pass
/// can.
///
/// A request that owes NO effect is never one of these: a cancel abandons the work it owed,
/// and what is then due is the cancellation itself, which this store settles on its own.
pub(super) fn owed_elsewhere(type_name: &str, effect: Option<&ResourceEffect>) -> bool {
    effect.is_some() && backs(type_name)
}

impl ComAmazonawsCloudcontrolCloudapiserviceState {
    /// CLAIM every request whose owed work is a call on an owning service's own API and which
    /// the clock has made due, answering the work to carry out.
    ///
    /// Claiming is what makes the call happen once: the work is a call made with this store
    /// UNLOCKED, so the record is leased for [`OWNING_CALL_LEASE`] before the guard is
    /// dropped and a pass that arrives meanwhile finds it not yet due. The lease is a span
    /// rather than a flag, so work a pass abandoned — a request whose caller went away
    /// mid-call — falls due again instead of being owed forever, and a call that comes back
    /// settles the record (or re-arms it: [`Self::owning_service_running`]) long before the
    /// lease is up.
    pub fn claim_work_owed_to_owning_services(&mut self, now: SystemTime) -> Vec<OwedWork> {
        let mut claimed = Vec::new();
        for record in self.requests.values_mut() {
            let Some(pending) = record.pending.as_mut() else {
                continue;
            };
            if pending.due > now || !owed_elsewhere(&record.type_name, pending.effect.as_ref()) {
                continue;
            }
            let effect = pending.effect.as_ref();
            claimed.push(OwedWork {
                request_token: record.request_token.clone(),
                type_name: record.type_name.clone(),
                operation: record.operation.clone(),
                region: effect.map(region_of).unwrap_or_default(),
                identifier: record.identifier.clone(),
                properties: effect.and_then(properties_of),
                started: matches!(effect, Some(ResourceEffect::Awaiting { .. })),
            });
            pending.due = now + OWNING_CALL_LEASE;
        }
        claimed
    }

    /// Record that the owning service has TAKEN the work this request owed, on the resource
    /// `identifier` names: the request goes on running, now following that service's own
    /// lifecycle for it — which the next call on reads.
    pub fn owning_service_started(
        &mut self,
        request_token: &str,
        identifier: &str,
        now: SystemTime,
    ) {
        let Some(record) = self.requests.get_mut(request_token) else {
            return;
        };
        let Some(pending) = record.pending.as_mut() else {
            return;
        };
        let region = pending.effect.as_ref().map(region_of).unwrap_or_default();
        pending.effect = Some(ResourceEffect::Awaiting { region });
        // The lease the claim took is over: what is owed now is reading that service's
        // lifecycle, which is due from here on.
        pending.due = now;
        // The resource the request acts on is the one the owning service answered to, which
        // for a type whose primary identifier that service ASSIGNS is known only once it
        // has: from here on every answer about this request names it.
        record.identifier = Some(identifier.to_string());
    }

    /// Record that the owning service reports the resource STILL CHANGING: the request goes
    /// on running, and its lifecycle is due to be read again from the next call on.
    pub fn owning_service_running(&mut self, request_token: &str, now: SystemTime) {
        if let Some(pending) = self
            .requests
            .get_mut(request_token)
            .and_then(|record| record.pending.as_mut())
        {
            pending.due = now;
        }
    }

    /// Record that the owning service's lifecycle has carried the request's operation out:
    /// the request is `SUCCESS`, and what it leaves behind is the model that service now
    /// holds.
    ///
    /// A record that no longer OWES the work is left exactly as it is: the call was made with
    /// this store unlocked, and a `CancelResourceRequest` that landed meanwhile abandoned the
    /// work and owns the request's status from then on ([`Self::cancel_request`]).
    pub fn owning_service_settled(&mut self, request_token: &str, model: Option<Value>) {
        let Some(record) = self.still_owing(request_token) else {
            return;
        };
        record.pending = None;
        record.retry_after = None;
        record.resource_model =
            model.map(|model| super::rendered_properties(&record.type_name, &model));
        record.operation_status = STATUS_SUCCESS.to_string();
    }

    /// The request `request_token` names while it still owes the work a claim took, and
    /// `None` once it owes none — the one check both recorders of an owning service's answer
    /// make, so an answer never moves a request a cancel has already settled.
    fn still_owing(&mut self, request_token: &str) -> Option<&mut RequestRecord> {
        self.requests
            .get_mut(request_token)
            .filter(|record| record.pending.is_some())
    }

    /// Record that the owning service would not carry the work out, or that its lifecycle
    /// reached a state the operation did not ask for: the request is `FAILED`, carrying that
    /// service's own verdict — the `HandlerErrorCode` it maps to and its diagnostic.
    ///
    /// A record that no longer owes the work is left as it is, for the reason
    /// [`Self::owning_service_settled`] states.
    pub fn owning_service_failed(&mut self, request_token: &str, refusal: Refusal) {
        let Some(record) = self.still_owing(request_token) else {
            return;
        };
        record.pending = None;
        record.retry_after = None;
        record.error_code = Some(refusal.code().to_string());
        record.status_message = Some(match refusal {
            // What the handler that owns this type states for a resource it cannot find,
            // which is this service's own sentence for the one verdict every type shares.
            Refusal::NotFound => super::handler_not_found_detail(
                &record.type_name,
                record.identifier.as_deref().unwrap_or_default(),
            ),
            Refusal::Refused { detail, .. } => detail,
        });
        record.operation_status = STATUS_FAILED.to_string();
    }

    /// The resource `identifier` of `type_name` as it exists NOW, resolved where the
    /// resource LIVES: by the owning service's own read where `resolution` carries one, and
    /// in the store that holds it otherwise ([`Self::resolve`]).
    ///
    /// The ONE resolution every operation of this service uses, so a type served by an
    /// owning service's own API is never looked for in this store — where no resource of it
    /// can be — and no type served by this store is answered from a call.
    pub fn resolve_as(
        &self,
        resolution: &Resolution,
        native: &NativeResources,
        region: &str,
        type_name: &str,
        identifier: &str,
    ) -> Option<ResolvedResource> {
        match resolution {
            Resolution::Owned(resolved) => resolved.clone(),
            Resolution::Stored => self.resolve(native, region, type_name, identifier),
        }
    }
}

/// How long a claim on one request's owed work holds it, so the call that work IS happens
/// once ([`ComAmazonawsCloudcontrolCloudapiserviceState::claim_work_owed_to_owning_services`]).
///
/// Chosen: comfortably longer than a call on an owning service of this emulator — one whose
/// create may itself provision a whole template through further services — and short enough
/// that work a pass abandoned falls due again well inside a client's poll loop.
const OWNING_CALL_LEASE: ::std::time::Duration = ::std::time::Duration::from_secs(30);

/// The Region an effect is to be applied in.
fn region_of(effect: &ResourceEffect) -> String {
    match effect {
        ResourceEffect::Create { region, .. }
        | ResourceEffect::Update { region, .. }
        | ResourceEffect::Delete { region }
        | ResourceEffect::Awaiting { region } => region.clone(),
    }
}

/// The resource model an effect is to apply, for the two that carry one.
fn properties_of(effect: &ResourceEffect) -> Option<Value> {
    match effect {
        ResourceEffect::Create { properties, .. } | ResourceEffect::Update { properties, .. } => {
            Some(properties.clone())
        }
        ResourceEffect::Delete { .. } | ResourceEffect::Awaiting { .. } => None,
    }
}

/// Carry out everything this store owes an owning service's own API, and bring every request
/// that is following such a service's lifecycle up to date.
///
/// Called by every operation of this service BEFORE it locks the store, so no two of them
/// can disagree about which of these requests have completed — the same reason
/// [`ComAmazonawsCloudcontrolCloudapiserviceState::materialise_due`] is called by every one
/// of them. The store's guard is taken only to read what is owed and to record what the
/// owning service answered, never across the call between the two.
pub async fn settle<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    now: SystemTime,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let owed = state.lock().claim_work_owed_to_owning_services(now);
    for work in owed {
        let identifier = work.identifier.clone().unwrap_or_default();
        if work.started {
            let progress = match work.type_name.as_str() {
                apis::TYPE_NAME => {
                    apis::progress(ctx, &work.region, &identifier, &work.operation).await
                }
                stacks::TYPE_NAME => {
                    stacks::progress(ctx, &work.region, &identifier, &work.operation).await
                }
                // No owning API answers for this type, so there is no lifecycle to follow:
                // the request fails rather than being read against another type's service.
                other => Progress::Failed(unanswered(other)),
            };
            match progress {
                // Still running: the request keeps owing the lifecycle it is following, so
                // the next call reads it again.
                Progress::Running => state
                    .lock()
                    .owning_service_running(&work.request_token, now),
                Progress::Settled(model) => state
                    .lock()
                    .owning_service_settled(&work.request_token, model),
                Progress::Failed(refusal) => state
                    .lock()
                    .owning_service_failed(&work.request_token, refusal),
            }
            continue;
        }
        // WHICH owning service is called is the TYPE's own business, named explicitly here
        // so a type added to [`identifier_property`] and not to this dispatch is refused
        // rather than quietly asked of another type's service; WHAT is asked of it is the
        // operation the request owes.
        let started = match (work.type_name.as_str(), work.operation.as_str()) {
            (apis::TYPE_NAME, crate::state::OPERATION_DELETE) => {
                apis::delete(ctx, &work.region, &identifier)
                    .await
                    .map(|()| identifier.clone())
            }
            (apis::TYPE_NAME, crate::state::OPERATION_UPDATE) => apis::update(
                ctx,
                &work.region,
                &identifier,
                &work.properties.clone().unwrap_or_default(),
            )
            .await
            .map(|()| identifier.clone()),
            (apis::TYPE_NAME, _) => {
                apis::create(
                    ctx,
                    &work.region,
                    &work.properties.clone().unwrap_or_default(),
                )
                .await
            }
            (stacks::TYPE_NAME, crate::state::OPERATION_DELETE) => {
                stacks::delete(ctx, &work.region, &identifier)
                    .await
                    .map(|()| identifier.clone())
            }
            (stacks::TYPE_NAME, crate::state::OPERATION_UPDATE) => stacks::update(
                ctx,
                &work.region,
                &identifier,
                &work.properties.clone().unwrap_or_default(),
            )
            .await
            .map(|()| identifier.clone()),
            (stacks::TYPE_NAME, _) => {
                stacks::create(
                    ctx,
                    &work.region,
                    &work.properties.clone().unwrap_or_default(),
                )
                .await
            }
            (other, _) => Err(unanswered(other)),
        };
        match started {
            Ok(identifier) => state
                .lock()
                .owning_service_started(&work.request_token, &identifier, now),
            Err(refusal) => state
                .lock()
                .owning_service_failed(&work.request_token, refusal),
        }
    }
}

/// The refusal for work owed on a type NO owning API answers for: nothing here can carry it
/// out, so the request fails naming the type rather than having another type's service asked
/// about it. [`backs`] admits only the types the dispatch above names, so nothing reaches
/// this today — it is what keeps a type added to [`identifier_property`] alone from being
/// sent to the wrong service.
fn unanswered(type_name: &str) -> Refusal {
    Refusal::rejected(format!(
        "no owning service of this emulator answers for resources of type {type_name}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each type this module stands behind is one it answers for, and the property it names
    /// as that type's primary identifier is the type's own.
    #[test]
    fn only_a_type_an_owning_api_answers_for_is_backed() {
        for (type_name, expected) in [
            (stacks::TYPE_NAME, stacks::IDENTIFIER_PROPERTY),
            (apis::TYPE_NAME, apis::IDENTIFIER_PROPERTY),
        ] {
            assert_eq!(
                identifier_property(type_name),
                Some(expected),
                "expected {type_name} to be answered by its owning service's own API"
            );
        }
        for other in ["AWS::SQS::Queue", "AWS::Logs::LogGroup", "AWS::CloudFormation::Macro"] {
            assert!(
                !backs(other),
                "type {other:?}: expected its resources to live in a store this emulator \
                 holds, got an owning-API type"
            );
        }
    }

    /// A request that owes no effect at all is never owed to an owning service: a cancel
    /// abandoned its work, and the cancellation this store settles itself.
    #[test]
    fn an_abandoned_request_is_not_owed_to_an_owning_service() {
        assert!(
            owed_elsewhere(
                stacks::TYPE_NAME,
                Some(&ResourceEffect::Delete {
                    region: "us-east-1".to_string()
                })
            ),
            "expected a delete of an owning-API type to be owed to that service"
        );
        assert!(
            !owed_elsewhere(stacks::TYPE_NAME, None),
            "expected a request whose work a cancel abandoned to be owed to nobody"
        );
    }
}
