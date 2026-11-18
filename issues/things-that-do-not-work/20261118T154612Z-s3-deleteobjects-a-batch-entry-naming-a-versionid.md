Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.delete-objects-partial-failure s3.s3-delete-objects-reports-every-requested-key-version-pair-exactly-once
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.delete-objects-partial-failure|run#16|AssertionError|assert bad_err.get("Code") == "NoSuchVersion", f"expected NoSuchVersion error for {bad_key!r}, got {errors}"
Issue-Key: s3.s3-delete-objects-reports-every-requested-key-version-pair-exactly-once|run#27|AssertionError|assert errored.count(bogus_pair) == 1, (
Issue-Commit: e811b8b41259ffda648a98493ef74b5680a725e4

# s3 DeleteObjects: a batch entry naming a VersionId that does not exist is reported in Deleted[] instead of Errors[] with NoSuchVersion

> _Note: this issue was identified against the code base at commit `e811b8b41259ffda648a98493ef74b5680a725e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteObjects

## Description
Cause 1 of 1: DeleteObjects launders an unresolvable version-pinned entry into a success.
AWS: Smithy: DeleteObjects takes Delete.Objects as a list of ObjectIdentifier (Key + optional VersionId) and returns two disjoint lists — Deleted of com.amazonaws.s3#DeletedObject (Key, VersionId, DeleteMarker, DeleteMarkerVersionId) and Errors of com.amazonaws.s3#Error (Key, VersionId, Code, Message, where Code is "a string that uniquely identifies an error condition" and Message "contains a generic description of the error condition"). DeleteObjects is processed per entry: an entry that names a VersionId which does not exist for that key is reported in Errors with Code "NoSuchVersion" and Message "The specified version does not exist." — that message is fixed text with no variable part; the Key and VersionId members echo the request entry's own key and version id. Every requested entry is reported exactly once, across Deleted and Errors together, and the two lists are disjoint. A refused entry must leave the key's existing versions untouched: no version removed, no delete marker invented.
Emulator: the entry naming a non-existent VersionId is reported in Deleted[] with its bogus VersionId echoed back, and Errors[] comes back empty. In one replay the response was Deleted=[('batch/versioned.bin','65d6a25626ef8b3aa6ea71225bbad0b2'), ('batch/versioned.bin','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')], Errors=[] — i.e. the unresolvable version id is treated as a successful delete rather than classified. (The literal key, version ids and bucket names above are one run's values, not requirements; the requirement is that an unresolvable VersionId lands in Errors with that code and message.)
Why: DeleteObjects is the batch primitive every bulk cleanup uses (bucket emptying, lifecycle tooling, CDK/CloudFormation bucket deletion, retention reconciliation). Reporting a delete that did not happen means a client that trusts Deleted[] records the version as gone, never retries it, and never discovers that its own VersionId bookkeeping is stale — the one signal AWS gives for a wrong or truncated version id is suppressed. Worse, it hides the opposite risk too: a caller cannot distinguish "this version id was wrong" from "this version was removed".
Fix: the S3 DeleteObjects handler's per-entry resolution against object-version state, and the serializer that partitions entries into Deleted[] and Errors[] — an entry whose VersionId does not resolve to a stored version of that key must be emitted as an Error (Key, VersionId echoed; Code NoSuchVersion; Message "The specified version does not exist.") and must not appear in Deleted[].
Traces: s3.delete-objects-partial-failure, s3.s3-delete-objects-reports-every-requested-key-version-pair-exactly-once.

## Evidence
s3.delete-objects-partial-failure [mismatch]:
Setup: versioning-enabled bucket; PutObject 'batch-good.txt' (version A) and 'batch-bad.txt' (version B); build a bogus version id by flipping the last character of A.
Request: DeleteObjects(Delete={Objects:[{Key:'batch-good.txt',VersionId:A},{Key:'batch-bad.txt',VersionId:<bogus>}], Quiet:false}).
Expected: Deleted contains 'batch-good.txt'; Errors contains an entry for 'batch-bad.txt' with Code 'NoSuchVersion' and Message 'The specified version does not exist.'; 'batch-bad.txt' version B still readable via HeadObject.
Actual: AssertionError "expected NoSuchVersion error for 'batch-bad.txt', got {}" — Errors[] is empty; the bogus entry was not classified as an error at all.

s3.s3-delete-objects-reports-every-requested-key-version-pair-exactly-once [mismatch]:
Request: DeleteObjects on a versioned key with two entries — the live 'doomed' version and the bogus version 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'.
Expected: the bogus (Key, VersionId) pair appears exactly once in Errors[] with Code 'NoSuchVersion' / Message 'The specified version does not exist.', Deleted[] and Errors[] disjoint.
Actual: AssertionError "the non-existent version named in the batch must be reported on the ERROR side exactly once rather than laundered into a success: ('batch/versioned.bin', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa') appears 0 time(s) in Errors=[] (Deleted=[('batch/versioned.bin', '65d6a25626ef8b3aa6ea71225bbad0b2'), ('batch/versioned.bin', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')])" — the bogus pair is reported on the success side.

## Steps to Reproduce
1) CreateBucket (add CreateBucketConfiguration.LocationConstraint outside us-east-1). 2) PutBucketVersioning Status=Enabled. 3) PutObject a key twice and keep both VersionIds; also PutObject a second key and keep its VersionId. 4) Construct a VersionId that does not exist for the key (e.g. 36 'a' characters, or the real id with its last character changed). 5) Call DeleteObjects with Delete.Objects = [{Key:<key>, VersionId:<a real live version>}, {Key:<key or the second key>, VersionId:<the non-existent version>}] and Quiet=false. 6) Inspect the response: Errors[] is empty and the non-existent (Key, VersionId) pair is listed in Deleted[]; AWS instead lists it in Errors[] with Code 'NoSuchVersion' and Message 'The specified version does not exist.' and leaves that key's stored versions unchanged.
