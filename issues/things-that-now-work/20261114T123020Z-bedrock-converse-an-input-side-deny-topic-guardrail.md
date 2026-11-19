Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.bedrock-guardrail-blocked-input-never-reaches-the-model
Issue-Key: bedrock.bedrock-guardrail-blocked-input-never-reaches-the-model|run#11|AssertionError|assert blocked["stopReason"] == "guardrail_intervened", (
Issue-Commit: 89c70456cbc5c776860ae07cdd8fb6580c26b557

# bedrock Converse: an input-side DENY-topic guardrail never intervenes before generation — stopReason stays end_turn, blockedInputMessaging is never substituted, tokens are billed and trace.guardrail.inputAssessment carries no topicPolicy finding

> _Note: this issue was identified against the code base at commit `89c70456cbc5c776860ae07cdd8fb6580c26b557`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Converse

## Description
Cause 1 of 1: Converse does not evaluate a guardrail's topicPolicy on the INPUT side, so an on-topic prompt through a DENY-topic guardrail is passed straight to the (stubbed) generation stage and answered normally instead of being refused before the model runs.

AWS: Smithy (com.amazonaws.bedrockruntime) Converse — ConverseResponse.stopReason is the StopReason enum [end_turn, tool_use, max_tokens, stop_sequence, guardrail_intervened, content_filtered, malformed_model_output, malformed_tool_use, model_context_window_exceeded]; ConverseResponse.trace.guardrail is GuardrailTraceAssessment{modelOutput, inputAssessment: GuardrailAssessmentMap, outputAssessments, actionReason}; a GuardrailAssessment carries topicPolicy: GuardrailTopicPolicyAssessment{topics: required list of GuardrailTopic{name required, type required (GuardrailTopicType, value DENY), action required (GuardrailTopicPolicyAction, value BLOCKED), detected}}; ConverseResponse.usage is TokenUsage{inputTokens required (min 0), outputTokens required (min 0), totalTokens required (min 0)}. When guardrailConfig={guardrailIdentifier, guardrailVersion, trace:'enabled'} names a READY guardrail whose topicPolicyConfig declares a topic with type=DENY, and the user prompt is on that topic, AWS evaluates the guardrail BEFORE invoking the model and returns HTTP 200 with stopReason='guardrail_intervened', output.message.content holding the guardrail's configured blockedInputMessaging byte-for-byte (the fixed text here is entirely operator-supplied — it is whatever string the CreateGuardrail call set for blockedInputMessaging, with no AWS-authored wording), usage.inputTokens=0 and usage.outputTokens=0 because the model was never invoked, and trace.guardrail.inputAssessment[<guardrailId>] containing a topicPolicy.topics entry whose name is the configured topic name, type='DENY', action='BLOCKED', plus appliedGuardrailDetails attributing the block to the guardrailId/guardrailVersion actually passed in the request (the member names, the stopReason value, the enum values DENY/BLOCKED and the required-ness of usage are modeled; the zero-token billing and the blockedInputMessaging substitution are recorded behavior). An off-topic prompt on the SAME guardrail version returns stopReason='end_turn' with a real completion and no BLOCKED topic entry.

Emulator: the on-topic Converse call returns stopReason='end_turn' and output text 'No language model runs in this emulator. amazon.nova-lite-v1:0 generated no text for this request. The request carried 1 message(s) and 15 input token(s).' — no guardrail intervention at all. blockedInputMessaging is not substituted, input tokens are billed (the notice itself reports 15 input tokens, so usage.inputTokens is non-zero where AWS reports 0), and no topicPolicy topics entry with type=DENY/action=BLOCKED appears in trace.guardrail.inputAssessment for the guardrail that was passed. The control leg (off-topic prompt) is never reached because the blocked leg asserts first.

Why: a client that relies on an input-side guardrail to refuse a disallowed prompt before it reaches the model gets the opposite of the safety contract — the prompt is forwarded to generation, the turn completes as a normal answer, the branch keyed on stopReason=='guardrail_intervened' is dead code, the operator's blockedInputMessaging is never shown to the end user, and the billed token counts claim the model consumed a prompt AWS would have refused for free. Nothing in the response lets the caller tell 'the guardrail allowed this prompt' from 'the guardrail was never evaluated'.

Fix: the bedrock-runtime Converse handler's guardrail stage must run the referenced guardrail version's topicPolicy against the request messages BEFORE the generation stage: on a DENY-topic match it must short-circuit generation, set stopReason='guardrail_intervened', place the guardrail's blockedInputMessaging in output.message.content, zero usage.inputTokens/outputTokens/totalTokens, and populate trace.guardrail.inputAssessment[<guardrailId>].topicPolicy.topics with {name, type:'DENY', action:'BLOCKED'} plus appliedGuardrailDetails naming the requested guardrailId and guardrailVersion; on no match it must fall through to generation and emit an inputAssessment with no BLOCKED entry.

Related but not covering: open issue 20261111T094120Z-bedrock-bedrock-runtime-converse-an-output-side-guardrail.md describes the OUTPUT-side word policy after generation, and 20261111T094120Z-bedrock-applyguardrail-deny-topic-policy-is-never-evaluated.md describes the topic policy on the ApplyGuardrail operation; neither is grounded in this input-side Converse check, which the suite marked neither 'ALREADY FILED' nor 'OWNED by'. Open issues 20261023T232022Z-bedrock-runtime-synchronous-inference-guardrailconfig-and-toolconfig-silently.md and 20261028T194707Z-bedrock-runtime-generation-stage-serves-a-constant-emulator.md describe adjacent causes (guardrailConfig ignored, constant generation) from other recordings.

