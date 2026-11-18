Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-prompt-tagging
Issue-Step: task#3
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-prompt-tagging
Issue-Shape-Id: com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda
Issue-Key: bedrock-agent|TagResource|2xx->2xx|status
Issue-Key: bedrock-agent|UntagResource|2xx->2xx|status
Issue-Commit: e9549b1dd16f60dcc5781cad715703321b701533

# bedrock-agent prompt tagging answers HTTP 200 where AWS answers 204 No Content

> _Note: this issue was identified against the code base at commit `e9549b1dd16f60dcc5781cad715703321b701533`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Two net-new causes sharing one fault: the bedrock-agent tagging operations have empty output shapes, and the emulator serializes that empty output as HTTP 200 where AWS answers HTTP 204 No Content. Both operations succeed otherwise - the tags are applied and removed - so the divergence is purely the success status line.

Cause 1 of 2: TagResource returns 200 instead of 204.
  AWS: recorded behavior only for the status code - TagResource's output shape TagResourceResponse has no members (Smithy: POST /tags/{resourceArn}, resourceArn @httpLabel required, tags required; the model's @http code is 200, but the recorded AWS response for a successful tagging of a prompt ARN is HTTP 204 with no body). The recording shows no error code and no message: a successful tag is an empty 204.
  Emulator: the same request answers HTTP 204's semantics with the wrong status line - HTTP 200 (empty body) where 204 is required.
  Why: a client or proxy that branches on the exact success status (204 => no body to parse, 200 => a body is expected) takes the wrong branch; strict assertion harnesses and HTTP caches treating 200 and 204 differently diverge from their behavior against AWS. The tagging effect itself is correct, so the mismatch is only visible at the wire level - which is exactly what a wire-fidelity emulator has to get right.
  Fix: the TagResource handler's response serializer - an empty output must be emitted as 204 No Content for this operation.
  Traces: bedrock-agent-prompt-tagging#task#3 (status: expected 204, got 200).

Cause 2 of 2: UntagResource returns 200 instead of 204.
  AWS: recorded behavior only for the status code - UntagResource's output shape UntagResourceResponse has no members (Smithy: DELETE /tags/{resourceArn}, idempotent, tagKeys @httpQuery required; the model's @http code is 200, but the recorded AWS response for a successful untag of a prompt ARN is HTTP 204 with no body). No error code or message is involved: success is an empty 204.
  Emulator: HTTP 200 with an empty body instead of 204.
  Why: identical to cause 1 - any caller that distinguishes 200 from 204 on the success path behaves differently against the emulator than against AWS.
  Fix: the UntagResource handler's response serializer - emit 204 No Content for the empty output.
  Traces: bedrock-agent-prompt-tagging#task#5 (status: expected 204, got 200).

## Evidence
Replay of trace_set simparity-wire-aws-v1, bedrock-agent clusters:

Cause 1 - fixture bedrock-agent-prompt-tagging step task#3, TagResource: divergence_key service=bedrock-agent|TagResource|status=204->200|keys=status; detail "status: expected 204, got 200". No body keys diverged.

Cause 2 - fixture bedrock-agent-prompt-tagging step task#5, UntagResource: divergence_key service=bedrock-agent|UntagResource|status=204->200|keys=status; detail "status: expected 204, got 200". No body keys diverged.

Model evidence (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda): TagResource POST /tags/{resourceArn}, output TagResourceResponse = {} ; UntagResource DELETE /tags/{resourceArn} idempotent, output UntagResourceResponse = {}. The recorded AWS responses for both are 204 No Content, which is the wire requirement the fixtures assert.

## Steps to Reproduce
1. Replay the wire corpus: replay_wire_corpus() against the emulator built from this checkout.
2. Replay fixture bedrock-agent-prompt-tagging. Step task#3 issues TagResource (POST /tags/{promptArn}) for the prompt created earlier in the fixture: expect HTTP 204 with an empty body, observe HTTP 200.
3. Step task#5 issues UntagResource (DELETE /tags/{promptArn}?tagKeys=...): expect HTTP 204 with an empty body, observe HTTP 200.
4. In both cases the tag state afterwards is correct (ListTagsForResource reflects the change); only the success status line diverges.
