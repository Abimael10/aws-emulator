Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: ec42f8aca5996e01f32b7888d9884c173467aef1

# The wrong-scope-vs-absent miss classification is written out at all ten triple-addressed handler sites instead of once

_Filed: 2026-11-23T17:17:19Z_
_Operation: GetComponent | Class: maintainability_

> _Note: this issue was identified against the code base at commit `ec42f8aca5996e01f32b7888d9884c173467aef1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every operation of this crate addressed by (appId, environmentName, id) has to decide the same thing when the lookup misses: if the id resolves under ANOTHER app environment the answer is the wrong-scope `InvalidParameterException` (400), otherwise the id is absent and the answer is a `ResourceNotFoundException` (404). The crate already shares the two pieces the decision uses — the predicate (`id_in_other_scope`, behind the per-kind `component_in_other_scope` / `form_in_other_scope` / `theme_in_other_scope` / `codegen_job_in_other_scope` wrappers) and the two payload builders (`item_wrong_scope()`, `item_not_found()` / `item_to_delete_not_found()`) — but the BRANCH that combines them is restated in ten handlers: `get_component.rs:165`, `get_form.rs:161`, `get_theme.rs:163`, `get_codegen_job.rs:171`, `delete_component.rs:185`, `delete_form.rs:184`, `delete_theme.rs:185`, `update_component.rs:285`, `update_form.rs:213` and `update_theme.rs:233`.

That is one rule in ten copies, and the copies have demonstrably drifted: three separate issues have been filed against individual sites answering the wrong class (GetTheme, GetCodegenJob — both fixed; the three tagging paths that omit the branch entirely — still open). A single classifier on the store (one that answers "this id missed in the wrong scope" vs "this id is absent anywhere", leaving each handler to pick its own error variant and its own not-found payload, which is why a delete keeps `item_to_delete_not_found()`) collapses the ten copies to one, and the next kind added cannot be written without it.

No response and no existing test changes: the classifier returns the same decision the ten branches compute today.

## Evidence

The crate states the rule is single-sited, over the predicate:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/state.rs:1478-1486
    /// Whether `id` names a record some OTHER app environment owns, in one of this
    /// store's (appId, environmentName)-keyed collections.
    ///
    /// The one place the wrong-scope test lives: every resource of this service that
    /// is addressed by (appId, environmentName, id) classifies a miss the same way,
    /// so the per-kind methods above ([`...::component_in_other_scope`])
    /// and its counterparts) differ only in which collection they search and where
    /// the record carries its id.
    fn id_in_other_scope<T>(

but the classification itself is copied per handler. Copy 1 (a read):

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops/get_component.rs:157-175
        let Some(component) = store.component(&input.app_id, &input.environment_name, &input.id)
        else {
            if store.component_in_other_scope(&input.app_id, &input.environment_name, &input.id) {
                return Err(GetComponentErrorModeled::InvalidParameterException(
                    crate::state::item_wrong_scope(),
                )
                .into());
            }
            return Err(GetComponentErrorModeled::ResourceNotFoundException(
                crate::state::item_not_found(),
            )
            .into());
        };

Copy 2 (another kind, same branch):

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops/get_theme.rs:155-173
        let Some(theme) = store.theme(&input.app_id, &input.environment_name, &input.id) else {
            if store.theme_in_other_scope(&input.app_id, &input.environment_name, &input.id) {
                return Err(GetThemeErrorModeled::InvalidParameterException(
                    crate::state::item_wrong_scope(),
                ).into());
            }
            return Err(GetThemeErrorModeled::ResourceNotFoundException(
                crate::state::item_not_found(),
            ).into());
        };

Copy 3 (a delete — same decision, different not-found payload):

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops/delete_component.rs:177-199
        if store.remove_component(...).is_none() {
            if store.component_in_other_scope(&input.app_id, &input.environment_name, &input.id) {
                return Err(DeleteComponentErrorModeled::InvalidParameterException(
                    crate::state::item_wrong_scope(),
                ).into());
            }
            return Err(DeleteComponentErrorModeled::ResourceNotFoundException(
                crate::state::item_to_delete_not_found(),
            ).into());
        }

Copy 4 (an update):

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops/update_form.rs:209-223 (`form_in_other_scope` + `item_not_found`)

and the remaining six at get_form.rs:161, get_codegen_job.rs:171, delete_form.rs:184, delete_theme.rs:185, update_component.rs:285, update_theme.rs:233 — a repo grep for `_in_other_scope\(` inside src/ops returns exactly these ten call sites.

Existing tests that pin the behaviour the fix must leave untouched: tests/all/read_and_delete_scope_classes.rs, tests/all/theme_scope_classes.rs, tests/all/codegen_job_scope.rs, tests/all/update_absent_id_refusal_class.rs, tests/all/wrong_scope_sentence.rs, tests/all/wrong_scope_sentence_every_path.rs, tests/all/component_refusals.rs, tests/all/form_delete.rs and tests/all/theme_deletion.rs.

## Steps to Reproduce

1. grep -n '_in_other_scope(' services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops — ten call sites, one per triple-addressed read, delete and update.
2. Read any two of them (src/ops/get_component.rs:157-175 and src/ops/get_theme.rs:155-173): the same two-branch decision, differing only in the collection searched and the error enum named.
3. Read src/state.rs:1478-1486: the crate says 'The one place the wrong-scope test lives ... every resource of this service that is addressed by (appId, environmentName, id) classifies a miss the same way' — yet the classification is restated ten times, and only the predicate is shared.
4. Confirm the copies drift: the FIXED issues 20261030T033747Z-amplifyuibuilder-gettheme-error-response and 20261030T052711Z-amplifyuibuilder-getcodegenjob-error-response each fixed ONE of these ten sites answering the wrong class.
