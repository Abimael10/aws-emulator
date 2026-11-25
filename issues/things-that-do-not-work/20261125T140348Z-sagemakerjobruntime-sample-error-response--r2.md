Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 702410a05ac0654086191d2ccee343e5e0162f35

# The crate's own pre-dispatch ValidationException wrapper answers in front of the IAM gate, so an unauthorized caller is told about its request instead of being refused 403

_Filed: 2026-11-25T14:03:48Z_
_Operation: Sample | Class: error-response_

> _Note: this issue was identified against the code base at commit `702410a05ac0654086191d2ccee343e5e0162f35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate wires every one of its four operations through its own `InModelledEnvelope` wrapper (`src/service.rs`), which collects the modelled-constraint violations of a request and RETURNS a 400 `ValidationException` itself, before it ever calls the wrapped operation's `run`. The IAM decision, however, lives inside each handler's `handle` body (`crate::service::authorization::authorize`, called as the first statement of all four handlers). So for a request that breaks a modelled constraint, the wrapper short-circuits and `authorize` is never reached: a caller whose identity-based policies grant it nothing at all is answered `400 ValidationException` quoting the member it got wrong, instead of the `403 AccessDeniedException` the crate states is "the answer every one of the four operations gives such a caller".

That is the opposite order from the one this crate documents in two places (the authorization module's own rule, and `authorize`'s own instruction "Call it FIRST in a handler ... before the request's own values are checked"). It is not a framework limitation the crate cannot reach around: the pre-dispatch refusal is this crate's OWN code in `src/service.rs`, and the helper it skips (`authorization::authorize`) takes the resource as a plain `&str` (`resource: &str`) — the same `JobArn` the request carries in its `X-Amzn-SageMaker-Job-Arn` header — so the wrapper path is a path that skips a helper the crate already has and every sibling path calls.

Symptom set (one class, all four operations, since the wrapper wraps all four): a caller with no grant gets 400 + the constraint clause for a `TrajectoryId` outside `@length` 1..256, for a `JobArn` outside its `@pattern` (the "Input arn <value> is malformed" sentence), for an absent `@required` member, and for a `Status` outside the `CompletionStatus` enum — never the modelled 403.

## Evidence

The rule the crate states, `src/service/authorization.rs:3-16`:
```
//! Whether IAM lets a request to this data plane perform the action it asks for — the
//! authorization step every operation runs BEFORE it resolves the job or the trajectory
//! the request names.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what the
//! request addresses, so a caller whose identity-based policies do not grant the action is
//! told it is not authorized rather than told anything about the job it named, and the call
//! performs nothing. `AccessDeniedException` (HTTP 403, "You do not have permission to
//! perform this operation") is a SERVICE-COMMON error of this model ... so it is
//! the answer every one of the four operations gives such a caller
```
and the same rule on the helper itself, `src/service/authorization.rs:118-121`:
```
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the modelled refusal otherwise, which each operation returns as its
/// own `AccessDeniedException`. Call it FIRST in a handler, before the store is read or
/// written and before the request's own values are checked.
```

The code in the SAME crate that breaks it, `src/service.rs:461-469` (`impl ErasedOperation for InModelledEnvelope`):
```rust
        Box::pin(async move {
            // Read the protocol out before the await: ...
            let protocol: &dyn ServerHttpProtocol = svc.definition().protocol();
            let violations = refused_by::<O>(protocol, req);
            if let Some(refusal) = validation_exception(protocol, &violations) {
                return refusal;
            }
            self.operation.run(svc, req, ctx).await
        })
```
`validation_exception` is this service's 400 `ValidationException` (`src/service.rs:513-528`), and the `return refusal` happens before `self.operation.run(...)` — the only path on which the gate runs.

The gate is reached only from inside the handlers, e.g. `src/ops/sample.rs:164-169`:
```rust
        // IAM decides authorization ahead of this data plane resolving what the request
        // names, so a caller whose policies do not allow `sagemaker:Sample` on the job it
        // addressed is told it is not authorized rather than told whether that job exists
        crate::service::authorization::authorize(&*self.engine, ctx, Self::NAME, &input.job_arn)
            .map_err(SampleErrorModeled::AccessDeniedException)?;
```
and identically at `src/ops/sample_with_response_stream.rs:169-170`, `src/ops/complete_rollout.rs:235-236`, `src/ops/update_reward.rs:211-212` (whose comment adds "IAM decides authorization before the trajectory is resolved and before this operation's own request checks run").

Test coverage: the two sides are each pinned, but never together, so nothing catches the ordering. `tests/all/iam_authorization.rs:536` pins `(403, "AccessDeniedException")` for an unauthorized caller — every row there sends a WELL-FORMED request (`grep -n 'malformed|repeat\(' tests/all/iam_authorization.rs` → no matches). `tests/all/modelled_rejections.rs:444-499` pins the 400 for a constraint-breaking request — driven through `mounted_service()`, i.e. with the ambient account credential the gate does not bound (`src/service/authorization.rs:70-74`), so those rows stay 400 under either ordering and a fix does not change them.

## Steps to Reproduce

1. Mount the service the way tests/all/iam_authorization.rs does (`router_with_identity()`, `given_session(&engine)`) and attach a role policy that grants nothing — `given_role_policy(&engine, None)` — so the caller falls to IAM's default deny.
2. Create the job (`create_job(&engine, "AgentRFT", "run-1")`) and sign a `POST /sample` with that session's access key id, carrying `X-Amzn-SageMaker-Job-Arn: arn:aws:sagemaker:us-east-1:000000000000:job/AgentRFT/run-1` and `X-Amzn-SageMaker-Trajectory-Id` set to "t".repeat(257) (one past the modelled `@length` max 256).
3. Observe the answer: `400 ValidationException` whose Message states "at 'trajectoryId' failed to satisfy constraint: Member must have length less than or equal to 256" — `crate::service::authorization::authorize` never ran, because `src/service.rs:466-468` returned before `self.operation.run`.
4. Repeat the identical request with a 1-character `X-Amzn-SageMaker-Trajectory-Id`: now the handler runs and the same unauthorized caller is answered `403 AccessDeniedException` (tests/all/iam_authorization.rs:536), showing the two orderings disagree on the same caller.
5. Repeat steps 2-3 against `/complete-rollout` and `/update-reward` with a malformed `X-Amzn-SageMaker-Job-Arn` (e.g. `not-a-job-arn`): the answer is this service's "Input arn not-a-job-arn is malformed" 400 rather than the modelled 403.
