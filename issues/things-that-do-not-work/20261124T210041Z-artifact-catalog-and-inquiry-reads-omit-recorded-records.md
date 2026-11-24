Issue-Source: trace-sweep-divergence
Issue-Trace: artifact-catalog-list-reports
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: artifact-catalog-list-reports artifact-compliance-inquiry-basic-workflow
Issue-Shape-Id: com.amazonaws.artifact#Artifact
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-073F0fUCdVVNKJrF:3"].periodEnd
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-073F0fUCdVVNKJrF:3"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].name
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].periodStart
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-0c1IXmudeXuTlHJJ:3"].periodStart
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
Issue-Key: artifact|ListReports|2xx->2xx|$.reports[arn="arn:aws:artifact:us-east-1::report/report-T5xWnQ8dKb3VzL6p:3"]
Issue-Key: artifact|ListComplianceInquiryQueries|2xx->2xx|$.queries[*]
Issue-Key: artifact|ListComplianceInquiryQueries|2xx->2xx|$.queries[*].citations[*].sourceLink
Issue-Related: 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports.md
Issue-Commit: de014b3026fe0bf980e23e8d36c49dd531cd5e09

# artifact catalog and inquiry reads omit recorded records and mis-serialize ReportSummary members (4 causes)

> _Note: this issue was identified against the code base at commit `de014b3026fe0bf980e23e8d36c49dd531cd5e09`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListReports

## Description
Four net-new wire divergences in the `artifact` service's two paginated read operations. Both steps answer HTTP 200 on both sides; every divergence is in the response BODY. Causes 1-3 are carried by fixture artifact-catalog-list-reports step task#1 (ListReports); cause 4 by fixture artifact-compliance-inquiry-basic-workflow step task#3 (ListComplianceInquiryQueries). A fixer must close all FOUR before submitting.

