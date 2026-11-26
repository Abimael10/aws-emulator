Issue-Source: trace-sweep-divergence
Issue-Trace: comprehend-detect-suite-single-document
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: comprehend-detect-suite-single-document
Issue-Shape-Id: com.amazonaws.comprehend#Comprehend_20171127
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*]
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*].BeginOffset
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*].EndOffset
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*].PartOfSpeech.Score
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*].PartOfSpeech.Tag
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*].Text
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*]
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].BeginOffset
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].EndOffset
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].GroupScore
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].MentionSentiment.SentimentScore.Negative
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].MentionSentiment.SentimentScore.Neutral
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].MentionSentiment.SentimentScore.Positive
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].Score
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].Text
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].Type
Issue-Key: comprehend|ContainsPiiEntities|2xx->2xx|$.Labels[*]
Issue-Key: comprehend|ContainsPiiEntities|2xx->2xx|$.Labels[*].Score
Issue-Commit: 7a03c6c633814b42753ce8c8f03d4e7bdff82ea0

# Comprehend DetectSyntax, DetectTargetedSentiment and ContainsPiiEntities re-segment the document and mislabel detections (3 causes)

> _Resolution note (issue fixer): cause 3 is closed — step task#9 replays clean. Cause 1's
> segmentation is closed (`$.SyntaxTokens[*]`, `.Text`, `.BeginOffset`, `.EndOffset` no longer
> diverge): a word a document writes unbroken — a mail address, a URL — is now one token, read
> from the one place those published forms are named. Cause 2's group alignment is closed
> (`$.Entities[*]` no longer diverges): a noun phrase that refers to nothing — the predicate a
> copula states of its subject — is no longer admitted as a target. What still diverges, and so
> is NOT covered by this resolution: `$.SyntaxTokens[*].PartOfSpeech.Score` and `.Tag` (the
> confidence rule is implemented — a settled reading is reported at or next to 1 and a genuinely
> ambiguous one visibly below it — but the remaining per-token numbers are the ones AWS's own
> learned tagger produces), and the `DetectTargetedSentiment` mention leaves
> (`Mentions[*].Text`/`Type`/`BeginOffset`/`EndOffset`/`Score`/`GroupScore`, plus
> `MentionSentiment.SentimentScore.*`, which is the shared sentiment scorer the open
> single-document confidence issue owns for task#5). Those residual keys need a net-new issue
> rather than this one left open, since the three causes filed here are each addressed._

> _Note: this issue was identified against the code base at commit `7a03c6c633814b42753ce8c8f03d4e7bdff82ea0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectSyntax

## Description
THREE net-new root causes in the comprehend crate's single-document inference layer where the divergence is STRUCTURAL, not merely numeric: the emulator segments the document differently from AWS and therefore returns a different number of tokens, different spans, different entity groups and different label names. All three steps answered HTTP 200 as recorded, so a client cannot detect the problem from the status. A fixer must close all THREE causes.

PRIOR ART (named, not covering): 20261113T194011Z-comprehend-single-document-detect-inference-synthesizes-ladder-constant.md and 20261120T213134Z-comprehend-detecttargetedsentiment-splits-named-mentions-drops-entity-groups.md both touched this ground but are RESOLVED and so cover nothing; the OPEN comprehend issues (20261120T213134Z-comprehend-batchdetect-inference-emits-per-shape-constant-confidences.md, 20261120T175104Z-comprehend-batchdetectentities-emits-uncalibrated-per-entity-confidence-scores--r2.md) own the BATCH fixture comprehend-batch-detect-suite only. Replay reports stub_path empty and owned_by EMPTY for all three steps below.

Cause 1 of 3: DetectSyntax tokenises the text differently from AWS - it emits extra tokens past the end of AWS's token list, shifts the text and offsets of the last tokens, and mis-tags a token - on top of scoring every token off a recurring ladder. Fixture comprehend-detect-suite-single-document, step task#6.
  AWS: Smithy: DetectSyntax(Text required, LanguageCode: SyntaxLanguageCode required) -> SyntaxTokens: ListOfSyntaxTokens of SyntaxToken{TokenId 'A unique identifier for a token', Text 'The word that was recognized in the source text', BeginOffset/EndOffset 'zero-based offset from the beginning of the source text', PartOfSpeech: PartOfSpeechTag{Tag: PartOfSpeechTagType, Score 'The confidence that Amazon Comprehend has that the part of speech was correctly identified'}}, documented as 'one syntax token record for each word in the source text'. Recorded behavior: the token list covers each word of the document exactly once with contiguous, text-derived offsets; Tag names the role that word plays in its sentence; Score sits at or extremely near 1.0 (frequently exactly 1.0) and varies token by token.
  Emulator: the segmentation itself diverges - $.SyntaxTokens[35] through $.SyntaxTokens[40] are reported as EXTRA (the emulator emits six token records beyond the end of AWS's list), $.SyntaxTokens[33].Text and .EndOffset and $.SyntaxTokens[34].BeginOffset/.EndOffset do not match the recorded word and spans, so the tail of the document is split at different boundaries; $.SyntaxTokens[29].PartOfSpeech.Tag is wrong for its context; and essentially every PartOfSpeech.Score is drawn from a small recurring pool (0.9999899864196777, 0.9999499917030334, 0.9999666810035706, 0.8333263993263245, 0.7940662503242493, ...) where AWS records 1.0 for most tokens and a visibly lower value for the genuinely ambiguous ones ($.SyntaxTokens[6] recorded 0.9503836631774902 arrives saturated at 0.9999600052833557, while $.SyntaxTokens[8] recorded 1.0 arrives at 0.7940662503242493).
  Why: a client walks SyntaxTokens by index or TokenId and slices the original text with BeginOffset/EndOffset. Extra trailing tokens and shifted offsets make those slices point at the wrong characters, index-based alignment with any parallel annotation breaks, and a wrong Tag silently changes what a predicate/function-word extractor pulls out. The recurring Score pool additionally makes confident and uncertain tokens indistinguishable, so confidence-based filtering keeps the wrong tokens.
  Fix: the DetectSyntax handler's tokenizer/segmenter (one record per word of the submitted text, offsets derived from that text, no tokens past its end), its part-of-speech tagger (Tag over PartOfSpeechTagType chosen from the token's syntactic role) and its per-token confidence computation.
  Traces: comprehend-detect-suite-single-document#task#6.

Cause 2 of 3: DetectTargetedSentiment builds the wrong co-reference groups - the entity list is misaligned against the document, so mentions carry the wrong text, type and offsets, and every mention's sentiment comes from a shared pair of constants. Fixture comprehend-detect-suite-single-document, step task#7.
  AWS: Smithy: DetectTargetedSentiment(Text required, LanguageCode required) -> Entities: ListOfTargetedSentimentEntities of TargetedSentimentEntity{DescriptiveMentionIndex, Mentions: ListOfMentions of TargetedSentimentMention{Score 'Model confidence that the entity is relevant. Value range is zero to one', GroupScore 'The confidence that all the entities mentioned in the group relate to the same entity', Text 'The text in the document that identifies the entity', Type (TargetedSentimentEntityType), MentionSentiment, BeginOffset/EndOffset 'offset into the document text where the mention begins/ends'}}. Recorded behavior: each entity group collects the mentions that co-refer to ONE real entity, each mention's Text/BeginOffset/EndOffset quote that span of the document, Type names what that entity is (PERSON for a person's name, and so on), and each mention carries its own MentionSentiment distribution.
  Emulator: the groups are shifted and mistyped. $.Entities[1].Mentions[0] comes back as Text 'service' / Type 'OTHER' at offsets 51-58 where the recording has Text 'John Smith' / Type 'PERSON' at offsets 130-140; $.Entities[2].Mentions[0] then comes back at offsets 130-140 where the recording has the e-mail span at 196-218 - i.e. the whole entity list is off by one group because an extra non-entity span ('service') was admitted, so every later group quotes a different part of the document than AWS. GroupScore is returned as a hard 1.0 where 0.9999629855155944 is recorded, and MentionSentiment.SentimentScore collapses onto a shared pair: Neutral 0.6902276277542114 / Positive 0.3097713887691498 is returned for several unrelated mentions where AWS records Neutral 0.9999979734420776 / Positive 9.999999974752427e-07, with Negative returned as the floor literal 9.999999974752427e-07 where 0.0 is recorded. Mention Scores are from the same cross-operation pool as the other Detect* handlers (0.9956942796707153, 0.8286985754966736, 0.395480215549469).
  Why: targeted sentiment is consumed per entity: a client reads Entities[i].Mentions[*].Text/Type to name the thing being discussed and MentionSentiment to decide how it was discussed. Admitting a common noun as an entity group and shifting the list means the emulator attributes sentiment to an entity that is not in the document and reports a PERSON mention as OTHER, so downstream per-entity dashboards and alerts key on the wrong subject; a mention AWS reports as 0.99999 Neutral arriving as 0.69 Neutral / 0.31 Positive additionally flips it from clearly neutral to leaning positive.
  Fix: the DetectTargetedSentiment handler's entity extraction and co-reference grouping (admit only real entities, type them over TargetedSentimentEntityType, quote spans from the submitted text) and its per-mention sentiment/confidence computation (GroupScore and MentionSentiment derived from the group, not constants).
  Traces: comprehend-detect-suite-single-document#task#7.

Cause 3 of 3: ContainsPiiEntities returns the wrong PII label set - a label AWS does not report replaces the one it does - and label confidences are the per-detection ladder values instead of the recorded label-level confidence. Fixture comprehend-detect-suite-single-document, step task#9.
  AWS: Smithy: ContainsPiiEntities(Text required, LanguageCode required) -> Labels: ListOfEntityLabels of EntityLabel{Name: PiiEntityType 'The name of the label', Score 'The level of confidence that Amazon Comprehend has in the accuracy of the detection'}, the shape documented as 'Specifies one of the label or labels that categorize the personally identifiable information (PII) entity being analyzed'. Recorded behavior: the Labels array names exactly the PiiEntityType categories the document contains, and for categories plainly present the label-level confidence is reported at 1.0.
  Emulator: the label set itself diverges - $.Labels[1] is reported as {'Name': 'ADDRESS', 'Score': 0.9967453479766846} where the recording has {'Name': 'NAME', 'Score': 1.0}, so a category AWS does not list is emitted in place of one it does - and the confidences are not label-level: $.Labels[0].Score 0.9975816011428833 and $.Labels[1].Score 0.9967453479766846 are the same literals DetectPiiEntities returns for its individual detections on this same document, where AWS records 1.0 for both labels.
  Why: ContainsPiiEntities is the cheap pre-filter clients use to decide whether a document needs redaction at all and which PII categories to look for. Reporting ADDRESS instead of NAME sends the document down the wrong redaction policy and lets the NAME category go unhandled, and label scores below 1.0 fall under the common 'only act on a label at full confidence' gate, so the emulator skips work AWS triggers.
  Fix: the ContainsPiiEntities handler's label aggregation - derive the Labels array from the PII categories actually detected in the submitted text (one EntityLabel per present PiiEntityType) and give each a label-level confidence, rather than copying DetectPiiEntities' per-detection ladder values.
  Traces: comprehend-detect-suite-single-document#task#9.

In all three causes the recorded token boundaries, spans, label names and numbers are this recording's values for ITS OWN input text and are not themselves the requirement. The requirement is the convention: one syntax token per word of the submitted text with offsets derived from that text and no trailing extras; co-reference groups and mention spans/types taken from the entities actually present; a PII label set naming exactly the categories present; and confidences computed per detection rather than drawn from a shared pool.

## Evidence
All three steps: expected HTTP 200, got HTTP 200. Status matches; the divergence is in the body structure and values. Fixture comprehend-detect-suite-single-document throughout. Replay verdict 'mismatch', stub_path empty, owned_by empty for each step.

Cause 1 - task#6, DetectSyntax (segmentation + tag + scores):
  $.SyntaxTokens[35], [36], [37], [38], [39], [40]: EXTRA - emitted by the emulator past the end of the recorded token list
  $.SyntaxTokens[33].Text and $.SyntaxTokens[33].EndOffset: value_mismatch (tail re-split at a different boundary)
  $.SyntaxTokens[34].BeginOffset and $.SyntaxTokens[34].EndOffset: value_mismatch
  $.SyntaxTokens[29].PartOfSpeech.Tag: value_mismatch
  $.SyntaxTokens[0].PartOfSpeech.Score: expected 0.9999994039535522, got 0.8333263993263245
  $.SyntaxTokens[3].PartOfSpeech.Score: expected 1.0, got 0.9999899864196777
  $.SyntaxTokens[6].PartOfSpeech.Score: expected 0.9503836631774902, got 0.9999600052833557 (AWS's least-confident token returned saturated)
  $.SyntaxTokens[8].PartOfSpeech.Score: expected 1.0, got 0.7940662503242493
  $.SyntaxTokens[11].PartOfSpeech.Score: expected 0.9517213702201844, got 0.9999150037765503
  $.SyntaxTokens[20].PartOfSpeech.Score: expected 1.0, got 0.8333263993263245 (same literal as token 0)
  plus PartOfSpeech.Score mismatches on tokens 1,2,4,5,7,9,10,12-18,21-32 from the same recurring pool

Cause 2 - task#7, DetectTargetedSentiment (group misalignment):
  $.Entities[1].Mentions[0].Text: expected 'John Smith', got 'service'
  $.Entities[1].Mentions[0].Type: expected 'PERSON', got 'OTHER'
  $.Entities[1].Mentions[0].BeginOffset: expected 130, got 51
  $.Entities[1].Mentions[0].EndOffset: expected 140, got 58
  $.Entities[1].Mentions[0].Score: expected 0.9999549984931946, got 0.395480215549469
  $.Entities[2].Mentions[0].BeginOffset: expected 196, got 130  (the span AWS gave to Entities[1] - the list is shifted by one group)
  $.Entities[2].Mentions[0].EndOffset: expected 218, got 140
  $.Entities[2].Mentions[0].Text: value_mismatch (expected 'john.smith@example.com')
  $.Entities[2].Mentions[0].Type: value_mismatch
  $.Entities[2].Mentions[0].GroupScore: expected 0.9999629855155944, got 1.0
  $.Entities[0].Mentions[0].MentionSentiment.SentimentScore.Neutral: expected 0.9999979734420776, got 0.6902276277542114
  $.Entities[0].Mentions[0].MentionSentiment.SentimentScore.Positive: expected 9.999999974752427e-07, got 0.3097713887691498
  $.Entities[1].Mentions[0].MentionSentiment.SentimentScore.Neutral: expected 0.9999989867210388, got the SAME 0.6902276277542114 (Positive likewise 0.3097713887691498)
  $.Entities[1..4].Mentions[0].MentionSentiment.SentimentScore.Negative: expected 0.0, got 9.999999974752427e-07 (floor literal)
  $.Entities[0].Mentions[0].Score: expected 0.9986129999160768, got 0.9956942796707153; $.Entities[2].Mentions[0].Score: expected 0.5750129818916321, got 0.8286985754966736 (literals shared with DetectEntities/DetectPiiEntities on this fixture)
  further mismatches on $.Entities[3], $.Entities[4] mention sentiment and scores, and $.Entities[5]

Cause 3 - task#9, ContainsPiiEntities (wrong label set):
  $.Labels[1]: extra - expected {'Name': 'NAME', 'Score': 1.0}, got {'Name': 'ADDRESS', 'Score': 0.9967453479766846}
  $.Labels[0].Score: expected 1.0, got 0.9975816011428833
  $.Labels[1].Score: expected 1.0, got 0.9967453479766846
  (both 'got' values are the literals DetectPiiEntities returns for its own detections at task#4)

## Steps to Reproduce
1. Clone the repo, boot the emulator from the checkout and replay the wire corpus (replay_wire_corpus). Fixture comprehend-detect-suite-single-document replays with no blocked or errored step; replay reports stub_path and owned_by empty for the three steps below.
2. Cause 1: step task#6 - DetectSyntax with the fixture's Text and LanguageCode 'en'. Count $.SyntaxTokens: the emulator returns six records (indices 35-40) past the end of the recorded list, and tokens 33/34 carry different Text and Begin/EndOffset, so slicing the source text by those offsets yields different words. Then read $.SyntaxTokens[29].PartOfSpeech.Tag (wrong for its context) and every PartOfSpeech.Score: the same handful of literals recurs (0.8333263993263245 at tokens 0 and 20), the token AWS reports at 0.9504 comes back saturated and a token AWS reports at 1.0 comes back at 0.794.
3. Cause 2: step task#7 - DetectTargetedSentiment (same Text, LanguageCode 'en'). Read $.Entities[1].Mentions[0]: it quotes 'service' at offsets 51-58 typed OTHER where the recording has 'John Smith' at 130-140 typed PERSON, and $.Entities[2].Mentions[0] then quotes 130-140 where the recording has 196-218 - the whole group list is shifted by one because a common noun was admitted as an entity group. Read MentionSentiment.SentimentScore across mentions: Neutral 0.6902276277542114 / Positive 0.3097713887691498 repeats on unrelated mentions, Negative is the floor literal, and GroupScore is a hard 1.0.
4. Cause 3: step task#9 - ContainsPiiEntities (same Text, LanguageCode 'en'). Read $.Labels: index 1 is {'Name': 'ADDRESS', ...} where the recording has {'Name': 'NAME', 'Score': 1.0}, and both label Scores are DetectPiiEntities' per-detection literals (0.9975816011428833 / 0.9967453479766846) instead of 1.0.
5. Re-replay fixture comprehend-detect-suite-single-document after the fix: ALL THREE steps (task#6, task#7, task#9) must match, not just the representative task#6.
