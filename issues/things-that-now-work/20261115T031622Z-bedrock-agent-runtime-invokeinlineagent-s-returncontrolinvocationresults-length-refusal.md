Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agent-runtime.invoke-inline-agent-return-control-result-reenters-the-next-turn-prompt
Issue-Shape-Id: com.amazonaws.bedrockagentruntime#AmazonBedrockAgentRunTimeService
Issue-Key: bedrock-agent-runtime.invoke-inline-agent-return-control-result-reenters-the-next-turn-prompt|run#33|AssertionError|assert over_message == expected_over, (
Issue-Commit: 70ff1500c355ed7fc4788da90f349959a442a9b5

# bedrock-agent-runtime: InvokeInlineAgent's returnControlInvocationResults length refusal omits the submitted value AWS quotes

> _Note: this issue was identified against the code base at commit `70ff1500c355ed7fc4788da90f349959a442a9b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeInlineAgent

## Description
Cause 1 of 1: the ValidationException message for an over-long `inlineSessionState.returnControlInvocationResults` list is re-worded — it drops the rejected value AWS renders into the message.

AWS: Smithy (com.amazonaws.bedrockagentruntime) — `InlineSessionState.returnControlInvocationResults` targets `ReturnControlInvocationResults`, a list with `@length(min: 1, max: 5)` of `InvocationResultMember`; the modeled refusal is `ValidationException` (HTTP 400). AWS's constraint envelope for this member words the refusal as the standard single-error template, quoting the submitted collection:
  `1 validation error detected: Value '<rendered list>' at 'inlineSessionState.returnControlInvocationResults' failed to satisfy constraint: Member must have length less than or equal to 5`
Fixed text: `1 validation error detected: Value '`, `' at '`, `' failed to satisfy constraint: Member must have length less than or equal to `. Variable parts taken from the request and the model: the member path (`inlineSessionState.returnControlInvocationResults`), the modeled maximum (`5`), and `<rendered list>` — the submitted list in the service's Java rendering, i.e. `[` + comma-joined `InvocationResultMember(functionResult=FunctionResult(actionGroup=<actionGroup>, confirmationState=<confirmationState|null>, function=<function>, responseBody={<contentType>=ContentBody(body=<body>, images=<images|null>)}, responseState=<responseState|null>, agentId=<agentId|null>))` members + `]`, every value inside coming from the caller's request. Nothing in that rendering is a per-run constant — the action-group name, function name and response body are whatever the request submitted.

Emulator: the HTTP status (400) and the error code (`ValidationException`) are right, but the message differs: it names the member and states the requirement while quoting NO value for the rejected collection. The emulator's own handler documents this as deliberate (services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/src/ops/invoke_inline_agent.rs lines 39-46: "That bound's refusal names the member and states the requirement and quotes no value, because the constraint envelope that words it (`aws_smithy_schema_server::constraints`, vendored and read-only) renders no value for a COLLECTION ... while AWS additionally quotes the submitted list"). The check asserted the AWS message and failed on the comparison; the surrounding assertions (returnControl shape, the unissued-invocationId ValidationException, and the `ValidationException` code for 6 results) all passed, so this is message text alone.

Why: a client that matches, logs or surfaces the validation message cannot tell WHICH results it submitted were rejected — AWS's message is self-describing and echoes the offending list, so parsers and operators built against AWS lose the only part of the response that identifies the bad input. The divergence is also systematic: every collection-length refusal in this workspace loses its value, so a fidelity test suite that pins AWS wording fails across operations rather than in one place.

Fix: the collection-length branch of the shared constraint-violation serializer that words this refusal (the vendored `aws_smithy_schema_server::constraints` envelope reached from InvokeInlineAgent's input validation), so a list/set length violation renders the submitted collection into the `Value '...'` slot the same way a scalar violation already renders its value. The per-operation handler should not special-case it.

Traces: bedrock-agent-runtime.invoke-inline-agent-return-control-result-reenters-the-next-turn-prompt (H4 leg).

## Evidence
Check `bedrock-agent-runtime.invoke-inline-agent-return-control-result-reenters-the-next-turn-prompt`, verdict mismatch.

Request: InvokeInlineAgent with foundationModel=amazon.titan-text-express-v1, one RETURN_CONTROL action group, and `inlineSessionState={invocationId: <the id the previous turn issued>, returnControlInvocationResults: [<same functionResult> x 6]}` — 6 entries against the modeled maximum of 5.

Expected (AWS): HTTP 400, code `ValidationException`, message
  1 validation error detected: Value '[InvocationResultMember(functionResult=FunctionResult(actionGroup=<actionGroup>, confirmationState=null, function=<function>, responseBody={TEXT=ContentBody(body=<body>, images=null)}, responseState=null, agentId=null)), ... x6]' at 'inlineSessionState.returnControlInvocationResults' failed to satisfy constraint: Member must have length less than or equal to 5

Actual (emulator): HTTP 400 with code `ValidationException` (correct) but a message that omits the `Value '<rendered list>'` portion — the handler's own module doc states the envelope "renders no value for a COLLECTION" and that "AWS additionally quotes the submitted list".

Assertion from the replay report:
  AssertionError: expected message "1 validation error detected: Value '[InvocationResultMember(functionResult=FunctionResult(actionGroup=LaunchCodeLookup, confirmationState=null, function=get_launch_code, responseBody={TEXT=ContentBody(body=The launch code for Falcon is LAUNCHCODE...., images=null)}, responseState=null, agentId=null)), ... ]' at 'inlineSessionState.returnControlInvocationResults' failed to satisfy constraint: Member must have length less than or equal to 5" (the action-group name, function name and body in that rendering are the check's own request values, not requirements).

Corroborating source: services/service-com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice/src/ops/invoke_inline_agent.rs:39-46; pinned by tests/all/collection_length_refusals.rs.

## Steps to Reproduce
1. CreateSession, then call InvokeInlineAgent with foundationModel=amazon.titan-text-express-v1, an instruction, enableTrace=true, and one action group whose actionGroupExecutor is {customControl: RETURN_CONTROL} declaring a single function.
2. Drain the `completion` event stream and capture `returnControl.invocationId` from the terminal returnControl event.
3. Call InvokeInlineAgent again on the same sessionId with inlineSessionState={invocationId: <that id>, returnControlInvocationResults: [ {functionResult: {actionGroup: <same>, function: <same>, responseBody: {TEXT: {body: "<any text>"}}}} ] * 6} — six entries, one over the modeled maximum of 5.
4. Inspect the ClientError: code is ValidationException (correct), but Error.Message carries no `Value '[...]'` rendering of the six submitted InvocationResultMember entries, whereas AWS quotes the whole submitted list before `at 'inlineSessionState.returnControlInvocationResults'`.