=== Cause 1 of 4: ListReports' first page omits most of the published report catalog (fixture artifact-catalog-list-reports, step task#1) ===
AWS: Smithy: ListReports is `GET /v1/report/list`, success 200, output ListReportsResponse$reports -> ReportsList of ReportSummary, @paginated(inputToken: nextToken, outputToken: nextToken, pageSize: maxResults, items: reports), input maxResults is MaxResultsAttribute @range 1..300 and nextToken is NextTokenAttribute @length 1..2048. The model fixes the SHAPE and the paging contract, not the catalog content, so for the content this is recorded behavior only: the AWS Artifact report catalog is service-owned (the API models no CreateReport), and the recorded response enumerates the whole published catalog — several hundred distinct report resources, each ReportSummary identified by its own `id`/`arn`/`version` — so a client that walks nextToken to exhaustion sees every published report. The requirement is that the pages ListReports cuts, taken together, be exactly the set of published report resources the catalog holds.
Emulator: the replay's cluster key for this step carries several hundred MEMBER-LESS, whole-entry paths of the form `$.reports[arn="arn:aws:artifact:us-east-1::report/<id>:<version>"]: missing`, each meaning the recorded entry with that arn has no counterpart anywhere in the emulator's response. Verbatim from the replay detail: `$.reports[arn="arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2"]: missing (expected {... 'category': 'Alignment Documents', 'companyName': 'AWS', 'id': 'report-CKWRe4CwVxUEA8yg', 'name': 'AWS User Guide to the European Banking Authority Guidelines and Bank of Italy’s Circular 285/2013', 'series': 'Laws and Regulations', 'state': 'PUBLISHED', 'version': 2, 'visibility': 'PUBLIC'}, got None)` and `$.reports[arn="arn:aws:artifact:us-east-1::report/report-EoPCqoQjoEEMygkU:1"]: missing (expected {'category': 'Certifications and Attestations', ... an Accessibility Conformance Report (ACR) based on the ITI VPAT ...}, got None)`; the same whole-entry form repeats for arns such as report-0nXRsThNM4eHtuyn:4, report-0rFcyFMKMKeV4r7E:1, report-LIE0oFai46aosWjf:58, report-fKrDN0SZZokcr6rx:29 and report-pMuIoxgVQUTXLcGa:30. In the checkout the catalog is a fixed compile-time seed table built by the state's `Default` (a `&'static [Seed...]` of per-programme rows carrying `series`/`category`/`acceptance_type`/`source_link`/`versions`), which holds only a few dozen rows, so no page the handler cuts can ever contain the remaining published reports.
Why: a client relying on AWS's behavior enumerates the catalog to find a report it then calls GetReportMetadata / GetTermForReport / GetReport on. Against the emulator most published reports simply do not exist, so compliance-document discovery and download flows for anything outside the small seeded subset fail at the listing step, and a caller cannot distinguish "AWS does not publish this report" from "the emulator does not hold it".
Fix: the report-catalog state the ListReports handler pages over — the `reports` map seeded by `Default` in the artifact service's state module — must hold the full published catalog the corpus records, and the handler's page walk must cut its pages from that full set (deterministically ordered, honouring maxResults, emitting nextToken while more remain).
Traces: artifact-catalog-list-reports#task#1. NOTE: the still-OPEN issue 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports describes this same omission as its "Cause 1 of 2"; it is not fixed, and the replay does not report this cluster as owned by it, so it is re-filed here with the current evidence.

=== Cause 2 of 4: ReportSummary periodStart/periodEnd are serialized with a millisecond fraction AWS does not emit (fixture artifact-catalog-list-reports, step task#1) ===
AWS: Smithy: ReportSummary$periodStart and ReportSummary$periodEnd both target TimestampAttribute with @timestampFormat date-time (RFC 3339). Recorded behavior: in the recorded ListReports response these instants are rendered at SECOND precision with NO fractional-seconds component — `2024-11-01T00:00:00Z`, `2025-10-31T00:00:00Z`, `2025-10-01T00:00:00Z`. The fixed part of the requirement is the rendering itself (`YYYY-MM-DDTHH:MM:SSZ`, no `.mmm`); the instants are variable catalog values, read from the stored report version identified by the entry's id/arn/version, and are never per-run values.
Emulator: the same instants are rendered with a three-digit fraction, so every entry whose period the emulator does hold diverges byte-for-byte. Verbatim from the replay detail: `$.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"].periodEnd: value_mismatch (expected '2025-10-31T00:00:00Z', got '2025-10-31T00:00:00.000Z')`; `$.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"].periodStart: value_mismatch (expected '2024-11-01T00:00:00Z', got '2024-11-01T00:00:00.000Z')`; `$.reports[arn="arn:aws:artifact:us-east-1::report/report-0c1IXmudeXuTlHJJ:3"].periodStart: value_mismatch (expected '2025-10-01T00:00:00Z', got '2025-10-01T00:00:00.000Z')`. The cluster key repeats `.periodStart`/`.periodEnd` for report-073F0fUCdVVNKJrF:3 and report-0QjvdIs6Bsw4uPxb:2 as well, i.e. this is the serializer, not one bad row. The dates themselves agree — only the fractional component is added.
Why: a client that diffs, hashes, caches or snapshot-tests the catalog sees every ReportSummary differ from AWS's bytes, and a strict RFC 3339 consumer that pins second precision rejects or re-normalises values AWS returns unchanged. Period windows are what callers use to pick the report version covering an audit period, so the rendering has to match.
Fix: the date-time timestamp serializer used by the artifact ListReports response projection (the TimestampAttribute/@timestampFormat date-time path, shared with GetReportMetadata / ListReportVersions) must emit seconds-precision RFC 3339 with no fractional-second component. This is a REGRESSION in the opposite direction from the already-filed issue 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports, whose "Cause 2 of 2" asserted AWS emits the millisecond component and asked for `.mmm` to be added; the recording replayed here carries no fractional part, so that correction overshot.
Traces: artifact-catalog-list-reports#task#1

=== Cause 3 of 4: a returned ReportSummary still carries a name that is not the named report's published title (fixture artifact-catalog-list-reports, step task#1) ===
AWS: Smithy: ReportSummary$name targets ShortStringAttribute (@length 1..256, @pattern ^[a-zA-Z0-9_\-\s]*$), documented as "Name for the report resource". Recorded behavior only for the value: `name` is the published title of the report resource that entry's `id`/`arn`/`version` identifies, so it is determined by WHICH catalog entry the element is and never by its position in the list or by any service the report happens to be about.
Emulator: for an arn the emulator does return, the projected `name` is not the recorded title. The replay's cluster key for this step lists `$.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].name` as a diverging member alongside that entry's `.periodStart` (the per-key expected/actual strings for this particular path fall beyond the truncated `detail` the replay returned; the key itself is the evidence that the member diverges while status stays 200/200). This is the residue of the member-misprojection family the predecessor issue documented: the entry is present and matched by arn, but at least one content member is still read from somewhere other than the stored catalog entry that arn names.
Why: a client resolves a report by arn and then shows or logs its title; against the emulator the title belongs to a different report, so a caller downloading "the" document for an arn cannot tell from the listing which document it is about.
Fix: the ReportSummary projection in the artifact service's state (the `to_summary` conversion the ListReports handler maps each catalog entry through) — every member must be read from the stored catalog entry identified by that id/arn/version, never templated, re-worded or regenerated.
Traces: artifact-catalog-list-reports#task#1

=== Cause 4 of 4: ListComplianceInquiryQueries does not report the recorded query record, and its citations' sourceLink names the wrong source (fixture artifact-compliance-inquiry-basic-workflow, step task#3) ===
AWS: Smithy: ListComplianceInquiryQueries is `GET /v1/compliance-inquiry/listQueries`, success 200, input complianceInquiryId is required InquiryId @pattern ^compliance-inquiry-[a-zA-Z0-9]{16}$, output `queries` -> QueriesList of QuerySummary, @paginated(items: queries). QuerySummary requires queryIdentifier, query, status, statusMessage and createdAt, and carries optional `response`, `reviewType` and `citations` -> CitationList of Citation {sourceLabel (ShortStringAttribute), sourceContent (LongStringAttribute), sourceLink (LongStringAttribute @length 1..2048, @pattern ^[^<>]*$, "Link to the compliance source")}. The model fixes the shape; the content is recorded behavior only: once the inquiry's processing completes, listing its queries reports one QuerySummary per query of that inquiry, each with the answer released for it and the citations supporting that answer, and each Citation's `sourceLink` is the published location of the passage its `sourceContent` quotes — so sourceLabel/sourceLink point at the document the quoted text actually appears in.
Emulator: the replay reports `$.queries[0]: missing (expected {'citations': [{'sourceContent': 'Amazon S3 is an AWS managed service, and third-party auditors regularly assess the security and compliance of it as part of multiple AWS compliance programs.', 'sourceLabel': 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/', 'sourceLink': 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/'}, {... 'Amazon S3 is in the scope of the following compliance programs. SOC 1,2,3; PCI; ISO/IEC 27001:2013 ...'}, {... 'You can obtain corresponding compliance reports under an AWS non-disclosure agreement (NDA) through AWS Artifact.'}], ...}, got None)` — the recorded first query record, with its three citations, has no counterpart in the emulator's `queries` list. The cluster's divergence key is `service=artifact|ListComplianceInquiryQueries|status=200->200|keys=$.queries[0],$.queries[0].citations[0].sourceLink,$.queries[0].citations[1].sourceLink,$.queries[0].citations[2].sourceLink`, i.e. the diverging member of that record is specifically the citation LINK, three times over. In the checkout a citation's link is read from the seeded report row's `source_link` field (a per-compliance-programme AWS landing page such as `https://aws.amazon.com/compliance/soc-faqs/` or `https://aws.amazon.com/compliance/iso-27001-faqs/`), and the retrieval that answers a query runs over that same small seeded catalog — so the emulator cites its own seed rows' programme pages instead of the published source each quoted passage comes from, and the query record the recording expects is not produced at all.
Why: a client reads the citations to show a compliance answer's provenance and to follow the link to the quoted material. Against the emulator the expected query record is absent from the listing (so a caller that enumerated the inquiry's queries after create sees nothing to render), and where citations are produced their sourceLink resolves to a generic programme page rather than the document the sourceContent was taken from, so provenance cannot be verified.
Why (status): both sides answer 200, so nothing signals the shortfall to the caller.
Fix: the ListComplianceInquiryQueries handler's QuerySummary projection and the stored answer it reads (`InquiryQuery::to_summary` and the inquiry-answers state it draws `response`/`citations` from): the inquiry's queries must all be reported once the clock-owed transition has released them, and each Citation's `sourceLink`/`sourceLabel` must be the published location of the source its `sourceContent` quotes rather than the seeded report row's programme landing page.
Traces: artifact-compliance-inquiry-basic-workflow#task#3

