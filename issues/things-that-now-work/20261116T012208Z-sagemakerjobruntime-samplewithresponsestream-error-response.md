Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: f22ccf27ef051903376e0d46dbb4d489e16f4aae

# The AccessDeniedException message is composed with no bound, so a 403 can overrun the 1024-byte maximum its own Message member declares

_Filed: 2026-11-16T01:22:08Z_
_Operation: SampleWithResponseStream | Class: error-response_

> _Note: this issue was identified against the code base at commit `f22ccf27ef051903376e0d46dbb4d489e16f4aae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states — and enforces on its constraint-refusal path — the rule that every refusal it answers must fit the `Message` member that carries it (`FailureReason`, `@length` max 1024), and its own comment names `AccessDeniedException` as one of the shapes that rule covers. `crate::service::refusal_message` implements the bound (it re-words the clauses when the sentence passes `LARGEST_MESSAGE_BYTES`). The authorization gate every one of the four handlers calls first does not: `crate::service::authorization::not_authorized` builds the `AccessDeniedException` prose with `format!` from the principal ARN read out of the IAM store, the action, the request's `JobArn` and the reason, and hands it back unbounded. Nothing downstream holds it either — the framework validates the INPUT before `handle` runs, which is exactly why this crate had to implement the 1024 bound itself for `ValidationException`. With the pieces each at the maximum its own model admits, the sentence is 1033 bytes: 52 bytes of fixed prose + `sagemaker:SampleWithResponseStream` (34) + a `JobArn` at its modelled 256 maximum + the 85-byte "because no identity-based policy allows the <action> action" reason + an IAM user ARN of 606 bytes (`arn:aws:iam::000000000000:user` = 30, plus `Path` at its modelled 512 maximum and `UserName` at its modelled 64 maximum). So the emulator answers a 403 whose `Message` is longer than the shape the model declares can hold, on the one path of this service that is not held to the bound the crate says applies to it. (Every other handler-authored sentence of this crate is bounded by construction: they interpolate only `TrajectoryId`/`JobArn`, both `@length` max 256, into fixed prose.)

## Evidence

The model (smithy_query query="shape" shape="AccessDeniedException"): `{"AccessDeniedException": {"httpError": 403, "members": {"Message": {"target": "FailureReason", "required": true, "length": {"min": 0, "max": 1024}}}}}` — the same member and the same maximum `ValidationException` declares.

The rule, stated by this crate, services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/service.rs:92-101:
```
/// The greatest number of bytes of prose THIS SERVICE's errors can carry, read off the
/// model: every error shape it declares (`AccessDeniedException`, `ConflictException`,
/// `InternalServiceError`, `ResourceNotFoundException`, `ServiceQuotaExceededException`,
/// `ThrottlingException`, `ValidationException`) has exactly one member, `Message`,
/// targeting `FailureReason`, whose `@length` is min 0 **max 1024**
...
/// It is the member that has to HOLD a refusal, so it is the bound on what a refusal may
/// say — not a property of any member a request carries.
const LARGEST_MESSAGE_BYTES: usize = 1024;
```

Enforced on the constraint path, src/service.rs:236-240:
```
    let sentence = worded(violations)?;
    if sentence.len() <= LARGEST_MESSAGE_BYTES {
        return Some(sentence);
    }
    held_within_the_message_maximum(violations, &sentence).or(Some(sentence))
```

Not enforced on the authorization path, src/service/authorization.rs:198-212:
```
fn not_authorized(
    caller: &Caller,
    action: &str,
    resource: &str,
    reason: &str,
) -> AccessDeniedException {
    let message = if caller.addresses_another_account(resource) {
        format!("User: {} is not authorized to access this resource", caller.arn)
    } else {
        format!(
            "User: {} is not authorized to perform: {action} on resource: {resource} {reason}",
            caller.arn
        )
    };
    AccessDeniedException { message }
}
```
reached from every handler, e.g. src/ops/sample_with_response_stream.rs:169-170:
```
        crate::service::authorization::authorize(&*self.engine, ctx, Self::NAME, &input.job_arn)
            .map_err(SampleWithResponseStreamErrorModeled::AccessDeniedException)?;
```
and the reason string that makes the sentence longest, src/service/authorization.rs:163: `&format!("because no identity-based policy allows the {action} action"),`.

Where the unbounded part comes from, src/service/authorization.rs:400: `caller.arn = user.arn.clone();` — the IAM store's own user ARN, built at services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/state.rs:1682-1684:
```
pub fn user_arn(path: &str, user_name: &str) -> String {
    ::engine::emulator_arn("iam", "", &format!("user{path}{user_name}"))
```
whose parts the IAM model bounds at services/service-com-amazonaws-iam-awsidentitymanagementv20100508/OPERATIONS.md:166: "`Path` (string, len 1..512, ...), `UserName` (string, len 1..64, ..., required)" — so `caller.arn` can be 30 + 512 + 64 = 606 bytes.

Test coverage, disclosed: tests/all/refusal_fits_the_modelled_message_member.rs:3-17 states the rule over every shape — "every error shape of this service (`AccessDeniedException`, `ConflictException`, ... `ValidationException`) declares exactly one member, `Message`, targeting `FailureReason`, whose `@length` is min 0 max 1024 ... So no refusal overruns the member that has to hold it" — but every row of that suite goes through `refusal_within_the_member`, which asserts `(reply.status, reply.error_code().as_str()) == (400, "ValidationException")` (tests/all/refusal_fits_the_modelled_message_member.rs:144-151), and tests/all/iam_authorization.rs drives the 403 without asserting anything about the length of its `Message`. Nothing pins the AccessDenied sentence against the 1024-byte maximum.

## Steps to Reproduce

1. Mount this service on the shared engine beside the IAM and STS services (as core/router/src/lib.rs:873 does).
2. Through IAM, create a user with `Path` at its modelled 512-character maximum and `UserName` at its modelled 64-character maximum, attach no policy document to it, and create an access key for it — its stored ARN is then `arn:aws:iam::000000000000:user<512-char path><64-char name>`, 606 bytes.
3. Register an AgentRFT job whose ARN is 256 bytes (the category segment `[a-zA-Z0-9_\-]+` is unbounded in the `JobArn` pattern, so a 256-byte well-formed ARN exists — tests/all/refusal_fits_the_modelled_message_member.rs:489-494 builds one the same way).
4. POST /sample-with-response-stream signed with that user's access key id, carrying that 256-byte `X-Amzn-SageMaker-Job-Arn`, a `X-Amzn-SageMaker-Trajectory-Id` and a JSON payload.
5. Observe the 403 `AccessDeniedException`: its `Message` is "User: <606-byte ARN> is not authorized to perform: sagemaker:SampleWithResponseStream on resource: <256-byte ARN> because no identity-based policy allows the sagemaker:SampleWithResponseStream action" — 1033 bytes, past the `@length` max 1024 the model declares for `AccessDeniedException$Message`, while the same request refused for a constraint violation is held to 1024 by `crate::service::refusal_message`.
