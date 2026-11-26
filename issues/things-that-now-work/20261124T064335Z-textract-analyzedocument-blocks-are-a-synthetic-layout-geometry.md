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
Issue-Related: 20261115T094519Z-textract-synchronous-blocks-geometry-confidence-and-text-do.md
Issue-Commit: dd48a918628c6f5a00bf2be4caa0b22ec4cdd0a8

# Textract AnalyzeDocument blocks are a synthetic layout: geometry, confidence, text and feature-block classification do not describe the submitted document

> _Note: this issue was identified against the code base at commit `dd48a918628c6f5a00bf2be4caa0b22ec4cdd0a8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AnalyzeDocument

## Description
AnalyzeDocument answers HTTP 200 with a Blocks list whose per-block CONTENT does not describe the document that was submitted. For the same request bytes the recording and the emulator agree on status, headers and overall response framing, but diverge on every block's Geometry.BoundingBox, every Geometry.Polygon point, Geometry.RotationAngle, Block$Confidence, Block$Text and - on the analysis-only blocks - Block$BlockType, Block$TextType, Block$EntityTypes and Block$Relationships. There is no diverging HTTP status, error code or error message anywhere in this step: the entire divergence is in response-body values, which is what makes it silent to a client.

This issue has ONE cause.

Cause 1 of 1: AnalyzeDocument assembles its Blocks from a fixed synthetic layout instead of deriving them from the submitted page, so both the geometry/confidence/text of the text blocks and the classification/linkage of the derived feature blocks diverge.

AWS: Smithy (services/service-com-amazonaws-textract-textract/smithy-model/com-amazonaws-textract-textract.json): AnalyzeDocument input {Document required, FeatureTypes required, HumanLoopConfig, QueriesConfig, AdaptersConfig}, output {DocumentMetadata, Blocks: BlockList, HumanLoopActivationOutput, AnalyzeDocumentModelVersion}; modeled errors AccessDeniedException/BadDocumentException/DocumentTooLargeException (400), HumanLoopQuotaExceededException (402), InvalidParameterException/InvalidS3ObjectException/ProvisionedThroughputExceededException/UnsupportedDocumentException (400), InternalServerError/ThrottlingException (500). Geometry is documented as "Information about where the following items are located on a document page: detected page, text, key-value pairs, tables, table cells, and selection elements", with members BoundingBox, Polygon and RotationAngle. BoundingBox{Left, Top, Width, Height}: "the upper-left corner of the image is the origin (0,0)"; Left and Top are ratios of overall page width/height (the model's own example: a 700x200 page with a top-left box corner at 350x50 px yields Left 0.5 and Top 0.25) and Width/Height are ratios of the page dimensions (a 70 px wide box on a 700 px page yields Width 0.1). Polygon is a list of Point{X, Y}, "within the bounding box, a fine-grained polygon around the recognized item", with X/Y likewise ratios of the page size, so its points are the corners of the recognized item and lie on or inside that item's BoundingBox. Geometry$RotationAngle is "a numerical value corresponding to the rotation of the WORD block. Possible values are 0, 90, 180, and 270". Block$Confidence targets Percent @range 0..100 and is "the confidence score that Amazon Textract has in the accuracy of the recognized text and the accuracy of the geometry points around the recognized text". Block$Text is "the word or line of text that's recognized". Block$BlockType enumerates PAGE ("contains a list of child Block objects that are detected on a document page"), LINE, WORD, KEY_VALUE_SET ("stores the KEY and VALUE Block objects for linked text", with EntityTypes distinguishing KEY from VALUE), TABLE, CELL, SELECTION_ELEMENT etc.; Block$TextType records handwritten vs printed; Block$Relationships "describe how blocks are related to each other" (a LINE carries CHILD to its WORDs, a KEY carries VALUE to its VALUE block). The requirement is general: for any rasterised page, each block's BoundingBox and Polygon must be the normalised extent of that block's own ink in that page, the PAGE block must span the page's detected content, RotationAngle must be the block's measured orientation out of {0, 90, 180, 270}, Confidence must be the recogniser's score in [0,100] for that block, Text must be the text actually recognised, and the derived feature blocks must carry the BlockType / TextType / EntityTypes / Relationships that the page's detected structure implies.

Emulator: the response is structurally well-formed - no missing or extra top-level keys are reported and the block count lines up - but the values come from a stock layout that ignores the submitted document. The PAGE block (Blocks[0]) is returned with a word-sized box (Left 0.03333333507180214, Top 0.23333333432674408, Width 0.3777777850627899, Height 0.1733333319425583) where the recording has a page-spanning one (Left 0.009066186845302582, Top 0.011760788038372992, Width 0.9776000380516052, Height 0.9818877577781676), so the block documented as containing the whole page's children is given the extent of a single word. Its Polygon is emitted in a different corner order/orientation from the recorded one (emulator Polygon X 0.0433, 0.4111, 0.4056, 0.0333 vs recorded 0.0103, 0.9867, 0.9859, 0.0091), so the page's orientation is not reflected. Subsequent blocks repeat the same stock geometry (Blocks[1] comes back with Height 0.1733333319425583 / Left 0.03333333507180214 / Top 0.23333333432674408 / Width 0.3777777850627899, i.e. byte-identical to the PAGE block it is nested in), Confidence is a generated number (Blocks[1].Confidence 82.90326690673828 where the recording has 94.70218658447266), Geometry.RotationAngle diverges on the blocks that carry it, and Block$Text diverges on the text blocks. Beyond geometry, the cluster's divergence key reports value_mismatch on $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and on into $.Blocks[6], so the analysis-only blocks are misclassified and mis-linked as well: the emulator does not merely place real blocks in the wrong spot, it invents which blocks exist, what kind each is and how they relate.

Why: geometry is the entire reason a client calls AnalyzeDocument rather than reading raw text - it multiplies Left/Top/Width/Height by the page's pixel dimensions and then redacts, highlights, crops or re-flows that region. With one stock box for every block, and a PAGE block the size of a word, the client acts on the wrong region of the page while receiving a 200 that passes every schema, range and ordering check it can make, so there is no signal to fall back on. A client that thresholds on Confidence admits or rejects blocks on a number unrelated to any recognition. A client that walks Relationships to pair KEY with VALUE, or that switches on BlockType/EntityTypes to decide whether a block is a key, a value or a selection element, builds a key/value map that does not exist in the document - and because AnalyzeDocument is the call that is supposed to add that structure, there is no second call to cross-check it against. Every failure mode here is silent.

Fix: the AnalyzeDocument response assembly in the textract crate's state module - the shared block/geometry builder it calls for PAGE/LINE/WORD blocks plus the derivation that attaches geometry, BlockType, TextType, EntityTypes and Relationships to the feature blocks (KEY_VALUE_SET / TABLE / CELL / SELECTION_ELEMENT) it emits for the requested FeatureTypes. It must consume measured per-block geometry, confidence and text from the page raster reader and normalise by the page's own width/height, and it must derive the feature blocks and their relationships from the page's detected structure rather than from a fixed layout.

Already-filed context: the open issue 20261115T094519Z-textract-synchronous-blocks-geometry-confidence-and-text-do.md describes this divergence class and discusses AnalyzeDocument in prose, but its replay pointer does not cover this step - its Issue-Trace/Issue-Step name task#1 (DetectDocumentText) and its Issue-Trace-Ids token is the bare fixture id "textract-sync-text-detection-and-document-analysis" with no #step half, so it binds no second step. The replay accordingly reports owned_by: [] for this cluster while reporting the task#1 cluster as owned. The related open behavioral issues 20261113T235649Z-textract-raster-reading-is-wrong-end-to-end.md, 20261110T200812Z (TABLES CELL text) and 20261114T190633Z (QUERIES QUERY_RESULT text) are grounded in behavioral check assertions about recognised text and missing derivations, and none of them is about Geometry, Polygon ordering, RotationAngle, Confidence or block classification, so none covers this wire step either. This issue exists to carry task#2's replay key so a fixer re-runs it.

Note on values: the floats, texts and enum values quoted above are one recording's numbers filling the template, not the requirement. The requirement is the normalisation and derivation rules stated under AWS above.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout: emulator_up true (/__admin/health answered 200 and the :34017 data listener accepts), bootstrap_signal false, 6 fixtures replayed, 38 steps total, 27 passed, 11 mismatched, 0 errored, 0 blocked, 9 not_implemented, score 71.1%, divergence_count 2.

This issue is the cluster reported as: fixture_id textract-sync-text-detection-and-document-analysis, step_key task#2, service textract, operation AnalyzeDocument, verdict mismatch, expected_status 200, actual_status 200, stub_path "" (empty), owned_by [] (empty), affected ["textract-sync-text-detection-and-document-analysis#task#2"], affected_count 1.

Status, headers and response shape all matched; only body VALUES diverged. Diffs reported by the replay (expected = recorded AWS, got = emulator):
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.9818877577781676, got 0.1733333319425583
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.009066186845302582, got 0.03333333507180214
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.011760788038372992, got 0.23333333432674408
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.9776000380516052, got 0.3777777850627899
  $.Blocks[0].Geometry.Polygon[0].X: expected 0.010326294228434564, got 0.04333333298563957
  $.Blocks[0].Geometry.Polygon[0].Y: expected 0.011760788038372992, got 0.23333333432674408
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.986666202545166,    got 0.41111111640930176
  $.Blocks[0].Geometry.Polygon[1].Y: expected 0.01344684325158596,  got 0.25999999046325684
  $.Blocks[0].Geometry.Polygon[2].X: expected 0.9859458804130554,   got 0.4055555462837219
  $.Blocks[0].Geometry.Polygon[2].Y: expected 0.9936485290527344,   got 0.40666666626930237
  $.Blocks[0].Geometry.Polygon[3].X: expected 0.009066186845302582, got 0.03333333507180214
  $.Blocks[0].Geometry.Polygon[3].Y: expected 0.9934206008911132,   got 0.4000000059604645
  $.Blocks[1].Confidence: expected 94.70218658447266, got 82.90326690673828
  $.Blocks[1].Geometry.BoundingBox.Height: expected 0.1839762181043625,   got 0.1733333319425583
  $.Blocks[1].Geometry.BoundingBox.Left:   expected 0.032841674983501434, got 0.03333333507180214
  $.Blocks[1].Geometry.BoundingBox.Top:    expected 0.2324480265378952,   got 0.23333333432674408
  $.Blocks[1].Geometry.BoundingBox.Width:  expected 0.3802343904972077,   got 0.3777777850627899
  $.Blocks[1].Geometry.Polygon[0].X: expected 0.03307491913437843, got 0.04333333298563957

The cluster's divergence_key enumerates the full set of diverging paths for this step, which continues beyond the diff excerpt above:
  $.Blocks[1].Text, $.Blocks[2].Confidence, $.Blocks[2].Geometry.BoundingBox.{Height,Left,Top,Width}, $.Blocks[2].Geometry.Polygon[0..3].{X,Y}, $.Blocks[2].Text,
  $.Blocks[3].Confidence, $.Blocks[3].Geometry.BoundingBox.*, $.Blocks[3].Geometry.Polygon[0..3].{X,Y}, $.Blocks[3].Text,
  $.Blocks[4].BlockType, $.Blocks[4].Confidence, $.Blocks[4].Geometry.BoundingBox.*, $.Blocks[4].Geometry.Polygon[0..3].{X,Y}, $.Blocks[4].Geometry.RotationAngle, $.Blocks[4].Text, $.Blocks[4].TextType,
  $.Blocks[5].Confidence, $.Blocks[5].EntityTypes[0], $.Blocks[5].Geometry.BoundingBox.*, $.Blocks[5].Geometry.Polygon[0..3].{X,Y}, $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1],
  $.Blocks[6] ...

Two observations that identify the fault rather than the values:
1. The emulator's Blocks[0] (PAGE) BoundingBox is byte-for-byte identical to its Blocks[1] box (Left 0.03333333507180214, Top 0.23333333432674408, Width 0.3777777850627899, Height 0.1733333319425583), whereas the recorded PAGE box spans the page (Width 0.9776, Height 0.9819). A PAGE block cannot have the extent of one of its own children.
2. The emulator's polygon corner order (X: 0.0433, 0.4111, 0.4056, 0.0333) differs from the recorded order (X: 0.0103, 0.9867, 0.9859, 0.0091), so the polygon is not a traversal of the recognized item's corners on this page.
3. $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type and $.Blocks[5].Relationships[1] diverge, so the analysis-only blocks are misclassified and mis-linked - this is not only a coordinate problem.

No diverging HTTP status, error code or error message was observed: 200 -> 200 with value-only divergence in the response body. All quoted floats/enums are one recording's values filling the template; the requirement is the rule stated in the description.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus). Confirm bootstrap_signal is false and emulator_up is true, so the emulator is actually serving.
2. Replay fixture textract-sync-text-detection-and-document-analysis, step task#2 - AnalyzeDocument over the fixture's document bytes with the fixture's FeatureTypes. Compare the response body against the recording field by field.
3. Observe HTTP 200 with the right block count and a structurally valid BlockList, but $.Blocks[*].Geometry.BoundingBox.{Left,Top,Width,Height}, $.Blocks[*].Geometry.Polygon[*].{X,Y}, $.Blocks[*].Geometry.RotationAngle, $.Blocks[*].Confidence and $.Blocks[*].Text all diverging.
4. Inspect the PAGE block (Blocks[0]) in the emulator's response: its BoundingBox is identical to the following text block's box rather than spanning the page's content, and its Polygon corner order differs from the recorded one. Multiply its Left/Top/Width/Height by the submitted page's pixel dimensions and confirm the resulting rectangle does not enclose the content it claims to describe; repeat for a WORD/LINE block and confirm the same.
5. Inspect the analysis-only blocks: compare $.Blocks[4].BlockType and $.Blocks[4].TextType, and $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type and $.Blocks[5].Relationships[1], against the recording. Confirm the emulator emits a different block kind / entity type / relationship set than the document implies, so walking Relationships to pair KEY with VALUE yields pairs that are not in the document.
6. After the fix, re-replay textract-sync-text-detection-and-document-analysis#task#2 (the step named in Issue-Trace/Issue-Step) and require it to pass. Note that the sibling step task#1 (DetectDocumentText) is owned by issue 20261115T094519Z-textract-synchronous-blocks-geometry-confidence-and-text-do.md; fixing only the detection path will leave this step diverging, and fixing only this path will leave that step diverging, so both must be re-replayed.
