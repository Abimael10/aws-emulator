Issue-Source: trace-sweep-divergence
Issue-Trace: artifact-compliance-inquiry-basic-workflow
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: artifact-compliance-inquiry-basic-workflow artifact-catalog-list-reports
Issue-Shape-Id: com.amazonaws.artifact#Artifact
Issue-Key: artifact|ListComplianceInquiryQueries|2xx->2xx|$.queries[*]
Issue-Key: artifact|ListComplianceInquiryQueries|2xx->2xx|$.queries[*].citations[*].sourceLink
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-073F0fUCdVVNKJrF:3"].periodEnd
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-073F0fUCdVVNKJrF:3"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].name
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-7g5j2DTVeNXWx7St:1"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-9pQ2mT8xLbV4sZ1c:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-Af3KdN7yRw2HqS5e:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-Bq7WmF3xTk9ZpL2a:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-C4tYhR8nJd6VxQ3b:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-D9kPzX2mLb7TnR4c:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-E2nVbK6yQw8MsZ5d:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-F6sJdL9xRt3WqN7e:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-G3mQfT7bXz5KpV8h:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-H8wRnP4cYk2JsL6m:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-J5zXkM9dVb3QtR7n:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-K7bNqS2fWx8LmT4p:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-L4dTvJ8hZn6RkQ9s:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-M2hXsL5bQw9TnF4j:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-N7kDvR3mYt8ZpB6c:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-P5qZmJ8hXc2VtL9d:3"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-Q8tBnW4fKz6RmS3h:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-R3vYkT9pLd5XnQ7b:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-S6mFqH2xVb8JtZ4n:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-T5xWnQ8dKb3VzL6p:3"]
Issue-Related: 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports.md
Issue-Commit: e775a2fec8b674e0d0a3cde44ce8df5c9a1c0b14

# artifact compliance-inquiry and report-catalog list responses drop recorded records and the millisecond component of date-time members

> _Note: this issue was identified against the code base at commit `e775a2fec8b674e0d0a3cde44ce8df5c9a1c0b14`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListComplianceInquiryQueries

## Description
Two net-new wire divergences on the artifact service, both on a 200 response whose body does not carry the records AWS returns. Both are response-projection defects in a paginated List* operation: the list member is served from an incomplete/short projection, and the TimestampAttribute members that ARE served lose their fractional-second component. 2 causes.

=== Cause 1 of 2: ListComplianceInquiryQueries returns a queries list with no element for a query the inquiry already accepted (fixture artifact-compliance-inquiry-basic-workflow, step task#3) ===

AWS: Smithy: ListComplianceInquiryQueries is GET /v1/compliance-inquiry/listQueries, httpResponseCode 200, input complianceInquiryId (httpQuery, @required, @pattern ^compliance-inquiry-[a-zA-Z0-9]{16}$) plus maxResults (@range 1..300) and nextToken (@length 1..2048); output is { queries: QueriesList, nextToken } and the operation is @paginated(inputToken: nextToken, outputToken: nextToken, pageSize: maxResults, items: "queries"). QueriesList is a list of QuerySummary, whose members queryIdentifier (Integer), query (LongStringAttribute @length 1..2048 @pattern ^[^<>]*$), status (QueryStatus enum), statusMessage (QueryStatusMessage enum) and createdAt (TimestampAttribute @timestampFormat date-time) are @required, with optional response, reviewType, updatedResponseVersions and citations -> CitationList of Citation { sourceLabel (ShortStringAttribute @length 1..256), sourceContent (LongStringAttribute @length 1..2048), sourceLink (LongStringAttribute @length 1..2048) }. The model fixes the shape, not the values, so for the content this is recorded behavior only: the recorded 200 body enumerates one QuerySummary per query submitted into the named compliance inquiry, each carrying that query's generated response text and its supporting Citation entries, every Citation populating all three of sourceLabel, sourceContent and sourceLink (in the recording sourceLabel and sourceLink are both the source document's URL, and sourceContent is the quoted passage from it). Which QuerySummary entries a page carries is determined by which queries the inquiry identified by complianceInquiryId contains, never by the emulator's own list length.

