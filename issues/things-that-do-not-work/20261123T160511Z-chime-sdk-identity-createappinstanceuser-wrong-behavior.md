Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1e64f11a81c09e5bdb1ba6ef11f3a197d10cebd0

# The create handlers discard the shared tag directory's refusal, so a create can answer 201 having stored none of the Tags it carried

_Filed: 2026-11-23T16:05:11Z_
_Operation: CreateAppInstanceUser | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1e64f11a81c09e5bdb1ba6ef11f3a197d10cebd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

All three create handlers that persist create-time `Tags` call the shared Chime tag directory's `tag(..)` and THROW AWAY its return value. That return value is the directory's documented outcome: `Tagged::TooManyTags(n)` means "nothing was written". When it comes back, the handler still answers 201/created and the `Tags` the caller sent are silently absent from the resource, while the crate's OWN `TagResource` handler maps the identical outcome from the identical call to the modelled `ResourceLimitExceededException` — an error `CreateAppInstance`, `CreateAppInstanceUser` and `CreateAppInstanceBot` all declare too.

This is not a complaint about the value of the per-resource tag bound (the bound is justified in `chime_tags.rs` and is accepted here); it is that one call site maps the helper's refusal onto a modelled answer and the sibling call sites drop it on the floor, so a create reports success for work the store refused to do.

`CreateAppInstanceUser` is the reachable case, because its ARN is fully determined by the request (`{AppInstanceArn}/user/{AppInstanceUserId}`) rather than freshly minted: the shared, ARN-keyed tag directory can already hold tags for that exact ARN before the user exists, since a tagging call served by any other member of the Chime family "takes a sibling's resource at its word". The merge of those pre-existing tags with the create's own then exceeds the per-resource maximum, `tag(..)` writes nothing, and the create answers 201 with none of its `Tags` stored. `CreateAppInstance` and `CreateAppInstanceBot` share the same ignored-outcome line (their ARNs carry a fresh uuid, so the collision does not arise there) — the fix belongs at all three sites.

## Evidence

The helper's own contract says the CALLER must map the outcome — services/common/src/chime_tags.rs:87-96:
```
/// What [`ChimeTagDirectory::tag`] did, so the caller maps it to the outcome its model
/// declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tagged {
    /// The pairs were stored on the resource.
    Stored,
    /// Storing them would take the resource past [`MAX_TAGS_PER_RESOURCE`]; nothing was
    /// written. Carries how many tags the resource would have ended up with.
    TooManyTags(usize),
}
```
and services/common/src/chime_tags.rs:147-153:
```
    /// Nothing is written when the merged set would exceed [`MAX_TAGS_PER_RESOURCE`], so a
    /// refused call leaves the resource's tags exactly as they were.
    pub fn tag<I>(&mut self, resource_arn: &str, pairs: I) -> Tagged
```
(the refusal itself, chime_tags.rs:166-168: `if merged.len() > max_tags { return Tagged::TooManyTags(merged.len()); }`)

The create throws the outcome away — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/create_app_instance_user.rs:342-348:
```
        if !tags.is_empty() {
            tag_directory.lock().tag(&arn, tags);
        }

        Ok(CreateAppInstanceUserOutput {
            app_instance_user_arn: Some(arn),
        })
```
the same line in src/ops/create_app_instance.rs:261-263 (`tag_directory.lock().tag(&arn, tags);`) and src/ops/create_app_instance_bot.rs:329-331.

The sibling handler in THIS crate, on the same helper, does map it — src/ops/tag_resource.rs:248-267:
```
        let outcome = directory.lock().tag(&input.resource_arn, pairs);
        match outcome {
            ::service_common::chime_tags::Tagged::Stored => Ok(TagResourceOutput),
            // Past the tag count every Chime `TagList` is bounded by, nothing is written
            // and the modelled resource-limit refusal says so.
            ::service_common::chime_tags::Tagged::TooManyTags(requested) => Err(
                TagResourceErrorModeled::ResourceLimitExceededException(
```
and `ResourceLimitExceededException (HTTP 400)` is declared for the creates as well — OPERATIONS.md:52 (`## CreateAppInstanceUser` … `- **Errors:** … ResourceLimitExceededException (HTTP 400) …`).

Why the refused merge is reachable for a user ARN: src/state.rs:865-869:
```
/// An `AppInstanceUserArn` is its instance's ARN with `/user/<AppInstanceUserId>`
/// appended, so a user ARN carries its parent and the id the caller chose.
pub fn app_instance_user_arn(app_instance_arn: &str, app_instance_user_id: &str) -> String {
    format!("{app_instance_arn}/user/{app_instance_user_id}")
```
and services/common/src/chime_tags.rs:282-295:
```
/// `own_resource_exists` is what the CALLING member's own store says about it:
/// … and `None` when the ARN names another member's
/// resource. A sibling's resource is taken at its word rather than refused as absent
…
pub fn addresses_taggable_resource(resource_arn: &str, own_resource_exists: Option<bool>) -> bool {
    crate::arn::is_resource_arn_of(resource_arn, CHIME_ARN_NAMESPACE)
        && own_resource_exists.unwrap_or(true)
}
```
confirmed at the sibling member that serves the same shared route — services/service-com-amazonaws-chimesdkmessaging-chimemessagingservice/src/ops/tag_resource.rs:185-206: it refuses only when its OWN `taggable(..)` says no, then calls `tag_within(..)`, so an identity-service user ARN it holds no record for is tagged and stored.

Test coverage: tests/all/resource_tagging.rs:227-304 (`RULE: tags given at CREATE time are the same state the tagging routes read and write`) tables only one- and three-tag creates, and tests/all/resource_tagging.rs:101-226 covers the tagging routes; no test exercises a create whose merged tag set is refused, so nothing pins the current silent 201.

## Steps to Reproduce

1. POST /app-instances {"Name":"tagged","ClientRequestToken":"tok-1"} -> 201, AppInstanceArn A
2. Through another Chime family member that binds the same shared /tags route (e.g. chime-sdk-messaging, whose handler takes a sibling's ARN at its word — chimemessagingservice/src/ops/tag_resource.rs:185-206), POST /tags?operation=tag-resource with {"ResourceARN": "<A>/user/alice", "Tags": [50 distinct Key/Value pairs]} -> 204; the shared directory now holds 50 tags for an ARN the identity service holds no user for
3. POST /app-instance-users {"AppInstanceArn": A, "AppInstanceUserId": "alice", "Name": "Alice", "ClientRequestToken": "tok-2", "Tags": [{"Key":"env","Value":"prod"}]} -> 201 with AppInstanceUserArn <A>/user/alice
4. GET /tags?arn=<A>/user/alice -> the 50 pre-existing tags only; "env"/"prod" is absent. `tag(..)` answered TooManyTags(51) and wrote nothing, and create_app_instance_user.rs:342-344 discarded that answer
5. POST /tags?operation=tag-resource with {"ResourceARN": "<A>/user/alice", "Tags": [{"Key":"env","Value":"prod"}]} -> 400 ResourceLimitExceeded: the same refused write is a modelled error on the tagging route and a silent 201 on the create route
