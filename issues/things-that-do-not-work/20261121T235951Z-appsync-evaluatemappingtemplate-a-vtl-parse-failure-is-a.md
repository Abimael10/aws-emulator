Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.evaluate-mapping-template-invalid-vtl-rejected appsync.evaluate-mapping-template-syntax-error-fails appsync.op-contract-evaluateMappingTemplate appsync.evaluate-mapping-template-foreach-iteration-limit
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|EvaluateMappingTemplate|BadRequestException
Issue-Commit: a9288303feb8dead80b5d9b52ed92320ffa70966

# AppSync EvaluateMappingTemplate: a VTL parse failure is a 400 refusal instead of a 200 error envelope, and #foreach over a range is rejected outright (2 causes)

> _Note: this issue was identified against the code base at commit `a9288303feb8dead80b5d9b52ed92320ffa70966`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EvaluateMappingTemplate

## Description
Two net-new root causes in EvaluateMappingTemplate, both in the VTL evaluator the handler delegates to. Neither is covered by the open issue 20261121T212257Z-appsync-evaluatemappingtemplate-vtl-evaluator-and-response-envelope-diverge.md: its cause 3 is about a template that deliberately RAISES ($util.error/$util.appendError) being refused 400, and its cause 4 is about a #foreach body silently rendering empty. What the behavioral replay shows now is different — a template that fails PARSING is refused 400 with emulator-authored prose, and a #foreach whose iterable is a range expression is refused as 'not an expression' before any body could run. The older reports 20261115T090000Z-appsync-evaluatemappingtemplate-renders-no-velocity-template.md and 20261029T174359Z-appsync-evaluatemappingtemplate-stub.md describe a 501 stub and no longer describe this operation.

Shared Smithy basis for both causes: Smithy (appsync) EvaluateMappingTemplate — POST /v1/dataplane-evaluatetemplate, HTTP success code 200; required input `template` (Template, @length 2..65536, @pattern ^[\s\S]*$) and `context` (Context, @length 2..28000); output members `evaluationResult` (@length 0..65536), `error` (ErrorDetail{message}), `logs` (list<String>), `stash`, `outErrors`; modeled errors AccessDeniedException|403, BadRequestException|400, InternalFailureException|500. The documentation describes the template language as Apache Velocity (VTL). Because `error` is an OUTPUT member of a 200 response, this operation reports an evaluation failure in the body, not as a transport refusal.

Cause 1 of 2: a template that fails VTL parsing is refused 400 BadRequestException instead of answered 200 with `error.message`.
AWS: per the model above, an unparseable template is a SERVED call: HTTP 200, no `evaluationResult`, and `error.message` carrying the Apache Velocity parse diagnostic — its fixed text names the exception class `ParseErrorException`, and its variable parts (the line/column and the offending token) come from the template the caller submitted. Recorded behavior only for the exact sentence; the requirement here is the status (200), the populated `error.message` naming ParseErrorException, and the absence of `evaluationResult`.
Emulator: HTTP 400 BadRequestException, with no response body members at all (`error`, `evaluationResult`, `logs`, `stash`, `outErrors` are all unavailable because the call never returns a result). The messages are emulator-authored prose, not AWS's: for the unterminated directive `#if($context.arguments.msg` it answers 'This directive is not written as ( … ).', and for an `#if(...)` block with no `#end` it answers 'This template opens a block it never closes with #end.' Neither sentence is AWS's text for this error class, and neither should be an HTTP-level error at all.
Why: a client cannot develop or lint a template against the sandbox — it can no longer distinguish a parse failure (which AWS reports in the body so the editor can show it) from an authorization or input-shape refusal, and it never receives the Velocity diagnostic that says WHERE the template is broken. Anything asserting on `response['error']['message']` sees a raised botocore BadRequestException instead.
Fix: the result mapping of the EvaluateMappingTemplate handler — a parse failure from the VTL evaluator must become a 200 response that populates `error.message` with the Velocity ParseErrorException diagnostic and leaves `evaluationResult` empty; BadRequestException is reserved for input that violates the modeled constraints.
Traces: appsync.evaluate-mapping-template-invalid-vtl-rejected, appsync.evaluate-mapping-template-syntax-error-fails, appsync.op-contract-evaluateMappingTemplate.

