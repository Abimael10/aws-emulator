Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 41be3a2659be45277be7049b64517ef95e5ebd6f

# The DeleteTheme suite's stated rule promises 404 for a wrong-scope id that its own table and the handler answer 400 for

_Filed: 2026-11-26T03:45:30Z_
_Operation: DeleteTheme | Class: error-response_

> _Note: this issue was identified against the code base at commit `41be3a2659be45277be7049b64517ef95e5ebd6f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`tests/all/theme_deletion.rs` is the suite that owns `DeleteTheme`'s refusal statuses. Its module doc states ONE refusal for every id the lookup misses on: "an id that names no theme in the addressed app environment is refused with the modelled `ResourceNotFoundException` at HTTP 404". An id a DIFFERENT app environment owns is exactly such an id — it names no theme in the addressed app environment — but the handler answers the wrong-scope `InvalidParameterException` (HTTP 400) for it, and the suite's OWN table asserts that 400 on four of its five rows. So the suite's stated rule contradicts both the code it covers and the rows underneath it.

The handler and every sibling delete state the rule correctly (two classes, told apart by where the id resolves): `src/ops/delete_theme.rs` and `src/ops/delete_form.rs` module docs, and the sibling suite `tests/all/form_delete.rs`. `theme_deletion.rs` is the one place in the crate that collapses the two classes into the 404.

The defect is the stated rule: a maintainer reading the suite's contract would conclude DeleteTheme must answer 404 for a wrong-scope id, which is the refusal class a FIXED issue deliberately moved off 404 ("Theme reads/updates/deletes answer 404 for an id another app environment owns, where every component and form path answers the wrong-scope 400").

## Evidence

The stated rule — `services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/theme_deletion.rs:9-13`:
```
//! The rule under test: `DeleteTheme` removes the one theme the addressed
//! (appId, environmentName, id) holds and answers an empty 2xx, after which that theme is
//! absent for every later reader, while every other stored theme is untouched; an id that
//! names no theme in the addressed app environment is refused with the modelled
//! `ResourceNotFoundException` at HTTP 404 and removes nothing.
```

The same suite's own test doc and table, `tests/all/theme_deletion.rs:331-334` and `:346-375`:
```
/// A theme is addressed by (appId, environmentName, id), so an id that resolves to no
/// theme in the addressed app environment removes nothing and is refused the way its class
/// calls for: HTTP 400 for an id another app environment owns, HTTP 404 for an id that
/// names no theme anywhere.
```
```
        ("an id another app owns", APP_ID, ENVIRONMENT, &elsewhere, 400),
```
(and three further rows at `:355-375` all expecting `400`).

The handler, `src/ops/delete_theme.rs:192-199`:
```
            return Err(store
                .classify_theme_miss(&input.app_id, &input.environment_name, &input.id)
                .refusal(
                    DeleteThemeErrorModeled::InvalidParameterException,
                    DeleteThemeErrorModeled::ResourceNotFoundException,
                    crate::state::item_to_delete_not_found,
                )
                .into());
```
where `src/state.rs:1515-1518` maps `IdMiss::WrongScope` to the first of those two:
```
        match self {
            Self::WrongScope => wrong_scope(item_wrong_scope()),
            Self::Absent => not_found(absent()),
        }
```

The handler's own doc says it correctly, `src/ops/delete_theme.rs:4-9`:
```
//! Removes the stored theme the request names and answers an empty 2xx. An id it cannot
//! resolve is refused the way its class calls for: the modelled
//! `InvalidParameterException` (HTTP 400) for an id another app environment owns, and the
//! modelled `ResourceNotFoundException` (HTTP 404), carrying the delete path's own
//! literal, for an id that names no theme anywhere
```

And so does the sibling suite for the same rule on the form delete, `tests/all/form_delete.rs:16-21`:
```
//! 2. A delete that resolves no form in the addressed app environment is refused and
//!    changes nothing: an id that names no form anywhere answers the modelled
//!    `ResourceNotFoundException` (404) with the DELETE path's own message, and an id a
//!    different app environment owns answers the modelled `InvalidParameterException`
//!    (400)
```

Test coverage of the behaviour: `tests/all/theme_deletion.rs::delete_theme_refuses_an_id_that_names_no_theme_in_that_app_environment`, `tests/all/theme_scope_classes.rs`, `tests/all/wrong_scope_sentence.rs` (its path census covers the theme delete) — all of which assert the 400 the module doc denies.

## Steps to Reproduce

1. Read `services/service-com-amazonaws-amplifyuibuilder-amplifyuibuilder/tests/all/theme_deletion.rs:9-13`: the suite's stated rule is that an id naming no theme in the addressed app environment is refused with HTTP 404.
2. Read the same file at `:331-334` and the table at `:346-375`: four of the five rows assert HTTP 400 for an id another app environment (or another environment of the same app) owns.
3. Read `src/ops/delete_theme.rs:192-199` together with `src/state.rs:1515-1518`: a `IdMiss::WrongScope` miss is answered as `InvalidParameterException` (HTTP 400), never the 404 the suite's rule states.
4. Compare with `src/ops/delete_theme.rs:4-9` and `tests/all/form_delete.rs:16-21`, which both state the two-class rule correctly.
