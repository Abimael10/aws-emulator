Issue-Source: trace-sweep-divergence
Issue-Trace: opensearchserverless-vpc-endpoint-errors
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearchserverless-vpc-endpoint-errors
Issue-Shape-Id: com.amazonaws.opensearchserverless#OpenSearchServerless
Issue-Key: aoss|CreateVpcEndpoint|4xx->4xx|$.message
Issue-Commit: 1332dc451a9c2b4446e90cf355e9a9abe7c79725

# opensearchserverless: CreateVpcEndpoint words its missing-VPC ValidationException unlike AWS

> _Note: this issue was identified against the code base at commit `1332dc451a9c2b4446e90cf355e9a9abe7c79725`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateVpcEndpoint

## Description
1 cause.

Cause 1 of 1 — the message CreateVpcEndpoint refuses a `vpcId` that names no VPC with diverges from the recorded one (fixture opensearchserverless-vpc-endpoint-errors, step task#1).
AWS: Smithy: CreateVpcEndpoint(name required, vpcId required — VpcId, @length 1..255, @pattern ^vpc-[0-9a-z]*$ —, subnetIds required, securityGroupIds, clientToken) with errors ConflictException(409), InternalServerException(500), ServiceQuotaExceededException(402), ValidationException(@httpError 400, "Thrown when the HTTP request contains invalid input or is missing required input"). The recorded AWS answer for a syntactically valid vpcId that resolves to no VPC of the account is HTTP 400 with the `ValidationException` code — that part the emulator already matches — and a `message` member. The recorded `message` TEXT is shape-asserted in the corpus (`<shaped-not-asserted>`) rather than literal, so this is recorded behavior only: the corpus records that AWS carries a message of a different shape than the one the emulator emits, and the replay reports the emulator's string as a value_mismatch against it. The requirement is the error class and a `message` of AWS's own wording for it; the VPC id in it is a value taken from the request, never part of the template. Hypothesis (labelled as such, neither the model nor the recording states it): AWS words this as a validation failure naming the offending input member rather than as EC2's resource sentence.
Emulator: answers HTTP 400 ValidationException with message "The Vpc with Id '<vpcId>' does not exist." — fixed text "The Vpc with Id '" … "' does not exist.", the only variable part being the `vpcId` the request carried (in the replayed step, the bogus id the fixture sends). The crate itself records that this sentence was never grounded in the recording: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/state/ec2_network.rs lines 98-103 say the corpus elides this refusal's recorded message and the sentence was "left as it stands rather than replaced by a guess". The replay now shows that guess does not match the recorded message.
Why: a client (or a human reading a provisioning log) that keys on the refusal text to tell "the VPC you named does not exist" from the other validation failures of this create (a duplicated subnet id, too many subnets, a subnet that does not exist) is handed wording AWS does not produce, so text-based branching and recorded-message assertions behave differently against the emulator than against AWS.
Fix: the refusal wording for an unresolvable VPC in the aoss VPC-endpoint placement component — `vpc_absent_message` — reworded to AWS's own sentence for this error class, with the request's `vpcId` as its only substituted value. The sibling `subnet_absent_message` is built from the same template and should be re-grounded with it. The error class (400 ValidationException) and the member it is raised from are already correct and must not change.
Traces: opensearchserverless-vpc-endpoint-errors#task#1 ($.message value_mismatch; status 400 expected and 400 observed). Code to look at: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/state/ec2_network.rs lines 90-109 (`vpc_absent_message`, `subnet_absent_message`) and src/ops/create_vpc_endpoint.rs lines 214-228 (where the placement refusal becomes the ValidationException message).

Note: no open aoss issue carries this fixture step or this refusal; the open opensearchserverless issues concern the published collectionEndpoint host, bedrock-agent knowledge-base storage configuration, control-plane IAM authorization and service-quotas default quota rows.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 150 steps, 145 passed, 3 mismatched).

Fixture opensearchserverless-vpc-endpoint-errors, step task#1, operation CreateVpcEndpoint, verdict mismatch:
  expected: HTTP 400 with $.message = <shaped, not literally asserted — the recorded AWS wording for a vpcId that names no VPC>
  actual:   HTTP 400 with $.message = "The Vpc with Id 'vpc-00000000000000000' does not exist."
  detail:   $.message: value_mismatch (expected '<shaped-not-asserted>', got "The Vpc with Id 'vpc-00000000000000000' does not exist.")
  divergence_key: service=aoss|CreateVpcEndpoint|status=400->400|keys=$.message

Status and error class agree (400 / ValidationException); the divergence is the message text only. Message anatomy of the emulator's string: fixed text "The Vpc with Id '" + "' does not exist."; variable part = the `vpcId` member of the request (the recording's vpc-00000000000000000 is only that run's filler value, not part of the requirement).

The emulator source concedes the wording is ungrounded: services/service-com-amazonaws-opensearchserverless-opensearchserverless/src/state/ec2_network.rs:98-103 — "NOT re-grounded against the recording: the wire corpus elides this refusal's recorded `message` (`<shaped-not-asserted>`) …".

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus: replay_wire_corpus() against trace set simparity-wire-aws-v1.
2. Fixture opensearchserverless-vpc-endpoint-errors, step task#1 — CreateVpcEndpoint naming a valid-looking `vpcId` (matching ^vpc-[0-9a-z]*$) that no VPC of the account carries, with a name and a subnetIds list.
3. Observe HTTP 400 ValidationException whose `message` is "The Vpc with Id '<vpcId>' does not exist.", which does not match the message the corpus records for this refusal.
4. Re-replay that fixture step after rewording the refusal; the step must pass with the status and error class unchanged.
