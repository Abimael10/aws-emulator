Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.put-metric-data-empty-metric-data-rejected cloudwatch.describe-alarms-for-metric-rejects-namespace-with-a-leading-colon cloudwatch.otel-enrichment-update-rejects-malformed-namespace-and-stopped-state cloudwatch.describe-alarms-rejects-an-unmodelled-alarm-type cloudwatch.describe-alarms-children-and-parents-of-alarm-name-reject-other-parameters cloudwatch.put-metric-data-dimension-name-colon-prefix-rejected cloudwatch.rpcv2cbor-protocol-discriminator-and-framing-refusals cloudwatch.tag-resource-malformed-arn-rejected cloudwatch.update-otel-enrichment-rejected-before-start cloudwatch.update-otel-enrichment-requires-running-rejected cloudwatch.deploy-cloudwatch-1-alarm-rename-replaces-and-dangling-composite-rejected
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.put-metric-data-empty-metric-data-rejected|_error_code#6|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: cloudwatch|DescribeAlarmsForMetric|ValidationError
Issue-Key: cloudwatch.otel-enrichment-update-rejects-malformed-namespace-and-stopped-state|run#16|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: cloudwatch|DescribeAlarms|InvalidParameterValue
Issue-Key: cloudwatch|DescribeAlarms|ValidationError
Issue-Key: cloudwatch|PutMetricData|InvalidParameterValue
Issue-Key: cloudwatch.rpcv2cbor-protocol-discriminator-and-framing-refusals|_coral_code#25|AssertionError|assert isinstance(etype, str) and etype.startswith(_CORAL), (
Issue-Key: cloudwatch.tag-resource-malformed-arn-rejected|run#6|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: cloudwatch|UpdateOTelEnrichment|ResourceNotFoundException
Issue-Key: cloudwatch.deploy-cloudwatch-1-alarm-rename-replaces-and-dangling-composite-rejected|run#78|AssertionError|assert code is not None and code.group(1) == "ValidationError", (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudwatch: 9 net-new causes — PutMetricData accepts an empty MetricData list, the rpc-v2-cbor unknown-operation error is not coral-namespaced, and seven refusal messages/codes are emulator-authored (namespace pattern, AlarmTypes enum, Children/ParentsOfAlarmName, dimension value, TagResource ARN, OTel enrichment, CFN composite reason)

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricData

## Description
This issue bundles 9 net-new root causes, all in the cloudwatch (monitoring, 2010-08-01) emulator. Each is numbered with its own failing check ids; all nine must be closed before submitting. None of these checks' replay keys are carried by the open issues 20261117T082319Z-cloudwatch-7-net-new-causes-putmetricalarm-accepts-threshold.md or 20261109T183935Z-cloudwatch-6-net-new-causes-in-the-control.md, which cover different operations and sentences.

Cause 1 of 9: PutMetricData accepts MetricData=[] instead of refusing the empty batch.
AWS: Smithy models PutMetricData with MetricData (list) and declares MissingRequiredParameterException (errorKind client, httpError 400, awsQueryError code MissingParameter / 400). An empty list is refused with HTTP 400, Error.Code 'MissingParameter' and the fixed message 'At least one MetricDatum object must be present in the request.' (the whole sentence is fixed text, no variable part); the identical call carrying one datum succeeds, so the refusal is attributable to the empty list alone.
Emulator: the call SUCCEEDS — HTTP 200, content-type application/x-amz-json-1.0, empty JSON body.
Why: a client whose batching loop produced no data believes it published, and the no-op is invisible; AWS's error is the only signal that a publish round did nothing.
Fix: PutMetricData's input validation (the MetricData emptiness check), before the ingest path.
Failing check: cloudwatch.put-metric-data-empty-metric-data-rejected.

