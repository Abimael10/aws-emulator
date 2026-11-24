Issue-Source: trace-sweep-divergence
Issue-Trace: mq-error-paths
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mq-error-paths
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq|CreateConfiguration|4xx->4xx|$.message
Issue-Commit: 2db67cdf583f83462fa29a2772c7057ac4b27efa

# mq CreateConfiguration rejects an invalid engineType with non-AWS message wording

> _Note: this issue was identified against the code base at commit `2db67cdf583f83462fa29a2772c7057ac4b27efa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfiguration

## Description
ONE cause.

Cause 1 of 1: CreateConfiguration refuses an unrecognized engineType with the right status and error shape but the emulator's own message wording, omitting the valid-values enumeration Amazon MQ appends (fixture mq-error-paths, step task#2).

AWS: Smithy (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json) models CreateConfiguration as POST /v1/configurations with required EngineType (jsonName "engineType") targeting enum com.amazonaws.mq#EngineType whose values are exactly [ACTIVEMQ, RABBITMQ], and errors BadRequestException (400) / ConflictException (409) / ForbiddenException (403) / InternalServerErrorException (500). BadRequestException carries members message (jsonName "message") and errorAttribute (jsonName "errorAttribute"), so the message text is part of the contract, not cosmetic. Recorded behavior for the text: status 400, BadRequestException, message "Broker engine type [NOTAREALENGINE] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]". Its fixed text is "Broker engine type [" + <the engineType value from the request> + "] is invalid. Valid values are: [" + <the service's EngineType enumeration, comma-space separated> + "]"; only the bracketed rejected value comes from the request, and the valid-values list is the modelled enum, not a per-recording value.

Emulator: answers the correct 400 BadRequestException but with message "Broker engine [NOTAREALENGINE] is invalid." — the noun phrase is "Broker engine" instead of "Broker engine type", and the entire ". Valid values are: [...]" clause enumerating the accepted engine types is missing. Replay: $.message: value_mismatch (expected 'Broker engine type [NOTAREALENGINE] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]', got 'Broker engine [NOTAREALENGINE] is invalid.'). Status, error shape and errorAttribute match.

Why: a client that surfaces or matches the service message gets text no AWS SDK or console would ever produce, and — more concretely — loses the remediation data AWS puts in the error: the list of engine types it may retry with. Interactive tooling and CLIs that echo the service message leave the user with no way to discover that ACTIVEMQ/RABBITMQ are the only accepted values, and a test asserting on the AWS wording passes against AWS and fails against the emulator.

Fix: CreateConfiguration's input validation / BadRequestException serializer — emit the AWS template above, formatting the rejected engineType from the request and the valid-values clause from the EngineType enumeration rather than a hardcoded literal, so it stays correct if the enum grows.

Related but not covering: the open issue 20261103T045546Z-mq-refuses-invalid-requests-with-the-wrong-status.md bundles eight mq request-rejection causes (ListBrokers maxResults bounds, UpdateConfiguration document parsing, DescribeConfigurationRevision bounds, CreateBroker engineVersion/hostInstanceType/password), and 20261030T170907Z-mq-configuration-operations-createconfiguration-wrongly-refuses-a-duplicate.md covers CreateConfiguration's duplicate-Name behavior; neither carries this step's replay key (the replay reported owned_by empty and no stub_path for this cluster) nor the engineType message template.

Traces: mq-error-paths#task#2 (CreateConfiguration, expected 400 / actual 400, $.message).

## Evidence
Replay summary: trace_set simparity-wire-aws-v1, emulator_up true, 9 fixtures replayed, 68 steps, 53 passed, 15 mismatched, 0 errored, 0 blocked, score 77.9%, 11 divergence clusters.

Cause 1 - mq-error-paths#task#2, CreateConfiguration, expected status 400 / actual status 400, verdict mismatch, owned_by empty, stub_path empty:
  $.message: value_mismatch (expected 'Broker engine type [NOTAREALENGINE] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]', got 'Broker engine [NOTAREALENGINE] is invalid.')

Fixed text of the AWS message: 'Broker engine type [' ... '] is invalid. Valid values are: [' ... ']'. Variable parts: the bracketed rejected engine type is the engineType member of the request (NOTAREALENGINE in this recording), and the trailing bracketed list is the service's EngineType enumeration (modelled as ACTIVEMQ, RABBITMQ). No per-run value (request id, timestamp, generated id) is involved in this divergence.

Model evidence: smithy_query operation=CreateConfiguration -> input member EngineType required, jsonName 'engineType', target enum EngineType; errors include BadRequestException httpError 400; http POST /v1/configurations. smithy_query shape=BadRequestException -> members message/errorAttribute; shape=EngineType -> values [ACTIVEMQ, RABBITMQ].

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1 - fixture mq-error-paths step task#2: POST /v1/configurations with a body whose engineType is a value outside the EngineType enum (e.g. engineType=NOTAREALENGINE) plus a name. AWS answers 400 BadRequestException with message 'Broker engine type [<value>] is invalid. Valid values are: [ACTIVEMQ, RABBITMQ]'.
4. Observe the emulator answers 400 BadRequestException with message 'Broker engine [<value>] is invalid.' — wrong noun phrase and no valid-values clause.
5. Re-replay mq-error-paths#task#2 after the fix; the step must pass with $.message matching the AWS template.
