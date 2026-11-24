Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-eval-vtl-dynamodb-json
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-eval-vtl-dynamodb-json appsync-eval-vtl-errors-logs appsync-unit-resolver-code-evaluation appsync-eval-vtl-foreach-computation
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|EvaluateMappingTemplate|2xx->4xx|$.evaluationResult
Issue-Key: appsync|EvaluateMappingTemplate|2xx->4xx|$.logs
Issue-Key: appsync|EvaluateMappingTemplate|2xx->4xx|$.outErrors
Issue-Key: appsync|EvaluateMappingTemplate|2xx->4xx|$.stash
Issue-Key: appsync|EvaluateMappingTemplate|2xx->4xx|status
Issue-Key: appsync|EvaluateMappingTemplate|2xx->2xx|$.error
Issue-Key: appsync|EvaluateMappingTemplate|2xx->2xx|$.evaluationResult
Issue-Key: appsync|EvaluateMappingTemplate|2xx->2xx|$.logs
Issue-Key: appsync|EvaluateMappingTemplate|2xx->4xx|$.error
Issue-Key: appsync|EvaluateMappingTemplate|2xx->2xx|$.outErrors
Issue-Key: appsync|EvaluateMappingTemplate|2xx->2xx|$.stash
Issue-Key: appsync|EvaluateMappingTemplate|2xx->2xx|$.error.message
Issue-Commit: a45eebcdd8d9a9d0a1f0906c2cbe25c9ebaa13ba

# AppSync EvaluateMappingTemplate: VTL evaluator and response envelope diverge (7 causes)

> _Note: this issue was identified against the code base at commit `a45eebcdd8d9a9d0a1f0906c2cbe25c9ebaa13ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EvaluateMappingTemplate

## Description
EvaluateMappingTemplate is no longer a 501 scaffold (`src/ops/evaluate_mapping_template.rs` now delegates to `crate::service::graphql_api::mapping_template`), so the open reports `20261115T090000Z-appsync-evaluatemappingtemplate-renders-no-velocity-template.md` and `20261029T174359Z-appsync-evaluatemappingtemplate-stub.md` no longer describe what it does. What the replay now shows is a VTL engine that renders a subset of the language and a response envelope that is incomplete. Seven net-new causes, all in this one operation; none is owned by an open issue.

Shared Smithy basis for every cause below: Smithy (appsync) EvaluateMappingTemplate — POST /v1/dataplane-evaluatetemplate, HTTP success code 200; required input `template` (Template, @length 2..65536) and `context` (Context, @length 2..28000); output `evaluationResult` (@length 0..65536), `error` (ErrorDetail{message}), `logs` (list<String>), `stash` (String), `outErrors` (String); modeled errors AccessDeniedException|403, BadRequestException|400, InternalFailureException|500. The documentation states mapping templates are written in the Apache Velocity Template Language (VTL); AWS evaluates whatever template and context the caller sends.

Cause 1 of 7: the $util.dynamodb helper family is missing, so a DynamoDB mapping template is refused instead of rendered. Fixture `appsync-eval-vtl-dynamodb-json`, step `task#1` (cluster also covers `appsync-eval-vtl-dynamodb-json#task#2` and `appsync-eval-vtl-errors-logs#task#1`).
AWS: recorded behavior only for the specific helper set (the Smithy model describes the operation, not the $util library). The recording shows status 200 with `evaluationResult` carrying the DynamoDB attribute-value encoding of every scalar and collection the template converts — a number as {"N":<number>}, a string as {"S":"<string>"}, a boolean as {"BOOL":<bool>}, a list as {"L":[...]}, a map as {"M":{...}}, null as {"NULL":null}, a string set as {"SS":[...]}, a number set as {"NS":[...]} — plus `logs` [] and `stash`/`outErrors` present.
Emulator: status 400 (BadRequestException), with no `evaluationResult`, `logs`, `stash` or `outErrors` at all — the engine cannot evaluate the template because it has no $util.dynamodb.toDynamoDB / toDynamoDBJson / toMapValues(Json) / toStringSet / toNumberSet bindings, so an unevaluable-template refusal is returned for a template AWS renders.
Why: a client using the sandbox to develop or lint any DynamoDB resolver template gets a hard 400 for a correct template and concludes its template is malformed; the same missing helpers are what the GraphQL data plane needs, so no DynamoDB VTL resolver can be developed against the emulator.
Fix: the $util helper library of the VTL evaluator (`crate::service::graphql_api::mapping_template`) — add the dynamodb helper namespace; the operation handler must only refuse BadRequestException for a template that genuinely cannot be parsed.
Traces: appsync-eval-vtl-dynamodb-json#task#1, appsync-eval-vtl-dynamodb-json#task#2, appsync-eval-vtl-errors-logs#task#1.

