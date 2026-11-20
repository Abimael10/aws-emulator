Issue-Source: trace-sweep-divergence
Issue-Trace: comprehend-batch-detect-suite
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: comprehend-batch-detect-suite
Issue-Shape-Id: com.amazonaws.comprehend#Comprehend_20171127
Issue-Key: comprehend|BatchDetectDominantLanguage|2xx->2xx|$.ResultList[*].Languages[*].Score
Issue-Key: comprehend|BatchDetectKeyPhrases|2xx->2xx|$.ResultList[*].KeyPhrases[*].Score
Issue-Key: comprehend|BatchDetectSentiment|2xx->2xx|$.ResultList[*].SentimentScore.Mixed
Issue-Key: comprehend|BatchDetectSentiment|2xx->2xx|$.ResultList[*].SentimentScore.Negative
Issue-Key: comprehend|BatchDetectSentiment|2xx->2xx|$.ResultList[*].SentimentScore.Neutral
Issue-Key: comprehend|BatchDetectSentiment|2xx->2xx|$.ResultList[*].SentimentScore.Positive
Issue-Key: comprehend|BatchDetectSyntax|2xx->2xx|$.ResultList[*].SyntaxTokens[*].PartOfSpeech.Score
Issue-Key: comprehend|BatchDetectSyntax|2xx->2xx|$.ResultList[*].SyntaxTokens[*].PartOfSpeech.Tag
Issue-Commit: 2b7ef28fb79df90eb190b034f8bd9899c7a59aff

# Comprehend BatchDetect* inference emits per-shape constant confidences and mis-tags syntax tokens (4 causes)

