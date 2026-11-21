// SPDX-License-Identifier: CC0-1.0

//! How this service states a request it refuses BEFORE any handler runs.
//!
//! A request that breaks a rule the MODEL declares — a `@required` member absent, a value
//! outside a declared `@length`, `@range` or `@pattern` — is refused at the boundary, by
//! the framework's own deserializer and constraint walk, before the operation's `handle`
//! is reached. `ValidationException` is an error EVERY operation of this service declares
//! (`smithy_query(query="operation", operation="CreateConfigurationSet")` and its peers),
//! and the model gives it three members: `Message`, `Reason`
//! (`ValidationExceptionReason`, whose value for a member that failed its own constraints
//! is `FIELD_VALIDATION_FAILED`) and `Fields`, a list of
//! `ValidationExceptionField {Name, Message}`. The framework's own refusal body carries an
//! error code and a lowercase `message` and nothing else
//! ([`smithy_server_runtime::Refused`]), so a refusal rendered there can carry neither the
//! reason nor the field list, and not even the modelled `Message` member name.
//!
//! So every modelled operation is served behind [`ModelledValidation`], which collects the
//! violations of a request with the framework's own two steps
//! ([`::service_common::validation::refusing_violations`]) and answers them in this
//! service's modelled shape, and hands every request it does not refuse to the generated
//! operation unchanged.
//!
//! # How this service words one
//!
//! One fixed sentence naming the members it rejected:
//!
//! ```text
//! The following parameter is null, empty or invalid: ['configurationSetName']
//! ```
//!
//! The text up to the colon is fixed; the bracketed, single-quoted list is the wire names
//! of the request's offending members, in the order the walk reports them. Each of those
//! members also gets a `Fields` entry naming it and the requirement it failed —
//! `Member must satisfy regular expression pattern: [A-Za-z0-9_-]+` for a `@pattern`, and
//! the framework's own sentence for every other constraint class
//! ([`::service_common::validation_message::requirement`]), so a requirement this service
//! states and one the framework states are the same sentence. A `@pattern` is quoted
//! WITHOUT the whole-value anchors the model writes around it
//! ([`::service_common::validation::unanchored`]): the expression a refusal advertises is
//! the one a caller can re-validate against.
//!
//! No submitted value is echoed anywhere in the body: this service's refusal names the
//! member and the rule it broke, so a caller's own input never comes back inside it.

use std::collections::HashMap;
use std::marker::PhantomData;

use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, ServerHttpProtocol, Service,
    Violation, ViolationKind,
};

use crate::ops::types::{ValidationException, ValidationExceptionField};

/// The wire error code a refused request is answered with: this service's modelled
/// `ValidationException`, which is also the code the framework's own refusal channel picks
/// for this `awsJson1_0` service. Restated here because this module renders the whole body.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// `ValidationException`'s modelled `@httpError`
/// (`smithy_query(query="shape",shape="ValidationException")`).
const VALIDATION_EXCEPTION_STATUS: u16 = 400;

/// The `ValidationExceptionReason` a member that failed its own modelled constraints earns.
///
/// From `smithy_query(query="shape",shape="ValidationExceptionReason")`, whose value set
/// also carries `CANNOT_PARSE`, `MISSING_PARAMETER`, `INVALID_PARAMETER` and `OTHER`
/// among others: what happened here is that a field was validated and failed, which is
/// exactly this value.
const FIELD_VALIDATION_FAILED: &str = "FIELD_VALIDATION_FAILED";

/// The sentence this service refuses a request with, naming every member it rejected.
///
/// `pub(super)` because the framework's own refusal channel states the same sentence for
/// the few refusals that do not reach this module's renderer (see
/// `crate::service::EmulatedService::override_refusal`), and one wording cannot be written
/// twice.
pub(super) fn parameter_list_message(violations: &[Violation]) -> String {
    let mut named: Vec<String> = Vec::with_capacity(violations.len());
    for violation in violations {
        // One member can fail two constraints; it is one rejected parameter either way.
        let quoted = format!("'{}'", violation.path);
        if !named.contains(&quoted) {
            named.push(quoted);
        }
    }
    format!(
        "The following parameter is null, empty or invalid: [{}]",
        named.join(", ")
    )
}

/// The requirement one violation states: the framework's own sentence for that constraint
/// class, with a `@pattern` quoted as the expression this service advertises rather than
/// as the anchored text the model writes it with.
fn requirement(violation: &Violation) -> String {
    let stated = match &violation.kind {
        ViolationKind::Pattern(pattern) => {
            ViolationKind::Pattern(::service_common::validation::unanchored(pattern))
        }
        other => other.clone(),
    };
    ::service_common::validation_message::requirement(stated)
}

