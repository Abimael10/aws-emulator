Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resource-explorer-2.search-and-list-resources-filter-string-validation-differs
Issue-Shape-Id: com.amazonaws.resourceexplorer2#ResourceExplorer
Issue-Key: resource-explorer-2.search-and-list-resources-filter-string-validation-differs|expect_error#5|AssertionError|raise AssertionError(f"expected {(code, message)!r}, but the call succeeded: {result!r}")
Issue-Commit: 8b0a0326e7d91c3197ca4e29d037f7bc2fabea56

# ListResources accepts a malformed Filters.FilterString (bare "service:") and returns 200 instead of ValidationException

> _Note: this issue was identified against the code base at commit `8b0a0326e7d91c3197ca4e29d037f7bc2fabea56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListResources

## Description
Cause 1 of 1: ListResources performs no syntax validation of Filters.FilterString, so a filter string that is not composed solely of supported AREX filter operators is silently treated as a matching-nothing query instead of being refused. Failing check: resource-explorer-2.search-and-list-resources-filter-string-validation-differs.

AWS: Smithy: ListResources.Filters -> SearchFilter (@sensitive), SearchFilter.FilterString is required String with @length min 0 max 2048, documented as "the search keywords, prefixes, and operators to control the results"; ListResources models ValidationException (client, httpError 400) with a required Message member. The model states only the length bound, so the operator-syntax rule itself is recorded behavior only: real AWS refuses a FilterString carrying a bare operator prefix with no value (an operator token such as "service:" with an empty value, i.e. freeform text rather than a complete operator/value pair) with HTTP 400, Error.Code "ValidationException" and Error.Message "FilterString must contain only AREX supported filter operators and no freeform text." That message is entirely fixed text for this error class - it has no variable parts, so it must be emitted verbatim and not templated with the submitted filter string.

Note the asymmetry the same check exercises and which the emulator gets right: Search.QueryString is NOT restricted to operator syntax, and the identical "service:" string is accepted there and completes with zero matches. Only ListResources' Filters.FilterString is validated. The fix must not extend the refusal to Search.

Emulator: ListResources(ViewArn=<view>, Filters={"FilterString": "service:"}, MaxResults=5) returns HTTP 200 with body {"Resources": [], "ViewArn": "<the requested view arn>"} - no error at all. The well-formed neighbour "service:sqs" also returns 200 with an empty Resources list and the echoed ViewArn, so the emulator makes no distinction between a well-formed and a malformed filter string. The malformed call is still accepted on repeat after intervening Search calls.

Why: a client that relies on AWS rejecting a malformed filter cannot distinguish "your filter is syntactically invalid" from "your filter is valid and matched nothing". Code that builds FilterStrings programmatically (string-concatenating tag keys/values or service names, where an empty value yields a bare "key:" prefix) sees a successful empty page against the emulator and silently reports "no resources" instead of surfacing the 400 that AWS returns; tests that assert the ValidationException contract pass in real AWS and fail here, and defensive error handling for ValidationException is never exercised.

Fix: the ListResources handler's input validation for the Filters member - FilterString must be parsed/validated against the supported AREX filter-operator grammar before the query is executed, and rejected with a 400 ValidationException carrying the exact message above when it contains an operator with no value or any freeform (non-operator) text. The length bound already modelled on SearchFilter.FilterString is unrelated and not the gap. Search's QueryString validation must be left as-is.

Traces: resource-explorer-2.search-and-list-resources-filter-string-validation-differs - assertion raised at the first expect_error, "expected ('ValidationException', 'FilterString must contain only AREX supported filter operators and no freeform text.'), but the call succeeded".

## Evidence
Check: resource-explorer-2.search-and-list-resources-filter-string-validation-differs (verdict: mismatch)

Request: POST /ListResources with ViewArn=<created view>, Filters={"FilterString": "service:"}, MaxResults=5

Expected (AWS): HTTP 400, Error.Code "ValidationException", Error.Message "FilterString must contain only AREX supported filter operators and no freeform text." (fixed text, no variable parts).

Actual (emulator): HTTP 200, content-type application/json, body {"Resources": [], "ViewArn": "arn:aws:resource-explorer-2:<region>:<account>:view/<view-name>/<view-id>"} - the call succeeded. (The arn/name/id in the recorded run are per-run values, not part of the requirement.)

Harness assertion: AssertionError: expected ('ValidationException', 'FilterString must contain only AREX supported filter operators and no freeform text.'), but the call succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'Resources': [], 'ViewArn': '...'}

Positive controls in the same check that behave correctly: ListResources with "service:sqs" returns 200 + Resources list + echoed ViewArn; Search(QueryString="service:") returns 200 with Resources [] and Count {"TotalResources": 0, "Complete": true} and the echoed ViewArn. So the gap is specifically missing FilterString syntax validation on ListResources.

## Steps to Reproduce
1. CreateIndex and poll GetIndex until State == ACTIVE.
2. CreateView with any unique ViewName; capture View.ViewArn.
3. Call ListResources(ViewArn=<that arn>, Filters={"FilterString": "service:"}, MaxResults=5).
   Expected: HTTP 400 ValidationException with message "FilterString must contain only AREX supported filter operators and no freeform text."
   Actual: HTTP 200 with {"Resources": [], "ViewArn": "<that arn>"}.
4. Confirm the intended asymmetry still holds after the fix: ListResources(ViewArn=<arn>, Filters={"FilterString": "service:sqs"}, MaxResults=5) must still return 200 with a Resources list and the echoed ViewArn, and Search(QueryString="service:", ViewArn=<arn>, MaxResults=5) must still return 200 with Resources == [] and Count == {"TotalResources": 0, "Complete": true}.
5. Repeat step 3 after the Search calls - the refusal must be stable.