> _Note: this issue was identified against the code base at commit `2b7ef28fb79df90eb190b034f8bd9899c7a59aff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchDetectDominantLanguage

## Description
FOUR net-new root causes in the comprehend crate's BATCH synchronous inference layer (the BatchDetect* handlers exercised by fixture comprehend-batch-detect-suite). All four steps answered HTTP 200 exactly as recorded and the response SHAPE is right (ResultList/ErrorList present, one result per TextList element, Index present), so the divergence is entirely in the analysed values. One fault recurs through all four: the per-result confidences are drawn from a small fixed ladder of literals belonging to the response shape rather than computed from each document, so the same literal recurs across unrelated documents and across operations, the ordering of documents by confidence is not preserved, and on the syntax path the part-of-speech TAG itself is wrong. A fixer must close all FOUR causes.

PRIOR ART (named, not covering): 20261120T175104Z-comprehend-batchdetectentities-emits-uncalibrated-per-entity-confidence-scores--r2.md owns this fixture's task#2 (BatchDetectEntities) and replay reports that step as owned, so it is NOT filed here; 20261030T173304Z-comprehend-batch-and-targeted-inference-re-segment-text.md is RESOLVED and therefore covers nothing; 20261113T194011Z-comprehend-single-document-detect-inference-synthesizes-ladder-constant.md covers the SINGLE-DOCUMENT Detect* fixture, not the batch fan-out. Replay reports owned_by empty for all four steps below.

Cause 1 of 4: BatchDetectDominantLanguage returns confidences that do not track each document's language evidence, and inverts the ordering between documents. Fixture comprehend-batch-detect-suite, step task#1.
  AWS: Smithy: BatchDetectDominantLanguage(TextList required, CustomerInputStringList @length min 1) -> ResultList: ListOfDetectDominantLanguageResult, each carrying Languages of DominantLanguage{LanguageCode, Score}; DominantLanguage.Score is documented 'The level of confidence that Amazon Comprehend has in the accuracy of the detection.' Recorded behavior: each document in the batch gets its OWN confidence reflecting how much evidence that document gives for the detected language, so a short or mixed-language document scores visibly lower than an unambiguous one.
  Emulator: the three results come back with values that neither match nor preserve the relative order of the recorded ones: $.ResultList[0].Languages[0].Score 0.9960810542106628 (recorded 0.9994072318077089), [1] 0.9980440735816956 (recorded 0.973076343536377), [2] 0.9947519898414612 (recorded 0.9928982257843018). The document AWS is LEAST confident about (0.973) is the one the emulator scores HIGHEST, i.e. the value is a shape-level constant family, not a per-document score.
  Why: a client batches documents and uses Languages[0].Score to decide which ones to accept for routing/translation and which to flag for review, often ranking the batch by confidence. With confidences that do not derive from the text, the low-confidence branch is unreachable and the ranking across the batch is wrong, so the emulator and AWS select different documents from the same request.
  Fix: the BatchDetectDominantLanguage handler's language-detection scorer — derive each result's Score from that element's own detection evidence instead of a per-shape constant.
  Traces: comprehend-batch-detect-suite#task#1.

Cause 2 of 4: BatchDetectKeyPhrases gives different key phrases the same confidence literal and cannot distinguish a weak phrase from a strong one. Fixture comprehend-batch-detect-suite, step task#3.
  AWS: Smithy: BatchDetectKeyPhrases(TextList required, LanguageCode required) -> ResultList of DetectKeyPhrasesResult carrying KeyPhrases of KeyPhrase{Score, Text ('The text of a key noun phrase'), BeginOffset, EndOffset}; KeyPhrase.Score is 'The level of confidence that Amazon Comprehend has in the accuracy of the detection.' Recorded behavior: each key phrase carries its own confidence, mostly saturated (~0.9999) for clear noun phrases but visibly lower for a marginal one, and distinct phrase by phrase.
  Emulator: within one result two different phrases get the IDENTICAL literal — $.ResultList[0].KeyPhrases[0].Score and [1].Score both 0.9999399781227112 where AWS records 0.999966561794281 and 0.9999895691871644 — and the one phrase AWS reports as marginal is returned as saturated: $.ResultList[2].KeyPhrases[1].Score expected 0.9115143418312072, got 0.9999650120735168 (also [2].KeyPhrases[0] expected 0.9999842643737792 got 0.9998400211334229, [2].KeyPhrases[2] expected 0.999948501586914 got 0.9999750256538391, [1].KeyPhrases[0] expected 0.9998527765274048 got 0.9999066591262817).
  Why: clients prune key-phrase output by Score to build tags, facets or search terms. Collapsing every phrase onto the same literal destroys the ranking, and promoting a 0.91 phrase to 0.99996 keeps a phrase a client would have dropped — so indexes and tag sets built against the emulator do not match the ones built against AWS.
  Fix: the BatchDetectKeyPhrases handler's key-phrase scorer — one confidence per extracted phrase, computed from that phrase's evidence in its own document, rather than a constant per shape.
  Traces: comprehend-batch-detect-suite#task#3.

Cause 3 of 4: BatchDetectSentiment dilutes the dominant sentiment and fills every non-dominant member with one shared floor constant. Fixture comprehend-batch-detect-suite, step task#4.
  AWS: Smithy: BatchDetectSentiment(TextList required, LanguageCode required) -> ResultList of DetectSentimentResult{Sentiment (SentimentType), SentimentScore{Positive, Negative, Neutral, Mixed}}; each SentimentScore member is documented 'The level of confidence that Amazon Comprehend has in the accuracy of its detection of the <SENTIMENT> sentiment.' Recorded behavior: for each document the four members form one distribution that concentrates on the reported Sentiment, with the remaining members at small and MUTUALLY DISTINCT magnitudes (order 1e-05 to 1e-02 for a confident document).
  Emulator: every non-dominant member across the whole batch is the SAME literal 9.999960184359225e-07 — $.ResultList[1].SentimentScore.Mixed and .Positive, $.ResultList[2].SentimentScore.Mixed, .Negative and .Positive all carry it, against recorded 5.03755800309591e-05 / 7.088874554028735e-05 / 2.23561091843294e-05 / 0.0008524285512976348 / 0.0027404760476201773 — and the dominant member is diluted with mass leaking elsewhere: $.ResultList[0].SentimentScore.Mixed expected 0.893014132976532 got 0.734043538570404 with Neutral expected 0.0034317385870963335 got 0.10584080219268799 and Positive expected 0.1019948422908783 got 0.16011464595794678; $.ResultList[1].SentimentScore.Negative expected 0.9998328685760498 got 0.9781920313835144 with Neutral expected 4.58155082014855e-05 got 0.02180597186088562.
  Why: clients threshold sentiment ('act only above 0.95', 'treat a second sentiment above 0.1 as ambiguous'). A dominant member pulled from 0.99983 to 0.978 or from 0.893 to 0.734 with 0.106 leaking into a second sentiment turns a decisive verdict into a borderline one, and because the residual members are one shared constant no relative ordering between documents in the batch survives.
  Fix: the BatchDetectSentiment handler's sentiment scorer — one normalised distribution per batch element concentrated on that element's reported Sentiment, instead of filling members from a fixed floor/ladder table.
  Traces: comprehend-batch-detect-suite#task#4.

Cause 4 of 4: BatchDetectSyntax assigns the wrong PartOfSpeech.Tag to tokens and scores every token off a recurring ladder. Fixture comprehend-batch-detect-suite, step task#5.
  AWS: Smithy: BatchDetectSyntax(TextList required, LanguageCode: SyntaxLanguageCode required) -> ResultList of DetectSyntaxResult carrying SyntaxTokens of SyntaxToken{TokenId, Text, BeginOffset, EndOffset, PartOfSpeech}; PartOfSpeechTag{Tag: PartOfSpeechTagType (ADJ|ADP|ADV|AUX|CONJ|CCONJ|DET|INTJ|NOUN|NUM|O|PART|PRON|PROPN|PUNCT|SCONJ|SYM|VERB), Score 'The confidence that Amazon Comprehend has that the part of speech was correctly identified'}. Recorded behavior: each token's Tag names the part of speech that token actually plays in its sentence, and the per-token Score is at or extremely near 1.0 (frequently exactly 1.0) and varies token by token.
  Emulator: tags are wrong for tokens whose role depends on their syntactic context — $.ResultList[1].SyntaxTokens[1].PartOfSpeech.Tag expected 'VERB', got 'AUX'; $.ResultList[1].SyntaxTokens[3].PartOfSpeech.Tag expected 'ADJ', got 'VERB' — and every PartOfSpeech.Score across all three results is drawn from the same handful of literals (0.9999899864196777, 0.9999499917030334, 0.9999949932098389, 0.9999666810035706, 0.9998900294303894, 0.999992311000824) where AWS records 1.0 for most tokens, with the same literal recurring on unrelated tokens of unrelated documents.
  Why: a client walks SyntaxTokens and branches on Tag to extract predicates, strip function words or build dependency features; tagging a main verb AUX or an adjective VERB silently changes what is extracted from the sentence. The recurring Score literals additionally destroy any confidence-based filtering, since tokens AWS reports at full confidence and tokens it reports as uncertain become indistinguishable.
  Fix: the BatchDetectSyntax handler's part-of-speech tagger (tag each token from its role in its own sentence, over PartOfSpeechTagType) and its per-token confidence computation.
  Traces: comprehend-batch-detect-suite#task#5.

In all four causes the recorded numbers and tags are this recording's values for ITS OWN TextList and are not themselves the requirement. The requirement is the convention: one confidence per detection derived from that detection in its own document, a normalised SentimentScore distribution concentrated on the reported Sentiment with distinct residual members, relative ordering across batch elements preserved, and a Tag chosen by the token's syntactic role — never a literal that recurs across detections, documents and operations.

## Evidence
All four steps: expected HTTP 200, got HTTP 200. Status and response shape match; the divergence is entirely in the body values. Fixture comprehend-batch-detect-suite throughout. Replay verdict 'mismatch', stub_path empty, owned_by empty for each.

Cause 1 — task#1, BatchDetectDominantLanguage:
  $.ResultList[0].Languages[0].Score: expected 0.9994072318077089, got 0.9960810542106628
  $.ResultList[1].Languages[0].Score: expected 0.973076343536377, got 0.9980440735816956  (AWS's least-confident document scored highest by the emulator)
  $.ResultList[2].Languages[0].Score: expected 0.9928982257843018, got 0.9947519898414612

Cause 2 — task#3, BatchDetectKeyPhrases:
  $.ResultList[0].KeyPhrases[0].Score: expected 0.999966561794281, got 0.9999399781227112
  $.ResultList[0].KeyPhrases[1].Score: expected 0.9999895691871644, got the SAME 0.9999399781227112
  $.ResultList[1].KeyPhrases[0].Score: expected 0.9998527765274048, got 0.9999066591262817
  $.ResultList[2].KeyPhrases[0].Score: expected 0.9999842643737792, got 0.9998400211334229
  $.ResultList[2].KeyPhrases[1].Score: expected 0.9115143418312072, got 0.9999650120735168  (marginal phrase promoted to saturated)
  $.ResultList[2].KeyPhrases[2].Score: expected 0.999948501586914, got 0.9999750256538391

Cause 3 — task#4, BatchDetectSentiment:
  $.ResultList[0].SentimentScore.Mixed: expected 0.893014132976532, got 0.734043538570404
  $.ResultList[0].SentimentScore.Neutral: expected 0.0034317385870963335, got 0.10584080219268799
  $.ResultList[0].SentimentScore.Positive: expected 0.1019948422908783, got 0.16011464595794678
  $.ResultList[0].SentimentScore.Negative: expected 0.0015593204880133271, got 9.999960184359225e-07
  $.ResultList[1].SentimentScore.Negative: expected 0.9998328685760498, got 0.9781920313835144
  $.ResultList[1].SentimentScore.Neutral: expected 4.58155082014855e-05, got 0.02180597186088562
  $.ResultList[1].SentimentScore.Mixed: expected 5.03755800309591e-05, got 9.999960184359225e-07
  $.ResultList[1].SentimentScore.Positive: expected 7.088874554028735e-05, got 9.999960184359225e-07
  $.ResultList[2].SentimentScore.Neutral: expected 0.9963847398757936, got 0.9999970197677612
  $.ResultList[2].SentimentScore.Mixed: expected 2.23561091843294e-05, got 9.999960184359225e-07
  $.ResultList[2].SentimentScore.Negative: expected 0.0008524285512976348, got 9.999960184359225e-07
  $.ResultList[2].SentimentScore.Positive: expected 0.0027404760476201773, got 9.999960184359225e-07
  (the single literal 9.999960184359225e-07 is returned for FIVE distinct recorded values spanning 1e-05 to 2.7e-03)

Cause 4 — task#5, BatchDetectSyntax:
  $.ResultList[1].SyntaxTokens[1].PartOfSpeech.Tag: expected 'VERB', got 'AUX'
  $.ResultList[1].SyntaxTokens[3].PartOfSpeech.Tag: expected 'ADJ', got 'VERB'
  $.ResultList[0].SyntaxTokens[0..3,5,6,7,8].PartOfSpeech.Score: expected 1.0, got 0.9999899864196777 / 0.9999499917030334 / 0.9999949932098389 / 0.9999666810035706 / 0.9999899864196777 / 0.9999499917030334 / 0.9998900294303894 / 0.999992311000824
  $.ResultList[0].SyntaxTokens[4].PartOfSpeech.Score: expected 0.9999988079071044, got 0.9999899864196777
  $.ResultList[1].SyntaxTokens[0..7] and $.ResultList[2].SyntaxTokens[0..8].PartOfSpeech.Score: same handful of literals recurring across unrelated tokens where 1.0 is recorded

Cross-cause evidence of a shared constant source: 0.9999899864196777 and 0.9999499917030334 recur across tokens of different documents in cause 4; 9.999960184359225e-07 is the single value returned for every non-dominant SentimentScore member in cause 3; 0.9999399781227112 is returned for two different key phrases in cause 2.

## Steps to Reproduce
1. Clone the repo, boot the emulator from the checkout and replay the wire corpus (replay_wire_corpus). Fixture comprehend-batch-detect-suite replays with no blocked or errored step; replay reports stub_path and owned_by empty for the four steps below (task#2/BatchDetectEntities IS owned and is excluded).
2. Cause 1: step task#1 — BatchDetectDominantLanguage with the fixture's three-element TextList. Read $.ResultList[*].Languages[0].Score: the three values do not match the recorded ones and their ORDER differs, i.e. the document AWS records at 0.973 is scored highest.
3. Cause 2: step task#3 — BatchDetectKeyPhrases (same TextList, LanguageCode 'en'). Read $.ResultList[0].KeyPhrases[*].Score: phrase 0 and phrase 1 carry the identical literal 0.9999399781227112, and $.ResultList[2].KeyPhrases[1].Score comes back saturated (0.99996) where 0.9115 is recorded.
4. Cause 3: step task#4 — BatchDetectSentiment (same TextList, LanguageCode 'en'). Read $.ResultList[*].SentimentScore: every non-dominant member is the single literal 9.999960184359225e-07, and the dominant member is diluted (result 0 Mixed 0.734 vs recorded 0.893, result 1 Negative 0.978 vs recorded 0.99983).
5. Cause 4: step task#5 — BatchDetectSyntax (same TextList, LanguageCode 'en'). Read $.ResultList[1].SyntaxTokens[1] and [3]: PartOfSpeech.Tag is 'AUX' where 'VERB' is recorded and 'VERB' where 'ADJ' is recorded. Then read every PartOfSpeech.Score: the same small set of literals recurs across all three results where AWS records 1.0.
6. Re-replay fixture comprehend-batch-detect-suite after the fix: all four steps must match, not just the representative task#1.
