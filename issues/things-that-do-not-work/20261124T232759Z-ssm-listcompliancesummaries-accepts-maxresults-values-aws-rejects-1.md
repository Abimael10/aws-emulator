Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.list-compliance-summaries-maxresults-only-fifty-or-absent
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.list-compliance-summaries-maxresults-only-fifty-or-absent|_error#3|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 4d64375b908e4858d980b965a76727675c50db36

# ssm ListComplianceSummaries accepts MaxResults values AWS rejects — 1, 2 and 49 are served HTTP 200 instead of ValidationException "ListComplianceSummaries only support MaxResults as null or 50"

> _Note: this issue was identified against the code base at commit `4d64375b908e4858d980b965a76727675c50db36`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListComplianceSummaries

## Description
Cause 1 of 1: ListComplianceSummaries serves any MaxResults inside the modeled 1..50 range, where AWS serves only MaxResults=50 or an absent MaxResults and refuses every other in-range value (failing check: ssm.list-compliance-summaries-maxresults-only-fifty-or-absent).

AWS: Smithy (ssm): ListComplianceSummaries input {Filters: ComplianceStringFilterList, NextToken, MaxResults: MaxResults @range min 1 max 50}; modeled errors InternalServerError (500), InvalidFilter (400), InvalidNextToken (400); paginated with pageSize=MaxResults. The model's own documentation for the member states the narrower rule the service enforces: "The maximum number of items to return for this call. Currently, you can specify null or 50." Recorded behavior accordingly: a request carrying MaxResults of 1, 2 or 49 — all inside the @range — is refused HTTP 400 ValidationException with the message "ListComplianceSummaries only support MaxResults as null or 50" (entirely fixed text; the operation name is the only name in it and it is this operation's). A request carrying MaxResults=51 — outside the @range — is refused HTTP 400 ValidationException with the standard constraint sentence "1 validation error detected: Value '51' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 50", whose fixed text is "1 validation error detected: Value '", "' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to " and whose variable parts are the value from the request and the member's modeled maximum. MaxResults=50 and an absent MaxResults are served HTTP 200.

Emulator: MaxResults=1 is served HTTP 200 with body {"ComplianceSummaryItems": []} — no refusal at all. The page-size gate behind the handler validates only the modeled range: services/service-com-amazonaws-ssm-amazonssm/src/ops/list_compliance_summaries.rs:184-185 calls compliance::compliance_page_size(input.max_results), which is services/service-com-amazonaws-ssm-amazonssm/src/state/compliance_items.rs:239-245 forwarding to the shared page_size helper with MIN_COMPLIANCE_PAGE_SIZE=1 and MAX_COMPLIANCE_PAGE_SIZE=50, so every value in 1..50 is accepted and the operation-specific "null or 50" rule is never applied. Because the first value the check sends (1) is accepted, whether the 51 refusal carries AWS's constraint sentence is untested here and should be verified with the same fix.

Why: a client that paginates compliance summaries with any page size other than 50 gets a silent, non-AWS success here and a hard ValidationException against real AWS — the divergence shows up only in production. Conversely, an SDK or tool that probes the accepted page size to choose a paging strategy learns a size AWS will not honour, and error-handling paths keyed on the documented ValidationException never execute against the emulator.

Fix: ListComplianceSummaries' input validation — the operation needs its own MaxResults rule in front of the generic range check: outside 1..50 answer 400 ValidationException with the standard constraint sentence, and inside the range answer 400 ValidationException with "ListComplianceSummaries only support MaxResults as null or 50" unless the value is exactly 50 (or absent). The generic page-size helper is shared, so the rule belongs in this operation's handler/validation rather than in compliance_page_size. Open issue 20261114T143448Z-ssm-listcompliancesummaries-stub.md asked for the operation to be implemented and does not cover this: the operation now answers 200, and open issue 20261120T214510Z-ssm-5-net-new-causes-patchbaseline-tag-reads.md covers neighbouring compliance causes (ListComplianceItems Details) but not this member's rule, and neither carries this check's replay key.

Traces: ssm.list-compliance-summaries-maxresults-only-fifty-or-absent.

## Evidence
Expected: ssm ListComplianceSummaries(Filters=[{Key:'ComplianceType',Values:['<unused type>']}], MaxResults=1) -> HTTP 400 ('ValidationException', 'ListComplianceSummaries only support MaxResults as null or 50'); same for MaxResults=2 and 49. MaxResults=51 -> HTTP 400 ('ValidationException', "1 validation error detected: Value '51' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 50"). MaxResults=50 and MaxResults absent -> HTTP 200 with ComplianceSummaryItems == [].

Actual: MaxResults=1 -> HTTP 200, body {'ComplianceSummaryItems': []} (content-type application/x-amz-json-1.1). Check failure: "AssertionError: expected the call to be rejected, but it succeeded: {'ComplianceSummaryItems': [], 'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}" (the request id and date in that response are per-run values and no part of the requirement). The run stops on the first value, so 2, 49 and 51 were not reached.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it; point an ssm client at it.
2. Call ssm ListComplianceSummaries(Filters=[{'Key':'ComplianceType','Values':['<a ComplianceType no PutComplianceItems call ever used>']}], MaxResults=1). Observe HTTP 200 with an empty ComplianceSummaryItems instead of 400 ValidationException 'ListComplianceSummaries only support MaxResults as null or 50'.
3. Repeat with MaxResults=2 and MaxResults=49 — same wrong 200.
4. Repeat with MaxResults=51 and confirm the refusal is 400 ValidationException with "1 validation error detected: Value '51' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 50".
5. Confirm MaxResults=50 and an absent MaxResults are still served 200 with ComplianceSummaryItems == [].
