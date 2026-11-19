Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: e5b74111f4def785a5d4295c90400d18c8715b4a

# Deletes drop tags with `forget` instead of recording the withdrawal the crate's own tagging handlers read, so another family member serving the shared /tags route reports a deleted app instance as live and untagged

_Filed: 2026-11-19T15:22:23Z_
_Operation: DeleteAppInstance | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `e5b74111f4def785a5d4295c90400d18c8715b4a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

All three delete handlers in this crate keep the shared Chime tag directory in step by calling `forget(..)`, which only drops the tag entry. They never record that the owning member deleted the ARN (`withdraw(..)`), which is the record this crate's OWN tagging handlers read through `withdrawn_refusal(..)` in order to refuse a tagging call naming a resource whose owner deleted it. Because the identity service never writes that record for its own app instances, users and bots, the refusal path those three handlers implement can never fire for this service's ARNs, and any other family member that serves the shared `/tags` routes — which cannot read this service's store and therefore 'takes a sibling's resource at its word' — answers a tagging call for a DELETED app instance as if the resource were live, with an empty tag list, instead of the refusal this crate's comment says the owner records. The same omission applies to DeleteAppInstanceUser and DeleteAppInstanceBot.

## Evidence

The delete path drops tags only — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/delete_app_instance.rs:219-226:
```
        // The cascade reaches the tags too: the instance's own and those of every child
        // it took with it.
        let mut tags = tag_directory.lock();
        tags.forget(&input.app_instance_arn);
        for child_arn in removed.users.keys().chain(removed.bots.keys()) {
            tags.forget(child_arn);
        }
```
(and src/ops/delete_app_instance_user.rs:221 `tag_directory.lock().forget(&input.app_instance_user_arn);`, src/ops/delete_app_instance_bot.rs:221 `tag_directory.lock().forget(&input.app_instance_bot_arn);`)

This crate's tagging handlers state the rule the delete path leaves unfulfilled, and read the record it never writes — src/ops/list_tags_for_resource.rs:188-196:
```
        // An ARN whose OWNING member has deleted the resource it named is refused with
        // that member's own class and wording: this service serves the family's shared
        // tagging routes for every member's resource and cannot read a sibling's store, so
        // the fact that the resource is gone is read from the shared directory its owner
        // recorded it in, and each class is mapped onto the shape THIS model declares.
        let withdrawn = directory
            .lock()
            .withdrawn_refusal(&input.resource_arn)
```
(verbatim the same comment and read in src/ops/tag_resource.rs:216-224 and src/ops/untag_resource.rs:209-217)

The two directory helpers, and which one an owner's delete is documented to call — services/common/src/chime_tags.rs:212-231:
```
    /// Drop every tag of `resource_arn` — what the service that owns the resource calls
    /// when the resource itself is deleted, so a deleted resource leaves no tags behind.
    pub fn forget(&mut self, resource_arn: &str) -> bool {
...
    /// Drop every tag of `resource_arn` AND record that the member owning the resource has
    /// deleted it, so a later tagging call naming that ARN is refused instead of served.
    ///
    /// The owning member calls this from its own delete; ...
```
and chime_tags.rs:229-230: "the member serving the call\n    /// cannot read a sibling's store, so without this it would report a deleted resource as\n    /// a live, untagged one."

The consequence at a sibling that serves the same shared route — services/service-com-amazonaws-chimesdkmessaging-chimemessagingservice/src/ops/list_tags_for_resource.rs:157-174: it checks only its own `taggable(..)` (which, per services/common/src/chime_tags.rs:292-295, returns true for another member's ARN) and then answers `tags_of(..)`, i.e. 200 with an empty list; it never consults `withdrawn_refusal`.

Test coverage: tests/all/resource_tagging.rs and tests/all/token_reuse_and_deleted_refusals.rs:533-553 exercise the tagging routes only through THIS service, where the own-store check in `taggable` hides the missing withdrawal; no test asserts the state of the shared directory after a delete.

## Steps to Reproduce

1. POST /app-instances -> AppInstanceArn A
2. POST /tags?operation=tag-resource with {"ResourceARN": A, "Tags": [{"Key": "env", "Value": "prod"}]} -> 204
3. DELETE /app-instances/{A} -> 204
4. Inspect the shared ChimeTagDirectory: `withdrawn_refusal(A)` is None, because the delete called `forget` and not `withdraw`
5. Send ListTagsForResource (GET /tags?arn={A}) to another Chime family member that binds the same shared route (e.g. chime-sdk-messaging): it answers 200 with an empty Tags list, reporting the deleted app instance as a live untagged resource, while this service's own handler answers 403 Forbidden for the same ARN
