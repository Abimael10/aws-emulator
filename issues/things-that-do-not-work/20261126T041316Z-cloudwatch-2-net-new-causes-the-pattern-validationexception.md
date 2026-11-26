Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.put-anomaly-detector-invalid-stat-rejected cloudwatch.rpcv2cbor-protocol-discriminator-and-framing-refusals
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.put-anomaly-detector-invalid-stat-rejected|run#7|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: cloudwatch.rpcv2cbor-protocol-discriminator-and-framing-refusals|_coral_code#47|AssertionError|assert isinstance(etype, str) and etype.startswith(_CORAL), (
Issue-Commit: 5b5c0bdcf53ebb03a5a8067f7321f26cc20710e1

# cloudwatch: 2 net-new causes — the @pattern ValidationException message wraps the regex in an extra capture group, and an rpc-v2-cbor type-mismatched member is answered ValidationException instead of SerializationException

> _Note: this issue was identified against the code base at commit `5b5c0bdcf53ebb03a5a8067f7321f26cc20710e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutAnomalyDetector

## Description
Two net-new root causes, both grounded in failing behavioral checks under the suite heading `cloudwatch`.

Cause 1 of 2: the ValidationException message for a @pattern violation quotes the modeled regular expression with an extra enclosing capture group that AWS does not emit.
AWS: Smithy: PutAnomalyDetector.Stat targets AnomalyDetectorMetricStat with @length min 0 max 50 and @pattern `^(SampleCount|Average|Sum|Minimum|Maximum|IQM|(p|tc|tm|ts|wm)(\d{1,2}(\.\d{0,10})?|100)|[ou]\d+(\.\d*)?)(_E|_L|_H)?|(TM|TC|TS|WM)\(...\)|(TM|TC|TS|WM|PR)\(...\)$`. A Stat value that does not match is refused with HTTP 400, error code `ValidationError` (the top-level request-validation envelope, not the modeled InvalidParameterValueException), and the message template `1 validation error detected: Value '<rejected value>' at '<member name>' failed to satisfy constraint: Member must satisfy regular expression pattern: <modeled pattern with its leading ^ and trailing $ removed, and NOTHING else added>`. The fixed text is everything outside the angle brackets; the variable parts are the rejected value (from the request), the member name (`stat`) and the pattern (from the model).
Emulator: it answers the right status and the right code, but renders the pattern as `((SampleCount|...)\))` — i.e. it strips the `^`/`$` anchors and then wraps the whole remaining expression in an additional `( ... )` group, so the quoted regex has one extra open paren at the start and one extra close paren at the end relative to the model and to AWS. No part of the rest of the message diverges.
Why: the pattern in this message is the only machine-readable statement of the constraint a client receives; a client (or a negative test, or a human) that compiles or compares the quoted regex gets an expression that is not the modeled one, and asserting on AWS's exact refusal text fails. The related resolved issue 20261025T135447Z-bcm-dashboards-validationexception-pattern-message-renders-the-regex-with-its.md fixed the anchor-stripping rule for a DIFFERENT service and does not cover this crate's extra wrapping group.
Fix: the request-validation error builder of the cloudwatch service that formats @pattern violations — it must quote the modeled pattern with only the leading `^` and trailing `$` removed and add no grouping of its own (the cloudwatch crate's validation/message-rendering layer under services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src). The rest of the check already passes: the refused call creates no detector and the identical call with Stat=Average succeeds.
Traces: cloudwatch.put-anomaly-detector-invalid-stat-rejected.

Cause 2 of 2: over the smithy-rpc-v2-cbor protocol, a structurally valid CBOR frame whose member carries the wrong CBOR major type is answered as a modeled validation failure instead of a protocol serialization failure.
AWS: Smithy: GraniteServiceVersion20100801 binds rpcv2Cbor alongside awsJson1_0/awsQuery; PutMetricData.Namespace targets a string. A CBOR body that encodes an unsigned integer where a string member is modeled cannot be deserialized, so AWS refuses it before validation with HTTP 400 and a CBOR error map whose `__type` is in the coral SERVICE namespace — `com.amazon.coral.service#SerializationException` — exactly as it does for a JSON body on the CBOR route and for an empty body. Recorded behavior only for the error map: SerializationException is a coral framework error and is not a modeled error of the operation. The namespace is the requirement: framing/deserialization failures are `com.amazon.coral.service#...`, while input-validation failures of a decodable frame are a different class (the same check asserts `MissingRequiredParameterException` with AWS's message text for an omitted required member, and that leg is reached only after this one).
Emulator: the same request is answered HTTP 400 with `__type` = `com.amazon.coral.validate#ValidationException` — the validation namespace and shape rather than `com.amazon.coral.service#SerializationException` — so the check cannot even read a coral service-namespaced code out of the body.
Why: a client discriminating on `__type` sees a modeled, service-level validation error where AWS reports a protocol serialization error; SDK error mapping, retry classification and any negative test that distinguishes "the frame could not be decoded" from "the decoded input was invalid" all diverge.
Fix: the rpcv2Cbor request-deserialization path of the cloudwatch service must map a CBOR major-type mismatch to the serialization-error response rather than routing it into the input-validation error builder (the cloudwatch crate's rpcv2Cbor protocol/deserializer layer).
Note on overlap: the open issue 20261125T220211Z-cloudwatch-rpc-v2-cbor-a-type-mismatched-member-answers.md describes this same mapping for PutMetricAlarm.Threshold and carries only the check `cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps`; the open issue 20261117T121318Z-cloudwatch-9-net-new-causes-putmetricdata-accepts-an.md is named there as carrying the discriminator/framing legs. Neither carries this check's replay key, and the suite did not report this check as owned, so it is filed here — if a fix for either of those also makes this check pass, close them together.
Traces: cloudwatch.rpcv2cbor-protocol-discriminator-and-framing-refusals.

## Evidence
Cause 1 — AssertionError: expected message "1 validation error detected: Value 'NotARealStatistic' at 'stat' failed to satisfy constraint: Member must satisfy regular expression pattern: (SampleCount|Average|Sum|Minimum|Maximum|IQM|(p|tc|tm|ts|wm)(\\d{1,2}(\\.\\d{0,10})?|100)|[ou]\\d+(\\.\\d*)?)(_E|_L|_H)?|(TM|TC|TS|WM)\\(...\\)|(TM|TC|TS|WM|PR)\\(...\\)", got the same sentence but with the pattern rendered as "((SampleCount|Average|...)\\))" — one extra `(` immediately after "pattern: " and one extra `)` at the very end. Status 400 and code ValidationError match; 'NotARealStatistic' and 'stat' are the request-supplied value and member name.

Cause 2 — AssertionError: CBOR integer where a string member is modeled: error __type was 'com.amazon.coral.validate#ValidationException', expected a 'com.amazon.coral.service#'-namespaced name. Request: POST /service/GraniteServiceVersion20100801/operation/PutMetricData, smithy-protocol: rpc-v2-cbor, content-type application/cbor, body = CBOR map {"Namespace": 5, "MetricData": []}. Expected (per the recording): 400 with __type com.amazon.coral.service#SerializationException. Actual: 400 with __type com.amazon.coral.validate#ValidationException. The status (400) and the refusal itself are correct; the earlier legs of the same check (CBOR PutMetricAlarm/PutAlarmMuteRule/GetAlarmMuteRule, the JSON-body and empty-body SerializationExceptions) are what the check reaches before this assertion.

## Steps to Reproduce
Cause 1:
1. Call PutAnomalyDetector(Namespace=<new namespace>, MetricName=<new metric>, Stat="NotARealStatistic").
2. Observe HTTP 400 / ValidationError (correct) but the message quotes the Stat @pattern wrapped in an extra outer `( ... )` group; AWS quotes the modeled pattern with only `^` and `$` removed.

Cause 2:
1. POST a sigv4-signed (signing name `monitoring`) request to /service/GraniteServiceVersion20100801/operation/PutMetricData with headers smithy-protocol: rpc-v2-cbor, content-type: application/cbor, accept: application/cbor.
2. Set the body to the definite-length CBOR map {"Namespace": 5, "MetricData": []} — an unsigned integer where the string member Namespace is modeled.
3. Decode the CBOR response: it is 400 with __type com.amazon.coral.validate#ValidationException instead of 400 with __type com.amazon.coral.service#SerializationException (the same class the adjacent JSON-body and empty-body cases already return).
