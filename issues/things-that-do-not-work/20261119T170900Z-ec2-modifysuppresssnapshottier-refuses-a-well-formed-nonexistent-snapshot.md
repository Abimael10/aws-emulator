Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-modify-snapshot-tier
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-modify-snapshot-tier|run#20|AssertionError|assert err == "InvalidParameterValue", err
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# ec2: ModifySuppressSnapshotTier refuses a well-formed, nonexistent snapshot id with InvalidSnapshotID.Malformed instead of InvalidParameterValue

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifySnapshotTier

## Description
Cause 1 of 1: ModifySnapshotTier classifies a well-formed but nonexistent snapshot id as a malformed id, so the refusal carries the wrong error code and the wrong message.

AWS:      Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) declares ModifySnapshotTier with input members SnapshotId (@required, target SnapshotId), StorageTier (enum TargetStorageTier, sole member `archive`) and DryRun, and declares NO modelled errors at all (ec2Query carries its refusals untyped), so the error classification is recorded behavior only. The recording shows that ModifySnapshotTier does NOT use the per-resource snapshot error classes the rest of the snapshot surface uses (InvalidSnapshot.NotFound / InvalidSnapshotID.Malformed): for a syntactically well-formed snapshot id (`snap-` followed by 17 hex characters) that names no snapshot, it answers an HTTP 400 ec2Query error with Code `InvalidParameterValue` and Message `Value (<snapshotId>) for parameter snapshotId is invalid.` In that message the fixed text is `Value (`, `) for parameter snapshotId is invalid.` and the member name `snapshotId` (the wire/ec2QueryName spelling of the SnapshotId member, lower-camel); the only variable part is the snapshot id echoed verbatim from the request. The generic InvalidParameterValue class is what ModifySnapshotTier uses for both the unreadable-id and the unknown-id readings of SnapshotId.

Emulator: for the same request the emulator answers Code `InvalidSnapshotID.Malformed`. The handler validates the id with `issued_snapshot_id_is_well_formed` and, when that predicate rejects it, returns `invalid_snapshot_id_malformed(...)` before the inventory is consulted; the fall-through path returns `invalid_snapshot_id_not_found(...)`. Neither path can ever produce the InvalidParameterValue code or the `Value (...) for parameter snapshotId is invalid.` sentence AWS words this refusal with, so both the unreadable-id and the unknown-id readings of SnapshotId diverge. The happy path of the check (archive accepted, TieringStartTime present, DescribeSnapshotTierStatus reporting archival-in-progress with LastTieringStartTime) passed; only the refusal classification diverges.

Why:      A client that branches on the error code to tell "you gave me a snapshotId I cannot use" from "that snapshot exists but is in the wrong state" takes the wrong branch. Code-keyed logic — botocore `exceptions.ClientError` handling, retry/ignore tables keyed on `InvalidParameterValue`, CloudFormation and Terraform providers that swallow a specific code — sees an unknown code and either aborts a tiering workflow or treats a parameter error as a transient resource error. The message a client surfaces to an operator also names the wrong cause ("malformed" for an id that is perfectly well formed).

Fix:      The refusal classification in the ModifySnapshotTier handler's SnapshotId validation and resolution: this operation must not reuse the shared snapshot id malformed/not-found refusal helpers, it needs the generic parameter-value refusal (code `InvalidParameterValue`, message `Value (<snapshotId>) for parameter snapshotId is invalid.`) for every SnapshotId it cannot resolve.

Note:     the open issue 20261105T021000Z-ec2-modifysnapshottier-stub.md lists this same check id, but it asks only that the scaffold stub be implemented (it describes a 501 NotImplemented). The handler IS implemented now and answers a real refusal, so that issue does not cover this classification defect.

Traces:   ec2.op-contract-modify-snapshot-tier. Handler: services/service-com-amazonaws-ec2-amazonec2/src/ops/modify_snapshot_tier.rs lines 179-181 (malformed branch) and 195-196 (not-found branch).

## Evidence
Failing check `ec2.op-contract-modify-snapshot-tier` (verdict: mismatch).

Request: ModifySnapshotTier(SnapshotId="snap-00000000000000000", StorageTier="archive") — `snap-` plus 17 hex characters, i.e. a well-formed id that names no snapshot in the account.

Expected (recorded AWS): HTTP 400, Error.Code = `InvalidParameterValue`, Error.Message = `Value (snap-00000000000000000) for parameter snapshotId is invalid.` (fixed text `Value (` / `) for parameter snapshotId is invalid.`; the id is taken from the request).

Actual (emulator): Error.Code = `InvalidSnapshotID.Malformed`.

Replay output:
  File "check_ec2_op_contract_modify_snapshot_tier.py", line 62, in run
    assert err == "InvalidParameterValue", err
  AssertionError: InvalidSnapshotID.Malformed

The preceding assertions in the same check passed, so ModifySnapshotTier's success path (SnapshotId echoed, TieringStartTime present) and DescribeSnapshotTierStatus (LastTieringOperationStatus archival-in-progress, LastTieringStartTime present) are already correct; only the refusal classification diverges.

## Steps to Reproduce
1. Boot the ec2 emulator.
2. CreateVolume(AvailabilityZone=<available az>, Size=1, VolumeType="gp3") and wait for `available`.
3. CreateSnapshot(VolumeId=<volume id>) and wait for `completed`.
4. ModifySnapshotTier(SnapshotId=<snapshot id>, StorageTier="archive") — this succeeds.
5. ModifySnapshotTier(SnapshotId="snap-00000000000000000", StorageTier="archive").
6. Observe Error.Code `InvalidSnapshotID.Malformed`; AWS answers Code `InvalidParameterValue` with Message `Value (snap-00000000000000000) for parameter snapshotId is invalid.`
