Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot-data.publish-wildcard-topic-rejected
Issue-Shape-Id: com.amazonaws.iotdataplane#IotMoonrakerService
Issue-Key: iot-data.publish-wildcard-topic-rejected|run#18|AssertionError|assert read_code == "InvalidRequestException", (
Issue-Commit: 8470bd15600894c04d6297f04472e87ed5582e16

# REOPENS: iot-data GetRetainedMessage reports an MQTT-wildcard topic as absent (404 ResourceNotFoundException) instead of refusing it 400 InvalidRequestException "Invalid topic filter"

> _Note: this issue was identified against the code base at commit `8470bd15600894c04d6297f04472e87ed5582e16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRetainedMessage

## Description
REOPENS 20261029T175031Z-iot-data-three-refusals-carry-the-wrong-error.md: its fix left the call refused. That issue's Cause 2 carried this exact replay key (iot-data.publish-wildcard-topic-rejected | run#18 | assert read_code == "InvalidRequestException") and was marked fixed, but the read path again answers the wildcard topic with a not-found refusal rather than the malformed-topic refusal. The regression looks like the pre-store guard being removed wholesale when 20261106T065146Z-iot-data-getretainedmessage-refuses-an-over-deep-topic.md was fixed (that issue asked the depth/length ceilings NOT to be applied on the read path); AWS separates the two rules, so removing the whole guard is too much.

Cause 1 of 1: GetRetainedMessage performs a bare retained-store lookup with no topic-name legality check, so a topic containing an MQTT subscription wildcard is answered as a well-formed topic that holds nothing.

AWS: Smithy (com.amazonaws.iotdataplane#IotMoonrakerService) GetRetainedMessage — GET /retainedMessage/{topic}, input topic : Topic (@httpLabel @required, no length/pattern constraint on Topic), modelled errors InternalFailureException 500, InvalidRequestException 400, MethodNotAllowedException 405, ResourceNotFoundException 404, ServiceUnavailableException 503, ThrottlingException 429, UnauthorizedException 401 — i.e. both classes are available on this operation. The model states nothing about wildcards, so for the rule itself this is recorded behavior only: a topic carrying an MQTT subscription wildcard ('#' or '+') is not a legal topic identity on EITHER path. The recording shows Publish refusing it HTTP 400 InvalidRequestException with the message "Invalid topic filter" (entirely fixed text — no part of it comes from the request), and GetRetainedMessage refusing the SAME name identically: HTTP 400 InvalidRequestException "Invalid topic filter". A legal-but-absent sibling topic is the contrast: HTTP 404 ResourceNotFoundException with the service's fixed retained-message not-found text. The depth/byte ceilings are NOT read-path rules (an over-deep topic reads back 404) — only the wildcard-name rule is symmetric.

Emulator: Publish still refuses the wildcard correctly (400 InvalidRequestException "Invalid topic filter"), nothing is stored under the literal wildcard name, and the legal-but-absent sibling still answers 404 — all of those assertions pass. The read path diverges: GetRetainedMessage(topic="<base>/#") answers Error.Code ResourceNotFoundException (HTTP 404) where AWS answers 400 InvalidRequestException "Invalid topic filter".

Why: the 400-vs-404 distinction is the whole signal a client branches on. A 404 tells the client the wildcard IS a valid topic name that merely holds no value, so a client that assembles topic names programmatically keeps a subscription filter in its request path and retries forever instead of being told the name is malformed; it also makes the read path asymmetric with Publish, which refuses the same string.

Fix: the GetRetainedMessage handler in the iot-data (iotmoonrakerservice) crate must apply the wildcard-name rule before the store lookup — a topic-legality check that refuses only MQTT wildcards with the service's fixed "Invalid topic filter" sentence, while deliberately NOT re-applying the publish-time part-count and byte-length ceilings (those must keep yielding the operation's 404, per the other fixed issue). Sharing the publish-side validator wholesale is what caused the earlier flip-flop; the read path needs the wildcard clause only.

Traces: iot-data.publish-wildcard-topic-rejected.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1), heading `### iot-data`, check [mismatch] iot-data.publish-wildcard-topic-rejected:

  File "check_iot_data_publish_wildcard_topic_rejected.py", line 136, in run
    assert read_code == "InvalidRequestException", (
  AssertionError: GetRetainedMessage must refuse the wildcard topic 'sim2real-behav-iotdata-wild-<id>/#' as malformed rather than report it absent, got 'ResourceNotFoundException'

Expected: GetRetainedMessage(topic="<base>/#") -> HTTP 400, Error.Code InvalidRequestException, Error.Message "Invalid topic filter" (all fixed text).
Actual:   HTTP 404, Error.Code ResourceNotFoundException.

Already correct in this run and must not regress:
  - Publish(topic="<base>/#", retain=true, qos=1) -> 400 InvalidRequestException "Invalid topic filter".
  - No retained message appears under the literal "<base>/#" in ListRetainedMessages, and the matching leaf topic still holds its original payload (no fan-out).
  - GetRetainedMessage(topic="<base>/never") -> 404 ResourceNotFoundException (legal-but-absent contrast).

The topic prefix 'sim2real-behav-iotdata-wild-<id>' is this run's generated name and is not part of the requirement; the requirement is the wildcard character in the topic and the resulting status/error code/message.

## Steps to Reproduce
1. Pick base = "<unique>"; publish a retained message at "<base>/leaf" (iot-data:Publish payload=b"x", retain=true, qos=1).
2. iot-data:Publish(topic="<base>/#", payload=b"y", retain=true, qos=1) -> 400 InvalidRequestException "Invalid topic filter" (already correct).
3. iot-data:GetRetainedMessage(topic="<base>/#") -> AWS answers HTTP 400 Error.Code InvalidRequestException, Error.Message "Invalid topic filter". Observe the emulator answers HTTP 404 ResourceNotFoundException.
4. Regression guards: iot-data:GetRetainedMessage(topic="<base>/never") must stay 404 ResourceNotFoundException, and GetRetainedMessage on a 9-level or >256-byte topic must stay 404 (do not re-introduce the publish-time depth/length refusal on the read path).
5. Replay iot-data.publish-wildcard-topic-rejected.
