Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__imagebuilder__logs.create-image-pipeline-both-recipe-arns-rejected
Issue-Key: iam__imagebuilder__logs.create-image-pipeline-both-recipe-arns-rejected|run#13|AssertionError|assert got_error == _BOTH_RECIPES, f"expected {_BOTH_RECIPES!r}, got {got_error!r}"
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# Image Builder CreateImagePipeline's both-recipe-ARNs refusal uses InvalidRequestException with emulator wording instead of AWS's InvalidParameterCombinationException

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateImagePipeline

## Description
Cause 1 of 1: the mutually-exclusive-recipe refusal has the wrong error class and message.
AWS: Smithy imagebuilder CreateImagePipeline models imageRecipeArn and containerRecipeArn as separate optional members (each with its own ARN pattern) and binds InvalidRequestException among its errors; a request naming BOTH is refused with the parameter-combination error class InvalidParameterCombinationException and the message "The value supplied for parameter 'imageRecipeArn' and 'containerRecipeArn' is not valid. You must specify either imageRecipeArn or containerRecipeArn." — the fixed text names the two conflicting member names, which are the only variable parts of the template.
Emulator: the request is refused, but as InvalidRequestException with the emulator's own sentence 'The request must specify exactly one of imageRecipeArn and containerRecipeArn.'
Why: the error CODE is what clients branch on and what the SDK maps to a typed exception; InvalidParameterCombinationException is a different class from InvalidRequestException, so a caller catching the modeled parameter-combination error never sees it, and the message text does not match AWS's.
Related open issues covering other imagebuilder refusal wordings (they do not carry this replay key): 20261107T064925Z-imagebuilder-3-net-new-refusal-message-divergences-resourcealreadyexistsexception.md, 20261114T230016Z-imagebuilder-2-net-new-refusal-message-divergences-distribution.md.
Fix: the CreateImagePipeline handler's input validation — the mutually-exclusive recipe check must raise the parameter-combination error class with AWS's message.
Failing check: iam__imagebuilder__logs.create-image-pipeline-both-recipe-arns-rejected

## Evidence
[mismatch] iam__imagebuilder__logs.create-image-pipeline-both-recipe-arns-rejected — AssertionError: expected ('InvalidParameterCombinationException', "The value supplied for parameter 'imageRecipeArn' and 'containerRecipeArn' is not valid. You must specify either imageRecipeArn or containerRecipeArn."), got ('InvalidRequestException', 'The request must specify exactly one of imageRecipeArn and containerRecipeArn.').

## Steps to Reproduce
1. Create an image recipe, a container recipe and an infrastructure configuration.
2. Call CreateImagePipeline naming both imageRecipeArn and containerRecipeArn and compare the error code and message with AWS's.
