Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: e62e0ae23225a1c6c2c24fb08c0dcaf4852934da

# CompleteSnapshot's Checksum and ChecksumAggregationMethod headers are refused through the boundary's synthetic envelope, so the refusal carries no Reason — unlike the same member on PutSnapshotBlock

_Filed: 2026-11-20T18:35:01Z_
_Operation: CompleteSnapshot | Class: error-response_

> _Note: this issue was identified against the code base at commit `e62e0ae23225a1c6c2c24fb08c0dcaf4852934da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CompleteSnapshot` carries three constrained header members — `ChecksumAlgorithm` (enum SHA256, len 0..32, pattern ^[A-Za-z0-9]+$), `Checksum` (len 0..64, pattern ^[A-Za-z0-9+/=]+$) and `ChecksumAggregationMethod` (enum LINEAR, len 0..32, pattern ^[A-Za-z0-9]+$) — but only `ChecksumAlgorithm` is taken off the request boundary (`skip_validation = ["ChecksumAlgorithm"]`) and judged by the handler so that the refusal is this service's own modelled `ValidationException` carrying `Message` AND `Reason`. The other two are left to the boundary, which this crate states twice carries NEITHER member.

So a `CompleteSnapshot` whose `x-amz-Checksum` is outside the modelled form of `com.amazonaws.ebs#Checksum` is answered with a refusal body a client cannot branch on, while the IDENTICAL member on the sibling `PutSnapshotBlock` — same shape, same constraints — is answered with the modelled shape carrying `Reason: INVALID_PARAMETER_VALUE`. The crate's own types files state the rule in the same words for both operations; `CompleteSnapshot`'s list simply omits the two members its handler then explicitly declines to judge (`let _ = (&input.checksum, &input.checksum_aggregation_method);`).

This is NOT the already-filed `CompleteSnapshot` parameter-ignored gap (that one is about never VERIFYING the aggregate checksum against the blocks the snapshot holds). This gap is about the CLASS and SHAPE of the refusal for a value that violates the member's own MODELLED form: `Reason` is absent from the response and the message travels in the synthetic envelope instead of the service's modelled `ValidationException`.

## Evidence

The rule, as the sibling operation's types file states it — services/service-com-amazonaws-ebs-ebs/src/ops/types/put_snapshot_block_types.rs:14-19 and :31-33:
"// `skip_validation` names the HEADER-bound members this operation judges itself, which is
// where the recorded refusals show the service wording them: an unsupported
// `ChecksumAlgorithm`, a `Checksum` outside its modelled form and a `Progress` outside its
// modelled range are all answered with the service's OWN modelled `ValidationException`,
// carrying the `Message` and the `Reason` a client reads, where the boundary's synthetic
// envelope carries neither."
…
"    skip_validation = [\"ChecksumAlgorithm\", \"Checksum\", \"Progress\"]"

The same rule, stated for THIS operation but applied to one member only — services/service-com-amazonaws-ebs-ebs/src/ops/types/complete_snapshot_types.rs:14-18 and :30-32:
"// `skip_validation` names the HEADER-bound `ChecksumAlgorithm`, which is where the
// recorded refusals show the service wording them: an algorithm it does not support is
// answered with the service's OWN modelled `ValidationException`, carrying the `Message`
// and the `Reason` a client reads, where the boundary's synthetic envelope carries
// neither."
…
"    skip_validation = [\"ChecksumAlgorithm\"]"
(the input struct right below it carries all three: `pub checksum: Option<String>`, `pub checksum_algorithm: Option<String>`, `pub checksum_aggregation_method: Option<String>` — complete_snapshot_types.rs:38-40)

The handler judging the one member it took off the boundary — services/service-com-amazonaws-ebs-ebs/src/ops/complete_snapshot.rs:196-209:
"        // `ChecksumAlgorithm` is checked HERE rather than at the boundary (the member
        // carries `skip_validation` on `CompleteSnapshotInput`): the service answers an
        // algorithm it does not support with its own modelled `ValidationException`,
        // which carries the `Reason` a client branches on and a message naming the
        // rejected algorithm and the supported set. The boundary's refusal would carry
        // neither.
        if let Some(algorithm) = input.checksum_algorithm.as_deref() {
            if algorithm != CHECKSUM_ALGORITHM_SHA256 {
                return Err(validation(
                    \"INVALID_PARAMETER_VALUE\",
                    unsupported_checksum_algorithm_message(algorithm),
                ));
            }
        }"

…and the two it leaves to the boundary — services/service-com-amazonaws-ebs-ebs/src/ops/complete_snapshot.rs:225-234:
"        // `ChecksumAggregationMethod` headers are NOT verified here. Their values are
        // constrained by the model (`LINEAR`) and the boundary validator already refuses
        // anything else, …
        let _ = (&input.checksum, &input.checksum_aggregation_method);"

The sibling that DOES judge `Checksum` itself, so its refusal carries the modelled members — services/service-com-amazonaws-ebs-ebs/src/ops/put_snapshot_block.rs:233-238:
"        // `Checksum` and `Progress`: the constraints are the MODEL's, and so is the
        // report — the same walk and the same sentence the boundary would have produced —
        // but it travels in the modelled error rather than in the synthetic envelope.
        if let Some(message) = header_constraint_refusal(&input.checksum, input.progress) {
            return Err(validation(\"INVALID_PARAMETER_VALUE\", message));
        }"

Test coverage: the PutSnapshotBlock side is pinned — services/service-com-amazonaws-ebs-ebs/tests/all/response_bodies_and_refusals.rs:481-510:
"    // The same rule covers the other header members the service judges: a `Checksum`
    // outside its modelled form and a `Progress` outside its modelled range are refused
    // through the modelled error too, each carrying a Message and a Reason."
…
"        member(&malformed_checksum, \"Reason\"),
        \"INVALID_PARAMETER_VALUE\","
No test in services/service-com-amazonaws-ebs-ebs/tests/ sends `x-amz-Checksum` or `x-amz-Checksum-Aggregation-Method` to `CompleteSnapshot` at all (tests/all/changed_blocks_count_decides_the_terminal_status.rs drives completion with `x-amz-ChangedBlocksCount` alone), so nothing pins the current refusal shape for those two members.

## Steps to Reproduce

1. POST /snapshots with {"VolumeSize": 1} and note the returned SnapshotId (HTTP 201).
2. PUT /snapshots/{SnapshotId}/blocks/0 with x-amz-Checksum: "not*a*checksum", x-amz-Checksum-Algorithm: SHA256, x-amz-Data-Length: 524288 and a 524288-byte payload. Observe HTTP 400 whose body carries Message "… at 'checksum' failed to satisfy constraint: …" AND Reason "INVALID_PARAMETER_VALUE" (put_snapshot_block.rs:236-238).
3. POST /snapshots/completion/{SnapshotId} with x-amz-ChangedBlocksCount: 0 and the SAME malformed value in x-amz-Checksum ("not*a*checksum" — it violates the modelled pattern ^[A-Za-z0-9+/=]+$ of com.amazonaws.ebs#Checksum).
4. Observe the refusal comes from the request boundary's synthetic envelope instead: the body carries no `Reason` member at all, because `Checksum` is absent from complete_snapshot_types.rs:32's skip_validation list and complete_snapshot.rs:234 discards the member without judging it.
5. Repeat step 3 with x-amz-Checksum-Aggregation-Method: "NOTLINEAR" for the same result: a refusal of a different shape from every other refusal this service words itself.