## Evidence
Replay summary for trace set simparity-wire-aws-v1: emulator_up=true, bootstrap_signal=false, steps_total=31, steps_passed=29, steps_mismatched=2, steps_errored=0, steps_blocked=0, score_pct=93.5, divergence_count=2. Both divergence clusters are service=artifact, verdict=mismatch, with stub_path empty and owned_by empty (neither is already filed).

--- Cluster A: fixture artifact-catalog-list-reports, step task#1, operation ListReports, expected_status 200, actual_status 200 ---
Cause 1 (entries absent) — member-less whole-entry keys, each meaning the recorded entry with that arn has no counterpart in the emulator response:
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2"]
    expected: {'acceptanceType': 'PASSTHROUGH', 'category': 'Alignment Documents', 'companyName': 'AWS', 'id': 'report-CKWRe4CwVxUEA8yg', 'name': 'AWS User Guide to the European Banking Authority Guidelines and Bank of Italy’s Circular 285/2013', 'productName': 'AWS', 'series': 'Laws and Regulations', 'state': 'PUBLISHED', 'version': 2, 'visibility': 'PUBLIC'}
    actual:   None (missing)
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-EoPCqoQjoEEMygkU:1"]
    expected: {'acceptanceType': 'PASSTHROUGH', 'category': 'Certifications and Attestations', 'companyName': 'AWS', 'description': 'This document is an Accessibility Conformance Report (ACR) based on the ITI VPAT® ...', ...}
    actual:   None (missing)
  Same whole-entry form for several hundred further arns, e.g. report-0nXRsThNM4eHtuyn:4, report-0rFcyFMKMKeV4r7E:1, report-0tuV14iFC4pwSHN8:1, report-LIE0oFai46aosWjf:58, report-fKrDN0SZZokcr6rx:29, report-pMuIoxgVQUTXLcGa:30.

