Issue-Source: trace-sweep-divergence
Issue-Trace: mq-catalog-describe-broker-engine-types
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mq-catalog-describe-broker-engine-types mq-rmq-traj-recreate-rabbitmq-broker-refusals
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq|DescribeBrokerEngineTypes|2xx->2xx|$.brokerEngineTypes[*]
Issue-Key: mq|DescribeBrokerEngineTypes|4xx->4xx|$.message
Issue-Commit: f55279b3dc5678191fb0e8465c0432e63e772ce0

# mq DescribeBrokerEngineTypes serves a stale RabbitMQ version catalog and a non-AWS invalid-engine message (2 root causes)

> _Note: this issue was identified against the code base at commit `f55279b3dc5678191fb0e8465c0432e63e772ce0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBrokerEngineTypes

## Description
Two net-new root causes in the mq DescribeBrokerEngineTypes handler: the engine catalog it publishes for RABBITMQ is stale, and the BadRequestException it raises for an unknown engineType is not worded the way AWS words it. Both are independent and both must be closed.

Cause 1 of 2 — the RABBITMQ entry of the engine-type catalog omits every currently published 4.x engine version.
AWS: DescribeBrokerEngineTypes (Smithy: GET /v1/broker-engine-types, output DescribeBrokerEngineTypesResponse.brokerEngineTypes -> __listOfBrokerEngineType; BrokerEngineType.engineType -> EngineType enum [ACTIVEMQ, RABBITMQ], BrokerEngineType.engineVersions -> __listOfEngineVersion) returns, for each supported engine type, the full list of engine versions that engine currently accepts, newest first. The model does not enumerate the version strings, so this is recorded behavior only: the recording shows the RABBITMQ entry carrying engineVersions [{name: '4.3'}, {name: '4.2'}, {name: '3.13'}]. The same version set is the one the service quotes back in its own CreateBroker refusal message ("Valid values: [4.3, 4.2, 3.13]."), so the catalog and the validator are expected to agree.
Emulator: the RABBITMQ entry is served with a single version, engineVersions [{name: '3.13'}]; 4.3 and 4.2 are absent. Status is 200 either way, so nothing signals the truncation.
Why: DescribeBrokerEngineTypes is the discovery call a client (console, CDK/Terraform provider, or a version-pinning script) makes to choose an engineVersion before CreateBroker. Against the emulator the only RabbitMQ version discoverable is 3.13, so a client that selects the newest supported version picks 3.13 instead of 4.3, and a client that validates a user-supplied 4.3/4.2 against the catalog rejects a version real AWS accepts. Version-matrix assertions and upgrade paths exercised against the emulator therefore cannot be reproduced on AWS.
Fix: the static engine catalog backing the DescribeBrokerEngineTypes handler — the RABBITMQ engine-version list must carry the full published set in newest-first order, and must be the same source of truth the CreateBroker engineVersion validator quotes. (Note: the CreateBroker engineVersion-refusal step of fixture mq-rmq-traj-recreate-rabbitmq-broker-refusals is owned by the open issue 20261103T045546Z-mq-refuses-invalid-requests-with-the-wrong-status.md; that issue does not cover the catalog this operation publishes, which is what this cause is about. The open issue 20261107T120000Z-mq-describebrokerinstanceoptions-catalog-is-missing-eight-published-instance-option-combinations.md covers the separate DescribeBrokerInstanceOptions catalog, not this one.)
Traces: mq-catalog-describe-broker-engine-types#task#1 ($.brokerEngineTypes[1] value_mismatch: expected engineType RABBITMQ with engineVersions ['4.3','4.2','3.13'], got engineType RABBITMQ with engineVersions ['3.13']).

Cause 2 of 2 — the invalid-engineType refusal message does not match AWS's wording for that error class.
AWS: an engineType query parameter that is not a member of the EngineType enum (Smithy: EngineType values [ACTIVEMQ, RABBITMQ]) is refused with BadRequestException (Smithy: DescribeBrokerEngineTypes errors BadRequestException, httpError 400; members message, errorAttribute). The recorded message is 'Broker engine [NOTREAL] is invalid.' — the fixed text is 'Broker engine [' ... '] is invalid.', and the only variable part is the rejected engine-type value taken verbatim from the request's engineType query parameter. AWS does NOT append a valid-values list to this message.
Emulator: the status (400) and the error class are right, but the message reads 'Broker engine type [NOTREAL] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]' — it says 'Broker engine type' where AWS says 'Broker engine', and appends a 'Valid values are: [...]' sentence AWS does not emit.
Why: the message is part of the contract for this error class. A client or test that asserts on, logs, or pattern-matches the refusal text (and SDK/CLI users see exactly this string) gets different text from the emulator than from AWS, so a passing emulator assertion does not hold against AWS and the reverse.
Fix: the DescribeBrokerEngineTypes input validation that builds the BadRequestException for an unrecognised engineType — emit AWS's fixed text with the request's engineType value interpolated and no valid-values suffix. The value in the trace ('NOTREAL') is a request value filling the template, not part of it.
Traces: mq-rmq-traj-recreate-rabbitmq-broker-refusals#task#2 ($.message value_mismatch, expected 'Broker engine [NOTREAL] is invalid.', got 'Broker engine type [NOTREAL] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]'; status 400 on both sides).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 68 steps, 47 passed, score 69.1%).

Cause 1 — fixture mq-catalog-describe-broker-engine-types, step task#1, DescribeBrokerEngineTypes, expected_status 200 / actual_status 200:
  $.brokerEngineTypes[1]: missing (expected {'engineType': 'RABBITMQ', 'engineVersions': [{'name': '4.3'}, {'name': '4.2'}, {'name': '3.13'}]}, got {'engineType': 'RABBITMQ', 'engineVersions': [{'name': '3.13'}]})
  $.brokerEngineTypes[1]: extra (same pair, reported from the other side)
Expected engineVersions for RABBITMQ: ['4.3', '4.2', '3.13'] (newest first). Actual: ['3.13'] only.

Cause 2 — fixture mq-rmq-traj-recreate-rabbitmq-broker-refusals, step task#2, DescribeBrokerEngineTypes, expected_status 400 / actual_status 400:
  $.message: value_mismatch
    expected: 'Broker engine [NOTREAL] is invalid.'
    actual:   'Broker engine type [NOTREAL] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]'
Fixed text per AWS: 'Broker engine [' + <requested engineType> + '] is invalid.' — no valid-values suffix. Status and error class (BadRequestException, 400) already agree.

Smithy consulted (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json): DescribeBrokerEngineTypes http {GET /v1/broker-engine-types, code 200}, output member brokerEngineTypes -> __listOfBrokerEngineType; BrokerEngineType {engineType: EngineType, engineVersions: __listOfEngineVersion}; EngineType enum values [ACTIVEMQ, RABBITMQ]; errors BadRequestException (client, httpError 400) with members message / errorAttribute.

## Steps to Reproduce
Cause 1:
1. Boot the emulator from this checkout and replay fixture mq-catalog-describe-broker-engine-types, step task#1 (GET /v1/broker-engine-types with no engineType filter).
2. Read $.brokerEngineTypes and find the entry whose engineType is RABBITMQ.
3. Observe engineVersions is [{'name': '3.13'}]; AWS returns [{'name': '4.3'}, {'name': '4.2'}, {'name': '3.13'}].

Cause 2:
1. Replay fixture mq-rmq-traj-recreate-rabbitmq-broker-refusals, step task#2 (GET /v1/broker-engine-types?engineType=NOTREAL, i.e. a value outside the EngineType enum).
2. Observe HTTP 400 with $.message = 'Broker engine type [NOTREAL] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]'.
3. AWS returns HTTP 400 BadRequestException with $.message = 'Broker engine [NOTREAL] is invalid.' and no valid-values suffix.

Both steps are part of the simparity-wire-aws-v1 corpus; re-replay those two fixture steps after the fix to confirm.
