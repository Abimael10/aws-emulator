// SPDX-License-Identifier: CC0-1.0

//! How this service states a request the framework refused BEFORE any handler ran,
//! on an operation whose model declares no `ValidationException`: a `@required`
//! member the request omitted, and a value the model's own constraints refuse.
//!
//! Both are found while the request is read and walked, so the framework answers
//! them in its own refusal body — an error code and a `message` member — under the
//! code `ValidationException`. For most operations of this service that is the right
//! identity: their model declares `ValidationException`, so a client branching on
//! the code sees an error the operation can answer with.
//!
//! Fifteen operations declare no `ValidationException` at all, among them every
//! operation of the license-configuration surface
//! (`smithy_query(query="digest")` lists each operation's errors). For those, the
//! only client error the model gives the service for a bad request is
//! `InvalidParameterValueException` ("One or more parameter values are not valid.",
//! `@httpError` 400), whose one member is `Message` — so that is the identity such a
//! request is refused under, and the sentence states which parameter was refused:
//!
//! * a member the request omitted — `Required parameter <member> must not be null.`
//! * a value a modelled constraint refuses — `Invalid parameter value <value> for
//!   <member>`
//!
//! Each is fixed text around the member's own wire name and, for the second, the
//! value the request carried.
//!
//! The refusal hook ([`crate::service::EmulatedService::override_refusal`]) can
//! reword such a refusal but not RESHAPE it: it chooses a code and a sentence, and
//! the framework writes them into its own body, whose prose member is `message`
//! rather than the `Message` the model declares. So the refusal is made HERE
//! instead, where the modelled shape itself is serialized through the model's own
//! schema — the same path a handler-raised `InvalidParameterValueException` takes,
//! and therefore the same body.
//!
//! Everything else about the request is left exactly as it was: one that breaks no
//! modelled rule is handed to the generated operation, which runs the whole
//! framework sequence (deserialize, validate, handle, serialize) over the same
//! bytes, and a failure that is not a refusal at all — a body the codec cannot read,
//! a member whose JSON type the modelled one cannot be read from — keeps the
//! framework's own answer, because no modelled rule was broken by it.

use std::collections::HashMap;
use std::marker::PhantomData;

use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, ServerHttpProtocol, Service,
    Violation, ViolationKind,
};

use crate::ops::types::InvalidParameterValueException;

/// The wire error code a refused request is answered with on an operation that
/// models no `ValidationException`.
const INVALID_PARAMETER_VALUE_EXCEPTION: &str = "InvalidParameterValueException";

