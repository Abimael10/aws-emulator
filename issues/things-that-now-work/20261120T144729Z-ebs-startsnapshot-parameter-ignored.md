Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: a14965d6060559849ea419b74faa25e7836c4558

# A snapshot started from an encrypted parent inherits the parent's SseType but is stored as not encrypted, with no KmsKeyArn

_Filed: 2026-11-20T14:47:29Z_
_Operation: StartSnapshot | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `a14965d6060559849ea419b74faa25e7836c4558`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StartSnapshot` derives the stored snapshot's encryption state from the REQUEST's `Encrypted`/`KmsKeyArn` only; `ParentSnapshotId` contributes nothing to it, even though the model states the encryption status depends on it. One line later the same handler DOES inherit the parent's server-side-encryption type into `sse_type`.

The result is a persisted record that contradicts the crate's own definition of those two fields: a child of a snapshot encrypted with the EBS-managed key (or with a caller-named KMS key) is stored with `sse_type == Some(SseEbs)` / `Some(SseKms)` while `encrypted == false` and `kms_key_arn == None`. `StartSnapshot`'s own 201 then reports `SseType: "sse-kms"` for a snapshot it reports no `KmsKeyArn` for, and the EC2 projection of the very same record — which reads these fields straight off this store — reports `encrypted: false` beside `sseType: sse-ebs`/`sse-kms` and a null `kmsKeyId`.

Either the child of an encrypted parent is encrypted (and `encrypted`, and for an inherited KMS type the key reference, must be inherited with the SSE type), or it is not (and no SSE type may be inherited). The handler does both at once, so no reader can tell which.

## Evidence

The model sentence the derivation drops — `smithy_query(query="doc",shape="com.amazonaws.ebs#StartSnapshotRequest",member="Encrypted")`, repeated verbatim on `KmsKeyArn` and on `ParentSnapshotId`:
"The encryption status of the snapshot depends on the values that you specify for Encrypted, KmsKeyArn, and ParentSnapshotId, and whether your Amazon Web Services account is enabled for encryption by default."

The handler — services/service-com-amazonaws-ebs-ebs/src/ops/start_snapshot.rs:391-400:
"        let encrypted = input.encrypted == Some(true) || input.kms_key_arn.is_some();
        let sse_type = if input.kms_key_arn.is_some() {
            Some(SseType::SseKms)
        } else if encrypted {
            Some(SseType::SseEbs)
        } else {
            // An unencrypted request inherits whatever the parent carried, which for an
            // unencrypted parent is no SSE type at all.
            parent_sse
        };"
`parent_sse` is the parent's stored type (start_snapshot.rs:348: "(Some(parent.blocks.clone()), parent.sse_type)"), so the `else` branch can only be reached with `encrypted == false` — and it is exactly the branch that can store `Some(...)`. Both values are then persisted side by side (start_snapshot.rs:426-429):
"            parent_snapshot_id: input.parent_snapshot_id.clone(),
            kms_key_arn: input.kms_key_arn.clone(),
            encrypted,
            sse_type,"

The rule this crate states for those fields — services/service-com-amazonaws-ebs-ebs/src/state.rs:110-116:
"/// The server-side encryption a snapshot reports through `StartSnapshotResponse$SseType`.
///
/// A snapshot that is not encrypted has NO SSE type: the member is optional with no
/// `@default`, and the recorded response of an unencrypted `StartSnapshot` carries no
/// `SseType` key at all. So \"no server-side encryption\" is `Option::None` — the member is
/// omitted — rather than the modelled `none` value, and this enum holds only the types a
/// snapshot can actually have."
and state.rs:196-198 for the stored member:
"    /// The server-side encryption this snapshot has, or `None` when it has none — in
    /// which case `SseType` is omitted from a response rather than reported as `none`.
    pub sse_type: Option<SseType>,"

The reader that makes the contradiction observable, which also asserts the invariant — services/service-com-amazonaws-ec2-amazonec2/src/state/direct_api_snapshots.rs:171 and 176-181:
"        encrypted: Some(snapshot.encrypted),"
"        // Both models spell the same server-side encryption types (`SSEType`: `sse-ebs`,
        // `sse-kms`, `none`), so the type the snapshot was created with is carried through as
        // it stands; a snapshot with no server-side encryption carries none.
        sse_type: snapshot
            .sse_type
            .map(|sse_type| sse_type.as_wire().to_string()),"

Test coverage: no test starts a snapshot from an ENCRYPTED parent. The encryption table in tests/all/snapshot_block_loop.rs:708-755 has no row carrying `ParentSnapshotId` ("encrypted with the EBS-managed key" = `{ "VolumeSize": 32, "Encrypted": true }`, "encrypted with a KMS key" = `{ "VolumeSize": 64, "KmsKeyArn": ... }`), tests/all/response_bodies_and_refusals.rs:96-130 likewise, and every parent/child fixture (snapshot_block_loop.rs:1151-1219, changed_blocks_between_snapshots.rs `sealed_snapshot`) starts its parent with no encryption input at all — so the inherited-SSE branch is reached by no assertion.

## Steps to Reproduce

1. POST /snapshots with {"VolumeSize": 1, "Encrypted": true} — 201, the body reports SseType "sse-ebs" (the record is stored with encrypted = true).
2. POST /snapshots/completion/{parent} with header x-amz-ChangedBlocksCount: 0 — the parent is sealed `completed`, so it may be used as a ParentSnapshotId.
3. POST /snapshots with {"VolumeSize": 1, "ParentSnapshotId": "<parent>"} — 201, and the body reports SseType "sse-ebs" inherited from the parent while the stored record has encrypted = false (start_snapshot.rs:391, neither Encrypted nor KmsKeyArn was given).
4. Read the same child through ec2:DescribeSnapshots: it reports encrypted = false together with sseType = sse-ebs — a snapshot that is simultaneously unencrypted and server-side encrypted.
5. Repeat steps 1-3 with a KMS parent ({"VolumeSize": 1, "KmsKeyArn": "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab"}): the child's StartSnapshot response carries SseType "sse-kms" and NO KmsKeyArn, and ec2:DescribeSnapshots reports kmsKeyId null with encrypted false.
