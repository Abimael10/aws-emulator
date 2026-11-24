Issue-Source: trace-sweep-divergence
Issue-Trace: medialive-cmc-traj-role-mistakes-and-idempotent-retry
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medialive-cmc-traj-role-mistakes-and-idempotent-retry
Issue-Shape-Id: com.amazonaws.medialive#MediaLive
Issue-Key: medialive|CreateChannel|4xx->4xx|$.validationErrors
Issue-Key: medialive|CreateChannel|2xx->4xx|$.channel
Issue-Key: medialive|CreateChannel|2xx->4xx|status
Issue-Key: medialive|DeleteInput|2xx->4xx|$.arn
Issue-Key: medialive|DeleteInput|2xx->4xx|$.attachedChannels
Issue-Key: medialive|DeleteInput|2xx->4xx|$.destinations
Issue-Key: medialive|DeleteInput|2xx->4xx|$.id
Issue-Key: medialive|DeleteInput|2xx->4xx|$.inputClass
Issue-Key: medialive|DeleteInput|2xx->4xx|$.inputDevices
Issue-Key: medialive|DeleteInput|2xx->4xx|$.inputNetworkLocation
Issue-Key: medialive|DeleteInput|2xx->4xx|$.inputPartnerIds
Issue-Key: medialive|DeleteInput|2xx->4xx|$.inputSourceType
Issue-Key: medialive|DeleteInput|2xx->4xx|$.mediaConnectFlows
Issue-Key: medialive|DeleteInput|2xx->4xx|$.name
Issue-Key: medialive|DeleteInput|2xx->4xx|$.securityGroups
Issue-Key: medialive|DeleteInput|2xx->4xx|$.smpte2110ReceiverGroupSettings
Issue-Key: medialive|DeleteInput|2xx->4xx|$.sources
Issue-Key: medialive|DeleteInput|2xx->4xx|$.state
Issue-Key: medialive|DeleteInput|2xx->4xx|$.tags
Issue-Key: medialive|DeleteInput|2xx->4xx|$.type
Issue-Key: medialive|DeleteInput|2xx->4xx|status
Issue-Key: medialive|DescribeInput|2xx->2xx|$.attachedChannels[*]
Issue-Key: medialive|DescribeInput|2xx->2xx|$.destinations[*]
Issue-Key: medialive|DescribeInput|2xx->2xx|$.state
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|$.arn
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|$.channels
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|$.id
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|$.inputs
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|$.state
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|$.tags
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|$.whitelistRules
Issue-Key: medialive|DeleteInputSecurityGroup|2xx->4xx|status
Issue-Related: 20261102T091340Z-medialive-request-validation-missing-or-misworded-across-channel.md
Issue-Commit: 2aee2ac365d85baf75c5f3ecd46379a60b3ff728

# medialive CreateChannel commits a refused channel: the roleArn trust refusal never fires and the stuck input attachment then blocks the retry and the whole teardown

> _Note: this issue was identified against the code base at commit `2aee2ac365d85baf75c5f3ecd46379a60b3ff728`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannel

