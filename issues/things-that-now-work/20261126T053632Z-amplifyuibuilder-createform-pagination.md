Issue-Severity: LOW
Issue-Gap-Class: pagination
Issue-Commit: 50f537bfb4a737f2b108e5c2b5f2a2e244f4e685

# The page-cursor counter's own doc names only components and themes, while form and codegen-job creates draw their cursors from it too

_Filed: 2026-11-26T05:36:32Z_
_Operation: CreateForm | Class: pagination_

> _Note: this issue was identified against the code base at commit `50f537bfb4a737f2b108e5c2b5f2a2e244f4e685`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`cursor_seq` is the single counter every paginated record of this service indexes into: `page_of` serves a page by comparing `record.cursor()` against the cursor a token carries, and `parse_page_token` admits only cursors in `FIRST_CURSOR..=cursor_seq`. Four record kinds draw from it — components, forms, themes and codegen jobs — because `CreateComponent`, `CreateForm`, `CreateTheme` and `StartCodegenJob` all call `next_cursor()`.

The field's own doc says it is the counter two of those four draw from: "The counter [`StoredComponent::cursor`] and [`StoredTheme::cursor`] values are drawn from." The `StoredForm::cursor` and `StoredCodegenJob::cursor` docs a few hundred lines above say the opposite — each is "drawn from the same counter [`StoredComponent::cursor`] is drawn from" — and the `CreateForm` handler says so inline while calling `next_cursor()`. So the one place a reader looks to learn what the counter is for under-reports its users by half, in a crate whose own module doc insists the page walk and its cursors are shared by every paginated operation (`ListForms`, `ListThemes`, the codegen-job list and the three exports included). Nothing answers differently today; the inconsistency is that the counter's definition and the records that draw from it disagree, which is what an editor of the page walk reads first.

## Evidence

The field doc, `src/state.rs:706-708`:
```
    /// The counter [`StoredComponent::cursor`] and [`StoredTheme::cursor`]
    /// values are drawn from.
    pub cursor_seq: u64,
```

But `StoredForm::cursor`, `src/state.rs:557-564`:
```
    /// The position this form holds in its environment's creation order.
    ///
    /// It is what a `ListForms` or an `ExportForms` page token indexes into:
    /// drawn from the same
    /// counter [`StoredComponent::cursor`] is drawn from, it never changes and
    /// never repeats, so a page resumes at the same place whether or not forms
    /// were created or deleted since the previous page.
    pub cursor: u64,
```

and `StoredCodegenJob::cursor`, `src/state.rs:445-451`:
```
    /// The position this job holds in its environment's start order.
    ///
    /// It is what a `ListCodegenJobs` page token indexes into: drawn from the
    /// same counter [`StoredComponent::cursor`] is drawn from, it never changes
    /// and never repeats, ...
```

The code that draws them, `src/ops/create_form.rs:238-241`:
```
        // The position in the service's one creation order an `ExportForms` page
        // token indexes into, drawn from the counter a component's cursor also
        // comes from.
        let cursor = store.next_cursor();
```
and `src/ops/start_codegen_job.rs:212-214`:
```
        // The position this job takes in its environment's start order, which is
        // what a `ListCodegenJobs` page token indexes into.
        let cursor = store.next_cursor();
```

The counter is what the whole page walk is bounded by, `src/state.rs:913-918`:
```
    /// The counter pre-increments, so the first cursor handed out is
    /// [`FIRST_CURSOR`]: the cursors in existence always run from there up to
    /// `cursor_seq`, which is the span [`parse_page_token`] admits.
    pub fn next_cursor(&mut self) -> u64 {
        self.cursor_seq = self.cursor_seq.saturating_add(1);
        self.cursor_seq
    }
```

Test coverage: `tests/all/shared_page_walk.rs` and `tests/all/page_token_collection_scope.rs` drive all four collections' cursors through `store.cursor_seq` (each list and export passes it to `page_of` as `issued`), so forms and codegen jobs provably draw from the counter the doc omits.

## Steps to Reproduce

1. Read `src/state.rs:706-708`: `cursor_seq` is documented as the counter `StoredComponent::cursor` and `StoredTheme::cursor` are drawn from.
2. Read `src/state.rs:557-564` and `src/state.rs:445-451`: a form's and a codegen job's cursor are each documented as drawn from that same counter.
3. Read `src/ops/create_form.rs:241` and `src/ops/start_codegen_job.rs:214`: both call `store.next_cursor()`, the only writer of `cursor_seq` (`src/state.rs:915-918`).
4. Read `src/ops/list_forms.rs:176-183` and `src/ops/list_codegen_jobs.rs:197-204`: both pass `store.cursor_seq` to `page_of` as the highest cursor issued, so the forms and codegen-job page walks are bounded by the counter whose doc does not mention them.
