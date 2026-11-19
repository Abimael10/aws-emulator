Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.concurrent-generated-template-limit cloudformation.generated-template-concurrency-limit-rejected
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.concurrent-generated-template-limit|run#17|AssertionError|assert message.startswith(
Issue-Key: cloudformation.generated-template-concurrency-limit-rejected|run#13|AssertionError|assert message.startswith(

# cloudformation: the generated-template concurrency-limit refusal is worded from the model, and AWS's sentence is in none of the evidence

> _Note: this cause was carried in
> `20261103T120000Z-cloudformation-generated-template-limit-wording-and-the-cdk-l2-deploys.md`
> as its cause 1, and in the two reports before that one. The change that fixed that report's
> cause 4 (the generated template's default `TemplateConfiguration`, whose unit now passes)
> implements NOTHING on this path, so it is carried here unchanged with the trace ids and
> replay keys that belong to it, and that report now lives under `issues/things-that-now-work/`.
> Its causes 2 and 3 (the CDK L2 deploys that must serve HTTP, and the in-use export guard on a
> CDK-driven producer update) need nothing: their units pass._

## Operation
CreateGeneratedTemplate

## Description

The generated-template concurrency-limit refusal message does not match AWS.
AWS: recorded behavior only — exceeding the in-flight generated-template limit is refused with
AWS's wording for that quota. Both checks assert `message.startswith(<AWS's sentence>)`, and
that sentence is not disclosed by the replay harness.
Emulator: `No more than 5 generated templates can be in an InProgress or Pending status at one
time`, which is the sentence the model's `ConcurrentResourcesLimitExceededException`
documentation states (`state::generated_template::concurrent_generations_exceeded`). The
refusal's HTTP status (429) and modelled code (`ConcurrentResourcesLimitExceeded`) agree with
the model and are asserted in `tests/all/generated_template_refusals.rs`; only the message
diverges.
Evidence that no reading of the model's text is AWS's — three forms have now been tried against
the recorded refusal and all three were reported "unexpected limit message":
1. the documented sentence as it stands (no terminal period, no envelope) — the form the
   emulator answers today;
2. that sentence WITH its terminal period, wrapped in this surface's SDK envelope
   `(Service: CfnTemplateGenerationServiceLambda; Status Code: 400; Error Code: <exception>;
   Request ID: <id>; Proxy: null)`, the form this surface's two RECORDED refusals carry
   (`state::generated_template::with_component_envelope`);
3. the WHOLE documented paragraph, both of its sentences ("…at one time. This error is also
   returned if a generated template that is in an InProgress or Pending status is attempted to
   be updated or deleted.").
Form 2 rules out every sentence that is a prefix of the documented one, and form 3 rules out
the documented paragraph itself, so AWS's sentence is neither the model's text nor any part or
extension of it that the model states. A further clue, if the refusal carries the envelope that
the other two refusals of this surface do: `ErrorMessage` is `@length(max: 255)` and the
envelope alone is about 180 characters, so AWS's sentence would then have to be around 75
characters or shorter — far shorter than the documented one (89).
Why: clients that branch on the limit message mis-handle the refusal.
Fix: `state::generated_template::concurrent_generations_exceeded`, from a recording of the
refusal — the sentence has to come from the corpus, since nothing in the model, the AWS
documentation or the replay output states it, and a sentence of this crate's own would be an
invented message. `tests/all/generated_template_limit_wording.rs` is the suite that records why:
it pins what the MODEL fixes about this refusal (HTTP 429, the awsQuery code
`ConcurrentResourcesLimitExceeded`, and a `Message` inside the modelled
`@length(min: 1, max: 255)` bound — which is itself what rules the enveloped form out) and
deliberately asserts no wording, so the change that words this refusal from a recording is the one
that adds the wording assertion there.
Traces: cloudformation.concurrent-generated-template-limit,
cloudformation.generated-template-concurrency-limit-rejected.

## Evidence
"unexpected limit message 'No more than 5 generated templates can be in an InProgress or
Pending status at one time'" (and "unexpected failure message '…'" for the second check), plus
the same two checks against forms 2 and 3 above.
Ids, names, uuids, values and timestamps are per-run fillers.

## Steps to Reproduce
1) Start more generated templates than the in-flight cap
   (`state::generated_template::MAX_CONCURRENT_GENERATIONS`) allows and read the refusal's
   `Message`. 2) Compare it with the sentence AWS answers for
   `ConcurrentResourcesLimitExceededException`, which has to be read off a recording of the
   real service.