Traces: bedrock.bedrock-guardrail-blocked-input-never-reaches-the-model.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replay: 162/204 passed. Listed under '### bedrock', marked neither 'ALREADY FILED' nor 'OWNED by' (the only such failure in the whole report):

[mismatch] bedrock.bedrock-guardrail-blocked-input-never-reaches-the-model
  check_bedrock_bedrock_guardrail_blocked_input_never_reaches_the_model.py, line 130, in run
  assert blocked["stopReason"] == "guardrail_intervened"
  AssertionError: an on-topic prompt through a DENY-topic guardrail must stop with 'guardrail_intervened'; got 'end_turn' text='No language model runs in this emulator. amazon.nova-lite-v1:0 generated no text for this request. The request carried 1 message(s) and 15 input token(s).'

Setup performed by the check before the failing call: bedrock.create_guardrail(name=<unique>, blockedInputMessaging=<IN>, blockedOutputsMessaging=<OUT>, topicPolicyConfig={'topicsConfig': [{'name': <topic>, 'definition': <definition>, 'examples': [...], 'type': 'DENY'}]}, tags=[...]); poll get_guardrail until status=='READY'; create_guardrail_version(guardrailIdentifier=<id>) and poll that version until READY.

Failing call: bedrock-runtime.converse(modelId=<a Nova text model>, guardrailConfig={'guardrailIdentifier': <id>, 'guardrailVersion': <version>, 'trace': 'enabled'}, messages=[{'role':'user','content':[{'text': <a prompt squarely on the DENY topic>}]}], inferenceConfig={'maxTokens': 200, 'temperature': 0.0}).

Expected (AWS), HTTP 200: stopReason='guardrail_intervened'; the concatenated output.message.content text equals the configured blockedInputMessaging exactly; usage.inputTokens==0 and usage.outputTokens==0; trace.guardrail.inputAssessment[<guardrailId>].topicPolicy.topics contains an entry with name==<configured topic name>, type=='DENY', action=='BLOCKED'; that same inputAssessment's appliedGuardrailDetails reports guardrailId==<the id passed> and guardrailVersion==<the version passed>. Then, on the SAME guardrail version, an off-topic prompt returns stopReason='end_turn', usage.outputTokens>0, non-empty text that does not contain blockedInputMessaging, and no topics entry with action=='BLOCKED'.

Got (emulator), HTTP 200: stopReason='end_turn' and the constant notice text quoted above. No error code is involved — the divergence is the status-200 response body: the wrong stopReason enum value, the un-substituted content, the non-zero billed input tokens (15, per the notice), and the absent DENY/BLOCKED topicPolicy finding in trace.guardrail.inputAssessment. The variable parts of the expected body are taken from the request and from guardrail state (the guardrail id and version passed in guardrailConfig, the topic name and the blockedInputMessaging string set at CreateGuardrail); there is no AWS-authored fixed message text for this response.

Model citations: com.amazonaws.bedrockruntime#StopReason values include 'guardrail_intervened'; GuardrailTraceAssessment.inputAssessment: GuardrailAssessmentMap; GuardrailTopicPolicyAssessment.topics required; GuardrailTopic.name/type/action required (GuardrailTopicType includes DENY, GuardrailTopicPolicyAction includes BLOCKED); TokenUsage.inputTokens/outputTokens/totalTokens required with range min 0.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. bedrock.create_guardrail(name=<unique>, description=..., blockedInputMessaging=<IN>, blockedOutputsMessaging=<OUT>, topicPolicyConfig={'topicsConfig': [{'name': <topic name>, 'definition': <one-sentence definition>, 'examples': [<two on-topic examples>], 'type': 'DENY'}]}); poll get_guardrail(guardrailIdentifier=<id>) until status=='READY'.
3. bedrock.create_guardrail_version(guardrailIdentifier=<id>) -> <version>; poll get_guardrail(guardrailIdentifier=<id>, guardrailVersion=<version>) until status=='READY'.
4. bedrock-runtime.converse(modelId=<a Nova text model>, guardrailConfig={'guardrailIdentifier': <id>, 'guardrailVersion': <version>, 'trace': 'enabled'}, messages=[{'role':'user','content':[{'text': <a prompt squarely on the DENY topic>}]}], inferenceConfig={'maxTokens': 200, 'temperature': 0.0}).
5. Observe stopReason=='end_turn' with the emulator's constant notice text as the answer. AWS instead returns stopReason=='guardrail_intervened', output text == <IN> byte-for-byte, usage.inputTokens==0 and usage.outputTokens==0, and trace.guardrail.inputAssessment[<id>].topicPolicy.topics naming <topic name> with type=='DENY' and action=='BLOCKED', attributed via appliedGuardrailDetails to <id>/<version>.
6. Control, to confirm the guardrail is not simply blocking everything: repeat step 4 on the SAME guardrail version with an unrelated off-topic prompt; AWS answers it normally (stopReason=='end_turn', usage.outputTokens>0, no BLOCKED topic entry).