Cause 2 (timestamp rendering) — expected (recorded AWS) vs actual (emulator):
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"].periodEnd
    expected: '2025-10-31T00:00:00Z'      actual: '2025-10-31T00:00:00.000Z'
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-5eevElBv6MyRmQsD:7"].periodStart
    expected: '2024-11-01T00:00:00Z'      actual: '2024-11-01T00:00:00.000Z'
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-0c1IXmudeXuTlHJJ:3"].periodStart
    expected: '2025-10-01T00:00:00Z'      actual: '2025-10-01T00:00:00.000Z'
  Cluster key also lists .periodStart/.periodEnd for report-073F0fUCdVVNKJrF:3 and .periodStart for report-0QjvdIs6Bsw4uPxb:2. Only the fractional-seconds component differs; the instants agree.

Cause 3 (member content) — cluster key lists:
  $.reports[arn="arn:aws:artifact:us-east-1::report/report-0QjvdIs6Bsw4uPxb:2"].name  (value_mismatch; the entry is present and matched by arn)

--- Cluster B: fixture artifact-compliance-inquiry-basic-workflow, step task#3, operation ListComplianceInquiryQueries, expected_status 200, actual_status 200 ---
divergence_key: service=artifact|ListComplianceInquiryQueries|status=200->200|keys=$.queries[0],$.queries[0].citations[0].sourceLink,$.queries[0].citations[1].sourceLink,$.queries[0].citations[2].sourceLink
detail: $.queries[0]: missing (expected {'citations': [{'sourceContent': 'Amazon S3 is an AWS managed service, and third-party auditors regularly assess the security and compliance of it as part of multiple AWS compliance programs.', 'sourceLabel': 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/', 'sourceLink': 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/'}, {'sourceContent': 'Amazon S3 is in the scope of the following compliance programs. SOC 1,2,3; PCI; ISO/IEC 27001:2013, 27017:2015, 27018:2019, 27701:2019, 22301:2019, 9001:2015 and CSA STAR CCM v4.0; ISMAP; FedRAMP; DoD CC SRG; HIPAA; IRAP; MTCS; C5; K-ISMS; ENS High; OSPAR; HITRUST CSF; FINMA; GSMA; PiTuKri; CCCS MEDIUM; GNS National Restricted Certification; IAR', 'sourceLabel': '...fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/', 'sourceLink': '...fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/'}, {'sourceContent': 'You can obtain corresponding compliance reports under an AWS non-disclosure agreement (NDA) through AWS Artifact.', 'sourceLabel': '...', 'sourceLink': '...'}], ...}, got None)

