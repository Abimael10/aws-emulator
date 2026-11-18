Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: transcribe.native-speech-to-text-round-trip-via-polly
Issue-Shape-Id: com.amazonaws.transcribe#Transcribe
Issue-Key: transcribe.native-speech-to-text-round-trip-via-polly|run#20|AssertionError|assert matched >= len(original_words) * 0.6, (
Issue-Commit: 470dfbec0fe477d8e49c9ee1cb489604c7adeb06

# A COMPLETED transcription job serves a transcript document with an empty transcript, so the end-to-end speech-to-text workflow returns no words

> _Note: this issue was identified against the code base at commit `470dfbec0fe477d8e49c9ee1cb489604c7adeb06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartTranscriptionJob

## Description
1 cause.

Cause 1 of 1: a transcription job started over real synthesized audio reaches TranscriptionJobStatus COMPLETED, and the document served at the Transcript.TranscriptFileUri it reports contains an EMPTY transcript string, so the canonical workflow (synthesize/upload audio -> StartTranscriptionJob -> poll GetTranscriptionJob to COMPLETED -> fetch TranscriptFileUri) completes while yielding none of the spoken words. Failing check: transcribe.native-speech-to-text-round-trip-via-polly.

AWS: Smithy models only the LOCATION, not the document: Transcript.TranscriptFileUri (target Uri, @length 1..2000, @pattern ^(s3://|http(s*)://).+$) is documented as "The Amazon S3 location of your transcript. You can use this URI to access or download your transcript." The document's schema is recorded behavior only: AWS writes a JSON envelope (jobName, accountId, status "COMPLETED") plus `results` holding (a) transcripts[0].transcript — the full spoken text of the media the job processed, as one string, (b) items[] — one entry per token in order, each with id, type ("pronunciation" or "punctuation"), alternatives[{confidence, content}] and, for pronunciations, start_time/end_time as second-valued strings, and (c) audio_segments[] — the segment breakdown over those items. The requirement is that the transcript text is the speech contained in the media the job processed and that items/audio_segments are present and consistent with it; the particular words, confidences, times, job name and account id of any single run are values filling that template, never part of it.

Emulator: the job does reach COMPLETED and the reported URI is served (200), but $.results.transcripts[0].transcript is the empty string '' and $.results.items and $.results.audio_segments are absent entirely. The check asserts the transcript resembles the audio it transcribed and fails with: "AssertionError: transcript '' does not resemble the synthesized speech 'The quick brown fox jumps over the lazy dog' (matched 0/8 words)" — 0 of 8 words matched, where at least 60% is required. This is deliberate in the current design rather than an accident: services/service-com-amazonaws-transcribe-transcribe/src/state.rs, module header lines 41-58, states the stored output document holds "the envelope the service itself owns (the job, the account, the status) with an EMPTY transcript and no caption, because no engine read the audio".

Why: a client that performs the single workflow this service exists for gets a successful, terminal COMPLETED job whose transcript carries no speech, so every downstream consumer — keyword extraction, captioning, search indexing, diarization — silently receives nothing instead of failing loudly. A client that walks results.items or results.audio_segments (the normal way to read word timings, confidences or segments) indexes members that are ABSENT rather than empty and crashes with a key/None error; an AWS-shaped empty result would still carry both arrays. There is no error, status or code a caller can key on to detect that no transcription happened.

Fix: the transcript-document builder on the transcribe side — the `transcript_document` / `store_job_outputs` path reached from the transcription-job lifecycle in services/service-com-amazonaws-transcribe-transcribe/src/state.rs — must render the words of the media the job pointed at into results.transcripts[0].transcript and emit results.items and results.audio_segments consistent with that text. GetObject's read path is not at fault: it correctly serves the bytes that were stored. The same applies to the redacted transcript and subtitle documents built alongside it (`subtitle_document`).

Related but NOT covering this: open issue 20261112T121939Z-s3-serves-the-transcribe-transcript-document-with-an.md records the same empty document from the WIRE corpus, filed under service s3 / operation GetObject against fixture transcribe-polly-speech-transcribed-to-s3-transcript#task#6; the replay reports this behavioral check as owned by no open issue, and that issue's service token and operation do not name the transcribe write path this check exercises. Open issue 20261111T185526Z-s3-getobject-404s-for-the-transcribe-transcript-document.md records the earlier state in which no object was stored at all (404) and does not cover a 200 serving an empty document.

Traces: transcribe.native-speech-to-text-round-trip-via-polly.

## Evidence
Check transcribe.native-speech-to-text-round-trip-via-polly, verdict mismatch.

Expected (AWS): the document fetched from Transcript.TranscriptFileUri of the COMPLETED job holds results.transcripts[0].transcript containing the speech in the media (at least 60% of the spoken words present), plus results.items[] (one entry per token: id, type, alternatives[{confidence, content}], start_time/end_time for pronunciations) and results.audio_segments[].

Actual (emulator): the job reaches COMPLETED and the URI is served, but the document's results.transcripts[0].transcript is '' and results.items / results.audio_segments are missing. Harness output:

  File "check_transcribe_native_speech_to_text_round_trip_via_polly.py", line 62, in run
    assert matched >= len(original_words) * 0.6, (
  AssertionError: transcript '' does not resemble the synthesized speech 'The quick brown fox jumps over the lazy dog' (matched 0/8 words)

The sentence, word count and the generated bucket/job names are this run's values; the requirement is that the transcript is the speech in the submitted media with the items/audio_segments arrays present and consistent with it. Corroborating source: services/service-com-amazonaws-transcribe-transcribe/src/state.rs lines 41-58 declare the stored document is written "with an EMPTY transcript and no caption, because no engine read the audio".

## Steps to Reproduce
1. polly.synthesize_speech(Text="The quick brown fox jumps over the lazy dog", OutputFormat="mp3", VoiceId="Joanna") and read the AudioStream.
2. s3.create_bucket(...) and s3.put_object(Bucket=<bucket>, Key="sim2real-speech.mp3", Body=<audio>, ContentType="audio/mpeg").
3. transcribe.start_transcription_job(TranscriptionJobName=<name>, LanguageCode="en-US", MediaFormat="mp3", Media={"MediaFileUri": "s3://<bucket>/sim2real-speech.mp3"}).
4. Poll transcribe.get_transcription_job(TranscriptionJobName=<name>) until TranscriptionJobStatus is COMPLETED (it is).
5. HTTP GET the TranscriptionJob.Transcript.TranscriptFileUri and parse the JSON.
6. payload["results"]["transcripts"][0]["transcript"] is '' and payload["results"] has no "items" or "audio_segments" keys, where AWS serves a transcript of the submitted audio together with both arrays.
Replay: transcribe.native-speech-to-text-round-trip-via-polly.
