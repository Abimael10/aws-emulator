Issue-Source: trace-sweep-divergence
Issue-Trace: artifact-catalog-list-reports
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: artifact-catalog-list-reports
Issue-Shape-Id: com.amazonaws.artifact#Artifact
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-073F0fUCdVVNKJrF:3"].periodEnd
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-073F0fUCdVVNKJrF:3"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].name
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0c1IXmudeXuTlHJJ:3"].description
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0c1IXmudeXuTlHJJ:3"].name
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0c1IXmudeXuTlHJJ:3"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0c1IXmudeXuTlHJJ:3"].series
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"].name
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"].periodEnd
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"].periodStart
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
Issue-Related: 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports.md
Issue-Commit: 2901dc6a3b1a6d3c36a8ae908870ebb8c494c947

# artifact ListReports catalog projection: page omits published reports and pairs ReportSummary members with the wrong catalog entry

> _Note: this issue was identified against the code base at commit `2901dc6a3b1a6d3c36a8ae908870ebb8c494c947`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListReports

## Description
Cause 1 of 2: the first ListReports page omits published report resources that the recorded AWS catalog returns.
AWS: Smithy: ListReports GET /v1/report/list, output ListReportsResponse.reports: ReportsList of ReportSummary, paginated {inputToken: nextToken, outputToken: nextToken, pageSize: maxResults, items: reports}, maxResults @range min 1 max 300. AWS returns the AWS Artifact report catalog: every published report resource (one ReportSummary per report id + version) is reachable across the paginated result, and a page carries up to maxResults entries with a nextToken when more remain. The recorded response for this request enumerates several hundred report ARNs of the form arn:aws:artifact:<region>::report/report-<16 alnum>:<version>.
Emulator: the response is HTTP 200 but its reports list does not contain many of the report ARNs the recording returns on the same page — e.g. $.reports[arn="arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2"] is reported 'missing', along with a long list of further ARNs in the same cluster (report-0nXRsThNM4eHtuyn:4, report-0rFcyFMKMKeV4r7E:1, report-0tuV14iFC4pwSHN8:1, report-LIE0oFai46aosWjf:58, report-fKrDN0SZZokcr6rx:29, report-pMuIoxgVQUTXLcGa:30, ... ). The emulator's catalog is a small hand-written subset rather than the full report catalog, so the page cannot be completed and no nextToken can carry the remainder.
Why: a client that enumerates AWS Artifact to find a compliance document (ISO, SOC, PCI, ACR/VPAT, IAR, ...) cannot see the report it needs; GetReport/GetReportMetadata/GetTermForReport for an omitted report id then fail, so an audit-evidence download workflow is unreachable even though the list call succeeds.
Fix: the artifact ListReports handler's catalog state (the seeded report-catalog table the list projection reads) must carry the full set of published report resources, and the handler must page over it with maxResults/nextToken rather than returning a truncated built-in list. Generated report ids, versions and the host/region in the ARN are environment values, not requirements — what is required is that every seeded published report appears exactly once across the pages.
Traces: artifact-catalog-list-reports#task#1 (ListReports, expected 200, actual 200, body divergence).

Cause 2 of 2: for the reports the emulator does return, the ReportSummary members are projected from the wrong catalog entry, and the period timestamps are synthesized.
AWS: Smithy: ReportSummary members name (ShortStringAttribute @length 1..256, pattern ^[a-zA-Z0-9_\-\s]*$), description (LongStringAttribute @length 1..2048), series, category, companyName, productName (ShortStringAttribute), periodStart / periodEnd (TimestampAttribute, @timestampFormat date-time), id (pattern ^report-[a-zA-Z0-9]{16}$), version, arn, state, uploadState, acceptanceType. Each ReportSummary's members describe the report resource named by its own id/arn/version: the name, description and series belong to that report, and periodStart/periodEnd are that report's effective reporting period. Recorded behavior shows that pairing, e.g. report-5eevElBv6MyRmQsD:7 carries name 'United Arab Emirates (UAE) Information Assurance Regulation (IAR) Compliance Assessment Report' with periodStart 2024-11-01T00:00:00Z / periodEnd 2025-10-31T00:00:00Z, and report-0c1IXmudeXuTlHJJ:3 carries name 'Accessibility Conformance Report (VPAT) - Amazon Polly', series 'ACR' and periodStart 2025-10-01T00:00:00Z with the matching ACR/VPAT description.
Emulator: the same ARNs come back carrying another report's metadata and a constant period. For report-5eevElBv6MyRmQsD:7 it answers name 'UAE IAR Compliance Report', periodStart '2025-01-01T00:00:00.000Z', periodEnd '2025-07-01T00:00:00.000Z'. For report-0c1IXmudeXuTlHJJ:3 it answers name 'ISO 27001 2022 Certification', series 'ISO', the ISO/IEC 27001:2022 certificate description, and periodStart '2025-01-01T00:00:00.000Z' instead of the ACR/VPAT entry's name, series and description. Further entries in the cluster diverge the same way on .name, .description, .series, .periodStart and .periodEnd (report-073F0fUCdVVNKJrF:3, report-0QjvdIs6Bsw4uPxb:2, ...). Two separate faults are visible: (a) the summary members are taken from a different catalog entry than the one the id/arn names, and (b) periodStart/periodEnd are emitted as a fixed synthesized window (a Jan-1/Jul-1 pair of the current year) rather than the report's own effective period, and with millisecond precision rather than the recorded second-precision RFC3339 rendering.
Why: a client filtering or displaying the catalog by series/name/period picks the wrong document — an ACR/VPAT entry is shown as an ISO 27001 certificate, and every report appears to cover the same reporting window — so compliance-evidence selection by reporting period or series silently returns the wrong report, while the abbreviated name also disagrees with the document a downloaded report actually is.
Fix: the artifact ListReports response serializer/projection must build each ReportSummary from the catalog entry keyed by that report's own id and version, and must carry that entry's stored periodStart/periodEnd through to the wire as @timestampFormat date-time instead of computing a placeholder window; the catalog state must store the full report name, description and series, not an abbreviation. The particular report ids, names and dates above are recorded example values, not requirements — the requirement is that each summary's members come from the entry its arn names.
Traces: artifact-catalog-list-reports#task#1 (ListReports, expected 200, actual 200, body divergence on $.reports[...].name/.description/.series/.periodStart/.periodEnd).