Cause 2 of 9: the namespace-pattern refusal message quotes the Smithy pattern string instead of AWS's published pattern text.
AWS: PutMetricData$Namespace, DescribeAlarmsForMetric$Namespace and the OTel-enrichment selector namespace all carry pattern '^[^:]' in the model, but AWS's constraint-violation message renders the pattern as '[^:].*': '1 validation error detected: Value '<value>' at '<member path>' failed to satisfy constraint: Member must satisfy regular expression pattern: [^:].*'. Fixed text is '1 validation error detected: Value ', " at '", "' failed to satisfy constraint: Member must satisfy regular expression pattern: [^:].*"; the variable parts are the rejected value and the member path from the request ('namespace', 'includeFilters.1.member.namespace', ...). Error.Code is ValidationError (ValidationException -> awsQueryError ValidationError / 400).
Emulator: status, code and member path are right, but the message ends 'pattern: ^[^:]' — the model's pattern literal echoed verbatim instead of AWS's rendering. Reproduced on two unrelated operations (DescribeAlarmsForMetric and UpdateOTelEnrichment), so the defect is in the shared constraint-message builder.
Why: the message is a requirement of the error class; clients that match CloudWatch's wording (or display the pattern to a user) see a regex AWS never emits. Any future member with a pattern constraint will diverge the same way.
Fix: the shared constrained-member validation message builder (the pattern-rendering step), not the two call sites.
Failing checks: cloudwatch.describe-alarms-for-metric-rejects-namespace-with-a-leading-colon, cloudwatch.otel-enrichment-update-rejects-malformed-namespace-and-stopped-state.

