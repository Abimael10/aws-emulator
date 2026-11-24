Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore.invoke-harness-managed-orchestration-result
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore.invoke-harness-managed-orchestration-result|run#16|AssertionError|assert expected in reply_text.replace(",", ""), (
Issue-Commit: 95ac6b97296b25f638775ef6173e027be717227c

# bedrock-agentcore: InvokeHarness streams a fixed "no language model runs" notice instead of the harness turn's answer

> _Note: this report is cause 1 of the two-cause bundle
> `20261122T214319Z-bedrock-agentcore-2-net-new-causes-invokeharness-streams.md`, filed on its
> own because that bundle's OTHER cause (the data-plane authorizer never publishing
> `bedrock-agentcore:namespace`) is fixed and closed while this one is not. Nothing of this
> cause has been fixed; the text below is the bundle's own account of it._

## Operation
InvokeHarness

## Description
AWS: Smithy models InvokeHarness as POST /harnesses/invoke with required `harnessArn` (query), `runtimeSessionId` (header X-Amzn-Bedrock-AgentCore-Runtime-Session-Id) and `messages`, optional `model`/`systemPrompt`/`tools`/`skills`/`allowedTools`/`maxIterations`/`maxTokens`, and an output that is the required streaming `stream` member of InvokeHarnessStreamOutput (source: smithy_query query="operation" operation="InvokeHarness" on com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json). The model says nothing about the CONTENT of the assistant text, so for that this is recorded behavior only: a harness configured with a Bedrock model (the recording used the us.amazon.nova-micro-v1:0 cross-region inference profile) runs a server-managed agent loop and the streamed assistant text ANSWERS the prompt — asked to add two operands chosen at request time and reply with only the digits, the streamed text contains the correct sum. The call is HTTP 200 with no stream-level error event in both AWS and the emulator; the divergence is purely the text the emulator serves.

Emulator: the stream is well-formed (messageStart/contentBlockDelta/contentBlockStop/messageStop/metadata, role `assistant`, counted tokens, measured latency) but the single text block is a fixed notice whose template is `No language model runs in this emulator. <modelId> generated no text for this request. The request carried <n> message(s) and <m> input token(s).` — its only variable parts are the configured model id and the two counts; nothing in it derives from the conversation. No inference, no tool use and no iteration of the agent loop happens, so arithmetic (or any other) content asked for in `messages` is never produced. (The model id, message count and token count in a single run are per-run values, not part of the requirement.)

Why: the whole point of a harness is that the service runs the agent loop and the client consumes the answer. A client that invokes a harness and parses its reply (an answer, a number, a JSON object, a tool result) gets a constant English notice from the emulator, so every agent-orchestration path a client builds on InvokeHarness is untestable against the emulator, and a test that asserts on the content passes only because it asserts nothing.

Fix: the harness invocation's turn composition — the text served would have to be derived from the request's conversation, with the harness's tools/skills/maxIterations applied, rather than from the fixed inference-floor notice; the surrounding frames, counts and stop reason are already correct and need no change. File/line pointers: services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/invoke_harness.rs (prompt + turn composition) and src/state/harness_invocation.rs (`harness_turn`, which delegates to the bedrock-runtime plane's `emulated_completion`).

## What a fixer should know first
The bundle's claim that the emulator "already owns" an in-process generation/tool-execution path for its Bedrock inference surface does not hold against the code: that path IS the fixed notice. Every inference surface of this emulator (Converse, ConverseStream, InvokeModel, InvokeModelWithResponseStream, the agent-runtime planes, this harness) answers from the one generation stage at
services/service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice/src/state/generation.rs, whose whole output is that notice. Closing this cause therefore needs a language model running in-process:

* there is no inference runtime or model-weights crate in Cargo.lock (no candle, llama, onnx, tract or burn entry), and a crate that would supply one needs model weights fetched over the network, which the emulator does not do;
* producing the answer by reading the prompt's English and computing from it is the fabrication the engine contract forbids (vendor/engine/src/lib.rs: "do not pattern-match source text, and do not hand-write an evaluator or engine for a subset of a language"), and would answer every prompt it was not sized for wrongly while reporting success.

So this is a candidate for `mark_wont_fix(platform_blocker)` — a runtime the platform does not provide — unless an in-process model runtime becomes available to the emulator. It must NOT be closed by re-wording the notice, by echoing the caller's own messages, or by computing an answer from the prompt's text.

## Evidence
Replay report (simparity-behavioral-aws-v1), service heading `### bedrock-agentcore`.

[mismatch] bedrock-agentcore.invoke-harness-managed-orchestration-result:
  Expected (AWS): InvokeHarness(harnessArn=<harness>, runtimeSessionId=<id>, messages=[{role:'user', content:[{text:"Compute <a> + <b>. Reply with ONLY the resulting integer digits…"}]}]) streams an assistant turn whose text contains the correct sum of the two run-time operands; no stream-level error event.
  Actual (emulator): HTTP 200 and a well-formed event stream, but the one text block is `No language model runs in this emulator. <modelId> generated no text for this request. The request carried <n> message(s) and <m> input token(s).` Assertion: `AssertionError: expected the server-computed sum '<sum>' to appear in the model's reply, got 'No language model runs in this emulator. …'`. The operand values, the sum, the model id and the token/message counts are per-run values; the requirement is that the served text answers the conversation rather than announcing that nothing ran.

## Steps to Reproduce
1. Boot the emulator. Create an IAM execution role, CreateHarness configured with a Bedrock model (e.g. us.amazon.nova-micro-v1:0), wait for the harness to be READY.
2. InvokeHarness(harnessArn=<arn>, runtimeSessionId=<33-100 char id>, messages=[{"role":"user","content":[{"text":"Compute 1234 + 5678. Reply with ONLY the resulting integer digits, no words."}]}]).
3. Drain the event stream: the contentBlockDelta text is the fixed `No language model runs in this emulator. <modelId> generated no text for this request. The request carried N message(s) and M input token(s).` notice instead of a reply containing 6912.
