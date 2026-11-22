Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 48fdd4b276fb760420919974f00f6d6e0750e9b1

# A RawMetricData.Timestamp sent as a non-epoch string escapes the envelope's deserialization refusal and is answered in the framework's ValidationException envelope the service says it never uses for that class

_Filed: 2026-11-22T18:39:57Z_
_Operation: BatchPutMetrics | Class: error-response_

> _Note: this issue was identified against the code base at commit `48fdd4b276fb760420919974f00f6d6e0750e9b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate words the class "a value the codec never turned into a value at all" itself: `crate::service::envelope` refuses it with HTTP 400 `SerializationException` and the sentence under a CAPITAL-M `Message` member, explicitly so that "a client reading a deserialization refusal finds the sentence under no other name". The gatekeeper for that class is `envelope::reads_into`, which the module documents as "the question that separates a value the codec never turned into a value at all from one that was read and then resolved per item", and which is deliberately kept in step with this protocol's codec (it adds the quoted non-finite float form "THIS protocol's codec reads that the general rule does not").

For `RawMetricData.Timestamp` the gatekeeper is WIDER than the codec. `reads_into` delegates to `service_common::json_wire_values::reads_as`, which answers true for ANY JSON string bound to a timestamp member "because `@timestampFormat` decides which" — but this model's `Timestamp` shape carries NO `@timestampFormat`, so the codec's format is `Format::EpochSeconds`, and its `read_timestamp` turns a string into a value only when that string parses as an epoch-seconds number. A point sent as `{"MetricName":"loss","Timestamp":"2026-01-01T00:00:00Z","Value":1.0}` therefore passes `envelope::refuses` (no unreadable member, no unset required member, within the item bound), reaches `deserialize_input`, and fails there with a custom `SerdeError`.

That failure is NOT describable as a framework `Refusal` (`refusal_of` maps only `MissingMember` and `Invalid`), so `EmulatedService::override_refusal` cannot re-word it either: the caller gets 400 with `__type`/`x-amzn-errortype` = `ValidationException` and the codec's internal sentence under the LOWERCASE `message` member — the exact envelope this service states it never answers for a value that never became a value, and the exact three properties the crate's own integration test asserts for this class (capital-M `Message` present, lowercase `message` absent, error type not `ValidationException`). The defect is the over-broad readability test for the one timestamp member of this service, not the model and not the codec.

## Evidence

The rule this service states for the class — `services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/src/service/envelope.rs:18-21`:
```
//! * a value whose JSON type the member it is bound to cannot be read from never became a
//!   value, which is a DESERIALIZATION failure: HTTP 400 [`SERIALIZATION_EXCEPTION`], with
//!   the sentence in the body member spelled `Message`, naming the CLASS of JSON that
//!   arrived ([`UNEXPECTED_JSON_TYPE`]);
```
and `src/service/envelope.rs:225-227`:
```
/// Spelled with a capital `M`, which is NOT how the framework's own refusals and this
/// service's modelled answers spell it: a client reading a deserialization refusal finds
/// the sentence under no other name.
```

The gatekeeper, and its claim to be in step with the codec — `src/service/envelope.rs:397-417`:
```
/// Whether a JSON value can be READ into the member it is bound to — the question that
/// separates a value the codec never turned into a value at all from one that was read and
/// then resolved per item.
...
/// (`service_common::json_wire_values::reads_as`) plus the one form THIS protocol's codec
/// reads that the general rule does not: a float member reads from a quoted non-finite
/// number ([`NON_FINITE_TEXTS`]). ...
fn reads_into(member: &Schema, held: &Value) -> bool {
    if ::service_common::json_wire_values::reads_as(member.shape_type(), held) {
        return true;
    }
```
The general rule it trusts — `services/common/src/json_wire_values.rs:130-131` and `:158-165`:
```
/// ... A timestamp reads from either a number or a
/// string, because `@timestampFormat` decides which. ...
        Value::String(_) => matches!(
            target,
            ShapeType::String
                | ShapeType::Blob
                | ShapeType::Timestamp
```
This model declares no `@timestampFormat`: `smithy_query(query="shape", shape="RawMetricData,Timestamp")` reports `"Timestamp": {"target": "Timestamp", "clientOptional": true, "required": true, "type": "timestamp"}` and `"Timestamp": {"shape_id": "com.amazonaws.sagemakermetrics#Timestamp", "type": "timestamp"}` — no format trait on either. The generated member schema types it `ShapeType::Timestamp` (`vendor/smithy-macros/src/codegen.rs:475`: `ShapeKind::Timestamp => "Timestamp",`), so `reads_as` takes the `ShapeType::Timestamp` arm above.

What the codec actually does with that string — `vendor/smithy-schema-server/src/json.rs:44-50`:
```
fn timestamp_format(schema_format: Option<TimestampFormat>) -> Format {
    match schema_format {
...
        Some(TimestampFormat::EpochSeconds) | None => Format::EpochSeconds,
```
and `vendor/smithy-schema-server/src/json.rs:1016-1020`:
```
            Token::ValueString { value, .. } => {
                let s = unescape(value)?;
                DateTime::from_str(&s, format)
                    .map_err(|e| SerdeError::custom(format!("invalid timestamp: {e}")))
            }
```
How that failure is then worded — `vendor/smithy-server-runtime/src/pre_dispatch.rs:446-464` (the restJson1 arm):
```
        ProtocolError::Serde(e) => (400, "ValidationException", serde_message(&e)),
...
    let body = format!(r#"{{"__type":"{type_name}","message":"{message}"}}"#);
```
and it cannot be re-worded by this service's seam, because `vendor/smithy-server-runtime/src/pre_dispatch.rs:198-209` describes only `SerdeError::MissingMember` and `ProtocolError::Invalid` as a `Refusal` — the one input `EmulatedService::override_refusal` receives (`src/service.rs:150-168`).

Test coverage proving this input class is unpinned: `tests/all/envelope_and_per_item_reporting.rs:298-302` states the RULE and `:384-409` asserts it (`answer.member("Message") == Some("<CLASS>_VALUE is not an expected Json type")`, `answer.member("message") == None`, `assert_ne!(answer.error_type.as_deref(), Some("ValidationException"))`), but no case row at `:307-382` sends `Timestamp` as a string — the nearest nested case is `"Value": "not-a-number"` at `:353-367`, which IS caught because `reads_into`'s float arm rejects any text outside `NON_FINITE_TEXTS` (`src/service/envelope.rs:395`).

## Steps to Reproduce

1. PUT /BatchPutMetrics with body {"TrialComponentName":"tc-ts","MetricData":[{"MetricName":"loss","Timestamp":"2026-01-01T00:00:00Z","Step":0,"Value":1.0}]}.
2. Observe the envelope layer does NOT claim the request: `reads_into` (src/service/envelope.rs:407) returns true for the string bound to the `ShapeType::Timestamp` member via services/common/src/json_wire_values.rs:158-165, `Timestamp` is not named in `unset_member_refusal_for` (src/service/envelope.rs:156-166), and the item count is within the bound — so `envelope::refuses` is false and the generated operation runs.
3. The codec then fails the member: vendor/smithy-schema-server/src/json.rs:1016-1020 calls DateTime::from_str(s, Format::EpochSeconds) (the default at json.rs:44-50, since the model puts no @timestampFormat on com.amazonaws.sagemakermetrics#Timestamp) and raises SerdeError::custom("invalid timestamp: ...").
4. The answer is HTTP 400 with {"__type":"ValidationException","message":"<codec text>"} (vendor/smithy-server-runtime/src/pre_dispatch.rs:447,464) and x-amzn-errortype: ValidationException — no capital-M `Message` member at all.
5. Compare with src/service/envelope.rs:18-21 and :225-227, and with the three assertions tests/all/envelope_and_per_item_reporting.rs:391-409 makes for every value "the codec never turned into a value at all": the sentence must be under `Message`, nothing under `message`, and the error type must not be ValidationException. A quoted epoch-seconds string such as "1700000000" still parses and is stored, so only the non-epoch string forms are affected.