Supporting code in the checkout (citations only; not a patch):
  services/service-com-amazonaws-artifact-artifact/src/ops/list_reports.rs:190-202 — the page is cut from store.reports, i.e. from the Default-seeded catalog.
  services/service-com-amazonaws-artifact-artifact/src/state.rs:3378-3392 — the compile-time Seed row type (document_object/term_object/source_link/versions) the catalog is built from; only a few dozen rows follow.
  services/service-com-amazonaws-artifact-artifact/src/state.rs:3468, 3511, 3631, 3696, 3752, 3856, 3903 — per-programme source_link values ('https://aws.amazon.com/compliance/...').
  services/service-com-amazonaws-artifact-artifact/src/state.rs:3970-3974 — Report.source_link is copied from the seed row and is what a citation reports.
  services/service-com-amazonaws-artifact-artifact/src/state.rs:1007-1019 — InquiryQuery::to_summary, the QuerySummary projection whose citations read QueryCitation::source_link.
  services/service-com-amazonaws-artifact-artifact/src/ops/list_compliance_inquiry_queries.rs:227-244 — the queries page walk and to_summary mapping.

## Steps to Reproduce
Causes 1-3 (ListReports), fixture artifact-catalog-list-reports, step task#1:
1. Boot the emulator from this checkout and replay the wire corpus: replay_wire_corpus() against trace set simparity-wire-aws-v1.
2. Issue the step's request: GET /v1/report/list on the artifact endpoint (SigV4, service name 'artifact', region us-east-1), no maxResults and no nextToken.
3. The call answers HTTP 200.
4. Cause 1: collect the set of `arn` values across ALL pages (follow nextToken to exhaustion) and diff it against the arn set of the recorded response. Recorded arns such as arn:aws:artifact:us-east-1::report/report-CKWRe4CwVxUEA8yg:2, .../report-EoPCqoQjoEEMygkU:1, .../report-0nXRsThNM4eHtuyn:4, .../report-LIE0oFai46aosWjf:58 appear in no page of the emulator's output.
5. Cause 2: for an arn the emulator DOES return (e.g. .../report-5eevElBv6MyRmQsD:7, .../report-0c1IXmudeXuTlHJJ:3), read periodStart/periodEnd. They are rendered with a three-digit fractional-seconds component ('2024-11-01T00:00:00.000Z') where the recording carries seconds precision only ('2024-11-01T00:00:00Z'); the instants otherwise agree.
6. Cause 3: for .../report-0QjvdIs6Bsw4uPxb:2, compare `name` member-by-member with the recorded entry carrying the same arn — it is not that report's published title.

Cause 4 (ListComplianceInquiryQueries), fixture artifact-compliance-inquiry-basic-workflow, step task#3:
7. Replay the same corpus; the fixture's earlier steps create a compliance inquiry (POST /v1/compliance-inquiry, answered 202 with a non-terminal PROCESSING status) and let its processing become due.
8. Issue the step's request: GET /v1/compliance-inquiry/listQueries?complianceInquiryId=<the compliance-inquiry-XXXXXXXXXXXXXXXX id minted in step 7>.
9. The call answers HTTP 200. Compare `queries` with the recorded response: the recorded first QuerySummary — the one whose three citations quote the AWS FSI-services-spotlight material about Amazon S3's compliance programmes — has no counterpart in the emulator's list ($.queries[0]: missing).
10. For any citations the emulator does report, read `sourceLink`: it is a per-compliance-programme landing page taken from the seeded report row (e.g. https://aws.amazon.com/compliance/soc-faqs/) rather than the published location of the passage in `sourceContent` (the recording's https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/).