Emulator: the call answers HTTP 200 (expected 200, actual 200 - no status or error-code divergence), but the queries list has no element at index 0 at all. Verbatim from the replay detail: `$.queries[0]: missing (expected {'citations': [{'sourceContent': 'Amazon S3 is an AWS managed service, and third-party auditors regularly assess the security and compliance of it as part of multiple AWS compliance programs.', 'sourceLabel': 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/', 'sourceLink': 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/'}, ... ]}, got None)` - i.e. the query the earlier step of the same fixture submitted into the inquiry is not projected into ListComplianceInquiryQueries, so the operation reports the inquiry as having no queries. The cluster's normalized divergence key is `service=artifact|ListComplianceInquiryQueries|status=200->200|keys=$.queries[0],$.queries[0].citations[0].sourceLink,$.queries[0].citations[1].sourceLink,$.queries[0].citations[2].sourceLink`: besides the whole-entry path it names Citation.sourceLink at all three citation positions, so sourceLink is unmet independently of the element being absent. Hypothesis: the inquiry's queries are not persisted into (or not read back from) the state ListComplianceInquiryQueries projects, and the Citation projection omits sourceLink even when it does build an entry - the three sourceLink keys would not be enumerated separately if sourceLink were simply carried along with the rest of the entry.

Why: a client relying on AWS's behavior cannot observe its own compliance inquiry. Code that submits a query and then polls ListComplianceInquiryQueries for status/statusMessage to reach a terminal value never sees the query appear, so it either loops until timeout or concludes the submission was lost; code that reads the generated response and renders its citations for an auditor gets an empty inquiry, and any attestation built on sourceLink has no source URL to link to, which is exactly the evidence trail the citations exist to provide.

Fix: the ListComplianceInquiryQueries handler and the compliance-inquiry state it reads - the query records accepted by the inquiry must be stored against their complianceInquiryId and enumerated by this operation (paging with nextToken per the @paginated trait) - plus the QuerySummary/Citation projection in its serializer, which must populate Citation.sourceLink alongside sourceLabel and sourceContent.

Traces: artifact-compliance-inquiry-basic-workflow#task#3 (ListComplianceInquiryQueries, verdict mismatch, 200 -> 200).

