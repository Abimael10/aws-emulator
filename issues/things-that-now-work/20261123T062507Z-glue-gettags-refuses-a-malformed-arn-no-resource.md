Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: glue.get-tags-malformed-arn-rejected
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue.get-tags-malformed-arn-rejected|run#4|AssertionError|assert code == "InvalidInputException", f"expected InvalidInputException, got {code!r}"
Issue-Commit: 84690972c7a63a9952a7abe9f5e5702ab42c29df

# Glue GetTags refuses a malformed ARN (no resource-type segment) with EntityNotFoundException instead of InvalidInputException with an empty message

> _Note: this issue was identified against the code base at commit `84690972c7a63a9952a7abe9f5e5702ab42c29df`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTags

## Description
One net-new root cause in the GetTags ARN-validation path of service-com-amazonaws-glue-awsglue. The single failing check below is listed under service heading 'glue' with NO 'OWNED by' owner and NO 'ALREADY FILED' marker. ONE cause must be closed before this issue is submitted.

Cause 1 of 1: GetTags called with a ResourceArn that satisfies the modeled Glue-ARN prefix but carries NO recognizable Glue resource-type segment (the resource part is a bare token such as 'not-a-resource-type', with no '<type>/<name>' structure) is refused with the wrong error class. Failing check: glue.get-tags-malformed-arn-rejected.

AWS: Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json): GetTags input {ResourceArn @required GlueResourceArn, @length 1..10240, pattern ^arn:aws(-(cn|us-gov|iso(-[bef])?))?:glue:}; output {Tags: TagsMap @length 0..50}; errors EntityNotFoundException (client, HTTP 400), InvalidInputException (client, HTTP 400), OperationTimeoutException (client, HTTP 400), InternalServiceException (server, HTTP 500). The model therefore fixes BOTH candidate codes and their status; which one applies to an unparseable resource part, and the message, are recorded behavior only. Recorded behavior: an ARN whose resource part names no Glue resource TYPE is a malformed INPUT, not a missing entity — it is refused 400 InvalidInputException with an EMPTY Error.Message (the empty message is the whole of the text; there is no fixed sentence and no variable part). The distinction is structural, not value-dependent: the ARN never addresses any resource, so no lookup is reached. The same operation over a well-formed resource ARN of a resource that DOES exist — 'arn:aws:glue:<region>:<account>:database/<name>' for a database created with Tags — still answers HTTP 200 with exactly that database's own tag map.

