Issue-Source: trace-sweep-divergence
Issue-Trace: artifact-compliance-inquiry-basic-workflow
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: artifact-compliance-inquiry-basic-workflow
Issue-Shape-Id: com.amazonaws.artifact#Artifact
Issue-Key: artifact|ListComplianceInquiryQueries|2xx->2xx|$.queries[*]
Issue-Key: artifact|ListComplianceInquiryQueries|2xx->2xx|$.queries[*].citations[*].sourceLink
Issue-Commit: c119c6290af0aef232d88082a679964269a74639

# artifact ListComplianceInquiryQueries citation sourceLink is synthesized as an emulator GetReportMetadata URL instead of the cited source's own link (1 cause)

> _Note: this issue was identified against the code base at commit `c119c6290af0aef232d88082a679964269a74639`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListComplianceInquiryQueries

## Description
Cause 1 of 1: every Citation that ListComplianceInquiryQueries reports for a completed compliance-inquiry query carries a sourceLink the emulator SYNTHESIZES against its own endpoint, instead of the link of the published compliance source the quoted passage was read out of. The step's status is correct (200) and the rest of the QuerySummary projection agrees with the recording; the divergence is confined to the three citations' sourceLink members, which is enough for the recorded $.queries[0] element to be reported 'missing' from the returned list.

AWS: Smithy (services/service-com-amazonaws-artifact-artifact/smithy-model/com-amazonaws-artifact-artifact.json) — ListComplianceInquiryQueries GET /v1/compliance-inquiry/listQueries, input complianceInquiryId (InquiryId, @required, @httpQuery, pattern ^compliance-inquiry-[a-zA-Z0-9]{16}$), maxResults (@range 1..300), nextToken (@length 1..2048); output ListComplianceInquiryQueriesResponse.queries: QueriesList of QuerySummary, @paginated {inputToken: nextToken, outputToken: nextToken, pageSize: maxResults, items: queries}; errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400. QuerySummary.citations: CitationList of Citation, documented '[s]upporting citations for the response'. Citation has three members: sourceLabel (ShortStringAttribute @length 1..256) 'Label identifying the compliance source', sourceContent (LongStringAttribute @length 1..2048, @pattern ^[^<>]*$) 'Content text from the compliance source', and sourceLink (LongStringAttribute @length 1..2048, @pattern ^[^<>]*$) 'Link to the compliance source'. The model therefore makes sourceLink the locator OF THE CITED SOURCE, not a locator of some other API read. Recorded behavior confirms that reading: for the Amazon S3 compliance question in this fixture every citation's sourceLink is the published document's own URL — the same locator the citation's sourceLabel carries — and sourceContent is the passage quoted out of that document (e.g. an AWS 'FSI services spotlight' article URL under aws.amazon.com/blogs/industries/..., quoted passages 'Amazon S3 is an AWS managed service, and third-party auditors regularly assess the security and compliance of it as part of multiple AWS compliance programs.', 'Amazon S3 is in the scope of the following compliance programs. SOC 1,2,3; PCI; ISO/IEC 27001:2013 ...', 'You can obtain corresponding compliance reports under an AWS non-disclosure agreement (NDA) through AWS Artifact.'). The requirement is the RULE, not those literal URLs: sourceLink must be the link of the source the citation names, i.e. the locator stored alongside the quoted passage, and it must agree with the sourceLabel/sourceContent it travels with.

Emulator: the 200 body's citations keep the recorded sourceLabel and sourceContent, but each sourceLink is replaced by a URL into the emulator's own report-metadata API, built per request from the endpoint the READER happened to reach the emulator on — of the form https://<emulator-host>/v1/report/getMetadata?reportId=report-<id>&reportVersion=<n>. The three divergent keys in the cluster are exactly $.queries[0].citations[0].sourceLink, $.queries[0].citations[1].sourceLink and $.queries[0].citations[2].sourceLink (three, because the emulator's answer builder caps an answer at ANSWER_CITATION_CAPACITY = 3 sources), and because no element of the returned list is equal to the recorded one the comparator additionally reports $.queries[0] itself as 'missing'. This is by construction and not an accident of the environment: the stored citation record deliberately does not persist a source link at all and the link is manufactured at projection time, so no stored value of the cited source's link can be reported even when one exists.

