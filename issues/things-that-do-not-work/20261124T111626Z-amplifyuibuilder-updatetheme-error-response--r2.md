Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: d5226c50c63bf82b71bab8846278d9255d30cd7f

# The UpdateTheme suite's stated rule promises 404 for a wrong-scope id that its own rows and the handler answer 400 for

_Filed: 2026-11-24T11:16:26Z_
_Operation: UpdateTheme | Class: error-response_

> _Note: this issue was identified against the code base at commit `d5226c50c63bf82b71bab8846278d9255d30cd7f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`tests/all/theme_update.rs` is the suite that states the rule `UpdateTheme` is held to. Its module doc states one refusal class for an unresolvable id: "An id that names no theme in the addressed app environment is refused (HTTP 404)". An id that a DIFFERENT app environment owns is exactly such an id — it names no theme in the addressed app environment — and the handler refuses it with the wrong-scope `InvalidParameterException` (HTTP 400), not 404. So the stated rule is false for two of the three rows the suite's own test drives, and it contradicts the handler it is written against.

The same file's test-level doc (lines 408-410) and the handler's own module doc both state the two-class rule correctly, and the sibling suite for the identical rule on forms (`tests/all/form_update.rs`, lines 556-561) spells out both classes. `theme_update.rs`'s module doc is the one place in the crate that collapses them onto 404, which is the sentence a reader and an editor of that suite meet first: anyone changing the theme update path from that sentence would make the wrong-scope id answer 404 and would then contradict `theme_scope_classes.rs`, `update_absent_id_refusal_class.rs` and the handler.

This is a different defect from the already-fixed UpdateTheme doc issue (which was about `src/ops/update_theme.rs` calling the widened 404 "modelled"): that one is about the error's MODELLED-ness in the handler file, this one is about the STATUS the test suite's stated rule gives a wrong-scope id.

## Evidence

The stated rule, `tests/all/theme_update.rs:9-11`:
```
//! instant of the call — and `GetTheme` then reports exactly that record. An id that names
//! no theme in the addressed app environment is refused (HTTP 404) and nothing is created:
//! an update is not an upsert.
```

The same file's own rows, `tests/all/theme_update.rs:408-439` — two of the three ids that "name no theme in the addressed app environment" are expected at 400, not 404:
```
/// An id `UpdateTheme` cannot resolve is refused the way its class calls for — HTTP 400
/// for an id another app environment owns, HTTP 404 for one that names no theme anywhere —
...
        ("an id nobody created", APP_ID, ENVIRONMENT, NEVER_CREATED_ID, 404,),
        ("this id under another app", "d9999zzzz", ENVIRONMENT, &here, 400,),
        ("this id under another environment", APP_ID, "prod", &here, 400,),
```

The handler, `src/ops/update_theme.rs:231-243` — the miss is classified and a wrong-scope id gets `InvalidParameterException` (400):
```
        if store
            .theme(&input.app_id, &input.environment_name, &input.id)
            .is_none()
        {
            return Err(store
                .classify_theme_miss(&input.app_id, &input.environment_name, &input.id)
                .refusal(
                    UpdateThemeErrorModeled::InvalidParameterException,
                    UpdateThemeErrorModeled::ResourceNotFoundException,
                    crate::state::item_not_found,
                )
                .into());
        }
```
and its module doc, `src/ops/update_theme.rs:6-11`: "an id it cannot resolve answers the refusal its class calls for — the modelled `InvalidParameterException` (HTTP 400) for an id another app environment owns, and `ResourceNotFoundException` (HTTP 404) for one that names no theme anywhere".

The sibling suite states it correctly, `tests/all/form_update.rs:556-561`:
```
/// An update is not an upsert: an id that names no form is refused with HTTP 404 and
/// nothing is stored, and an id a DIFFERENT app environment owns is refused as the
/// wrong-scope class the service answers for every (appId, environmentName, id)-addressed
/// operation — HTTP 400 carrying the fixed phrase
```

Test coverage: `tests/all/theme_update.rs::update_theme_refuses_an_id_that_names_no_theme_in_that_app_environment` (the rows above), `tests/all/theme_scope_classes.rs` (lines 305, 392) and `tests/all/update_absent_id_refusal_class.rs` (lines 258, 318) all pin the two-class behaviour the module doc misstates.

## Steps to Reproduce

1. Read `tests/all/theme_update.rs:9-11`: the suite states that an id naming no theme in the addressed app environment is refused with HTTP 404.
2. Read the rows of that same file's `update_theme_refuses_an_id_that_names_no_theme_in_that_app_environment` (lines 418-440): the id of a theme owned by `d9999zzzz`/`staging` and by `d1234abcd`/`prod` — neither of which names a theme in the addressed app environment — are expected at HTTP 400.
3. Read `src/ops/update_theme.rs:231-243`: the handler passes the miss through `classify_theme_miss(..).refusal(InvalidParameterException, ResourceNotFoundException, item_not_found)`, so a wrong-scope id answers 400.
4. Compare with `tests/all/form_update.rs:556-561`, which states both classes for the identical rule on forms.
