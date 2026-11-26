Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 50f537bfb4a737f2b108e5c2b5f2a2e244f4e685

# The tagging surface reads and writes codegen-job records without the settle the state module says every codegen handler must call first

_Filed: 2026-11-26T05:36:32Z_
_Operation: ListTagsForResource | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `50f537bfb4a737f2b108e5c2b5f2a2e244f4e685`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::settle_codegen_jobs` is the one place a codegen job's owed status transition is materialised, and its own doc instructs every handler that reaches a codegen job to call it FIRST — "including a read". `ListCodegenJobs`'s module doc restates that rule for readers ("Like every codegen reader this settles first"), and `GetCodegenJob`, `ListCodegenJobs` and `StartCodegenJob` all do call it.

The three tagging handlers also reach codegen-job records — `crate::state::resource_tags` reads `state.codegen_job(..)` and `crate::state::with_resource_tags_mut` takes `state.codegen_job_mut(..)` and mutates it — yet none of `ListTagsForResource`, `TagResource` or `UntagResource` calls `settle_codegen_jobs` after taking the store lock. The crate's own codegen-job tagging suite calls `ListTagsForResource` and `GetCodegenJob` "the two readers of one job", so one of those two readers follows the stated rule and the other does not.

There is no divergence in what a client is handed today, because no tagging response reports a job's `status`, `statusMessage` or `modifiedAt`: the three tagging paths are the only places in the crate that reach a codegen-job record while skipping the helper the state module mandates for exactly that record kind. That is the defect — a rule the crate writes down once and then applies on three of six paths, so the next member added to a tagging answer (or any tagging-side decision that consults a job's status) silently reports a pre-settle job.

## Evidence

The rule, `src/state.rs:1040-1043`:
```
    /// Materialise every codegen-job status transition the clock has already made
    /// due. Call this FIRST in every codegen handler, including a read: it only
    /// applies a change that was owed at `now` whether or not anybody called.
    pub fn settle_codegen_jobs(&mut self, now: SystemTime) {
```

Restated for readers, `src/ops/list_codegen_jobs.rs:13-15`:
```
//! Like every codegen reader this settles first, so a page reports the status
//! transitions the clock has already made due rather than moving a job because it
//! was listed — see [`crate::state::StoredCodegenJob`].
```
and obeyed there (`src/ops/list_codegen_jobs.rs:179-184`): `let mut store = state.lock();` … `store.settle_codegen_jobs(::std::time::SystemTime::now());`, as in `src/ops/get_codegen_job.rs:158-161` and `src/ops/start_codegen_job.rs:163-174`.

The tagging reader skips it, `src/ops/list_tags_for_resource.rs:161-164`:
```
        // A REPORTING call: the tag set is read out of the resource's own record and
        // nothing is written, so a later caller sees exactly what this one saw.
        let store = state.lock();
        let tags = crate::state::resource_tags(&store, &input.resource_arn).map_err(|miss| {
```
and the tagging writers skip it too, `src/ops/tag_resource.rs:163-168` (and `src/ops/untag_resource.rs:161-170`):
```
        let mut store = state.lock();
        let written = crate::state::with_resource_tags_mut(
            &mut store,
            &input.resource_arn,
            |tags| tags.extend(input.tags),
        );
```

That those paths really reach codegen-job records, `src/state.rs:2514-2516`:
```
        TaggedKind::CodegenJob => state
            .codegen_job(app, environment, id)
            .map(|job| job.tags.clone()),
```
and `src/state.rs:2552-2554`:
```
        TaggedKind::CodegenJob => state
            .codegen_job_mut(app, environment, id)
            .map(|job| change(&mut job.tags)),
```

The crate calls both operations readers of one job, `tests/all/codegen_job_tagging.rs:19-21`:
```
//! 1. `ListTagsForResource` reports the tag set the start stored, and `GetCodegenJob`
//!    reports the same one: there is a single stored set, so the two readers of one job can
//!    never disagree about what it carries or about whether it exists.
```

Test coverage: `tests/all/codegen_job_tagging.rs::tagging_a_codegen_job_changes_nothing_but_its_tag_set` sleeps `PAST_THE_DUE_INSTANT` and settles the job through `GetCodegenJob` BEFORE it tags (lines 454-460), so no existing test drives a tagging call against a job whose transition is due but unsettled.

## Steps to Reproduce

1. Read `src/state.rs:1040-1043`: `settle_codegen_jobs` must be called FIRST in every codegen handler, including a read.
2. Read `src/ops/get_codegen_job.rs:158-161`, `src/ops/list_codegen_jobs.rs:179-184` and `src/ops/start_codegen_job.rs:163-174`: each takes the store lock and settles.
3. Read `src/ops/list_tags_for_resource.rs:161-164`, `src/ops/tag_resource.rs:163-168` and `src/ops/untag_resource.rs:161-170`: each takes the store lock and goes straight to `resource_tags` / `with_resource_tags_mut`, with no settle.
4. Read `src/state.rs:2514-2516` and `src/state.rs:2552-2554`: those two helpers read and mutate `StoredCodegenJob` records, so the three tagging paths are codegen handlers by the rule's own terms.
5. Compare with `tests/all/codegen_job_tagging.rs:19-21`, which names `ListTagsForResource` and `GetCodegenJob` the two readers of one job — only one of which settles.
