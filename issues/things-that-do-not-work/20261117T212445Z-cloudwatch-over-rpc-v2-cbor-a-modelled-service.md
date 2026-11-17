Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps|run#38|AssertionError|assert (status, decoded) == (
Issue-Commit: 645fb92de80d09594abb534f9630624c4b349bb7

# cloudwatch: over rpc-v2-cbor a modelled service error (GetAlarmMuteRule ResourceNotFoundException) answers HTTP 404 with a bare, member-populated __type instead of AWS's HTTP 400 and the fully-qualified com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `645fb92de80d09594abb534f9630624c4b349bb7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAlarmMuteRule

## Description
This issue carries 1 net-new root cause in the cloudwatch (monitoring, 2010-08-01) emulator. It must be closed before submitting.

Cause 1 of 1: on the Smithy rpc-v2-cbor route, a MODELLED service error is serialized with the wrong HTTP status, an un-namespaced `__type` discriminator and error members AWS does not emit.

AWS: Smithy models GetAlarmMuteRule (service shape com.amazonaws.cloudwatch#GraniteServiceVersion20100801) with input AlarmMuteRuleName (target Name, required, @length 1..255) and declares exactly one error, ResourceNotFoundException (errorKind client, httpError 404, awsQueryError {code ResourceNotFoundException, httpResponseCode 404}; members ResourceType, ResourceId, Message; doc "The named resource does not exist."). The CBOR error envelope itself is not expressed in the API model, so for the rpc-v2-cbor route this is recorded behavior only: a GetAlarmMuteRule naming a mute rule that has just been deleted is answered HTTP 400, header `smithy-protocol: rpc-v2-cbor`, `content-type: application/cbor`, and a CBOR map carrying exactly ONE entry — `__type` = "com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException". There is no Message, no ResourceType and no ResourceId member in that map. The discriminator is the fully-qualified Coral-style shape id of the MODELLED exception (`com.amazonaws.cloudwatch.v2010_08_01#<ExceptionName>`); that is a different namespace from the PROTOCOL-level errors on the same route, which the recording shows as `com.amazon.coral.service#<ExceptionName>` (e.g. SerializationException for a text-valued Threshold on PutMetricAlarm, InvalidParameterValueException for DescribeInsightRules MaxResults=0). Fixed text here is the whole shape id; the only request/state-derived part of the response would be a Message, which AWS does not populate for this refusal.

Emulator: the same call answers HTTP 404 and the CBOR map `{'__type': 'ResourceNotFoundException', 'ResourceType': 'AlarmMuteRule', 'ResourceId': '<the requested mute rule name>'}`. Three divergences in one response: (a) status 404 instead of 400; (b) `__type` is the bare exception name with no namespace, neither `com.amazonaws.cloudwatch.v2010_08_01#...` nor the coral form; (c) two error members (ResourceType, ResourceId) are projected into the map that AWS leaves absent for a missing mute rule. The preceding CBOR calls in the same run (PutMetricAlarm, ListAlarmMuteRules, PutAlarmMuteRule, GetAlarmMuteRule on the live rule, DeleteAlarmMuteRule) all answered 200 with the exact expected CBOR maps, so the route and its success serialization are sound — the defect is confined to the error path.

Why: an SDK maps a CBOR error response to an exception class by matching `__type` against the modelled shape id. A bare, un-namespaced `ResourceNotFoundException` matches nothing, so a client that deleted a mute rule and re-reads it gets an unknown/unmodelled error instead of ResourceNotFoundException — `is not found` and retry-classification branches never fire, and idempotent delete-then-verify loops break. The 404 instead of 400 compounds this: proxies, gateways and generic HTTP layers between client and service treat 404 as a routing/endpoint problem rather than a service-level client error, and a caller asserting on the status sees a different error class. Serializing ResourceType/ResourceId that AWS omits also means a client reading those members gets values on the emulator that are absent against AWS.

Fix: the rpc-v2-cbor error serializer for modelled service errors — its `__type` shape-id construction (fully-qualify with the service's `com.amazonaws.cloudwatch.v2010_08_01` namespace, keeping the coral namespace only for protocol-level errors), its HTTP status selection for the CBOR route, and the error-member projection that decides which members of the exception shape are written into the CBOR map. This is the shared serializer, not GetAlarmMuteRule's handler: every modelled error reachable over the CBOR route is affected the same way. Because the check halts at this assertion, the remaining CBOR error expectations in the same scenario were never exercised and are presumed to diverge identically: PutMetricAlarm with a text Threshold (expect 400 / `com.amazon.coral.service#SerializationException`, and no alarm created) and DescribeInsightRules with MaxResults=0 (expect 400 / `com.amazon.coral.service#InvalidParameterValueException` with message `INVALID_MAX_RESULTS: MaxResults must be greater than 0.`). Related but NOT covering: cause 6 of the open issue 20261117T121318Z-cloudwatch-9-net-new-causes-putmetricdata-accepts-an.md covers the PROTOCOL-level unknown-operation `__type` on this same route (bare 'UnknownOperationError'); it does not carry this check's replay key and says nothing about modelled service errors, the HTTP status, or error-member projection.

Traces: cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps (check file check_cloudwatch_rpcv2_cbor_insight_rule_mute_rule_and_error_maps.py, line 249 — the GetAlarmMuteRule-after-DeleteAlarmMuteRule assertion).

## Evidence
Cause 1 — failing check cloudwatch.rpcv2-cbor-insight-rule-mute-rule-and-error-maps.

Expected (AWS, recorded): (status, decoded CBOR body) == (400, {"__type": "com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException"}) for POST /service/GraniteServiceVersion20100801/operation/GetAlarmMuteRule with headers `smithy-protocol: rpc-v2-cbor`, `content-type: application/cbor`, `accept: application/cbor` and the CBOR-encoded body {"AlarmMuteRuleName": <the just-deleted rule name>}.

Actual (emulator): status 404, decoded CBOR body {'__type': 'ResourceNotFoundException', 'ResourceType': 'AlarmMuteRule', 'ResourceId': '<rule name>'}.

Raw assertion from the replay:
  File "check_cloudwatch_rpcv2_cbor_insight_rule_mute_rule_and_error_maps.py", line 249, in run
    assert (status, decoded) == (
  AssertionError: CBOR GetAlarmMuteRule on the just-deleted rule answered 404 {'__type': 'ResourceNotFoundException', 'ResourceType': 'AlarmMuteRule', 'ResourceId': 'sim2real-behav-cwcbor-mute-9425bb29083e'}

The mute-rule name ('sim2real-behav-cwcbor-mute-<suffix>') is a per-run generated value and is not part of the requirement; the divergence is the 404 status, the un-namespaced '__type' and the presence of the ResourceType/ResourceId members at all.

Positive controls in the same run, all over the same CBOR route, passed: PutMetricAlarm -> 200; ListAlarmMuteRules -> (200, {"AlarmMuteRuleSummaries": []}); PutAlarmMuteRule -> 200; GetAlarmMuteRule on the live rule -> 200 with the full expected map (Name, AlarmMuteRuleArn, Rule.Schedule{Expression,Duration,Timezone:'UTC'}, MuteTargets.AlarmNames, Status 'SCHEDULED', MuteType 'one-time', a CBOR tag-1 float LastUpdatedTimestamp and a <uuid>/<epoch-ms> CreationId); DeleteAlarmMuteRule -> 200. So success-path CBOR serialization and the mute-rule state machine are correct; only the error envelope diverges.

Smithy consulted: GetAlarmMuteRule input {AlarmMuteRuleName: Name, required, @length 1..255}; errors [ResourceNotFoundException: errorKind client, httpError 404, awsQueryError {code ResourceNotFoundException, httpResponseCode 404}]; shape com.amazonaws.cloudwatch#ResourceNotFoundException members {ResourceType, ResourceId, Message} doc 'The named resource does not exist.'

## Steps to Reproduce
1. Point a client at the cloudwatch emulator endpoint and obtain SigV4 credentials for signing name 'monitoring'.
2. Define a CBOR helper that POSTs to <endpoint>/service/GraniteServiceVersion20100801/operation/<Op> with headers `smithy-protocol: rpc-v2-cbor`, `content-type: application/cbor`, `accept: application/cbor`, a SigV4-signed definite-length CBOR body, and that decodes the CBOR response.
3. CBOR PutMetricAlarm {AlarmName: A, Namespace: 'Sim2RealBehav/Cbor', MetricName: 'Requests', Statistic: 'Average', Period: 60, EvaluationPeriods: 1, Threshold: 5.0, ComparisonOperator: 'GreaterThanThreshold', ActionsEnabled: false} -> expect 200.
4. CBOR PutAlarmMuteRule {Name: M, Rule: {Schedule: {Expression: 'at(<a timestamp ~1 year out, %Y-%m-%dT%H:%M>)', Duration: 'PT15M'}}, MuteTargets: {AlarmNames: [A]}} -> expect 200.
5. CBOR DeleteAlarmMuteRule {AlarmMuteRuleName: M} -> expect 200.
6. CBOR GetAlarmMuteRule {AlarmMuteRuleName: M} and read the HTTP status and the decoded CBOR map. AWS answers 400 with exactly {"__type": "com.amazonaws.cloudwatch.v2010_08_01#ResourceNotFoundException"}; the emulator answers 404 with {'__type': 'ResourceNotFoundException', 'ResourceType': 'AlarmMuteRule', 'ResourceId': M}.
7. Presumed-same-cause follow-ups blocked by step 6: CBOR PutMetricAlarm with Threshold: 'high' (expect 400 / 'com.amazon.coral.service#SerializationException', and DescribeAlarms must show no such alarm created), and CBOR DescribeInsightRules {MaxResults: 0} (expect 400 / 'com.amazon.coral.service#InvalidParameterValueException' with message 'INVALID_MAX_RESULTS: MaxResults must be greater than 0.').
