Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 9201f5afcd8a8fca0f840d0fba4dca06027d197c

# Collapse the six copies of the resource-ARN syntax-then-authorization rule into one state helper

_Filed: 2026-11-24T21:22:30Z_
_Operation: ListTagsForResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `9201f5afcd8a8fca0f840d0fba4dca06027d197c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every operation addressed by a `ResourceArn` applies the SAME two-branch rule in the same order: a value outside the published resource-ARN syntax is the modelled `ValidationException`, and a well-formed ARN that names no resource of this catalog/account is the modelled `AccessDeniedException` built from the operation name. That rule is implemented SIX times, once per handler (`ListTagsForResource`, `TagResource`, `UntagResource`, `PutResourcePolicy`, `GetResourcePolicy`, `DeleteResourcePolicy`), as a verbatim copy of the same two branches plus the same five-line comment that points at `ListTagsForResource` as the canonical copy.

Nothing keeps the six copies in step: the branch ORDER (syntax before authorization) and the error CLASS of each branch (422 vs 403) are re-decided in each handler, so a future edit to one copy — reordering the two checks, or answering `ResourceNotFoundException` where the rule says `AccessDeniedException` — silently diverges from the other five while the crate still compiles. This crate already has the pattern for avoiding exactly that: the comparable two-fault member rule for `Catalog` lives in ONE state function (`check_catalog`) that every operation calls, and the page-size and page-token decisions are centralised for the same stated reason.

The fix is a single `crate::state` function that takes the operation name and the submitted ARN and answers either the parsed `CatalogResource` or the one fault to raise (a two-variant fault the caller maps into its own modelled error enum). Each handler then calls it and lifts the fault into its own `…ErrorModeled` variant. No response body, status, message or existing test changes: the two refusal payloads already come from the shared `crate::state::invalid_resource_arn` / `crate::state::access_denied` builders, so only the duplicated branch structure moves.

## Evidence

The rule, stated ONCE for all six operations, in the test that drives them from one table — `services/service-com-amazonaws-marketplacecatalog-awsmpseymour/tests/all/resource_policy_and_tagging.rs:821-824`:

```
/// Every operation addressed by a `ResourceArn` classifies a refusal the same way, in
/// this order: a value outside the modelled `ResourceARN` syntax is a validation failure
/// naming the value and that syntax; a well-formed ARN naming no resource of this catalog
/// in this account is `AccessDeniedException`, because authorization is evaluated on the
/// ARN before anything is looked up;
```

and the operations it covers (`tests/all/resource_policy_and_tagging.rs:876-883`):

```
    for operation in [
        "ListTagsForResource",
        "TagResource",
        "UntagResource",
        "PutResourcePolicy",
        "GetResourcePolicy",
        "DeleteResourcePolicy",
    ] {
```

Copy 1 — the canonical one, `src/ops/list_tags_for_resource.rs:157-178`:

```
        // `ResourceArn` is judged here rather than at the boundary
        // (`skip_validation` on this input): a value outside the syntax the
        // member publishes is no resource ARN at all, so it is refused with the
        // modelled `ValidationException` and its 422, quoting that syntax, before
        // the ARN is read for the resource it names.
        if !crate::state::is_resource_arn(&input.resource_arn) {
            return Err(ListTagsForResourceErrorModeled::ValidationException(
                crate::state::invalid_resource_arn(&input.resource_arn),
            )
            .into());
        }

        // Authorization is evaluated on the ARN BEFORE anything is looked up: an
        // ARN that names no resource of this catalog in this account is one no
        // identity or resource policy of this catalog can grant an action on, so
        // the caller is not authorized to read it rather than it being absent.
        let Some(resource) = crate::state::parse_resource_arn(&input.resource_arn) else {
            return Err(ListTagsForResourceErrorModeled::AccessDeniedException(
                crate::state::access_denied(Self::NAME, &input.resource_arn),
            )
            .into());
        };
```

Copy 2 — `src/ops/get_resource_policy.rs:154-175`, the same two branches under a comment that names the copy it was taken from:

```
        // `ResourceArn` is judged here rather than at the boundary
        // (`skip_validation` on this input), the way `ListTagsForResource`
        // judges it: a value outside the syntax the member publishes is no
        // resource ARN at all, so it is refused with the modelled
        // `ValidationException` and its 422, quoting that syntax.
        if !crate::state::is_resource_arn(&input.resource_arn) {
            return Err(GetResourcePolicyErrorModeled::ValidationException(
                crate::state::invalid_resource_arn(&input.resource_arn),
            )
            .into());
        }
        ...
        let Some(resource) = crate::state::parse_resource_arn(&input.resource_arn) else {
            return Err(GetResourcePolicyErrorModeled::AccessDeniedException(
                crate::state::access_denied(Self::NAME, &input.resource_arn),
            )
            .into());
        };
```

Copies 3-6 are the identical pair of branches, with the identical comment, at `src/ops/delete_resource_policy.rs:149-170`, `src/ops/put_resource_policy.rs:153-174`, `src/ops/tag_resource.rs:155-176` and `src/ops/untag_resource.rs:156-177` (grep for `is_resource_arn(&input.resource_arn)` / `parse_resource_arn(&input.resource_arn)` lists exactly six of each).

The in-crate precedent for collapsing a two-fault member rule into one place — `src/state.rs:3494-3508`:

```
/// The `Catalog` check every operation of this service runs before it does
/// anything else: a value that is not a catalog NAME at all is refused for its
/// syntax, and a well-formed name other than the one catalog this service serves
/// is refused as naming nothing it has.
///
/// The two are different faults: the first says the request is malformed, the
/// second that the catalog it named is not this one.
pub fn check_catalog(catalog: &str) -> Result<(), ValidationException> {
```

and the same "one place" reasoning used for the paging rules, `src/state.rs:1791-1794`: "This is the one place that decision is made, so each paginated operation judges a page size the same way".

Test coverage that must stay unchanged: `tests/all/resource_policy_and_tagging.rs:834-955` (all six operations x malformed -> 422 `ValidationException` with the one sentence, x unauthorized -> 403 `AccessDeniedException`), `tests/all/resource_policy_validation.rs`, `tests/all/shared_service_arn_fields.rs:560-630`, `tests/all/request_validation.rs:1110-1125`.

## Steps to Reproduce

1. grep -n "is_resource_arn(&input.resource_arn)" services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/ops/*.rs — six hits, one per ARN-addressed operation
2. grep -n "parse_resource_arn(&input.resource_arn)" services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/ops/*.rs — the same six handlers, each re-deciding that the parse failure is AccessDeniedException and that it comes after the syntax check
3. Read tests/all/resource_policy_and_tagging.rs:821-883: the rule and the list of operations it holds are written once, over a table, while the implementation is copied six times
4. Compare with src/state.rs:3501 check_catalog, where the analogous two-fault member rule for `Catalog` is implemented once and called by all nine Catalog-bearing handlers
5. Edit any one of the six copies (e.g. swap the two checks) and observe that the crate still compiles with five handlers classifying the same ARN differently from the sixth
