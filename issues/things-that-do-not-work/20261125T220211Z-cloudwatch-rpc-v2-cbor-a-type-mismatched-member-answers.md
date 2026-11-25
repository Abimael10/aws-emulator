Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps|run#44|AssertionError|assert (status, decoded) == (
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# rpc-v2-cbor: a type-mismatched member answers ValidationException with an internal message instead of SerializationException

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
One net-new root cause, grounded in the failing behavioral check under the suite heading `cloudwatch`.

Cause 1 of 1: over the smithy-rpc-v2-cbor protocol, a CBOR body whose member is the wrong major type is answered as a modeled validation failure instead of a deserialization failure.
AWS: Smithy: GraniteServiceVersion20100801 binds awsJson1_0, awsQuery and rpcv2Cbor; PutMetricAlarm.Threshold targets `double`. A CBOR request that encodes a text string where a double is modeled cannot be deserialized at all, so it is refused before validation with HTTP 400 and the CBOR body map {"__type": "com.amazon.coral.service#SerializationException"} — the single fixed member `__type`, with NO `message` member — and no alarm is created. Recorded behavior only for that error map: SerializationException is a coral framework error and is not a modeled error of the operation (the model declares only LimitExceededFault).
Emulator: the same request is answered HTTP 400 with {"__type": "com.amazon.coral.validate#ValidationException", "message": "invalid input: unexpected type string at position 158: expected f64"} — the wrong error shape (`com.amazon.coral.validate#ValidationException` rather than `com.amazon.coral.service#SerializationException`), plus an extra `message` member whose text is an internal decoder diagnostic (a CBOR byte offset and the Rust type name `f64`); AWS emits no message at all for this class, and `f64`/"position N" are not AWS vocabulary.
Why: a client discriminating on `__type` sees a modeled, service-level validation error where AWS reports a protocol serialization error; SDK error mapping, retry classification and negative tests that assert the error class all diverge, and the leaked decoder text exposes implementation internals in a client-visible message.
Fix: the rpcv2Cbor request-deserialization path of the cloudwatch service must map a CBOR major-type mismatch to the serialization-error response (`__type` only, no message) rather than routing it into the input-validation error builder (the cloudwatch crate's rpcv2Cbor protocol/deserializer layer, services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src). The open issue 20261117T121318Z-cloudwatch-9-net-new-causes-putmetricdata-accepts-an.md carries the sibling check rpcv2cbor-protocol-discriminator-and-framing-refusals (discriminator/framing refusals) and does not carry this check or this body-decode case.
Traces: cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps.

## Evidence
AssertionError: CBOR PutMetricAlarm with a text Threshold answered 400 {'__type': 'com.amazon.coral.validate#ValidationException', 'message': 'invalid input: unexpected type string at position 158: expected f64'}

Expected (per the recording the check replays): (400, {'__type': 'com.amazon.coral.service#SerializationException'}).
Actual (emulator): (400, {'__type': 'com.amazon.coral.validate#ValidationException', 'message': 'invalid input: unexpected type string at position 158: expected f64'}).

The status code (400) and the refusal itself are right; the error shape and the presence/wording of `message` are wrong. Everything earlier in the same check — the CBOR mute-rule lifecycle, ListAlarmMuteRules empty map, and the ResourceNotFoundException map for a deleted mute rule — passed, so the divergence is isolated to the body-decode error mapping.

## Steps to Reproduce
1. POST a sigv4-signed rpc-v2-cbor request to /service/GraniteServiceVersion20100801/operation/PutMetricAlarm with Content-Type application/cbor and smithy-protocol: rpc-v2-cbor, encoding a valid alarm definition (AlarmName, Namespace, MetricName, Statistic=Average, Period=60, EvaluationPeriods=1, ComparisonOperator=GreaterThanThreshold) but with Threshold as the CBOR text string "high".
2. Observe the response: 400 with __type com.amazon.coral.validate#ValidationException and a decoder message, instead of 400 with __type com.amazon.coral.service#SerializationException and no message.
3. DescribeAlarms for the alarm name confirms no alarm was created (this part already matches).
