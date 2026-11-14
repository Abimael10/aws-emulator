Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: textract.analyze-document-queries-feature-returns-query-block
Issue-Shape-Id: com.amazonaws.textract#Textract
Issue-Key: textract.analyze-document-queries-feature-returns-query-block|run#16|AssertionError|assert any("99887" in text.replace(" ", "") for text in answer_texts), (
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# Textract AnalyzeDocument QUERIES: QUERY_RESULT text drops leading characters of the answer value read off the page

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AnalyzeDocument

## Description
This issue carries 1 net-new root cause in the textract crate (services/service-com-amazonaws-textract-textract). A fixer must close that one cause before submitting.

Context on what is already filed: the open issue 20261113T235649Z-textract-raster-reading-is-wrong-end-to-end.md carries this same check (textract.analyze-document-queries-feature-returns-query-block) at the replay key `assert answer_ids, "QUERY block has no ANSWER relationship to a QUERY_RESULT block"` — i.e. the QUERY block carried NO ANSWER relationship at all. That assertion is no longer reached: the emulator now DOES emit an ANSWER relationship resolving to a QUERY_RESULT block, and the check fails later, on the answer's TEXT. So that issue's key does not cover this failure, and the suite did not mark this check as owned. The other textract checks the suite reports (glyph misreading, lost word breaks, empty FORMS/TABLES/AnalyzeExpense output) are marked OWNED by 20261113T235649Z and by 20261030T113700Z / 20261109T200548Z and are skipped here. This is not a reopened refusal: the call returns HTTP 200 with no error code.

Cause 1 of 1 — the QUERIES answer path returns a QUERY_RESULT whose Text is a truncated fragment of the value drawn on the page, losing the leading characters of the matched value.
AWS: Smithy (services/service-com-amazonaws-textract-textract/smithy-model/com-amazonaws-textract-textract.json): AnalyzeDocument input {Document required, FeatureTypes required, HumanLoopConfig, QueriesConfig, AdaptersConfig}, output {DocumentMetadata, Blocks, HumanLoopActivationOutput, AnalyzeDocumentModelVersion}; BlockType enum includes QUERY and QUERY_RESULT; RelationshipType enum includes ANSWER; Block$Text is documented "The word or line of text that's recognized by Amazon Textract"; Block$Confidence is Percent @range 0..100. AnalyzeDocument's documentation states a QUERY block contains the query text/alias and a link to the associated query-results block, and a QUERY_RESULT block contains the answer to the query plus a confidence score. The requirement is general: when a query matches a value rendered on the page, the QUERY_RESULT block's Text is the COMPLETE value as drawn — every character of it, in order — not a substring of it. The particular value in the recording ('INV-99887') is one run filling that template, not the requirement.
Emulator: HTTP 200, no error code, no error message. The QUERY block is present exactly once, echoes Query.Text and Query.Alias correctly, and now carries an ANSWER relationship to a real QUERY_RESULT block (all of those assertions pass). But the QUERY_RESULT's Text is a truncated fragment: for a page rendering the line "Invoice Number: INV-99887" and the query "What is the invoice number?" the answer text came back as '9887' — the leading characters of the drawn value ('INV-9') are missing, so the answer does not even contain the digits of the invoice number ('99887'). Hypothesis: the value side of the matched field is being re-sliced after the label/colon split (the labelled_split / answer_for path in crate::state that builds the QUERY_RESULT text), taking an offset into the recognised value run instead of the whole run; the sibling glyph-fidelity defect filed as Cause 1 of 20261113T235649Z can corrupt individual characters, but it does not explain characters being dropped from the FRONT of the value while the tail survives, so this is a separate truncation of the answer span.
Why: QUERIES exists so a client can read a single field out of a document without parsing it. The client gets a 200 with its question echoed, an ANSWER relationship, a QUERY_RESULT block and a confidence score — every structural check it can make passes — yet the answer string is a fragment of the real value. An invoice number, an account id or an amount read this way is silently wrong, and because the prefix is gone the client cannot even detect the truncation by pattern-matching the expected format.
Fix: the QUERIES answer derivation in crate::state for AnalyzeDocument (the query-block builder around answer_for / the QUERY_RESULT block emission, and the field-value span it takes from labelled_split): the QUERY_RESULT Text must be the full recognised value span of the matched field, with its measured Confidence and Geometry, and must not drop leading characters of that span.
Traces: textract.analyze-document-queries-feature-returns-query-block.

## Evidence
Suite simparity-behavioral-aws-v1 replayed against the emulator booted from this checkout: 16/26 passed (61.5%), 8 mismatched, 2 errored, 0 skipped, 0 unloadable. Per-service: textract 16/24 (67%) 8 failed; cloudformation__textract 0/2 (both OWNED by 20261109T200548Z and skipped). bootstrap_signal was false — the emulator was serving and textract answered HTTP 200 on this call. The divergence is entirely in response CONTENT; no diverging status code, error code or error message was observed.

[mismatch] textract.analyze-document-queries-feature-returns-query-block
Request: AnalyzeDocument(Document={"Bytes": <PNG rendering the single line "Invoice Number: INV-99887">}, FeatureTypes=["QUERIES"], QueriesConfig={"Queries": [{"Text": "What is the invoice number?", "Alias": "INVOICE_NUMBER"}]}).
Passed before the failure: exactly one QUERY block returned; QUERY.Query.Text == "What is the invoice number?"; QUERY.Query.Alias == "INVOICE_NUMBER"; a Relationships entry of Type "ANSWER" with non-empty Ids; each ANSWER id resolves to a block whose BlockType is "QUERY_RESULT".
Failure (check line 53):
  AssertionError: QUERY_RESULT text does not contain the invoice number rendered into the document pixels ('INV-99887'); got ['9887']
Expected: the QUERY_RESULT Text carries the complete value drawn on the page — for this page, a string whose digits are '99887' (the check normalises away spaces, so 'INV-99887' or 'INV 99887' both satisfy it).
Actual: QUERY_RESULT Text == '9887' — the leading 'INV-9' of the drawn value is absent, so the answer is a 4-character tail of a 9-character value.
Note on per-run values: 'INV-99887' is the value this recording drew onto the page; the requirement is that whatever value the page draws comes back complete, not that this literal string appears. Block Id values, confidences and geometry coordinates are per-run and are not asserted here.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the simparity-behavioral-aws-v1 behavioral suite, textract group.
2. Rasterise a PNG containing one line of the form "<Label>: <VALUE>" where <VALUE> is a multi-character token with a distinctive prefix (the check uses "Invoice Number: INV-99887").
3. Call AnalyzeDocument(Document={"Bytes": <that PNG>}, FeatureTypes=["QUERIES"], QueriesConfig={"Queries": [{"Text": "What is the <label>?", "Alias": "INVOICE_NUMBER"}]}).
4. Locate the single QUERY block, follow its Relationships entry of Type "ANSWER" to the QUERY_RESULT block, and compare that block's Text to the value drawn on the page.
5. Observe HTTP 200 and a structurally complete answer whose Text is a truncated tail of the drawn value (got '9887' for a drawn 'INV-99887'), with the leading characters dropped.
6. Re-replay textract.analyze-document-queries-feature-returns-query-block after the fix; the QUERY_RESULT Text must contain the full drawn value, and the check must pass through its final assertion.
