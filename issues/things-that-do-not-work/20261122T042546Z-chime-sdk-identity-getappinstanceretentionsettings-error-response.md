Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 0607523cc4a05d467d979a52c3af01f5efd82726

# The retention-settings pair never calls unresolved_arn, so a DELETED AppInstance earns 400 NotFound where the sibling it cites answers 403 Forbidden

_Filed: 2026-11-22T04:25:46Z_
_Operation: GetAppInstanceRetentionSettings | Class: error-response_

> _Note: this issue was identified against the code base at commit `0607523cc4a05d467d979a52c3af01f5efd82726`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both retention-settings handlers resolve the addressed `AppInstanceArn` with a bare `app_instances.get(..)` and collapse EVERY miss onto `BadRequestException`/`NotFound`. The comment under each one names `DescribeAppInstance` as its authority — "the same refusal `DescribeAppInstance` states for an absent instance" — but `DescribeAppInstance` does not state ONE refusal: it asks `state.unresolved_arn(..)` and splits the answer, giving the modelled 403 `ForbiddenException` for an instance that has been DELETED and 400 `NotFound` only for an ARN that never named one. So for the same input (an app instance created and then deleted) this crate answers two different ways depending on which route is called: `GET/PUT /app-instances/{A}/retention-settings` answer 400 `NotFound`, while `GET /app-instances/{A}`, `DELETE /app-instances/{A}`, `ListAppInstanceUsers`, `ListAppInstanceBots`, `DeleteAppInstanceAdmin`, `TagResource`, `ListTagsForResource` and ten more handlers of the same crate answer 403 with the fixed forbidden sentence. The two handlers never call `unresolved_arn` at all, so they cannot make the distinction the state module exists to make; both carry the identical branch, so one fix covers them.

DISCLOSURE for the fixer: the current answer IS pinned, by one test row — tests/all/app_instance_retention_settings.rs:338 ("an instance that has been deleted") asserts 400/`NotFound` for both get and put — while ANOTHER test in the same crate states the opposite rule as universal (token_reuse_and_deleted_refusals.rs:557-560) and that test's table claims to cover "Every operation of this service that is ADDRESSED to an app instance" while stating exclusions for only two operations, neither of them these. Fixing this therefore means moving that one retention row from 400/`NotFound` to 403/`Forbidden` for the deleted-instance case; the never-existed row and every other assertion in both files stay exactly as they are.

## Evidence

The handler under review — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/get_app_instance_retention_settings.rs:156-174:
```
        // A reporting operation: it reads the settings the named instance holds and
        // writes nothing, so two reads with no write between them answer the same.
        let Some(record) = state.app_instances.get(&input.app_instance_arn) else {
            // This operation models no 404 shape, so an unknown ARN is refused with the
            // modelled `BadRequestException` carrying the `NotFound` error code — the
            // same refusal `DescribeAppInstance` states for an absent instance.
            return Err(
                GetAppInstanceRetentionSettingsErrorModeled::BadRequestException(
                    BadRequestException {
                        code: Some("NotFound".to_string()),
```
(no `unresolved_arn` call anywhere in the file; verbatim the same branch in src/ops/put_app_instance_retention_settings.rs:157-173)

The sibling the comment CITES does not state one refusal — src/ops/describe_app_instance.rs:153-178:
```
        let Some(record) = state.app_instances.get(&input.app_instance_arn) else {
            return Err(match state.unresolved_arn(&input.app_instance_arn) {
                // The AppInstance this ARN names has been deleted, so it can no longer
                // serve a read of its own ARN: the answer is the modelled permanent
                // refusal, which is what a caller polling a deleted instance waits for.
                crate::state::UnresolvedArn::DeletedAppInstance => {
                    DescribeAppInstanceErrorModeled::ForbiddenException(ForbiddenException {
...
                crate::state::UnresolvedArn::NeverExisted => {
                    DescribeAppInstanceErrorModeled::BadRequestException(BadRequestException {
                        code: Some("NotFound".to_string()),
```

The rule the state module states for exactly this question — src/state.rs:16-19:
```
//! * `deleted_app_instances` — the ARN of every app instance this store has deleted, so
//!   a request addressed to a dead instance (or to a resource the delete cascaded away
//!   with it) is refused as one this caller may no longer make rather than as an ARN
//!   that never named anything.
```
and the helper both handlers skip — src/state.rs:553-560:
```
    /// Why `arn` resolves to no live resource of this service: because the `AppInstance`
    /// it belongs to has been deleted, or because nothing here ever named it.
...
    pub fn unresolved_arn(&self, arn: &str) -> UnresolvedArn {
```

The crate's own test states the rule as universal — tests/all/token_reuse_and_deleted_refusals.rs:557-560:
```
/// RULE: a request addressed to a deleted `AppInstance` — its own ARN, or the ARN of a
/// resource the delete cascaded away with it — is refused the modelled
/// `ForbiddenException` (403) carrying AWS's fixed forbidden sentence, while an ARN that
/// never named a resource of this service keeps the unknown-ARN refusal (400 `NotFound`).
```
and its table claims completeness while excluding only two named operations — token_reuse_and_deleted_refusals.rs:406-414:
```
/// Every operation of this service that is ADDRESSED to an app instance, to one of its
/// users or to one of its bots, built over the three ARNs given.
///
/// `DescribeAppInstanceBot` is not here: it answers the permanent refusal for ANY bot ARN
/// it holds no record for, so it cannot tell the two cases below apart.
/// `DescribeAppInstanceAdmin` is out for the same reason...
fn calls_addressed_to(instance: &str, user: &str, bot: &str) -> Vec<AddressedCall> {
```
Neither retention operation appears in that table, and no reason is stated for their absence.

The contradicting row that has to move — tests/all/app_instance_retention_settings.rs:332-357:
```
    // Rows: an ARN of the right shape naming nothing, an instance that has been deleted,
...
        ("an instance that has been deleted", deleted.clone()),
...
            assert_eq!(
                status, 400,
                "case {label:?} / {operation}: expected 400 for {arn:?}, got {status} \
                 {body:?}"
            );
            assert_eq!(
                body["Code"].as_str(),
                Some("NotFound"),
```

## Steps to Reproduce

1. POST /app-instances with {"Name":"i","ClientRequestToken":"t1"} -> AppInstanceArn A
2. PUT /app-instances/{A}/retention-settings with {"AppInstanceRetentionSettings":{"ChannelRetentionSettings":{"RetentionDays":30}}} -> 200
3. DELETE /app-instances/{A} -> 204
4. GET /app-instances/{A} (DescribeAppInstance) -> 403, Code "Forbidden", "You are not allowed to make this request."
5. GET /app-instance-bots?app-instance-arn={A} (ListAppInstanceBots) -> 403 Forbidden for the same deleted instance
6. GET /app-instances/{A}/retention-settings -> observe 400 with Code "NotFound" instead of the 403 every sibling just gave
7. PUT /app-instances/{A}/retention-settings with the same body -> observe the same 400 "NotFound"
