Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 24511e4ce5aef4d33e503a00bb0e57c4ce26e21d

# state.rs's refusal-text docs name the wrong operations for three of the instance-scoped 404 sentences

_Filed: 2026-11-25T19:55:47Z_
_Operation: DeleteConnectInstanceConfig | Class: maintainability_

> _Note: this issue was identified against the code base at commit `24511e4ce5aef4d33e503a00bb0e57c4ce26e21d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate words an instance-scoped 404 with one of four sentences, and `src/state.rs` documents, for each sentence, WHICH operations state it — the stated invariant being that "two of them cannot disagree about a request they both refuse". Three of those doc claims are falsified by the crate's own handlers, and two of them contradict each other inside the same file:

1. `instance_config_not_found_message`'s doc claims the sentence is shared by "the two on the instance's communication-limits". Those two handlers (`GetInstanceCommunicationLimits`, `PutInstanceCommunicationLimits`) answer `INSTANCE_NOT_FOUND_MESSAGE` instead, and `instance_config_not_found_message` appears in neither.
2. `INSTANCE_NOT_FOUND_MESSAGE`'s doc claims the sentence is shared by "starting an onboarding job, creating a campaign, and listing an instance's integrations". `StartInstanceOnboardingJob` answers `instance_id_not_found_message` — which is what the doc of THAT helper says, 220 lines earlier in the same file ("The one operation whose wire states this sentence today — the onboarding start"), so the two docs directly contradict one another. The list also omits the two communication-limits operations that do state it.
3. `InstanceConfigDeletion::NotConfigured`'s doc claims "The caller answers this with the modelled `ResourceNotFoundException` (HTTP 404) carrying [`instance_config_not_found_message`]". Its only caller, `delete_connect_instance_config.rs`, answers `INSTANCE_NOT_ONBOARDED_MESSAGE` on exactly that arm.

The WIRE is not at issue: `tests/all/instance_refusal_messages.rs` pins each operation to the sentence it currently answers, so the handlers are the authoritative side and the three doc sentences are the drifted copy of the same rule. The hazard is concrete: a later reader who takes `InstanceConfigDeletion::NotConfigured`'s doc at face value would "restore" `instance_config_not_found_message` in the config deletion and break the pinned text, and the mutually contradictory claims about `StartInstanceOnboardingJob` make the file unusable as the single statement of the rule it says it is. The fix is to make the three lists say what the handlers do; every response and every existing test stays byte-for-byte unchanged.

## Evidence

Doc side 1 — services/service-com-amazonaws-connectcampaignsv2-amazonconnectcampaignservicev2/src/state.rs:2072-2077 (doc of `instance_config_not_found_message`, defined at :2080):
```
/// Composed from the request, so the refusal names the instance the caller asked
/// about, as the recorded responses of this class do. The one text shared by every
/// operation whose wire states it — the two on `/v2/connect-instance/{id}/config`, the
/// two on the instance's communication-limits, which are a member of that same stored
/// configuration (see [`StoredInstanceOnboarding::communication_limits`]), and the two
/// that write an instance's integrations
```
Code side 1 — src/ops/get_instance_communication_limits.rs:171-174:
```
                        // The usability sentence, the one this service's wire states for
                        // the instance limits — the write of the same member answers it
                        // too, so the two cannot disagree.
                        message: crate::state::INSTANCE_NOT_FOUND_MESSAGE.to_string(),
```
and src/ops/put_instance_communication_limits.rs:250: `message: crate::state::INSTANCE_NOT_FOUND_MESSAGE.to_string(),`

Doc side 2 — src/state.rs:2274-2277 (doc of `INSTANCE_NOT_FOUND_MESSAGE`, defined at :2284):
```
/// kind reads the same and the response is deterministic. The one text shared by the
/// operations whose wire states the instance's usability (starting an onboarding job,
/// creating a campaign, and listing an instance's integrations), so two of them cannot
/// disagree about a request they both refuse.
```
Contradicting doc in the same file — src/state.rs:2056-2057 (doc of `instance_id_not_found_message`):
```
/// the recorded response of this class does. The one operation whose wire states this
/// sentence today — the onboarding start;
```
Code side 2 — src/ops/start_instance_onboarding_job.rs:265-270:
```
                        // The instance id itself is reported unresolved, naming it:
                        // the sentence this service's onboarding plane states (see
                        // `crate::state::instance_id_not_found_message`).
                        message: crate::state::instance_id_not_found_message(
                            &input.connect_instance_id,
                        ),
```

Doc side 3 — src/state.rs:3699-3702 (`InstanceConfigDeletion::NotConfigured`):
```
    /// The region holds no configuration for the instance at the instant the call was
    /// judged, so there was none to delete. The caller answers this with the modelled
    /// `ResourceNotFoundException` (HTTP 404) carrying
    /// [`instance_config_not_found_message`].
```
Code side 3 — its only caller, src/ops/delete_connect_instance_config.rs:268-271:
```
                            // The instance holds no finished onboarding, so the sentence
                            // is the one this service states about an instance that is
                            // not onboarded (`crate::state::INSTANCE_NOT_ONBOARDED_MESSAGE`).
                            message: crate::state::INSTANCE_NOT_ONBOARDED_MESSAGE.to_string(),
```

Test coverage that pins the CODE side (so the fix must be doc-only) — tests/all/instance_refusal_messages.rs:126-150:
```
        Op {
            name: "DeleteConnectInstanceConfig",
            ...
            refusal: Refusal::NotOnboarded,
        },
        Op {
            name: "GetInstanceCommunicationLimits",
            ...
            refusal: Refusal::Instance,
        },
        Op {
            name: "PutInstanceCommunicationLimits",
            ...
            refusal: Refusal::Instance,
        },
```
with `Refusal::message` at tests/all/instance_refusal_messages.rs:71-84 spelling `Refusal::Config` as `"ConnectInstanceConfig not found {instance}."`, `Refusal::Instance` as `"Connect Instance does not exist or is not enabled to use Campaigns."` and `Refusal::NotOnboarded` as `"Instance is not onboarded"`, and `the_shared_texts_are_the_ones_the_handlers_compose` (:380-406) pinning each helper's wording.

## Steps to Reproduce

1. Open services/service-com-amazonaws-connectcampaignsv2-amazonconnectcampaignservicev2/src/state.rs:2072-2077 and read the claim that `instance_config_not_found_message` is shared by "the two on the instance's communication-limits".
2. grep -n 'instance_config_not_found_message\|INSTANCE_NOT_FOUND_MESSAGE\|INSTANCE_NOT_ONBOARDED_MESSAGE\|instance_id_not_found_message' src/ops/*.rs — the two communication-limits handlers use INSTANCE_NOT_FOUND_MESSAGE, never instance_config_not_found_message.
3. Read src/state.rs:2274-2277 (INSTANCE_NOT_FOUND_MESSAGE is "shared by ... starting an onboarding job") beside src/state.rs:2056-2057 (instance_id_not_found_message is stated by "the onboarding start") — the two docs cannot both be true; src/ops/start_instance_onboarding_job.rs:268 settles it in favour of the latter.
4. Read src/state.rs:3699-3702 (`NotConfigured` "carrying instance_config_not_found_message") beside its only caller src/ops/delete_connect_instance_config.rs:271, which carries INSTANCE_NOT_ONBOARDED_MESSAGE.
5. Run tests/all/instance_refusal_messages.rs: it passes, confirming the handlers (not the docs) match the pinned wire texts.
