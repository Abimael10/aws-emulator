// SPDX-License-Identifier: CC0-1.0

//! @op UpdateHarness
//!
//! Publishes the next version of a stored harness: the configuration the request changes
//! becomes version N+1 of the record [`crate::ops::create_harness`] wrote at version 1
//! ([`crate::state::HarnessRecord`]), and the clock owes the harness its `UPDATING` ->
//! `READY` transition. Every later read of the harness ([`crate::ops::get_harness`]) comes
//! from that one record.
//!
//! What an update CHANGES is what the request carries: a member it names replaces the
//! stored one, a member it omits is "retained when not specified" as the model documents,
//! and the three `Updated*` wrappers carry PATCH semantics of their own — the wrapper
//! present with `optionalValue` sets the member, present without it CLEARS the member, and
//! the wrapper absent leaves it alone.
//!
//! HAND-AUTHORED, not generated: the scaffold's op-stub generator declined all four
//! harness operations ("shape missing from model: smithy.api#Unit"), so `UpdateHarness`
//! was not in the service's `operations(..)` allow-list at all — and a route the model
//! binds but the service does not wire is claimed by no service, which the router answers
//! `404 UnknownService` in its own envelope, an error neither this operation nor any AWS
//! model declares. This file puts `PATCH /harnesses/{harnessId}` back on the routing table.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `UpdateHarness::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! ---
//!
//! # Modeled contract for `UpdateHarness`
//!
//! From this service's Smithy model
//! (`smithy_query(query="operation",operation="UpdateHarness",model_path="services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/smithy-model/com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json")`).
//!
//! Operation to update a harness. `PATCH /harnesses/{harnessId}`, success status 200.
//!
//! ## Input
//!
//! * `harnessId` (HarnessId, @httpLabel, pattern
//!   ^[a-zA-Z][a-zA-Z0-9_]{0,39}-[a-zA-Z0-9]{10}$) **required** — the harness to update.
//! * `clientToken` (ClientToken, len 33..256, @idempotencyToken)
//! * `executionRoleArn` (RoleArn, len 1..2048) — retained when not specified.
//! * `environment` (HarnessEnvironmentProviderRequest) — retained when not specified.
//! * `environmentArtifact` (UpdatedHarnessEnvironmentArtifact) — PATCH wrapper.
//! * `environmentVariables` (EnvironmentVariablesMap, len 0..50) — replaces all when given.
//! * `authorizerConfiguration` (UpdatedAuthorizerConfiguration) — PATCH wrapper.
//! * `model` (HarnessModelConfiguration), `systemPrompt`, `tools`, `skills`,
//!   `allowedTools`, `truncation`, `hooks` — each replaces the stored value when given.
//! * `memory` (UpdatedHarnessMemoryConfiguration) — PATCH wrapper.
//! * `maxIterations`, `maxTokens`, `timeoutSeconds` (Integer) — retained when not given.
//!
//! ## Output
//!
//! * `harness` (Harness) **required** — the updated harness.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — the request is denied per access permissions.
//! * `ConflictException` (HTTP 409) — the request conflicts with the resource's state.
//! * `InternalServerException` (HTTP 500) — an unexpected error during processing.
//! * `ResourceNotFoundException` (HTTP 404) — a resource the operation references does not
//!   exist.
//! * `ServiceQuotaExceededException` (HTTP 402) — a service quota was exceeded.
//! * `ThrottlingException` (HTTP 429) — the number of requests exceeds the limit.
//! * `ValidationException` (HTTP 400) — the input fails to satisfy the service's
//!   constraints.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol.json");

