Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: 039d38dc4c02de0c6d595d33e91f7f516f07aa43

# GetWebFunctionRevision never resolves the documented revision-ARN form, so a read by revisionArn answers 404

_Filed: 2026-11-23T00:27:40Z_
_Operation: GetWebFunctionRevision | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `039d38dc4c02de0c6d595d33e91f7f516f07aa43`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model documents that `GetWebFunctionRevisionRequest$revisionId` may be given either the revision identifier OR the revision ARN. The handler looks the value up in the store verbatim (`function.revisions.get(&input.revision_id)`), and the store is keyed by the bare `revisionId`, so a request that names the revision by its ARN — the exact `revisionArn` this service's own `CreateWebFunctionRevision` / `GetWebFunctionRevision` responses hand back — finds nothing and is refused 404 `ResourceNotFoundException`.

This crate already owns the resolution helper for exactly this member (`crate::state::resolve_revision_id`, whose doc cites the same "The revision identifier. The revision ARN." sentence), and the sibling handler `DeleteWebFunctionRevision` calls it on the very same input member. The same handler also resolves the FUNCTION by ARN (`resolve_function_name`), so inside one handler one identifier honours the ARN form and the other does not.

## Evidence

Smithy (smithy_query doc, GetWebFunctionRevisionRequest$revisionId): "<p>The revision to retrieve.</p> <p>You can specify either of the following:</p> <ul> <li> <p>The revision identifier.</p> </li> <li> <p>The revision ARN.</p> </li> </ul> <p>The length constraint applies only to the full ARN.</p>"

Handler, no resolution — services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/get_web_function_revision.rs:187-198:
```
        let name = crate::state::resolve_function_name(&input.function_name);
        let Some(function) = state.functions.get(name) else { ... };
        let Some(revision) = function.revisions.get(&input.revision_id) else {
```
(the function is ARN-resolved on :187, the revision is not on :198)

The helper this crate already has — services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/revisions.rs:18-32:
```
/// The `revisionId` a request names, resolved to the key the store holds.
/// The member's documentation says the caller "can specify either of the following: The
/// revision identifier. The revision ARN." ...
pub fn resolve_revision_id(value: &str) -> &str {
```

Sibling that calls it — services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/delete_web_function_revision.rs:229-232:
```
        // Either resource may be named by name or by ARN; both resolve to the one stored
        // record.
        let function_name = crate::state::resolve_function_name(&input.function_name).to_string();
        let revision_id = crate::state::resolve_revision_id(&input.revision_id).to_string();
```

Test coverage: the delete path's ARN form is exercised (tests/all/revision_build_outcome.rs:1194-1202 picks `newest_arn` when `by_arn`, percent-encoded by `delete_revision` at :1086-1098); the GET helper `read_revision` (tests/all/revision_build_outcome.rs:1103-1116) is never driven with an ARN of a revision that still exists, so nothing pins the current 404.

## Steps to Reproduce

1. POST /2025-03-07/web-functions/{functionName}/revisions to create a revision; note `revisionArn` in the 202 response (e.g. arn:aws:lambda:us-east-1:<acct>:web-function/my-fn/revision/<revisionId>)
2. GET /2025-03-07/web-functions/my-fn/revisions/<percent-encoded revisionArn>
3. Observe 404 ResourceNotFoundException ("The revision <arn> of web function my-fn doesn't exist.") instead of the revision
4. GET the same revision by its bare `revisionId` — 200, proving the record is stored and only the documented ARN form is mis-resolved
5. DELETE /2025-03-07/web-functions/my-fn/revisions/<percent-encoded revisionArn> — 204, proving the sibling handler accepts the form this reader rejects
