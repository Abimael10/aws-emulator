Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot-data.publish-response-topic-wildcard-rejected
Issue-Shape-Id: com.amazonaws.iotdataplane#IotMoonrakerService
Issue-Key: iot-data.publish-response-topic-wildcard-rejected|_error#10|AssertionError|raise AssertionError(f"expected a ClientError, but the call SUCCEEDED and returned {result!r}")
Issue-Commit: eb25315957aceaabd7d1a9e502f713207998e734

# iot-data Publish accepts a responseTopic carrying an MQTT wildcard with 200 instead of refusing it with InvalidRequestException

> _Note: this issue was identified against the code base at commit `eb25315957aceaabd7d1a9e502f713207998e734`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
Cause 1 of 1: `Publish` does not validate the `responseTopic` query parameter at all, so a request-response publish whose reply topic carries an MQTT subscription wildcard (`+` or `#`) is accepted and retained.

AWS: Smithy (services/service-com-amazonaws-iotdataplane-iotmoonrakerservice/smithy-model/com-amazonaws-iotdataplane-iotmoonrakerservice.json) models `Publish` as POST /topics/{topic} with `responseTopic` bound to the `responseTopic` httpQuery and documents it as "A UTF-8 encoded string that's used as the topic name for a response message. ... The topic must not contain wildcard characters." The operation models `InvalidRequestException` (client error, httpError 400, single `message` member). The modeled `ResponseTopic` shape carries no length/pattern constraint, so the rule is enforced by the service, not by constraint validation; recorded behavior shows the refusal is HTTP 400, `Error.Code` = `InvalidRequestException`, with the message `This response topic isn't valid. Response topics must not contain MQTT wildcard characters (+ or #)`. That whole string is fixed text for this error class — it interpolates nothing from the request, so it must be reproduced verbatim. The refusal happens before any state change: the publish must leave no retained message under its own topic, and a subsequent `GetRetainedMessage` on that topic must answer 404 `ResourceNotFoundException` with the message `Retained message not found`.

Emulator: the publish SUCCEEDS. The replay recorded `{'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '65', ...}}}` for a `Publish` with `retain=true, qos=1, responseTopic=<base>/refused/response#`. No error is raised, and because `retain=true` was honoured the refused message is also stored as the topic's retained message — state AWS never creates. The handler (services/service-com-amazonaws-iotdataplane-iotmoonrakerservice/src/ops/publish.rs) validates only `input.topic` (reserved `$` prefix, then `crate::state::topic_name_refusal` for wildcards/depth/length) and the payload size; `input.response_topic` is never inspected on any path.

Why: a client that uses the MQTT5 request-response flow relies on AWS rejecting a malformed reply topic at publish time. Against the emulator the bad publish looks accepted, so the sender believes a responder can reply on a topic no broker will ever accept a PUBLISH to, and the message silently goes unanswered; negative-path tests that assert the 400/`InvalidRequestException` contract pass against AWS and fail here. The spurious retained message additionally makes later reads of that topic return data AWS would 404 on.

Fix: `Publish`'s input validation in the operation handler — extend the pre-state-change refusal sequence in services/service-com-amazonaws-iotdataplane-iotmoonrakerservice/src/ops/publish.rs to reject a `responseTopic` containing `+` or `#` with `InvalidRequestException` and the service's fixed wording, ordered before the retained-message write and before rule dispatch so nothing is persisted. The wildcard predicate belongs with the existing topic rule in `crate::state` so one place owns it, but note the responseTopic message is its own fixed text and is NOT the publish-topic wildcard message.

Traces: iot-data.publish-response-topic-wildcard-rejected (the positive control in the same check — an ordinary `responseTopic` accepted and retained — already passes, so only the refusal leg diverges).

No existing open issue covers responseTopic validation; the nearest, 20261106T065146Z-iot-data-getretainedmessage-refuses-an-over-deep-topic.md and 20261029T224815Z-iot-data-validation-refusals-come-from-the-generic.md, concern GetRetainedMessage topic depth and the generic constraint-validator envelope respectively, not this unvalidated member.

## Evidence
Check iot-data.publish-response-topic-wildcard-rejected:

Expected (AWS): Publish(topic=<base>/refused, payload=..., retain=true, qos=1, responseTopic="<base>/refused/response#") -> HTTP 400, Error.Code="InvalidRequestException", Error.Message="This response topic isn't valid. Response topics must not contain MQTT wildcard characters (+ or #)"; then GetRetainedMessage(topic=<base>/refused) -> HTTP 404 ResourceNotFoundException, message "Retained message not found".

Actual (emulator): the Publish returned HTTP 200 with the {message, traceId} envelope:
  AssertionError: expected a ClientError, but the call SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '65', 'date': '...'}, 'RetryAttempts': 0}}
raised at check_iot_data_publish_response_topic_wildcard_rejected.py line 72 (_error wrapper, line 24). Because retain=true was accepted, a retained message now also exists under the publish topic, so the check's follow-on absence assertion could not even be reached.

Code evidence: services/service-com-amazonaws-iotdataplane-iotmoonrakerservice/src/ops/publish.rs lines 295-327 validate only `input.topic` and the payload length; `response_topic` appears nowhere in `handle`.

## Steps to Reproduce
1. Point an iot-data client at the emulator's data endpoint.
2. Positive control: publish(topic="t/control", payload=b"x", retain=True, qos=1, responseTopic="t/control/response") -> accepted, and get_retained_message(topic="t/control") returns the payload (this leg matches AWS).
3. Divergent leg: publish(topic="t/refused", payload=b"y", retain=True, qos=1, responseTopic="t/refused/response#").
   - AWS: ClientError, HTTP 400, Code=InvalidRequestException, Message="This response topic isn't valid. Response topics must not contain MQTT wildcard characters (+ or #)".
   - Emulator: HTTP 200 success.
4. get_retained_message(topic="t/refused"): AWS answers 404 ResourceNotFoundException "Retained message not found"; the emulator serves the message the refused publish stored.
5. Repeat with responseTopic="t/refused/+/response" — the single-level wildcard must be refused the same way.
