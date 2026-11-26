Issue-Source: trace-sweep-divergence
Issue-Trace: comprehend-detect-suite-single-document
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: comprehend-detect-suite-single-document
Issue-Shape-Id: com.amazonaws.comprehend#Comprehend_20171127
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*].PartOfSpeech.Score
Issue-Key: comprehend|DetectSyntax|2xx->2xx|$.SyntaxTokens[*].PartOfSpeech.Tag
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].BeginOffset
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].EndOffset
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].GroupScore
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].MentionSentiment.SentimentScore.Negative
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].MentionSentiment.SentimentScore.Neutral
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].MentionSentiment.SentimentScore.Positive
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].Score
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].Text
Issue-Key: comprehend|DetectTargetedSentiment|2xx->2xx|$.Entities[*].Mentions[*].Type
Issue-Commit: 832e9f5059543777b388ffa865d01db1cd157680

# Comprehend single-document DetectSyntax and DetectTargetedSentiment mislabel tokens and misalign entity mentions (2 causes)

> _Note: this issue was identified against the code base at commit `832e9f5059543777b388ffa865d01db1cd157680`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DetectSyntax

## Description
TWO net-new root causes in the comprehend crate's SINGLE-DOCUMENT synchronous inference layer, both exercised by fixture comprehend-detect-suite-single-document. Both steps answered HTTP 200 exactly as recorded and the modelled output members are present, so the divergence is entirely in the analysed VALUES and in the per-mention structure: the emulator labels syntax tokens and lays out targeted-sentiment mentions from fixed per-shape tables instead of analysing the submitted document. A fixer must close BOTH causes (task#6 and task#7) and re-replay both steps.

PRIOR ART (named, NOT covering): 20261125T014337Z-comprehend-single-document-detect-confidence-scores-are-drawn.md is OPEN on this same fixture but owns only task#1/task#2/task#4/task#5/task#8 (DetectDominantLanguage, DetectEntities, DetectPiiEntities, DetectSentiment, DetectToxicContent); 20261120T213134Z-comprehend-batchdetect-inference-emits-per-shape-constant-confidences.md and 20261120T175104Z-comprehend-batchdetectentities-emits-uncalibrated-per-entity-confidence-scores--r2.md are OPEN on the BATCH fixture comprehend-batch-detect-suite only. Replay reports stub_path empty and owned_by EMPTY for the two steps below, i.e. no open issue carries their replay key.

Cause 1 of 2: DetectSyntax assigns the wrong PartOfSpeech.Tag to tokens and takes PartOfSpeech.Score from a small constant ladder (1.0 / 0.9998999834060669 / 0.9999799728393555) rather than from the token in context. Fixture comprehend-detect-suite-single-document, step task#6.
  AWS: Smithy: DetectSyntax(Text required, CustomerInputString @length min 1 sensitive; LanguageCode required, SyntaxLanguageCode) -> SyntaxTokens: ListOfSyntaxTokens of SyntaxToken{TokenId, Text, BeginOffset, EndOffset, PartOfSpeech: PartOfSpeechTag{Tag: PartOfSpeechTagType enum [ADJ, ADP, ADV, AUX, CONJ, CCONJ, DET, INTJ, NOUN, NUM, O, PART, PRON, PROPN, PUNCT, SCONJ, SYM, VERB], Score 'The confidence that Amazon Comprehend has that the part of speech was correctly identified'}}. There is one record per word, each carrying the part of speech of THAT word in THAT sentence and its own confidence; AUX is reserved for auxiliary/copular verbs and is distinct from VERB, and O is the catch-all for a token with no assignable part of speech, so a common noun is never tagged O. Recorded behavior: confidences vary per token across several decimal places (e.g. 0.9503836631774902 and 0.9517213702201844 on genuinely ambiguous tokens, near-1.0 on unambiguous ones), and no single literal repeats across unrelated tokens.
  Emulator: tags are wrong on specific tokens and the scores collapse onto three literals. $.SyntaxTokens[29].PartOfSpeech.Tag comes back 'VERB' where the recording has 'AUX'; $.SyntaxTokens[33].PartOfSpeech.Tag comes back 'O' where the recording has 'NOUN'. On the score side, 1.0 is returned for tokens recorded at 0.9999939203262328, 0.9999998807907104, 0.9999991655349731, 0.9999997615814208, 0.9999922513961792, 0.9910897016525269, 0.9999996423721312, 0.9999945163726808, 0.9999885559082032, 0.9999988079071044 and 0.9870320558547974; 0.9998999834060669 is returned for tokens recorded at 0.9999998807907104 and 1.0; 0.9999799728393555 is returned for tokens recorded at 0.9999994039535522 and 1.0; and the two genuinely ambiguous tokens come back 0.9852216839790344 / 0.9638554453849792 against recorded 0.9503836631774902 / 0.9517213702201844 (22 diverging members in all, through $.SyntaxTokens[34].PartOfSpeech.Score).
  Why: a client uses DetectSyntax output to filter or chunk text by part of speech — collecting NOUN/PROPN tokens as candidate terms, treating AUX separately from VERB when extracting predicates, and dropping O. Tagging an auxiliary as VERB and a noun as O changes the token set the client extracts for the same text, so downstream term lists and dependency heuristics differ from AWS's. The ladder scores additionally make every per-token confidence threshold ('accept above 0.99') tie or saturate, so no token is ever flagged ambiguous.
  Fix: the DetectSyntax handler's tokenizer/tagger in the comprehend crate — assign PartOfSpeechTagType per token from that token's role in the submitted sentence (AUX vs VERB, O only for genuinely unassignable tokens) and derive PartOfSpeech.Score from that tagging decision instead of selecting from a fixed set of literals.
  Traces: comprehend-detect-suite-single-document#task#6.

