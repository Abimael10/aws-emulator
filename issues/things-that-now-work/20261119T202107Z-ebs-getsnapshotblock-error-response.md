Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 1be784fbae94098fd005b87b2fe1a5254865c868

# A blockToken that fails the enforced constraints is refused through the synthetic ValidationException, so the refusal carries no Reason and spells the message member differently from every other refusal of this service

_Filed: 2026-11-19T20:21:07Z_
_Operation: GetSnapshotBlock | Class: error-response_

> _Note: this issue was identified against the code base at commit `1be784fbae94098fd005b87b2fe1a5254865c868`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetSnapshotBlock` takes `BlockToken`'s constraints off the request boundary (`skip_validation = ["BlockToken"]`) and judges them in the handler. When the token fails, the handler returns `crate::scaffolded::Scaffolded::validation(message)` — the SYNTHETIC `smithy.framework#ValidationException`, whose only member is a lowercase `message` — instead of this service's own modelled `ValidationException`, which carries `Message` and `Reason`.

That is the opposite of what the crate does for the identical situation everywhere else, and the opposite of what the crate's own error channel tells a handler to do:

* `PutSnapshotBlock` also takes header members off the boundary with `skip_validation` and also builds the refusal text with the SAME framework helper (`ValidatingSerializer` + `aws_smithy_schema_server::envelope`), but it returns the MODELLED error (`validation("INVALID_PARAMETER_VALUE", message)`), stating in a comment that the synthetic envelope carries neither of the two members a client reads.
* `CompleteSnapshot` does the same for its `skip_validation` `ChecksumAlgorithm`.
* Every other refusal in `get_snapshot_block.rs` itself (malformed snapshot id, absent snapshot, unresolvable token) goes through the modelled shapes.

The consequence a client sees: a 400 for a malformed/over-long `blockToken` has no `Reason` member at all (so a client branching on `ValidationExceptionReason` — the branch this crate sets for every other validation refusal — gets nothing), and the sentence arrives under `message` rather than `Message`. The message TEXT is not in question: the framework envelope string the handler already computes can travel inside the modelled error exactly as `PutSnapshotBlock` does with its own header-constraint report.

## Evidence

The defective path — services/service-com-amazonaws-ebs-ebs/src/ops/get_snapshot_block.rs:198-206:
"        // `BlockToken` is judged HERE rather than at the boundary (the member carries
        // `skip_validation` on `GetSnapshotBlockInput`) ...
        if let Some(message) = block_token_refusal(&input.block_token) {
            return Err(crate::scaffolded::Scaffolded::validation(message));
        }"
while the same file keeps a modelled constructor it uses for every other refusal — get_snapshot_block.rs:327-335:
"/// The modelled `ValidationException` (HTTP 400) with one of `ValidationExceptionReason`'s
/// values.
fn validation(reason: &str, message: String) -> GetSnapshotBlockError {
    GetSnapshotBlockErrorModeled::ValidationException(ValidationException {
        message: Some(message),
        reason: Some(reason.to_string()),
    })"

The sibling that does it the other way, for the same class of member — services/service-com-amazonaws-ebs-ebs/src/ops/put_snapshot_block.rs:217-238:
"        // The HEADER-bound members this operation takes off the boundary are judged
        // first ... Every refusal below is the service's own modelled `ValidationException`, so
        // each carries the `Message` and the `Reason` a client reads — the boundary's
        // synthetic envelope carries neither.
        ...
        // `Checksum` and `Progress`: the constraints are the MODEL's, and so is the
        // report — the same walk and the same sentence the boundary would have produced —
        // but it travels in the modelled error rather than in the synthetic envelope.
        if let Some(message) = header_constraint_refusal(&input.checksum, input.progress) {
            return Err(validation("INVALID_PARAMETER_VALUE", message));
        }"
Both refusal texts come from the same helper: put_snapshot_block.rs:459-467 and get_snapshot_block.rs:309-314 each call `ValidatingSerializer` + `::aws_smithy_schema_server::envelope(...)`.

The rule this crate's own error channel states — services/service-com-amazonaws-ebs-ebs/src/scaffolded.rs:290-295:
"    /// If this service's model declares its own `ValidationException` (or another
    /// client-input error) use THAT: the modeled shape may carry members beyond
    /// `message` — a `reason`, a field list — and a real client reads them. When the
    /// operation already lists it, it is a plain variant on the generated error enum;"
and `GetSnapshotBlock` does list it (OPERATIONS.md:38 lists `ValidationException (HTTP 400)` for the operation; get_snapshot_block.rs:128 has the variant).

The divergence is visible in one test file: services/service-com-amazonaws-ebs-ebs/tests/all/response_bodies_and_refusals.rs:299 reads the blockToken refusal as lowercase `message` ("let message = member(&reply, \"message\");") and asserts nothing about `Reason`, while lines 505-510 assert the PutSnapshotBlock header refusal carries both:
"    assert_eq!(
        member(&malformed_checksum, \"Reason\"),
        \"INVALID_PARAMETER_VALUE\",
        \"case \\\"a Checksum outside its modelled form\\\": the refusal must carry a Reason, got {}\","
RULE 3 of that file (lines 245-249) pins only the expression and the report text ("refused with the framework's constraint report naming the member and THAT expression"), giving no reason for the envelope, and state.rs:38-49 records only the live message SENTENCE for `BLOCK_TOKEN_PATTERN`, not an envelope without `Reason`.

## Steps to Reproduce

1. Start a snapshot, write one block and complete it, then list its blocks to get a valid snapshot id.
2. GET /snapshots/{snapshot}/blocks/0?blockToken= (empty token): HTTP 400 with a body carrying only lowercase `message` ('1 validation error detected: Value \'\' at \'blockToken\' failed to satisfy constraint: Member must satisfy regular expression pattern: ^[A-Za-z0-9+/=_-]+$') and NO `Reason`.
3. GET the same block with a syntactically valid token that names no block: HTTP 400 with `Message` and `Reason` = INVALID_BLOCK_TOKEN (the modelled shape) — the same operation, two different envelopes.
4. PUT /snapshots/{snapshot}/blocks/0 with x-amz-Checksum: not*a*checksum (the sibling operation's own skip_validation header member): HTTP 400 with `Message` ('... at \'checksum\' failed to satisfy constraint ...') AND `Reason` = INVALID_PARAMETER_VALUE.
5. Compare get_snapshot_block.rs:204-206 with put_snapshot_block.rs:236-238: the same framework report, one wrapped in the synthetic shape and one in the modelled shape.
