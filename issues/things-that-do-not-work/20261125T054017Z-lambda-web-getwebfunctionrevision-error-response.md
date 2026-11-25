Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 24df4b5bb34709ed7c873e3f6d0f18f4590f38dd

# The revision-not-found sentence names the function by the caller's raw identifier here and by the stored name on every sibling path

_Filed: 2026-11-25T05:40:17Z_
_Operation: GetWebFunctionRevision | Class: error-response_

> _Note: this issue was identified against the code base at commit `24df4b5bb34709ed7c873e3f6d0f18f4590f38dd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::revision_not_found_message(identifier, function_name)` is the ONE sentence this crate reports an absent revision with, and its own documentation says the wording cannot drift between the branches that use it. It does drift: `GetWebFunctionRevision` passes the request's RAW `functionName` (which the member documents may be a function ARN) as the `function_name` argument, while the endpoint weight-rule paths pass the STORED `function.function_name`, and the sibling sub-resource reader `GetWebFunctionEndpoint` passes the stored name too.

So a caller that addresses the function by its ARN and names a revision the function does not carry is told "The revision <id> of web function arn:aws:lambda:us-east-1:000000000000:web-function/f doesn't exist.", while the same absent revision named in `revisionWeights` on `CreateWebFunctionEndpoint`/`UpdateWebFunctionEndpoint` is reported as "... of web function f doesn't exist." — two renderings of the one sentence the helper exists to word once. The crate deliberately echoes the caller's identifier only for the FUNCTION-absent sentence (`function_not_found_message`, whose doc states that choice); the revision sentence's second argument is documented as "the function it was looked for under", i.e. the store key.

## Evidence

The helper and the rule it states, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/revisions.rs:34-44:
```
/// The `ResourceNotFoundException` message for a revision a function does not carry,
/// naming it by `identifier` — the value the request addressed it by, verbatim — and
/// naming the function it was looked for under.
///
/// ... One function serves
/// every branch that reports an absent revision, so the wording cannot drift between the
/// readers, the delete and the endpoint weight rule.
pub fn revision_not_found_message(identifier: &str, function_name: &str) -> String {
    format!("The revision {identifier} of web function {function_name} doesn't exist.")
}
```

This handler passes the RAW request identifier, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/get_web_function_revision.rs:198-204:
```
        let Some(revision) = function.revisions.get(&input.revision_id) else {
            return Err(GetWebFunctionRevisionErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: crate::state::revision_not_found_message(
                        &input.revision_id,
                        &input.function_name,
                    ),
```

The weight-rule path passes the STORED name, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoints.rs:364-371:
```
impl RevisionWeightProblem {
    /// The refusal's message, worded once here so every operation that applies the rule
    /// reports it identically.
    pub fn message(&self, function_name: &str) -> String {
        match self {
            Self::UnknownRevision { revision_id } => {
                super::revision_not_found_message(revision_id, function_name)
```
called as `problem.message(&function.function_name)` at services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_endpoint.rs:325 and services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/update_web_function_endpoint.rs:275.

The sibling sub-resource reader does it the other way, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/get_web_function_endpoint.rs:226-231:
```
                GetWebFunctionEndpointErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: crate::state::endpoint_not_found_message(
                            &input.endpoint_name,
                            &function.function_name,
                        ),
```

Test coverage: tests/all/absent_function_message.rs pins only the FUNCTION-absent sentence (`absent_function_message`, which does echo the identifier verbatim, by design); no test asserts the revision-absent sentence for a function addressed by its ARN, so nothing pins the current rendering.

## Steps to Reproduce

1. PUT /2025-03-07/web-functions with functionName "f" and a revisionConfig, so a function and one revision are stored
2. GET /2025-03-07/web-functions/arn%3Aaws%3Alambda%3Aus-east-1%3A000000000000%3Aweb-function%2Ff/revisions/00000000-0000-0000-0000-000000000000 (the function addressed by its ARN, a revisionId it does not carry)
3. Observe 404 with message "The revision 00000000-0000-0000-0000-000000000000 of web function arn:aws:lambda:us-east-1:000000000000:web-function/f doesn't exist."
4. PATCH /2025-03-07/web-functions/<same ARN>/endpoints/<endpoint> with revisionWeights naming the same unknown revisionId
5. Observe 404 with the same sentence but the function rendered as "f" — the one helper, two renderings
