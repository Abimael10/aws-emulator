// SPDX-License-Identifier: CC0-1.0

//! WHICH INVOCATION an integration to a Lambda function makes: the `InvocationType` its
//! `requestParameters` map onto the integration request, read once for both integration types
//! that invoke a function.
//!
//! `Integration$requestParameters` is "a key-value map specifying request parameters that are
//! passed from the method request to the back end", keyed
//! `integration.request.header.{name}` for a header the back end receives. The back end here
//! is AWS Lambda's function-invoking action, whose `Invoke` binds its `InvocationType` member
//! to the header [`INVOCATION_TYPE_HEADER`]
//! (`smithy_query(query="operation", operation="Invoke")` of the Lambda model) — so a value
//! an integration maps onto that header of the integration request names the invocation type
//! API Gateway makes the call under, and an integration that maps nothing onto it makes the
//! call the type `Invoke` documents as its default ("RequestResponse (default) – Invoke the
//! function synchronously").
//!
//! Only the VALUE is read here; whether it names an invocation type at all is Lambda's own
//! answer to the invocation that is then issued, exactly as the function the `uri` spells is.
//! That keeps the two documented invocations of a function that return before any run has
//! happened — the asynchronous one ("Event – Invoke the function asynchronously … The API
//! response only includes a status code") and the validating one ("DryRun – Validate
//! parameter values and verify that the user or role has permission to invoke the function")
//! — a property of the integration's own records rather than a case written here: both are
//! answered by Lambda with no payload of a run, because at that point no run has produced
//! one.

use ::aws_sdk_lambda::types::InvocationType;

use crate::ops::types::GetIntegrationOutput;

use super::super::super::mapping_template::TemplateInputs;
use super::super::INTEGRATION_REQUEST_HEADER_PREFIX;

/// The header AWS Lambda's `Invoke` binds its `InvocationType` to, which is therefore the
/// integration request header whose mapped value chooses the invocation.
const INVOCATION_TYPE_HEADER: &str = "X-Amz-Invocation-Type";

/// The invocation type `integration` asks this call to be made under, which is
/// `RequestResponse` — the documented default of `Invoke` — for an integration that maps no
/// value onto [`INVOCATION_TYPE_HEADER`].
pub(in crate::service::invoke) fn asked_for(
    integration: &GetIntegrationOutput,
    inputs: &TemplateInputs,
) -> InvocationType {
    stated(integration, inputs)
        .map(|value| InvocationType::from(value.as_str()))
        .unwrap_or(InvocationType::RequestResponse)
}

/// Whether an invocation of this type is one whose ANSWER carries what the function's run
/// produced.
///
/// Only the synchronous invocation does: `Invoke` gives `RequestResponse` as "invoke the
/// function synchronously. Keep the connection open until the function returns a response or
/// times out", while the other types answer before a run has produced anything — so there is
/// no function answer for the call to be served from.
pub(in crate::service::invoke) fn awaits_the_run(invocation_type: &InvocationType) -> bool {
    *invocation_type == InvocationType::RequestResponse
}

/// The value `integration`'s `requestParameters` map onto [`INVOCATION_TYPE_HEADER`] of the
/// integration request for this call, or `None` where they map none.
///
/// The header name is compared without regard to case, as an HTTP field name is: a developer
/// writes the mapping key in the case they choose, and the member it names is the same one
/// whichever case that is. The value is read through the ONE reading of a
/// `requestParameters` source this plane has ([`super::super::aws_integration::mapped_value`]),
/// so a static value in single quotes and a method request parameter are resolved here exactly
/// as they are for every other parameter of an integration request — and a mapping whose
/// source this request carries nothing for maps nothing, which is the invocation type the
/// integration states none for.
fn stated(integration: &GetIntegrationOutput, inputs: &TemplateInputs) -> Option<String> {
    let source = integration
        .request_parameters
        .as_ref()?
        .iter()
        .find_map(|(key, source)| {
            key.strip_prefix(INTEGRATION_REQUEST_HEADER_PREFIX)
                .filter(|name| name.eq_ignore_ascii_case(INVOCATION_TYPE_HEADER))
                .map(|_| source)
        })?;
    super::super::aws_integration::mapped_value(source, inputs).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An integration whose `requestParameters` are `parameters`.
    fn integration(parameters: &[(&str, &str)]) -> GetIntegrationOutput {
        GetIntegrationOutput {
            request_parameters: Some(
                parameters
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect(),
            ),
            ..GetIntegrationOutput::default()
        }
    }

    /// The inputs of a call carrying the header `X-Invoke-As: Event`, which is what a mapping
    /// from a method request parameter reads.
    fn inputs() -> TemplateInputs {
        let mut inputs = TemplateInputs::default();
        inputs
            .header
            .insert("X-Invoke-As".to_string(), "Event".to_string());
        inputs
    }

    /// RULE: the invocation is made under the type the integration maps onto the invocation
    /// type header, in either mapping form, and under the documented default where it maps
    /// none.
    #[test]
    fn the_integrations_mapped_header_names_the_invocation_type() {
        /// (case, the integration's `requestParameters`, the invocation type, awaited)
        type Row<'a> = (&'a str, &'a [(&'a str, &'a str)], InvocationType, bool);
        let rows: &[Row<'_>] = &[
            (
                "no requestParameters at all: the documented default",
                &[],
                InvocationType::RequestResponse,
                true,
            ),
            (
                "a static value naming the asynchronous invocation",
                &[(
                    "integration.request.header.X-Amz-Invocation-Type",
                    "'Event'",
                )],
                InvocationType::Event,
                false,
            ),
            (
                "the same header written in another case",
                &[(
                    "integration.request.header.x-amz-invocation-type",
                    "'Event'",
                )],
                InvocationType::Event,
                false,
            ),
            (
                "a static value naming the synchronous invocation",
                &[(
                    "integration.request.header.X-Amz-Invocation-Type",
                    "'RequestResponse'",
                )],
                InvocationType::RequestResponse,
                true,
            ),
            (
                "a static value naming the validating invocation",
                &[(
                    "integration.request.header.X-Amz-Invocation-Type",
                    "'DryRun'",
                )],
                InvocationType::DryRun,
                false,
            ),
            (
                "a method request header the call carries",
                &[(
                    "integration.request.header.X-Amz-Invocation-Type",
                    "method.request.header.X-Invoke-As",
                )],
                InvocationType::Event,
                false,
            ),
            (
                "a method request header the call does not carry, which maps nothing",
                &[(
                    "integration.request.header.X-Amz-Invocation-Type",
                    "method.request.header.X-Absent",
                )],
                InvocationType::RequestResponse,
                true,
            ),
            (
                "a mapping onto another header of the integration request",
                &[("integration.request.header.X-Tenant", "'Event'")],
                InvocationType::RequestResponse,
                true,
            ),
            (
                "an empty static value, which names no invocation type",
                &[("integration.request.header.X-Amz-Invocation-Type", "''")],
                InvocationType::RequestResponse,
                true,
            ),
        ];
        for (case, parameters, expected, awaited) in rows {
            let actual = asked_for(&integration(parameters), &inputs());
            assert_eq!(
                actual.as_str(),
                expected.as_str(),
                "case {case:?}: expected the {expected:?} invocation, got {actual:?}"
            );
            assert_eq!(
                awaits_the_run(&actual),
                *awaited,
                "case {case:?}: expected awaiting the run to be {awaited}, got {}",
                awaits_the_run(&actual)
            );
        }
    }
}