Cause 2 of 2: DetectTargetedSentiment emits mentions that do not correspond to the spans in the submitted document — mention Text/Type and Begin/EndOffset belong to a different entity than the one the group names — and fills MentionSentiment.SentimentScore from the same floor/ladder table the other sentiment paths use. Fixture comprehend-detect-suite-single-document, step task#7.
  AWS: Smithy: DetectTargetedSentiment(Text required sensitive, LanguageCode required) -> Entities: ListOfTargetedSentimentEntities of TargetedSentimentEntity{DescriptiveMentionIndex, Mentions: ListOfMentions of TargetedSentimentMention{Score 'Model confidence that the entity is relevant. Value range is zero to one', GroupScore 'The confidence that all the entities mentioned in the group relate to the same entity', Text 'The text in the document that identifies the entity', Type (TargetedSentimentEntityType), MentionSentiment: MentionSentiment{Sentiment, SentimentScore{Positive, Negative, Neutral, Mixed}}, BeginOffset 'The offset into the document text where the mention begins', EndOffset}}. Each mention is therefore anchored to a real span: Text MUST be the substring of the submitted document between BeginOffset and EndOffset, Type MUST be the entity type of that span, and the per-mention SentimentScore is one normalised distribution over that mention. Recorded behavior: residual sentiment members take small but mutually distinct magnitudes (and exact 0.0 where the recording has it), each mention has its own Score and GroupScore, and an e-mail-address mention is typed OTHER while an organization mention is typed ORGANIZATION.
  Emulator: the mention layout is shifted. $.Entities[2].Mentions[0].Text comes back 'Example Corp' where the recording has 'john.smith@example.com', with Type 'ORGANIZATION' where the recording has 'OTHER', and BeginOffset/EndOffset 150/162 where the recording has 196/218 — i.e. the mention is reported at another entity's span, and the same shift continues into $.Entities[3].Mentions[0] (BeginOffset 166 vs recorded 150, EndOffset 173 vs recorded 162, with Text and Type likewise diverging) and $.Entities[4].Mentions[0]. On the value side, mention Scores repeat literals used by other handlers (0.9956942796707153 for a mention recorded at 0.9986129999160768; 0.8286985754966736 for one recorded at 0.9999549984931946 — the same literal DetectEntities and DetectPiiEntities return on this fixture; 0.8121159076690674 for one recorded at 0.5750129818916321), GroupScore is saturated to 1.0 where the recording has 0.9999629855155944, and every MentionSentiment.SentimentScore is drawn from the fixed pair {9.999999974752427e-07 floor, 0.9978637099266052 / 0.002135267248377204}: Neutral 0.6902276277542114 and Positive 0.3097713887691498 against recorded 0.9999979734420776 / 9.999999974752427e-07, Negative 9.999999974752427e-07 where the recording has exactly 0.0, and Neutral 0.9978637099266052 / Positive 0.002135267248377204 repeated verbatim for several unrelated mentions (31 diverging members in all).
  Why: a targeted-sentiment client slices the document with BeginOffset/EndOffset to highlight each mention and keys its per-aspect verdict on Type and on the mention's SentimentScore. When Text/Type/offsets name a different span than AWS reports, the client highlights and attributes sentiment to the wrong part of the text — an e-mail address is reported as an organization, and the aspect-level report is about entities the document does not place there. Reusing one SentimentScore distribution for several mentions makes every mention look equally confident, and 0.690/0.310 instead of ~1.0/~1e-06 flips a decisive NEUTRAL mention into an ambiguous one.
  Fix: the DetectTargetedSentiment handler in the comprehend crate — anchor every TargetedSentimentMention to the span it actually matched in the submitted Text (Text == document[BeginOffset..EndOffset], Type from that span's entity type), and compute Score, GroupScore and a normalised per-mention MentionSentiment.SentimentScore from that mention instead of the shared floor/ladder table used by DetectSentiment and the batch sentiment path.
  Traces: comprehend-detect-suite-single-document#task#7.

In both causes the recorded numbers, tags and offsets are this recording's values for ITS OWN input text and are NOT themselves the requirement. The requirements are the conventions: one part-of-speech tag per token chosen from PartOfSpeechTagType according to that token's role (AUX distinct from VERB, O only when nothing applies) with a confidence derived from that decision; and one mention per real span, whose Text is exactly the document substring between its own BeginOffset and EndOffset, whose Type describes that span, and whose SentimentScore is a normalised distribution computed for that mention — never a literal shared across unrelated tokens, mentions or operations.

## Evidence
Both steps: expected HTTP 200, got HTTP 200. Status and response shape match; the divergence is entirely in body values and per-mention span data. Fixture comprehend-detect-suite-single-document. Replay verdict 'mismatch', stub_path empty, owned_by EMPTY for both steps.

Cause 1 - step task#6, DetectSyntax (22 diverging members):
  $.SyntaxTokens[29].PartOfSpeech.Tag: expected 'AUX', got 'VERB'
  $.SyntaxTokens[33].PartOfSpeech.Tag: expected 'NOUN', got 'O'
  $.SyntaxTokens[0].PartOfSpeech.Score: expected 0.9999994039535522, got 0.9999799728393555
  $.SyntaxTokens[1].PartOfSpeech.Score: expected 0.9999939203262328, got 1.0
  $.SyntaxTokens[2].PartOfSpeech.Score: expected 0.9999998807907104, got 0.9998999834060669
  $.SyntaxTokens[4].PartOfSpeech.Score: expected 0.9999998807907104, got 1.0
  $.SyntaxTokens[5].PartOfSpeech.Score: expected 0.9999991655349731, got 1.0
  $.SyntaxTokens[6].PartOfSpeech.Score: expected 0.9503836631774902, got 0.9852216839790344
  $.SyntaxTokens[8].PartOfSpeech.Score: expected 1.0, got 0.9998999834060669
  $.SyntaxTokens[10].PartOfSpeech.Score: expected 0.9999997615814208, got 1.0
  $.SyntaxTokens[11].PartOfSpeech.Score: expected 0.9517213702201844, got 0.9638554453849792
  $.SyntaxTokens[12].PartOfSpeech.Score: expected 0.9999922513961792, got 1.0
  $.SyntaxTokens[15].PartOfSpeech.Score: expected 0.9910897016525269, got 1.0
  $.SyntaxTokens[20].PartOfSpeech.Score: expected 1.0, got 0.9999799728393555
  $.SyntaxTokens[22].PartOfSpeech.Score: expected 0.9999997615814208, got 1.0
  $.SyntaxTokens[24].PartOfSpeech.Score: expected 0.9999996423721312, got 1.0
  $.SyntaxTokens[27].PartOfSpeech.Score: expected 0.9999945163726808, got 1.0
  $.SyntaxTokens[28].PartOfSpeech.Score: expected 0.9999885559082032, got 1.0
  $.SyntaxTokens[29].PartOfSpeech.Score: expected 0.9999998807907104, got 0.9998999834060669
  $.SyntaxTokens[30].PartOfSpeech.Score: expected 0.9999988079071044, got 1.0
  $.SyntaxTokens[33].PartOfSpeech.Score: expected 0.9870320558547974, got 1.0
  $.SyntaxTokens[34].PartOfSpeech.Score: diverges (reported in the cluster's key list)
  Only three distinct literals appear on the emulator side across all 20 diverging scores: 1.0, 0.9998999834060669 and 0.9999799728393555 (plus 0.9852216839790344 / 0.9638554453849792 on the two ambiguous tokens).

Cause 2 - step task#7, DetectTargetedSentiment (31 diverging members):
  $.Entities[2].Mentions[0].Text: expected 'john.smith@example.com', got 'Example Corp'
  $.Entities[2].Mentions[0].Type: expected 'OTHER', got 'ORGANIZATION'
  $.Entities[2].Mentions[0].BeginOffset: expected 196, got 150
  $.Entities[2].Mentions[0].EndOffset: expected 218, got 162
  $.Entities[2].Mentions[0].GroupScore: expected 0.9999629855155944, got 1.0
  $.Entities[2].Mentions[0].Score: expected 0.5750129818916321, got 0.8121159076690674
  $.Entities[3].Mentions[0].BeginOffset: expected 150, got 166
  $.Entities[3].Mentions[0].EndOffset: expected 162, got 173
  $.Entities[3].Mentions[0].Text / .Type / .Score / .MentionSentiment.SentimentScore.{Negative,Neutral,Positive}: all diverge (cluster key list)
  $.Entities[4].Mentions[0].BeginOffset / .EndOffset / .Text / .Type / .Score / .MentionSentiment.SentimentScore.{Negative,Neutral,Positive}: all diverge (cluster key list)
  $.Entities[0].Mentions[0].MentionSentiment.SentimentScore.Neutral: expected 0.9999979734420776, got 0.6902276277542114
  $.Entities[0].Mentions[0].MentionSentiment.SentimentScore.Positive: expected 9.999999974752427e-07, got 0.3097713887691498
  $.Entities[0].Mentions[0].Score: expected 0.9986129999160768, got 0.9956942796707153
  $.Entities[1].Mentions[0].MentionSentiment.SentimentScore.Negative: expected 0.0, got 9.999999974752427e-07 (floor literal)
  $.Entities[1].Mentions[0].MentionSentiment.SentimentScore.Neutral: expected 0.9999989867210388, got 0.9978637099266052
  $.Entities[1].Mentions[0].MentionSentiment.SentimentScore.Positive: expected 9.999999974752427e-07, got 0.002135267248377204
  $.Entities[1].Mentions[0].Score: expected 0.9999549984931946, got 0.8286985754966736
  $.Entities[2].Mentions[0].MentionSentiment.SentimentScore.Negative: expected 0.0, got 9.999999974752427e-07
  $.Entities[2].Mentions[0].MentionSentiment.SentimentScore.Neutral: expected 1.0, got 0.9978637099266052
  $.Entities[2].Mentions[0].MentionSentiment.SentimentScore.Positive: expected 0.0, got 0.002135267248377204
  The pair (Neutral 0.9978637099266052, Positive 0.002135267248377204, Negative 9.999999974752427e-07) is returned verbatim for several unrelated mentions, and 0.8286985754966736 / 0.9956942796707153 are the same literals DetectEntities and DetectPiiEntities return on this fixture — evidence of one shared constant pool rather than per-mention computation.

## Steps to Reproduce
1. Clone the repo, boot the emulator from the checkout and replay the wire corpus (replay_wire_corpus). Fixture comprehend-detect-suite-single-document replays with no blocked or errored step; replay reports stub_path empty and owned_by EMPTY for steps task#6 and task#7.
2. Cause 1: step task#6 - DetectSyntax with the fixture's Text and LanguageCode. Read $.SyntaxTokens[*].PartOfSpeech. Token 29 is tagged 'VERB' where AWS recorded 'AUX' and token 33 is tagged 'O' where AWS recorded 'NOUN'. Collect the Score values of all tokens: they fall onto the three literals 1.0, 0.9998999834060669 and 0.9999799728393555 instead of varying per token, so a per-token confidence filter cannot distinguish tokens.
3. Cause 2: step task#7 - DetectTargetedSentiment with the same Text and LanguageCode. For each $.Entities[i].Mentions[0], slice the request's Text with the returned BeginOffset/EndOffset and compare with the returned Text: for Entities[2] the emulator returns Text 'Example Corp' with Type 'ORGANIZATION' at offsets 150/162, while AWS recorded the e-mail mention 'john.smith@example.com' typed 'OTHER' at 196/218 - the mention is reported at another entity's span, and the same shift continues through Entities[3] and Entities[4].
4. Still in task#7, read $.Entities[*].Mentions[0].MentionSentiment.SentimentScore: the triple (Neutral 0.9978637099266052, Positive 0.002135267248377204, Negative 9.999999974752427e-07) recurs verbatim across unrelated mentions, Negative is the floor literal where AWS recorded exactly 0.0, and GroupScore is saturated to 1.0. Cross-check $.Entities[1].Mentions[0].Score (0.8286985754966736) against DetectEntities/DetectPiiEntities on the same fixture: the same literal, i.e. one shared pool.
5. Re-replay fixture comprehend-detect-suite-single-document after the fix: BOTH steps must match - task#6 (the representative, DetectSyntax) AND task#7 (DetectTargetedSentiment), not just the representative.
