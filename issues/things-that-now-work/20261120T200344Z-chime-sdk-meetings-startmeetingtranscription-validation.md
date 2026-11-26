Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 15d412edf6dc0def4a35d95df3d7a89a9f8b446b

# StartMeetingTranscription accepts a PreferredLanguage that is not in LanguageOptions, and LanguageOptions naming two dialects of one language

_Filed: 2026-11-20T20:03:44Z_
_Operation: StartMeetingTranscription | Class: validation_

> _Note: this issue was identified against the code base at commit `15d412edf6dc0def4a35d95df3d7a89a9f8b446b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`transcribe_settings_problem` in `start_meeting_transcription.rs` implements the cross-member rules the model states on `EngineTranscribeSettings` (language-code vs. IdentifyLanguage, content identification vs. redaction, PiiEntityTypes, LanguageOptions-requires-IdentifyLanguage, PreferredLanguage-requires-both, the singular/plural vocabulary split). Two documented rules about the CONTENT of those members are left out, so a configuration the model forbids is accepted and persisted on the meeting as the running transcription:

1. `PreferredLanguage` is only checked for PRESENCE of `IdentifyLanguage` and `LanguageOptions`; it is never checked for being one of the language codes `LanguageOptions` lists. `EngineTranscribeSettings$PreferredLanguage` documents it as a choice out of that list, and the handler's own comment says so — then does not check it. So `LanguageOptions: "en-US,fr-FR"` with `PreferredLanguage: "de-DE"` is accepted.
2. `LanguageOptions` may name two dialects of one language. `EngineTranscribeSettings$LanguageOptions` states the prohibition and even gives the counter-example, so `LanguageOptions: "en-US,en-AU"` must be refused; the handler only checks that `IdentifyLanguage` is set, so it is accepted.

Neither rule is reachable for the boundary validator: `TranscribeLanguageOptions` carries only `@length(1..200)` and `@pattern(^[a-zA-Z-,]+$)`, and `PreferredLanguage` is a plain `TranscribeLanguageCode` enum — so the handler doc's claim that the per-member constraints are already handled does not hold for these two. Both symptoms belong to the same check and are reported here as one validation gap.

## Evidence

Smithy, `EngineTranscribeSettings$PreferredLanguage` (`smithy_query(query="shape",shape="EngineTranscribeSettings")`): "Specify a preferred language from the subset of languages codes you specified in <code>LanguageOptions</code>." / "You can only use this parameter if you include <code>IdentifyLanguage</code> and <code>LanguageOptions</code>."

Smithy, `EngineTranscribeSettings$LanguageOptions`: "You can only include one language dialect per language. For example, you cannot include <code>en-US</code> and <code>en-AU</code>."

Handler, `services/service-com-amazonaws-chimesdkmeetings-chimemeetingssdkservice/src/ops/start_meeting_transcription.rs:331-341` — the comment states the rule the code under it does not enforce:
```
    // `PreferredLanguage`: "You can only use this parameter if you include IdentifyLanguage
    // and LanguageOptions" — it is a choice from the languages the request listed.
    if settings.preferred_language.is_some()
        && (!identify_language || settings.language_options.is_none())
```
Only `is_some()`/`is_none()` are consulted; the listed codes are never parsed.

Same file, `:321-329` — the only `LanguageOptions` rule implemented:
```
    // `LanguageOptions`: "If you include LanguageOptions, you must also include
    // IdentifyLanguage."
    if settings.language_options.is_some() && !identify_language {
```

Same file, `:251-256` (the function doc that claims the rest is covered): "Every rule here is a sentence the model states about the member it constrains … Per-member constraints — the required members of the medical settings, the enums, the lengths and the patterns — are refused by the boundary validator before this handler runs, so only the cross-member rules are left." `TranscribeLanguageOptions` declares only `length {min:1,max:200}` and `pattern ^[a-zA-Z-,]+$`, so neither rule above is one the validator can refuse.

`grep -n 'preferred_language|language_options' src/` matches only `start_meeting_transcription.rs:323,333-334` and the type declaration `src/ops/types/start_meeting_transcription_types.rs:43-44`: no membership or dialect check exists anywhere in the crate.

Test coverage: `tests/all/meeting_transcription.rs:320-321` (accepted row) uses `LanguageOptions: "en-US,fr-FR"` with `PreferredLanguage: "fr-FR"` — a member of the list and distinct languages — and `tests/all/meeting_transcription.rs:459-472` pins the two PRESENCE refusals. No existing test accepts a non-member preferred language or a repeated dialect, so no pinned behaviour contradicts this.

## Steps to Reproduce

1. CreateMeeting(ClientRequestToken='tok-1', MediaRegion='us-east-1', ExternalMeetingId='ext-1') and keep the MeetingId.
2. StartMeetingTranscription(MeetingId=<id>, TranscriptionConfiguration={EngineTranscribeSettings:{IdentifyLanguage:true, LanguageOptions:'en-US,fr-FR', PreferredLanguage:'de-DE'}}) — the model says PreferredLanguage must come from the subset listed in LanguageOptions; actual: HTTP 200 and the configuration is stored as the meeting's running transcription.
3. StartMeetingTranscription(MeetingId=<id>, TranscriptionConfiguration={EngineTranscribeSettings:{IdentifyLanguage:true, LanguageOptions:'en-US,en-AU'}}) — the model says you cannot include en-US and en-AU; actual: HTTP 200 and the configuration replaces the one in force.
4. Contrast with StartMeetingTranscription(MeetingId=<id>, TranscriptionConfiguration={EngineTranscribeSettings:{LanguageOptions:'en-US,fr-FR', LanguageCode:'en-US'}}), which IS refused with BadRequestException by the sibling rule at start_meeting_transcription.rs:323.