=== Cause 2 of 2: ListReports first page still omits most of the published report catalog and still renders periodStart/periodEnd without milliseconds (fixture artifact-catalog-list-reports, step task#1) ===

AWS: Smithy: ListReports is GET /v1/report/list, httpResponseCode 200, output { reports: ReportsList, nextToken }, @paginated(inputToken: nextToken, outputToken: nextToken, pageSize: maxResults, items: "reports"), maxResults @range 1..300. ReportsList is a list of ReportSummary, whose members include id -> ReportId (@pattern ^report-[a-zA-Z0-9]{16}$), arn -> LongStringAttribute (@length 1..2048), version -> VersionAttribute (long, @range min 1), name/series/category/companyName/productName -> ShortStringAttribute (@length 1..256), description -> LongStringAttribute (@length 1..2048), state -> PublishedState, acceptanceType -> AcceptanceType, and periodStart/periodEnd -> TimestampAttribute with @timestampFormat date-time. The model fixes the shape, not the values, so for the content this is recorded behavior only: the recorded 200 response enumerates the published report catalog - every PUBLISHED report resource, keyed by its arn of the form arn:aws:artifact:<region>::report/<reportId>:<version> - and each entry's members are the stored metadata of the report that entry's id/arn/version identifies. The recording renders periodStart/periodEnd as an RFC 3339 date-time that carries the three-digit fractional-second component and a Z offset (YYYY-MM-DDTHH:MM:SS.mmmZ); that rendering, not any one instant, is the requirement - the date and time of a given entry are catalog content keyed by the report id, and the '.mmm' group is fixed text of the format.

Emulator: the call answers HTTP 200 (expected 200, actual 200), but the body diverges in two ways that this replay still shows. (a) Entries absent: the recorded catalog entries have no counterpart in the emulator's reports list, reported as member-less whole-entry paths, e.g. `$.reports[arn="arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2"]: missing (expected {...'id': 'report-CKWRe4CwVxUEA8yg', 'name': 'AWS User Guide to the European Banking Authority Guidelines and Bank of Italy's Circular 285/2013', 'state': 'PUBLISHED', 'version': 2, 'visibility': 'PUBLIC'}, got None)` and `$.reports[arn="arn:aws:artifact:us-east-1::report/report-EoPCqoQjoEEMygkU:1"]: missing (expected {...'name': 'Accessibility Conformance Report (VPAT) - Amazon SageMaker Neo'...}, got None)`; the cluster's divergence key repeats hundreds of such whole-entry paths (report-0nXRsThNM4eHtuyn:4, report-0rFcyFMKMKeV4r7E:1, report-0tuV14iFC4pwSHN8:1, report-LIE0oFai46aosWjf:58, ... ), so the served catalog is a small subset of the published one rather than a paging artefact. (b) date-time members lose their fractional second: `$.reports[arn="arn:aws:artifact:us-east-1::report/report-7g5j2DTVeNXWx7St:1"].periodStart: value_mismatch (expected '2024-11-01T00:00:00.000Z', got '2024-11-01T00:00:00Z')` - the instant now agrees and only the '.000' group is dropped - and the divergence key names the same member for further entries (report-073F0fUCdVVNKJrF:3 .periodStart and .periodEnd, report-0QjvdIs6Bsw4uPxb:2 .periodStart), together with report-0QjvdIs6Bsw4uPxb:2 .name, so a present entry can still carry a name that is not the stored one. Open issue 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports.md describes the ListReports omission and member misprojection, but the replay reports this cluster as NOT owned (owned_by: []) and its key set has moved since that issue was written - the entry content it quotes has partly changed (periodStart for report-7g5j2DTVeNXWx7St:1 is now the correct instant, only without milliseconds), while the omissions and the millisecond rendering persist - so the cluster is filed here with its current replay key rather than left to a pointer that no longer matches it.

Why: a client relying on AWS's behavior cannot enumerate the compliance catalog. Report pickers and compliance automation that page ListReports to the end and then GetReport by id simply do not see most published reports - an auditor looking for an EBA/Circular-285 guide or a SageMaker Neo VPAT finds nothing to download - and a present entry whose name is not the stored one is selected under the wrong title. Dropping the fractional-second group makes periodStart/periodEnd compare unequal to the same instant as AWS and any recorded response serialize it, so string-equality checks, cache keys and golden-file comparisons over the reporting period all fail even where the entry is otherwise right.

Fix: the ListReports handler and the seeded report catalog in the artifact service's state (the full set of PUBLISHED report resources must be enumerable across nextToken pages, and each ReportSummary member must come from the stored report identified by that entry's id/arn/version), plus the ReportSummary serializer's TimestampAttribute rendering, which must emit date-time with the three-digit fractional-second component for periodStart and periodEnd.

Traces: artifact-catalog-list-reports#task#1 (ListReports, verdict mismatch, 200 -> 200).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, bootstrap_signal=false, 11 fixtures replayed, 31 steps, 29 passed, 2 mismatched, 0 errored, 0 blocked, score 93.5%. Both divergence clusters are service=artifact with stub_path="" and owned_by=[].

Cause 1 - fixture artifact-compliance-inquiry-basic-workflow, step task#3, operation ListComplianceInquiryQueries, verdict mismatch, expected_status 200, actual_status 200, affected_count 1.
  Expected (recorded AWS): $.queries[0] present - a QuerySummary carrying the submitted query with its generated response and a citations array of three Citation entries, each with sourceLabel, sourceContent AND sourceLink populated, e.g. sourceContent 'Amazon S3 is an AWS managed service, and third-party auditors regularly assess the security and compliance of it as part of multiple AWS compliance programs.' with sourceLabel/sourceLink 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/'.
  Actual (emulator): $.queries[0]: missing (... got None) - the queries list has no entry for the query the inquiry accepted.
  Divergence key: service=artifact|ListComplianceInquiryQueries|status=200->200|keys=$.queries[0],$.queries[0].citations[0].sourceLink,$.queries[0].citations[1].sourceLink,$.queries[0].citations[2].sourceLink

Cause 2 - fixture artifact-catalog-list-reports, step task#1, operation ListReports, verdict mismatch, expected_status 200, actual_status 200, affected_count 1.
  Expected vs actual, per diverging JSON path:
    $.reports[arn="arn:aws:artifact:us-east-1::report/report-7g5j2DTVeNXWx7St:1"].periodStart -> expected '2024-11-01T00:00:00.000Z', got '2024-11-01T00:00:00Z' (value_mismatch; only the three-digit fractional-second group is missing)
    $.reports[arn="arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2"] -> expected the full ReportSummary (id report-CKWRe4CwVxUEA8yg, version 2, state PUBLISHED, series 'Laws and Regulations', category 'Alignment Documents', acceptanceType PASSTHROUGH, periodStart '2023-06-01T00:00:00.000Z'), got None (entry absent)
    $.reports[arn="arn:aws:artifact:us-east-1::report/report-EoPCqoQjoEEMygkU:1"] -> expected the full ReportSummary (id report-EoPCqoQjoEEMygkU, version 1, category 'Certifications and Attestations', acceptanceType PASSTHROUGH, periodStart '2024-11-01T00:00:00.000Z'), got None (entry absent)
  Divergence key (abridged) names hundreds of further member-less whole-entry paths - report-073F0fUCdVVNKJrF:3 (.periodStart, .periodEnd), report-0QjvdIs6Bsw4uPxb:2 (.name, .periodStart), report-0nXRsThNM4eHtuyn:4, report-0rFcyFMKMKeV4r7E:1, report-0tuV14iFC4pwSHN8:1, report-LIE0oFai46aosWjf:58, report-fKrDN0SZZokcr6rx:29, report-pMuIoxgVQUTXLcGa:30 - all under service=artifact|ListReports|status=200->200.

Model evidence consulted (services/service-com-amazonaws-artifact-artifact/smithy-model/com-amazonaws-artifact-artifact.json):
  - ListReports: GET /v1/report/list, code 200, output { reports: ReportsList, nextToken: NextTokenAttribute @length 1..2048 }, @paginated(items: reports, pageSize: maxResults @range 1..300), @readonly.
  - ReportSummary: id -> ReportId @pattern ^report-[a-zA-Z0-9]{16}$; arn -> LongStringAttribute @length 1..2048; version -> VersionAttribute long @range min 1; name/series/category/companyName/productName -> ShortStringAttribute @length 1..256; description -> LongStringAttribute @length 1..2048; state -> PublishedState; acceptanceType -> AcceptanceType; periodStart/periodEnd -> TimestampAttribute @timestampFormat date-time.
  - ListComplianceInquiryQueries: GET /v1/compliance-inquiry/listQueries, code 200, complianceInquiryId @required httpQuery @pattern ^compliance-inquiry-[a-zA-Z0-9]{16}$, output { queries: QueriesList, nextToken }, @paginated(items: queries), @readonly.
  - QuerySummary: queryIdentifier/query/status/statusMessage/createdAt @required; citations -> CitationList of Citation { sourceLabel, sourceContent, sourceLink }; createdAt -> TimestampAttribute @timestampFormat date-time.

No per-run value is part of either claim: no request id, clock reading, host, port or generated name is asserted. The report ids/arns and citation URLs quoted are stored catalog content keyed by the resource the entry names, and the '.mmm' fractional-second group is fixed text of the date-time format.

## Steps to Reproduce
Cause 1 (ListComplianceInquiryQueries - fixture artifact-compliance-inquiry-basic-workflow, step task#3):
1. Boot the emulator from this checkout and point an AWS Artifact client at its data listener.
2. Replay fixture artifact-compliance-inquiry-basic-workflow in order: it starts a compliance inquiry and submits a compliance query against it (earlier steps pass), then at step task#3 calls ListComplianceInquiryQueries with complianceInquiryId set to the inquiry created above (GET /v1/compliance-inquiry/listQueries?complianceInquiryId=compliance-inquiry-<16 alnum>).
3. The call answers HTTP 200. Read $.queries: it has no element at index 0, so the query accepted in step 2 is not enumerated.
4. Expected: $.queries carries one QuerySummary per query in that inquiry, each with its required queryIdentifier/query/status/statusMessage/createdAt, its generated response, and a citations array whose every Citation populates sourceLabel, sourceContent AND sourceLink; page with nextToken when the queries do not fit one page.

Cause 2 (ListReports - fixture artifact-catalog-list-reports, step task#1):
1. Boot the same emulator.
2. Call ListReports (GET /v1/report/list), following nextToken to the end of the listing.
3. The call answers HTTP 200. Collect the set of entry arns the emulator returns across all pages and diff it against the recorded catalog: recorded arns such as arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2, .../report-EoPCqoQjoEEMygkU:1, .../report-0nXRsThNM4eHtuyn:4 and .../report-LIE0oFai46aosWjf:58 appear in no page.
4. For an arn the emulator DOES return - e.g. .../report-7g5j2DTVeNXWx7St:1 - read .periodStart: it is serialized as date-time WITHOUT the fractional-second group ('2024-11-01T00:00:00Z' where the recording has '2024-11-01T00:00:00.000Z'). Repeat for .../report-073F0fUCdVVNKJrF:3 (.periodStart and .periodEnd) and .../report-0QjvdIs6Bsw4uPxb:2 (.periodStart, and .name which is not the stored name for that arn).
5. Expected: the listing enumerates every PUBLISHED report of the stored catalog across its nextToken pages, each ReportSummary member equals the stored metadata of the report that entry's id/arn/version identifies, and periodStart/periodEnd are rendered as date-time including the three-digit fractional-second component.