Emulator: the call is refused with code 'EntityNotFoundException' (replay detail: "AssertionError: expected InvalidInputException, got 'EntityNotFoundException'"). The handler routes an ARN it cannot parse into its resource-lookup/not-found branch instead of rejecting it as invalid input, so a shape error is reported as a missing entity. Because the check stops at the code assertion, the empty-message requirement and the following positive control (GetTags over a real database ARN returning that database's exact tag map) are not yet observed and must be made to hold too.

Why: InvalidInputException and EntityNotFoundException drive opposite client branches. A caller that treats EntityNotFoundException as 'this resource was deleted — tolerate and continue' (teardown, tag reconciliation, cost-allocation sweeps) will silently swallow a genuinely malformed ARN it should have surfaced as a programming/configuration error, and retry logic keyed on not-found will poll forever for an ARN that can never resolve. Conversely a caller that validates ARNs by probing for InvalidInputException will conclude the malformed ARN is well-formed.

Fix: the ResourceArn parsing/validation step of the GetTags handler in the glue crate (and the shared Glue resource-ARN resolver it calls): classify an ARN with no recognizable Glue resource-type segment as invalid input and raise InvalidInputException with an empty message BEFORE any resource lookup, reserving EntityNotFoundException for ARNs that parse to a known resource type whose resource is absent.

Related open issues that do NOT cover this cause (named here so a fixer does not conflate them): 20261117T183344Z-glue-catalog-surface-7-net-new-causes-gettags.md Cause 1 is the inverse defect (a WELL-FORMED live database/registry ARN refused with InvalidInputException and an empty message) and carries Issue-Key glue|GetTags|InvalidInputException; 20261120T124947Z-glue-2-net-new-causes-gettags-post-delete.md Cause 1 is the MESSAGE of the post-DELETE EntityNotFoundException for a well-formed ARN and carries Issue-Key glue|GetTags|EntityNotFoundException; 20261120T051845Z-glue-gettags-refuses-an-existing-untagged-resource-with.md is an existing untagged resource refused instead of returning an empty Tags map. None of them concerns ARN-FORM classification, and the replay marked this check neither OWNED nor ALREADY FILED. The fixed issue 20261102T114851Z-glue-8-net-new-root-causes-refusal-messages.md reworded Glue refusal text including GetTags' but does not carry this check or this branch.

Traces: glue.get-tags-malformed-arn-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 613/724 passed (84.7%); 77 mismatched, 34 errored. The check below appears under service heading 'glue', unowned and unmarked — it is the only failing check in the whole report that is neither 'OWNED by' an open issue nor 'ALREADY FILED'.

[mismatch] glue.get-tags-malformed-arn-rejected
  run.py line 456 -> check_glue_get_tags_malformed_arn_rejected.py line 31:
    assert code == "InvalidInputException", f"expected InvalidInputException, got {code!r}"
  -> AssertionError: expected InvalidInputException, got 'EntityNotFoundException'

Call exercised: GetTags(ResourceArn='arn:aws:glue:<region>:<account>:not-a-resource-type') — the ARN matches the modeled GlueResourceArn prefix pattern but its resource part has no '<type>/<name>' structure.
  Required: HTTP 400, Error.Code = 'InvalidInputException', Error.Message = '' (empty).
  Actual:   Error.Code = 'EntityNotFoundException'.
The region and account in the ARN are this run's environment values filling the template and are not part of the requirement; the requirement is the ARN FORM (a resource part naming no Glue resource type).

Positive control in the same check, reached only after the code and empty-message assertions pass (check lines 33-40): CreateDatabase(DatabaseInput={'Name': <db>}, Tags={'stack': 'warehouse'}) then GetTags('arn:aws:glue:<region>:<account>:database/<db>') must answer HTTP 200 with exactly {'stack': 'warehouse'} — no superset and no subset. 'stack'/'warehouse' are the check's chosen tag pair; the requirement is that the returned map equal the map the request seeded.

Model evidence (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json): GetTags input {ResourceArn @required GlueResourceArn, @length 1..10240, pattern ^arn:aws(-(cn|us-gov|iso(-[bef])?))?:glue:} -> output {Tags: TagsMap @length 0..50}; errors [EntityNotFoundException (client, 400), InternalServiceException (server, 500), InvalidInputException (client, 400), OperationTimeoutException (client, 400)]. The model declares both codes at HTTP 400; which one an unparseable resource part maps to, and the empty message, are recorded behavior.

## Steps to Reproduce
1. Clone this checkout and boot the emulator built from it.
2. Call GetTags with a ResourceArn that matches Glue's ARN prefix but whose resource part names no Glue resource type, e.g. ResourceArn='arn:aws:glue:<region>:<account>:not-a-resource-type'. Read the refusal: it must be HTTP 400 with Error.Code='InvalidInputException' and Error.Message='' (empty), NOT EntityNotFoundException.
3. Repeat with other resource parts that carry no '<type>/<name>' segment (e.g. ':nosuchtype', ':database' with no name) — all must classify as InvalidInputException, not as a missing entity.
4. Positive control, which must keep working: CreateDatabase(DatabaseInput={'Name': <db>}, Tags={'stack':'warehouse'}); GetTags(ResourceArn='arn:aws:glue:<region>:<account>:database/'+<db>) must answer HTTP 200 with Tags exactly equal to {'stack':'warehouse'}; then DeleteDatabase(Name=<db>).
5. Confirm the split is preserved: GetTags on a WELL-FORMED ARN of a resource type that exists but whose resource was deleted must still be EntityNotFoundException (not InvalidInputException).
6. Re-replay the check in trace_ids to confirm the fix.
