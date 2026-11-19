// SPDX-License-Identifier: CC0-1.0

//! How this service words the refusals the FRAMEWORK gives for the rules the MODEL declares —
//! an absent `@required` member, a value outside a declared enum catalog or `@range` — in EC2's
//! own vocabulary.
//!
//! EC2 declares no error SHAPES at all (`smithy_query(query="operation", …)` reports
//! `errors: []` for every operation), so the ec2Query `<Code>` is the whole contract a client
//! can branch on: the framework's own model-constraint codes are not part of that vocabulary
//! and no EC2 client error handler matches them. What this module answers is therefore EC2's
//! code and EC2's sentence for the refusal CLASS, and the class is read off the violation the
//! constraint walk reports — never off the operation that happened to carry it, except where
//! EC2's own wording for one differs and was recorded ([`crate::state::is_modelled_catalog`]
//! and [`crate::state::RECORDED_PAGE_SIZE_WORDINGS`] record both rules).
//!
//! A refusal class whose EC2 wording this service has not recorded answers `None`, which keeps
//! the framework's own code and its request-built clause rather than inventing a second dialect
//! for a parameter whose EC2 words are not known.
//!
//! ## The input shape the refusal belongs to
//!
//! Two of EC2's sentences differ by the OPERATION and not by the value — the page-size ceiling
//! is one — and the framework hands `override_refusal` the violations alone. The input shape
//! being read is therefore held here for the span of the request walk, put there by the one
//! place this service reads a request ([`Ec2QueryEnvelope::deserialize_input`]) and read back
//! by the wording below.

use std::cell::RefCell;

use super::*;