Cause 3 of 9: DescribeAlarms refuses an unmodelled AlarmTypes value with the wrong error code and an enumeration of the wrong value set.
AWS: DescribeAlarms$AlarmTypes targets AlarmTypes (list of the AlarmType enum). An off-enum member is refused as a client-side constraint violation: HTTP 400, Error.Code 'ValidationError' and the message '1 validation error detected: Value '[<value>]' at 'alarmTypes' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy enum value set: [MetricAlarm, CompositeAlarm]]' — fixed text with the submitted list as the variable part; AWS enumerates exactly MetricAlarm and CompositeAlarm (the on-disk model also carries LogAlarm, which AWS's refusal does not list). DescribeAlarms declares only InvalidNextToken, so this refusal is recorded behavior only.
Emulator: Error.Code is 'InvalidParameterValue' and the message is 'The parameter AlarmTypes.member.1.<list element> must be a value in the set [CompositeAlarm, MetricAlarm, LogAlarm].' — a different error class, a different sentence, an AWS-Query member path and a third enum value.
Why: the code is what a client branches on; 'InvalidParameterValue' instead of 'ValidationError' breaks retry/validation handling, and advertising LogAlarm invites a request AWS rejects.
Fix: DescribeAlarms' AlarmTypes member validation — raise the constraint-violation (ValidationError) envelope with AWS's enum-value-set sentence.
Failing check: cloudwatch.describe-alarms-rejects-an-unmodelled-alarm-type.

Cause 4 of 9: the ChildrenOfAlarmName / ParentsOfAlarmName exclusivity refusal is an emulator-authored sentence.
AWS: DescribeAlarms models ChildrenOfAlarmName and ParentsOfAlarmName alongside AlarmNames, AlarmNamePrefix, AlarmTypes, StateValue, ActionPrefix, MaxRecords, NextToken. Combining either with any parameter other than MaxRecords/NextToken is refused HTTP 400 Error.Code ValidationError with '<Parameter> requests cannot be used with the following parameters: ActionPrefix, AlarmNamePrefix, AlarmNames, AlarmTypes, StateValue' — fixed text listing the forbidden parameters alphabetically, with the offending parameter name as the variable part (recorded behavior only; DescribeAlarms declares only InvalidNextToken).
Emulator: code is right, message is 'ChildrenOfAlarmName cannot be combined with any parameter other than MaxRecords and NextToken' — the complementary phrasing, and it never names the forbidden parameters.
Why: the message is a requirement of the error class; it is also the only thing telling the caller which parameter to drop.
Fix: DescribeAlarms' parameter-exclusivity validation message construction.
Failing check: cloudwatch.describe-alarms-children-and-parents-of-alarm-name-reject-other-parameters.

Cause 5 of 9: the colon-prefixed-dimension-name refusal reports the whole submitted value instead of the offending prefix.
AWS: Smithy documents Dimension$Name as 'Dimension names must contain only ASCII characters, must include at least one non-whitespace character, and cannot start with a colon (:)' and PutMetricData declares InvalidParameterValueException (client, 400, awsQueryError InvalidParameterValue / 400). A dimension name starting with ':' is refused HTTP 400, Error.Code InvalidParameterValue, message 'The value <reported-value> for parameter MetricData.member.<n>.Dimensions.member.<m>.Name is invalid.' — fixed text 'The value ', ' for parameter ', ' is invalid.'; the variable parts are the reported value and the member path, and for this violation AWS reports only the offending ':' prefix, not the full name (recorded behavior only).
Emulator: status, code and member path are right, but the value reported is the whole submitted dimension name (':Env<suffix>') rather than ':'.
Why: the message is a requirement of the error class, and the reported value tells the caller which character AWS objected to; echoing the whole name hides that and breaks clients matching the text.
Fix: PutMetricData's dimension-name validation message construction (which portion of the value is interpolated).
Failing check: cloudwatch.put-metric-data-dimension-name-colon-prefix-rejected.

Cause 6 of 9: the rpc-v2-cbor route's unknown-operation error __type is neither coral-namespaced nor the modelled name.
AWS: on the Smithy rpc-v2-cbor route, a request naming an operation the service does not have is answered HTTP 404 with the discriminator echoed (smithy-protocol: rpc-v2-cbor) and a CBOR error map whose '__type' is the coral-namespaced 'com.amazon.coral.service#UnknownOperationException'; the same applies to a wrong Smithy service name in the path. Framing failures use 'com.amazon.coral.service#SerializationException' and auth failures 'com.amazon.coral.service#InvalidSignatureException' (recorded behavior only — the CBOR error envelope is not expressed in the API model).
Emulator: the error map's '__type' is the bare 'UnknownOperationError' — not namespaced and not the Exception name — so a client cannot classify the refusal at all.
Why: the '__type' value is how an SDK maps a CBOR error to an exception class; a bare, misnamed type is unmatchable and surfaces as an unknown/unmodelled error. Because the check stops here, the sibling SerializationException / MissingRequiredParameterException / InvalidSignatureException types on the same route are presumed namespaced the same wrong way.
Fix: the rpc-v2-cbor error serializer's __type construction (the coral namespace and the Exception suffix) for the protocol-level errors.
Failing check: cloudwatch.rpcv2cbor-protocol-discriminator-and-framing-refusals.

Cause 7 of 9: TagResource's malformed-ARN refusal is an emulator-authored sentence.
AWS: TagResource models ResourceARN as required (AmazonResourceName, @length 1..1024) and declares InvalidParameterValueException (client, httpError 400, awsQueryError InvalidParameterValue / 400). A value that is not an ARN is refused with the message 'ResourceARN must be valid and belong to a CloudWatch resource: <value>' — fixed text 'ResourceARN must be valid and belong to a CloudWatch resource: ' with the submitted value as the only variable part.
Emulator: 'The value <value> for parameter ResourceARN is not valid: it is not the ARN of a CloudWatch alarm, dashboard, metric stream or Contributor Insights rule' — a different sentence that enumerates resource types AWS's message does not.
Why: the message is a requirement of the error class; clients matching CloudWatch's wording break.
Fix: TagResource's ResourceARN validation message construction.
Failing check: cloudwatch.tag-resource-malformed-arn-rejected.

Cause 8 of 9: UpdateOTelEnrichment's not-started refusal carries the wrong message.
AWS: UpdateOTelEnrichment declares ResourceNotFoundException (client, httpError 404, awsQueryError ResourceNotFoundException / 404) and ValidationException. Called before StartOTelEnrichment — or after the singleton has been stopped — it is refused with Error.Code 'ResourceNotFoundException' and the fixed message 'OTel enrichment is not enabled for this account' (the whole sentence is fixed text; no variable part).
Emulator: the code is right but the message is 'OTel enrichment is not running for this account. To start enrichment, use StartOTelEnrichment.' — two emulator-authored sentences.
Why: the message is a requirement of the error class; a client matching the wording, or asserting on the not-enabled vs. not-running distinction, diverges from AWS.
Fix: the OTel-enrichment state-machine guard shared by UpdateOTelEnrichment's pre-start and post-stop branches (its ResourceNotFoundException message).
Failing checks: cloudwatch.update-otel-enrichment-rejected-before-start, cloudwatch.update-otel-enrichment-requires-running-rejected.

Cause 9 of 9: the CloudFormation failure reason for a composite alarm whose AlarmRule names a missing alarm does not carry CloudWatch's error code.
AWS: when AWS::CloudWatch::CompositeAlarm creation is refused because its AlarmRule references an alarm that does not exist, the stack ends ROLLBACK_COMPLETE and the Composite resource's CREATE_FAILED ResourceStatusReason carries the service error envelope, including 'Error Code: ValidationError' and the ARN of the missing alarm (fixed field labels; the message detail, request id and ARN are variable parts taken from the request/state).
Emulator: the reason is the registry-handler wrapper only — 'Resource handler returned message: "Could not save the composite alarm as alarms [arn:aws:cloudwatch:<region>:<account>:alarm:<missing>] in the alarm rule do not exist" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)' — with no 'Error Code:' field, so CloudWatch's ValidationError is unrecoverable from the event.
Why: tooling parses the reason for the service error code to decide whether a rollback was caused by a client-side validation error; the handler's own InvalidRequest code is not that. The same reason-format defect reached through a dashboard is filed under cloudformation__cloudwatch.
Fix: the CloudFormation resource-failure reason builder for CloudWatch-backed resources — propagate the service error's code (and request id) into the reason.
Failing check: cloudwatch.deploy-cloudwatch-1-alarm-rename-replaces-and-dangling-composite-rejected.

## Evidence
Cause 1 — Expected: ('MissingParameter', 'At least one MetricDatum object must be present in the request.'). Actual: 'AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {"ResponseMetadata": {"HTTPStatusCode": 200, "HTTPHeaders": {"content-type": "application/x-amz-json-1.0", "content-length": "2", ...}}}'. The one-datum positive control returned 200 as expected.

Cause 2 — Expected: "1 validation error detected: Value ':<ns>' at 'namespace' failed to satisfy constraint: Member must satisfy regular expression pattern: [^:].*". Actual: the same sentence ending 'pattern: ^[^:]'. Identical divergence at 'includeFilters.1.member.namespace' on UpdateOTelEnrichment.

Cause 3 — Expected: ('ValidationError', "1 validation error detected: Value '[NotARealType]' at 'alarmTypes' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy enum value set: [MetricAlarm, CompositeAlarm]]"). Actual: ('InvalidParameterValue', 'The parameter AlarmTypes.member.1.<list element> must be a value in the set [CompositeAlarm, MetricAlarm, LogAlarm].').

Cause 4 — Expected: ('ValidationError', 'ChildrenOfAlarmName requests cannot be used with the following parameters: ActionPrefix, AlarmNamePrefix, AlarmNames, AlarmTypes, StateValue'). Actual: ('ValidationError', 'ChildrenOfAlarmName cannot be combined with any parameter other than MaxRecords and NextToken').

Cause 5 — Expected: ('InvalidParameterValue', 'The value : for parameter MetricData.member.1.Dimensions.member.1.Name is invalid.'). Actual: ('InvalidParameterValue', 'The value :Env<suffix> for parameter MetricData.member.1.Dimensions.member.1.Name is invalid.') — the suffix is per-run; the divergence is that the whole value is echoed instead of ':'.

Cause 6 — Expected: on the CBOR route, POST /service/GraniteServiceVersion20100801/operation/NoSuchOperationAtAll with smithy-protocol: rpc-v2-cbor answers 404 with __type 'com.amazon.coral.service#UnknownOperationException'. Actual: 'AssertionError: unknown operation on the CBOR route: error __type was \'UnknownOperationError\', expected a \'com.amazon.coral.service#\'-namespaced name'. The off-route 404s (no header / smithy-protocol: rpc-v2-json answering the bare XML '<UnknownOperationException/>') passed.

Cause 7 — Expected message: 'ResourceARN must be valid and belong to a CloudWatch resource: <value>'. Actual: 'The value <value> for parameter ResourceARN is not valid: it is not the ARN of a CloudWatch alarm, dashboard, metric stream or Contributor Insights rule'.

Cause 8 — Expected: ('ResourceNotFoundException', 'OTel enrichment is not enabled for this account'). Actual: ('ResourceNotFoundException', 'OTel enrichment is not running for this account. To start enrichment, use StartOTelEnrichment.') — identical before start and after stop.

Cause 9 — Expected: the Composite CREATE_FAILED ResourceStatusReason matches 'Error Code: ValidationError' and names the missing alarm ARN. Actual: 'AssertionError: Composite failure should carry CloudWatch\'s ValidationError, reason was \'Resource handler returned message: "Could not save the composite alarm as alarms [arn:aws:cloudwatch:us-west-2:000000000000:alarm:sim2real-cwdep1-missing-<suffix>] in the alarm rule do not exist" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)\'' (account/region/suffix/token are per-run).

## Steps to Reproduce
Cause 1: PutMetricData(Namespace=N, MetricData=[{MetricName:'M', Value:1.0, Unit:'Count'}]) (control, 200), then PutMetricData(Namespace=N, MetricData=[]) and read the error — expect 400 MissingParameter.
Cause 2: DescribeAlarmsForMetric(Namespace=':'+<name>, MetricName='M') and read Error.Message; then StartOTelEnrichment and UpdateOTelEnrichment(IncludeFilters=[{Namespace:':'+<name>}]) and read Error.Message.
Cause 3: DescribeAlarms(AlarmTypes=['NotARealType']) and read (Error.Code, Error.Message).
Cause 4: DescribeAlarms(ChildrenOfAlarmName=<a composite alarm>, AlarmNames=[...]) (and the same with ParentsOfAlarmName) and read Error.Message.
Cause 5: PutMetricData(Namespace=N, MetricData=[{MetricName:'M', Value:1.0, Dimensions:[{Name:':Env'+<suffix>, Value:'v'}]}]) and read Error.Message.
Cause 6: SigV4-sign and POST /service/GraniteServiceVersion20100801/operation/NoSuchOperationAtAll with headers smithy-protocol: rpc-v2-cbor and content-type application/cbor and a CBOR-encoded empty map; decode the CBOR body and read '__type'. Repeat with the correct operation under the wrong service name in the path.
Cause 7: TagResource(ResourceARN='not-an-arn', Tags=[{Key:'k', Value:'v'}]) and read Error.Message.
Cause 8: with OTel enrichment never started, UpdateOTelEnrichment(IncludeFilters=[{Namespace:'X'}]) and read Error.Message; then StartOTelEnrichment, StopOTelEnrichment and repeat.
Cause 9: CreateStack with an AWS::CloudWatch::Alarm ('ValidAlarm') and an AWS::CloudWatch::CompositeAlarm whose AlarmRule references an alarm name that does not exist; wait for ROLLBACK_COMPLETE, DescribeStackEvents and read the Composite CREATE_FAILED ResourceStatusReason.
