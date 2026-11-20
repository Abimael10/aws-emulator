Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: a3c668f6bb53d78c1d1232fe39d40072881572f4

# PutSnapshotBlock drops ChecksumAlgorithm's modelled @length/@pattern and echoes the whole header into a 256-capped Message

_Filed: 2026-11-20T19:24:38Z_
_Operation: PutSnapshotBlock | Class: error-response_

> _Note: this issue was identified against the code base at commit `a3c668f6bb53d78c1d1232fe39d40072881572f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`PutSnapshotBlockInput` puts `ChecksumAlgorithm` on `skip_validation` together with `Checksum` and `Progress`, which removes the member's modelled `@length 0..32` and `@pattern ^[A-Za-z0-9]+$` from the request boundary. The handler then re-applies the modelled constraints of the OTHER two skipped members through the framework's own constraint walk (`header_constraint_refusal`, backed by `CHECKSUM_TRAITS`/`PROGRESS_TRAITS`), but nothing re-applies `ChecksumAlgorithm`'s. The only thing the handler does with the value is compare it to the literal `SHA256` and, on any mismatch, interpolate the ENTIRE header value verbatim into the refusal sentence.

Two consequences follow from the same omission:

1. A `x-amz-Checksum-Algorithm` value that violates the member's own modelled traits (longer than 32 characters, or containing a character outside `[A-Za-z0-9]`, e.g. `SHA 256`) is reported as an "unsupported algorithm" value refusal instead of the constraint report its two sibling skipped members get — the very report the handler says it restated the traits in order to produce.
2. Because the length trait is gone and the handler bounds nothing, the echoed value is unbounded. A 300-byte header produces a `Message` of ~370 characters, while the model types `ValidationException$Message` as `ErrorMessage`, a string with `@length 0..256`. The comment beside the sibling check names this exact property of the framework walk it avoided — "its ceiling on how much of the rejected value is echoed" — so the value-echoing refusal for `ChecksumAlgorithm` has no ceiling at all.

The same hole exists on `CompleteSnapshot`, which also puts `ChecksumAlgorithm` on `skip_validation` and refuses with the same shared sentence; a fix belongs in the shared helper so both operations word it once.

## Evidence

Smithy (contract) — `smithy_query(query="shape",shape="com.amazonaws.ebs#PutSnapshotBlockRequest")`:
  "ChecksumAlgorithm": {"target": "ChecksumAlgorithm", "httpHeader": "x-amz-Checksum-Algorithm", "required": true, "length": {"min": 0, "max": 32}, "pattern": "^[A-Za-z0-9]+$", "type": "enum"}
and `smithy_query(query="shape",shape="com.amazonaws.ebs#ErrorMessage")`:
  {"shape_id": "com.amazonaws.ebs#ErrorMessage", "type": "string", "length": {"min": 0, "max": 256}}
(`ValidationException$Message` targets `ErrorMessage`: "Message": {"target": "ErrorMessage", "length": {"min": 0, "max": 256}}.)

Repo side 1 — the constraints are taken off the boundary. services/service-com-amazonaws-ebs-ebs/src/ops/types/put_snapshot_block_types.rs:31-34:
    #[smithy_macros::smithy(
        shape = "com.amazonaws.ebs#PutSnapshotBlockRequest",
        skip_validation = ["ChecksumAlgorithm", "Checksum", "Progress"]
    )]

Repo side 2 — the crate states WHY the skipped members' traits must be restated, and restates only two of the three. services/service-com-amazonaws-ebs-ebs/src/ops/put_snapshot_block.rs:399-405:
    /// The modelled constraint traits of the header members this operation judges itself:
    /// `Checksum`'s `@length` 0..64 and `@pattern` and `Progress`'s `@range` 0..100, as
    /// `smithy_query(query="shape",shape="com.amazonaws.ebs#PutSnapshotBlockRequest")`
    /// reports them.
    ///
    /// Restated here because `skip_validation` takes them off the generated member schemas,
    /// so there is none left at the boundary to read them from.
and the entry point takes only two members — put_snapshot_block.rs:459:
    fn header_constraint_refusal(checksum: &str, progress: Option<i32>) -> Option<String> {
called at put_snapshot_block.rs:236-238:
            if let Some(message) = header_constraint_refusal(&input.checksum, input.progress) {
                return Err(validation("INVALID_PARAMETER_VALUE", message));
            }

Repo side 3 — the only thing `ChecksumAlgorithm` gets, echoing the value unbounded. put_snapshot_block.rs:227-232:
            if input.checksum_algorithm != CHECKSUM_ALGORITHM_SHA256 {
                return Err(validation(
                    "INVALID_PARAMETER_VALUE",
                    unsupported_checksum_algorithm_message(&input.checksum_algorithm),
                ));
            }
and services/service-com-amazonaws-ebs-ebs/src/state.rs:623-628:
    pub fn unsupported_checksum_algorithm_message(algorithm: &str) -> String {
        format!(
            "Unsupported Checksum Algorithm '{algorithm}', supported algorithms are \
             {CHECKSUM_ALGORITHM_SHA256}"
        )
    }

Repo side 4 — the ceiling the sibling check gets and this one does not. put_snapshot_block.rs:419-422:
    /// `Checksum` and `Progress` as MEMBER schemas carrying their modelled constraints, so
    /// the check is the framework's walk and the violation it reports — its kind, its
    /// lower-camelled path and its ceiling on how much of the rejected value is echoed — is
    /// the framework's too.

Test coverage — the existing suite only exercises values that already satisfy the dropped traits, so nothing pins the out-of-constraint case. services/service-com-amazonaws-ebs-ebs/tests/all/response_bodies_and_refusals.rs:395:
    let unsupported: &[&str] = &["MD5", "SHA1", "CRC32C", "sha256"];
(each ≤ 6 characters and inside `^[A-Za-z0-9]+$`), asserted at response_bodies_and_refusals.rs:420-434 against the same `Unsupported Checksum Algorithm '{algorithm}'…` sentence.

The sibling operation with the identical omission: services/service-com-amazonaws-ebs-ebs/src/ops/types/complete_snapshot_types.rs:32 `skip_validation = ["ChecksumAlgorithm"]` with complete_snapshot.rs:202-209 doing only the `!= CHECKSUM_ALGORITHM_SHA256` comparison.

## Steps to Reproduce

1. StartSnapshot with {"VolumeSize": 1} and keep the returned SnapshotId (it stays `pending`).
2. PUT /snapshots/{SnapshotId}/blocks/0 with a 524288-byte body, `x-amz-Data-Length: 524288`, a correct `x-amz-Checksum`, and `x-amz-Checksum-Algorithm` set to a 300-character value of `A`s.
3. Observe the 400 `ValidationException`: `Reason` is `INVALID_PARAMETER_VALUE` and `Message` is `Unsupported Checksum Algorithm '<300 A's>', supported algorithms are SHA256` — about 370 characters, while the model types that member as `ErrorMessage` with `@length 0..256`.
4. Repeat with `x-amz-Checksum-Algorithm: SHA 256` (a space violates the member's `@pattern ^[A-Za-z0-9]+$`): the same value refusal comes back, not the framework constraint report that `x-amz-Checksum: not+base64!` or `x-amz-Progress: 500` get from `header_constraint_refusal` — even though all three members are on the same `skip_validation` list.
5. Compare put_snapshot_block.rs:459 (`header_constraint_refusal(checksum, progress)`) with types/put_snapshot_block_types.rs:33 (`skip_validation = ["ChecksumAlgorithm", "Checksum", "Progress"]`): the third skipped member has no restated schema.
