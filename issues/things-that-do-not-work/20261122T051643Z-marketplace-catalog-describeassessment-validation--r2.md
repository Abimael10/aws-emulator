Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: fff33ceb5efd21b424356734b860ab1e86f98eb5

# The assessment-ARN alternative is resolved without judging its partition, region or account fields, so identifiers outside the syntax the refusal quotes are accepted as well formed

_Filed: 2026-11-22T05:16:43Z_
_Operation: DescribeAssessment | Class: validation_

> _Note: this issue was identified against the code base at commit `fff33ceb5efd21b424356734b860ab1e86f98eb5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DescribeAssessmentRequest$AssessmentIdentifier` carries `skip_validation`, so the boundary validator does NOT apply the modelled `@pattern` and the handler is the named replacement check. That replacement — `crate::state::assessment_id_of` — destructures the ARN alternative into six fields and binds the partition, the region and the account as WILDCARDS (`Some(_partition)`, `Some(_region)`, `Some(_account)`); the only things it judges are the literal `arn`, this service's `ARN_NAMESPACE`, the `<catalog>/Assessment/` resource prefix and the id. Anything that contains no colon is accepted in those three fields.

Both the modelled `@pattern` and the crate's OWN published regex — `ASSESSMENT_IDENTIFIER_PATTERN`, the string `invalid_assessment_identifier` quotes back to the caller — require a NON-EMPTY partition of `[a-zA-Z0-9-]`, a region of `[a-zA-Z0-9-]*`, and an account that is digits only (the model's `[0-9]*`; the crate's own string requires the field to be empty). The crate's own doc on that constant enumerates exactly ONE deliberate widening over it — the account field — and says the predicate is otherwise "stricter on everything the shape of an ARN cannot say". The partition and region are widened too, and to no stated rule at all.

So each of these resolves as a well-formed identifier and is answered with the 404 `ResourceNotFoundException` the handler reserves for a WELL-FORMED identifier that names nothing (or 200, for an id the store happens to hold), instead of the 422 `ValidationException`:

* `arn::aws-marketplace:eu-west-1::AWSMarketplace/Assessment/assessment-1` — empty partition, which both patterns forbid (`[a-zA-Z0-9-]+`).
* `arn:aws:aws-marketplace:eu_west_1::AWSMarketplace/Assessment/assessment-1` — an underscore in the region, outside `[a-zA-Z0-9-]*`.
* `arn:aws:aws-marketplace:eu-west-1:abc:AWSMarketplace/Assessment/assessment-1` — a non-digit account, outside the model's `[0-9]*`.

This is also the crate disagreeing with itself. The sibling member taken off the SAME boundary for the same reason — `ResourceArn` on the six ARN-addressed operations — spells out exactly these field character classes AND their non-emptiness in `is_resource_arn`, in the same file, before `parse_resource_arn` resolves the ARN. The two ARN surfaces of this one crate therefore hold different views of what the partition/region/account fields of an ARN of this service may look like.

Note this is NOT the already-fixed `@length(1..2048)` gap on the same member (issue 20261029T104213Z-marketplace-catalog-describeassessment-validation): that fix added the length bound at the top of `assessment_id_of` (state.rs:3484) and left the ARN field classes unchecked.

## Evidence

Smithy model (smithy_query query="shape" shape="AssessmentIdentifier"):
  "pattern": "^(assessment-[a-zA-Z0-9]+|arn:[a-zA-Z0-9-]+:[a-zA-Z0-9-]+:[a-zA-Z0-9-]*:[0-9]*:[a-zA-Z0-9-]+/Assessment/assessment-[a-zA-Z0-9]+)$"
— i.e. partition `[a-zA-Z0-9-]+` (non-empty), region `[a-zA-Z0-9-]*`, account `[0-9]*`.

The boundary is switched off and the handler named as the replacement — services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/ops/types/describe_assessment_types.rs:21:
  "    skip_validation = [\"Catalog\", \"AssessmentIdentifier\", \"MaxResults\", \"NextToken\"]"

