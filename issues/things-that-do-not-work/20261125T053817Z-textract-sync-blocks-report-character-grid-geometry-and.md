Issue-Source: trace-sweep-divergence
Issue-Trace: textract-sync-text-detection-and-document-analysis
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: textract-sync-text-detection-and-document-analysis
Issue-Shape-Id: com.amazonaws.textract#Textract
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Confidence
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Height
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Left
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Top
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Width
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].X
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].Y
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.RotationAngle
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Confidence
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Height
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Left
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Top
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Width
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].X
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].Y
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Text
Issue-Related: 20261124T064335Z-textract-analyzedocument-blocks-are-a-synthetic-layout-geometry.md
Issue-Commit: 28610a062b9260661e33fa524fd2bdfd7b2024de

# Textract sync Blocks report character-grid geometry and invented feature blocks instead of values measured from the submitted page

> _Note: this issue was identified against the code base at commit `28610a062b9260661e33fa524fd2bdfd7b2024de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectDocumentText

## Description
Both synchronous Textract read paths answer HTTP 200 with a structurally valid BlockList whose per-block CONTENT does not describe the page that was submitted. Status, headers and response framing match the recording exactly; the whole divergence is in response-body VALUES, which is what makes it silent to a client. The shared symptom is that the block geometry the emulator reports is a character-grid/stock layout rather than the normalised extent of the ink each block was read from, and on the analysis path the derived feature blocks are invented rather than derived from the page's detected structure.

This issue has TWO causes.

Cause 1 of 2: DetectDocumentText (fixture textract-sync-text-detection-and-document-analysis, step task#1) returns PAGE/LINE/WORD geometry and confidence that are grid-quantised constants instead of measurements of the page.

AWS: Smithy (services/service-com-amazonaws-textract-textract/smithy-model/com-amazonaws-textract-textract.json): DetectDocumentText input {Document required}, output {DocumentMetadata, Blocks: BlockList, DetectDocumentTextModelVersion}; modeled errors AccessDeniedException / BadDocumentException / DocumentTooLargeException / InvalidParameterException / InvalidS3ObjectException / ProvisionedThroughputExceededException / UnsupportedDocumentException (400) and InternalServerError / ThrottlingException (500). Geometry is "information about where the following items are located on a document page", with members BoundingBox, Polygon and RotationAngle. BoundingBox{Left, Top, Width, Height} is documented with the upper-left corner of the image as origin (0,0); Left/Top are ratios of the overall page width/height and Width/Height are ratios of the page dimensions. Polygon is a list of Point{X, Y} — "within the bounding box, a fine-grained polygon around the recognized item" — so its points are the recognized item's own corners and lie on or inside that item's BoundingBox. Geometry$RotationAngle is the rotation of the WORD block, one of 0, 90, 180, 270. Block$Confidence targets Percent @range 0..100 and is the recogniser's score for the accuracy of the recognized text and of the geometry points around it. Block$Text is "the word or line of text that's recognized". The requirement is general, not one recording's numbers: for any rasterised page, each block's BoundingBox and Polygon must be the normalised extent of that block's OWN ink on that page, a PAGE block must span the content detected on the page (not the extent of one of its children), RotationAngle must be the measured quarter turn, and Confidence must be the measured score in [0,100].

Emulator: every reported block comes back on a fixed character grid that ignores the page. Blocks[0] (PAGE) is served with Left 0.035555556416511536, Top 0.23333333432674408, Width 0.273333340883255, Height 0.1733333319425583 — a word-sized box — where the recording has a page-spanning one (Left 0.011839513666927814, Top 0.10043054074048996, Width 0.8971883058547974, Height 0.875543475151062); the following block Blocks[1] is returned with Height 0.1733333319425583, Left 0.035555556416511536, Top 0.23333333432674408, i.e. byte-identical to the PAGE block that nests it. A PAGE cannot have the extent of its own child. The polygon corner order also differs from the recording (emulator X 0.041111111640930176, 0.30888888239860535, 0.30000001192092896, 0.035555556416511536 vs recorded 0.9090278148651124, 0.011839591898024082, 0.011839513666927814, 0.9090277552604676), so the polygon is not a traversal of the recognized item's corners at the orientation the page was read at. Confidence is a generated number (Blocks[1].Confidence 89.96180725097656 vs recorded 99.9255599975586), and RotationAngle diverges on the blocks that carry it. The constants are grid fractions (1/30, 13/75 …) repeated across blocks, which identifies the fallback character-grid layout rather than a measurement path.

Why: geometry is the reason a client calls Textract instead of reading raw text — it multiplies Left/Top/Width/Height by the page's pixel size and then redacts, highlights, crops or re-flows that region. With one stock box per block and a word-sized PAGE box, the client operates on the wrong region of the page while receiving a 200 that passes every schema, range and ordering check it can make, so there is no signal to fall back on. A client that thresholds on Confidence admits or rejects blocks on a number unrelated to any recognition.

Fix: the shared block derivation in the textract crate's state module — the PAGE/LINE/WORD block builder and the geometry it attaches. It must take per-block extents and confidences from the page raster reader and normalise them by the page's own width and height, and it must only fall back to a laid-out grid when there is genuinely no measurement, instead of serving the grid as the answer. Citations: services/service-com-amazonaws-textract-textract/src/state.rs:2218 (blocks), :1977 (geometry), :1822 (bounding_box, the character-grid layout).

Traces: textract-sync-text-detection-and-document-analysis#task#1 (DetectDocumentText, 200 -> 200, value-only mismatch).

Cause 2 of 2: AnalyzeDocument (fixture textract-sync-text-detection-and-document-analysis, step task#2) additionally invents the analysis-only block set — its BlockType, TextType, EntityTypes and Relationships — and its block list does not line up with the recording's.

AWS: Smithy (same model): AnalyzeDocument input {Document required, FeatureTypes required, HumanLoopConfig, QueriesConfig, AdaptersConfig}, output {DocumentMetadata, Blocks: BlockList, HumanLoopActivationOutput, AnalyzeDocumentModelVersion}; modeled errors as above plus HumanLoopQuotaExceededException (402). Block$BlockType enumerates PAGE ("contains a list of child Block objects that are detected on a document page"), LINE, WORD, KEY_VALUE_SET ("stores the KEY and VALUE Block objects for linked text", with Block$EntityTypes distinguishing KEY from VALUE), TABLE, CELL, SELECTION_ELEMENT and so on; Block$TextType records handwritten vs printed; Block$Relationships "describe how blocks are related to each other" (a LINE carries CHILD to its WORDs, a KEY carries VALUE to its VALUE block). The requirement: for the requested FeatureTypes, the feature blocks that exist, their kind, their entity types and their relationship edges must be derived from the structure detected on the submitted page, and each such block's geometry must span exactly the words it was read from.

Emulator: on top of the same stock geometry and generated confidence as cause 1 (Blocks[0] PAGE box Left 0.03333333507180214, Top 0.23333333432674408, Width 0.3777777850627899, Height 0.1733333319425583 against a recorded page-spanning Left 0.009066186845302582, Top 0.011760788038372992, Width 0.9776000380516052, Height 0.9818877577781676; Blocks[1].Confidence 82.90326690673828 vs recorded 94.70218658447266), the replay reports value_mismatch on $.Blocks[1..4].Text, $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and on into $.Blocks[6]. So the emulator does not merely misplace real blocks: it decides which feature blocks exist, what kind each one is, whether it is a KEY or a VALUE, and which blocks it points at, from a template rather than from the page.

Why: a client walks Relationships to pair KEY with VALUE and switches on BlockType/EntityTypes to decide whether a block is a key, a value or a selection element. Adding that structure is the entire reason to call AnalyzeDocument rather than DetectDocumentText, so there is no second call to cross-check against: the client builds a key/value map that is not in the document and gets a 200 while doing it.

Fix: the AnalyzeDocument response assembly in the textract crate's state module — the derivation that emits the feature blocks (KEY_VALUE_SET / TABLE / CELL / SELECTION_ELEMENT) for the requested FeatureTypes and attaches their BlockType, TextType, EntityTypes, Relationships and geometry. It must enumerate and link those blocks from the page's detected structure, and span each one over the words it was actually read from. Citation: services/service-com-amazonaws-textract-textract/src/state.rs:2218 (blocks, FeatureSet-driven derivation).

Traces: textract-sync-text-detection-and-document-analysis#task#2 (AnalyzeDocument, 200 -> 200, value-only mismatch).

No diverging HTTP status, error code or error message appears in either cause: both are 200 -> 200 with body-value divergence only.

Already-filed context (none of these covers these two steps; replay reports owned_by [] and stub_path "" for both clusters): the open issue 20261124T064335Z-textract-analyzedocument-blocks-are-a-synthetic-layout-geometry.md discusses the AnalyzeDocument synthetic-layout class and names task#2, but the replay no longer reports this cluster as owned by it, so its replay key does not gate the current divergence set. The open issues 20261119T223911Z-textract-ocr-still-misclassifies-glyphs-inside-recognised-words.md, 20261124T021047Z-textract-ocr-glyph-recognition-still-wrong-on-two.md, 20261124T120152Z-textract-detectdocumenttext-document-bytes-misrecognises-glyphs-in-a-rendered.md and 20261030T113700Z-textract-detectdocumenttext-positive-control-after-a-rejected-corrupt-image.md are about recognised glyph TEXT, not about Geometry, Polygon ordering, RotationAngle, Confidence or feature-block classification. The resolved issues 20261115T094519Z-textract-synchronous-blocks-geometry-confidence-and-text-do.md and 20261124T165641Z-textract-synchronous-blocks-carry-placeholder-unit-square-page.md are closed and cover nothing. This bundle exists to carry the replay keys for BOTH steps so a fixer re-runs them.

Note on values: every float, text and enum quoted above is one recording's numbers filling the template, not the requirement. The requirement is the normalisation and derivation rules stated under AWS for each cause.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout: emulator_up true (/__admin/health answered 200 and the :39517 data listener accepts), bootstrap_signal false, 6 fixtures replayed, 38 steps total, 27 passed, 11 mismatched, 0 errored, 0 blocked, 9 not_implemented, score_pct 71.1, divergence_count 2. Both divergence clusters are textract and both are bundled here.

Cause 1 cluster: fixture_id textract-sync-text-detection-and-document-analysis, step_key task#1, service textract, operation DetectDocumentText, verdict mismatch, expected_status 200, actual_status 200, stub_path "" (empty), owned_by [] (empty), affected ["textract-sync-text-detection-and-document-analysis#task#1"], affected_count 1. Diffs (expected = recorded AWS, got = emulator):
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.875543475151062,    got 0.1733333319425583
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.011839513666927814, got 0.035555556416511536
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.10043054074048996,  got 0.23333333432674408
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.8971883058547974,   got 0.273333340883255
  $.Blocks[0].Geometry.Polygon[0].X: expected 0.9090278148651124,   got 0.041111111640930176
  $.Blocks[0].Geometry.Polygon[0].Y: expected 0.975973904132843,    got 0.23333333432674408
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.011839591898024082, got 0.30888888239860535
  $.Blocks[0].Geometry.Polygon[1].Y: expected 0.9759740233421326,  got 0.23999999463558197
  $.Blocks[0].Geometry.Polygon[2].X: expected 0.011839513666927814, got 0.30000001192092896
  $.Blocks[0].Geometry.Polygon[2].Y: expected 0.10043059289455414,  got 0.40666666626930237
  $.Blocks[0].Geometry.Polygon[3].X: expected 0.9090277552604676,   got 0.035555556416511536
  $.Blocks[0].Geometry.Polygon[3].Y: expected 0.10043054074048996,  got 0.3799999952316284
  $.Blocks[1].Confidence: expected 99.9255599975586, got 89.96180725097656
  $.Blocks[1].Geometry.BoundingBox.{Height,Left,Top,Width}: expected 0.1828515976667404 / 0.03252305090427399 / 0.21599966287612915 / 0.27597329020500183, got 0.1733333319425583 / 0.035555556416511536 / 0.23333333432674408 / 0.2733333110809326
The cluster's divergence_key continues through $.Blocks[1].Geometry.Polygon[0..3].{X,Y}, $.Blocks[2].Confidence, $.Blocks[2].Geometry.BoundingBox.*, $.Blocks[2].Geometry.Polygon[0..3].{X,Y}, $.Blocks[2].Geometry.RotationAngle, $.Blocks[3].Confidence, $.Blocks[3].Geometry.BoundingBox.*, $.Blocks[3].Geometry.Polygon[0..3].{X,Y} and $.Blocks[3].Geometry.RotationAngle.

Cause 2 cluster: fixture_id textract-sync-text-detection-and-document-analysis, step_key task#2, service textract, operation AnalyzeDocument, verdict mismatch, expected_status 200, actual_status 200, stub_path "" (empty), owned_by [] (empty), affected ["textract-sync-text-detection-and-document-analysis#task#2"], affected_count 1. Diffs (expected = recorded AWS, got = emulator):
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.9818877577781676,   got 0.1733333319425583
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.009066186845302582, got 0.03333333507180214
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.011760788038372992, got 0.23333333432674408
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.9776000380516052,   got 0.3777777850627899
  $.Blocks[0].Geometry.Polygon[0].X: expected 0.010326294228434564, got 0.04333333298563957
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.986666202545166,    got 0.41111111640930176
  $.Blocks[0].Geometry.Polygon[2].X: expected 0.9859458804130554,   got 0.4055555462837219
  $.Blocks[0].Geometry.Polygon[3].Y: expected 0.9934206008911132,   got 0.4000000059604645
  $.Blocks[1].Confidence: expected 94.70218658447266, got 82.90326690673828
  $.Blocks[1].Geometry.BoundingBox.{Height,Left,Top,Width}: expected 0.1839762181043625 / 0.032841674983501434 / 0.2324480265378952 / 0.3802343904972077, got 0.1733333319425583 / 0.03333333507180214 / 0.23333333432674408 / 0.3777777850627899
The cluster's divergence_key continues through $.Blocks[1].Text, $.Blocks[2].{Confidence,Text}, $.Blocks[2].Geometry.*, $.Blocks[3].{Confidence,Text}, $.Blocks[3].Geometry.*, $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[4].{Confidence,Text}, $.Blocks[4].Geometry.RotationAngle, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and $.Blocks[6].

Two observations that identify the fault rather than the values:
1. In both steps the emulator's PAGE box (Blocks[0]) is byte-identical to its Blocks[1] box, while the recorded PAGE box spans the page (Width 0.897 / Height 0.876 on task#1, Width 0.978 / Height 0.982 on task#2). A PAGE block cannot have the extent of one of its own children.
2. The emulator's repeated constants are grid fractions (0.03333333507180214, 0.1733333319425583, 0.2733333110809326, 0.3777777850627899) reused across blocks and across both operations, which is the character-grid fallback layout (state.rs:1822) being served as the answer rather than a measurement.
3. On task#2 the divergence extends past coordinates into $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and $.Blocks[6], so the analysis-only block set is invented and mis-linked, not merely misplaced.

No diverging HTTP status, error code or error message in either cause: 200 -> 200, body values only.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus). Confirm emulator_up is true and bootstrap_signal is false, so the emulator is actually serving.
2. Cause 1: replay fixture textract-sync-text-detection-and-document-analysis, step task#1 — DetectDocumentText over the fixture's document. Compare the response body against the recording field by field. Observe HTTP 200 with a structurally valid BlockList but $.Blocks[*].Geometry.BoundingBox.{Left,Top,Width,Height}, $.Blocks[*].Geometry.Polygon[*].{X,Y}, $.Blocks[*].Geometry.RotationAngle and $.Blocks[*].Confidence all diverging.
3. Inspect the emulator's Blocks[0] (PAGE): its BoundingBox equals the following block's box rather than spanning the detected page content, and its Polygon corner order differs from the recorded one. Multiply Left/Top/Width/Height by the submitted page's pixel dimensions and confirm the rectangle does not enclose the content it claims to describe; repeat for a WORD/LINE block.
4. Note that the same constants (0.0333…, 0.1733…, 0.2733…/0.3777…) recur across unrelated blocks and across both steps — i.e. the character-grid layout at services/service-com-amazonaws-textract-textract/src/state.rs:1822 is being served instead of measured geometry from the raster reader.
5. Cause 2: replay the same fixture's step task#2 — AnalyzeDocument over the fixture's document bytes with the fixture's FeatureTypes. Observe the same geometry/confidence divergence plus value_mismatch on $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and $.Blocks[6]. Walk Relationships to pair KEY with VALUE and confirm the pairs are not the ones the document implies.
6. After the fix, re-replay BOTH textract-sync-text-detection-and-document-analysis#task#1 (Issue-Trace/Issue-Step) and textract-sync-text-detection-and-document-analysis#task#2 (Issue-Trace-Ids) and require both to pass. Fixing only the detection path leaves task#2 diverging and fixing only the analysis path leaves task#1 diverging, so neither step may be closed on the strength of the other.