Cause 2 of 7: an invalid mock context is accepted and rendered instead of being reported as a context-validation error. Fixture `appsync-eval-vtl-dynamodb-json`, step `task#3`.
AWS: recorded behavior only. AWS validates the supplied mock `context` against the shape of a resolver context and answers 200 with NO `evaluationResult` and an `error.message` whose fixed text is "The following properties in your mock context object failed validation: " followed by the dotted path(s) of the offending properties taken from the request's own context document, ending in a period (the recording's path value is `ctx.arguments.` — the path is a value from the request, the sentence is the fixed text).
Emulator: status 200 with `evaluationResult` rendered from the invalid context and no `error` member at all, so the malformed context is silently accepted.
Why: a client that mistypes its mock context gets a rendered result it believes validates its template, and never learns the context it will see at runtime differs; the error member AWS uses to report this is never populated.
Fix: input validation of the `context` document in the EvaluateMappingTemplate handler, before evaluation, reporting through the output `error` member rather than an exception.
Traces: appsync-eval-vtl-dynamodb-json#task#3.

Cause 3 of 7: a template that RAISES an error is refused 400 instead of being a served 200 carrying `error` and `outErrors`. Fixture `appsync-eval-vtl-errors-logs`, step `task#2` (cluster also covers `appsync-unit-resolver-code-evaluation#task#6`).
AWS: recorded behavior only, consistent with the modeled output: `error` and `outErrors` exist as OUTPUT members, so a raised template error is a successful call that reports it. The recording shows status 200, `error.message` equal to the message the template passed to the raise helper (the message is a value from the request's template), `stash` present, and `outErrors` a JSON array of the errors the template appended, each with the `message`, `type`, `data` and `info` the template supplied (null where not supplied).
Emulator: status 400, with no `error`, `stash` or `outErrors` — a raise inside the template is turned into a transport-level refusal.
Why: the sandbox cannot be used to exercise a template's error path at all: a client probing $util.error / $util.appendError sees BadRequestException, cannot read the error message or the appended error list, and cannot distinguish "my template is syntactically broken" (a real 400) from "my template deliberately raised" (a 200).
Fix: the EvaluateMappingTemplate handler's result mapping — a raise from the evaluator must become a 200 response populating `error`/`outErrors`, not a BadRequestException.
Traces: appsync-eval-vtl-errors-logs#task#2, appsync-unit-resolver-code-evaluation#task#6.

Cause 4 of 7: #foreach iteration and #set arithmetic are not evaluated, so computed output collapses to empty/zero and the stash is dropped. Fixture `appsync-eval-vtl-foreach-computation`, step `task#1` (cluster also covers `appsync-eval-vtl-foreach-computation#task#2`).
AWS: recorded behavior only. The recording shows status 200 whose `evaluationResult` contains the list built by iterating the context's collection (one entry per element, each formed from that element's own fields) and the numeric totals accumulated by the arithmetic in the directives, plus the whitespace the unrendered directive lines leave behind; `stash` reflects the keys the template put into it (here a count of the iterated lines), and `outErrors` is an empty JSON array.
Emulator: status 200 but the iterated list renders EMPTY and every accumulated number renders as 0 — the #foreach body never executes and #set arithmetic never accumulates — while only the literals pass through unchanged; `stash` and `outErrors` are absent from the response.
Why: a template whose whole purpose is to build a projection over a list silently renders a well-formed but empty/zeroed document. This is worse than a refusal: the client gets a 200 and a plausible-looking result, so a resolver validated in the sandbox ships with its loop body dead.
Fix: the directive evaluation of the VTL engine (`crate::service::graphql_api::mapping_template`) — #foreach/#set/reference-arithmetic — and the handler's propagation of the evaluator's stash.
Traces: appsync-eval-vtl-foreach-computation#task#1, appsync-eval-vtl-foreach-computation#task#2.