Why: a client that consumes an inquiry's citations — an audit-evidence or questionnaire-automation tool that renders 'response + where it came from' to a reviewer — is handed a link that does not identify the cited compliance source. It points at a report-metadata read rather than at the document the passage was quoted from, so following the citation cannot confirm the quotation, and the link disagrees with the sourceLabel/sourceContent it is paired with. The same wrong triple is then baked into the object ExportComplianceInquiry publishes (the export renders citations through the same projection), so the divergence is persisted into the downloaded evidence document as well. Clients that compare citation links across the listing and the export, or that de-duplicate citations by link, also see values that change with the endpoint the reader used rather than with the source cited.

Fix: the artifact compliance-inquiry answer state and its citation projection. The citation record must carry the cited source's own link as stored data beside the label and the quoted passage (built when the answer is produced from the catalog entry the passage was read out of), and the Citation projection must report that stored link through to the wire instead of formatting a per-request URL from the reading request's host; the exported-document rendering must take the same stored value, so the listing and the export cannot disagree. The emulator-local host, port, report id and report version that appear in the current link are environment values and are not what is required — what is required is that sourceLink locates the source sourceLabel/sourceContent name.

Traces: artifact-compliance-inquiry-basic-workflow#task#3 (ListComplianceInquiryQueries, expected 200, actual 200, body divergence on $.queries[0] and $.queries[0].citations[0..2].sourceLink).

Related but not covering: the open issues 20261118T144128Z-artifact-listreports-first-page-both-omits-published-reports.md and 20261119T112439Z-artifact-listreports-catalog-projection-page-omits-published-reports.md concern the artifact ListReports catalog projection and own that replay key; neither mentions ListComplianceInquiryQueries or the Citation projection, so this divergence is filed with its own replay key. 20261110T162501Z-artifact-exportcomplianceinquiry-serves-a-presigned-url-for-an.md concerns ExportComplianceInquiry's refusal for a PROCESSING inquiry, not the content of a citation.

## Evidence
Fixture artifact-compliance-inquiry-basic-workflow, step task#3, service artifact, operation ListComplianceInquiryQueries (GET /v1/compliance-inquiry/listQueries). Expected status 200, actual status 200; verdict mismatch, so the entire divergence is in the response body. stub_path is empty (the operation is implemented) and owned_by is empty (no open issue carries this replay key). affected_count 1 (artifact-compliance-inquiry-basic-workflow#task#3).

Divergence key reported by the replay:
  service=artifact|ListComplianceInquiryQueries|status=200->200|keys=$.queries[0],$.queries[0].citations[0].sourceLink,$.queries[0].citations[1].sourceLink,$.queries[0].citations[2].sourceLink

Replay detail (expected = recorded AWS, actual = emulator):
  $.queries[0]: missing — the recorded QuerySummary element is not present in the returned list. Its expected citations are, in order:
    citations[0].sourceContent 'Amazon S3 is an AWS managed service, and third-party auditors regularly assess the security and compliance of it as part of multiple AWS compliance programs.'
      with sourceLabel and sourceLink both 'https://aws.amazon.com/blogs/industries/fsi-services-spotlight-featuring-amazon-simple-storage-service-amazon-s3/'
    citations[1].sourceContent 'Amazon S3 is in the scope of the following compliance programs. SOC 1,2,3; PCI; ISO/IEC 27001:2013, 27017:2015, 27018:2019, 27701:2019, 22301:2019, 9001:2015 and CSA STAR CCM v4.0; ISMAP; FedRAMP; DoD CC SRG; HIPAA; IRAP; MTCS ...; C5; K-ISMS; ENS High; OSPAR; HITRUST CSF; FINMA; GSMA ...; PiTuKri; CCCS MEDIUM; GNS National Restricted Certification; IAR (United Arab Emirates Information Assurance Regulation)'
      with the same sourceLabel/sourceLink
    citations[2].sourceContent 'You can obtain corresponding compliance reports under an AWS non-disclosure agreement (NDA) through AWS Artifact.'
      with the same sourceLabel/sourceLink
  $.queries[0].citations[0].sourceLink, $.queries[0].citations[1].sourceLink, $.queries[0].citations[2].sourceLink: value_mismatch — the emulator reports a URL into its OWN report-metadata API rather than the cited source's link.

