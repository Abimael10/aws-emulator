Issue-Source: trace-sweep-divergence
Issue-Trace: connectcases-field-lifecycle-and-batch-get
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcases-field-lifecycle-and-batch-get
Issue-Shape-Id: com.amazonaws.connectcases#AmazonConnectCases
Issue-Key: connectcases|BatchGetField|2xx->2xx|$.fields[*].description
Issue-Related: 20261109T181134Z-connectcases-field-state-system-field-seed-and-updatefield.md
Issue-Commit: 40a8392fd0198e21a06fbe4eb97ae7b924b10207

# connectcases BatchGetField returns pre-update description and emulator-worded per-entry error

> _Note: this issue was identified against the code base at commit `40a8392fd0198e21a06fbe4eb97ae7b924b10207`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetField

## Description
Two net-new wire divergences on BatchGetField, both on 200 responses, both reported by the replay with an EMPTY stub_path and an EMPTY owned_by. THIS ISSUE HAS TWO CAUSES - close both before submitting.

NOTE ON AN EXISTING ISSUE THAT DOES NOT COVER THESE: the open issue 20261109T181134Z-connectcases-field-state-system-field-seed-and-updatefield.md describes the same family of faults, but its replay pointer names only bare fixture ids (Issue-Trace-Ids: 'connectcases-field-and-options-lifecycle connectcases-field-lifecycle-and-batch-get') with no '#step' half, so neither of the steps below resolves to it and the replay reports both as unowned. The two causes here must be re-replayed and closed on their own keys. The related UpdateField scaffold issue 20261102T010613Z-connectcases-updatefield-stub.md owns the UpdateField operation itself, not these read-back/serialization divergences.

=== Cause 1 of 2 (representative): the per-entry error inside BatchGetField's `errors` list is worded by the emulator instead of AWS (fixture connectcases-field-lifecycle-and-batch-get, step task#7) ===
AWS: Smithy (services/service-com-amazonaws-connectcases-amazonconnectcases/smithy-model/com-amazonaws-connectcases-amazonconnectcases.json): BatchGetField = POST /domains/{domainId}/fields-batch, output {fields required, errors required}; BatchGetFieldErrorList holds FieldError = {id required (doc 'The field identifier that caused the error'), errorCode required, message}. For a field identifier the domain does not hold, AWS does NOT fail the call: it returns 200 with an `errors` entry. Recorded behavior: that entry's `message` is the fixed sentence `This resource cannot be found` - the sentence is entirely FIXED TEXT with NO variable parts; the requested identifier is not interpolated into it, because it is already carried in the entry's `id` member, which is where a client reads it from.
Emulator: the entry's message is a generated sentence of the form `Field <requested-id> was not found in domain <domainId>`, interpolating the requested identifier and the domain id (both of which are per-run values and are NOT part of any requirement; the requirement is the fixed AWS sentence).
Why: a client that classifies a partial batch failure by matching the per-entry message never matches AWS's sentence, so a resolvable 'missing field' outcome is misread as an unknown error class; and the identifier appears twice in the emulator's payload (once in `id`, once inlined in prose) in a shape AWS never emits.
Fix: BatchGetField's errors projection in the operation's handler/serializer - emit the fixed `message` for the not-found entry and leave the identifier to the entry's `id` member.
Traces: connectcases-field-lifecycle-and-batch-get#task#7.

