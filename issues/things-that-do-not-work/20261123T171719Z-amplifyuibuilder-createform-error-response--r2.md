Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: ec42f8aca5996e01f32b7888d9884c173467aef1

# The emulator-store-bound refusal suite claims to span every bounded store but has no row for the form or the theme store

_Filed: 2026-11-23T17:17:19Z_
_Operation: CreateForm | Class: error-response_

> _Note: this issue was identified against the code base at commit `ec42f8aca5996e01f32b7888d9884c173467aef1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/state.rs` holds SIX stores under an emulator-owned bound, and each one has a refusal site in a handler: the metadata store (`METADATA_ENVIRONMENT_QUOTA`, refused 400 by `PutMetadataFlag`), the token-session store (`PROVIDER_TOKEN_SESSION_QUOTA`, refused 400 by `RefreshToken` and `ExchangeCodeForToken`), the codegen-job store (`CODEGEN_JOB_QUOTA`, refused 429 by `StartCodegenJob`), the component store (`COMPONENT_QUOTA`, refused 402 by `CreateComponent`), the FORM store (`FORM_QUOTA`, refused 402 by `CreateForm`) and the THEME store (`THEME_QUOTA`, refused 402 by `CreateTheme`).

The suite that owns the rule for these refusals states that its rows span all of them, and then enumerates five sites: the form store and the theme store are named nowhere, in the doc or in the table. `CreateForm`'s and `CreateTheme`'s `ServiceQuotaExceededException` (402) refusal — the only two bound refusals of this crate that nothing drives — is therefore pinned by no test, behind a suite whose own doc asserts the opposite. A later edit of either create's refusal (status, shape, or the sentence `store_bound_reached`-style refusals must not interpolate the request into) breaks no test, which is exactly the drift the suite exists to stop: the crate has already had the sibling creates diverge once (the FIXED `CreateForm never applies the crate's name-conflict rule` / `CreateTheme never applies the crate's name-conflict rule` pair).

The defect is the contradiction between the suite's stated span and its rows, not the refusals themselves, which are implemented correctly today.

## Evidence

The claim, in the suite that owns this rule:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/emulator_store_bound_refusals.rs:14-24
    //! The rows span every store this emulator bounds on its own account, and so both sides
    //! of the rule:
    //!
    //! * `PutMetadataFlag`, `RefreshToken` and `ExchangeCodeForToken` declare no quota and no
    //!   throttling error, so each refuses with the `InvalidParameterException` (400) it does
    //!   declare ...
    //! * `StartCodegenJob` declares `ThrottlingException` (429) and refuses with that;
    //! * `CreateComponent` declares `ServiceQuotaExceededException` (402) and refuses with
    //!   that.

The rows themselves — five, and no form or theme store among them:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/emulator_store_bound_refusals.rs:96
    const SITES: [Site; 5] = [
  ...:121-126
        Site {
            operation: "CreateComponent",
            store: Bounded::Component,
            refusal: 402,
            bound: COMPONENT_QUOTA,
        },

and the imports show which bounds the whole test tree reaches (no `FORM_QUOTA`, no `THEME_QUOTA`):

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/emulator_store_bound_refusals.rs:40-43
    use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::state::{
        CODEGEN_JOB_QUOTA, COMPONENT_QUOTA, METADATA_ENVIRONMENT_QUOTA, METADATA_FEATURES,
        METADATA_FEATURE_DEFAULT, PROVIDER_TOKEN_SESSION_QUOTA,
    };

The two stores the claim omits are bounds of exactly the same kind, declared next to the one it covers:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/state.rs:176-185
    /// How many forms the store holds before `CreateForm` refuses with the modelled
    /// `ServiceQuotaExceededException`.
    ...
    pub const FORM_QUOTA: usize = 1_000;
  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/state.rs:187-195
    /// How many themes the store holds before `CreateTheme` refuses with the
    /// modelled `ServiceQuotaExceededException`.
    ...
    pub const THEME_QUOTA: usize = 1_000;

and each has its own refusal site:

  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops/create_form.rs:208-219
        if store.total_forms() >= crate::state::FORM_QUOTA {
            return Err(CreateFormErrorModeled::ServiceQuotaExceededException(
                ServiceQuotaExceededException { message: Some(format!(
                    "this emulator already holds the maximum of {} forms across all app \
                     environments", crate::state::FORM_QUOTA)) },
            ).into());
        }
  services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/src/ops/create_theme.rs:197-208 (same shape, `total_themes()` / `THEME_QUOTA`)

Test coverage today: a repo-wide grep over `tests/` for `402|ServiceQuotaExceeded` matches only `emulator_store_bound_refusals.rs:23,124` (the `CreateComponent` row) and the two `BOUND_SHAPES_ELSEWHERE` lists in `exchange_code_for_token_declared_statuses.rs:63` / `refresh_token_declared_statuses.rs:55`, which assert the token operations must NOT answer 402. Nothing drives `CreateForm`'s or `CreateTheme`'s bound.

## Steps to Reproduce

1. Read tests/all/emulator_store_bound_refusals.rs:14 — "The rows span every store this emulator bounds on its own account".
2. Read the table at tests/all/emulator_store_bound_refusals.rs:96-127: five rows — PutMetadataFlag, RefreshToken, ExchangeCodeForToken, StartCodegenJob, CreateComponent.
3. Read src/state.rs:185 and src/state.rs:195: FORM_QUOTA and THEME_QUOTA are two further emulator-owned store bounds, each with a refusal site (src/ops/create_form.rs:208, src/ops/create_theme.rs:197).
4. grep the tests tree for FORM_QUOTA / THEME_QUOTA: no match anywhere, so neither bound refusal is driven by any suite, although the suite that owns the rule says its rows span every one of them.
