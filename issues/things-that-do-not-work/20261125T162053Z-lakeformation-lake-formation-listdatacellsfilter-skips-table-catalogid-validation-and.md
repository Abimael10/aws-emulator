Issue-Source: trace-sweep-divergence
Issue-Trace: lakeformation-data-cells-filter-lifecycle
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lakeformation-data-cells-filter-lifecycle
Issue-Shape-Id: com.amazonaws.lakeformation#AWSLakeFormation
Issue-Key: lakeformation|ListDataCellsFilter|4xx->2xx|$.Message
Issue-Key: lakeformation|ListDataCellsFilter|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: lakeformation|ListDataCellsFilter|4xx->2xx|status
Issue-Commit: 990310f3c08624998ca7ab018ed844542a47dd84

# Lake Formation ListDataCellsFilter skips Table.CatalogId validation and answers 200 instead of 400 InvalidInputException

> _Note: this issue was identified against the code base at commit `990310f3c08624998ca7ab018ed844542a47dd84`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDataCellsFilter

## Description
1 net-new cause.

Cause 1 of 1: ListDataCellsFilter does not validate the Table.CatalogId member of its request, so a request carrying a null/empty CatalogId is served as a successful listing instead of being rejected.

AWS: ListDataCellsFilter takes a single `Table` member of shape TableResource and models InvalidInputException (errorKind client, httpError 400) among its errors. Smithy: ListDataCellsFilter.Table -> TableResource; TableResource.CatalogId -> CatalogIdString with @length(min: 1, max: 255) and @pattern "^[\\u0020-\\uD7FF\\uE000-\\uFFFD\\uD800\\uDC00-\\uDBFF\\uDFFF\\t]*$"; error InvalidInputException {Message: MessageString}, documented as "The input provided was not valid." A CatalogId supplied as an empty string (or an explicit null) violates the min length of 1, so the request is invalid input and AWS rejects it before listing anything. The recorded response is HTTP 400 with the error shaped in the body as {"Message": "CatalogId should not be null or empty"} and the error type carried in the `x-amzn-errortype` response header (the recording asserts the header is present and shaped, not its literal value). Message template: the fixed text is "CatalogId should not be null or empty" — the whole string is fixed text and the variable part is only the member name "CatalogId", i.e. the offending request member, taken from the request rather than from any recorded run. No per-run value (account id, catalog id, request id, timestamp) appears in it.

Emulator: the replayed step returns HTTP 200 with a normal ListDataCellsFilter response body. The replay reports `status: expected 400, got 200`, `$.Message: missing (expected 'CatalogId should not be null or empty', got None)` and `$headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)`. So neither the status, nor the InvalidInputException body, nor the error-type header is produced: the invalid CatalogId is accepted and the operation proceeds to list filters.

Why: a client that relies on AWS's behavior to surface a mis-built request gets a silent success. SDK call sites that catch InvalidInputException to report a bad catalog identifier never see an exception, and the empty CatalogId is instead silently coerced (presumably to the caller's own account catalog), so the client believes it enumerated the data cells filters of the catalog it named while it actually read a different one. Paging and "filter not found" logic built on that answer draws the wrong conclusion, and a validation bug in the caller passes CI against the emulator and only fails against real Lake Formation.

Fix: belongs in the ListDataCellsFilter request-validation step of the lakeformation operation handler, ahead of the state lookup that enumerates the filters. It must reject a Table.CatalogId that is absent-but-present-as-empty or otherwise violates CatalogIdString's @length/@pattern constraints by returning the modeled InvalidInputException: HTTP 400, error type InvalidInputException in the x-amzn-errortype header, and a body whose Message is "CatalogId should not be null or empty" with the member name taken from the request. The same empty-string check should be applied consistently wherever TableResource.CatalogId is read, not only on this operation.

Traces: lakeformation-data-cells-filter-lifecycle#task#8 (the only member of this divergence cluster; divergence_key service=lakeformation|ListDataCellsFilter|status=400->200|keys=$.Message,$headers.x-amzn-errortype,status).

Related open issues, neither of which covers this operation or this cause: 20261120T155738Z-lakeformation-lake-formation-grantpermissions-accepts-an-invalid-permissions-modification.md (GrantPermissions accepting an invalid permissions modification) and 20261125T031909Z-lakeformation-lake-formation-listpermissions-synthesizes-lastupdated-and-lastupdatedby-on.md (ListPermissions synthesizing LastUpdated/LastUpdatedBy).

## Evidence
Replay of fixture lakeformation-data-cells-filter-lifecycle, step task#8, operation ListDataCellsFilter (service lakeformation), verdict mismatch.

Expected (recorded AWS behavior):
  HTTP 400
  body: {"Message": "CatalogId should not be null or empty"}
  header x-amzn-errortype: present (shaped, value not asserted) -> InvalidInputException

Actual (emulator):
  HTTP 200
  body: a successful ListDataCellsFilter response; no Message member
  header x-amzn-errortype: absent

Replay detail, verbatim:
  status: expected 400, got 200
  $.Message: missing (expected 'CatalogId should not be null or empty', got None)
  $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)

Model evidence: Smithy ListDataCellsFilter input ListDataCellsFilterRequest{Table: TableResource, NextToken, MaxResults}; errors include InvalidInputException (client, httpError 400); TableResource.CatalogId -> CatalogIdString @length(min 1, max 255), so an empty CatalogId is invalid input.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1); fixture lakeformation-data-cells-filter-lifecycle, step task#8.
3. Equivalently, by hand: POST /ListDataCellsFilter to the lakeformation endpoint with a body whose Table object carries an empty (or null) CatalogId together with the DatabaseName and table Name used by the fixture, e.g. {"Table": {"CatalogId": "", "DatabaseName": "<db>", "Name": "<table>"}}.
4. Observe HTTP 200 and a listing body with no Message and no x-amzn-errortype header.
5. Expected: HTTP 400, x-amzn-errortype naming InvalidInputException, body {"Message": "CatalogId should not be null or empty"}.
