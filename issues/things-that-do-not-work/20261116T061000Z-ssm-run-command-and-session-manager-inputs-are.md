Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-list-command-invocations-filters-and-pagination
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-list-command-invocations-filters-and-pagination ssm-list-command-invocations-not-found-and-empty ssm-list-command-invocations-unknown-command-id ssm-vscan-traj-send-command-validation-order ssm-session-manager-connection-checks ssm-sshp-traj-session-manager-ssh-document-and-state-mistakes ssm-cloudconnector-create-invalid-input
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|ListCommandInvocations|2xx->4xx|$.CommandInvocations
Issue-Key: ssm|ListCommandInvocations|2xx->4xx|status
Issue-Key: ssm|SendCommand|4xx->4xx|$.message
Issue-Key: ssm|GetCommandInvocation|4xx->4xx|$.message
Issue-Key: ssm|TerminateSession|4xx->2xx|$
Issue-Key: ssm|TerminateSession|4xx->2xx|status
Issue-Key: ssm|StartSession|4xx->4xx|$.__type
Issue-Key: ssm|StartSession|4xx->4xx|$.message
Issue-Key: ssm|GetConnectionStatus|4xx->2xx|$.__type
Issue-Key: ssm|GetConnectionStatus|4xx->2xx|$.message
Issue-Key: ssm|GetConnectionStatus|4xx->2xx|status
Issue-Key: ssm|CreateCloudConnector|4xx->4xx|$.message
Issue-Commit: f8c3fa03cff68de185dd43b864dedf3dabbebd07

# ssm Run Command and Session Manager inputs are validated wrongly (7 causes)

> _Note: this issue was identified against the code base at commit `f8c3fa03cff68de185dd43b864dedf3dabbebd07`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCommandInvocations

## Description
Seven net-new causes in the SSM Run Command, Session Manager and CloudConnector request paths. Each is an independent defect and all seven must be closed before this issue is resubmitted.

=== Cause 1 of 7: ListCommandInvocations refuses every call with 400 where AWS answers 200 with an empty list ===
AWS: Smithy: ListCommandInvocations input CommandId @length min=36 max=36, optional InstanceId/Filters/MaxResults/NextToken/Details; output CommandInvocations (CommandInvocationList) + NextToken; modeled errors InvalidCommandId, InvalidFilterKey, InvalidInstanceId, InvalidNextToken (all httpError 400). Recorded behavior: a well-formed request — including one whose CommandId is a syntactically valid 36-character id that names no command in the account — is answered HTTP 200 with CommandInvocations serialized as an empty list, not with an error. The same holds for the filter and pagination variants.
Emulator: every ListCommandInvocations step answers HTTP 400 and the response carries no CommandInvocations member at all. Six recorded steps across three fixtures are affected, i.e. the operation never succeeds.
Why: a client that lists invocations to poll Run Command progress, or that treats "no invocations yet" as an empty page, sees a hard client error instead. Paginating clients abort, and SDK retry logic does not recover a 400.
Fix: the ListCommandInvocations handler and its command/invocation lookup — an unresolved or not-yet-populated command id must project an empty CommandInvocations list under a 200, and only the modeled error conditions may produce a 400.
Traces: ssm-list-command-invocations-filters-and-pagination#task#1 (also task#2, task#3), ssm-list-command-invocations-not-found-and-empty#task#1, ssm-list-command-invocations-unknown-command-id#task#1 and #task#2.

=== Cause 2 of 7: SendCommand's "no InstanceIds and no Targets" refusal is emulator prose ===
AWS: Smithy: SendCommand.InstanceIds @length 0..50 and SendCommand.Targets @length 0..5 are both optional in the model, so the "one of them is required" rule is service-side; recorded behavior only for the message. The recorded response is HTTP 400 whose message is the fixed sentence pair "You must specify instance IDs or a target value. Targets can include tag-key names, tag key-pair values, or resource group names." — this message is entirely fixed text, with no value interpolated from the request or from state.
Emulator: HTTP 400 with message "Either InstanceIds or Targets must be specified".
Why: the status matches but the wording does not. Callers and test suites that assert on AWS's wording (or surface it to an operator) see emulator-authored prose, and the operator loses AWS's hint about which target forms are accepted.
Fix: the SendCommand validation step that enforces the InstanceIds/Targets precondition — emit AWS's fixed message verbatim.
Traces: ssm-vscan-traj-send-command-validation-order#task#5.

