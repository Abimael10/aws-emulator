Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appsync.access-denied-sandbox-eval
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync.access-denied-sandbox-eval|_poll#17|AssertionError|raise AssertionError(
Issue-Commit: 65a2c27ae16c291e3090e92ad3719dd0862a9e67

# AppSync EvaluateMappingTemplate renders no Velocity template, so the sandbox's template half answers the unmodeled NotImplemented where AWS answers 200 with an evaluationResult (1 cause)

> _Note: this report carries the replay key of the `appsync.access-denied-sandbox-eval`
> check, which is the remaining open cause of
> `issues/things-that-now-work/20261107T183746Z-appsync-authorization-gaps-domain-name-and-flushapicache-calls.md`
> (its other two causes — the domain-name surface's and `FlushApiCache`'s identity-based
> gates — are closed, and the authorization gate that report asks for in front of BOTH
> sandbox evaluations is in place and pinned by
> `services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/tests/all/sandbox_evaluation_authorization.rs`).
> It is filed separately rather than left inside that bundle so the key keeps being replayed
> against the one thing still missing: an evaluator for the template language._

## Operation
EvaluateMappingTemplate

## Description
AWS: Smithy (appsync) EvaluateMappingTemplate — POST /v1/dataplane-evaluatetemplate, required
template (@length 2-65536) and context (@length 2-28000); output
evaluationResult/error/logs/stash/outErrors; modeled errors AccessDeniedException|403,
BadRequestException|400, InternalFailureException|500. "Evaluates a given template and returns
the response": a caller whose identity policy allows appsync:EvaluateMappingTemplate on
Resource '*' gets 200 carrying the RENDERED template as `evaluationResult` — for
template='$util.toJson($context)' with context='{}' that is the JSON of the context the
request supplied — and the identical call is refused 403 AccessDeniedException once the grant
is removed.

Emulator: `src/ops/evaluate_mapping_template.rs` is still the scaffold — it answers HTTP 501
with the error code `NotImplemented` ('operation EvaluateMappingTemplate is scaffolded but not
implemented in this emulator'), a code AppSync does not model at all, for every caller the
authorization gate lets through. The gate itself is correct: an unauthorized caller is refused
403 AccessDeniedException with IAM's sentence before the scaffold is reached, and a caller the
documents allow reaches the operation — where the 501 is what it meets. The check therefore
polls for the 200 its granted principal must get, never observes it, and is killed in its
poll; the deny/re-grant assertions after it never run.

Why: resolver-development tooling (the AppSync console sandbox, `aws appsync
evaluate-mapping-template`, CI checks that lint resolver templates) gets an unmodeled error its
error handling cannot classify, and the emulator's whole VTL surface — the
`requestMappingTemplate`/`responseMappingTemplate` of a resolver or a function, which the
GraphQL data plane also cannot evaluate
(`src/service/graphql_api/execution.rs`, "no component of this emulator evaluates a Velocity
template") — stays unverifiable.

Fix: evaluate the template for real. The call's answer is whatever the caller's own template
renders over the caller's own context, so nothing short of an evaluator for the language
produces it: rendering one recorded template, matching on the template's text, or answering a
different refusal would each be a fabricated success.

## Blocker found while fixing the bundle this cause came from
The language is Apache Velocity (VTL) plus AppSync's own `$util` helper library and
`$context`/`$ctx` bindings, and no maintained Rust crate evaluates it. Checked against the
registry with `cargo info`:

* `velocity` 0.1.2 — a TUI library, unrelated;
* `vtl-rs` 0.0.3 — an interpreter for the historical "Very Tiny Language" of the Altair 680b,
  unrelated, and GPL-3.0-only;
* `velocity_template` 0.0.2 — the only candidate, a `regex` + `evalexpr` "velocity-similar"
  engine at version 0.0.x which implements neither Velocity's directive set nor method calls on
  context objects, so it cannot render `$util.toJson($context)`;
* `vtl`, `vtl-parser`, `velocity-parser`, `velocity-rs`, `velocity_engine`,
  `velocity-template-engine`, `apache-velocity`, `nvelocity`, `appsync-vtl` — not in the
  registry.

The crate this service already embeds for the other half of the sandbox (`rquickjs`, which runs
`EvaluateCode`'s APPSYNC_JS) runs JavaScript, not VTL. Implementing this cause therefore needs
either a maintained Velocity engine crate to exist (or be vendored by the platform), or an
explicit decision that this one template language may be interpreted by hand in-tree — which
is what the execution and code-review rules otherwise forbid, since a parser for an expression
language written to satisfy a fixture is where unbounded recursion and unbounded allocation
live. Whoever takes this on needs that decision first; if it is "write it", the evaluator wants
its own module beside `ops/evaluate_code/appsync_js.rs`, with the template-size, nesting,
iteration and output bounds that module's engine already holds itself to, and tests feeding it
malformed, truncated and oversized templates.

The authorization half of the original bundle's cause 3 needs nothing further: both evaluations
are judged against `arn:aws:appsync:<region>:<account>:*` before the operation runs, including
while this one is a scaffold.

## Evidence
Expected a 200 EvaluateMappingTemplate response carrying an `evaluationResult` while the user
policy grants appsync:EvaluateMappingTemplate on '*'. Actual: HTTP 501, error code
`NotImplemented`, message 'operation EvaluateMappingTemplate is scaffolded but not implemented
in this emulator' — see the `//! SCAFFOLDED, NOT IMPLEMENTED:` header of
`services/service-com-amazonaws-appsync-awsdeepdishcontrolplaneservice/src/ops/evaluate_mapping_template.rs`.
`EvaluateCode`, the other half of the same sandbox, is implemented and answers 200 for the same
principal, which is why the check gets past its first poll and stops at this one.

Related: `issues/things-that-do-not-work/20261029T174359Z-appsync-evaluatemappingtemplate-stub.md`
records the same scaffold without carrying this check's replay key.

## Steps to Reproduce
iam.CreateUser + CreateAccessKey, then PutUserPolicy granting Allow
['appsync:EvaluateCode','appsync:EvaluateMappingTemplate'] on '*'. As that principal call
EvaluateMappingTemplate(template='$util.toJson($context)', context='{}') -> must be 200 with an
`evaluationResult` that is the rendered template; observe 501 `NotImplemented`. Vary the
template over the language rather than over that one line — a reference with a property
(`$context.arguments.id`), a `#set` with an `#if`/`#else`, a `#foreach` over a list in the
context, and a template whose syntax is broken (which must be 400 BadRequestException naming
the template error). DeleteUserPolicy and repeat -> must be 403 AccessDeniedException with
'User: <user arn> is not authorized to perform: appsync:EvaluateMappingTemplate on resource:
arn:aws:appsync:<region>:<account>:* because no identity-based policy allows the
appsync:EvaluateMappingTemplate action' (this part already holds). Re-attach the identical
policy and repeat -> must return 200 again.
