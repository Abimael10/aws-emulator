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
Issue-Commit: 2609c9d3e41fb0a74c389bfc9db2db8a80f77cb8

# Textract synchronous Blocks carry placeholder unit-square PAGE geometry and grid-quantised child geometry/confidence instead of values measured from the submitted page

> _Note: this issue was identified against the code base at commit `2609c9d3e41fb0a74c389bfc9db2db8a80f77cb8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectDocumentText

## Description
Both synchronous Textract analysis entry points answer HTTP 200 with a structurally valid BlockList whose per-block Geometry and Confidence do not describe the document that was submitted. Status, headers and response framing all match the recording; the whole divergence is in response-body VALUES, which is what makes it silent to a client. The shared fault is that the block builder does not measure the submitted page: the PAGE block is emitted as a placeholder covering the entire normalised page (BoundingBox Left 0 / Top 0 / Width 1 / Height 1 with unit-square polygon corners), and the text blocks under it get geometry and confidence from a coarse fixed grid / generator rather than from the detected ink.

This issue has TWO causes.

Cause 1 of 2: DetectDocumentText emits a unit-square placeholder PAGE geometry and grid-quantised WORD/LINE geometry and confidence. Fixture textract-sync-text-detection-and-document-analysis, step task#1.
AWS: Smithy (services/service-com-amazonaws-textract-textract/smithy-model/com-amazonaws-textract-textract.json): DetectDocumentText input {Document required}, output {DocumentMetadata, Blocks: BlockList, DetectDocumentTextModelVersion}; modeled errors AccessDeniedException / BadDocumentException / DocumentTooLargeException / InvalidParameterException / InvalidS3ObjectException / ProvisionedThroughputExceededException / UnsupportedDocumentException (400), InternalServerError / ThrottlingException (500). Shape BoundingBox documents Left/Top as 'the left/top coordinate of the bounding box as a ratio of overall document page width/height' with 'the upper-left corner of the image is the origin (0,0)' and Width/Height as ratios of the page dimensions (the model's own example: a 700x200 page with a top-left box corner at 350x50 px yields Left 0.5, Top 0.25; a 70 px wide box on a 700 px page yields Width 0.1). Shape Point: X/Y 'are ratios of the overall document page size' and Polygon is 'within the bounding box, a fine-grained polygon around the recognized item', so its points traverse the recognised item's own corners and lie on or inside that item's BoundingBox. Block$Confidence targets Percent (@range 0..100) and is 'the confidence score that Amazon Textract has in the accuracy of the recognized text and the accuracy of the geometry points around the recognized text'. Block$BlockType PAGE 'contains a list of the LINE Block objects that are detected on a document page'. The requirement is general: for any rasterised page, each block's BoundingBox and Polygon must be that block's own ink extent normalised by the page's own width and height, the PAGE block must span the page's detected content (not the whole canvas by construction), and Confidence must be the recogniser's score for that block.
Emulator: the PAGE block is returned with BoundingBox Left 0.0, Top 0.0, Width 1.0, Height 1.0 where the recording has the detected content extent (Left 0.011839513666927814, Top 0.10043054074048996, Width 0.8971883058547974, Height 0.875543475151062) - i.e. the degenerate 'whole canvas' placeholder rather than a measurement - and its Polygon is the four unit-square corners emitted in an order that does not match the recorded corner traversal (emulator X 0.0, 1.0, 1.0, 0.0 / Y 0.0, 0.0, 1.0, 1.0 vs recorded X 0.9090278148651124, 0.011839591898024082, 0.011839513666927814, 0.9090277552604676). Every text block beneath it then diverges the same way: Blocks[1] BoundingBox comes back Left 0.035555556416511536 / Top 0.23333333432674408 / Width 0.2733333110809326 / Height 0.1733333319425583 against recorded 0.03252305090427399 / 0.21599966287612915 / 0.27597329020500183 / 0.1828515976667404, and Blocks[1].Confidence comes back 89.96180725097656 against recorded 99.9255599975586. The emulator's values are multiples of a coarse cell (0.1733333 = 26/150, 0.0355555 = 3.2/90), so they are snapped to a fixed grid instead of derived from the glyph extents, and the confidence is a generated number. The cluster's divergence_key enumerates the same divergence on $.Blocks[2].* and $.Blocks[3].* including $.Blocks[2].Geometry.RotationAngle and $.Blocks[3].Geometry.RotationAngle.
Why: geometry is the reason a client calls Textract rather than reading plain text - it multiplies Left/Top/Width/Height by the page's pixel size and then crops, redacts, highlights or re-flows that region. A PAGE block that is the whole canvas by construction makes any page-relative re-normalisation or deskew wrong, and grid-snapped child boxes put the client's crop next to the text instead of on it. A client that thresholds on Confidence admits or rejects blocks on a number unrelated to any recognition. All of it arrives as a 200 that passes every schema, range and ordering check the client can make, so there is no signal to fall back on.
Fix: the DetectDocumentText response assembly in the textract crate's state/block-builder module - the shared geometry builder that fills BoundingBox / Polygon / RotationAngle and the confidence source. It must consume measured per-block extents from the page raster reader and normalise them by the submitted page's own width and height (PAGE from the page's detected content, WORD/LINE from each item's ink), and report the recogniser's per-block score, instead of emitting a unit-square page box and grid-quantised children.
Traces: textract-sync-text-detection-and-document-analysis#task#1 (DetectDocumentText, 200 -> 200, value-only mismatch).

Cause 2 of 2: AnalyzeDocument shows the identical placeholder/quantised geometry and confidence on its own blocks, and additionally diverges on block classification and linkage. Fixture textract-sync-text-detection-and-document-analysis, step task#2.
AWS: Smithy (same model): AnalyzeDocument input {Document required, FeatureTypes required, HumanLoopConfig, QueriesConfig, AdaptersConfig}, output {DocumentMetadata, Blocks: BlockList, HumanLoopActivationOutput, AnalyzeDocumentModelVersion}; modeled errors as for DetectDocumentText plus HumanLoopQuotaExceededException (402). Geometry/BoundingBox/Point carry the same normalisation rules cited in cause 1; Geometry$RotationAngle is 'a numerical value corresponding to the rotation of the WORD block. Possible values are 0, 90, 180, and 270'. Block$Text is 'the word or line of text that's recognized by Amazon Textract'; Block$BlockType enumerates PAGE / LINE / WORD / KEY_VALUE_SET ('stores the KEY and VALUE Block objects for linked text', with EntityTypes distinguishing KEY from VALUE) / TABLE / CELL / SELECTION_ELEMENT; Block$TextType records handwritten vs printed; Block$Relationships 'describe how blocks are related to each other' (a LINE carries CHILD to its WORDs, a KEY carries VALUE to its VALUE block). The requirement is that for the requested FeatureTypes the derived blocks, their types, entity types and relationships follow the page's detected structure, with each block's geometry measured as above.
Emulator: Blocks[0] (PAGE) again comes back Left 0.0 / Top 0.0 / Width 1.0 / Height 1.0 with unit-square polygon corners where the recording has the detected extent (Left 0.009066186845302582, Top 0.011760788038372992, Width 0.9776000380516052, Height 0.9818877577781676 and polygon X 0.010326294228434564, 0.986666202545166, 0.9859458804130554, 0.009066186845302582). Blocks[1] comes back Left 0.03333333507180214 / Top 0.23333333432674408 / Width 0.3777777850627899 / Height 0.1733333319425583 against recorded 0.032841674983501434 / 0.2324480265378952 / 0.3802343904972077 / 0.1839762181043625 - the same grid multiples as cause 1 - and Blocks[1].Confidence 82.90326690673828 against recorded 94.70218658447266. The cluster's divergence_key additionally reports value_mismatch on $.Blocks[1].Text, $.Blocks[2].Text, $.Blocks[3].Text, $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[4].Geometry.RotationAngle, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and on into $.Blocks[6], so the analysis-only blocks are also misclassified and mis-linked, not merely mis-placed.
Why: on top of the wrong-region problem of cause 1, a client that switches on BlockType / EntityTypes or walks Relationships to pair KEY with VALUE builds a key/value map that is not in the document - and AnalyzeDocument is the call that is supposed to add that structure, so there is no second call to cross-check it against. Again 200 with a schema-valid body, so the failure is silent.
Fix: the AnalyzeDocument response assembly in the same textract crate state module - the shared block/geometry builder (same defect as cause 1) plus the derivation that attaches BlockType, TextType, EntityTypes and Relationships to the feature blocks emitted for the requested FeatureTypes, which must come from the page's detected structure.
Traces: textract-sync-text-detection-and-document-analysis#task#2 (AnalyzeDocument, 200 -> 200, value-only mismatch).

Already-filed context: the open issue 20261124T064335Z-textract-analyzedocument-blocks-are-a-synthetic-layout-geometry.md names the same fixture/step for cause 2, but it was filed against commit dd48a918628c6f5a00bf2be4caa0b22ec4cdd0a8 and describes a DIFFERENT emulator behaviour (its evidence records the PAGE block coming back with the stock word-sized box 0.0333/0.2333/0.3778/0.1733); the emulator now returns the unit-square placeholder 0.0/0.0/1.0/1.0 for PAGE instead, and the replay reports owned_by [] for both clusters, so neither step's current replay key is carried by an open issue. The other open textract issues (20261119T223911Z, 20261124T021047Z, 20261124T120152Z, 20261030T113700Z) are about recognised glyph/word text on other paths and none of them is about BoundingBox / Polygon / RotationAngle / Confidence on these two steps. This issue exists to carry both steps' replay keys.

Note on values: every float quoted above is one recording's numbers filling the template, not the requirement. No HTTP status, error code or error message diverges anywhere in this bundle (200 -> 200 on both steps); the requirement is the normalisation and derivation rules stated under AWS for each cause.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout: emulator_up true (/__admin/health answered 200 and the :36913 data listener accepts), bootstrap_signal false, 6 fixtures replayed, 38 steps total, 27 passed, 11 mismatched, 0 errored, 0 blocked, 9 not_implemented, score 71.1%, divergence_count 2. Both divergence clusters are bundled here.

Cluster A (representative, Issue-Trace/Issue-Step): fixture_id textract-sync-text-detection-and-document-analysis, step_key task#1, service textract, operation DetectDocumentText, verdict mismatch, expected_status 200, actual_status 200, stub_path "" (empty), owned_by [] (empty), affected ["textract-sync-text-detection-and-document-analysis#task#1"], affected_count 1.
Diffs reported by the replay (expected = recorded AWS, got = emulator):
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.875543475151062,     got 1.0
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.011839513666927814,  got 0.0
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.10043054074048996,   got 0.0
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.8971883058547974,    got 1.0
  $.Blocks[0].Geometry.Polygon[0].X: expected 0.9090278148651124,   got 0.0
  $.Blocks[0].Geometry.Polygon[0].Y: expected 0.975973904132843,    got 0.0
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.011839591898024082, got 1.0
  $.Blocks[0].Geometry.Polygon[1].Y: expected 0.9759740233421326,   got 0.0
  $.Blocks[0].Geometry.Polygon[2].X: expected 0.011839513666927814, got 1.0
  $.Blocks[0].Geometry.Polygon[2].Y: expected 0.10043059289455414,  got 1.0
  $.Blocks[0].Geometry.Polygon[3].X: expected 0.9090277552604676,   got 0.0
  $.Blocks[0].Geometry.Polygon[3].Y: expected 0.10043054074048996,  got 1.0
  $.Blocks[1].Confidence: expected 99.9255599975586, got 89.96180725097656
  $.Blocks[1].Geometry.BoundingBox.Height: expected 0.1828515976667404,   got 0.1733333319425583
  $.Blocks[1].Geometry.BoundingBox.Left:   expected 0.03252305090427399,  got 0.035555556416511536
  $.Blocks[1].Geometry.BoundingBox.Top:    expected 0.21599966287612915,  got 0.23333333432674408
  $.Blocks[1].Geometry.BoundingBox.Width:  expected 0.27597329020500183,  got 0.2733333110809326
  $.Blocks[1].Geometry.Polygon[0].X: expected 0.3084963262081146, got 0.041111111640930176
  $.Blocks[1].Geometry.Polygon[0].Y: expected 0.398851215839386,  got 0.23333333432674408
  $.Blocks[1].Geometry.Polygon[1].X: expected 0.03252306953072548, got 0.30888888239860535
Its divergence_key continues over $.Blocks[1].Polygon[2..3], $.Blocks[2].Confidence, $.Blocks[2].Geometry.BoundingBox.*, $.Blocks[2].Geometry.Polygon[0..3].{X,Y}, $.Blocks[2].Geometry.RotationAngle, $.Blocks[3].Confidence, $.Blocks[3].Geometry.BoundingBox.*, $.Blocks[3].Geometry.Polygon[0..3].{X,Y} and $.Blocks[3].Geometry.RotationAngle.

Cluster B (named in Issue-Trace-Ids): fixture_id textract-sync-text-detection-and-document-analysis, step_key task#2, service textract, operation AnalyzeDocument, verdict mismatch, expected_status 200, actual_status 200, stub_path "" (empty), owned_by [] (empty), affected ["textract-sync-text-detection-and-document-analysis#task#2"], affected_count 1.
Diffs reported by the replay:
  $.Blocks[0].Geometry.BoundingBox.Height: expected 0.9818877577781676,   got 1.0
  $.Blocks[0].Geometry.BoundingBox.Left:   expected 0.009066186845302582, got 0.0
  $.Blocks[0].Geometry.BoundingBox.Top:    expected 0.011760788038372992, got 0.0
  $.Blocks[0].Geometry.BoundingBox.Width:  expected 0.9776000380516052,   got 1.0
  $.Blocks[0].Geometry.Polygon[0].X: expected 0.010326294228434564, got 0.0
  $.Blocks[0].Geometry.Polygon[0].Y: expected 0.011760788038372992, got 0.0
  $.Blocks[0].Geometry.Polygon[1].X: expected 0.986666202545166,    got 1.0
  $.Blocks[0].Geometry.Polygon[1].Y: expected 0.01344684325158596,  got 0.0
  $.Blocks[0].Geometry.Polygon[2].X: expected 0.9859458804130554,   got 1.0
  $.Blocks[0].Geometry.Polygon[2].Y: expected 0.9936485290527344,   got 1.0
  $.Blocks[0].Geometry.Polygon[3].X: expected 0.009066186845302582, got 0.0
  $.Blocks[0].Geometry.Polygon[3].Y: expected 0.9934206008911132,   got 1.0
  $.Blocks[1].Confidence: expected 94.70218658447266, got 82.90326690673828
  $.Blocks[1].Geometry.BoundingBox.Height: expected 0.1839762181043625,   got 0.1733333319425583
  $.Blocks[1].Geometry.BoundingBox.Left:   expected 0.032841674983501434, got 0.03333333507180214
  $.Blocks[1].Geometry.BoundingBox.Top:    expected 0.2324480265378952,   got 0.23333333432674408
  $.Blocks[1].Geometry.BoundingBox.Width:  expected 0.3802343904972077,   got 0.3777777850627899
  $.Blocks[1].Geometry.Polygon[0].X: expected 0.03307491913437843, got 0.04333333298563957
  $.Blocks[1].Geometry.Polygon[0].Y: expected 0.2324480265378952,  got 0.23333333432674408
  $.Blocks[1].Geometry.Polygon[1].X: expected 0.4130760729312897,  got 0.41111111640930176
Its divergence_key additionally enumerates $.Blocks[1].Text, $.Blocks[2].Text, $.Blocks[3].Text, $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[4].Geometry.RotationAngle, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type, $.Blocks[5].Relationships[1] and $.Blocks[6].

Two observations that identify the fault rather than the values:
1. On BOTH steps the PAGE block (Blocks[0]) is returned as the degenerate whole-canvas box Left 0.0 / Top 0.0 / Width 1.0 / Height 1.0 with the four unit-square polygon corners, while the recordings give a measured content extent with a non-trivial corner traversal. A unit square is a placeholder, not a measurement, and it is identical across two different submitted documents.
2. The emulator's child-block coordinates are multiples of a coarse cell (0.1733333319425583 = 26/150, 0.035555556416511536 = 3.2/90, 0.2733333110809326 = 24.6/90), i.e. snapped to a fixed grid, and Confidence is a generated value (89.96180725097656 / 82.90326690673828) rather than a per-block recognition score.

No diverging HTTP status, error code or error message was observed on either step: 200 -> 200 with value-only divergence in the response body. All quoted floats are one recording's values filling the template.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus). Confirm bootstrap_signal is false and emulator_up is true, so the emulator is actually serving textract.
2. Cause 1: replay fixture textract-sync-text-detection-and-document-analysis, step task#1 - DetectDocumentText over the fixture's document bytes. Compare the response body against the recording field by field. Observe HTTP 200 with a valid BlockList but $.Blocks[*].Geometry.BoundingBox.{Left,Top,Width,Height}, $.Blocks[*].Geometry.Polygon[*].{X,Y}, $.Blocks[*].Geometry.RotationAngle and $.Blocks[*].Confidence diverging.
3. Inspect that response's Blocks[0] (PAGE): confirm it is Left 0.0 / Top 0.0 / Width 1.0 / Height 1.0 with unit-square polygon corners, i.e. the whole canvas regardless of where the page's content actually is, and that its polygon corner order does not match the recorded traversal.
4. Multiply a WORD/LINE block's Left/Top/Width/Height by the submitted page's pixel dimensions and confirm the resulting rectangle does not enclose that block's drawn glyphs; confirm the coordinates are multiples of a fixed grid cell rather than ink-derived.
5. Cause 2: replay the same fixture's step task#2 - AnalyzeDocument over the fixture's document bytes with the fixture's FeatureTypes. Observe the same unit-square PAGE box and the same grid-snapped child geometry and generated Confidence, plus divergence on $.Blocks[4].BlockType, $.Blocks[4].TextType, $.Blocks[5].EntityTypes[0], $.Blocks[5].Relationships[0].Type and $.Blocks[5].Relationships[1]; walk Relationships to pair KEY with VALUE and confirm the pairs are not in the submitted document.
6. After the fix, re-replay BOTH steps named by this issue and require both to pass: textract-sync-text-detection-and-document-analysis#task#1 (Issue-Trace/Issue-Step) and textract-sync-text-detection-and-document-analysis#task#2 (Issue-Trace-Ids). This issue has two causes; fixing only the detection path leaves task#2 diverging and fixing only the analysis path leaves task#1 diverging, so neither may be closed on the strength of the other.