=== Cause 3 of 7: GetCommandInvocation reports the two instanceId validation errors in the wrong order ===
AWS: Smithy: GetCommandInvocation.InstanceId is required, target InstanceId @pattern (^i-(\w{8}|\w{17})$)|(^mi-\w{17}$) with a minimum length of 10. When a value violates both constraints AWS reports both in one ValidationException, 400, with the message template "<N> validation errors detected: Value '<value>' at '<member>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>; Value '<value>' at '<member>' failed to satisfy constraint: Member must have length greater than or equal to <min>". Fixed text: "validation errors detected:", "Value", "at", "failed to satisfy constraint: Member must satisfy regular expression pattern:", "failed to satisfy constraint: Member must have length greater than or equal to", and the "; " separator. Variable parts: the count N, the rejected value, the member name, the pattern and the length bound — all taken from the request and the model. The pattern constraint is reported BEFORE the length constraint.
Emulator: the same two constraint failures with the same count and the same per-constraint wording, but emitted in the reverse order — length first, then pattern.
Why: the message is a requirement, and its clause order is part of it. A client that string-matches the first reported constraint, or that diffs the whole message, mis-reports which rule the input broke.
Fix: the shared input-validation error collector for this operation — order the detected constraint violations the way AWS orders them (pattern before length for the same member).
Traces: ssm-vscan-traj-send-command-validation-order#task#10.

=== Cause 4 of 7: TerminateSession accepts a session that is not in a terminable state ===
AWS: Smithy: TerminateSession.SessionId required, @length 1..96; output SessionId. The model declares only InternalServerError, so the state precondition is recorded behavior only: terminating a session that is not in a valid state for termination is refused with HTTP 400 and body {"__type": "ValidationException", "message": "Session is not in a valid state"}. The message is entirely fixed text.
Emulator: HTTP 200 with an EMPTY body — neither the modeled SessionId member nor any error.
Why: the call looks like it succeeded, so a client believes a session was torn down that was never in a terminable state; and because the 200 body is empty it also loses the modeled SessionId echo, so a caller cannot even confirm which session it acted on.
Fix: the TerminateSession handler's session-state precondition plus its success serializer (SessionId must be projected on the 200 path).
Traces: ssm-session-manager-connection-checks#task#4.

=== Cause 5 of 7: StartSession misclassifies a malformed Target as TargetNotConnected instead of validating it ===
AWS: Smithy: StartSession.Target is required, target SessionTarget @length 1..400; modeled errors InvalidDocument and TargetNotConnected. Recorded behavior: a Target whose FORM is invalid is rejected by input validation before any connectivity check, with HTTP 400 and __type ValidationException, message "1 validation error detected: Value at 'target' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-zA-Z0-9_:-]{1,400}$" (the pattern itself is recorded, not modeled). Fixed text: "1 validation error detected: Value at '", "' failed to satisfy constraint: Member must satisfy regular expression pattern: "; variable parts: the member name and the pattern. TargetNotConnected is for a WELL-FORMED target that is not connected.
Emulator: HTTP 400 with __type TargetNotConnected and NO message member at all.
Why: the two conditions mean opposite things to a client — ValidationException says "fix your input", TargetNotConnected says "the node is not reachable, retry or check the agent". A caller retries forever against an input it should have rejected, and gets no message explaining why.
Fix: the StartSession handler's input validation, which must run and classify before the target-connectivity lookup; the error serializer must also carry the message member. (The open issue "Implement ssm StartSession: unimplemented scaffold stub", 20261114T143448Z-ssm-startsession-stub.md, covers standing the operation up at all and does not carry this replay key or this validation-ordering requirement.)
Traces: ssm-sshp-traj-session-manager-ssh-document-and-state-mistakes#task#2.

=== Cause 6 of 7: GetConnectionStatus does not validate the Target ID format ===
AWS: Smithy: GetConnectionStatus.Target is required, target SessionTarget @length 1..400; output Target + Status (ConnectionStatus enum); the model declares only InternalServerError, so the format rule is recorded behavior only: a Target that is not a valid managed-node id form is refused with HTTP 400, __type ValidationException, message "Invalid target ID format" (entirely fixed text).
Emulator: HTTP 200 with a connection status and no __type/message, i.e. it answers a status for an id that cannot name any node.
Why: a client probing reachability gets an authoritative-looking status for a target that does not exist in any valid form, so a typo in an instance id reads as a real connectivity answer rather than a client error.
Fix: the GetConnectionStatus handler's Target validation, ahead of the node lookup.
Traces: ssm-sshp-traj-session-manager-ssh-document-and-state-mistakes#task#6.

=== Cause 7 of 7: CreateCloudConnector's regex validation message strips the pattern's anchors ===
AWS: Smithy: CreateCloudConnector.ConfigConnectorArn is required, @length 1..512, @pattern ^arn:aws(-cn|-us-gov)?:config:([^:]+):\d{12}:connector/.+$ . On violation AWS answers HTTP 400 with message "1 validation error detected: Value '<value>' at '<member>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>", where <pattern> is the MODELED pattern reproduced verbatim — including its leading ^ and trailing $ anchors. Fixed text is the surrounding sentence; variable parts are the rejected value, the member name and the pattern string taken from the model.
Emulator: the same status and the same sentence, but the echoed pattern has had its ^ and $ anchors removed ("...pattern: arn:aws(-cn|-us-gov)?:config:([^:]+):\d{12}:connector/.+").
Why: the message is a requirement. A client or operator that copies the reported pattern to repair its input gets an UNANCHORED regex, which accepts strings AWS will still reject (anything merely containing a connector ARN), so the reported rule is wrong, not just differently worded.
Fix: the shared constraint-violation message formatter — echo the modeled @pattern string unmodified rather than stripping anchors. (The open issue 20261113T190530Z-ssm-6-net-new-causes-cloudconnector-delete-validate.md covers CreateCloudConnector accepting roles AWS refuses; it does not cover this message defect and does not carry this replay key.)
Traces: ssm-cloudconnector-create-invalid-input#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 850 steps, score 62.4%). Seven net-new ssm clusters, none carrying stub_path or owned_by:

