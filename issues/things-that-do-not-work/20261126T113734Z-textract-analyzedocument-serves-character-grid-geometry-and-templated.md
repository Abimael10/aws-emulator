Issue-Source: trace-sweep-divergence
Issue-Trace: textract-sync-text-detection-and-document-analysis
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: textract-sync-text-detection-and-document-analysis
Issue-Shape-Id: com.amazonaws.textract#Textract
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*]
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].BlockType
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].EntityTypes[*]
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.RotationAngle
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Relationships[*]
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Relationships[*].Type
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].TextType
Issue-Related: 20261125T053817Z-textract-sync-blocks-report-character-grid-geometry-and.md
Issue-Commit: 5d6a27891ebfadd30d77a79c020fa521e3c02338

# Textract AnalyzeDocument serves character-grid geometry and templated feature blocks instead of structure measured from the submitted page

> _Note: this issue was identified against the code base at commit `5d6a27891ebfadd30d77a79c020fa521e3c02338`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AnalyzeDocument

## Description
This issue has ONE cause.

Cause 1 of 1: AnalyzeDocument (fixture textract-sync-text-detection-and-document-analysis, step task#2) answers HTTP 200 with a structurally valid BlockList whose per-block CONTENT does not describe the page that was submitted: block geometry is a fixed character-grid layout rather than the normalised extent of the ink each block was read from, block Confidence is a generated number, and the analysis-only feature blocks (their BlockType, TextType, EntityTypes and Relationships) are produced from a template rather than derived from the page's detected structure. Status, headers and framing match the recording exactly, so the entire divergence is in response-body values.

AWS: Smithy (services/service-com-amazonaws-textract-textract/smithy-model/com-amazonaws-textract-textract.json). AnalyzeDocument input {Document required, FeatureTypes required, HumanLoopConfig, QueriesConfig, AdaptersConfig}; output {DocumentMetadata, Blocks: BlockList, HumanLoopActivationOutput, AnalyzeDocumentModelVersion}; modeled errors AccessDeniedException / BadDocumentException / DocumentTooLargeException / InvalidParameterException / InvalidS3ObjectException / ProvisionedThroughputExceededException / UnsupportedDocumentException (400), HumanLoopQuotaExceededException (402), InternalServerError / ThrottlingException (500). Geometry is "information about where the following items are located on a document page: detected page, text, key-value pairs, tables, table cells, and selection elements"; Geometry$BoundingBox is "an axis-aligned coarse representation of the location of the recognized item on the document page", with the upper-left corner of the image as origin (0,0) and Left/Top/Width/Height documented as ratios of the overall document page width/height (BoundingBox doc: a 700x200 page with a top-left at 350x50 px gives Left 0.5 and Top 0.25; a 70 px wide box gives Width 0.1). Geometry$Polygon is "within the bounding box, a fine-grained polygon around the recognized item", so its Point{X,Y} corners lie on or inside that same item's BoundingBox and traverse the item's own corners. Geometry$RotationAngle is "the rotation of the WORD block. Possible values are 0, 90, 180, and 270". Block$Confidence targets Percent @range 0..100 and is "the confidence score that Amazon Textract has in the accuracy of the recognized text and the accuracy of the geometry points around the recognized text". Block$Text is "the word or line of text that's recognized". Block$BlockType (enum KEY_VALUE_SET, PAGE, LINE, WORD, TABLE, CELL, SELECTION_ELEMENT, MERGED_CELL, TITLE, QUERY, QUERY_RESULT, SIGNATURE, TABLE_TITLE, TABLE_FOOTER, LAYOUT_*) documents PAGE as "contains a list of child Block objects that are detected on a document page" and KEY_VALUE_SET as "stores the KEY and VALUE Block objects for linked text that's detected on a document page. Use the EntityType field to determine if a KEY_VALUE_SET object is a KEY Block object or a VALUE Block object". Block$EntityTypes (enum KEY, VALUE, COLUMN_HEADER, TABLE_TITLE, TABLE_FOOTER, TABLE_SECTION_TITLE, TABLE_SUMMARY, STRUCTURED_TABLE, SEMI_STRUCTURED_TABLE) distinguishes KEY from VALUE for FORMS analysis. Block$TextType (enum HANDWRITING, PRINTED) is "the kind of text that Amazon Textract has detected". Block$Relationships is "a list of relationship objects that describe how blocks are related to each other" — a LINE carries CHILD to the WORDs that make it up, a KEY carries VALUE to its VALUE block — with Relationship{Type, Ids}. The requirement is general and not one recording's numbers: for the FeatureTypes requested, every block's BoundingBox and Polygon must be the normalised extent of that block's OWN ink on the submitted page, a PAGE block must span the content detected on the page rather than the extent of one of its children, Polygon corners must traverse the recognized item at the orientation the page was read at, RotationAngle must be the measured quarter turn, Confidence must be the measured score in [0,100], Text must be the recognised word/line, and the feature blocks that exist — their BlockType, TextType, EntityTypes and the Relationship edges linking them — must be enumerated from the structure actually detected on the page.

Emulator: every block comes back on a fixed character grid that ignores the page, and the analysis-only block set is invented. Blocks[0] (PAGE) is served Left 0.03333333507180214, Top 0.23333333432674408, Width 0.3777777850627899, Height 0.1733333319425583 — a word-sized box — where the recording has a page-spanning box (Left 0.009066186845302582, Top 0.011760788038372992, Width 0.9776000380516052, Height 0.9818877577781676); the emulator's Blocks[1] box (Height 0.1733333319425583, Left 0.03333333507180214, Top 0.23333333432674408, Width 0.3777777850627899) is byte-identical to its PAGE box, and a PAGE cannot have the extent of its own child. The polygon corner sequence differs from the recording (emulator Polygon X 0.04333333298563957, 0.41111111640930176, 0.4055555462837219, 0.03333333507180214 against recorded 0.010326294228434564, 0.986666202545166, 0.9859458804130554, 0.009066186845302582), so the polygon is not a traversal of the recognized item's corners. Confidence is generated (Blocks[1].Confidence 82.90326690673828 vs recorded 94.70218658447266). The repeated constants are grid fractions (1/30 = 0.0333…, 13/75 = 0.1733…, 0.3777…) reused across unrelated blocks, which identifies the character-grid fallback layout being served as the answer instead of a measurement. Beyond coordinates, the replay reports value_mismatch on $.Blocks[1..4].Text, $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and on into $.Blocks[6]: so the emulator also decides which feature blocks exist, what kind each one is, whether it is a KEY or a VALUE and which blocks it points at from a template rather than from the page.

Why: geometry and structure are the reason a client calls AnalyzeDocument. The client multiplies Left/Top/Width/Height by the page's pixel size to redact, highlight, crop or re-flow a region, and walks Relationships while switching on BlockType/EntityTypes to pair KEY with VALUE or to read a CELL. With one stock box per block, a word-sized PAGE box and a templated feature set, the client operates on the wrong region and builds a key/value map that is not in the document, while receiving a 200 that passes every schema, range and ordering check it can make — there is no second call to cross-check against and no error signal to fall back on. A client that thresholds on Confidence admits or rejects blocks on a number unrelated to any recognition.

Fix: the AnalyzeDocument response assembly in the textract crate's state module — the block builder and the geometry it attaches, plus the FeatureTypes-driven derivation that emits KEY_VALUE_SET / TABLE / CELL / SELECTION_ELEMENT blocks and their BlockType, TextType, EntityTypes, Relationships and Text. Per-block extents and confidences must come from the page raster reader and be normalised by the page's own width and height; feature blocks must be enumerated and linked from the page's detected structure and each span exactly the words it was read from; the laid-out character grid must only be a fallback when there is genuinely no measurement, not the served answer. Citations: services/service-com-amazonaws-textract-textract/src/state.rs:2218 (blocks / FeatureSet-driven derivation), :1977 (geometry), :1822 (bounding_box, the character-grid layout).

No diverging HTTP status, error code or error message: 200 -> 200 with body-value divergence only, so there is no error wording to reproduce here.

Traces: textract-sync-text-detection-and-document-analysis#task#2 (AnalyzeDocument, 200 -> 200, value-only mismatch).

Already-filed context: the replay reports this cluster with stub_path "" and owned_by [] — nothing owns it. The open issue 20261125T053817Z-textract-sync-blocks-report-character-grid-geometry-and.md discusses this class and names task#2 in its prose, but its replay pointer only binds task#1 (Issue-Step: task#1, and its Issue-Trace-Ids entry carries no #step token), which is why replay still reports task#2 as un-owned; that issue therefore does not gate this step. The open issues 20261119T223911Z-textract-ocr-still-misclassifies-glyphs-inside-recognised-words.md, 20261124T021047Z-textract-ocr-glyph-recognition-still-wrong-on-two.md, 20261124T120152Z-textract-detectdocumenttext-document-bytes-misrecognises-glyphs-in-a-rendered.md and 20261030T113700Z-textract-detectdocumenttext-positive-control-after-a-rejected-corrupt-image.md concern recognised glyph TEXT, not Geometry, Polygon ordering, RotationAngle, Confidence or feature-block classification.

Note on values: every float, text and enum quoted above is one recording's numbers filling the template, not the requirement. The requirement is the normalisation and derivation rules stated under AWS.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout: emulator_up true (/__admin/health answered 200 and the :43087 data listener accepts), bootstrap_signal false, 6 fixtures replayed, 38 steps total, 27 passed, 11 mismatched, 0 errored, 0 blocked, 9 not_implemented, score_pct 71.1, divergence_count 2.

Cluster filed here: fixture_id textract-sync-text-detection-and-document-analysis, step_key task#2, service textract, operation AnalyzeDocument, verdict mismatch, expected_status 200, actual_status 200, stub_path "" (empty), owned_by [] (empty), affected ["textract-sync-text-detection-and-document-analysis#task#2"], affected_count 1.

Diffs (expected = recorded AWS, got = emulator):
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.9818877577781676,   got 0.1733333319425583
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.009066186845302582, got 0.03333333507180214
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.011760788038372992, got 0.23333333432674408
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.9776000380516052,   got 0.3777777850627899
  $.Blocks[0].Geometry.Polygon[0].X: expected 0.010326294228434564, got 0.04333333298563957
  $.Blocks[0].Geometry.Polygon[0].Y: expected 0.011760788038372992, got 0.23333333432674408
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.986666202545166,    got 0.41111111640930176
  $.Blocks[0].Geometry.Polygon[1].Y: expected 0.01344684325158596,  got 0.25999999046325684
  $.Blocks[0].Geometry.Polygon[2].X: expected 0.9859458804130554,   got 0.4055555462837219
  $.Blocks[0].Geometry.Polygon[2].Y: expected 0.9936485290527344,   got 0.40666666626930237
  $.Blocks[0].Geometry.Polygon[3].X: expected 0.009066186845302582, got 0.03333333507180214
  $.Blocks[0].Geometry.Polygon[3].Y: expected 0.9934206008911132,   got 0.4000000059604645
  $.Blocks[1].Confidence: expected 94.70218658447266, got 82.90326690673828
  $.Blocks[1].Geometry.BoundingBox.{Height,Left,Top,Width}: expected 0.1839762181043625 / 0.032841674983501434 / 0.2324480265378952 / 0.3802343904972077, got 0.1733333319425583 / 0.03333333507180214 / 0.23333333432674408 / 0.3777777850627899
  $.Blocks[1].Geometry.Polygon[0].Y: expected 0.2324480265378952 (emulator value continues the grid constants)

The cluster's divergence_key continues through $.Blocks[1].Text, $.Blocks[2].{Confidence,Text}, $.Blocks[2].Geometry.BoundingBox.* and Polygon[0..3].{X,Y}, $.Blocks[3].{Confidence,Text} and Geometry.*, $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[4].{Confidence,Text}, $.Blocks[4].Geometry.* including RotationAngle, $.Blocks[5].Confidence, $.Blocks[5].EntityTypes[0], $.Blocks[5].Geometry.*, $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and $.Blocks[6].

Three observations that identify the fault rather than the values:
1. The emulator's PAGE box (Blocks[0]) is byte-identical to its Blocks[1] box, while the recorded PAGE box spans the page (Width 0.9776000380516052 / Height 0.9818877577781676). A PAGE block cannot have the extent of one of its own children.
2. The emulator's repeated constants are grid fractions (0.03333333507180214, 0.1733333319425583, 0.3777777850627899, 0.04333333298563957) reused across unrelated blocks, i.e. the character-grid fallback layout at services/service-com-amazonaws-textract-textract/src/state.rs:1822 is served as the answer instead of measured geometry.
3. The divergence extends past coordinates into $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and $.Blocks[6], so the analysis-only block set is invented and mis-linked, not merely misplaced.

No diverging HTTP status, error code or error message: 200 -> 200, body values only.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus). Confirm emulator_up is true and bootstrap_signal is false, so the emulator is actually serving the corpus.
2. Replay fixture textract-sync-text-detection-and-document-analysis, step task#2 — AnalyzeDocument over the fixture's document with the fixture's FeatureTypes. Observe HTTP 200 with a structurally valid BlockList.
3. Compare the response body against the recording field by field. Observe value_mismatch on $.Blocks[*].Geometry.BoundingBox.{Left,Top,Width,Height}, $.Blocks[*].Geometry.Polygon[*].{X,Y}, $.Blocks[*].Geometry.RotationAngle, $.Blocks[*].Confidence and $.Blocks[*].Text.
4. Inspect the emulator's Blocks[0] (PAGE): its BoundingBox is byte-identical to the following block's box instead of spanning the detected page content, and its Polygon corner order differs from the recorded one. Multiply Left/Top/Width/Height by the submitted page's pixel dimensions and confirm the rectangle does not enclose the content it claims to describe; repeat for a WORD/LINE block.
5. Note that the same constants (0.0333…, 0.1733…, 0.3777…, 0.0433…) recur across unrelated blocks — i.e. the character-grid layout at services/service-com-amazonaws-textract-textract/src/state.rs:1822 is being served instead of measured geometry from the raster reader.
6. Walk the feature blocks: observe divergence on $.Blocks[4].BlockType and $.Blocks[4].TextType, on $.Blocks[5].EntityTypes[0] (KEY vs VALUE) and on $.Blocks[5].Relationships[0].Type / $.Blocks[5].Relationships[1], then pair KEY with VALUE by following Relationships and confirm the pairs are not the ones the document implies.
7. After the fix, re-replay textract-sync-text-detection-and-document-analysis#task#2 and require it to pass.