/// The operation's MODELED errors.
#[smithy_macros::operation_error("com.amazonaws.bedrockagentcorecontrol#UpdateHarness")]
#[derive(Debug)]
pub enum UpdateHarnessErrorModeled {
    AccessDeniedException(AccessDeniedException),
    ConflictException(ConflictException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceQuotaExceededException(ServiceQuotaExceededException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it. Wrapping
/// the modeled enum is what lets a scaffolded operation answer 501.
pub type UpdateHarnessError = crate::scaffolded::Scaffolded<UpdateHarnessErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel.
impl From<UpdateHarnessErrorModeled> for UpdateHarnessError {
    fn from(modeled: UpdateHarnessErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `UpdateHarness` operation.
pub struct UpdateHarness<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> UpdateHarness<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for UpdateHarness<Eng>
{
    type Input = UpdateHarnessInput;
    type Output = UpdateHarnessOutput;
    type Error = UpdateHarnessError;

    const NAME: &'static str = "UpdateHarness";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        super::types::UPDATE_HARNESS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        super::types::UPDATE_HARNESS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = ctx;
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            UpdateHarnessErrorModeled::InternalServerException(InternalServerException {
                message: Some("the harness store is unavailable".to_string()),
            })
        })?;
        let region = crate::state::request_region(ctx.parts());
        let now = ::std::time::SystemTime::now();
        let mut store = state.lock();

        // A replayed request carrying the same `clientToken` answers with the harness that
        // token already changed, rather than publishing a second version.
        if let Some(harness_id) =
            store.replayed_harness_result(Self::NAME, input.client_token.as_deref())
        {
            if let Some(record) = store.harnesses.get(&harness_id) {
                return Ok(UpdateHarnessOutput {
                    harness: record.harness(now),
                });
            }
        }

        let record = store.harnesses.get_mut(&input.harness_id).ok_or_else(|| {
            UpdateHarnessErrorModeled::ResourceNotFoundException(crate::state::harness_not_found(
                &input.harness_id,
            ))
        })?;

        // The version number the wire can carry is bounded (`HarnessVersion`, pattern
        // `^([1-9][0-9]{0,4})$`), so repeated updates meet the modelled quota rather than
        // publishing a version no response could report.
        if record.version >= crate::state::MAX_HARNESS_VERSION {
            return Err(UpdateHarnessErrorModeled::ServiceQuotaExceededException(
                ServiceQuotaExceededException {
                    message: Some(crate::state::harness_version_quota_met(&input.harness_id)),
                },
            )
            .into());
        }

        // Each member the request names replaces the stored one; each member it omits is
        // "retained when not specified". The three `Updated*` wrappers carry their own PATCH
        // semantics, which `crate::state` states once for every operation that takes one.
        if let Some(execution_role_arn) = input.execution_role_arn {
            record.execution_role_arn = execution_role_arn;
        }
        if let Some(environment) = input.environment {
            record.environment = crate::state::harness_runtime_environment(
                &region,
                &record.harness_id,
                &record.harness_name,
                Some(environment),
            );
        }
        if let Some(environment_variables) = input.environment_variables {
            record.environment_variables = crate::state::sorted_map(Some(environment_variables));
        }
        if let Some(model) = input.model {
            record.model = Some(model);
        }
        if let Some(system_prompt) = input.system_prompt {
            record.system_prompt = system_prompt;
        }
        if let Some(tools) = input.tools {
            record.tools = tools;
        }
        if let Some(skills) = input.skills {
            record.skills = skills;
        }
        if let Some(allowed_tools) = input.allowed_tools {
            record.allowed_tools = allowed_tools;
        }
        if let Some(truncation) = input.truncation {
            record.truncation = crate::state::effective_truncation_configuration(Some(truncation));
        }
        if let Some(hooks) = input.hooks {
            record.hooks = hooks;
        }
        if let Some(max_iterations) = input.max_iterations {
            record.max_iterations = Some(max_iterations);
        }
        if let Some(max_tokens) = input.max_tokens {
            record.max_tokens = Some(max_tokens);
        }
        if let Some(timeout_seconds) = input.timeout_seconds {
            record.timeout_seconds = Some(timeout_seconds);
        }
        // The wrapper present SETS or CLEARS the member; absent, it leaves it unchanged.
        if let Some(wrapper) = input.environment_artifact {
            record.environment_artifact = wrapper.optional_value;
        }
        if let Some(wrapper) = input.authorizer_configuration {
            record.authorizer_configuration = wrapper.optional_value;
        }
        if let Some(wrapper) = input.memory {
            record.memory = wrapper.optional_value;
        }

        // The change is ACCEPTED here and the harness publishes its next version; reaching
        // `READY` again is owed by the clock, not by this call, so a caller polling the
        // harness sees `UPDATING` and then `READY`.
        record.version = record.version.saturating_add(1);
        record.updated_at = now;
        record.transient_status = crate::state::STATUS_UPDATING.to_string();
        record.settled_status = crate::state::STATUS_READY.to_string();
        record.settles_at = now + crate::state::HARNESS_SETTLE_DELAY;
        let output = UpdateHarnessOutput {
            harness: record.harness(now),
        };

        store.remember_harness_client_token(Self::NAME, input.client_token, &input.harness_id);

        Ok(output)
    }
}
