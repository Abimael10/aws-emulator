Issue-Source: trace-sweep-divergence
Issue-Trace: imagebuilder-ibp-traj-cleanup-in-dependency-order
Issue-Step: task#15
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: imagebuilder-ibp-traj-cleanup-in-dependency-order
Issue-Shape-Id: com.amazonaws.imagebuilder#imagebuilder
Issue-Key: imagebuilder|GetImageRecipe|4xx->2xx|$.message
Issue-Key: imagebuilder|GetImageRecipe|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: imagebuilder|GetImageRecipe|4xx->2xx|status
Issue-Key: imagebuilder|GetComponent|4xx->2xx|$.message
Issue-Key: imagebuilder|GetComponent|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: imagebuilder|GetComponent|4xx->2xx|status
Issue-Commit: 1428e63d4058e13a4521886f9248923e22581e2a

# Image Builder get paths still serve a deleted component and image recipe

> _Note: this issue was identified against the code base at commit `1428e63d4058e13a4521886f9248923e22581e2a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetImageRecipe

## Description
ONE root cause with TWO causes: after a component or image recipe is deleted, the imagebuilder emulator's single-resource read paths keep resolving the deleted ARN and answer 200 with the full resource body instead of the modeled not-found error. The store is never evicted (or the read path does not consult the deletion), so a deleted resource stays readable for the rest of the trajectory. Both causes must be closed.

Cause 1 of 2 — GetImageRecipe resolves a deleted image recipe. AWS: Smithy GetImageRecipe is GET /GetImageRecipe with required httpQuery member imageRecipeArn (pattern ^arn:aws[^:]*:imagebuilder:[^:]+:(?:[0-9]{12}|aws):image-recipe/[a-z0-9-_]+/(?:[0-9]+|x)\.(?:[0-9]+|x)\.(?:[0-9]+|x)$), success @http code 200. Once the recipe no longer exists AWS refuses the read with HTTP 404 and the not-found error (Smithy: ResourceNotFoundException, errorKind client, @httpError 404, documented "At least one of the resources referenced by your request does not exist"), carried in $.message and reflected in the x-amzn-errortype header. Message template, from the recorded response: fixed text "The following required resource 'ImageRecipe' cannot be found: '" + the ARN + "'." — the only variable part is the image-recipe build-version ARN taken verbatim from the request's imageRecipeArn; everything else, including the resource-type token 'ImageRecipe', is fixed text. Emulator: HTTP 200 with the deleted recipe's body and no $.message and no x-amzn-errortype at all.

Cause 2 of 2 — GetComponent resolves a deleted component build version. AWS: Smithy GetComponent is GET /GetComponent with required httpQuery member componentBuildVersionArn, success @http code 200; after the component build version is deleted the read is refused with HTTP 404 and the same not-found error class (ResourceNotFoundException, client, @httpError 404). Message template, from the recorded response: fixed text "The following required resource 'Component' cannot be found: '" + the ARN + "'." — the variable part is the component build-version ARN from the request's componentBuildVersionArn; the resource-type token 'Component' and the rest of the sentence are fixed text. Emulator: HTTP 200 with the deleted component's body, no $.message, no x-amzn-errortype.

Why: a client uses the single-resource get as the authority on whether a resource exists. Because the emulator keeps answering 200, every delete-then-verify and poll-until-gone loop either spins forever or concludes the delete silently failed; idempotent teardown logic re-issues deletes, dependency-ordered cleanup never advances, and code that branches on ResourceNotFoundException (the documented way to test for existence) takes the wrong branch and treats a removed recipe/component as still usable — then builds a recipe or pipeline on an ARN AWS would reject. It also hides real failures in the opposite direction: a caller can no longer distinguish 'not deleted' from 'deleted'.

Fix: the imagebuilder state layer plus the get_image_recipe and get_component handlers. The delete path must evict the component build version and the image recipe from the store (or mark them deleted and have the read path honour that), and the two get handlers must resolve a missing/deleted ARN to the modeled ResourceNotFoundException — HTTP 404, x-amzn-errortype ResourceNotFoundException, and $.message rendered from the fixed templates above with the ARN taken from the request — instead of returning the stored body with 200.

Related but not covering: open issue 20261102T085510Z-imagebuilder-image-builder-delete-and-recreate-lifecycle-leaves-stored.md carries the same not-evicted fault for GetInfrastructureConfiguration (its Cause 4) and for the DeleteImageRecipe/DeleteComponent 400 refusals, but it does not carry the GetImageRecipe or GetComponent read paths or their replay keys, so these two causes are unfiled and unexercised by that issue's pointer.

Traces: imagebuilder-ibp-traj-cleanup-in-dependency-order#task#15 (Cause 1), imagebuilder-ibp-traj-cleanup-in-dependency-order#task#21 (Cause 2).

## Evidence
Replay of simparity-wire-aws-v1 against the emulator (emulator_up=true, score 68.4%):

- imagebuilder-ibp-traj-cleanup-in-dependency-order#task#15 (GetImageRecipe): status expected 404, got 200; $.message expected "The following required resource 'ImageRecipe' cannot be found: 'arn:aws:imagebuilder:us-west-2:000000000000:image-recipe/<recipe-name>/<major.minor.patch>'.", got None; $headers.x-amzn-errortype expected to be present (shaped, ResourceNotFoundException), got None. The emulator returned the image recipe body for an ARN whose DeleteImageRecipe step preceded it.

- imagebuilder-ibp-traj-cleanup-in-dependency-order#task#21 (GetComponent): status expected 404, got 200; $.message expected "The following required resource 'Component' cannot be found: 'arn:aws:imagebuilder:us-west-2:000000000000:component/<component-name>/<major.minor.patch>/<build>'.", got None; $headers.x-amzn-errortype expected to be present (shaped, ResourceNotFoundException), got None. The emulator returned the component body for an ARN whose DeleteComponent step preceded it.

Model (smithy_query on services/service-com-amazonaws-imagebuilder-imagebuilder/smithy-model/com-amazonaws-imagebuilder-imagebuilder.json):
- GetImageRecipe: GET /GetImageRecipe, success code 200, required httpQuery imageRecipeArn; output requestId/imageRecipe/latestVersionReferences.
- GetComponent: GET /GetComponent, success code 200, required httpQuery componentBuildVersionArn; output requestId/component/latestVersionReferences.
- ResourceNotFoundException: com.amazonaws.imagebuilder#ResourceNotFoundException, errorKind client, httpError 404, member message.
The not-found status and the message text above are taken from the recorded responses; the ARN values in them are per-run and are template fill-ins, not requirements.

## Steps to Reproduce
1. CreateComponent(name=A, semanticVersion='1.0.0', platform='Linux', data=<valid document>, clientToken=<uuid>) and CreateImageRecipe(name=B, semanticVersion='1.0.0', parentImage=<valid AMI>, components=[A], clientToken=<uuid>), plus the pipeline/infrastructure resources that reference them.
2. Tear down in dependency order: delete the pipeline, then DeleteImageRecipe(<B build-version arn>).
3. GetImageRecipe(imageRecipeArn=<B build-version arn>) — expect HTTP 404, x-amzn-errortype ResourceNotFoundException and message "The following required resource 'ImageRecipe' cannot be found: '<that arn>'."; the emulator answers 200 with the recipe body.
4. DeleteComponent(<A build-version arn>), then GetComponent(componentBuildVersionArn=<A build-version arn>) — expect HTTP 404, x-amzn-errortype ResourceNotFoundException and message "The following required resource 'Component' cannot be found: '<that arn>'."; the emulator answers 200 with the component body.
5. Re-replay imagebuilder-ibp-traj-cleanup-in-dependency-order#task#15 and #task#21 to confirm both causes are closed.
