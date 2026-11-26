Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agent-prompt-crud-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agent-prompt-crud-lifecycle bedrock-agent-prompt-error-paths bedrock-agent-prompt-tagging bedrock-agent-prompt-versioning
Issue-Shape-Id: com.amazonaws.bedrockagent#AmazonBedrockAgentBuildTimeLambda
Issue-Key: bedrock-agent|GetPrompt|2xx->2xx|$.description
Issue-Key: bedrock-agent|GetPrompt|4xx->2xx|$.message
Issue-Key: bedrock-agent|GetPrompt|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agent|GetPrompt|4xx->2xx|status
Issue-Key: bedrock-agent|GetPrompt|4xx->2xx|$
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.arn
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.createdAt
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.defaultVariant
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.description
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.id
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.name
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.updatedAt
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.variants
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|$.version
Issue-Key: bedrock-agent|GetPrompt|2xx->4xx|status
Issue-Commit: e9549b1dd16f60dcc5781cad715703321b701533

# bedrock-agent prompt lifecycle state is not applied: 4 net-new causes across UpdatePrompt, DeletePrompt and prompt-version reads

> _Note: this issue was identified against the code base at commit `e9549b1dd16f60dcc5781cad715703321b701533`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPrompt

## Description
Four net-new causes, all in the bedrock-agent Prompt resource plane: a mutation that is accepted but never applied to state, a delete that is accepted but never applied to state, and a versioned read that cannot find a version the prompt should hold. The prompt write path acknowledges mutations without changing the stored prompt, so every later read of the same prompt answers from the pre-mutation record.

Cause 1 of 4: UpdatePrompt is acknowledged but the stored prompt is never mutated, so a following GetPrompt returns the pre-update representation.
  AWS: UpdatePrompt (Smithy: PUT /prompts/{promptIdentifier}/, idempotent, output member `description` -> PromptDescription @length min 1 max 200) replaces the DRAFT prompt's mutable members. A subsequent GetPrompt (Smithy: GET /prompts/{promptIdentifier}/, readonly) returns the updated `description`, and `updatedAt` advances. The update is a replace, not a merge: the members named in the request become the prompt's members.
  Emulator: GetPrompt answers 200 but `$.description` is still the value the prompt was created with; the description supplied to the immediately preceding UpdatePrompt is absent from the read model. In the fixture, the created description is still returned where the updated one is required.
  Why: a client that updates a prompt and then reads it back to confirm (the ordinary read-after-write confirmation, and what CloudFormation/CDK drift detection does) sees its own write silently discarded. Nothing is refused, so the client has no error to retry on, and any workflow keyed on the updated prompt body continues against the stale one.
  Fix: the UpdatePrompt handler's state write - the update must be committed to the stored prompt record (and `updatedAt` bumped), not just reflected in the UpdatePrompt response.
  Traces: bedrock-agent-prompt-crud-lifecycle#task#4 ($.description value_mismatch: expected the updated description, got the create-time description).

Cause 2 of 4: DeletePrompt is acknowledged but the prompt remains readable, so GetPrompt answers 200 instead of 404 ResourceNotFoundException.
  AWS: DeletePrompt (Smithy: DELETE /prompts/{promptIdentifier}/, idempotent, errors include ResourceNotFoundException @httpError 404) removes the prompt. GetPrompt for a removed prompt fails with ResourceNotFoundException, HTTP 404, header `x-amzn-errortype: ResourceNotFoundException`, and the body message "Couldn't perform GetPrompt operation. Failed to retrieve resource type Prompt because it doesn't exist. Check the resource ID and retry your request." In that message the fixed text is "Couldn't perform <Operation> operation. Failed to retrieve resource type <ResourceType> because it doesn't exist. Check the resource ID and retry your request."; the variable parts are the operation name (GetPrompt) and the resource type (Prompt) - no id, name or timestamp from the request appears in it.
  Emulator: GetPrompt after DeletePrompt answers HTTP 200 with the full prompt representation; there is no `message` member and no `x-amzn-errortype` header at all.
  Why: a client that deletes a prompt and confirms removal by reading it back is told the prompt still exists, so cleanup and teardown logic loops or reports a leak; error-handling code paths keyed on ResourceNotFoundException are never exercised, and a recreate-after-delete flow collides with a record AWS would have removed.
  Fix: the DeletePrompt handler's state write (the prompt must be removed from, or tombstoned in, the service state) plus the GetPrompt handler's not-found path, which must raise ResourceNotFoundException with the fixed sentence above.
  Traces: bedrock-agent-prompt-crud-lifecycle#teardown#7 (status: expected 404, got 200; $.message missing; $headers.x-amzn-errortype missing).

Cause 3 of 4: the same delete-not-honored fault under polling - GetPrompt never converges to 404 after DeletePrompt, exhausting the step's budget.
  AWS: as in cause 2; after a successful DeletePrompt the prompt is gone and GetPrompt refuses with 404 ResourceNotFoundException and the fixed sentence quoted above. This is terminal, not eventually-consistent-in-the-other-direction: it never reverts to 200.
  Emulator: the step polls GetPrompt for the deleted prompt and gets HTTP 200 with the live prompt representation on every attempt (status 200 not in [404] after 13 attempts, poll capped at 60s). The response carries the prompt's name, variants, id, arn, version DRAFT and timestamps where the 404 envelope is required.
  Why: a client that waits for deletion to become visible waits forever and then times out, which converts a tidy teardown into a hung or failed job. Combined with cause 2, no caller can observe a prompt deletion at all.
  Fix: same component as cause 2 - DeletePrompt's state write and GetPrompt's not-found path.
  Traces: bedrock-agent-prompt-error-paths#task#7 and bedrock-agent-prompt-tagging#teardown#8 (one cluster; status expected 404, got 200 with the full prompt body).