/// One `Fields` entry per rejected member: its wire name and the requirement it failed.
///
/// One entry per MEMBER rather than per violation, so the list and the bracketed names in
/// [`parameter_list_message`] describe the same set; a member that fails two constraints
/// states the first requirement the walk reported for it.
fn rejected_fields(violations: &[Violation]) -> Vec<ValidationExceptionField> {
    let mut fields: Vec<ValidationExceptionField> = Vec::with_capacity(violations.len());
    for violation in violations {
        if fields.iter().any(|field| field.name == violation.path) {
            continue;
        }
        fields.push(ValidationExceptionField {
            name: violation.path.clone(),
            message: requirement(violation),
        });
    }
    fields
}

/// The modelled `ValidationException` for a set of boundary violations, or `None` when
/// there is nothing to state, i.e. no violation.
fn boundary_exception(violations: &[Violation]) -> Option<ValidationException> {
    if violations.is_empty() {
        return None;
    }
    Some(ValidationException {
        message: Some(parameter_list_message(violations)),
        reason: Some(FIELD_VALIDATION_FAILED.to_string()),
        fields: Some(rejected_fields(violations)),
    })
}

/// One modelled operation, served so that a request refused BEFORE its handler runs is
/// answered in this service's modelled `ValidationException` — with the `Reason` and
/// `Fields` the model declares — rather than in the framework's refusal body, which
/// carries an error code and a lowercase `message` and nothing else.
///
/// Zero-sized: the wrapper holds no engine and no operation instance. It carries the
/// operation TYPE, which is all it needs — the input shape to read the request into and
/// the input `Schema` to read it against — and hands a request the model accepts to the one
/// operation instance the generated routing table built from the shared engine. There is
/// therefore no second instance of any operation and no second store.
///
/// A request this wrapper does not refuse is READ TWICE, once here and once in the
/// operation's own sequence, because the framework hands no already-built input to an
/// operation. That cost is deliberate and bounded by the body limits the framework already
/// enforces; it is what buys the refusal a shape only the handler side could otherwise
/// have.
struct ModelledValidation<Op> {
    operation: PhantomData<fn() -> Op>,
}

impl<Op> ModelledValidation<Op> {
    const fn new() -> Self {
        Self {
            operation: PhantomData,
        }
    }
}

