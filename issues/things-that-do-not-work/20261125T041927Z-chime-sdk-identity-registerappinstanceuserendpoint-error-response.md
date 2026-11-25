Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 9ee3d281406e2656923d5b4a61d84c7725448854

# RegisterAppInstanceUserEndpoint resolves ResourceArn before the AppInstanceUserArn its path names, so an absent user earns 403 Forbidden instead of the crate's unknown-ARN refusal

_Filed: 2026-11-25T04:19:27Z_
_Operation: RegisterAppInstanceUserEndpoint | Class: error-response_

> _Note: this issue was identified against the code base at commit `9ee3d281406e2656923d5b4a61d84c7725448854`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other endpoint-keyed operation of this crate resolves the `AppInstanceUserArn` in its path FIRST and states that rule in a comment, so an ARN this service holds no live user for always earns the same unknown-ARN refusal (400 `BadRequestException` / `Code: NotFound` / "AppInstanceUser <arn> does not exist", or the modelled 403 when the ARN hangs off a deleted `AppInstance`).

`RegisterAppInstanceUserEndpoint` is the one user-keyed handler that does it the other way round: it resolves the `ResourceArn` (the Pinpoint application) and can RETURN from that match, and only afterwards resolves the user, inside `insert_endpoint`. The consequence is that the SAME operation gives two different refusals for the same non-existent user depending on the value of an unrelated member:

* `ResourceArn` that resolves (or is no Pinpoint ARN at all) + absent user -> 400 `BadRequestException` `NotFound`, the crate's unknown-ARN refusal (this leg is pinned by a test);
* `ResourceArn` in Pinpoint's `apps` space with an id Pinpoint does not mint + the SAME absent user -> 403 `ForbiddenException` carrying "You are not authorized to perform this action.", a sentence that appears nowhere else in the crate (every other Forbidden refusal carries `crate::state::FORBIDDEN_REQUEST_MESSAGE`, documented at src/state.rs:117-123 as the ONE sentence "whichever resource the caller addressed").

So a client that mistypes both the user ARN and the Pinpoint ARN is told it is unauthorized rather than that the user does not exist, and the answer is not the one the crate's own rule prescribes. The fix is local and ordering-only: resolve `AppInstanceUserArn` (the two-answer `unresolved_arn` branch the siblings use) before the `ResourceArn` match. It keeps both existing tests green, because the ResourceArn test uses a LIVE user and the user test uses a resolvable ResourceArn.

## Evidence

The rule, stated in the sibling handlers — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/describe_app_instance_user_endpoint.rs:157-161:
```
        // An endpoint exists only under an `AppInstanceUser`, and that ARN is the
        // caller's own choice rather than anything this service minted, so it is
        // resolved FIRST and reported absent exactly as every other user-keyed
        // operation of this service reports it.
        if state.user(&input.app_instance_user_arn).is_none() {
```
the byte-identical comment and check in src/ops/update_app_instance_user_endpoint.rs:168-172 and src/ops/deregister_app_instance_user_endpoint.rs:175-179.

Register breaks it — src/ops/register_app_instance_user_endpoint.rs:229-250 RETURNS before the user is ever looked up:
```
        let endpoint_state = match crate::state::pinpoint_application_of(&input.resource_arn) {
            crate::state::PinpointApplication::Resolved => EndpointState {
...
            crate::state::PinpointApplication::Unresolvable => {
                return Err(
                    RegisterAppInstanceUserEndpointErrorModeled::ForbiddenException(
                        ForbiddenException {
                            code: Some("Forbidden".to_string()),
                            message: Some(
                                "You are not authorized to perform this action.".to_string(),
                            ),
```
and the user is resolved only afterwards, at src/ops/register_app_instance_user_endpoint.rs:275-303:
```
        match state.insert_endpoint(record) {
            crate::state::InsertEndpoint::Stored => {}
...
            crate::state::InsertEndpoint::NoSuchAppInstanceUser => {
                return Err(match state.unresolved_arn(&input.app_instance_user_arn) {
...
                    crate::state::UnresolvedArn::NeverExisted => {
                        RegisterAppInstanceUserEndpointErrorModeled::BadRequestException(
                            BadRequestException {
                                code: Some("NotFound".to_string()),
                                message: Some(format!(
                                    "AppInstanceUser {} does not exist",
                                    input.app_instance_user_arn
```

The one-sentence rule the hardcoded message departs from — src/state.rs:117-123:
```
/// The `Message` a refusal of a request this caller may not make carries.
///
/// One sentence, whichever resource the caller addressed and whatever left that ARN
/// unresolvable: a client branches on the `Code` (`Forbidden`), and the reason a
/// particular request was refused stays in the comment at the refusal rather than being
/// disclosed to a caller that may not read the resource.
pub const FORBIDDEN_REQUEST_MESSAGE: &str = "You are not allowed to make this request.";
```

Which inputs reach the out-of-order 403 — src/state.rs:942-961: `pinpoint_application_of` answers `Unresolvable` for any `mobiletargeting` `apps/<id>` whose id is not exactly 32 lowercase-hex characters.

Test coverage proves the gap is unpinned and that the fix keeps both tests green:
* tests/all/user_endpoint_registration.rs:1195-1200 states the rule and tests/all/user_endpoint_registration.rs:1319-1339 applies it to Register (`expected` = `(400u16, Some("NotFound"), Some(absent_user_message(user_arn)))`, lines 1262-1267) — but with `let resource_arn = resolvable_pinpoint_arn(71);` (line 1207), so only the RESOLVABLE ResourceArn leg is pinned.
* tests/all/user_endpoint_registration.rs:354-363 (`RULE: a register resolves the Amazon Pinpoint application its ResourceArn names before registering anything`) builds its `Resolution::Forbidden` rows (lines 387-411) against a LIVE user — `let user_arn = user(&emulator, "state").await;` (line 363) — so resolving the user first does not disturb it.

## Steps to Reproduce

1. POST /app-instances {"Name":"i","ClientRequestToken":"t1"} -> 201, AppInstanceArn A
2. POST /app-instance-users/{A}%2Fuser%2Fnobody/endpoints with {"ClientRequestToken":"t2","Type":"APNS","ResourceArn":"arn:aws:mobiletargeting:us-east-1:123456789012:apps/0123456789abcdef0123456789abcdef","EndpointAttributes":{"DeviceToken":"d"}} (a resolvable Pinpoint ARN, a user that does not exist) -> 400 BadRequestException, Code NotFound, Message "AppInstanceUser <A>/user/nobody does not exist" (the leg tests/all/user_endpoint_registration.rs:1319-1339 pins)
3. POST the same URL with {"ClientRequestToken":"t3","Type":"APNS","ResourceArn":"arn:aws:mobiletargeting:us-east-1:123456789012:apps/not-an-app-id","EndpointAttributes":{"DeviceToken":"d"}} -> 403 ForbiddenException, Code Forbidden, Message "You are not authorized to perform this action."
4. The addressed user is the same in both calls and does not exist, yet the refusal changes class, code and sentence because the ResourceArn match at register_app_instance_user_endpoint.rs:238-250 runs before the user resolution at lines 275-303
5. For comparison, GET/PUT/DELETE on /app-instance-users/{A}%2Fuser%2Fnobody/endpoints/{any-id} all answer 400 NotFound "AppInstanceUser <A>/user/nobody does not exist" regardless of any other member, per describe_app_instance_user_endpoint.rs:157-190