1) ListCommandInvocations — ssm-list-command-invocations-filters-and-pagination#task#1 (6 steps in cluster): expected status 200, got 400; $.CommandInvocations: missing (expected [], got None).
2) SendCommand — ssm-vscan-traj-send-command-validation-order#task#5: 400->400, $.message value_mismatch: expected 'You must specify instance IDs or a target value. Targets can include tag-key names, tag key-pair values, or resource group names.', got 'Either InstanceIds or Targets must be specified'.
3) GetCommandInvocation — ssm-vscan-traj-send-command-validation-order#task#10: 400->400, $.message value_mismatch: expected "2 validation errors detected: Value 'i-zzz' at 'instanceId' failed to satisfy constraint: Member must satisfy regular expression pattern: (^i-(\w{8}|\w{17})$)|(^mi-\w{17}$); Value 'i-zzz' at 'instanceId' failed to satisfy constraint: Member must have length greater than or equal to 10", got the same two clauses in reverse order (length first, pattern second).
4) TerminateSession — ssm-session-manager-connection-checks#task#4: expected status 400, got 200; $: empty_body (expected {'__type': 'ValidationException', 'message': 'Session is not in a valid state'}, got {}).
5) StartSession — ssm-sshp-traj-session-manager-ssh-document-and-state-mistakes#task#2: 400->400, $.__type value_mismatch (expected 'ValidationException', got 'TargetNotConnected'); $.message missing (expected "1 validation error detected: Value at 'target' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[a-zA-Z0-9_:-]{1,400}$", got None).
6) GetConnectionStatus — ssm-sshp-traj-session-manager-ssh-document-and-state-mistakes#task#6: expected status 400, got 200; $.__type missing (expected 'ValidationException'); $.message missing (expected 'Invalid target ID format').
7) CreateCloudConnector — ssm-cloudconnector-create-invalid-input#task#1: 400->400, $.message value_mismatch: expected "...Member must satisfy regular expression pattern: ^arn:aws(-cn|-us-gov)?:config:([^:]+):\d{12}:connector/.+$", got the same text with the ^ and $ anchors absent.

Model evidence (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json): ListCommandInvocations output CommandInvocations:CommandInvocationList, errors InvalidCommandId/InvalidFilterKey/InvalidInstanceId/InvalidNextToken; GetCommandInvocation.InstanceId required @pattern (^i-(\w{8}|\w{17})$)|(^mi-\w{17}$) min length 10; StartSession errors InvalidDocument/TargetNotConnected, Target @length 1..400; TerminateSession output SessionId @length 1..96; GetConnectionStatus output Target+Status; CreateCloudConnector.ConfigConnectorArn required @pattern ^arn:aws(-cn|-us-gov)?:config:([^:]+):\d{12}:connector/.+$.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: replay ssm-list-command-invocations-filters-and-pagination step task#1 (and ssm-list-command-invocations-not-found-and-empty#task#1, ssm-list-command-invocations-unknown-command-id#task#1/#task#2) — ListCommandInvocations answers 400 instead of 200 with CommandInvocations: [].
4. Cause 2: replay ssm-vscan-traj-send-command-validation-order step task#5 — SendCommand with neither InstanceIds nor Targets; compare $.message.
5. Cause 3: replay ssm-vscan-traj-send-command-validation-order step task#10 — GetCommandInvocation with InstanceId 'i-zzz'; compare the order of the two constraint clauses in $.message.
6. Cause 4: replay ssm-session-manager-connection-checks step task#4 — TerminateSession on a session not in a terminable state; expect 400 ValidationException 'Session is not in a valid state'.
7. Cause 5: replay ssm-sshp-traj-session-manager-ssh-document-and-state-mistakes step task#2 — StartSession with a malformed Target; expect 400 ValidationException with the target pattern message, not TargetNotConnected.
8. Cause 6: replay the same fixture's step task#6 — GetConnectionStatus with a malformed Target; expect 400 ValidationException 'Invalid target ID format'.
9. Cause 7: replay ssm-cloudconnector-create-invalid-input step task#1 — CreateCloudConnector with ConfigConnectorArn 'not-a-valid-config-connector-arn'; compare the echoed pattern in $.message for its ^ and $ anchors.
