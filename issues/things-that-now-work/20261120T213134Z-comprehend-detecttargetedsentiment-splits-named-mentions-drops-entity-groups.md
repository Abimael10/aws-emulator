Issue-Source: trace-sweep-divergence
Issue-Trace: comprehend-detect-suite-single-document
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: comprehend-detect-suite-single-document
Issue-Shape-Id: com.amazonaws.comprehend#Comprehend_20171127
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].MentionSentiment.SentimentScore.Mixed
Issue-Related: 20261113T194011Z-comprehend-single-document-detect-inference-synthesizes-ladder-constant.md
Issue-Commit: 2b7ef28fb79df90eb190b034f8bd9899c7a59aff

# Comprehend DetectTargetedSentiment splits named mentions, drops entity groups and flattens mention sentiment

> _Note: this issue was identified against the code base at commit `2b7ef28fb79df90eb190b034f8bd9899c7a59aff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectTargetedSentiment

## Description
ONE root cause in the comprehend crate's DetectTargetedSentiment handler (fixture comprehend-detect-suite-single-document, step task#7). The step answered HTTP 200 exactly as recorded and the response shape is right (Entities -> Mentions -> MentionSentiment.SentimentScore), so the divergence is entirely in the mention segmentation, the entity typing, the number of entity groups and the mention sentiment values.

PRIOR ART (named, not covering): 20261113T194011Z-comprehend-single-document-detect-inference-synthesizes-ladder-constant.md is open and describes a DetectTargetedSentiment cause on this same fixture/step, but replay reports owned_by EMPTY for this step, so that issue does not carry this step's replay key; the current divergence is also a DIFFERENT value set than the one it records (mention sentiment now splits Neutral 0.6902268528938293 / Positive 0.3097711503505707 where that issue records 0.5322045087814331 / 0.46580344438552856, and the mention Score is now 0.949999988079071), i.e. the handler has drifted and the divergence persists. Filed here net-new so the step has an issue that carries its key.

  AWS: Smithy: DetectTargetedSentiment(Text required CustomerInputString @length min 1, LanguageCode required) -> Entities: ListOfTargetedSentimentEntities of TargetedSentimentEntity{DescriptiveMentionIndex, Mentions}; TargetedSentimentMention{Score 'Model confidence that the entity is relevant. Value range is zero to one, where one is highest confidence', GroupScore 'The confidence that all the entities mentioned in the group relate to the same entity', Text 'The text in the document that identifies the entity', Type (TargetedSentimentEntityType, including ORGANIZATION/PERSON/COMMERCIAL_ITEM), MentionSentiment{Sentiment, SentimentScore{Positive,Negative,Neutral,Mixed}}, BeginOffset 'The offset into the document text where the mention begins', EndOffset}. Recorded behavior: AWS returns one entity GROUP per distinct entity the document discusses; each mention spans the WHOLE named mention at its true offsets in the source text, Type names the real entity class of that mention, and a purely factual mention is reported NEUTRAL with SentimentScore.Neutral saturated (~0.999998) and the other three members at or near zero (~1e-06 / 0.0), with Score and GroupScore per mention.
  Emulator: multi-word named mentions are cut at the first word boundary and the halves become separate groups — $.Entities[0].Mentions[0].Text expected 'Amazon Comprehend' got 'Amazon' with EndOffset expected 17 got 6, and $.Entities[1].Mentions[0].Text expected 'John Smith' got 'Comprehend' with BeginOffset expected 130 got 7 and EndOffset expected 140 got 17, that fragment typed 'COMMERCIAL_ITEM' where 'PERSON' is recorded; every subsequent group is therefore pointed at the wrong span ($.Entities[2].Mentions[0].BeginOffset expected 196 got 51, EndOffset expected 218 got 58, and the same for $.Entities[3] and $.Entities[4], including their Type), and two recorded groups are absent outright ($.Entities[5] and $.Entities[6] missing). The mention sentiment is the SAME flat split for every mention — Neutral 0.6902268528938293 with Positive 0.3097711503505707 where AWS records Neutral 0.9999979734420776/0.9999989867210388 and Positive ~9.999999974752427e-07 — the residual members are the shared floor literal 9.999960184359225e-07 where AWS records 0.0 or 1e-06, and Score/GroupScore are per-shape constants (0.949999988079071 for every mention).
  Why: targeted sentiment answers 'which entity, where in the text, and how is it spoken of'. Splitting a named mention hands the client a fragment ('Amazon' instead of 'Amazon Comprehend'), the knock-on offsets mis-align every later mention against the source document so highlighting and redaction land on the wrong characters, the wrong Type routes the entity to the wrong handling, two entities the document discusses vanish from the analysis, and a flat 0.69/0.31 Neutral/Positive split on a factual mention turns a decisive NEUTRAL into a borderline one so any per-entity sentiment aggregate computed over the document is wrong.
  Fix: the DetectTargetedSentiment handler — its mention detector and co-reference grouping (one group per entity, each mention span covering the whole named mention at its own offsets, no group dropped), its entity typer over TargetedSentimentEntityType, and its mention sentiment scorer plus Score/GroupScore computation (one normalised distribution per mention concentrated on the reported Sentiment, confidence derived per mention rather than from a constant).
  Traces: comprehend-detect-suite-single-document#task#7.

The recorded texts, offsets, types and numbers are this recording's values for ITS OWN input document and are not themselves the requirement. The requirement is the convention: one group per entity with every mention span bounded by the full named mention it describes, Type drawn from the modelled enum by what the mention actually is, no recorded group missing, and one normalised SentimentScore per mention concentrated on its reported Sentiment instead of a literal repeated across mentions.

## Evidence
Step comprehend-detect-suite-single-document#task#7, DetectTargetedSentiment: expected HTTP 200, got HTTP 200 — status and shape match, body values diverge. Replay verdict 'mismatch', stub_path empty, owned_by empty.

  $.Entities[0].Mentions[0].Text: expected 'Amazon Comprehend', got 'Amazon'; EndOffset expected 17, got 6
  $.Entities[1].Mentions[0].Text: expected 'John Smith', got 'Comprehend'; BeginOffset expected 130, got 7; EndOffset expected 140, got 17; Type expected 'PERSON', got 'COMMERCIAL_ITEM'
  $.Entities[2].Mentions[0].BeginOffset: expected 196, got 51; EndOffset expected 218, got 58; GroupScore diverges
  $.Entities[3].Mentions[0] and $.Entities[4].Mentions[0]: BeginOffset/EndOffset/Text/Type/Score all diverge (same off-by-group shift)
  $.Entities[5], $.Entities[6]: missing (two recorded entity groups absent)
  $.Entities[0].Mentions[0].MentionSentiment.SentimentScore.Neutral: expected 0.9999979734420776, got 0.6902268528938293; Positive expected 9.999999974752427e-07, got 0.3097711503505707
  $.Entities[1].Mentions[0].MentionSentiment.SentimentScore.Neutral: expected 0.9999989867210388, got the SAME 0.6902268528938293; Positive expected 9.999999974752427e-07, got the SAME 0.3097711503505707
  $.Entities[0|1].Mentions[0].MentionSentiment.SentimentScore.Mixed: expected 0.0, got 9.999960184359225e-07; Negative expected 9.999999974752427e-07 / 0.0, got 9.999960184359225e-07
  $.Entities[0].Mentions[0].Score: expected 0.9986129999160768, got 0.949999988079071; $.Entities[1].Mentions[0].Score: expected 0.9999549984931946, got the SAME 0.949999988079071

The identical Neutral/Positive pair and the identical mention Score across unrelated mentions, and the floor literal 9.999960184359225e-07 (also returned by this emulator's BatchDetectSentiment path), show the values come from a fixed table rather than per-mention inference.

## Steps to Reproduce
1. Clone the repo, boot the emulator from the checkout and replay the wire corpus (replay_wire_corpus). Fixture comprehend-detect-suite-single-document replays with no blocked step; replay reports stub_path and owned_by empty for step task#7.
2. Step task#7 — DetectTargetedSentiment with the fixture's Text (a document naming a multi-word product, a person and a contact address) and LanguageCode 'en'.
3. Read $.Entities[0].Mentions[0].Text/EndOffset: the mention is the fragment 'Amazon' ending at offset 6 instead of the full 'Amazon Comprehend' ending at 17, and the second half appears as its OWN group in $.Entities[1], typed COMMERCIAL_ITEM where PERSON is recorded.
4. Count $.Entities: the emulator returns two groups fewer than recorded ($.Entities[5] and $.Entities[6] missing), and every group after the split carries the wrong BeginOffset/EndOffset against the source text.
5. Read $.Entities[*].Mentions[0].MentionSentiment.SentimentScore: every mention carries the same Neutral 0.6902268528938293 / Positive 0.3097711503505707 split with the residual members at the single literal 9.999960184359225e-07, and every mention's Score is 0.949999988079071.
6. Re-replay fixture comprehend-detect-suite-single-document step task#7 after the fix to confirm.