impl<Op: Operation> ErasedOperation for ModelledValidation<Op> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r aws_smithy_runtime_api::http::Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, aws_smithy_runtime_api::http::Response> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            // The protocol is read, used and dropped inside this one block:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the await
            // below would make this future non-`Send`.
            let refused = {
                let protocol: &dyn ServerHttpProtocol = svc.definition().protocol();
                // The framework's own two steps, in its order: read the request into the
                // operation's input shape, then walk the constraint traits over what was
                // read. A failure that is not a refusal — a body the codec cannot read at
                // all — yields no violation and keeps the framework's own rendering.
                let violations =
                    ::service_common::validation::refusing_violations::<Op>(protocol, req);
                match boundary_exception(&violations) {
                    Some(modelled) => match protocol.serialize_error(
                        &modelled,
                        &crate::ops::types::SCHEMA_VALIDATION_EXCEPTION,
                        VALIDATION_EXCEPTION,
                        VALIDATION_EXCEPTION_STATUS,
                    ) {
                        Ok(response) => Some(response),
                        // A refusal that will not serialize leaves the request to the
                        // framework, which refuses it in its own envelope — the very
                        // divergence this wrapper removes, so it is logged rather than
                        // passed over silently.
                        Err(error) => {
                            tracing::warn!(
                                operation = Op::NAME,
                                %error,
                                "this service's modelled refusal would not serialize; it \
                                 falls back to the framework's own envelope"
                            );
                            None
                        }
                    },
                    None => None,
                }
            };
            if let Some(response) = refused {
                // The row the framework sequence would have recorded for this request, so
                // a refused one is not a hole in the telemetry series.
                record_refusal::<Op>(ctx, started.elapsed());
                return response;
            }
            // Nothing to state: the request is served by the generated operation, which
            // runs the whole framework sequence (deserialize, validate, handle, serialize)
            // plus its telemetry over the same bytes. Reached through the routing table
            // rather than through `override_operation`, which would route straight back
            // here.
            match svc.definition().routing_table().get(Op::NAME) {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable in practice: this wrapper is only reachable under the key the
                // generated table registered. Answer the framework's own no-such-operation
                // response rather than inventing one.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    /// `None` — the framework reads the schemes off the MODELLED operation the request's
    /// routing key names (the generated table's instance), which is what the model bakes
    /// them on, so wrapping an operation cannot change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}

/// Record the refusal answered above as the operation row the framework sequence would
/// have recorded, so a request refused by the wrapper is not a hole in the telemetry
/// series: the same classification (from the refusal's status and error shape), the same
/// folded handle (this service's signing name and the operation's own name), and the same
/// already-sanitized request metadata. Everything is gated on a client collecting, exactly
/// as the framework gates it.
fn record_refusal<Op: Operation>(ctx: &OperationContext<'_>, latency: std::time::Duration) {
    if !emulator_telemetry::recording_active() {
        return;
    }
    let handle =
        emulator_telemetry::OperationHandle::folded(ctx.identity().signing_name_static(), Op::NAME);
    emulator_telemetry::record_generated_operation_with_handle(
        &handle,
        latency,
        Some(emulator_telemetry::OperationFailure::classify(
            VALIDATION_EXCEPTION_STATUS,
            VALIDATION_EXCEPTION,
        )),
        smithy_server_runtime::RequestTelemetry::from_headers(&ctx.parts().headers).into_metadata(),
    );
}

/// Register one operation's modelled-refusal wrapper under the key the model names that
/// operation by, read off the operation itself so the table cannot be keyed by a
/// hand-typed name that drifts from the model.
fn register<Op: Operation>(refusals: &mut HashMap<&'static str, Box<dyn ErasedOperation>>) {
    refusals.insert(Op::NAME, Box::new(ModelledValidation::<Op>::new()));
}

/// The modelled-refusal wrapper for EVERY operation this service serves.
///
/// `ValidationException` is an error every one of this service's operations declares, so
/// every operation states a request refused before its handler ran the same way, and there
/// is no operation whose boundary refusal may answer in the framework's envelope instead.
/// An operation added to the model later is wrapped by adding one line here.
pub(super) fn modelled_validation_table<Eng: Send + Sync + 'static>(
) -> HashMap<&'static str, Box<dyn ErasedOperation>> {
    let mut refusals: HashMap<&'static str, Box<dyn ErasedOperation>> = HashMap::new();
    register::<crate::ops::associate_origination_identity::AssociateOriginationIdentity<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::associate_protect_configuration::AssociateProtectConfiguration<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::carrier_lookup::CarrierLookup<Eng>>(&mut refusals);
    register::<crate::ops::create_configuration_set::CreateConfigurationSet<Eng>>(&mut refusals);
    register::<crate::ops::create_event_destination::CreateEventDestination<Eng>>(&mut refusals);
    register::<crate::ops::create_notify_configuration::CreateNotifyConfiguration<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::create_opt_out_list::CreateOptOutList<Eng>>(&mut refusals);
    register::<crate::ops::create_pool::CreatePool<Eng>>(&mut refusals);
    register::<crate::ops::create_protect_configuration::CreateProtectConfiguration<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::create_rcs_agent::CreateRcsAgent<Eng>>(&mut refusals);
    register::<crate::ops::create_registration::CreateRegistration<Eng>>(&mut refusals);
    register::<crate::ops::create_registration_association::CreateRegistrationAssociation<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::create_registration_attachment::CreateRegistrationAttachment<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::create_registration_version::CreateRegistrationVersion<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::create_verified_destination_number::CreateVerifiedDestinationNumber<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::delete_account_default_protect_configuration::DeleteAccountDefaultProtectConfiguration<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::delete_configuration_set::DeleteConfigurationSet<Eng>>(&mut refusals);
    register::<crate::ops::delete_default_message_type::DeleteDefaultMessageType<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::delete_default_sender_id::DeleteDefaultSenderId<Eng>>(&mut refusals);
    register::<crate::ops::delete_event_destination::DeleteEventDestination<Eng>>(&mut refusals);
    register::<crate::ops::delete_keyword::DeleteKeyword<Eng>>(&mut refusals);
    register::<
        crate::ops::delete_media_message_spend_limit_override::DeleteMediaMessageSpendLimitOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::delete_notify_configuration::DeleteNotifyConfiguration<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::delete_notify_message_spend_limit_override::DeleteNotifyMessageSpendLimitOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::delete_opt_out_list::DeleteOptOutList<Eng>>(&mut refusals);
    register::<crate::ops::delete_opted_out_number::DeleteOptedOutNumber<Eng>>(&mut refusals);
    register::<crate::ops::delete_pool::DeletePool<Eng>>(&mut refusals);
    register::<crate::ops::delete_protect_configuration::DeleteProtectConfiguration<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::delete_protect_configuration_rule_set_number_override::DeleteProtectConfigurationRuleSetNumberOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::delete_rcs_agent::DeleteRcsAgent<Eng>>(&mut refusals);
    register::<
        crate::ops::delete_rcs_message_spend_limit_override::DeleteRcsMessageSpendLimitOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::delete_registration::DeleteRegistration<Eng>>(&mut refusals);
    register::<crate::ops::delete_registration_attachment::DeleteRegistrationAttachment<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::delete_registration_field_value::DeleteRegistrationFieldValue<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::delete_resource_policy::DeleteResourcePolicy<Eng>>(&mut refusals);
    register::<
        crate::ops::delete_text_message_spend_limit_override::DeleteTextMessageSpendLimitOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<
        crate::ops::delete_verified_destination_number::DeleteVerifiedDestinationNumber<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::delete_voice_message_spend_limit_override::DeleteVoiceMessageSpendLimitOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::describe_account_attributes::DescribeAccountAttributes<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::describe_account_limits::DescribeAccountLimits<Eng>>(&mut refusals);
    register::<crate::ops::describe_configuration_sets::DescribeConfigurationSets<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::describe_keywords::DescribeKeywords<Eng>>(&mut refusals);
    register::<crate::ops::describe_notify_configurations::DescribeNotifyConfigurations<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::describe_notify_templates::DescribeNotifyTemplates<Eng>>(&mut refusals);
    register::<crate::ops::describe_opt_out_lists::DescribeOptOutLists<Eng>>(&mut refusals);
    register::<crate::ops::describe_opted_out_numbers::DescribeOptedOutNumbers<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::describe_phone_numbers::DescribePhoneNumbers<Eng>>(&mut refusals);
    register::<crate::ops::describe_pools::DescribePools<Eng>>(&mut refusals);
    register::<crate::ops::describe_protect_configurations::DescribeProtectConfigurations<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::describe_rcs_agent_country_launch_status::DescribeRcsAgentCountryLaunchStatus<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::describe_rcs_agents::DescribeRcsAgents<Eng>>(&mut refusals);
    register::<
        crate::ops::describe_registration_attachments::DescribeRegistrationAttachments<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::describe_registration_field_definitions::DescribeRegistrationFieldDefinitions<
            Eng,
        >,
    >(&mut refusals);
    register::<
        crate::ops::describe_registration_field_values::DescribeRegistrationFieldValues<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::describe_registration_section_definitions::DescribeRegistrationSectionDefinitions<
            Eng,
        >,
    >(&mut refusals);
    register::<
        crate::ops::describe_registration_type_definitions::DescribeRegistrationTypeDefinitions<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::describe_registration_versions::DescribeRegistrationVersions<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::describe_registrations::DescribeRegistrations<Eng>>(&mut refusals);
    register::<crate::ops::describe_sender_ids::DescribeSenderIds<Eng>>(&mut refusals);
    register::<crate::ops::describe_spend_limits::DescribeSpendLimits<Eng>>(&mut refusals);
    register::<
        crate::ops::describe_verified_destination_numbers::DescribeVerifiedDestinationNumbers<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::disassociate_origination_identity::DisassociateOriginationIdentity<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::disassociate_protect_configuration::DisassociateProtectConfiguration<Eng>,
    >(&mut refusals);
    register::<crate::ops::discard_registration_version::DiscardRegistrationVersion<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::get_protect_configuration_country_rule_set::GetProtectConfigurationCountryRuleSet<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::get_resource_policy::GetResourcePolicy<Eng>>(&mut refusals);
    register::<crate::ops::list_available_phone_numbers::ListAvailablePhoneNumbers<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::list_notify_countries::ListNotifyCountries<Eng>>(&mut refusals);
    register::<crate::ops::list_pool_origination_identities::ListPoolOriginationIdentities<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::list_protect_configuration_rule_set_number_overrides::ListProtectConfigurationRuleSetNumberOverrides<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::list_registration_associations::ListRegistrationAssociations<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::list_tags_for_resource::ListTagsForResource<Eng>>(&mut refusals);
    register::<crate::ops::put_keyword::PutKeyword<Eng>>(&mut refusals);
    register::<crate::ops::put_message_feedback::PutMessageFeedback<Eng>>(&mut refusals);
    register::<crate::ops::put_opted_out_number::PutOptedOutNumber<Eng>>(&mut refusals);
    register::<
        crate::ops::put_protect_configuration_rule_set_number_override::PutProtectConfigurationRuleSetNumberOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::put_registration_field_value::PutRegistrationFieldValue<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::put_resource_policy::PutResourcePolicy<Eng>>(&mut refusals);
    register::<crate::ops::release_phone_number::ReleasePhoneNumber<Eng>>(&mut refusals);
    register::<crate::ops::release_sender_id::ReleaseSenderId<Eng>>(&mut refusals);
    register::<crate::ops::request_phone_number::RequestPhoneNumber<Eng>>(&mut refusals);
    register::<crate::ops::request_sender_id::RequestSenderId<Eng>>(&mut refusals);
    register::<
        crate::ops::send_destination_number_verification_code::SendDestinationNumberVerificationCode<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::send_media_message::SendMediaMessage<Eng>>(&mut refusals);
    register::<crate::ops::send_notify_text_message::SendNotifyTextMessage<Eng>>(&mut refusals);
    register::<crate::ops::send_notify_voice_message::SendNotifyVoiceMessage<Eng>>(&mut refusals);
    register::<crate::ops::send_rcs_message::SendRcsMessage<Eng>>(&mut refusals);
    register::<crate::ops::send_text_message::SendTextMessage<Eng>>(&mut refusals);
    register::<crate::ops::send_voice_message::SendVoiceMessage<Eng>>(&mut refusals);
    register::<
        crate::ops::set_account_default_protect_configuration::SetAccountDefaultProtectConfiguration<
            Eng,
        >,
    >(&mut refusals);
    register::<
        crate::ops::set_default_message_feedback_enabled::SetDefaultMessageFeedbackEnabled<Eng>,
    >(&mut refusals);
    register::<crate::ops::set_default_message_type::SetDefaultMessageType<Eng>>(&mut refusals);
    register::<crate::ops::set_default_sender_id::SetDefaultSenderId<Eng>>(&mut refusals);
    register::<
        crate::ops::set_media_message_spend_limit_override::SetMediaMessageSpendLimitOverride<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::set_notify_message_spend_limit_override::SetNotifyMessageSpendLimitOverride<
            Eng,
        >,
    >(&mut refusals);
    register::<
        crate::ops::set_rcs_message_spend_limit_override::SetRcsMessageSpendLimitOverride<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::set_text_message_spend_limit_override::SetTextMessageSpendLimitOverride<Eng>,
    >(&mut refusals);
    register::<
        crate::ops::set_voice_message_spend_limit_override::SetVoiceMessageSpendLimitOverride<Eng>,
    >(&mut refusals);
    register::<crate::ops::submit_registration_version::SubmitRegistrationVersion<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::tag_resource::TagResource<Eng>>(&mut refusals);
    register::<crate::ops::untag_resource::UntagResource<Eng>>(&mut refusals);
    register::<crate::ops::update_event_destination::UpdateEventDestination<Eng>>(&mut refusals);
    register::<crate::ops::update_notify_configuration::UpdateNotifyConfiguration<Eng>>(
        &mut refusals,
    );
    register::<crate::ops::update_phone_number::UpdatePhoneNumber<Eng>>(&mut refusals);
    register::<crate::ops::update_pool::UpdatePool<Eng>>(&mut refusals);
    register::<crate::ops::update_protect_configuration::UpdateProtectConfiguration<Eng>>(
        &mut refusals,
    );
    register::<
        crate::ops::update_protect_configuration_country_rule_set::UpdateProtectConfigurationCountryRuleSet<
            Eng,
        >,
    >(&mut refusals);
    register::<crate::ops::update_rcs_agent::UpdateRcsAgent<Eng>>(&mut refusals);
    register::<crate::ops::update_sender_id::UpdateSenderId<Eng>>(&mut refusals);
    register::<crate::ops::verify_destination_number::VerifyDestinationNumber<Eng>>(&mut refusals);
    refusals
}