Note on what is and is not a requirement here: the emulator's link is assembled per request as 'https://' + the endpoint the reading request reached + '/v1/report/getMetadata?reportId=...&reportVersion=...'. The host and port in it are per-environment values and are deliberately NOT part of the finding; the finding is that sourceLink is a locator of a different resource (a report-metadata read) instead of the link of the compliance source the citation's sourceLabel and sourceContent name. sourceLabel and sourceContent are NOT reported divergent, so the emulator already holds the correct source identity and the correct quoted passage for each of the three citations — only the link beside them is replaced.

Code citations for the responsible component (commit as cloned for this run):
  services/service-com-amazonaws-artifact-artifact/src/state/inquiry_answers.rs:90-93 — doc comment states 'The Citation$sourceLink a reader sees is NOT held here: it is built per projection ... against the endpoint the reading request reached this emulator on'.
  services/service-com-amazonaws-artifact-artifact/src/state/inquiry_answers.rs:94-107 — struct QueryCitation stores source_label, source_content, report_id, report_version and NO source link.
  services/service-com-amazonaws-artifact-artifact/src/state/inquiry_answers.rs:121-128 — fn source_link(&self, host) formats 'https://{host}/v1/report/getMetadata?reportId={report_id}&reportVersion={version}' (REPORT_METADATA_PATH at line 37).
  services/service-com-amazonaws-artifact-artifact/src/state/inquiry_answers.rs:131-137 — fn to_citation puts that synthesized value into Citation.source_link.
  services/service-com-amazonaws-artifact-artifact/src/state/inquiry_answers.rs:45 — ANSWER_CITATION_CAPACITY = 3, which is why exactly three citation keys diverge.
  services/service-com-amazonaws-artifact-artifact/src/state/inquiry_answers.rs:174-179 — QueryAnswer::reported_citations(host) maps every stored citation through to_citation(host).
  services/service-com-amazonaws-artifact-artifact/src/state.rs:1003-1016 — InquiryQuery::to_summary(&host) sets citations: answer.map(|answer| answer.reported_citations(host)).
  services/service-com-amazonaws-artifact-artifact/src/ops/list_compliance_inquiry_queries.rs:201 — host = crate::state::document_url_host(ctx.parts()), i.e. the reader's endpoint.
  services/service-com-amazonaws-artifact-artifact/src/ops/list_compliance_inquiry_queries.rs:231-243 — walk_page(..., |query| query.to_summary(&host)) builds the reported page.
  services/service-com-amazonaws-artifact-artifact/src/state.rs:1358-1373 — ExportComplianceInquiry's document renders the same citations through QueryCitation::to_document_entry(host), so the exported evidence carries the same wrong link.

## Steps to Reproduce
1. Clone this repository and boot the emulator built from the checkout.
2. Call replay_wire_corpus() to replay the wire corpus simparity-wire-aws-v1.
3. Locate the divergence cluster with service 'artifact', operation 'ListComplianceInquiryQueries', fixture_id 'artifact-compliance-inquiry-basic-workflow', step_key 'task#3'.
4. That fixture creates a compliance inquiry asking a compliance question about Amazon S3, lets its processing become due, and then issues GET /v1/compliance-inquiry/listQueries?complianceInquiryId=compliance-inquiry-<16 alnum> (no maxResults/nextToken) against the emulator's artifact endpoint.
5. Compare the 200 body with the recorded one. For each entry of $.queries[0].citations, sourceLabel and sourceContent match the recording, but sourceLink does not: the emulator answers 'https://<emulator-host>/v1/report/getMetadata?reportId=report-<id>&reportVersion=<n>' where the recording carries the published compliance source's own URL (the same value its sourceLabel carries). Because no returned element equals the recorded one, the replay also reports $.queries[0] as missing.
6. Repeat the GET through a second endpoint/host for the same emulator: the sourceLink values change with the reading endpoint while the cited source does not, confirming the link is synthesized per request rather than read off the stored citation.
7. Expected: each Citation's sourceLink is the stored link of the compliance source its sourceLabel and sourceContent name, identical for the same citation whichever endpoint the listing is read through, and identical to the link the same citation carries in the document ExportComplianceInquiry publishes.
