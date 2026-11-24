Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: a0a325cd7a8f100e0b15b3c52772a25e70aa00e8

# The tagging-ARN refusal rule is implemented three times verbatim across TagResource, UntagResource and ListTagsForResource

_Filed: 2026-11-19T14:39:21Z_
_Operation: TagResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `a0a325cd7a8f100e0b15b3c52772a25e70aa00e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The rule that decides whether a tagging call may address an ARN — `taggable` + `unresolved_arn` mapped to Forbidden/NotFound, then the shared directory's `withdrawn_refusal` mapped from `WithdrawnClass` onto this model's two error shapes — is written out three times in this crate, once per tagging handler, differing only in the name of the generated error enum. Roughly 55 lines are duplicated per copy, so a change to the rule has to be made in three places and the copies can drift (the ListAppInstanceAdmins divergence filed separately shows exactly how a copied refusal rule drifts in this crate). Behaviour and responses must stay identical; the three copies should resolve the refusal once (e.g. a helper returning the `UnresolvedArn`/`WithdrawnClass` decision, with each handler only wrapping it in its own error enum).

## Evidence

Copy 1 — services/service-com-amazonaws-chimesdkidentity-chimeidentityservice/src/ops/tag_resource.rs:185-215:
```
        // `None` when the ARN names a resource a tagging call may address; otherwise WHY
        // it does not, which is what decides the refusal.
        let unresolved = {
            let store = state.lock();
            (!store.taggable(&input.resource_arn))
                .then(|| store.unresolved_arn(&input.resource_arn))
        };
```
plus tag_resource.rs:216-241 (`withdrawn_refusal` -> `WithdrawnClass::InvalidRequest|Forbidden`).

Copy 2 — src/ops/untag_resource.rs:178-208, byte-identical apart from the enum name:
```
        // `None` when the ARN names a resource a tagging call may address; otherwise WHY
        // it does not, which is what decides the refusal.
        let unresolved = {
            let store = state.lock();
            (!store.taggable(&input.resource_arn))
                .then(|| store.unresolved_arn(&input.resource_arn))
        };
```
plus untag_resource.rs:209-234.

Copy 3 — src/ops/list_tags_for_resource.rs:157-187 and 188-213, the same two blocks again:
```
        let unresolved = {
            let store = state.lock();
            (!store.taggable(&input.resource_arn))
                .then(|| store.unresolved_arn(&input.resource_arn))
        };
```

Existing tests that must stay green unchanged: tests/all/resource_tagging.rs — `tags_round_trip_through_the_shared_tagging_routes` (line 104), `tags_supplied_at_create_time_are_listed_and_can_be_changed` (line 231) and `a_tagging_call_this_service_cannot_serve_is_refused_and_writes_nothing` (line 333, which asserts the 403/`Forbidden` and 400/`NotFound` rows at line 322), plus the three tagging rows of tests/all/token_reuse_and_deleted_refusals.rs:533-553.

## Steps to Reproduce

1. Open src/ops/tag_resource.rs:185-241 and src/ops/untag_resource.rs:178-234 side by side: the two blocks differ only in the error-enum name
2. Open src/ops/list_tags_for_resource.rs:157-213 for the third copy of the same two blocks
3. Note that `grep -c 'withdrawn_refusal' src/ops` reports one occurrence in each of the three handlers and nowhere else in the crate