#[cfg(test)]
mod tests {
    //! The parts of this renderer that need no request: the sentence a set of violations is
    //! stated with, and the requirement one violation carries. The end-to-end half — which
    //! body a refused request receives over the wire — is
    //! `tests/all/modelled_validation_refusals.rs`.

    use super::*;

    /// One violation of `kind` at `path`, as the framework's walk reports it.
    fn violation(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: None,
            sensitive: false,
        }
    }

    /// Every operation of this service is wrapped, so the table is keyed by one name per
    /// modelled operation and none of them collides.
    #[test]
    fn the_table_wraps_every_modelled_operation() {
        let table = modelled_validation_table::<::engine::InMemoryEngine>();
        assert!(
            table.contains_key("CreateConfigurationSet"),
            "the table holds CreateConfigurationSet, got {:?} entries",
            table.len()
        );
        assert!(
            table.contains_key("PutOptedOutNumber"),
            "the table holds PutOptedOutNumber, got {:?} entries",
            table.len()
        );
        assert_eq!(
            table.len(),
            110,
            "one entry per modelled operation, got {}",
            table.len()
        );
    }

    /// The sentence names each rejected member once, in the order reported.
    #[test]
    fn the_sentence_names_every_rejected_member_once() {
        let cases: Vec<(&str, Vec<Violation>, &str)> = vec![
            (
                "one member",
                vec![violation(
                    "configurationSetName",
                    ViolationKind::Pattern("^[A-Za-z0-9_-]+$".to_string()),
                )],
                "The following parameter is null, empty or invalid: ['configurationSetName']",
            ),
            (
                "two members",
                vec![
                    violation("optOutListName", ViolationKind::MinLength(1)),
                    violation(
                        "optedOutNumber",
                        ViolationKind::Pattern("^\\+?[1-9][0-9]{1,18}$".to_string()),
                    ),
                ],
                "The following parameter is null, empty or invalid: ['optOutListName', \
                 'optedOutNumber']",
            ),
            (
                "one member failing two constraints",
                vec![
                    violation("optedOutNumber", ViolationKind::MaxLength(20)),
                    violation(
                        "optedOutNumber",
                        ViolationKind::Pattern("^\\+?[1-9][0-9]{1,18}$".to_string()),
                    ),
                ],
                "The following parameter is null, empty or invalid: ['optedOutNumber']",
            ),
        ];
        for (case, violations, expected) in cases {
            let stated = parameter_list_message(&violations);
            assert_eq!(stated, expected, "case {case:?}: got {stated:?}");
        }
    }

    /// A `@pattern` is quoted without the model's whole-value anchors; every other class
    /// keeps the framework's own sentence.
    #[test]
    fn a_requirement_quotes_the_pattern_this_service_advertises() {
        let cases: [(&str, ViolationKind, &str); 4] = [
            (
                "an anchored pattern",
                ViolationKind::Pattern("^[A-Za-z0-9_-]+$".to_string()),
                "Member must satisfy regular expression pattern: [A-Za-z0-9_-]+",
            ),
            (
                "a pattern with an escaped character",
                ViolationKind::Pattern("^\\+?[1-9][0-9]{1,18}$".to_string()),
                "Member must satisfy regular expression pattern: \\+?[1-9][0-9]{1,18}",
            ),
            (
                "an absent required member",
                ViolationKind::MissingRequiredMember,
                "Member must not be null",
            ),
            (
                "a value past a declared maximum length",
                ViolationKind::MaxLength(64),
                "Member must have length less than or equal to 64",
            ),
        ];
        for (case, kind, expected) in cases {
            let stated = requirement(&violation("member", kind));
            assert_eq!(stated, expected, "case {case:?}: got {stated:?}");
        }
    }

    /// The exception states the reason and one field per rejected member.
    #[test]
    fn the_exception_carries_the_reason_and_one_field_per_member() {
        assert!(
            boundary_exception(&[]).is_none(),
            "no violation is nothing to state"
        );
        let stated = boundary_exception(&[violation(
            "optedOutNumber",
            ViolationKind::Pattern("^\\+?[1-9][0-9]{1,18}$".to_string()),
        )])
        .expect("a violation is stated");
        assert_eq!(
            stated.reason.as_deref(),
            Some(FIELD_VALIDATION_FAILED),
            "a member that failed its constraints is a field-validation failure, got {:?}",
            stated.reason
        );
        let fields = stated.fields.expect("the fields are stated");
        assert_eq!(fields.len(), 1, "one field, got {}", fields.len());
        assert_eq!(
            fields[0].name, "optedOutNumber",
            "the field names the member, got {:?}",
            fields[0].name
        );
        assert_eq!(
            fields[0].message,
            "Member must satisfy regular expression pattern: \\+?[1-9][0-9]{1,18}",
            "got {:?}",
            fields[0].message
        );
    }
}
