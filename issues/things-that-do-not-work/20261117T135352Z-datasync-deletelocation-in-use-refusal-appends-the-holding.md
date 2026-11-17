Issue-Source: trace-sweep-divergence
Issue-Trace: datasync-task-lifecycle-and-tags
Issue-Step: task#14
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: datasync-task-lifecycle-and-tags
Issue-Shape-Id: com.amazonaws.datasync#FmrsService
Issue-Key: datasync|DeleteLocation|4xx->4xx|$.message
Issue-Commit: f8316590bcf97b65fd5e64b3c988acfd983e0aed

# DataSync DeleteLocation in-use refusal appends the holding task to the message text

> _Note: this issue was identified against the code base at commit `f8316590bcf97b65fd5e64b3c988acfd983e0aed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteLocation

## Description
Cause 1 of 1: DeleteLocation refuses to delete a location that is still referenced by a task, but the emulator writes a different error message than AWS does for that error class.

AWS: Smithy (com-amazonaws-datasync-fmrsservice) models DeleteLocation with input LocationArn (required, @length 0..128, @pattern ^arn:(aws|...):datasync:[a-z\-0-9]+:[0-9]{12}:location/loc-[0-9a-z]{17}$) and errors InvalidRequestException (client, httpError 400) and InternalException (500). The model does not state the message text, so for the message this is recorded behavior only: the recording shows HTTP 400 with __type/errorCode/datasyncErrorCode consistent with the InvalidRequestException the emulator already returns, and a message whose template is the fixed text "Location " + <location id> + " is in use." — the only variable part is the location id taken from the request's LocationArn (in the recording, loc-015495749bfe38055). There is no second variable part: AWS does not name which task holds the location.

Emulator: returns the correct 400 and the correct error code (only $.message diverges), but emits "Location <location-id> is in use by task <task-id>." — i.e. it substitutes its own location id and then appends the fixed clause " by task " + <task id> + "." that AWS never emits. In the replayed step the emulator produced "Location loc-00000000000000001 is in use by task task-00000000000000001." where AWS produced "Location loc-015495749bfe38055 is in use." (the differing ids themselves are per-run values and are not the defect; the extra " by task ..." clause and the resulting sentence shape are).

Why: a client that matches, asserts on, or parses the InvalidRequestException message for this refusal — error-message assertions in tests, log/alarm patterns, or code that extracts the trailing identifier from the sentence — reads the message as naming a task resource where AWS names only the location. Any such consumer either fails to match AWS's text or, worse, extracts a task id from the emulator that AWS would never supply, so behavior written against the emulator breaks against the real service.

Fix: the DeleteLocation handler's in-use refusal path — the place where it formats the InvalidRequestException message after finding a referencing task. It should emit only the fixed text "Location {location_id} is in use." with the location id taken from the request's LocationArn, and must not interpolate the referencing task's id into the message (the task lookup may stay; only the message template changes). The status and error codes are already correct and need no change.

Traces: datasync-task-lifecycle-and-tags#task#14 (DeleteLocation of a location still bound to a task, expected 400 + "Location <loc-id> is in use.", got 400 + "Location <loc-id> is in use by task <task-id>."). No existing open DataSync issue covers this message: the nearest ones are 20261114T222251Z-datasync-tagresource-on-a-nonexistent-resource-returns-syncsetnotfound.md (TagResource NotFound code/message) and 20261029T180920Z-datasync-task-reads-never-settle-to-available-omit.md (DescribeTask state/lists), neither of which touches DeleteLocation.

## Evidence
Fixture datasync-task-lifecycle-and-tags, step task#14, operation DeleteLocation, verdict mismatch.
Expected status 400, actual status 400 (status agrees).
Diverging key: $.message
  expected: 'Location loc-015495749bfe38055 is in use.'
  actual:   'Location loc-00000000000000001 is in use by task task-00000000000000001.'
Message anatomy — fixed text AWS emits: "Location " ... " is in use."; variable part: the location id, taken from the request's LocationArn. Emulator-only addition (not part of AWS's template): the clause " by task <task id>" before the terminating period, with the task id taken from emulator state.
Error code fields ($.__type / $.errorCode / $.datasyncErrorCode) matched and are not part of this finding.
Smithy reference: DeleteLocation -> errors [InvalidRequestException (client, httpError 400), InternalException (server, 500)]; input LocationArn required, @length 0..128, @pattern ^arn:(aws|aws-cn|aws-us-gov|aws-eusc|aws-iso|aws-iso-b):datasync:[a-z\-0-9]+:[0-9]{12}:location/loc-[0-9a-z]{17}$.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1); or reproduce by hand:
   a. CreateLocationS3 (or any CreateLocation*) to get a location ARN.
   b. CreateTask referencing that location as source or destination.
   c. Call DeleteLocation with the still-referenced LocationArn.
3. Observe the 400 InvalidRequestException body. The emulator's message is "Location <loc-id> is in use by task <task-id>."; AWS returns "Location <loc-id> is in use." with no task clause.
4. Confirmation: re-replay datasync-task-lifecycle-and-tags, step task#14.