Related but not covering: the open issue 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports.md ('artifact ListReports first page both omits published reports and misprojects ReportSummary members (2 causes)') describes a similar fault, but the replay still reports this cluster with no owning issue (owned_by is empty) — its recorded keys do not cover the member-misprojection and period-timestamp keys this replay flags, so this divergence is filed with its own replay key.

## Evidence
Fixture artifact-catalog-list-reports, step task#1, operation ListReports (GET /v1/report/list). Expected status 200, actual status 200; verdict mismatch, so the divergence is entirely in the body.

Omitted members (cause 1) — reported as 'missing' against the recorded page, e.g.:
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2"]: missing
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-0nXRsThNM4eHtuyn:4"]: missing
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-0rFcyFMKMKeV4r7E:1"]: missing
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-LIE0oFai46aosWjf:58"]: missing
  (several hundred further report ARNs in the same cluster)

Misprojected members (cause 2) — expected (recorded AWS) vs actual (emulator):
  $.reports[...report-5eevElBv6MyRmQsD:7].name: expected 'United Arab Emirates (UAE) Information Assurance Regulation (IAR) Compliance Assessment Report', got 'UAE IAR Compliance Report'
  $.reports[...report-5eevElBv6MyRmQsD:7].periodStart: expected '2024-11-01T00:00:00Z', got '2025-01-01T00:00:00.000Z'
  $.reports[...report-5eevElBv6MyRmQsD:7].periodEnd: expected '2025-10-31T00:00:00Z', got '2025-07-01T00:00:00.000Z'
  $.reports[...report-0c1IXmudeXuTlHJJ:3].name: expected 'Accessibility Conformance Report (VPAT) - Amazon Polly', got 'ISO 27001 2022 Certification'
  $.reports[...report-0c1IXmudeXuTlHJJ:3].series: expected 'ACR', got 'ISO'
  $.reports[...report-0c1IXmudeXuTlHJJ:3].description: expected the ITI VPAT/ACR accessibility-conformance text, got the ISO/IEC 27001:2022 certificate text
  $.reports[...report-0c1IXmudeXuTlHJJ:3].periodStart: expected '2025-10-01T00:00:00Z', got '2025-01-01T00:00:00.000Z'
  (same shape of divergence on report-073F0fUCdVVNKJrF:3 .periodStart/.periodEnd and report-0QjvdIs6Bsw4uPxb:2 .name/.periodStart)

Model grounding: smithy_query operation=ListReports and shape=ReportSummary on services/service-com-amazonaws-artifact-artifact/smithy-model/com-amazonaws-artifact-artifact.json — ListReports is @paginated (items: reports, pageSize: maxResults @range 1..300, token nextToken) and ReportSummary.periodStart/.periodEnd are TimestampAttribute with @timestampFormat date-time.

Divergence key: service=artifact|ListReports|status=200->200|keys=$.reports[...] (cluster of missing entries plus .name/.description/.series/.periodStart/.periodEnd mismatches). owned_by: [] (no open issue carries this replay key), stub_path: '' (operation is implemented).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
2. Fixture artifact-catalog-list-reports, step task#1 issues the recorded AWS Artifact call GET /v1/report/list (ListReports, no maxResults/nextToken) against the emulator's artifact endpoint.
3. Compare the 200 response body with the recorded one: the reports list is missing a large set of report ARNs present in the recording (e.g. arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2).
4. For a report ARN that IS returned, compare its ReportSummary members with the recorded entry for the same arn: name, description and series come from a different catalog entry, and periodStart/periodEnd are a fixed synthesized window ('2025-01-01T00:00:00.000Z' / '2025-07-01T00:00:00.000Z') instead of that report's recorded effective period.
5. Expected: every published report in the catalog is reachable across the paginated result, and each ReportSummary's name/description/series/periodStart/periodEnd are the ones stored for the report its id/arn names, serialized as RFC3339 date-time.