What the handler actually accepts — src/state.rs:3490-3510 (`assessment_id_of`):
```
    let mut fields = identifier.splitn(6, ':');
    let (
        Some("arn"),
        Some(_partition),
        Some(ARN_NAMESPACE),
        Some(_region),
        Some(_account),
        Some(resource),
    ) = (...)
    else {
        return None;
    };
    let id = resource.strip_prefix(&format!("{catalog}/{ASSESSMENT_ARN_RESOURCE_TYPE}/"))?;
    is_assessment_id(id).then_some(id)
```

The syntax the service quotes back in the refusal — src/state.rs:3535-3538:
```
pub const ASSESSMENT_IDENTIFIER_PATTERN: &str = concat!(
    r"(^assessment-[a-zA-Z0-9]+$)|(^arn:[a-zA-Z0-9-]+:[a-zA-Z0-9-]+:[a-zA-Z0-9-]*::",
    r"[a-zA-Z0-9-]+/Assessment/assessment-[a-zA-Z0-9]+$)"
);
```
and the rule its own doc states, which the code breaks — src/state.rs:3527-3531:
```
/// [`assessment_id_of`] is wider than this string on the ARN alternative — the
/// account field of an ARN names where an assessment lives, not which one it is,
/// so an ARN carrying one resolves — and stricter on everything the shape of an
/// ARN cannot say: one naming another service's namespace or another catalog
/// names no assessment of this catalog and is refused too.
```
(the account widening is stated; the partition and region widenings are not.)

The SIBLING `skip_validation` ARN member in the same file DOES judge those fields — src/state.rs:3691-3699 (`is_resource_arn`):
```
    if partition.is_empty() || !partition.chars().all(field) {
        return false;
    }
    if !region.chars().all(field) {
        return false;
    }
    if account.is_empty() || !account.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
```

The answer the handler's own comment says this must not produce — src/ops/describe_assessment.rs:202-214:
```
        // "You can provide either the assessment ID … or the full assessment ARN".
        // An identifier of neither form names no assessment at all, so it is
        // refused BEFORE the store is read — a malformed identifier is a
        // `ValidationException`, an unknown one a `ResourceNotFoundException`, and
        // the two must not be confused.
        let Some(assessment_id) =
            crate::state::assessment_id_of(&input.assessment_identifier, &input.catalog)
```

Test coverage: tests/all/assessments.rs:170-219 is the table that pins which ARN identifiers are accepted (404) and which refused (422). It covers "the arn form with no account" (line 177-181) and "the arn form with no region" (line 182-187) and "the arn form in another partition" (line 188-194) — but NO row has an empty partition, a non-digit account, or a region character outside `[a-zA-Z0-9-]`, so none of the three behaviours above is pinned by an existing test.

## Steps to Reproduce

1. POST /DescribeAssessment with {"Catalog":"AWSMarketplace","AssessmentIdentifier":"arn::aws-marketplace:eu-west-1::AWSMarketplace/Assessment/assessment-1","MaxResults":50} — the partition field is empty.
2. Observe HTTP 404 ResourceNotFoundException ("Requested assessment '…' does not exist."), i.e. the identifier was accepted as well formed, although the modelled @pattern requires `arn:[a-zA-Z0-9-]+:` and the regex the service's own refusal quotes (ASSESSMENT_IDENTIFIER_PATTERN, state.rs:3535-3538) requires the same.
3. Repeat with "arn:aws:aws-marketplace:eu-west-1:abc:AWSMarketplace/Assessment/assessment-1" (account field is not digits, against the model's `[0-9]*`) and with "arn:aws:aws-marketplace:eu_west_1::AWSMarketplace/Assessment/assessment-1" (underscore in the region, against `[a-zA-Z0-9-]*`): both are answered 404 rather than the 422 ValidationException.
4. Expected in each case: the 422 ValidationException the handler answers for an identifier outside the syntax it publishes, per its own comment at src/ops/describe_assessment.rs:202-206.
5. Contrast with the sibling ARN surface: GET /GetResourcePolicy?resourceArn=arn::aws-marketplace:us-east-1:123456789012:AWSMarketplace/AmiProduct/ent-1 — `is_resource_arn` (state.rs:3691) refuses the empty partition with 422, showing the crate already has the field-level rule for one of its two ARN-addressed surfaces and not the other.
