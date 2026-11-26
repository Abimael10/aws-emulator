Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: d82ca6f06b5699dc6fed4e4753e0a6d0cb330bc2

# The withdrawal a child delete publishes carries a class that renders a different error Code than this service's own refusal for the same ARN

_Filed: 2026-11-26T11:53:27Z_
_Operation: DeleteAppInstanceUser | Class: error-response_

> _Note: this issue was identified against the code base at commit `d82ca6f06b5699dc6fed4e4753e0a6d0cb330bc2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/state/tagging_arn.rs` states ONE rule in two directions: `tagging_arn_refusal` answers what a tagging call naming an ARN earns, and `withdrawal_for` publishes, for an ARN this service has just stopped holding, "the class and the wording a tagging call naming it earns here" into the shared Chime tag directory so a sibling serving the family's shared /tags route answers the same thing. Its module header claims the two directions therefore cannot drift.

They do drift for one of the two cases. For a child deleted on its own under a still-live `AppInstance` (what `DeleteAppInstanceUser` and `DeleteAppInstanceBot` do), `withdrawal_for` publishes `WithdrawnClass::InvalidRequest`, and this very file maps `WithdrawnClass::InvalidRequest` onto `TaggingArnRefusal::BadRequest { code: BAD_REQUEST_CODE }` = `Code: "BadRequest"`. But when this service answers a tagging call for that same ARN out of its OWN store it takes the `UnresolvedArn::NeverExisted` arm, which is `TaggingArnRefusal::BadRequest { code: NOT_FOUND_CODE }` = `Code: "NotFound"`. The message is identical (both call `absent_resource_message`), only the `Code` differs — the member this file itself documents as "what a client branches on".

The round-trip therefore fails exactly where the published record is meant to stand in for this service's own answer: the class the delete publishes does not map back, through this crate's own reader, to the Code this crate serves. The other case is consistent: a cascade under a deleted instance publishes `WithdrawnClass::Forbidden`, which maps back to the same `Code: "Forbidden"` the own-store `DeletedAppInstance` arm serves. So a client that tags, deletes and then re-reads a user ARN sees `Code: "NotFound"` when the call reaches this service and the published `InvalidRequest` class (this crate's `Code: "BadRequest"`) when it is served off the shared record.

## Evidence

The invariant the file states — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/state/tagging_arn.rs:29-34:
```
//! The same rule read the other way round is what this service's own deletes PUBLISH:
//! [`withdrawal_for`] answers, for an ARN this service has just stopped holding, the class
//! and the wording a tagging call naming it earns here, and a delete records that pair in
//! the shared directory. Both directions are then one statement of the rule, so the refusal
//! a sibling serves for one of this service's deleted ARNs is the refusal this service
//! serves for it.
```

What the delete PUBLISHES for a child under a live instance — tagging_arn.rs:131-137:
```
        // A child deleted on its own, under an `AppInstance` that is still live: nothing
        // this service holds carries the ARN any more, which is an invalid request rather
        // than a revoked one.
        UnresolvedArn::NeverExisted => (
            WithdrawnClass::InvalidRequest,
            absent_resource_message(resource_arn),
        ),
```

What this service SERVES for the same ARN out of its own store — tagging_arn.rs:164-170:
```
            // These operations model no 404 shape, so an ARN that names no resource this
            // service serves tagging for is refused with the modelled `BadRequestException`
            // carrying the `NotFound` error code.
            UnresolvedArn::NeverExisted => TaggingArnRefusal::BadRequest {
                code: NOT_FOUND_CODE,
                message: absent_resource_message(resource_arn),
            },
```

What the published class renders as, in the same file — tagging_arn.rs:183-187:
```
    withdrawn.map(|(class, message)| match class {
        WithdrawnClass::InvalidRequest => TaggingArnRefusal::BadRequest {
            code: BAD_REQUEST_CODE,
            message,
        },
```
with the two codes being different strings — tagging_arn.rs:46 `const NOT_FOUND_CODE: &str = "NotFound";` and tagging_arn.rs:53 `const BAD_REQUEST_CODE: &str = "BadRequest";` — and tagging_arn.rs:83-84 declaring why the difference matters: "/// The refusal's `Code` member, which is what a client branches on."

The publisher — src/ops/delete_app_instance_user.rs:227-230:
```
        let (class, message) = crate::state::withdrawal_for(&state, &input.app_instance_user_arn);
        tag_directory
            .lock()
            .withdraw(&input.app_instance_user_arn, class, message);
```
(the same call in src/ops/delete_app_instance_bot.rs:215-218; src/ops/delete_app_instance.rs:247-250 publishes the Forbidden class, which does round-trip).

The shared record is only ever read through the class — services/common/src/chime_tags.rs:260-267 `withdrawn_refusal` hands back `WithdrawnClass` + message and nothing else, and `WithdrawnClass` has only the two variants, so "NotFound" cannot be published at all.

Test coverage: tests/all/token_reuse_and_deleted_refusals.rs:574-594 drives TagResource/UntagResource/ListTagsForResource only through THIS service, where the own-store half always answers first (src/state.rs:618-623 `taggable` returns `Some(false)` for an ARN of this service it no longer holds), so no test exercises the published class for one of this service's own deleted ARNs; tests/all/resource_tagging.rs likewise asserts only this service's own answers.

## Steps to Reproduce

1. POST /app-instances with Name/ClientRequestToken -> AppInstanceArn A (201)
2. POST /app-instance-users with AppInstanceArn=A, AppInstanceUserId=u1, Name=u1, ClientRequestToken=t -> AppInstanceUserArn U (201)
3. POST /tags?operation=tag-resource with {"ResourceARN": U, "Tags": [{"Key":"env","Value":"prod"}]} -> 204
4. DELETE /app-instance-users/{U} -> 204; the handler publishes withdrawal_for(U) = (WithdrawnClass::InvalidRequest, "resource U does not exist")
5. GET /tags?arn={U} on this service -> 400 BadRequestException with Code "NotFound" (own-store arm, tagging_arn.rs:167-169)
6. Render the published withdrawal through the same file's reader (tagging_arn.rs:183-187), i.e. have the shared /tags route served off the withdrawn record rather than this store -> 400 BadRequestException with Code "BadRequest" for the same ARN, so the two directions of the one rule disagree on the Code a client branches on
