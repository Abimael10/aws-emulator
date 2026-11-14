Issue-Source: trace-sweep-divergence
Issue-Trace: datasync-task-lifecycle-and-tags
Issue-Step: task#17
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: datasync-task-lifecycle-and-tags
Issue-Shape-Id: com.amazonaws.datasync#FmrsService
Issue-Key: datasync|TagResource|4xx->4xx|$.datasyncErrorCode
Issue-Key: datasync|TagResource|4xx->4xx|$.errorCode
Issue-Key: datasync|TagResource|4xx->4xx|$.message
Issue-Commit: 8fe616c4199d8d04dc490aa9485fc162b633d53d

# DataSync TagResource on a nonexistent resource returns SyncSetNotFound with an ARN-bearing message instead of NotFound / "Not found"

> _Note: this issue was identified against the code base at commit `8fe616c4199d8d04dc490aa9485fc162b633d53d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
One cause.

Cause 1 of 1: TagResource against a well-formed but nonexistent DataSync resource ARN is refused with the wrong error code and the wrong error message.

AWS: TagResource takes a required ResourceArn (Smithy: datasync#TagResource input ResourceArn -> TaggableResourceArn, required, @length max 128, @pattern anchored on arn:<partition>:datasync:<region>:<account>:(agent|task|location)/(agent|task|loc)-[a-z0-9]{17}...) and models InvalidRequestException (client error, HTTP 400) whose members are message, errorCode and datasyncErrorCode (Smithy: datasync#InvalidRequestException, httpError 400, doc "This exception is thrown when the client submits a malformed request."). The model does not state which code value a missing resource yields, so for the code and message this is recorded behavior only: the recording shows HTTP 400 with __type "InvalidRequestException", errorCode "NotFound", datasyncErrorCode "NotFound" and message "Not found". That message is entirely FIXED text — DataSync's not-found refusal on the tagging APIs carries no variable part, no ARN, no id and no member name interpolated into it. The code value "NotFound" is likewise fixed for this error class.

Emulator: the status is right (400) and __type is right, but the code and message are its own. It answers errorCode "SyncSetNotFound" and datasyncErrorCode "SyncSetNotFound", and a message built by interpolating the requested ARN — of the form "Task <resource-arn> is not found." (the recorded instance read "Task arn:aws:datasync:us-west-2:000000000000:task/task-00000000000000001 is not found."; the ARN there is the request's value filling the emulator's template, not part of any requirement). So it substitutes an internal resource-kind-specific code for DataSync's generic one, and an ARN-bearing sentence for DataSync's fixed "Not found".

Why: clients branch on datasyncErrorCode/errorCode, not on prose. Code that treats "NotFound" as "this task/location/agent is gone, stop retrying and move on" sees an unrecognized "SyncSetNotFound" and falls through to its generic-failure path — retrying, aborting a cleanup loop, or surfacing an opaque error — while tag-reconciliation code that asserts on the message text never matches. Tests and runbooks pinned to the real DataSync code pass against AWS and fail against the emulator.

Fix: the TagResource handler's not-found path in the datasync service crate (the resource-lookup failure that is converted into InvalidRequestException): it must emit errorCode and datasyncErrorCode "NotFound" and the fixed message "Not found" for an unresolvable ResourceArn, rather than a per-resource-kind code and an ARN-interpolated sentence. The same code/message pair should be used for every taggable resource kind, since AWS does not vary it by kind.

Traces: datasync-task-lifecycle-and-tags#task#17 (TagResource on a task ARN that does not exist).

No open issue covers this: the existing DataSync tag/lifecycle issue 20261029T180920Z-datasync-task-reads-never-settle-to-available-omit.md concerns DescribeTask field fidelity and UpdateTask edits, and 20261105T072924Z-datasync-deletelocation-ignores-task-references-an-in-use.md concerns DeleteLocation in-use enforcement; neither touches the TagResource not-found code or message.

## Evidence
Fixture datasync-task-lifecycle-and-tags, step task#17, operation TagResource.

Expected (recorded AWS response), HTTP 400:
  $.__type            = "InvalidRequestException"
  $.errorCode         = "NotFound"
  $.datasyncErrorCode = "NotFound"
  $.message           = "Not found"

Actual (emulator), HTTP 400:
  $.__type            = "InvalidRequestException"   (matches)
  $.errorCode         = "SyncSetNotFound"
  $.datasyncErrorCode = "SyncSetNotFound"
  $.message           = "Task arn:aws:datasync:us-west-2:000000000000:task/task-00000000000000001 is not found."

Replay verdict: mismatch, status 400 -> 400, diverging keys $.datasyncErrorCode, $.errorCode, $.message.

Message anatomy: AWS's text is 100% fixed ("Not found"); it has no variable segment. The emulator's text is a template "Task <ResourceArn> is not found." whose only variable segment is the ARN copied from the request — so the divergence is the wording and the code, not a per-run value.

## Steps to Reproduce
1. Boot the emulator and replay fixture datasync-task-lifecycle-and-tags.
2. At step task#17 the fixture calls DataSync TagResource with a ResourceArn that matches TaggableResourceArn's pattern but names a task that was already deleted / never created (recorded as arn:aws:datasync:<region>:<account>:task/task-00000000000000001), with a non-empty Tags list.
3. Observe the 400 response body: the emulator returns errorCode/datasyncErrorCode "SyncSetNotFound" and the message "Task <arn> is not found.".
4. AWS returns errorCode/datasyncErrorCode "NotFound" and the message "Not found" for the same request.