=== Cause 2 of 2: a description changed by UpdateField is not visible to a later BatchGetField (fixtures connectcases-field-lifecycle-and-batch-get, steps task#7 and task#11) ===
AWS: Smithy: UpdateField = PUT /domains/{domainId}/fields/{fieldId} with optional members name (FieldName @length 1..100, @pattern ^.*[\S]$), description (FieldDescription @length max 255) and attributes; it is @idempotent and returns an empty body. The field detail shape (GetFieldResponse / the BatchGetField entry) carries `description` and `lastModifiedTime`. Recorded behavior: after a successful UpdateField that sets a new description, the NEXT read of that field returns the NEW description - the mutation is durable and visible to every subsequent read of the same field, on the first read after the update and on any later one.
Emulator: the BatchGetField entry carries the PRE-update description - the value the field had at CreateField time - on the read immediately after the update (task#7) and again on a later read in the same fixture (task#11). The update is accepted and then discarded, so no read ever observes it.
Why: a client cannot distinguish a successful UpdateField from a no-op. Configuration flows that rename/redescribe a field and then re-read it to confirm see the edit silently reverted, and every consumer of the field's metadata keeps operating on stale values that would have changed on AWS.
Fix: UpdateField's state mutation - persist the submitted `description` (and the other mutable members) onto the stored field so BatchGetField, GetField and ListFields observe it; the read path itself returns whatever state holds.
Traces: connectcases-field-lifecycle-and-batch-get#task#7 (first read after the update), connectcases-field-lifecycle-and-batch-get#task#11 (later read, same stale value).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, 182 steps, 143 passed, 30 mismatched, 9 blocked, score 78.6%, 9 divergence clusters (all connectcases). The two clusters below are the only ones with BOTH stub_path empty AND owned_by empty.

Cause 1 + Cause 2 - cluster service=connectcases|BatchGetField|status=200->200|keys=$.errors[0].message,$.fields[0].description, fixture connectcases-field-lifecycle-and-batch-get, step task#7, verdict=mismatch, expected 200 / actual 200, owned_by=[], stub_path='':
  $.errors[0].message: value_mismatch - expected 'This resource cannot be found' (fixed text, no variable parts), got 'Field <requested-id> was not found in domain <domainId>' (recorded instance: 'Field sim2real-probe-cc-missing-field-91b15066 was not found in domain 1b755540-694e-44c0-a10a-b59a3fe512c5'; the probe id and domain id are per-run values, the divergence is that AWS interpolates nothing)
  $.fields[0].description: value_mismatch - expected the post-UpdateField text (recorded instance 'sim2real text field, renamed'), got the pre-update text (recorded instance 'sim2real text field')

Cause 2 (second occurrence) - cluster service=connectcases|BatchGetField|status=200->200|keys=$.fields[0].description, fixture connectcases-field-lifecycle-and-batch-get, step task#11, verdict=mismatch, expected 200 / actual 200, owned_by=[], stub_path='':
  $.fields[0].description: value_mismatch - expected the post-UpdateField text, got the pre-update text (same stale value as task#7, on a later read)

Smithy citations: BatchGetField POST /domains/{domainId}/fields-batch -> {fields required, errors required}; FieldError {id required, errorCode required, message}; UpdateField PUT /domains/{domainId}/fields/{fieldId}, @idempotent, members name (@length 1..100, @pattern ^.*[\S]$), description (FieldDescription @length max 255), attributes, empty output; GetFieldResponse.description -> FieldDescription @length max 255.

## Steps to Reproduce
Replay the simparity-wire-aws-v1 corpus against the connectcases emulator and read the clusters at connectcases-field-lifecycle-and-batch-get#task#7 and connectcases-field-lifecycle-and-batch-get#task#11. Manually:
1. CreateDomain(name=<unique>); poll GetDomain until domainStatus == 'Active'.
2. CreateField(domainId, name=<N1>, type='Text', description=<D1>) -> fieldId.
3. UpdateField(domainId, fieldId, name=<N2>, description=<D2>) -> expect 200 with an empty body.
4. BatchGetField(domainId, fields=[{id: fieldId}, {id: <an id never created in this domain>}]).
   Expect 200 with fields[0].description == <D2> and errors[0] == {id: <the id never created>, errorCode: <AWS's code>, message: 'This resource cannot be found'}.
   The emulator returns fields[0].description == <D1> (Cause 2) and errors[0].message == 'Field <the id never created> was not found in domain <domainId>' (Cause 1).
5. Call BatchGetField(domainId, fields=[{id: fieldId}]) again later in the same session: the emulator still returns <D1> (Cause 2, second occurrence).