## Description
One MediaLive trajectory (create an input security group, create an RTMP_PUSH input, try CreateChannel with role ARNs MediaLive cannot assume, then retry with a good role and tear the estate down) diverges in five places. The shared fault is that CreateChannel never evaluates the roleArn against IAM, so a create AWS refuses at validation is COMMITTED instead, and the input attachment that commit takes is never released: every later call that needs the input free (the legitimate retry, DeleteInput, DescribeInput's DELETED poll, DeleteInputSecurityGroup) is then refused 409 or reports attached state. This bundle has 5 causes; all five must be closed.

Cause 1 of 5 — CreateChannel with a role that does not trust medialive.amazonaws.com is answered 409 ConflictException instead of the modeled 422 roleArn validation report.
AWS: CreateChannel (Smithy: POST /prod/channels, success code 201; modeled errors BadRequestException 400, ConflictException 409, ForbiddenException 403, UnprocessableEntityException httpError 422, …; UnprocessableEntityException members Message jsonName "message" and ValidationErrors jsonName "validationErrors" of ValidationError{elementPath, errorMessage}) resolves the request's RoleArn against IAM and checks the role's trust policy BEFORE any attachment/conflict rule. A role that exists but does not name medialive.amazonaws.com as a trusted service principal is refused with HTTP 422 UnprocessableEntityException whose message is the elementPath followed by the sentence — "roleArn Make sure the role '<roleArn from the request>' exists in the user account and medialive.amazonaws.com is a trusted service" — and whose validationErrors is exactly one entry [{elementPath: "roleArn", errorMessage: "Make sure the role '<roleArn from the request>' exists in the user account and medialive.amazonaws.com is a trusted service"}]. Fixed text: "roleArn Make sure the role '" / "' exists in the user account and medialive.amazonaws.com is a trusted service" and the elementPath literal "roleArn"; the only variable part is the role ARN echoed from the request. Because AWS refuses here, nothing is created and the named input is NOT attached.
Emulator: HTTP 409 with x-amzn-errortype ConflictException and message "inputId=<input id> is already attached" — no validationErrors member at all. The roleArn is never resolved, and the attachment conflict (left over by the earlier create this same handler should also have refused) is evaluated ahead of input validation.
Why: a client probing its IAM wiring is told its input is double-booked rather than that its role is untrusted; it branches on 409 (free the input) instead of 422 (fix the trust policy), and tooling that reads validationErrors[].elementPath to point at the offending member gets nothing.
Fix: CreateChannel's validation in its handler — resolve RoleArn and its trust policy and raise UnprocessableEntityException with the modeled validationErrors list, ordered ahead of the input-attachment conflict check.
Traces: medialive-cmc-traj-role-mistakes-and-idempotent-retry#task#8.

Cause 2 of 5 — the legitimate retry of CreateChannel with a good role is refused 409 because the emulator still holds the input for a channel AWS never created.
AWS: once the bad-role creates have been refused, the input is still unattached, so the corrected CreateChannel (same name, same single RTMP_PUSH inputAttachment, same two HLS destination settings) succeeds with HTTP 201 and the channel body (state "CREATING", pipelinesRunningCount 0, the inputAttachments entry naming the input with its defaulted inputSettings, the destinations/egressEndpoints and encoderSettings as submitted). Smithy: CreateChannel success code 201, output member Channel jsonName "channel".
Emulator: HTTP 409 ConflictException "inputId=<input id> is already attached" and no channel member — the attachment is held by a channel that exists only because a refused create was committed, and no client action can release it.
Why: the idempotent-retry-after-fixing-the-role path — the normal way a client recovers from a 422 — is unreachable; the client sees a conflict it cannot explain or clear.
Fix: CreateChannel's state writes must be atomic with its validation (take the input attachment only after the whole request validates), so a refused create leaves no channel and no attachment behind.
Traces: medialive-cmc-traj-role-mistakes-and-idempotent-retry#task#9.

Cause 3 of 5 — DeleteInput is refused 409 for an input whose only attachment comes from that uncommitted-create leak.
AWS: DeleteInput (Smithy: DELETE /prod/inputs/{InputId}, success code 200; modeled errors BadRequestException 400, ConflictException 409, NotFoundException 404, …) accepts the delete of an input no live channel holds and answers HTTP 200; recorded behavior only for the body (the model declares an empty output shape, the recording shows the input document: arn, id, name, type "RTMP_PUSH", state "DELETING", sources [], destinations with the two push endpoints, attachedChannels [], securityGroups, tags {}, inputClass "STANDARD", inputNetworkLocation "AWS", inputSourceType "STATIC", smpte2110ReceiverGroupSettings{smpte2110ReceiverGroups: []}, mediaConnectFlows [], inputDevices [], inputPartnerIds []).
Emulator: HTTP 409 with no body members at all — the delete is refused because of the phantom attachment.
Why: teardown is impossible; the input (and everything referencing it) leaks for the lifetime of the emulator, and a client's cleanup step fails with a conflict that no sequence of calls can resolve.
Fix: the input-attachment bookkeeping in medialive state (an attachment must exist only for a channel a successful CreateChannel actually persisted) plus DeleteInput's precondition check reading it.
Traces: medialive-cmc-traj-role-mistakes-and-idempotent-retry#teardown#15.

Cause 4 of 5 — DescribeInput never converges to DELETED: the input is reported ATTACHED with a phantom channel id and its push destinations still present.
AWS: DescribeInput (Smithy: GET /prod/inputs/{InputId}, code 200; State targets InputState with values CREATING | DETACHED | ATTACHED | DELETING | DELETED) reports state "DELETED" once the delete settles, with attachedChannels empty and no destinations member for the tombstoned input.
Emulator: state stays "ATTACHED" across the whole poll window (10 attempts), attachedChannels still carries the phantom channel id, and destinations still lists both rtmp:// push endpoints.
Why: a client that waits for DELETED before releasing dependent resources (its security group, its own records) waits forever, and a lifecycle state machine driven off DescribeInput never terminates.
Fix: the input lifecycle in medialive state — the detach-on-channel-delete / delete-settles transition, so a deleted input reaches DELETED and drops its attachment and destinations.
Traces: medialive-cmc-traj-role-mistakes-and-idempotent-retry#teardown#16.

Cause 5 of 5 — DeleteInputSecurityGroup is refused 409, a status the operation does not even model, because the stale input still references the group.
AWS: DeleteInputSecurityGroup (Smithy: DELETE /prod/inputSecurityGroups/{InputSecurityGroupId}, success code 200; modeled errors BadRequestException 400, ForbiddenException 403, NotFoundException 404, InternalServerErrorException 500, BadGatewayException 502, GatewayTimeoutException 504, TooManyRequestsException 429 — ConflictException is NOT in this operation's error set) accepts the delete once no input references the group and answers HTTP 200; recorded behavior only for the body (the model declares an empty output, the recording shows arn, id, whitelistRules [{cidr: "0.0.0.0/0"}], inputs [], channels [], state "IDLE", tags {}).
Emulator: HTTP 409 with no body members — it refuses on a reference held by the input that cause 3 could not delete, and it does so with an error class this operation does not declare.
Why: the last teardown step fails, so the estate cannot be cleaned up; a client branching on the operation's modeled errors has no 409 branch and treats the refusal as an unknown protocol error.
Fix: DeleteInputSecurityGroup's precondition check must read a reference list that is kept accurate by the input lifecycle (causes 3 and 4), and must refuse only with an error class this operation models. The related open issue 20261108T083110Z-medialive-input-security-group-stays-in-use-with.md covers a stale group reference left after a SUCCESSFUL input delete; it does not cover this path, where the input delete itself is refused, and does not carry this fixture step's replay key.

Related open issues named for context, none of which carries these replay keys: 20261102T091340Z-medialive-request-validation-missing-or-misworded-across-channel.md (owns the earlier step of this same fixture, task#7, where CreateChannel with a nonexistent role is answered 201 — the create whose commit poisons the state the four causes above trip over) and 20261103T014723Z-medialive-refusals-missing-or-misworded-cross-field-reference.md (the behavioral counterpart of the missing roleArn checks).

All ids, names and ARNs in the replay output (channel id 1000000, input id 1000000, the sim2real-probe-ml-* names, the 198.18.0.x rtmp:// URLs, the role ARNs) are per-run fillers for the variable slots of these messages and documents, not literal requirements.

## Evidence
Replay of simparity-wire-aws-v1 against this checkout, fixture medialive-cmc-traj-role-mistakes-and-idempotent-retry (all five verdicts: mismatch, service medialive):

1. task#8 CreateChannel (role exists but trusts another principal) — expected HTTP 422 with $.message "roleArn Make sure the role '<roleArn>' exists in the user account and medialive.amazonaws.com is a trusted service" and $.validationErrors [{errorMessage: "Make sure the role '<roleArn>' exists in the user account and medialive.amazonaws.com is a trusted service", elementPath: "roleArn"}]; got HTTP 409, $.message 'inputId=1000000 is already attached', $.validationErrors absent, header x-amzn-errortype 'ConflictException'.
2. task#9 CreateChannel (corrected role) — expected HTTP 201 with $.channel (state 'CREATING', pipelinesRunningCount 0, one inputAttachment 'MyRTMPInputAttachment', destination1 with two stream URLs, the AAC/HLS encoderSettings); got HTTP 409 and no $.channel. Same cluster: task#11.
3. teardown#15 DeleteInput — expected HTTP 200 with the input document ($.state 'DELETING', $.attachedChannels [], $.type 'RTMP_PUSH', $.inputClass 'STANDARD', $.inputNetworkLocation 'AWS', $.securityGroups ['<group id>'], $.tags {}); got HTTP 409 with every one of those members absent.
4. teardown#16 DescribeInput — expected $.state 'DELETED' (polled); got $.state 'ATTACHED' after 10 attempts, with extra $.attachedChannels[0] '1000000' and extra $.destinations[0]/[1] (rtmp://198.18.0.0:1935/live/stream1, rtmp://198.18.0.1:1935/live/stream2).
5. teardown#17 DeleteInputSecurityGroup — expected HTTP 200 with $.whitelistRules [{cidr: '0.0.0.0/0'}], $.inputs [], $.channels [], $.state 'IDLE', $.tags {}; got HTTP 409 (an error class absent from this operation's modeled error set) with no body members.

Model references used (services/service-com-amazonaws-medialive-medialive/smithy-model/com-amazonaws-medialive-medialive.json): CreateChannel POST /prod/channels code 201, errors {BadRequestException 400, ConflictException 409, ForbiddenException 403, UnprocessableEntityException 422, …}, output member Channel jsonName 'channel'; UnprocessableEntityException {message, validationErrors: [ValidationError{elementPath, errorMessage}]}; ConflictException {message}; DeleteInput DELETE /prod/inputs/{InputId} code 200 with ConflictException 409 and NotFoundException 404 modeled, empty output shape; DescribeInput GET /prod/inputs/{InputId} code 200, State -> InputState [CREATING, DETACHED, ATTACHED, DELETING, DELETED]; DeleteInputSecurityGroup DELETE /prod/inputSecurityGroups/{InputSecurityGroupId} code 200, errors {BadRequestException 400, ForbiddenException 403, NotFoundException 404, TooManyRequestsException 429, InternalServerErrorException 500, BadGatewayException 502, GatewayTimeoutException 504} — no ConflictException.

Context from the same fixture, already owned by 20261102T091340Z-medialive-request-validation-missing-or-misworded-across-channel.md: task#7 CreateChannel with a nonexistent role was answered 201 instead of the same 422, and that committed channel (id 1000000) is what holds the input in all five causes above.

## Steps to Reproduce
1. Clone this checkout, boot the emulator and replay the wire corpus simparity-wire-aws-v1; fixture medialive-cmc-traj-role-mistakes-and-idempotent-retry carries every step below.
2. Setup (steps that pass): POST /prod/inputSecurityGroups with whitelistRules [{cidr: '0.0.0.0/0'}]; POST /prod/inputs with type RTMP_PUSH, two destinations and that security group.
3. task#7: POST /prod/channels naming a roleArn whose role does not exist -> AWS refuses 422; the emulator answers 201 and attaches the input (already filed; it is the precondition for the rest).
4. task#8 (cause 1): POST /prod/channels naming a role that exists but trusts a non-medialive principal -> expect 422 UnprocessableEntityException with message 'roleArn Make sure the role '<roleArn>' exists in the user account and medialive.amazonaws.com is a trusted service' and validationErrors[0].elementPath 'roleArn'; observe 409 ConflictException 'inputId=<id> is already attached'.
5. task#9 / task#11 (cause 2): POST /prod/channels again with a role that does exist and trusts medialive.amazonaws.com, same inputAttachment -> expect 201 with the channel body in state CREATING; observe 409 ConflictException.
6. Teardown the channel/input the fixture created, then teardown#15 (cause 3): DELETE /prod/inputs/{inputId} -> expect 200 with the input document in state DELETING and attachedChannels []; observe 409.
7. teardown#16 (cause 4): GET /prod/inputs/{inputId} repeatedly -> expect state to reach DELETED; observe ATTACHED for all 10 polls with attachedChannels still naming the phantom channel and both push destinations still present.
8. teardown#17 (cause 5): DELETE /prod/inputSecurityGroups/{groupId} -> expect 200 with inputs [], channels [], state IDLE; observe 409, an error class this operation does not model.
