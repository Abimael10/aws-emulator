// SPDX-License-Identifier: CC0-1.0

//! The floating-point members of this service's responses, rendered the way AWS renders
//! one on the XML wire: as a decimal carrying a fractional part.
//!
//! # The gap this closes
//!
//! A Smithy `double` leaves an AWS XML response as a decimal — a whole value reads `0.0`,
//! `50.0`, never `0`, `50` — while Rust's own rendering drops the fractional part of a
//! whole value, and the framework's XML writer hands the value straight to `to_string()`.
//! So every whole-valued floating-point member this service answers with left it in a
//! form no AWS service answers with, and a strict parser reading such a member — a target
//! value, the lower bound of a step, a forecast value, an instance-requirement limit —
//! saw an integer where the API documents a decimal.
//!
//! The rule is the PROTOCOL's, not this service's, and the writer it belongs in cannot be
//! edited here (`vendor/` is read-only), so it is applied at the one seam a service owns:
//! each operation is served behind
//! [`service_common::query_decimal_floats::DecimalFloatsOperation`], which runs the
//! framework's own request walk and changes nothing but how a floating-point leaf of the
//! response is written.
//!
//! # Which operations are served through it
//!
//! Every operation whose MODELLED OUTPUT can reach a floating-point member — named where
//! the service's operations are served ([`crate::service::quoted_values`]), which wraps each
//! of them in [`rendering_decimals`]. This model declares two floating-point shapes —
//! `smithy_query(query="shape",shape="MetricScale,NullablePositiveDouble")`, both
//! `double` — and three operations' outputs reach them:
//!
//! * `DescribePolicies`, through a reported policy's `StepAdjustments` bounds, its
//!   `TargetTrackingConfiguration$TargetValue` and its
//!   `PredictiveScalingConfiguration` metric specifications' `TargetValue`;
//! * `DescribeAutoScalingGroups`, through a group's mixed-instances policy, whose
//!   launch-template overrides carry `InstanceRequirements` — `MemoryGiBPerVCpu`,
//!   `TotalLocalStorageGB` and `NetworkBandwidthGbps` state their `Min` and `Max` as
//!   `NullablePositiveDouble`;
//! * `GetPredictiveScalingForecast`, through each forecast's
//!   `PredictiveScalingForecastValues`.
//!
//! An operation whose output declares no floating-point member is not listed, because for
//! it the wrapper is a no-op: the rule reaches a value only where the model puts one.

use service_common::query_decimal_floats::DecimalFloatsOperation;
use smithy_server_runtime::PreDispatchProtocol;

/// The envelope a pre-dispatch failure of a wrapped operation renders in: this service
/// speaks `awsQuery` (not the EC2 dialect), which is the same value the generated
/// per-operation shim passes.
const QUERY_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Query { ec2: false };

/// One already-constructed operation, served with every floating-point member of its
/// response rendered as decimal text.
///
/// The wrapper is still an operation of the same name, input schema and modelled errors
/// ([`DecimalFloatsOperation`]), so it composes inside the wrapper that quotes a refused
/// value ([`crate::service::quoted_values`]) without either rule knowing about the other.
pub fn rendering_decimals<O>(operation: O) -> DecimalFloatsOperation<O> {
    DecimalFloatsOperation::new(operation, QUERY_ENVELOPE)
}

#[cfg(test)]
mod tests {
    use super::rendering_decimals;
    use smithy_server_runtime::Operation;
    use std::sync::Arc;

    /// The modelled name of whatever operation `operation` is, read off its own
    /// [`Operation`] impl.
    fn modelled_name<O: Operation>(_operation: &O) -> &'static str {
        O::NAME
    }

    /// Wrapping an operation in the decimal renderer leaves its modelled identity alone: the
    /// name the protocol resolves, which is what files it in the service's table, is still
    /// the operation's own.
    #[test]
    fn the_renderer_keeps_the_operations_modelled_name() {
        let wrapped =
            rendering_decimals(crate::ops::describe_policies::DescribePolicies::new(Arc::new(())));
        let name = modelled_name(&wrapped);
        assert_eq!(
            name, "DescribePolicies",
            "expected the wrapper to keep the operation's modelled name, got {name:?}"
        );
    }
}
