Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 3b2c489229ee1293122b93552c056649e886f841

# The token-authorization handler rule (principal entity types + entities check, and the TokenRefusal-to-error mapping) is implemented twice, once per token operation

_Filed: 2026-11-15T16:57:39Z_
_Operation: IsAuthorizedWithToken | Class: maintainability_

> _Note: this issue was identified against the code base at commit `3b2c489229ee1293122b93552c056649e886f841`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`IsAuthorizedWithToken` and `BatchIsAuthorizedWithToken` carry two copies of the same handler-level logic, with nothing shared between them:

1. The computation of the principal entity types a token call's `entities` slice may not carry — the identity source's own `owned_entity_types` mapped through `PrincipalEntityType::from_identity_source`, extended with `schema_principal_entity_types(...)` read off the store's schema — followed by `check_token_request_entities(supplied, &principal_entity_types)` and `supplied.extend(identity.entities())`. Fourteen lines, character-for-character identical in the two files.

2. The mapping of a `crate::state::token::TokenRefusal` onto the operation's modelled errors: the same five arms, the same `"Failed to decode {}: {reason}"` / `"Failed to verify {}: {reason}"` compositions, the same `fieldList` construction from `kind.path()`, the same audience/not-found/not-checkable classification.

Only the error-enum name differs between the copies, so either can be changed without the other — which is how a token rule comes to answer one way on the single-request path and another on the batch path. The crate already extracts exactly this kind of shared handler rule into `crate::ops` (`persisted_identity_source_configuration`, `persisted_principal_entity_type`, `page`/`page_entries`, `identity_source_not_found_for_issuer`), so the home for it exists.

A fix lifts (1) into one `crate::ops` helper and (2) into one translation that returns a refusal DESCRIPTION the two operations lift into their own enums (the shape `InvalidInput`/`Refusal` already use). Every response byte and every existing test stays unchanged.

## Evidence

COPY 1, `src/ops/is_authorized_with_token.rs:328-341`:

        let mut principal_entity_types: Vec<crate::state::cedar::PrincipalEntityType> = identity
            .owned_entity_types
            .iter()
            .map(crate::state::cedar::PrincipalEntityType::from_identity_source)
            .collect();
        principal_entity_types.extend(
            crate::state::cedar::schema_principal_entity_types(
                store.schema.as_ref().map(|schema| schema.cedar_json.as_str()),
            )
            .map_err(refused_by_cedar)?,
        );
        crate::state::cedar::check_token_request_entities(supplied, &principal_entity_types)
            .map_err(invalid)?;
        supplied.extend(identity.entities());

COPY 2, `src/ops/batch_is_authorized_with_token.rs:357-370` — the same fourteen lines:

        let mut principal_entity_types: Vec<crate::state::cedar::PrincipalEntityType> = identity
            .owned_entity_types
            .iter()
            .map(crate::state::cedar::PrincipalEntityType::from_identity_source)
            .collect();
        principal_entity_types.extend(
            crate::state::cedar::schema_principal_entity_types(
                store.schema.as_ref().map(|schema| schema.cedar_json.as_str()),
            )
            .map_err(refused_by_cedar)?,
        );
        crate::state::cedar::check_token_request_entities(supplied, &principal_entity_types)
            .map_err(invalid)?;
        supplied.extend(identity.entities());

SECOND DUPLICATED RULE — the refusal mapping. `src/ops/is_authorized_with_token.rs:470-478`:

        crate::state::token::TokenRefusal::NotDecoded { kind, reason } => {
            IsAuthorizedWithTokenErrorModeled::ValidationException(ValidationException {
                message: format!("Failed to decode {}: {reason}", kind.reported_name()),
                field_list: Some(vec![ValidationExceptionField {
                    path: kind.path().to_string(),
                    message: reason,
                }]),

and `src/ops/batch_is_authorized_with_token.rs:536-544`, the same arm:

        crate::state::token::TokenRefusal::NotDecoded { kind, reason } => {
            BatchIsAuthorizedWithTokenErrorModeled::ValidationException(ValidationException {
                message: format!("Failed to decode {}: {reason}", kind.reported_name()),
                field_list: Some(vec![ValidationExceptionField {
                    path: kind.path().to_string(),
                    message: reason,
                }]),

The remaining four arms (`NotVerified`, `AudienceNotAccepted`, `NoIdentitySource`, `NotCheckable`) are duplicated the same way at `is_authorized_with_token.rs:481-519` and `batch_is_authorized_with_token.rs:546-586`, and the sentences the two mappings rely on are duplicated as per-file constants: `AUDIENCE_NOT_ACCEPTED` (`is_authorized_with_token.rs:397` and `batch_is_authorized_with_token.rs:460`), `TOKEN_REQUIRED` (`:383` / `:446`), `DIFFERENT_PRINCIPALS` (`:389` / `:452`), `REQUIRED_FOR_EVALUATION` (`:380` / `:443`).

That the two copies are supposed to stay identical is stated in the crate itself, `src/ops/batch_is_authorized_with_token.rs:507-509`:

    /// The modelled error a token this call could not use is refused with — the same three
    /// answers [`crate::ops::is_authorized_with_token`] gives, because the token rules are the
    /// token's and not the calling operation's.

TEST COVERAGE (both copies, so a single implementation keeps them green): `tests/all/token_body_decode_refusal.rs:20` ("`IsAuthorizedWithToken` and `BatchIsAuthorizedWithToken`, because the token rules are the token rules") and its matrix at `:302-305`; `tests/all/entity_list_refusal_pointers.rs:170`; `tests/all/identity_source_refusal_pointers.rs`; `tests/all/oidc_identity_source_authorization.rs:818-864`; `tests/all/is_authorized_with_token.rs`; `tests/all/batch_is_authorized_with_token.rs`; `tests/all/not_found_precedes_payload_refusal.rs:282-294`.

## Steps to Reproduce

1. Open `src/ops/is_authorized_with_token.rs` and `src/ops/batch_is_authorized_with_token.rs` side by side.
2. Compare lines 328-341 of the first with lines 357-370 of the second: the principal-entity-type computation, the `check_token_request_entities` call and the `supplied.extend(identity.entities())` write are identical.
3. Compare `fn refused` (`is_authorized_with_token.rs:466-520`) with `fn refused_token` (`batch_is_authorized_with_token.rs:531-587`): the same five `TokenRefusal` arms, message compositions and `fieldList` construction, differing only in the error-enum name.
4. Note the four refusal sentences duplicated as per-file constants (`AUDIENCE_NOT_ACCEPTED`, `TOKEN_REQUIRED`, `DIFFERENT_PRINCIPALS`, `REQUIRED_FOR_EVALUATION`).
5. Change any one of these rules in one file and observe that the other operation keeps the old behaviour, with no compile error to catch the drift.
