Issue-Source: trace-sweep-divergence
Issue-Trace: artifact-catalog-list-reports
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: artifact-catalog-list-reports
Issue-Shape-Id: com.amazonaws.artifact#Artifact
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-west-2::report/report-0QjvdIs6Bsw4uPxb:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-west-2::report/report-M2hXsL5bQw9TnF4j:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-west-2::report/report-N7kDvR3mYt8ZpB6c:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-west-2::report/report-P5qZmJ8hXc2VtL9d:3"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-west-2::report/report-Q8tBnW4fKz6RmS3h:1"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-west-2::report/report-R3vYkT9pLd5XnQ7b:2"]
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-west-2::report/report-S6mFqH2xVb8JtZ4n:1"]
Issue-Related: 20261107T124604Z-artifact-listreports-catalog-enumeration-omits-nearly-every-published.md
Issue-Commit: 6d844bf2d7a54913ccf026b8beb416d1976fa525

# artifact ListReports serves a truncated report catalog: the PUBLISHED reports recorded for the first page are absent (1 cause)

> _Note: this issue was identified against the code base at commit `6d844bf2d7a54913ccf026b8beb416d1976fa525`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListReports

## Description
Cause 1 of 1: GET /v1/report/list (ListReports) answers HTTP 200 with a `reports` list that is missing nearly every report the recorded AWS response returns on the same first page, so the AWS Artifact report catalog the emulator serves is not the catalog clients enumerate.

AWS: Smithy (com.amazonaws.artifact#Artifact) models ListReports as `GET /v1/report/list`, readonly, @paginated {inputToken: nextToken, outputToken: nextToken, pageSize: maxResults, items: reports}, with input maxResults @range min 1 max 300 and nextToken @length 1..2048; output is `reports: ReportsList` of `ReportSummary` plus an optional nextToken. ReportSummary members are id (@pattern ^report-[a-zA-Z0-9]{16}$), name, state (PublishedState), arn, version (long @range min 1), uploadState, description, periodStart/periodEnd (timestamp, date-time), series, category, companyName, productName, statusMessage, acceptanceType (AcceptanceType enum). The model does not enumerate the catalog contents, so the catalog itself is recorded behavior only: the recording for this step returns a first page of several hundred PUBLISHED, PUBLIC, PASSTHROUGH report summaries covering the AWS compliance catalog (SOC/PCI/ISO, the per-service VPAT/ACR series, per-jurisdiction assessments such as the UAE TDRA IAR report, etc.), each with a stable `arn` of the form arn:aws:artifact:<region>::report/<reportId>:<version> and a `version` matching the ARN's trailing version. Status is 200 on both sides; this is purely a body/content divergence.

Emulator: status 200, but the replay reports every recorded report summary as `missing` from `$.reports[...]` keyed by ARN — e.g. $.reports[arn="arn:aws:artifact:us-east-1::report/report-7g5j2DTVeNXWx7St:1"] expected the Accessibility Conformance Report (VPAT) summary and got None, $.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"] expected the UAE IAR assessment summary and got None, and so on for the whole recorded page (the cluster's divergence_key lists ~hundreds of such ARN-keyed paths). The emulator's ListReports therefore returns a report set that shares essentially no members with the catalog AWS publishes; the generic requirement is that enumerating the catalog with no filter returns the service's published report inventory (each entry carrying the ReportSummary members above, with arn/id/version mutually consistent and state PUBLISHED), paged by maxResults with nextToken continuation — not an unrelated or near-empty inventory.

Why: a client relying on AWS behavior cannot find any report it knows by id/ARN or by series/category. Compliance tooling that lists the catalog and then calls GetReportMetadata/GetReport (or GetTermForReport) for a selected report gets an empty or non-matching selection, so no report is ever retrievable through the documented list-then-get flow, and pagination over the catalog (maxResults/nextToken) enumerates the wrong population. Because the response is a 200, the client has no error to fall back on — it concludes the account simply has no such reports.

Fix: the artifact service's ListReports handler plus the seeded report catalog it reads from (the report state/catalog the operation enumerates and the ListReports serializer that projects ReportSummary); the page assembly must enumerate the full published catalog and page it with maxResults/nextToken rather than emitting a different or partial inventory.

Traces: fixture artifact-catalog-list-reports, step task#1 (service artifact, ListReports, expected 200, actual 200, verdict mismatch).

Note on existing issues: this replay cluster came back with an EMPTY owned_by, so no open issue currently carries its replay key. The open issue "artifact ListReports catalog enumeration omits nearly every published report from its first page" (issues/things-that-do-not-work/20261107T124604Z-artifact-listreports-catalog-enumeration-omits-nearly-every-published.md) describes the same fault and quotes the same fixture/step in its front matter, but its Issue-Key set is keyed to the ARN paths of an earlier run and no longer binds this cluster; whoever fixes this should close both together.

## Evidence
Replay of fixture artifact-catalog-list-reports step task#1 (service=artifact, operation=ListReports, verdict=mismatch, expected_status=200, actual_status=200, stub_path="", owned_by=[]).

Expected (recorded AWS): a 200 ListReports body whose `reports` list contains the published catalog, including (examples, values are per-recording and only illustrate the shape):
  - {id: report-7g5j2DTVeNXWx7St, arn: arn:aws:artifact:us-east-1::report/report-7g5j2DTVeNXWx7St:1, version: 1, name: "Accessibility Conformance Report (VPAT) - AWS Transfer Family", series: ACR, category: "Certifications and Attestations", companyName: AWS, productName: AWS, state: PUBLISHED, visibility: PUBLIC, acceptanceType: PASSTHROUGH, periodStart: 2024-11-01T00:00:00.000Z}
  - {id: report-5eevElBv6MyRmQsD, arn: arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7, version: 7, name: "United Arab Emirates (UAE) Information Assurance Regulation (IAR) Compliance Assessment Report", series: "UAE TDRA IAR", state: PUBLISHED, acceptanceType: PASSTHROUGH, periodStart: 2024-11-01T00:00:00Z, periodEnd: 2025-10-31T00:00:00Z}
  - ... and the remaining ARN-keyed entries listed in the cluster's divergence_key.

Actual (emulator): 200, but each of those ARN-keyed paths resolves to None — replay detail reads `$.reports[arn="arn:aws:artifact:us-east-1::report/report-7g5j2DTVeNXWx7St:1"]: missing (expected {...}, got None)` and likewise for every other recorded summary in the page.

No status or error-code divergence: both sides are 200 with no error body. The divergence is the content of `reports`.

## Steps to Reproduce
1. Boot the emulator from this checkout (the artifact service answers on the data listener).
2. Replay fixture artifact-catalog-list-reports, step task#1 — or issue the equivalent request directly:
   GET /v1/report/list  (SigV4-signed, service `artifact`, no maxResults/nextToken), i.e. `aws artifact list-reports`.
3. Read the 200 response body's `reports` array and compare it to the recorded response: look up any recorded report by ARN, e.g. arn:aws:artifact:us-east-1::report/report-7g5j2DTVeNXWx7St:1 or arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7.
4. Observed: those entries are absent from `reports` (the emulator serves a different, near-empty inventory). Expected: the published catalog, each entry a ReportSummary with id/arn/version mutually consistent and state PUBLISHED, paged with maxResults (1..300) and nextToken.
5. Follow-up check: page with `aws artifact list-reports --max-results 300` and follow nextToken — the union of pages still omits the recorded catalog.
