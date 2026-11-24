Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-catalog-list-foundation-models
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-catalog-list-foundation-models bedrock-catalog-list-inference-profiles
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|ListInferenceProfiles|2xx->2xx|$.inferenceProfileSummaries[*]
Issue-Key: bedrock|ListFoundationModels|2xx->2xx|$.modelSummaries[modelId="amazon.nova-2-5-sonic"].converse
Issue-Related: 20261113T142522Z-bedrock-catalog-discovery-lists-diverge-from-the-recorded.md
Issue-Related: 20261108T085833Z-bedrock-listfoundationmodels-catalog-diverges-from-aws-for-newly.md

# bedrock discovery lists: the per-model capability values and the SYSTEM_DEFINED profile set still diverge (2 causes)

> _Successor of 20261113T142522Z-bedrock-catalog-discovery-lists-diverge-from-the-recorded.md,
> which fixed the CATALOG MEMBERSHIP half of that report (the eleven `modelId`s the recorded
> catalog no longer offers are offered no longer, and a published cross-region profile now
> outlives the offering of its model). What that change did not reach is filed here so the
> remaining divergence stays owned._

## Operation
ListInferenceProfiles

## Description

Cause 1 of 2 — ListInferenceProfiles serves 14 of the 93 SYSTEM_DEFINED profiles the recording
carries, in its own order, and emits no `toolboxArn` (fixture
bedrock-catalog-list-inference-profiles, step task#1).
AWS: `GET /inference-profiles`, 200, `inferenceProfileSummaries` +`nextToken`; paginated
{inputToken nextToken, outputToken nextToken, pageSize maxResults, items
inferenceProfileSummaries}; `@readonly`
(`smithy_query(query="operation",operation="ListInferenceProfiles")`). Which profiles the
account-level catalog holds, the ORDER it reports them in, and the `toolboxArn` member (null
for a system-defined profile) are recorded behaviour only — `InferenceProfileSummary` in the
checked-in model declares neither the set nor that member.
Emulator: 200 with the 14 profiles `crate::state::CROSS_REGION_MODELS` publishes in the
addressed geography, ordered by `inferenceProfileName`; the replay pairs 10 of them with
recorded entries and reports the remaining 83 recorded summaries as unmatched
($.inferenceProfileSummaries[10]..[92]). The four Meta Llama 3.2 profiles the predecessor
added do not pair, which leaves their published instant as the one member still to establish.
Why: a client lists the profiles and then passes an `inferenceProfileId`/ARN as the `modelId`
of Converse or InvokeModel, so a profile the listing does not carry cannot be routed to at
all, and a client that page-slices (`maxResults`/`nextToken`) or indexes the result picks a
profile that routes to models it did not ask for.
Fix: the cross-region profile seed behind ListInferenceProfiles
(`services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/src/state/foundation_models/published_routing.rs`):
the published set, the order a listing reports it in, the per-model routed regions and the
instant each profile came into being, plus the `toolboxArn` member on the summary projection.
Traces: bedrock-catalog-list-inference-profiles#task#1 (expected 200 / actual 200, body
mismatch).

Cause 2 of 2 — ListFoundationModels: the per-model CAPABILITY values of a summary diverge
(fixture bedrock-catalog-list-foundation-models, step task#1).
AWS: `GET /foundation-models`, 200, `modelSummaries: FoundationModelSummaryList`; the model
declares only modelArn, modelId, modelName, providerName, inputModalities, outputModalities,
responseStreamingSupported, customizationsSupported, inferenceTypesSupported and
modelLifecycle, so `converse` (maxTokensDefault/maxTokensMaximum/stopSequencesDefault/
reasoningSupported/additionalRequestFieldsSchema/user*TypesSupported), `featuresSupported`,
`batchSupported`, `explicitPromptCaching`, `intelligentPromptRouting`, `guardrailsSupported`,
`inferenceAPIsSupported`, `latencyOptimizationSupported`, `modelFamily`, `description` and
`modelLifecycle.startOfLifeTime` are recorded behaviour only, with the recorded 200 body as
the authority.
Emulator: 200, with every catalogued `modelId` the recording carries now present, but the
replay still reports 1166 member-level divergences across the catalog — the first of them
`$.modelSummaries[modelId="amazon.nova-2-5-sonic"].converse`,
`...featuresSupported.promptOptimization`, `...guardrailsSupported` and
`...inferenceAPIsSupported`. These are per-model published facts: the emulator computes each
from the attributes its catalog row carries, and the recording publishes different values for
them.
Why: a client reads `converse.maxTokensMaximum` or `converse.additionalRequestFieldsSchema` to
build a Converse request, and `featuresSupported`/`batchSupported`/`guardrailsSupported` to
decide whether it may batch, guard or cache a model it picked out of the listing, so a wrong
value is acted on rather than reported.
Fix: the per-model published data behind the capability projection
(`crate::state::foundation_models`): the catalog rows and the tables the projection reads
(`CONVERSE_MAX_OUTPUT_TOKENS`, `REASONING_MODELS`, `PUBLISHED_FEATURE_VALUES`,
`CONVERSE_PROVIDER_DOCUMENT_FORMATS`, the lifecycle instants), model by model.
Traces: bedrock-catalog-list-foundation-models#task#1 (expected 200 / actual 200, body
mismatch).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from the predecessor's
fix: both steps answer 200 and both bodies still differ.

* ListInferenceProfiles: `$.inferenceProfileSummaries[10]` .. `[92]` — 83 recorded summaries
  with no counterpart. Recorded detail the predecessor's report carries for two of them:
  `us.meta.llama3-2-11b-instruct-v1:0` / `us.meta.llama3-2-90b-instruct-v1:0`, each with
  `inferenceProfileName` "US Meta Llama 3.2 11B/90B Instruct", two `models[]`
  `foundation-model` ARNs (us-east-1 and us-west-2), `status` ACTIVE, `type` SYSTEM_DEFINED,
  `toolboxArn` null and matching `createdAt`/`updatedAt`. Those two ids are published now; the
  recorded instants are not known from the replay, which reports an unmatched recorded entry
  by index only.
* ListFoundationModels: 1166 member paths, the first being the four
  `$.modelSummaries[modelId="amazon.nova-2-5-sonic"]` members named above. The replay reports
  the diverging PATHS and not the recorded values, so a fix needs the recorded body itself.

Model evidence: `smithy_query(query="operation",operation="ListFoundationModels,ListInferenceProfiles")`
and `query="shape",shape="FoundationModelSummary,InferenceProfileSummary"` on
services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json.

## Steps to Reproduce
1. Clone this repo and boot the emulator built from the checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Cause 1: replay fixture bedrock-catalog-list-inference-profiles, step task#1 — GET
   /inference-profiles. The response is 200 with 14 summaries where the recording carries 93;
   diff the set, the order and the members (no summary carries `toolboxArn`).
4. Cause 2: replay fixture bedrock-catalog-list-foundation-models, step task#1 — GET
   /foundation-models with no query filters. The response is 200 and every recorded `modelId`
   is present; diff the capability members of each summary, starting with
   `modelId="amazon.nova-2-5-sonic"`.
5. Re-replay both fixture steps after the fix; both must pass.