Cause 2 of 2: `#foreach` over a range expression `[a..b]` is refused as not an expression, so a range loop can never be evaluated and the per-evaluation iteration ceiling is unreachable.
AWS: recorded behavior only. A template that loops over a literal range and accumulates the loop counter — e.g. `#set($n=0)#foreach($i in [1..N])#set($n=$velocityCount)#end$util.toJson($n)` — is evaluated and answered 200 with `evaluationResult` equal to the accumulated value (N at the ceiling) and no `error`. One iteration PAST AppSync's per-evaluation processing limit is reported through the output `error` member: HTTP 200 with `error.message` exactly the fixed text 'A processing limit has been exceeded' (the sentence has no variable parts) and no usable `evaluationResult`; a template back at the ceiling renders again. The ceiling is a service limit, not a value of any one recording.
Emulator: HTTP 400 BadRequestException 'This is not an expression.' already for the AT-limit template — the evaluator does not accept a range expression as a `#foreach` iterable, so the loop body never runs, `$velocityCount` is never bound, and the over-limit template fails identically. There is therefore no processing-limit behaviour at all, and the message is emulator-authored.
Why: range loops are ordinary VTL and appear in AppSync's own template examples; a client validating such a template in the sandbox is told its correct template is malformed, and a client probing the processing limit can never observe AWS's 'A processing limit has been exceeded' report, so retry/split logic around large templates cannot be exercised.
Fix: the `#foreach` iterable evaluation in the VTL engine the handler delegates to — accept a range expression `[a..b]` as an iterable, bind `$velocityCount`/`$foreach` inside the body, and enforce the per-evaluation iteration ceiling by reporting it through the output `error` member rather than raising BadRequestException.
Traces: appsync.evaluate-mapping-template-foreach-iteration-limit.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout (354/455 passed). All three checks below are listed under service heading `### appsync` and are neither marked ALREADY FILED nor OWNED.

Cause 1 — appsync.evaluate-mapping-template-invalid-vtl-rejected [error]:
  botocore.errorfactory.BadRequestException: An error occurred (BadRequestException) when calling the EvaluateMappingTemplate operation: This directive is not written as ( … ).
  Template sent: "#if($context.arguments.msg" (unterminated #if), context {"arguments": {"msg": "well-formed"}}.
  Expected (AWS): HTTP 200, no evaluationResult, error.message carrying the Velocity parse diagnostic.

Cause 1 — appsync.evaluate-mapping-template-syntax-error-fails [error]:
  botocore.errorfactory.BadRequestException: ... EvaluateMappingTemplate operation: This directive is not written as ( … ).
  Same unterminated-#if template; the check asserts on resp["error"]["message"] of a 200, not on an exception.

Cause 1 — appsync.op-contract-evaluateMappingTemplate [error]:
  botocore.errorfactory.BadRequestException: ... EvaluateMappingTemplate operation: This template opens a block it never closes with #end.
  Template sent: "#if($context.arguments.id == '1')\n$util.toJson($context.arguments)" (missing #end).
  Expected (AWS): HTTP 200 whose error.message contains 'ParseErrorException' and whose evaluationResult is empty.

Cause 2 — appsync.evaluate-mapping-template-foreach-iteration-limit [error]:
  botocore.errorfactory.BadRequestException: ... EvaluateMappingTemplate operation: This is not an expression.
  Raised on the FIRST call, the at-limit template "#set($n=0)#foreach($i in [1..1000])#set($n=$velocityCount)#end$util.toJson($n)" with context {"arguments": {}}.
  Expected (AWS): 200 with evaluationResult 1000 and no error; one more iteration -> 200 with error.message 'A processing limit has been exceeded' and no evaluationResult.

Smithy cited: appsync EvaluateMappingTemplate (POST /v1/dataplane-evaluatetemplate, code 200; template @length 2..65536 @required; context @length 2..28000 @required; output evaluationResult/error/logs/stash/outErrors; errors AccessDeniedException|403, BadRequestException|400, InternalFailureException|500).

## Steps to Reproduce
Against the emulator, as a principal allowed appsync:EvaluateMappingTemplate (aws appsync evaluate-mapping-template --template ... --context ...):
1. Cause 1a: template '#if($context.arguments.msg' (unterminated directive), context '{"arguments":{"msg":"well-formed"}}'. Expected HTTP 200 with error.message carrying the Velocity ParseErrorException diagnostic and no evaluationResult; observe HTTP 400 BadRequestException 'This directive is not written as ( … ).'
2. Cause 1b: template '#if($context.arguments.id == \'1\')\n$util.toJson($context.arguments)' (no #end), any valid context. Expected HTTP 200 whose error.message names ParseErrorException; observe HTTP 400 BadRequestException 'This template opens a block it never closes with #end.'
3. Control for cause 1: '$util.toJson($context.arguments.msg)' on the same context renders 200 — so only the parse path diverges.
4. Cause 2: template '#set($n=0)#foreach($i in [1..1000])#set($n=$velocityCount)#end$util.toJson($n)', context '{"arguments":{}}'. Expected HTTP 200 with evaluationResult 1000; observe HTTP 400 BadRequestException 'This is not an expression.'
5. Cause 2 (ceiling): the same template with one more iteration than the limit. Expected HTTP 200 with error.message exactly 'A processing limit has been exceeded' and no usable evaluationResult; observe the same 400 as step 4, so the ceiling is never reached.
Re-replay appsync.evaluate-mapping-template-invalid-vtl-rejected, appsync.evaluate-mapping-template-syntax-error-fails, appsync.op-contract-evaluateMappingTemplate and appsync.evaluate-mapping-template-foreach-iteration-limit to confirm both causes.
