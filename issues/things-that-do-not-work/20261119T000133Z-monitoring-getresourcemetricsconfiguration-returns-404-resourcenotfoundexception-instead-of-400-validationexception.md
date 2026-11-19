Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatch-resource-metrics-configuration-validation-errors
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatch-resource-metrics-configuration-validation-errors
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|GetResourceMetricsConfiguration|4xx->4xx|$.__type
Issue-Key: monitoring|GetResourceMetricsConfiguration|4xx->4xx|$.message
Issue-Key: monitoring|GetResourceMetricsConfiguration|4xx->4xx|$headers.x-amzn-query-error
Issue-Key: monitoring|GetResourceMetricsConfiguration|4xx->4xx|status
Issue-Commit: e822fa4ca12543e97642b0a71000bedf3ee319c6

# GetResourceMetricsConfiguration returns 404 ResourceNotFoundException instead of 400 ValidationException for an unsupported resource type

> _Note: this issue was identified against the code base at commit `e822fa4ca12543e97642b0a71000bedf3ee319c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourceMetricsConfiguration

## Description
Cause 1 of 1: GetResourceMetricsConfiguration misclassifies a syntactically well-formed ResourceArn whose resource TYPE is not one CloudWatch resource-metrics configuration supports. The emulator treats it as a lookup miss instead of a rejected input, so it answers with the wrong HTTP status class, the wrong error code, the wrong awsQuery error header, and no message at all.

AWS: Smithy: GetResourceMetricsConfiguration.ResourceArn is @required with @length min 20 / max 2048 and @pattern ^arn:[a-zA-Z0-9-]+:[a-zA-Z0-9-]+:[a-zA-Z0-9-]*:\\d{12}:.+$; the operation's only modeled error is ResourceNotFoundException (client, httpError 404, awsQueryError code ResourceNotFoundException). The model therefore says nothing about the unsupported-resource-type case, so for that case this is recorded behavior only: the recording shows AWS accepting the ARN as syntactically valid (it satisfies the pattern and the length bounds) and then rejecting its resource type as INPUT, answering HTTP 400 with __type 'com.amazon.coral.validate#ValidationException', the awsQuery error header 'x-amzn-query-error: ValidationError;Sender', and the message 'Unsupported resource type'. That message is entirely fixed text for this error class — it has no variable part interpolated from the request or from state, so it must be reproduced verbatim and must NOT be embellished with the submitted ARN. The general rule: an ARN that parses but names a resource type outside the set CloudWatch resource-metrics configuration covers is an input fault (400 / ValidationException / ValidationError;Sender), not an absent resource; a 404 ResourceNotFoundException is reserved for an ARN of a SUPPORTED type that has no configuration.

Emulator: answers HTTP 404 with __type 'com.amazon.coral.service#ResourceNotFoundException' and 'x-amzn-query-error: ResourceNotFoundException;Sender', and omits the $.message member entirely (expected 'Unsupported resource type', got None). So all four observable facets of the error — status, __type, query-error header and message — diverge on the same step, which is the signature of the handler skipping the resource-type admissibility check and falling straight through to its state lookup.

Why: a client cannot tell a bad request from a missing resource. The two carry opposite remedies: a 400/ValidationException means the caller must correct the ARN it sent, while a 404/ResourceNotFoundException means the ARN was fine and the configuration simply has not been created yet, which invites the caller to create or poll for one that can never exist. SDK error-shape dispatch keys off __type (and, on the awsQuery protocol, off x-amzn-query-error), so a catch on ValidationException never fires and a catch on ResourceNotFoundException fires wrongly; retry policies that treat 4xx-not-found as benign will loop. The absent $.message also leaves the caller with no explanation at all, since ValidationException's message is the only place AWS states which facet of the input was rejected.

Fix: the GetResourceMetricsConfiguration handler's input validation, in the monitoring (CloudWatch) service crate — the resource-type admissibility check must run on the parsed ResourceArn BEFORE the state lookup, and on failure raise the service-common ValidationException (400, __type com.amazon.coral.validate#ValidationException, x-amzn-query-error ValidationError;Sender, message 'Unsupported resource type') rather than letting the lookup miss produce ResourceNotFoundException. The ResourceNotFoundException path must remain for supported-type ARNs with no stored configuration.

Traces: cloudwatch-resource-metrics-configuration-validation-errors#task#4 (the only member of this cluster; affected_count 1).

Note: no open issue covers this operation — a search of the open issues for 'resource-metrics' returns nothing, and the other monitoring divergences in this replay are each already owned by an existing open issue (the CloudWatch ValidationException-message, alarm-write-validation, alarm-history, anomaly-detector, awsQuery-XML-serializer and ListDashboards-Size issues). This divergence is reported by the replay with an empty owned_by and an empty stub_path.

## Evidence
Replay of fixture cloudwatch-resource-metrics-configuration-validation-errors, step task#4 (operation GetResourceMetricsConfiguration, service monitoring, verdict mismatch, divergence_key service=monitoring|GetResourceMetricsConfiguration|status=400->404|keys=$.__type,$.message,$headers.x-amzn-query-error,status).

Expected (recorded AWS response):
  HTTP status: 400
  $.__type: 'com.amazon.coral.validate#ValidationException'
  $.message: 'Unsupported resource type'
  $headers.x-amzn-query-error: 'ValidationError;Sender'

Actual (emulator response):
  HTTP status: 404
  $.__type: 'com.amazon.coral.service#ResourceNotFoundException'
  $.message: absent (missing)
  $headers.x-amzn-query-error: 'ResourceNotFoundException;Sender'

Message anatomy: 'Unsupported resource type' is wholly fixed text for this error class; there are no variable parts drawn from the request or from emulator state, so no ARN, name, id, limit or member name is interpolated into it.

Model evidence (smithy_query, services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json): GetResourceMetricsConfiguration input member ResourceArn -> target ResourceArn, required, @length {min:20,max:2048}, @pattern ^arn:[a-zA-Z0-9-]+:[a-zA-Z0-9-]+:[a-zA-Z0-9-]*:\\d{12}:.+$; errors = [ResourceNotFoundException (client, httpError 404, awsQueryError ResourceNotFoundException/404)]. The submitted ARN satisfies the modeled pattern and length, so the modeled constraints alone do not explain the rejection — the unsupported-resource-type rule is recorded behavior only.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from it.
2. Replay wire fixture 'cloudwatch-resource-metrics-configuration-validation-errors' against it (replay_wire_corpus).
3. Observe step 'task#4': a GetResourceMetricsConfiguration call whose ResourceArn is a syntactically valid ARN (matching the modeled pattern and length bounds) but naming a resource type CloudWatch resource-metrics configuration does not support.
4. Expected per the recording: HTTP 400, __type 'com.amazon.coral.validate#ValidationException', header x-amzn-query-error 'ValidationError;Sender', message 'Unsupported resource type'.
5. Observed: HTTP 404, __type 'com.amazon.coral.service#ResourceNotFoundException', header x-amzn-query-error 'ResourceNotFoundException;Sender', and no message member in the body.
6. Re-replay that fixture/step after the fix to confirm all four facets (status, __type, query-error header, message) match.
