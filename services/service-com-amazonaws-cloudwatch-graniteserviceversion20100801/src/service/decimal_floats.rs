// SPDX-License-Identifier: CC0-1.0

//! The floating-point members of this service's responses, rendered on the XML wire the way
//! AWS renders one there: as a decimal carrying a fractional part.
//!
//! # The gap this closes
//!
//! A Smithy `double` leaves an AWS `awsQuery` response as a decimal — a whole value reads
//! `5.0`, `95.0`, never `5`, `95` — while Rust's own rendering drops the fractional part of a
//! whole value and the framework's XML writer hands the value straight to `to_string()`. So
//! every whole-valued floating-point member this service answered a query call with left it in
//! a form no AWS service answers with: a client binding such an element to a decimal or double
//! type saw an integer where the model declares a double, and a client comparing the number it
//! published against the number it read back saw two different texts for one value.
//!
//! The rule is the PROTOCOL's, not any one shape's, and the writer it belongs in cannot be
//! edited here (`vendor/` is read-only), so it is applied at the one seam a service owns: the
//! typed [`Operation`] boundary, where the answer is still a structure rather than bytes. The
//! rewrite itself is the shared one every XML-wire service of this workspace renders a float
//! through (`service_common::query_decimal_floats`), so no shape of this service hand-authors a
//! serializer and the rule reaches every double at every depth of a response — a datapoint's
//! statistics inside a list, an alarm's threshold inside a reported alarm, a map's values.
//!
//! # Why it is decided per REQUEST
//!
//! This service's model declares three wire protocols side by side — `awsJson1_0`, `awsQuery`
//! and `rpcv2Cbor` — and each request is answered in the one it arrived in
//! ([`super::request_protocol`]). The decimal form is the XML wire's: on the two binary wires a
//! `double` is a NUMBER, not text, so writing it as decimal text there would answer a JSON
//! client a string where its SDK expects a number. So the rendering follows the protocol of the
//! request being answered, read off its content type exactly as the protocol selection reads it
//! ([`super::request_protocol::is_a_query_call`]).
//!
//! # Which operations are served through it
//!
//! Every operation whose MODELLED OUTPUT can reach a floating-point member — the rule reaches a
//! value only where the model puts one, so for any other operation the wrapper is a no-op. This
//! model's output-side doubles are reached by five reads
//! (`smithy_query(query="closure",operation="GetMetricStatistics,GetMetricData,DescribeAlarms,DescribeAlarmsForMetric,GetInsightRuleReport",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`):
//!
//! * `GetMetricStatistics`, through each `Datapoint`'s `SampleCount`/`Average`/`Sum`/
//!   `Minimum`/`Maximum` (all `DatapointValue`) and the values of its `ExtendedStatistics` map;
//! * `GetMetricData`, through each `MetricDataResult$Values` entry (`DatapointValue`);
//! * `DescribeAlarms` and `DescribeAlarmsForMetric`, through a reported alarm's `Threshold`
//!   (`MetricAlarm$Threshold`, and `LogAlarm$Threshold` on the first of the two);
//! * `GetInsightRuleReport`, through its `AggregateValue`, each contributor's
//!   `ApproximateAggregateValue` and `ApproximateValue`, and each
//!   `InsightRuleMetricDatapoint`'s statistics.
//!
//! Where this service serves one of those reads behind another wrapper of [`super`], this one
//! sits INSIDE it: the other wrappers decide whether a request is served at all, and this one
//! only how the answer to a served request is written.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use aws_smithy_schema::Schema;
use service_common::query_decimal_floats::DecimalFloats;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, Service,
};

/// One modelled operation, served so that a floating-point member of its response is rendered
/// the way the protocol the request is served in renders one ([the module docs](self)).
pub struct RenderingFloatsForTheWire<O> {
    /// The operation that serves the call; its answer is untouched, only its rendering decided.
    operation: O,
}

impl<O> RenderingFloatsForTheWire<O> {
    /// Wrap the operation, already constructed from the shared engine.
    pub fn new(operation: O) -> Self {
        Self { operation }
    }
}

/// One operation's answer, carrying the rendering the request's own protocol asks of its
/// floating-point members.
///
/// Both variants hold the SAME output: the handler's answer is built once and this only decides
/// which serializer walks it, so no value, member or element order is this type's.
pub enum RenderedFloats<T> {
    /// The XML wire's form: every floating-point leaf written as its decimal text.
    AsDecimalText(DecimalFloats<T>),
    /// The form the request's own codec writes, which is a NUMBER on the two binary wires.
    AsTheCodecWrites(T),
}

impl<T: SerializableStruct> SerializableStruct for RenderedFloats<T> {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        match self {
            Self::AsDecimalText(output) => output.serialize_members(serializer),
            Self::AsTheCodecWrites(output) => output.serialize_members(serializer),
        }
    }
}

impl<O: Operation> Operation for RenderingFloatsForTheWire<O> {
    type Input = O::Input;
    type Output = RenderedFloats<O::Output>;
    type Error = O::Error;

    const NAME: &'static str = O::NAME;
    const INPUT_SCHEMA: &'static Schema = O::INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static Schema = O::OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = O::SUCCESS_STATUS;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // The handler's own answer, untouched: what the protocol of this request decides is how
        // a floating-point leaf of it is WRITTEN, never what the read computed.
        let answered = self.operation.handle(input, ctx).await?;
        Ok(if super::request_protocol::is_a_query_call(ctx.parts()) {
            RenderedFloats::AsDecimalText(DecimalFloats(answered))
        } else {
            RenderedFloats::AsTheCodecWrites(answered)
        })
    }
}

impl<O> ErasedOperation for RenderingFloatsForTheWire<O>
where
    O: Operation + ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // The framework's own per-request sequence — deserialize, validate, handle, serialize,
        // and every modelled error — driven with THIS wrapper as the operation, so the handler
        // it calls is the wrapped one and the only difference is the serializer the output is
        // written with. The envelope is the one the generated shim passes, read here before the
        // await because `&dyn ErasedServiceDefinition` is not `Sync`.
        let envelope = svc.definition().wire_protocol(req).envelope();
        Box::pin(async move {
            smithy_server_runtime::run_operation_http(self, svc, req, ctx, envelope).await
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not change
    /// which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}
