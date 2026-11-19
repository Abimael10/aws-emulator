Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 35358047cadec8da3b04a119cef2ff3c58aff3e2

# The amend handler's documented answer order puts the entitlement gate first and cites a constant that does not exist, while the handler it heads resolves the Catalog first

_Filed: 2026-11-19T09:52:41Z_
_Operation: AmendBenefitApplication | Class: error-response_

> _Note: this issue was identified against the code base at commit `35358047cadec8da3b04a119cef2ff3c58aff3e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/ops/amend_benefit_application.rs` opens with a numbered list that claims to be the order in which this operation answers refusals. Step 1 is the centralized constraint walk and step 2 is the partner-entitlement gate; no catalog resolution appears in the list at all, so the header states that an un-entitled caller is denied before this service looks anything up.

The handler under that header does the opposite, and says so in its own body comment: it calls `crate::state::write_request_refusal`, which resolves the `Catalog` FIRST and returns `ResourceNotFoundException` for a catalog this service does not hold whoever asked, applying the entitlement gate only to a request whose catalog resolved. That order is not incidental — it is the rule `state::write_request_refusal` documents as the single construction for every writing operation, and `tests/all/write_gate_order.rs` pins it over the wire with `AmendBenefitApplication` named in its table of writing operations.

The same header paragraph also links `[`crate::state::ENTITLEMENT_REQUIRED`]`, a path that exists nowhere in the crate (the constant is `crate::state::PARTNER_ENTITLEMENT_REQUIRED`), so the one doc reference a reader would follow to check the claim is dangling.

Every other write handler's header matches its body on this point; this one is the single file whose stated refusal order contradicts the code beneath it, which is what will mislead the next person who copies an answer order from a handler header.

## Evidence

The header's ordered list, which omits the catalog step and puts the gate second — src/ops/amend_benefit_application.rs:8-13:
    //! The operation answers in this order, which is the order AWS reports:
    //!
    //! 1. request-shape validation — the centralized walk, before this handler runs;
    //! 2. the partner-entitlement gate — a caller holding no active AWS partner benefit is
    //!    refused with `AccessDeniedException` (see [`crate::state::ENTITLEMENT_REQUIRED`]),
    //!    so it never learns whether the application it named exists;

The body of the same file, stating and implementing the opposite order — src/ops/amend_benefit_application.rs:832-841:
        // The catalog this write would land in is resolved FIRST, and the
        // partner-entitlement gate applies to a request whose catalog resolved — see
        // [`crate::state::write_request_refusal`]. It takes the store lock itself, so it
        // runs before this handler takes one.
        match crate::state::write_request_refusal(&*self.engine, &input.catalog) {
            Some(crate::state::WriteRequestRefusal::MissingCatalog(missing)) => {
                return Err(
                    AmendBenefitApplicationErrorModeled::ResourceNotFoundException(missing).into(),
                )
            }

The helper that owns the order, src/state.rs:3246-3252:
    /// The refusal an operation that WRITES answers a request with ahead of its own work: the
    /// `Catalog` the request names is resolved FIRST (...catalog_refusal),
    /// and the partner-entitlement gate applies only to a request whose catalog resolved.
    ///
    /// ONE construction of that order for every writing operation, so a catalog this service
    /// does not hold is reported as the missing resource it is whichever write named it

The behaviour is pinned over the wire for this very operation — tests/all/write_gate_order.rs:6-11:
    //! The rule under test: an operation that WRITES resolves the `Catalog` its request names
    //! before the account-level entitlement gate, so a catalog this service does not hold is
    //! answered with that lookup's own 404 `ResourceNotFoundException` (``Catalog `<name>` not
    //! found``) whatever the calling account holds, and the gate refuses only a request whose
    //! catalog resolved
and tests/all/write_gate_order.rs:51-52 lists it in that class:
    const WRITING_OPERATIONS: &[&str] = &[
        "AmendBenefitApplication",

The dangling doc path: `grep -r ENTITLEMENT_REQUIRED src/` returns only src/state.rs:93 (`pub const PARTNER_ENTITLEMENT_REQUIRED`), src/state.rs:1654 (its use) and src/ops/amend_benefit_application.rs:12 (the reference above) — there is no `crate::state::ENTITLEMENT_REQUIRED` item in the crate.

Test coverage: tests/all/write_gate_order.rs covers the actual order for all eight writing operations; tests/all/entitlement_denial.rs pins the denial itself. No test asserts the header's text, so correcting the header changes no response and no test.

## Steps to Reproduce

1. Read src/ops/amend_benefit_application.rs:8-13 and note that the documented answer order lists the partner-entitlement gate as step 2, immediately after request-shape validation, with no catalog resolution before it.
2. Read src/ops/amend_benefit_application.rs:832-848 in the same file and note that the first thing the handler does is `crate::state::write_request_refusal`, whose `MissingCatalog` arm answers `ResourceNotFoundException` before the `NotEntitled` arm can be reached.
3. Read src/state.rs:3246-3252 for that helper's contract: the `Catalog` is resolved FIRST and the gate applies only to a request whose catalog resolved.
4. Confirm the real behaviour is the pinned one: tests/all/write_gate_order.rs:6-11 states the rule and :51-52 lists `AmendBenefitApplication` among the writing operations it asserts it for.
5. Grep the crate for `ENTITLEMENT_REQUIRED`: the only matches are `PARTNER_ENTITLEMENT_REQUIRED` in src/state.rs:93/1654 and the dangling `[`crate::state::ENTITLEMENT_REQUIRED`]` reference in src/ops/amend_benefit_application.rs:12.
