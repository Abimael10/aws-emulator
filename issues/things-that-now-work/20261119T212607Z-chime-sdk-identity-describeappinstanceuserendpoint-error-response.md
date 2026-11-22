Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: fae8da064c2d4dbbf13168e02f63a16c4cc6fce8

# Endpoint-keyed operations answer 403 Forbidden for an AppInstanceUserArn this service never held, where the crate's other user-keyed operations answer 400 NotFound

_Filed: 2026-11-19T21:26:07Z_
_Operation: DescribeAppInstanceUserEndpoint | Class: error-response_

> _Note: this issue was identified against the code base at commit `fae8da064c2d4dbbf13168e02f63a16c4cc6fce8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The three endpoint-keyed handlers (DescribeAppInstanceUserEndpoint, UpdateAppInstanceUserEndpoint, DeregisterAppInstanceUserEndpoint) resolve the request with a single lookup over `(AppInstanceUserArn, EndpointId)` and collapse EVERY miss onto the modelled 403 ForbiddenException. The reason each one states for that collapse is about the ENDPOINT ID only — "An `EndpointId` is minted by the service and opaque to the caller" — but the same branch also swallows the case where the `AppInstanceUserArn` itself names a user this service never held, which is not an opaque, service-minted value at all: it is the caller-chosen ARN every other user-keyed operation in this crate reports as absent with 400 `BadRequestException`/`NotFound`.

So for one and the same input — a user ARN that never named anything — this crate answers two different ways depending only on which operation of the same route prefix (`/app-instance-users/{AppInstanceUserArn}/endpoints...`) is called: RegisterAppInstanceUserEndpoint and ListAppInstanceUserEndpoints answer 400 `NotFound` with "AppInstanceUser {arn} does not exist", while Describe/Update/Deregister of an endpoint under that same absent user answer 403 with the fixed forbidden sentence. The crate's own test states the rule the three endpoint handlers break: a deleted-AppInstance ARN earns 403, while "an ARN that never named a resource of this service keeps the unknown-ARN refusal (400 `NotFound`)". The three handlers never call `unresolved_arn(..)` at all, so they cannot make that distinction, even though the helper exists and 14 other handlers in the crate use it.

All three handlers carry the identical branch; one fix (resolve the user first with `unresolved_arn`, keep the 403 only for the endpoint-id miss) covers them.

## Evidence

The endpoint-keyed handler collapses every miss onto 403, with a reason that speaks only about the EndpointId — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/describe_app_instance_user_endpoint.rs:158-172:
```
        let Some(record) = state.endpoint(&input.app_instance_user_arn, &input.endpoint_id) else {
            // An `EndpointId` is minted by the service and opaque to the caller, so an
            // endpoint this user does not hold — deregistered, or never registered — is
            // nothing this caller may read, and the refusal is the modelled permanent
            // one rather than a report on whether that endpoint ever existed.
            return Err(
                DescribeAppInstanceUserEndpointErrorModeled::ForbiddenException(
                    ForbiddenException {
                        code: Some("Forbidden".to_string()),
                        message: Some(crate::state::FORBIDDEN_REQUEST_MESSAGE.to_string()),
```
(verbatim the same branch in src/ops/update_app_instance_user_endpoint.rs:168-184 and src/ops/deregister_app_instance_user_endpoint.rs:178-195)

The sibling operation on the SAME user ARN, in the same crate, distinguishes the two cases — src/ops/list_app_instance_user_endpoints.rs:161-189:
```
        let Some(user) = state.user(&input.app_instance_user_arn) else {
            return Err(match state.unresolved_arn(&input.app_instance_user_arn) {
...
                // This operation models no 404 shape, so an ARN that never named a user
                // is refused with the modelled `BadRequestException` carrying the
                // `NotFound` error code.
                crate::state::UnresolvedArn::NeverExisted => {
                    ListAppInstanceUserEndpointsErrorModeled::BadRequestException(
                        BadRequestException {
                            code: Some("NotFound".to_string()),
                            message: Some(format!(
                                "AppInstanceUser {} does not exist",
                                input.app_instance_user_arn
                            )),
```
and so does the register on the very same route — src/ops/register_app_instance_user_endpoint.rs:291-301: `UnresolvedArn::NeverExisted => ... BadRequestException { code: Some("NotFound"), message: Some(format!("AppInstanceUser {} does not exist", ...)) }`.

The crate-wide rule the three handlers break is stated by this crate's own test — tests/all/token_reuse_and_deleted_refusals.rs:557-560:
```
/// RULE: a request addressed to a deleted `AppInstance` — its own ARN, or the ARN of a
/// resource the delete cascaded away with it — is refused the modelled
/// `ForbiddenException` (403) carrying AWS's fixed forbidden sentence, while an ARN that
/// never named a resource of this service keeps the unknown-ARN refusal (400 `NotFound`).
```

Test coverage: tests/all/user_endpoint_registration.rs:837-842 pins the register against a never-existed user ARN (`format!("{owner}x")`) to `(400, Some("NotFound"))`. The 403 rows for the endpoint-keyed calls at user_endpoint_registration.rs:786-823 all address an EXISTING user (`owner`/`other`) with an unknown or foreign endpoint id — no test addresses an endpoint-keyed call to a user ARN this service never held, so the current answer for that input is unpinned.

## Steps to Reproduce

1. POST /app-instances with {"Name":"i","ClientRequestToken":"t1"} -> AppInstanceArn A
2. POST /app-instance-users with {"AppInstanceArn":A,"AppInstanceUserId":"owner","Name":"owner","ClientRequestToken":"t2"} -> AppInstanceUserArn U
3. Build an absent user ARN: Ux = U + "x" (an ARN this service never held)
4. GET /app-instance-users/{Ux}/endpoints (ListAppInstanceUserEndpoints) -> 400, Code "NotFound", "AppInstanceUser ... does not exist"
5. POST /app-instance-users/{Ux}/endpoints (RegisterAppInstanceUserEndpoint, valid body) -> 400, Code "NotFound"
6. GET /app-instance-users/{Ux}/endpoints/00000000-0000-4000-8000-000000000000 (DescribeAppInstanceUserEndpoint) -> 403 Forbidden instead of the 400 NotFound the two calls above gave for the same absent user ARN
7. Same for PUT (UpdateAppInstanceUserEndpoint) and DELETE (DeregisterAppInstanceUserEndpoint) on that path