Cause 5 of 7: an early return directive is ignored, so the template keeps running and the wrong document is returned. Fixture `appsync-eval-vtl-foreach-computation`, step `task#3`.
AWS: recorded behavior only. The recording shows status 200 whose `evaluationResult` is the document produced AT the early-return point (the values the template had set when it returned), and nothing from the template text after it.
Emulator: status 200 whose `evaluationResult` is the document produced by the text AFTER the early return — the return is a no-op and evaluation continues to the end of the template; `logs` is serialized as an empty list where AWS omits it, and `stash`/`outErrors` are absent.
Why: a conditional short-circuit (a cache hit, an authorization shortcut, an empty-input guard) is inverted: the branch the template chose is discarded and the branch it refused to take is what the client gets, with a 200 and no indication anything was skipped.
Fix: the directive evaluation of the VTL engine — #return must terminate evaluation and fix the rendered output at that point.
Traces: appsync-eval-vtl-foreach-computation#task#3.

Cause 6 of 7: the unauthorized helper reports the data plane's field-level sentence instead of the sandbox's. Fixture `appsync-eval-vtl-foreach-computation`, step `task#4`.
AWS: recorded behavior only. $util.unauthorized() inside an EvaluateMappingTemplate call is a served 200 whose `error.message` is exactly the fixed text "Unauthorized" — there is no field or type to name, because the sandbox evaluates a template outside any GraphQL field resolution.
Emulator: status 200 but `error.message` is "Not Authorized to access  on type " — the data plane's field-authorization template, whose two variable parts (the field name and the parent type name) are empty in the sandbox, leaving a sentence with two gaps. The fixed text is wrong for this error class and the variables have no values to fill.
Why: a client asserting on the denial message (or merely logging it) sees a truncated, grammatically broken sentence instead of AWS's word, and an automated check for the documented denial never matches. Note the inverse defect on the data plane is already filed as `20261118T202853Z-appsync-2-net-new-causes-util-unauthorized-denial.md`, which asks for the "Not Authorized to access <field> on type <Type>" wording THERE; that issue does not cover the sandbox, and the fix must not make both surfaces share one message.
Fix: the $util.unauthorized binding of the VTL engine when it is driven by the EvaluateMappingTemplate handler — the sandbox's denial message is the fixed text "Unauthorized".
Traces: appsync-eval-vtl-foreach-computation#task#4.

Cause 7 of 7: the response envelope omits `stash` and `outErrors` even on a fully successful evaluation. Fixture `appsync-unit-resolver-code-evaluation`, step `task#7`.
AWS: Smithy EvaluateMappingTemplateResponse models `stash` (Stash, String) and `outErrors` (OutErrors, String) alongside `evaluationResult`/`error`/`logs`. The recording shows both present on a successful evaluation — `stash` the JSON object of what the template left in $ctx.stash (an empty JSON object when the template stashed nothing) and `outErrors` the JSON array of appended errors (an empty JSON array when none were appended). Correspondingly `logs` is OMITTED when the template logged nothing, not serialized as an empty list.
Emulator: `stash` and `outErrors` are never serialized at all, and `logs` is emitted as [] where AWS omits the member — so the envelope is wrong on every call, including the ones that otherwise pass (the same two members are missing in causes 1, 3, 4, 5 and 6 above).
Why: a client that reads $ctx.stash out of a sandbox evaluation — the normal way to develop a pipeline resolver, where each function's stash is the next one's input — gets no member to read and cannot tell an empty stash from an unimplemented one; the same holds for the appended-error list.
Fix: the response serialization in the EvaluateMappingTemplate handler — populate `stash` and `outErrors` from the evaluator's run state and omit `logs` when empty.
Traces: appsync-unit-resolver-code-evaluation#task#7.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (92 fixtures, 743 steps, 74 mismatched). All seven clusters below are appsync, verdict `mismatch`, with empty `stub_path` and empty `owned_by`.

Cause 1 — appsync-eval-vtl-dynamodb-json#task#1 (EvaluateMappingTemplate): expected status 200, actual 400. Expected `$.evaluationResult` = '{"n": {"N":3}, "f": {"N":9.5}, "s": {"S":"Widget"}, "b": {"BOOL":true}, "l": {"L":[{"S":"a"},{"S":"b"}]}, "m": {"M":{"k":{"S":"v"}}}, "z": {"NULL":null}, "ss": {"SS":["a","b"]}, "ns": {"NS":[1,2.5,3]}}', got None; `$.logs` expected [], got None; `$.stash` expected '{}', got None; `$.outErrors` expected '[]', got None. Also affected: appsync-eval-vtl-dynamodb-json#task#2, appsync-eval-vtl-errors-logs#task#1.

