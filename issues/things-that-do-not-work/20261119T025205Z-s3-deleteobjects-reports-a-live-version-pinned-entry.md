Issue-Source: trace-sweep-divergence
Issue-Trace: s3-mbes-traj-delete-objects-batch-mistakes
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-mbes-traj-delete-objects-batch-mistakes
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|DeleteObjects|2xx->2xx|$.DeleteResult.Deleted
Issue-Key: s3|DeleteObjects|2xx->2xx|$.DeleteResult.Error
Issue-Commit: 9b7b0440ca3eb4edd2b2fd218f92aa2e777da262

# s3 DeleteObjects reports a live version-pinned entry as NoSuchVersion instead of Deleted

> _Note: this issue was identified against the code base at commit `9b7b0440ca3eb4edd2b2fd218f92aa2e777da262`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteObjects

## Description
Cause 1 of 1: DeleteObjects mis-partitions a mixed batch — the entry whose VersionId DOES resolve to a stored version is reported as a NoSuchVersion failure, so the response carries no Deleted element at all and every requested entry lands in Errors[].

AWS: Smithy: DeleteObjects (POST /{Bucket}?delete, 200) takes Delete.Objects as a list of com.amazonaws.s3#ObjectIdentifier (Key required, VersionId optional) and returns DeleteResult with two disjoint, xmlFlattened lists — Deleted of com.amazonaws.s3#DeletedObject (Key, VersionId, DeleteMarker, DeleteMarkerVersionId) and Errors (xmlName "Error") of com.amazonaws.s3#Error (Key, VersionId, Code "a string that uniquely identifies an error condition", Message). The operation is evaluated per entry and every requested (Key, VersionId) pair is reported exactly once, on exactly one of the two sides. An entry naming a VersionId that IS a stored version of that key is deleted and reported in Deleted[] with the request's Key and VersionId echoed; only an entry whose VersionId does not resolve is reported in Errors[] with Code "NoSuchVersion" and Message "The specified version does not exist." — that message is fixed text with no variable part, while the Key and VersionId members are taken verbatim from the request entry. A batch containing one resolvable and one unresolvable version therefore yields exactly one Deleted entry and exactly one Errors entry.

Emulator: for the same mixed batch the emulator emits NO Deleted element and puts BOTH requested entries into Errors[], each with Code "NoSuchVersion" / Message "The specified version does not exist." — including the entry whose VersionId is a live version the emulator itself minted and served earlier in the same fixture. The step still answers 200, so the fault is purely in per-entry version resolution and the Deleted/Errors partition, not in the status or envelope shape. Hypothesis: the handler's per-entry lookup never matches a stored version id (it fails every version-pinned entry closed), which also means no version is actually removed from state.

Why: DeleteObjects is the batch primitive used by every bulk cleanup path — bucket emptying, CDK/CloudFormation bucket deletion, lifecycle and retention tooling. A client that reads Errors[] sees a delete that AWS performed reported as a failure: it retries forever, treats an emptied bucket as non-empty, or aborts a teardown. Because Deleted[] is absent entirely, a caller that drives progress from Deleted[] (the documented success side) makes no progress at all, and a caller that counts reported entries cannot distinguish "this version id was wrong" from "this version was removed". It is the exact inverse of the already-open behavioral issue 20261118T154612Z-s3-deleteobjects-a-batch-entry-naming-a-versionid.md (which reports unresolvable version ids being laundered INTO Deleted[]); that issue does not cover this step and this cluster is unowned, so the two together say the per-entry classification is simply not keyed on stored version state in either direction.

Fix: the S3 DeleteObjects handler's per-entry resolution of ObjectIdentifier.VersionId against stored object-version state, and the serializer that partitions entries into Deleted[] and Errors[] in DeleteResult: an entry whose VersionId names an existing version of that key must delete that version and be emitted in Deleted[] (Key and VersionId echoed), and only an unresolvable VersionId may be emitted in Errors[] with Code NoSuchVersion and the fixed message. The two lists must remain disjoint and together cover every requested entry exactly once.

Traces: s3-mbes-traj-delete-objects-batch-mistakes#task#8.

## Evidence
Fixture s3-mbes-traj-delete-objects-batch-mistakes, step task#8, operation DeleteObjects, verdict mismatch, status expected 200 / actual 200.

Expected (recorded AWS response, DeleteResult):
  Deleted = {Key: 'svc-file1.txt', VersionId: 'c2bf84a85265ec6f97e204a28277db65'}
  Error   = {Key: 'svc-file2.txt', VersionId: '3sL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY+MTRCxf3vjVBH40Nr8X8gdRQBpUMLUo', Code: 'NoSuchVersion', Message: 'The specified version does not exist.'}

Actual (emulator):
  $.DeleteResult.Deleted: missing (expected the svc-file1.txt entry, got None)
  $.DeleteResult.Error: type_mismatch — a two-element list instead of the single error element:
    [{Key: 'svc-file1.txt', VersionId: 'c2bf84a85265ec6f97e204a28277db65', Code: 'NoSuchVersion', Message: 'The specified version does not exist.'},
     {Key: 'svc-file2.txt', VersionId: '3sL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY+MTRCxf3vjVBH40Nr8X8gdRQBpUMLUo', Code: 'NoSuchVersion', Message: 'The specified version does not exist.'}]

Reading: the error code 'NoSuchVersion' and the message 'The specified version does not exist.' are correct and are fixed text (no variable part); Key and VersionId are values taken from the request entry. The divergence is the SIDE each entry is reported on — the live, resolvable version (first entry) belongs in Deleted[], not Errors[] — plus the resulting absence of any Deleted element. The specific key names and version-id strings above are this recording's values filling the template, not requirements.

## Steps to Reproduce
1) CreateBucket (supply CreateBucketConfiguration.LocationConstraint outside us-east-1). 2) PutBucketVersioning Status=Enabled. 3) PutObject two keys and keep the VersionId each PutObject returns for the first key; for the second key construct a VersionId that does not exist (e.g. an unrelated 64-character version-id-shaped string). 4) POST /{Bucket}?delete with Delete.Objects = [{Key: <first key>, VersionId: <the real VersionId returned in step 3>}, {Key: <second key>, VersionId: <the non-existent version id>}] and Quiet=false. 5) Parse the DeleteResult: the emulator returns no Deleted element and two Error elements, both Code NoSuchVersion / 'The specified version does not exist.'. AWS returns one Deleted element echoing the first key and its real VersionId, and exactly one Error element for the second key. 6) Re-replay fixture s3-mbes-traj-delete-objects-batch-mistakes step task#8 to confirm.
