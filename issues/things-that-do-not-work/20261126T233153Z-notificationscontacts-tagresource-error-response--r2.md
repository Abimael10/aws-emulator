Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: e0fb8928b55d063818ff45281a7e96a54cacfd74

# TagResource and UntagResource module docs claim the not-found refusal is "the modelled 404", which their own handlers contradict

_Filed: 2026-11-26T23:31:53Z_
_Operation: TagResource | Class: error-response_

> _Note: this issue was identified against the code base at commit `e0fb8928b55d063818ff45281a7e96a54cacfd74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler-level module docs of both tag mutations state that an ARN naming no stored contact is answered with the MODELLED 404 (`ResourceNotFoundException`). The code directly under those docs does the opposite: both call `crate::state::tagged_resource_not_found()`, which deliberately answers the tag plane's UNMODELLED `NotFoundException` through `Scaffolded::unmodeled`, and `crate::state` says in so many words that this is NOT the modelled shape.

The defect is the stale doc sentence, not the response: the unmodelled `NotFoundException` is pinned by this crate's own tests and by the resolution of a trace-based issue, so the RESPONSE MUST NOT BE CHANGED. What should change is the two header sentences, so a reader of the handler is not told the opposite of what the handler does (and so a future fixer does not "restore" the modelled shape on the strength of the doc). `ListTagsForResource`'s own header doc, by contrast, makes no such claim — only the two mutations drifted.

## Evidence

Doc side — src/ops/tag_resource.rs:9-11:
```
//! both survive. A well-formed ARN naming no stored contact is the modelled 404, and
//! the stored set is kept inside the modelled `TagMap` bound, because
//! `ListTagsForResource` reports it through a member carrying that same bound.
```
and src/ops/untag_resource.rs:9:
```
//! its value. A well-formed ARN naming no stored contact is the modelled 404.
```

Code side — src/ops/tag_resource.rs:178-183 (identical shape at src/ops/untag_resource.rs:176-181):
```
// An ARN that resolves to no resource is refused by the shared tagging frontend in
// front of this service, under ITS error shape rather than this operation's modelled
// `ResourceNotFoundException` (see `crate::state::tagged_resource_not_found`).
let contact = store
    .contact_mut(&input.arn)
    .ok_or_else(crate::state::tagged_resource_not_found)?;
```
and src/state.rs:669-675:
```
/// It is NOT the `ResourceNotFoundException` the rest of this service answers and the model
/// declares for those three operations: ...
pub const TAGGING_NOT_FOUND_CODE: &str = "NotFoundException";
```

The response is pinned, so only the doc can move: tests/all/resource_tags.rs:33-38 ("The error SHAPE the ARN-addressed tag operations answer for an ARN that names no resource — the shared tagging frontend's own, which is NOT the `ResourceNotFoundException` this model declares for them ... pub(crate) const TAGGING_NOT_FOUND_CODE: &str = \"NotFoundException\";") is asserted by tests/all/resource_tags.rs:248-255, tests/all/resource_tag_writes.rs:446-453, tests/all/tag_mutation_success_status.rs:419-426, tests/all/tag_key_constraints.rs:410-418, tests/all/untag_resource_tag_keys.rs:417-425 and tests/all/refusal_wording.rs:544-560; src/service/error_type_locator.rs:9-14 shows the same `NotFoundException` on the wire.

## Steps to Reproduce

1. Read src/ops/tag_resource.rs lines 3-18 — the header says a well-formed ARN naming no stored contact "is the modelled 404".
2. Read the same file's handler body, lines 177-183 — it answers `crate::state::tagged_resource_not_found()`, documented at src/state.rs:666-701 as explicitly NOT the modelled `ResourceNotFoundException` but the tag plane's `NotFoundException` (src/state.rs:675).
3. Repeat for src/ops/untag_resource.rs lines 9 and 176-181 — the same contradiction.
4. Confirm which side is authoritative: tests/all/resource_tags.rs:38 fixes the answered code to `NotFoundException` and five suites assert it, so the two header sentences are the stale side and are what needs correcting; the response must stay as it is.
