Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 4d1f355f3d73bc4e7320a2117fe933e2319f32c2

# An INVALID_BLOCK_TOKEN refusal echoes the whole blockToken, so the Message can exceed the modelled ErrorMessage @length 0..256

_Filed: 2026-11-23T04:04:40Z_
_Operation: GetSnapshotBlock | Class: error-response_

> _Note: this issue was identified against the code base at commit `4d1f355f3d73bc4e7320a2117fe933e2319f32c2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetSnapshotBlock` enforces the `BlockToken` `@length` 0..256 itself (the member is on `skip_validation`), so a 256-character token that satisfies the enforced pattern passes the constraint walk and then reaches the token-resolution refusal, which interpolates the ENTIRE token verbatim into the `Message`: `"The block token " (16 chars) + <256 chars> + " is invalid." (12 chars)` = 284 characters. `ValidationException$Message` targets `com.amazonaws.ebs#ErrorMessage`, which the model constrains to `@length` 0..256, so the service answers a modelled member with a value 28 characters outside its own declared maximum.

This is the same omission the crate avoids one screen earlier: for the CONSTRAINT report the handler deliberately routes through the framework's walk precisely because that walk has "its ceiling on how much of the rejected value is echoed" — but the `INVALID_BLOCK_TOKEN` sentence beside it has no ceiling at all and echoes a value whose only bound (256) is already larger than the message member that carries it.

This is NOT the already-FIXED `GetSnapshotBlock` error-response gap (that one was about the refusal travelling in the boundary's synthetic envelope with no `Reason`; the refusal now correctly carries `Reason: INVALID_BLOCK_TOKEN`). This gap is the LENGTH of the `Message` that refusal carries.

## Evidence

Smithy (contract) — `smithy_query(query="shape",shape="com.amazonaws.ebs#ValidationException")`:
    "Message": {"target": "ErrorMessage", "length": {"min": 0, "max": 256}}
and the token member it echoes, `smithy_query(query="operation",operation="GetSnapshotBlock")`:
    "BlockToken": {"target": "BlockToken", "httpQuery": "blockToken", "required": true, "length": {"min": 0, "max": 256}, "pattern": "^[A-Za-z0-9+/=]+$"}

Repo side 1 — the handler admits a token up to 256 characters. services/service-com-amazonaws-ebs-ebs/src/state.rs:51-53:
    /// The modelled `BlockToken` `@length`, which `skip_validation` takes off the boundary
    /// along with the pattern, so `crate::ops::get_snapshot_block` enforces it too.
    pub const BLOCK_TOKEN_LENGTH: (i64, i64) = (0, 256);
enforced at services/service-com-amazonaws-ebs-ebs/src/ops/get_snapshot_block.rs:279-286:
    static BLOCK_TOKEN_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
        let mut traits = TraitMap::new();
        traits.insert(Box::new(PatternTrait::new(BLOCK_TOKEN_PATTERN)));
        traits.insert(Box::new(LengthTrait::new(
            Some(BLOCK_TOKEN_LENGTH.0),
            Some(BLOCK_TOKEN_LENGTH.1),
        )));

Repo side 2 — the unbounded echo. services/service-com-amazonaws-ebs-ebs/src/ops/get_snapshot_block.rs:254-259:
            let Some(entry) = resolved else {
                return Err(validation(
                    "INVALID_BLOCK_TOKEN",
                    invalid_block_token_message(&input.block_token),
                ));
            };
and services/service-com-amazonaws-ebs-ebs/src/state.rs:678-680:
    pub fn invalid_block_token_message(block_token: &str) -> String {
        format!("The block token {block_token} is invalid.")
    }

Repo side 3 — the ceiling the handler relies on for its OTHER refusal of the same member, stated in its own words. get_snapshot_block.rs:289-293:
    /// `blockToken` as a MEMBER schema carrying [`BLOCK_TOKEN_TRAITS`], so the check itself
    /// is the framework's walk rather than this module's and the violation it reports — its
    /// kind, its path and its ceiling on how much of the rejected value is echoed — is the
    /// framework's too.

Test coverage — the suite only ever sends tokens that are short (a 64-character minted token) or one character PAST the maximum (which is refused by the constraint walk, not by this sentence), so nothing pins the 256-character case. services/service-com-amazonaws-ebs-ebs/tests/all/response_bodies_and_refusals.rs:1398:
    let over_long_token = "A".repeat(BLOCK_TOKEN_MAX_LENGTH + 1);
and the sentence as pinned for a short token, services/service-com-amazonaws-ebs-ebs/tests/all/response_bodies_and_refusals.rs:394:
                format!("The block token {token} is invalid."),
(also tests/all/refusal_order.rs:421 and tests/all/read_path_requires_a_completed_snapshot.rs:272, both with minted 64-character tokens).

## Steps to Reproduce

1. POST /snapshots with {"VolumeSize": 1}, write one 524288-byte block with PutSnapshotBlock, then POST /snapshots/completion/{SnapshotId} with x-amz-ChangedBlocksCount: 1 so the snapshot is readable.
2. GET /snapshots/{SnapshotId}/blocks/0?blockToken=AAAA…A with exactly 256 'A' characters (inside the enforced ^[A-Za-z0-9+/=_-]+$ and inside BLOCK_TOKEN_LENGTH, so block_token_refusal returns None).
3. Observe HTTP 400 ValidationException with Reason INVALID_BLOCK_TOKEN and a Message of 284 characters: 'The block token <256 A's> is invalid.'
4. Compare with smithy_query(query="shape",shape="com.amazonaws.ebs#ErrorMessage"), which declares @length 0..256 for the member that Message targets.
