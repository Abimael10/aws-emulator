Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: textract.detect-document-text-reads-rendered-text
Issue-Shape-Id: com.amazonaws.textract#Textract
Issue-Key: textract.detect-document-text-reads-rendered-text|run#8|AssertionError|assert "SIM2REAL" in joined and token in joined, (
Issue-Commit: 4e337ab52f336eedfc3db50cb4feef60117418c9

# DetectDocumentText (Document.Bytes) misrecognises glyphs in a rendered line: drawn characters come back substituted so the recognised WORD/LINE text is not the text drawn on the page

> _Note: this issue was identified against the code base at commit `4e337ab52f336eedfc3db50cb4feef60117418c9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectDocumentText

## Description
This issue reports 1 root cause, grounded in 1 failing check (textract.detect-document-text-reads-rendered-text). The call returns HTTP 200 and a structurally valid Blocks list, but the recognised text is not the text rasterised onto the submitted page, so this is a silent data-corruption path, not a refusal.

Context on what is already filed. Four of the six textract failures in this replay are marked OWNED and were skipped (20261119T223911Z-textract-ocr-still-misclassifies-glyphs-inside-recognised-words.md owns analyze-document-forms-extracts-key-value-pairs and analyze-expense-extracts-summary-fields; 20261124T021047Z-textract-ocr-glyph-recognition-still-wrong-on-two.md owns analyze-document-combined-features-tables-and-forms and detect-document-text-from-s3-object-source; 20261030T113700Z owns detect-document-text-corrupt-image-rejected), and both cloudformation__textract failures are OWNED by 20261109T200548Z. The check below is marked as having NO owner: no open issue carries its replay key. The open WIRE issues 20261115T094519Z-textract-synchronous-blocks-geometry-confidence-and-text-do.md and 20261124T064335Z-textract-analyzedocument-blocks-are-a-synthetic-layout-geometry.md concern Blocks geometry/confidence/classification and carry no behavioral key for this check. This is not a reopened refusal: the call succeeds with 200.

Cause 1 of 1 — DetectDocumentText on Document.Bytes returns LINE/WORD Text whose characters were never drawn on the page.
AWS: Smithy (services/service-com-amazonaws-textract-textract/smithy-model/com-amazonaws-textract-textract.json): DetectDocumentText input {Document required} / output {DocumentMetadata, Blocks, DetectDocumentTextModelVersion}; errors BadDocumentException, DocumentTooLargeException, InvalidParameterException, InvalidS3ObjectException, UnsupportedDocumentException (400), InternalServerError/ThrottlingException (500). Block$Text is documented "The word or line of text that's recognized by Amazon Textract", and Block$BlockType documents LINE as a string of space-delimited contiguous words and WORD as one or more ISO basic Latin script characters not separated by spaces. The requirement is general and character-exact: for ANY ASCII text rasterised onto a page at legible size, the LINE Text spells the drawn line and each WORD Text spells exactly the drawn characters of that word — including alphanumeric strings with no dictionary context. The call is a 200; no error code or error message is involved on this path, so no message template applies.
Emulator: the response is a 200 with LINE blocks, but individual glyphs are classified as other characters. For a page drawing two lines, a label word followed by a freshly generated 8-character alphanumeric token and a second line of two words, the recognised LINE texts come back as ['SIM2REAL MKSE4DIB', 'TEXTRACT CHECK'] for a page whose token was drawn as MKSE4BR8: the dictionary-like words ('SIM2REAL', 'TEXTRACT', 'CHECK') and the token's leading glyphs are recovered, while the token's trailing glyphs are substituted ('B' read as 'D', 'R' read as 'I', '8' read as 'B'). The specific strings are one run's output filling the template above — the token is generated per run from an OCR-unambiguous alphabet (0/O, 1/I/Q and similar glyphs excluded), so the requirement is per-glyph fidelity for whatever is drawn, not these values. Because recognition fails at the assertion, the check's downstream assertions (>= 4 WORD blocks, Confidence within [0,100], BoundingBox Left/Top within [0,1] and positive Width/Height, non-empty DetectDocumentTextModelVersion) are not exercised and must also hold after the fix.
Why: a client calling DetectDocumentText to read a document gets a 200 whose text is partially fabricated, with high-looking confidence and no signal that recognition failed. Any consumer that matches an identifier, order number, code or reference extracted from a scan against its own records silently fails to match or stores a corrupted value; the structural validity of the response makes the corruption undetectable client-side.
Fix: the per-glyph classification/scoring in the OCR module of the textract crate reached from the Document.Bytes read path of the DetectDocumentText handler (the shared glyph recogniser also feeding the already-filed TABLES-cell and S3Object paths), so that each rendered ASCII glyph resolves to the drawn character before any WORD/LINE Text is emitted. Only the recogniser needs changing; the block builder and serializer are correct once the text is.
Traces: textract.detect-document-text-reads-rendered-text.

## Evidence
Suite simparity-behavioral-aws-v1 replayed against the emulator booted from this checkout: 18/26 passed (69.2%), 6 mismatched, 2 errored, 0 skipped, 0 unloadable. Per-service: textract 18/24 (75%) 6 failed; cloudformation__textract 0/2 (both OWNED by 20261109T200548Z and skipped). bootstrap_signal was false — the emulator was serving and textract answered HTTP 200 on the call below.

[mismatch] textract.detect-document-text-reads-rendered-text (no owner)
  Request: DetectDocumentText(Document={"Bytes": <PNG rendering two lines: "SIM2REAL     <token>" and "TEXTRACT     CHECK", upscaled 6x with LANCZOS>}), where <token> is 8 characters drawn fresh per run from the alphabet "23456789ABCDEFGHJKLMNPRSTUVWXYZ" (OCR-ambiguous glyphs excluded).
  Failed (check line 29): assert "SIM2REAL" in joined and token in joined
  AssertionError: expected run-specific token 'MKSE4BR8' in OCR output, got: ['SIM2REAL MKSE4DIB', 'TEXTRACT CHECK']
  Expected: the uppercased LINE texts contain the drawn label and the drawn token verbatim (here 'SIM2REAL MKSE4BR8'), plus 'TEXTRACT' and 'CHECK'.
  Actual: HTTP 200 with LINE texts ['SIM2REAL MKSE4DIB', 'TEXTRACT CHECK'] — the drawn token MKSE4BR8 recognised as MKSE4DIB: 'B'->'D', 'R'->'I', '8'->'B'. Word segmentation and line segmentation are correct; only glyph identity diverges.

No diverging HTTP status, error code or error message was observed: the divergence is value-only in the response body of a 200. The quoted token is this run's value filling the template; the requirement is that whatever ASCII glyphs are drawn are the glyphs returned. The assertion is on text the emulator itself produced from pixels supplied in the request — it does not depend on a live account, a credential, a clock value or a request id.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the simparity-behavioral-aws-v1 behavioral suite, textract group.
2. Run textract.detect-document-text-reads-rendered-text: render a white PNG with two black text lines — a dictionary-like label followed by an 8-character alphanumeric token drawn from an OCR-unambiguous alphabet, then a second line of two plain words — upscale it (e.g. 6x LANCZOS) and call DetectDocumentText(Document={"Bytes": png}).
3. Collect the Text of every BlockType == "LINE" block, uppercase and join them. Observe HTTP 200 and that the label and the plain words are recovered while characters inside the token are substituted (e.g. drawn MKSE4BR8 returned as MKSE4DIB).
4. Re-run with a different randomly generated token: the substitutions follow the individual glyphs rather than the whole string, confirming the defect is in the per-glyph classifier shared by the Document.Bytes read path, not in line/word segmentation (which is correct here) and not in a cached or canned response.
5. After the fix, re-replay the check id in trace_ids — textract.detect-document-text-reads-rendered-text — and require it to pass end to end, including the assertions downstream of the text check that are not reached today: at least 4 WORD blocks, each WORD Confidence within [0.0, 100.0], each WORD Geometry.BoundingBox with Left and Top within [0.0, 1.0] and Width and Height greater than 0 and at most 1.0, and a non-empty DetectDocumentTextModelVersion.
