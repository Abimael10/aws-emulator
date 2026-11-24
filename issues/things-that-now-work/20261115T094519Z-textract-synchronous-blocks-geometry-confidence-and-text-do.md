Issue-Source: trace-sweep-divergence
Issue-Trace: textract-sync-text-detection-and-document-analysis
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: textract-sync-text-detection-and-document-analysis
Issue-Shape-Id: com.amazonaws.textract#Textract
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*]
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Confidence
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Height
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Left
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Top
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Width
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].X
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].Y
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Geometry.RotationAngle
Issue-Key: textract|DetectDocumentText|2xx->2xx|$.Blocks[*].Text
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Confidence
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Height
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Left
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Top
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.BoundingBox.Width
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].X
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Geometry.Polygon[*].Y
Issue-Key: textract|AnalyzeDocument|2xx->2xx|$.Blocks[*].Text
Issue-Commit: e0c14c163b8da9dd370f62569dab38f2b4fb063e

# Textract synchronous Blocks geometry, confidence and text do not describe the submitted page

> _Note: this issue was identified against the code base at commit `e0c14c163b8da9dd370f62569dab38f2b4fb063e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectDocumentText

## Description
Both synchronous Textract read paths answer HTTP 200 with a structurally valid, correctly shaped Blocks list whose per-block CONTENT does not describe the document that was submitted: every Geometry.BoundingBox, every Geometry.Polygon point, Geometry.RotationAngle, Block$Confidence and Block$Text diverges from the recorded AWS response for the same request bytes. There is no diverging status, no error code and no error message anywhere in this fixture - the whole divergence is in the values of the response body, which is what makes it silent to a client.

This issue bundles 2 distinct causes (one per operation / derivation path). A fixer must close BOTH before submitting.

Context on what is already filed: the open behavioral issue 20261113T235649Z-textract-raster-reading-is-wrong-end-to-end.md and the open 20261110T200812Z (TABLES CELL text) / 20261114T190633Z (QUERIES QUERY_RESULT text) issues are all grounded in behavioral-check assertions about recognised TEXT and about missing FORMS/TABLES/QUERIES/Expense derivations. None of them carries this fixture's replay key, and none of them is about Geometry, Polygon ordering, RotationAngle or Confidence at all, so none covers the wire divergence reported here; the replay reported owned_by: [] for both clusters. The open 20261030T113700Z (corrupt-image positive control) and 20261109T200548Z (CloudFormation AWS::Textract::Adapter) issues own unrelated checks.

Cause 1 of 2: DetectDocumentText returns Blocks whose geometry, polygon, confidence and text are synthesized rather than measured off the submitted page, and the PAGE block's box does not span the page's content.
AWS: Smithy (services/service-com-amazonaws-textract-textract/smithy-model/com-amazonaws-textract-textract.json): DetectDocumentText input {Document required}, output {DocumentMetadata, Blocks: BlockList, DetectDocumentTextModelVersion}. Block$Geometry is documented as "The location of the recognized text on the image"; Geometry{BoundingBox, Polygon, RotationAngle}; BoundingBox{Left, Top, Width, Height} are documented as ratios of the overall document page size with the image's upper-left corner as origin (0,0) - Left = x_px/page_width, Top = y_px/page_height, Width = w_px/page_width, Height = h_px/page_height; Polygon is a list of Point{X, Y}, "within the bounding box, a fine-grained polygon around the recognized item", so its points are the corners of the recognized item and lie on/in its BoundingBox; Geometry$RotationAngle is documented as the rotation of the WORD block with possible values 0, 90, 180, 270; Block$Confidence targets Percent @range 0..100 and is "the confidence score that Amazon Textract has in the accuracy of the recognized text and the accuracy of the geometry points"; BlockType$PAGE "contains a list of the LINE Block objects that are detected on a document page", so the PAGE block's geometry describes the page/content extent, not an individual word. The requirement is general: for any rasterised page, each block's BoundingBox and Polygon must be derived from the pixels of that block in that page, the PAGE block must span the page's detected content, RotationAngle must be the block's measured orientation out of {0,90,180,270}, and Confidence must be the recogniser's score for that block.
Emulator: the block count and the structure match (no missing/extra keys are reported), but the values are from a fixed synthetic layout. The PAGE block (Blocks[0]) comes back with exactly the same box as the first text block (Left 0.035555556416511536, Top 0.23333333432674408, Width 0.2733333706855774, Height 0.1733333319425583) instead of a page-spanning box (recorded AWS: Left 0.011839513666927814, Top 0.10043054074048996, Width 0.8971883058547974, Height 0.875543475151062) - i.e. the PAGE block is given a word-sized box rather than the extent of the page's content. Its Polygon is emitted in a different corner order/orientation from the recorded one (emulator Polygon X values 0.0411, 0.3089, 0.3000, 0.0356 vs recorded 0.9090, 0.0118, 0.0118, 0.9090), so the page's orientation is ignored. Downstream blocks repeat the same stock geometry (several blocks share Height 0.1733333319425583 and Left 0.035555556416511536), Confidence is a generated value (e.g. 86.90715026855469 where the recording has 99.9255599975586), Geometry.RotationAngle diverges on the block that carries it, and Block$Text diverges from the recorded word/line text. Nothing about the response signals that the geometry is not measured.
Why: Geometry is the whole point of the Blocks list for a client that redacts, highlights, crops or re-flows a scanned document: it multiplies Left/Top/Width/Height by the page's pixel dimensions and acts on that region. With a stock box for every block - and a PAGE block the size of a word - a client masks or crops the wrong region of the page while receiving a 200 that passes every schema, range and ordering check it can make. A client that thresholds on Confidence accepts or rejects blocks on a number unrelated to the recognition, and one that reads Text indexes characters the page does not contain. Every failure mode is silent.
Fix: the synchronous DetectDocumentText block derivation in the textract crate's state module (the block builder that fills Geometry/BoundingBox/Polygon/RotationAngle and Confidence for PAGE/LINE/WORD blocks, fed by the page-raster reader) - boxes and polygons must be computed from the recognized ink of each block in the submitted raster and normalised by the page's own width/height, the PAGE block must span the detected content, and Confidence must come from the recogniser rather than a generated constant.
Traces: textract-sync-text-detection-and-document-analysis#task#1 (DetectDocumentText, expected 200, actual 200, value_mismatch on $.Blocks[*].Geometry.BoundingBox.{Left,Top,Width,Height}, $.Blocks[*].Geometry.Polygon[*].{X,Y}, $.Blocks[*].Geometry.RotationAngle, $.Blocks[*].Confidence, $.Blocks[*].Text).

Cause 2 of 2: AnalyzeDocument's block geometry, confidence and text diverge the same way on the analysis path, so the defect is not confined to the detection handler.
AWS: Smithy: AnalyzeDocument input {Document required, FeatureTypes required, HumanLoopConfig, QueriesConfig, AdaptersConfig}, output {DocumentMetadata, Blocks: BlockList, HumanLoopActivationOutput, AnalyzeDocumentModelVersion}. The same Geometry/BoundingBox/Point/Confidence contract applies to every block AnalyzeDocument returns (Block$Geometry is documented for "detected page, text, key-value pairs, tables, table cells, and selection elements"), so for the same page AnalyzeDocument's PAGE/LINE/WORD geometry and confidences agree with what DetectDocumentText reports for that page, measured off the submitted raster.
Emulator: same divergence class on the AnalyzeDocument step of the same fixture: the PAGE block again carries a word-sized stock box (Left 0.03333333507180214, Top 0.23333333432674408, Width 0.3777777850627899, Height 0.1733333319425583) where the recording has a page-spanning one (Left 0.009066186845302582, Top 0.011760788038372992, Width 0.9776000380516052, Height 0.9818877577781676); the Polygon corner order again differs; Confidence diverges on every text block (e.g. 87.1407470703125 vs the recorded 94.70218658447266); and Block$Text diverges. Hypothesis: both operations build blocks through one shared layout/geometry helper, so a single stock-layout source feeds both paths - but it is reported as a separate cause because AnalyzeDocument has its own response assembly (it must additionally attach geometry to the feature blocks it derives) and a fix to the detection path alone would not be observable here.
Why: a client that asks for AnalyzeDocument because it needs geometry for the features it extracts (cells to crop, key/value regions to highlight) gets a 200 whose coordinates point at the wrong part of the page, and gets geometry that disagrees with what the detection call reports for the same document, so cross-checking the two calls cannot reveal the problem either.
Fix: the AnalyzeDocument response assembly in the textract crate's state module (the shared block/geometry builder it calls, plus the geometry attached to the derived feature blocks) - it must consume the measured per-block geometry and confidence from the page reader, the same source Cause 1 fixes, instead of a synthesized layout.
Traces: textract-sync-text-detection-and-document-analysis#task#2 (AnalyzeDocument, expected 200, actual 200, value_mismatch on $.Blocks[*].Geometry.BoundingBox.*, $.Blocks[*].Geometry.Polygon[*].*, $.Blocks[*].Confidence, $.Blocks[*].Text).

Note on values: the floats quoted above are one recording's numbers filling the template, not the requirement. The requirement is that each block's box/polygon be the normalised extent of that block's ink in the submitted page, that the PAGE block span the page's content, that RotationAngle be one of 0/90/180/270 for the block's measured orientation, and that Confidence be the recogniser's score in [0,100].

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout: emulator_up true (/__admin/health answered 200 and the :44761 data listener accepts), bootstrap_signal false, 6 fixtures replayed, 38 steps, 27 passed, score 71.1%. Two divergence clusters remained after the stub-blocked/not-implemented steps, both service=textract, both in fixture textract-sync-text-detection-and-document-analysis, both with stub_path empty and owned_by empty.

Cluster 1 - step task#1, operation DetectDocumentText, verdict mismatch, expected_status 200, actual_status 200. Status, headers and response shape all matched; only values diverged. Representative diffs reported by the replay:
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.875543475151062, got 0.1733333319425583
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.011839513666927814, got 0.035555556416511536
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.10043054074048996, got 0.23333333432674408
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.8971883058547974, got 0.2733333706855774
  $.Blocks[0].Geometry.Polygon[0].X: expected 0.9090278148651124, got 0.041111111640930176
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.011839591898024082, got 0.30888888239860535
  $.Blocks[0].Geometry.Polygon[2].Y: expected 0.10043059289455414, got 0.40666666626930237
  $.Blocks[0].Geometry.Polygon[3].X: expected 0.9090277552604676, got 0.035555556416511536
  $.Blocks[1].Confidence: expected 99.9255599975586, got 86.90715026855469
  $.Blocks[1].Geometry.BoundingBox.{Height,Left,Top,Width}: all mismatched
  ... and, per the cluster's divergence key, the same mismatch on $.Blocks[1].Text, $.Blocks[2].Confidence, $.Blocks[2].Geometry.* including $.Blocks[2].Geometry.RotationAngle, $.Blocks[2].Text and on into the remaining blocks.
Note the shape of the error: the emulator's Blocks[0] (PAGE) box is byte-for-byte the same stock box as its Blocks[1] (Left 0.035555556416511536 / Height 0.1733333319425583), whereas the recorded PAGE box spans the page (Width 0.897, Height 0.876). The emulator's polygon corner order (X: 0.041, 0.309, 0.300, 0.036) differs from the recorded order (X: 0.909, 0.012, 0.012, 0.909).

Cluster 2 - step task#2, operation AnalyzeDocument, verdict mismatch, expected_status 200, actual_status 200:
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.9818877577781676, got 0.1733333319425583
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.009066186845302582, got 0.03333333507180214
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.011760788038372992, got 0.23333333432674408
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.9776000380516052, got 0.3777777850627899
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.986666202545166, got 0.41111111640930176
  $.Blocks[0].Geometry.Polygon[2].Y: expected 0.9936485290527344, got 0.40666666626930237
  $.Blocks[1].Confidence: expected 94.70218658447266, got 87.1407470703125
  $.Blocks[1].Geometry.BoundingBox.{Height,Left,Top,Width} and $.Blocks[1].Geometry.Polygon[*].{X,Y}: all mismatched
  ... continuing through $.Blocks[6].Geometry.Polygon[3].Y plus $.Blocks[1..3].Text per the cluster's divergence key.

No diverging HTTP status, error code or error message was observed on either step: both are 200 -> 200 with value-only divergence in the response body. The quoted floats are one recording's values; the requirement is the normalisation rule in the description.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
2. Cause 1: replay fixture textract-sync-text-detection-and-document-analysis, step task#1 - DetectDocumentText over the fixture's document bytes. Compare the response body against the recording field by field. Observe HTTP 200 with the right block count and shape, but $.Blocks[*].Geometry.BoundingBox.{Left,Top,Width,Height}, $.Blocks[*].Geometry.Polygon[*].{X,Y}, $.Blocks[*].Geometry.RotationAngle, $.Blocks[*].Confidence and $.Blocks[*].Text all diverging.
3. In the same response, check the PAGE block (Blocks[0]): its BoundingBox is identical to the following text block's box instead of spanning the page content, and its Polygon corner order differs from the recorded one. Multiply Left/Top/Width/Height by the submitted page's pixel dimensions and confirm the resulting rectangle does not enclose the ink it claims to describe.
4. Cause 2: replay the same fixture's step task#2 - AnalyzeDocument over the same document. Observe the identical divergence class (PAGE block given a word-sized stock box, polygon order differing, Confidence generated, Text diverging), confirming the analysis path does not derive geometry from the raster either, and that its geometry disagrees with what DetectDocumentText reports for the same page.
5. After the fix, re-replay BOTH steps named in Issue-Trace/Issue-Step and Issue-Trace-Ids - textract-sync-text-detection-and-document-analysis#task#1 and textract-sync-text-detection-and-document-analysis#task#2 - and require both to pass; fixing only the detection path leaves the AnalyzeDocument step diverging.
