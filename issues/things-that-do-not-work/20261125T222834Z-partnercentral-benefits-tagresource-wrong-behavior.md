Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 849876c9e4fa69c10b30bb76c9334887d39383a5

# TagResource persists a change to a terminal benefit application, although the crate states every other change to one is refused

_Filed: 2026-11-25T22:28:33Z_
_Operation: TagResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `849876c9e4fa69c10b30bb76c9334887d39383a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every other write path in this crate that mutates a stored `BenefitApplication` gates on `BenefitApplication::is_terminal()` first and refuses with `ConflictException`, persisting nothing. `TagResource` mutates the very same stored struct — `BenefitApplication::tags`, as its own header says — but applies no status gate at all: it resolves the resource and assigns `resource.tags = merged;` unconditionally. So an application in `APPROVED`, `REJECTED` or `CANCELED` answers 200 and its stored tag set changes, while `AssociateBenefitApplicationResource` / `DisassociateBenefitApplicationResource` / `AmendBenefitApplication` / `CancelBenefitApplication` on that same record all answer 409.

The crate states the rule as universal ("as every other change to a terminal application is", "No change to its content is accepted in one"), and the only reason `TagResource` gives for differing from the application write paths is about `Revision`/`UpdatedAt`, not about the terminal gate — so the skipped check is a gap rather than a reconciled difference. `UntagResource` reaches the same unguarded write through the same `taggable_resource_mut` handle and would be covered by the same gate; the model declares `ConflictException` on both operations and on neither is it ever constructed.

## Evidence

The rule, stated as universal:

* `services/service-com-amazonaws-partnercentralbenefits-partnercentralbenefitsservice/src/state.rs:1283-1284` — "/// Whether AWS has finished with this application: true exactly for the statuses in" / "/// [`TERMINAL_STATUSES`]. No change to its content is accepted in one."
* `services/service-com-amazonaws-partnercentralbenefits-partnercentralbenefitsservice/src/ops/associate_benefit_application_resource.rs:15-17` — "An association on an application AWS has finished with (a status in [`crate::state::TERMINAL_STATUSES`]) is refused with the modelled `ConflictException` and persists nothing, **as every other change to a terminal application is**."
* `services/service-com-amazonaws-partnercentralbenefits-partnercentralbenefitsservice/src/ops/disassociate_benefit_application_resource.rs:14-16` — "A disassociation of an application AWS has finished with (a status in [`crate::state::TERMINAL_STATUSES`]) is refused with the modelled `ConflictException` and persists nothing, **as every other change to a terminal application is**."

The sibling handler that applies it, `services/.../src/ops/associate_benefit_application_resource.rs:286-299`:
```rust
        if application.is_terminal() {
            return Err(
                AssociateBenefitApplicationResourceErrorModeled::ConflictException(
                    ConflictException {
                        message: crate::state::status_conflict_message(...),
```

The handler that skips it, `services/.../src/ops/tag_resource.rs:221-258` — the resource is resolved and written with no status read at all:
```rust
        let mut store = state.lock();
        let Some(resource) = store.taggable_resource_mut(&input.resource_arn) else { ... };
        ...
        let merged = crate::state::merged_tags(&resource.tags, &input.tags);
        ...
        resource.tags = merged;
```
and the same file confirms the mutated field is the application's own stored state, `services/.../src/ops/tag_resource.rs:13-15`: "The tag set lives on the stored resource itself (`BenefitApplication::tags`, which `CreateBenefitApplication` already fills from `CreateBenefitApplicationInput$Tags`), so there is no second copy of a resource's tags."

The reason the handler does give for differing from the application write paths is narrower than the gate, `services/.../src/ops/tag_resource.rs:254-257`: "`Revision` and `UpdatedAt` describe the application's own content, which the update operations move; a tag set is resource metadata the model binds on no application output, so tagging leaves both as they were" — nothing there addresses the terminal status.

Test coverage: the terminal gate is pinned for the siblings (`tests/all/associate_benefit_application_resource.rs:589-595` `an_association_is_refused_once_the_application_is_terminal`, `tests/all/disassociate_benefit_application_resource.rs:404-411`, `tests/all/application_status_gates.rs:326-333`). No test in `tests/all/resource_tagging.rs` or `tests/all/resource_tags.rs` exercises a tagging call against a terminal application — `tests/all/resource_tags.rs:272-321` only pins that a removal leaves `Revision`/`UpdatedAt`/`Status` alone — so nothing in the crate pins the current answer.

## Steps to Reproduce

1. Grant the emulator account the partner entitlement, then `CreateBenefitApplication` in catalog `AWS` and note the returned `Id`/`Arn`.
2. `CancelBenefitApplication` on that application, so its status becomes `CANCELED` (a `TERMINAL_STATUSES` member).
3. Call `AssociateBenefitApplicationResource` with that `BenefitApplicationIdentifier`: the request is refused 409 `ConflictException` ("benefit application benappl-… is CANCELED, so it cannot be associated with an AWS resource") and nothing is persisted.
4. Call `TagResource` with that application's `resourceArn` and `tags: [{"Key": "env", "Value": "test"}]`: the request answers 200.
5. Call `ListTagsForResource` on the same ARN: the new tag is reported, so a change to the terminal application's stored record was persisted — the opposite of the rule quoted in the two sibling headers and in `is_terminal`'s own doc.