thread_local! {
    /// The INPUT SHAPE whose request this thread is reading, as the model names it
    /// (`DescribeSubnetsRequest`) — the key EC2's per-operation wordings are recorded under,
    /// and the same key a member read without regard to case is recorded under
    /// (`crate::state::CASE_INSENSITIVE_MEMBERS`).
    ///
    /// # The invariant this rests on
    ///
    /// The two sides of it are ONE poll apart and nothing is awaited between them: the
    /// framework's sequence reads the request through [`Ec2QueryEnvelope::deserialize_input`],
    /// walks the value it built against the model's rules, and asks the service to word any
    /// violation it found, all without yielding
    /// (`smithy_server_runtime::run_operation_http`). So the shape read back here is the shape
    /// of the request being refused.
    ///
    /// Nothing is left to be quietly wrong if that ever stops holding: the run about to serve a
    /// call CLEARS the slot ([`forget_input_being_read`]) before anything is dispatched, and an
    /// empty slot words the refusal the way every operation whose own wording is not recorded
    /// is worded — never with another request's shape.
    static INPUT_BEING_READ: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Hold `input_shape` as the input this thread is reading.
pub fn reading_input(input_shape: &str) {
    INPUT_BEING_READ.with(|slot| *slot.borrow_mut() = Some(input_shape.to_string()));
}

/// Forget whatever input shape this thread may still hold, before a call is served.
pub fn forget_input_being_read() {
    INPUT_BEING_READ.with(|slot| *slot.borrow_mut() = None);
}

/// The input shape this thread is reading, if it has read one.
fn input_being_read() -> Option<String> {
    INPUT_BEING_READ.with(|slot| slot.borrow().clone())
}

/// EC2's own code and sentence for the refusal `violations` describe, or `None` when none of
/// them is a refusal this service has EC2's wording for.
///
/// EC2 reports ONE parameter at a time, so a request that failed several constraints is refused
/// with the first parameter this service can state — except for the one refusal that is about a
/// LIST of values rather than about one value, which answers for every entry at fault
/// ([`nonexistent_instance_types`]) and is therefore asked first.
pub(super) fn ec2_refusal(violations: &[aws_smithy_schema_server::Violation]) -> Option<Refused> {
    nonexistent_instance_types(violations).or_else(|| violations.iter().find_map(ec2_value_refusal))
}

/// EC2's `InvalidInstanceType` for a request that asked the catalog about instance types it
/// does not hold, or `None` when no violation reports one.
///
/// A member that names instance types for a report whose SUBJECT is the catalog is asking the
/// catalog ABOUT those types, so a name the catalog does not hold is a type that does not
/// exist — which EC2 answers with its own `InvalidInstanceType`, naming every type the request
/// asked about that does not exist ([`crate::state::nonexistent_instance_types_message`]). A
/// member that names types to NARROW a report of something else, or the type a resource is
/// PLACED as, is a different refusal: there the type is the value of a parameter, and EC2
/// answers `InvalidParameterValue` for it ([`ec2_value_refusal`]).
///
/// The two are told apart by the member the constraint walk reports — the LIST of types a
/// report covers ([`crate::state::INSTANCE_TYPES_ELEMENT_PATH_PREFIX`]) against the single type
/// every create carries — and by the input being read, which says whether the catalog is what
/// the operation reports on ([`crate::state::INSTANCE_TYPE_CATALOG_LOOKUP_INPUT_SHAPES`]). Each
/// entry is reported in the order the request named it, because the lookup was of a list.
fn nonexistent_instance_types(
    violations: &[aws_smithy_schema_server::Violation],
) -> Option<Refused> {
    use aws_smithy_schema_server::ViolationKind;

    let input_shape = input_being_read()?;
    if !crate::state::INSTANCE_TYPE_CATALOG_LOOKUP_INPUT_SHAPES.contains(&input_shape.as_str()) {
        return None;
    }
    let unknown: Vec<String> = violations
        .iter()
        .filter(|violation| {
            violation
                .path
                .starts_with(crate::state::INSTANCE_TYPES_ELEMENT_PATH_PREFIX)
                && matches!(&violation.kind,
                    ViolationKind::EnumValue(allowed)
                        if crate::state::is_instance_type_catalog(allowed))
        })
        // Already elided to the framework's `MAX_ECHOED_VALUE`, and the list itself is bounded
        // by the member's own `@length`, so naming them cannot amplify a request into an
        // arbitrarily long response.
        .map(|violation| violation.value.clone().unwrap_or_default())
        .collect();
    if unknown.is_empty() {
        return None;
    }
    Some(Refused {
        code: crate::state::INVALID_INSTANCE_TYPE.to_string(),
        message: crate::state::nonexistent_instance_types_message(&unknown),
    })
}

/// EC2's own code and sentence for a value this service knows how EC2 refuses, or `None`
/// when the refusal has no recorded EC2 wording.
///
/// EC2 words a refused value per parameter TYPE rather than as a validation clause —
/// "`<value>` is not a valid value for RoleType. Specify either odcr-owner or
/// unused-reservation-billing-owner." for a value outside the `CallerRole` catalog. The
/// wording lives with the type it belongs to ([`crate::state::caller_role_refusal_message`]),
/// keyed on the value set the constraint walk reports, so every operation that carries that
/// member is refused in the same words and nothing here keys on an operation or a request's
/// shape. A type whose EC2 wording this service has not recorded answers `None` and keeps
/// the framework's own clause.
///
/// A value outside the burstable `UnlimitedSupportedInstanceFamily` catalog is the second
/// catalog this service has EC2's wording for: EC2 answers its own `InvalidInstanceFamily`
/// with the sentence it words for that parameter type
/// ([`crate::state::instance_family_refusal_message`]), keyed on that catalog the same way.
///
/// A `Mode` outside the per-VPC `VpcEncryptionControlMode` catalog is the third: EC2 answers
/// its own `InvalidParameterValue` with the sentence
/// [`crate::ops::create_vpc_encryption_control::vpc_encryption_controls::vpc_encryption_control_mode_refusal_message`]
/// words, which is also what that operation's own backstop answers with.
///
/// A value outside the `InstanceType` catalog is the fourth, and the one every member of this
/// model that names the type an instance is PLACED as carries: EC2 answers its own
/// `InvalidParameterValue` naming the value and the member that refused it
/// ([`crate::state::instance_type_refusal_message`]), keyed on that catalog the same way — so
/// a launch, a Spot Instance request, a Spot Fleet pool and a launch template's own data are
/// each refused an unpublished type identically.
///
/// A `MaxResults` below the smallest page the operation publishes is the other refusal this
/// service states: EC2 answers its own code and sentence for it, naming the value and the
/// bound ([`crate::state::max_results_refusal`]). It is keyed on the member the constraint
/// walk reports, on the bound it failed and on the value it carried — a positive page size
/// the operation will not page with is the code recorded for that operation
/// ([`crate::state::recorded_page_size_code`]) or `InvalidMaxResults`, a value that is no
/// page size at all is `InvalidParameter` — so every paged operation of this service answers
/// one code and one wording for each of the two, the minimum coming from the violation rather
/// than from a number written here.
///
/// A `MaxResults` ABOVE the largest page it publishes is that refusal's other half: a page
/// size is a parameter whose value EC2 cannot use, which it answers `InvalidParameterValue`
/// for, naming the value, the parameter and the ceiling
/// ([`crate::state::max_results_above_maximum_message`]). The ceiling comes from the
/// violation — the member's own `@range` — and only the two words EC2 was recorded varying
/// per operation come from the input shape being read.
///
/// An absent `@required` member is the one refusal here that is about no value at all: EC2
/// answers its own `MissingParameter` in the sentence the validator of that operation's SURFACE
/// words ([`crate::state::absent_required_member_message`]) — the core parameter validator's
/// `The request must contain the parameter <parameter>` for the surfaces of the original EC2 and
/// VPC objects, the later surfaces' `The request must include the <Member> parameter. Add the
/// required parameter and retry the request.` otherwise, and a required filter list's own
/// non-empty sentence for a member that selects what the operation reports. That is the sentence
/// this service's handlers answer for a member they resolve as absent themselves, so a caller
/// reads one code and one wording whichever side of dispatch found the absence.
pub(super) fn ec2_value_refusal(violation: &aws_smithy_schema_server::Violation) -> Option<Refused> {
    use aws_smithy_schema_server::ViolationKind;

    match &violation.kind {
        ViolationKind::MissingRequiredMember => Some(Refused {
            code: crate::state::MISSING_PARAMETER.to_string(),
            // The sentence EC2's own validator for this operation's surface words, read off the
            // member the walk reports and the input being read
            // (`crate::state::absent_required_member_message`) — which is the sentence this
            // service's handlers answer for a member they resolve as absent themselves, so a
            // caller reads one code and one wording whichever side of dispatch found the
            // absence. Nothing of the request is echoed: an absent member carried no value.
            message: crate::state::absent_required_member_message(
                input_being_read().as_deref(),
                &violation.path,
            ),
        }),
        ViolationKind::EnumValue(allowed) if crate::state::is_caller_role_catalog(allowed) => {
            // Already elided to the framework's `MAX_ECHOED_VALUE`, so echoing it cannot
            // amplify a request into an arbitrarily long response.
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message: crate::state::caller_role_refusal_message(
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::state::is_unlimited_supported_instance_family_catalog(allowed) =>
        {
            // A family outside the burstable catalog is EC2's own `InvalidInstanceFamily`
            // rather than a validation clause, in the sentence EC2 words for that parameter
            // type ([`crate::state::instance_family_refusal_message`], which the handlers
            // that resolve a family answer with too). The value it names is already elided
            // to the framework's `MAX_ECHOED_VALUE`, so echoing it cannot amplify a request
            // into an arbitrarily long response.
            Some(Refused {
                code: crate::state::INVALID_INSTANCE_FAMILY.to_string(),
                message: crate::state::instance_family_refusal_message(
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::ops::create_vpc_encryption_control::vpc_encryption_controls::
                is_vpc_encryption_control_mode_catalog(allowed) =>
        {
            // A `Mode` outside the per-VPC `VpcEncryptionControlMode` catalog is EC2's own
            // `InvalidParameterValue` rather than a validation clause, in the sentence EC2
            // words for that parameter (the one `ModifyVpcEncryptionControl`'s own backstop
            // answers with, so the two cannot disagree). That sentence states the catalog and
            // echoes no part of the request at all.
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message:
                    crate::ops::create_vpc_encryption_control::vpc_encryption_controls::
                        vpc_encryption_control_mode_refusal_message(),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::state::is_output_format_catalog(allowed) =>
        {
            // A format outside the `OutputFormat` catalog is EC2's own
            // `InvalidParameterValue` rather than a validation clause, in the sentence EC2
            // words for that parameter — it names the parameter and offers the formats it
            // accepts ([`crate::state::output_format_refusal_message`]). The value it names
            // is already elided to the framework's `MAX_ECHOED_VALUE`, so echoing it cannot
            // amplify a request into an arbitrarily long response.
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message: crate::state::output_format_refusal_message(
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::state::is_capacity_reservation_instance_platform_catalog(allowed) =>
        {
            // A platform outside the `CapacityReservationInstancePlatform` catalog is EC2's
            // own `InvalidParameterValue` rather than a validation clause, in the sentence
            // EC2 words for that parameter type
            // ([`crate::state::capacity_reservation_instance_platform_refusal_message`]).
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message: crate::state::capacity_reservation_instance_platform_refusal_message(
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::state::is_report_instance_reason_codes_catalog(allowed) =>
        {
            // A reason outside the `ReportInstanceReasonCodes` catalog is EC2's own
            // `InvalidParameterValue` rather than a validation clause, in the sentence EC2
            // words for that parameter type
            // ([`crate::state::report_instance_reason_code_refusal_message`]).
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message: crate::state::report_instance_reason_code_refusal_message(
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::state::is_instance_type_catalog(allowed) =>
        {
            // An instance type outside the catalog the model publishes is EC2's own
            // `InvalidParameterValue` rather than a validation clause, in the sentence EC2
            // words for that parameter type ([`crate::state::instance_type_refusal_message`]).
            // Keyed on the catalog, so every member of this model that names the type an
            // instance is placed as — a launch's own, a Spot Instance request's and a Spot
            // Fleet pool's launch specification, a launch template's data — is refused in the
            // same words. The value it names is already elided to the framework's
            // `MAX_ECHOED_VALUE`, so echoing it cannot amplify a request into an arbitrarily
            // long response — and the catalog itself is NOT stated back, which is what makes
            // this sentence EC2's rather than the framework's clause.
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message: crate::state::instance_type_refusal_message(
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::state::is_flow_log_parameter_catalog(allowed) =>
        {
            // A value outside one of the flow-log surface's own catalogs is refused by the
            // PARAMETER layer that surface validates its members through — the layer whose
            // clause a refused `Filter` name and a refused `FlowLogId` carry
            // ([`crate::state::parameter_format_violation_message`]) — rather than reported as
            // a model-constraint violation: EC2's `InvalidParameterValue` with that clause,
            // naming the parameter by the path the walk reports and the value the request
            // sent. The value is already elided to the framework's `MAX_ECHOED_VALUE`, so
            // echoing it cannot amplify a request into an arbitrarily long response, and the
            // catalog itself is not stated back.
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message: crate::state::parameter_format_violation_message(
                    &violation.path,
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::EnumValue(allowed)
            if crate::state::is_instance_bandwidth_weighting_catalog(allowed) =>
        {
            // A weighting outside the `InstanceBandwidthWeighting` catalog is EC2's own
            // `InvalidParameterValue` rather than a validation clause, in the sentence this
            // crate words an unpublished catalog value with
            // ([`crate::state::instance_bandwidth_weighting_refusal_message`]) — which is the
            // sentence the handlers that resolve the member answer, so a caller reads one
            // wording whichever side of dispatch refused the value. Keyed on the catalog, so
            // a launch's own network performance options and the modify of one instance's are
            // refused identically. The value it names is already elided to the framework's
            // `MAX_ECHOED_VALUE`, so echoing it cannot amplify a request into an arbitrarily
            // long response.
            Some(Refused {
                code: crate::state::INVALID_PARAMETER_VALUE.to_string(),
                message: crate::state::instance_bandwidth_weighting_refusal_message(
                    violation.value.as_deref().unwrap_or_default(),
                ),
            })
        }
        ViolationKind::MinValue(minimum)
            if violation.path == crate::state::MAX_RESULTS_MEMBER_PATH =>
        {
            let value = violation.value.as_deref().unwrap_or_default();
            // Which code and which sentence depend on what the value IS — a page size the
            // operation will not page with, or no page size at all
            // ([`crate::state::max_results_refusal`], which answers both so they cannot
            // disagree). Only the two words EC2 was recorded varying per operation come from
            // the input shape this walk is reading.
            let (code, message) = crate::state::max_results_refusal(
                input_being_read().as_deref(),
                &violation.path,
                value,
                minimum,
            );
            Some(Refused {
                code: code.to_string(),
                message,
            })
        }
        ViolationKind::MaxValue(maximum)
            if violation.path == crate::state::MAX_RESULTS_MEMBER_PATH =>
        {
            // A page size the operation will not page with because it is too LARGE: EC2's own
            // code for a page size it cannot use, naming the value, the parameter and the
            // bound the member's own `@range` publishes — so an operation with another
            // ceiling is refused with its own. Which code, and the two words EC2 varies in
            // that sentence, come from the input shape this walk is reading
            // ([`crate::state::recorded_page_size_code`],
            // [`crate::state::page_size_outside_range_message`]).
            let input_shape = input_being_read();
            let code = crate::state::recorded_page_size_code(
                input_shape.as_deref(),
                crate::state::PageSizeBound::Largest,
            )
            .unwrap_or(crate::state::INVALID_MAX_RESULTS);
            Some(Refused {
                code: code.to_string(),
                message: crate::state::page_size_outside_range_message(
                    input_shape.as_deref(),
                    violation.value.as_deref().unwrap_or_default(),
                    maximum,
                    crate::state::PageSizeBound::Largest,
                ),
            })
        }
        _ => None,
    }
}