/// `InvalidParameterValueException`'s modelled `@httpError`
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`).
const INVALID_PARAMETER_VALUE_STATUS: u16 = 400;

/// The sentence for a `@required` member the request omitted: fixed text around the
/// one variable part, the member's own wire name.
fn absent_member_message(member: &str) -> String {
    format!("Required parameter {member} must not be null.")
}

/// The sentence for a value a modelled constraint refuses: fixed text around the
/// value the request carried and the member that carried it. The one the handlers'
/// own catalog guards state too ([`crate::state::invalid_parameter_value_message`]).
fn invalid_value_message(member: &str, value: &str) -> String {
    crate::state::invalid_parameter_value_message(member, value)
}

/// How this service states `violation`, or `None` when it carries nothing to state
/// it with.
///
/// `violation.path` is the member's lowerCamel wire name, spelled by the framework's
/// own path speller (`aws_smithy_schema_server::constraints::path`) and prefixed by
/// an enclosing member for one nested inside a request structure; `violation.value`
/// is the value the request carried at that member, restored to the submitted bytes
/// by [`::service_common::validation::refusing_violations`].
///
/// A violation of a `@sensitive` member is left to the framework, whose own wording
/// states the requirement without echoing the value: this service's sentence for a
/// refused value is built AROUND that value, so there is no way to state it here
/// without disclosing one.
fn stated(violation: &Violation) -> Option<String> {
    if matches!(violation.kind, ViolationKind::MissingRequiredMember) {
        return Some(absent_member_message(&violation.path));
    }
    if violation.sensitive {
        return None;
    }
    violation
        .value
        .as_deref()
        .map(|value| invalid_value_message(&violation.path, value))
}

/// One wrapper per operation whose model declares `InvalidParameterValueException`
/// and NOT `ValidationException`, keyed by the name the model names that operation
/// by ([`Operation::NAME`], the same key the generated routing table uses).
///
/// Built once when the service is constructed, never per request; each entry is
/// zero-sized, so this is a table of NAMES rather than a second set of operations —
/// the operation that serves a request is still the one the generated table built
/// from the shared engine.
///
/// Every other operation of this service is absent from the table and keeps the
/// framework's `ValidationException`, which its model declares.
pub(crate) fn modelled_refusal_table<Eng: Send + Sync + 'static>(
) -> HashMap<&'static str, Box<dyn ErasedOperation>> {
    let mut wrappers: HashMap<&'static str, Box<dyn ErasedOperation>> = HashMap::new();
    register::<crate::ops::create_license_configuration::CreateLicenseConfiguration<Eng>>(
        &mut wrappers,
    );
    register::<crate::ops::delete_license_configuration::DeleteLicenseConfiguration<Eng>>(
        &mut wrappers,
    );
    register::<crate::ops::get_license_configuration::GetLicenseConfiguration<Eng>>(&mut wrappers);
    register::<crate::ops::get_license_conversion_task::GetLicenseConversionTask<Eng>>(
        &mut wrappers,
    );
    register::<
        crate::ops::list_associations_for_license_configuration::ListAssociationsForLicenseConfiguration<Eng>,
    >(&mut wrappers);
    register::<
        crate::ops::list_failures_for_license_configuration_operations::ListFailuresForLicenseConfigurationOperations<Eng>,
    >(&mut wrappers);
    register::<crate::ops::list_license_configurations::ListLicenseConfigurations<Eng>>(
        &mut wrappers,
    );
    register::<
        crate::ops::list_license_configurations_for_organization::ListLicenseConfigurationsForOrganization<Eng>,
    >(&mut wrappers);
    register::<crate::ops::list_license_conversion_tasks::ListLicenseConversionTasks<Eng>>(
        &mut wrappers,
    );
    register::<
        crate::ops::list_license_specifications_for_resource::ListLicenseSpecificationsForResource<Eng>,
    >(&mut wrappers);
    register::<crate::ops::list_license_versions::ListLicenseVersions<Eng>>(&mut wrappers);
    register::<crate::ops::list_resource_inventory::ListResourceInventory<Eng>>(&mut wrappers);
    register::<crate::ops::list_usage_for_license_configuration::ListUsageForLicenseConfiguration<Eng>>(
        &mut wrappers,
    );
    register::<crate::ops::update_license_configuration::UpdateLicenseConfiguration<Eng>>(
        &mut wrappers,
    );
    register::<
        crate::ops::update_license_specifications_for_resource::UpdateLicenseSpecificationsForResource<Eng>,
    >(&mut wrappers);
    wrappers
}

/// Register one operation's wrapper under the key the model names that operation by,
/// read off the operation itself so the table cannot be keyed by a hand-typed name
/// that drifts from the model.
fn register<Op: Operation>(wrappers: &mut HashMap<&'static str, Box<dyn ErasedOperation>>) {
    wrappers.insert(Op::NAME, Box::new(ModelledRefusal::<Op>::new()));
}

/// One modelled operation, served so that a request refused before its handler ran
/// is answered in the operation's own modelled `InvalidParameterValueException`
/// rather than under a code the operation does not declare.
///
/// Zero-sized: it holds no engine and no operation instance. It carries the operation
/// TYPE, which is all it needs — the input shape to read the request into and the
/// input `Schema` to walk it against — and hands every request it does not refuse to
/// the one operation instance the generated routing table built from the shared
/// engine. There is therefore no second instance of any operation and no second
/// store.
///
/// A request this wrapper does not refuse is READ TWICE, once here and once in the
/// operation's own sequence, because the framework hands no already-built input to an
/// operation. That cost is deliberate and bounded by the body limits the framework
/// already enforces; it is what buys the refusal a shape only the handler side could
/// otherwise have.
struct ModelledRefusal<Op> {
    operation: PhantomData<fn() -> Op>,
}

impl<Op> ModelledRefusal<Op> {
    const fn new() -> Self {
        Self {
            operation: PhantomData,
        }
    }
}

impl<Op: Operation> ErasedOperation for ModelledRefusal<Op> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r aws_smithy_runtime_api::http::Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, aws_smithy_runtime_api::http::Response> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            // The protocol is read, used and dropped inside this one block:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the
            // await below would make this future non-`Send`.
            let refused = {
                let protocol: &dyn ServerHttpProtocol = svc.definition().protocol();
                // The violations of this request, from the framework's OWN machinery
                // — the protocol's deserializer and the constraint walk over the
                // built input — so this wrapper cannot find a different set from the
                // sequence that follows it, and only the SHAPE and the WORDING of the
                // answer are this service's.
                //
                // The shape carries ONE `Message`, so the first violation is the one
                // stated: it is the parameter the walk refused first, and the request
                // has to be fixed before any later one can be reached.
                let violations =
                    ::service_common::validation::refusing_violations::<Op>(protocol, req);
                violations
                    .iter()
                    .find_map(stated)
                    .and_then(|message| refusal_response(protocol, message, Op::NAME))
            };
            if let Some(response) = refused {
                // The row the framework sequence would have recorded for this
                // request, so a refused one is not a hole in the telemetry series.
                record_refusal::<Op>(
                    ctx,
                    started.elapsed(),
                    INVALID_PARAMETER_VALUE_STATUS,
                    INVALID_PARAMETER_VALUE_EXCEPTION,
                );
                return response;
            }
            // Nothing to state: the request is served by the generated operation,
            // which runs the whole framework sequence over the same bytes. Reached
            // through the routing table rather than through `override_operation`,
            // which would route straight back here.
            match svc.definition().routing_table().get(Op::NAME) {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable in practice: this wrapper is only reachable under the
                // key the generated table registered. Answer the framework's own
                // no-such-operation response rather than inventing one.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    /// `None` — the framework reads the schemes off the MODELLED operation the
    /// request's routing key names (the generated table's instance), which is what
    /// the model bakes them on, so wrapping an operation cannot change which schemes
    /// sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}

/// `message` as this service answers it: the modelled client error, serialized
/// through the model's own schema in the envelope of the protocol the request is
/// served in — the same path a handler-raised `InvalidParameterValueException`
/// takes.
fn refusal_response(
    protocol: &dyn ServerHttpProtocol,
    message: String,
    operation: &'static str,
) -> Option<aws_smithy_runtime_api::http::Response> {
    let refusal = InvalidParameterValueException {
        message: Some(message),
    };
    protocol
        .serialize_error(
            &refusal,
            &crate::ops::types::SCHEMA_INVALID_PARAMETER_VALUE_EXCEPTION,
            INVALID_PARAMETER_VALUE_EXCEPTION,
            INVALID_PARAMETER_VALUE_STATUS,
        )
        .map_err(|error| {
            // A refusal that will not serialize leaves the request to the framework,
            // which refuses it in its own envelope — the very divergence this wrapper
            // removes, so it is logged rather than passed over silently.
            ::tracing::warn!(
                operation,
                %error,
                "this service's modelled refusal would not serialize; it falls back \
                 to the framework's own envelope"
            );
        })
        .ok()
}

/// Record the refusal answered above as the operation row the framework sequence
/// would have recorded, so a request refused by the wrapper is not a hole in the
/// telemetry series: the same classification (from the refusal's status and error
/// shape), the same folded handle (this service's signing name and the operation's
/// own name), and the same already-sanitized request metadata. Everything is gated on
/// a client collecting, exactly as the framework gates it.
fn record_refusal<Op: Operation>(
    ctx: &OperationContext<'_>,
    latency: std::time::Duration,
    status: u16,
    code: &str,
) {
    if !::emulator_telemetry::recording_active() {
        return;
    }
    let handle = ::emulator_telemetry::OperationHandle::folded(
        ctx.identity().signing_name_static(),
        Op::NAME,
    );
    ::emulator_telemetry::record_generated_operation_with_handle(
        &handle,
        latency,
        Some(::emulator_telemetry::OperationFailure::classify(
            status, code,
        )),
        smithy_server_runtime::RequestTelemetry::from_headers(&ctx.parts().headers).into_metadata(),
    );
}