Cause 4 of 4: GetPrompt with a numeric `promptVersion` refuses with 404 for a version the prompt holds.
  AWS: GetPrompt accepts `promptVersion` as an httpQuery parameter (Smithy: target Version, @length min 1 max 5, @pattern ^(DRAFT|[0-9]{0,4}[1-9][0-9]{0,4})$) and returns that published snapshot: required members `name`, `id`, `arn`, `version`, `createdAt`, `updatedAt`, plus `description`, `defaultVariant` and `variants` as the version captured them. The `arn` of a versioned read carries the version suffix (:<version>), per PromptArn's pattern ^(arn:aws:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:prompt/[0-9a-zA-Z]{10}(?::[0-9]{1,5})?)$, and `version` is the requested numeric version rather than DRAFT.
  Emulator: the versioned GetPrompt answers HTTP 404 and every output member is absent ($.arn, $.createdAt, $.defaultVariant, $.description, $.id, $.name, $.updatedAt, $.variants, $.version all missing) - the emulator holds no version snapshot to read, so a published version is indistinguishable from a nonexistent prompt.
  Why: a client that publishes a prompt version and then pins its invocations to that version cannot read the version at all, so version-pinned prompt usage (the whole point of prompt versioning) is unreachable and the failure is reported as if the prompt did not exist. Related open issue "Implement bedrock-agent CreatePromptVersion: unimplemented scaffold stub" (20261118T170616Z-bedrock-agent-createpromptversion-stub.md) covers the create side only; this cause is the read side and is not covered by it.
  Fix: prompt-version state (published snapshots must be retained and keyed by numeric version) and the GetPrompt handler's promptVersion resolution, which must select the snapshot instead of falling through to not-found.
  Traces: bedrock-agent-prompt-versioning#task#3 (status: expected 200, got 404; all nine output members missing).

## Evidence
Replay of trace_set simparity-wire-aws-v1 (score 87.2%, 25 mismatched steps), bedrock-agent clusters:

Cause 1 - fixture bedrock-agent-prompt-crud-lifecycle step task#4, GetPrompt, status 200 -> 200:
  $.description: value_mismatch (expected 'Updated prompt description', got 'Initial prompt for CRUD lifecycle test')

Cause 2 - fixture bedrock-agent-prompt-crud-lifecycle step teardown#7, GetPrompt, expected 404, actual 200:
  status: expected 404, got 200
  $.message: missing (expected "Couldn't perform GetPrompt operation. Failed to retrieve resource type Prompt because it doesn't exist. Check the resource ID and retry your request.", got None)
  $headers.x-amzn-errortype: missing (expected 'ResourceNotFoundException', got None)

Cause 3 - fixture bedrock-agent-prompt-error-paths step task#7 (also bedrock-agent-prompt-tagging#teardown#8), GetPrompt, expected 404, actual 200:
  $: poll capped at 60s; poll: status 200 not in [404] after 13 attempt(s); actual body is the live prompt representation (name/variants/id/arn/version DRAFT/createdAt/updatedAt)
  $.message: missing (expected the fixed ResourceNotFoundException sentence)
  $headers.x-amzn-errortype: missing

Cause 4 - fixture bedrock-agent-prompt-versioning step task#3, GetPrompt (promptVersion), expected 200, actual 404:
  status: expected 200, got 404
  $.arn, $.createdAt, $.defaultVariant, $.description, $.id, $.name, $.updatedAt, $.variants, $.version: all missing

Model evidence (services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda): GetPrompt GET /prompts/{promptIdentifier}/ readonly, promptVersion httpQuery (@pattern ^(DRAFT|[0-9]{0,4}[1-9][0-9]{0,4})$), errors include ResourceNotFoundException @httpError 404; UpdatePrompt PUT /prompts/{promptIdentifier}/ idempotent; DeletePrompt DELETE /prompts/{promptIdentifier}/ idempotent.

## Steps to Reproduce
1. Replay the wire corpus: replay_wire_corpus() against the emulator built from this checkout.
2. Cause 1: replay fixture bedrock-agent-prompt-crud-lifecycle and inspect step task#4 - CreatePrompt, UpdatePrompt (new description), then GetPrompt; the read returns the create-time description.
3. Cause 2: same fixture, step teardown#7 - DeletePrompt then GetPrompt; expect 404 ResourceNotFoundException, observe 200 with the prompt body.
4. Cause 3: replay fixture bedrock-agent-prompt-error-paths step task#7 (and bedrock-agent-prompt-tagging teardown#8) - poll GetPrompt after DeletePrompt; it stays 200 until the 60s poll cap.
5. Cause 4: replay fixture bedrock-agent-prompt-versioning step task#3 - publish a prompt version, then GET /prompts/{promptIdentifier}/?promptVersion=1; expect 200 with version '1' and a version-suffixed arn, observe 404 with no members.
