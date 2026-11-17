Issue-Source: trace-sweep-divergence
Issue-Trace: kafka-create-cluster-v2-validation-errors
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kafka-create-cluster-v2-validation-errors
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka|CreateClusterV2|4xx->4xx|$.message
Issue-Commit: cbdd8eddbbdb1b7ec4c9861dd469ed3eef3832d3

# MSK CreateClusterV2 cluster-type validation refusal carries emulator prose instead of AWS's "Invalid request body"

> _Note: this issue was identified against the code base at commit `cbdd8eddbbdb1b7ec4c9861dd469ed3eef3832d3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateClusterV2

## Description
ONE root cause, net-new: the replay report lists no stub_path and no owning open issue for this cluster (owned_by is empty), so no filed issue carries this step's replay key.

Cause 1 of 1: CreateClusterV2's cluster-type validation refusal re-words the message AWS returns.

AWS: CreateClusterV2 takes a required clusterName plus the two mutually-exclusive cluster-type members provisioned and serverless (Smithy: CreateClusterV2 POST /api/v2/clusters, input CreateClusterV2Request members ClusterName (jsonName clusterName, @required, @length min 1 max 64), Provisioned (jsonName provisioned, target ProvisionedRequest), Serverless (jsonName serverless, target ServerlessRequest); errors BadRequestException httpError 400, ConflictException 409, ForbiddenException 403, UnauthorizedException 401, TooManyRequestsException 429, InternalServerErrorException 500, ServiceUnavailableException 503). The model does not state the mutual-exclusion message, so for the wording this is recorded behavior only: MSK refuses a request whose cluster-type selection is not a single valid choice with HTTP 400, x-amzn-errortype BadRequestException, and the body message "Invalid request body" — that sentence is entirely FIXED text, with no parameter name, cluster name or type token interpolated into it, and the recorded payload carries no invalidParameter member for this refusal (both status and invalidParameter already match; only the message diverges). Hypothesis: MSK rejects this at request-body deserialization, before any per-member validator runs, which is why the text names no member.

Emulator: status and error class are correct (HTTP 400, x-amzn-errortype BadRequestException), but the body's message is the emulator's own sentence, "You can specify only one cluster type, either provisioned or serverless." instead of the fixed text "Invalid request body".

Why: a client that matches, logs or snapshots MSK's refusal text does not recognise the emulator's sentence, so error-classification code that distinguishes a malformed request body from the operation's other 400s (quota, duplicate name, bad broker spec) takes the wrong branch; a test asserting MSK's message fails against the emulator even though the HTTP status and error code are right. The emulator is also more informative than AWS here, so code written against it assumes a member-level explanation AWS never sends.

Fix: the CreateClusterV2 handler's input validation in the kafka service (the cluster-type selection check that rejects a request not naming exactly one of provisioned/serverless) — emit MSK's fixed message text "Invalid request body" for this BadRequestException instead of locally authored prose, and leave invalidParameter unset as the recording shows. Related but distinct open issues, neither of which covers this step: 20261107T230554Z-kafka-createclusterv2-s-unsupported-kafkaversion-refusal-offers-versions-msk.md (CreateClusterV2's unsupported-KafkaVersion refusal) and 20261107T210159Z-kafka-msk-control-plane-refusals-two-reworded-messages-updatechannel.md (the same reworded-message defect on other operations and behavioral check ids).

Traces: kafka-create-cluster-v2-validation-errors#task#2.

## Evidence
Wire replay of fixture kafka-create-cluster-v2-validation-errors, step task#2, operation CreateClusterV2 (service kafka), verdict mismatch:

  status: expected 400, got 400 (matches)
  $headers.x-amzn-errortype: BadRequestException (matches)
  $.invalidParameter: matches (absent on both sides)
  $.message: value_mismatch
      expected (AWS): 'Invalid request body'
      actual (emulator): 'You can specify only one cluster type, either provisioned or serverless.'

Fixed vs variable text: the expected message "Invalid request body" is entirely fixed text — no part of it is taken from the request or from emulator state, so there is no per-run value in it. The emulator's replacement sentence is not a reformatting of AWS's text; it is different prose and additionally volunteers the member names provisioned/serverless, which the recorded AWS payload never mentions.

Smithy evidence cited: CreateClusterV2 (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json) binds POST /api/v2/clusters, models BadRequestException with httpError 400, and declares Provisioned and Serverless as two separate optional structure members (not a union), so the mutual-exclusion rule is enforced by the service, not by the model — the model says nothing about the message, hence 'recorded behavior only' for the wording.

Step counts for the run: 163 steps replayed, 132 passed, 31 mismatched, score 81.0%; 23 divergence clusters, of which this is the only one not owned by an open issue or blocked by a stub.

## Steps to Reproduce
1. Boot the emulator and point an MSK (kafka) client at its data listener.
2. Call CreateClusterV2 with a valid clusterName and a cluster-type selection that is not exactly one valid choice — i.e. the request body of fixture kafka-create-cluster-v2-validation-errors step task#2, which supplies both the provisioned and the serverless member.
3. Observe the refusal: HTTP 400 with x-amzn-errortype: BadRequestException (correct) and body {"message": "You can specify only one cluster type, either provisioned or serverless."}.
4. Assert instead that the body message is exactly the fixed text "Invalid request body" and that no invalidParameter member is present.
5. Re-replay fixture kafka-create-cluster-v2-validation-errors step task#2 to confirm the fix; the fixture's other steps already pass and must keep passing.