Cause 2 — appsync-eval-vtl-dynamodb-json#task#3: status 200 both sides. `$.error` expected {'message': 'The following properties in your mock context object failed validation: ctx.arguments.'}, got None; `$.evaluationResult` expected None, got '{"key": {"NULL":null}}'; `$.logs` expected None, got [].

Cause 3 — appsync-eval-vtl-errors-logs#task#2: expected status 200, actual 400. `$.error` expected {'message': 'qty must be positive'}, got None; `$.stash` expected '{}', got None; `$.outErrors` expected '[{"message":"partial failure","type":"PartialError","data":{"id":"x1"},"info":null}]', got None. Also affected: appsync-unit-resolver-code-evaluation#task#6.

Cause 4 — appsync-eval-vtl-foreach-computation#task#1: status 200 both sides. `$.evaluationResult` expected '      \n          \n  \n{"skus": ["A1x2","C3x5"], "total": 26, "net": 16, "isOwner": true}', got '  \n{"skus": [], "total": 0, "net": 0, "isOwner": true}'; `$.stash` expected '{"lineCount":2}', got None; `$.outErrors` expected '[]', got None. Also affected: appsync-eval-vtl-foreach-computation#task#2.

Cause 5 — appsync-eval-vtl-foreach-computation#task#3: status 200 both sides. `$.evaluationResult` expected '{"early":5,"from":"cache"}', got '{"late": true}'; `$.logs` expected None, got []; `$.stash` expected '{}', got None; `$.outErrors` expected '[]', got None.

Cause 6 — appsync-eval-vtl-foreach-computation#task#4: status 200 both sides. `$.error.message` expected 'Unauthorized', got 'Not Authorized to access  on type '; `$.logs` expected None, got []; `$.stash` expected '{}', got None; `$.outErrors` expected '[]', got None.

Cause 7 — appsync-unit-resolver-code-evaluation#task#7: status 200 both sides. `$.stash` expected '{}', got None; `$.outErrors` expected '[]', got None.

Component: `services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/src/ops/evaluate_mapping_template.rs` (lines 3-10 of its header state it delegates to the single VTL reader) and `crate::service::graphql_api::mapping_template`, which that header names as the one evaluator of this language in the crate — so every cause above is shared with the resolver request/response mapping-template path.

## Steps to Reproduce
Against the emulator, as a principal allowed appsync:EvaluateMappingTemplate, call POST /v1/dataplane-evaluatetemplate (aws appsync evaluate-mapping-template --template ... --context ...), varying the template over the language rather than over one recorded line:
1. A template that converts a context value through the $util.dynamodb helpers (toDynamoDB / toDynamoDBJson / toMapValuesJson, plus a string set and a number set) -> must be 200 with `evaluationResult` carrying the attribute-value encoding; observe 400 BadRequestException (cause 1).
2. The same call with a mock `context` whose properties do not match a resolver context -> must be 200 with no `evaluationResult` and `error.message` 'The following properties in your mock context object failed validation: <dotted path from the submitted context>.'; observe 200 with a rendered `evaluationResult` and no `error` (cause 2).
3. A template that calls $util.error('<message>') and $util.appendError(...) -> must be 200 with `error.message` equal to the message the template passed and `outErrors` the JSON array of appended errors (message/type/data/info); observe 400 (cause 3).
4. A template with a #foreach over a list in the context plus #set arithmetic accumulating totals -> must be 200 whose `evaluationResult` contains one entry per element and the accumulated numbers, with `stash` carrying whatever the template stashed; observe an empty list, zeroed numbers and no `stash` (cause 4).
5. A template that #return-s early from inside a conditional -> must be 200 whose `evaluationResult` is the document as of the return; observe the document produced by the text after it (cause 5).
6. A template that calls $util.unauthorized() -> must be 200 with `error.message` exactly 'Unauthorized'; observe 'Not Authorized to access  on type ' (cause 6).
7. Any template that evaluates cleanly and stashes nothing and logs nothing -> the 200 must carry `stash` as an empty JSON object and `outErrors` as an empty JSON array and must OMIT `logs`; observe `stash`/`outErrors` absent and `logs` serialized as [] (cause 7).
Re-replay fixtures appsync-eval-vtl-dynamodb-json, appsync-eval-vtl-errors-logs, appsync-eval-vtl-foreach-computation and appsync-unit-resolver-code-evaluation to confirm all seven.
