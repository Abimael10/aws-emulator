Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-cfg-resolver-sync-config-rules
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-cfg-resolver-sync-config-rules appsync-type-sdl-json-format-fidelity
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|CreateResolver|4xx->4xx|$.message
Issue-Key: appsync|GetType|2xx->2xx|$.type.definition
Issue-Commit: c9344e2779494e078eb181c7552bbea5ec625bd0

# AppSync control plane: 2 net-new causes — CreateResolver does not validate syncConfig's LambdaConflictHandlerArn, and GetType(format=SDL) drops a blank line from the type definition

> _Note: this issue was identified against the code base at commit `c9344e2779494e078eb181c7552bbea5ec625bd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResolver

## Description
Cause 1 of 2 — CreateResolver accepts a syncConfig whose lambdaConflictHandlerConfig.lambdaConflictHandlerArn is not a Lambda ARN in the calling account. AWS: Smithy (appsync) CreateResolver takes syncConfig: SyncConfig (conflictHandler/conflictDetection plus LambdaConflictHandlerConfig$lambdaConflictHandlerArn) and models BadRequestException (400). When conflictHandler is LAMBDA and the supplied lambdaConflictHandlerArn does not name a Lambda function in the caller's account, the request is refused with 400 BadRequestException and the message 'LambdaConflictHandlerArn is not a valid Lambda Arn in the calling account.' — that message is entirely fixed text, with no request- or state-derived parts. Emulator: it never performs that check; the request is rejected for an unrelated reason, 400 with message 'Only one resolver is allowed per field.', because a resolver for the same type/field is already present (left behind by the earlier create AWS refuses — that earlier accept is already owned by open issue 20261110T162826Z-appsync-createresolver-accepts-resolvers-aws-refuses-cachingkeys-prefix.md). Whatever the resolver state, the LambdaConflictHandlerArn validation is absent: no step in the corpus ever elicits its message. Why: a client configuring LAMBDA conflict resolution gets no signal that its conflict-handler ARN is wrong, so it believes conflict detection is wired up (or mis-diagnoses the refusal as a duplicate-resolver conflict) and only discovers the broken handler when a write conflict occurs. Fix: CreateResolver's (and UpdateResolver's) input validation for syncConfig — validate lambdaConflictHandlerConfig.lambdaConflictHandlerArn against the caller's account before the per-field uniqueness check. Traces: appsync-cfg-resolver-sync-config-rules#task#7 (same cluster: #task#8).

Cause 2 of 2 — GetType with format=SDL does not reproduce the type's definition text. AWS: recorded behavior only — Type$definition for format=SDL is the type's source text exactly as it was submitted with the schema, interior blank lines included; in the recording the interface body is followed by two newlines before the closing brace. Emulator: it serves a re-emitted, whitespace-normalized form that collapses one of those blank lines, so the definition string differs from the submitted source. Why: a client that round-trips a type through GetType (diffing against its own SDL, or re-submitting the retrieved definition) sees a spurious change on every read, so SDL-level change detection reports drift where there is none. Fix: the GetType handler's Type projection / SDL rendering — preserve the stored source span for the type verbatim rather than reformatting it. Related open issues on the neighbouring projections, which this does NOT duplicate (they cover ListTypes/UpdateType, not GetType): 20261112T215152Z-appsync-listtypes-echoes-stored-sdl-text-for-type-definition.md, 20261113T063900Z-appsync-type-definition-in-listtypes-responses-is-not-rendered.md, 20261112T042459Z-appsync-type-projections-diverge-list-responses-serve-the.md. Traces: appsync-type-sdl-json-format-fidelity#task#10 (same cluster: #task#12, #task#14, #task#15).

## Evidence
Replayed trace set simparity-wire-aws-v1:
- Cause 1: fixture appsync-cfg-resolver-sync-config-rules, step task#7 (and task#8), operation CreateResolver, verdict mismatch, status 400 -> 400: $.message expected 'LambdaConflictHandlerArn is not a valid Lambda Arn in the calling account.', got 'Only one resolver is allowed per field.' (stub_path = "", owned_by = []).
- Cause 2: fixture appsync-type-sdl-json-format-fidelity, step task#10 (and task#12, task#14, task#15), operation GetType, verdict mismatch, status 200 -> 200: $.type.definition expected 'interface Node\n\n{\nid: ID!\n\n\n}\n', got 'interface Node\n\n{\nid: ID!\n\n}\n' (stub_path = "", owned_by = []).

## Steps to Reproduce
1. Replay fixture appsync-cfg-resolver-sync-config-rules and inspect step task#7: CreateResolver with syncConfig.conflictHandler = LAMBDA and a lambdaConflictHandlerConfig.lambdaConflictHandlerArn that is not a Lambda function in the calling account must answer 400 BadRequestException 'LambdaConflictHandlerArn is not a valid Lambda Arn in the calling account.'; the emulator answers 400 'Only one resolver is allowed per field.' instead.
2. Replay fixture appsync-type-sdl-json-format-fidelity and inspect step task#10: GetType(apiId, typeName='Node', format=SDL) must return Type$definition as the submitted source text, including the blank line before the closing brace; the emulator returns a form with that blank line removed.
