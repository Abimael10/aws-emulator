Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 748a8a1bc704f94a0a2a8328e7bfeac677ed6e66

# The union-borne constraint checks are documented by two contradictory rules for what the centralized walk reaches

_Filed: 2026-11-26T22:56:21Z_
_Operation: CreateCampaign | Class: maintainability_

> _Note: this issue was identified against the code base at commit `748a8a1bc704f94a0a2a8328e7bfeac677ed6e66`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state` states ONE rule for which constrained leaves reach a handler unchecked, and it is the rule the crate's helpers are built around: a constraint escapes the centralized walk only when the UNION MEMBER SCHEMA was the only thing that could have carried it — a union member that is a bare string, or one whose own `@length` bounds a list — while "[w]here the union member is a STRUCTURE, a LIST of structures or a MAP of them, the walk descends and judges each structure against its own generated schema ... so those leaves arrive checked" (src/state.rs:530-534). The crate relies on that rule twice over: `ChannelSubtypeConfig` gets NO helper at all because every member of its `outboundMode`/`PacingStrategy` unions is a structure (src/state.rs:541-553), and `DeleteConnectInstanceIntegration` labels its own identical structure check "[a] BACKSTOP rather than the first line of defence — the boundary validator reaches that structure's members and refuses such a value before `handle` runs" (src/ops/delete_connect_instance_integration.rs:180-183).

Three handler call sites state the opposite rule for the very same mechanism. `CreateCampaign` says the ARNs behind the `Source` union "sit where the centralized constraint walk does not reach" (src/ops/create_campaign.rs:218-221), `UpdateCampaignSource` says they "sit behind a union member, which the centralized constraint walk does not reach" (src/ops/update_campaign_source.rs:187-189), and `PutOutboundRequestBatch` says the template ARNs and attribute keys "sit behind the `channelSubtypeParameters` union, which the centralized constraint walk does not reach" (src/ops/put_outbound_request_batch.rs:175-182). Both `Source$eventTrigger` and all four `ChannelSubtypeParameters` members are STRUCTURES, so under the crate's own rule their ARN leaves DO arrive checked and those handler checks are backstops exactly like the integration identifier's — only the bare-string `Source$customerProfilesSegmentArn` arm and the map KEYS are first-line. `source_violation` itself documents the structure arm the second way ("its generated member schemas carry the model's constraints, so the walk judges it as the boundary would have", src/state.rs:668-670), directly under the call sites that say the walk never gets there.

So the one rule that decides whether a helper is needed at all is written down twice, in opposite terms, and the two copies have already drifted into labelling the identical check differently (backstop in one handler, sole line of defence in three). A maintainer reading only a call site cannot tell which checks may be removed if codegen changes, and the next union member added to this model will be judged by whichever copy its author happens to read.

The fix is to state the one rule once — the call-site comments restating the crate's own block in src/state.rs:524-553, and the structure arms labelled backstops as `delete_connect_instance_integration.rs` already labels its own. No check is removed and no refusal changes, so every response and every existing test stays exactly as it is.

## Evidence

The rule, as `crate::state` states it — services/service-com-amazonaws-connectcampaignsv2-amazonconnectcampaignservicev2/src/state.rs:530-534:

```
// [`communication_limits_violation`] above exists). Where the union member is a
// STRUCTURE, a LIST of structures or a MAP of them, the walk descends and judges each
// structure against its own generated schema, whose member schemas DO carry the
// model's `@length`/`@pattern`, so those leaves arrive checked — a map KEY stays the
// exception, which is what [`communication_time_violation`] above weighs.
```

The crate acts on that rule: src/state.rs:541-549:

```
// `ChannelSubtypeConfig` is the union-borne case that rule settles with NO check here,
// which is why no helper for it stands beside the ones above: every member of the four
// `outboundMode` unions ... is a
// STRUCTURE carrying no constraint trait on the union member itself, so the centralized
// walk descends to every leaf behind them ... and the operations that accept such a config
// (`CreateCampaign`, `UpdateCampaignChannelSubtypeConfig`) are handed one already
// checked.
```

and src/ops/delete_connect_instance_integration.rs:177-183:

```
        // The ARN the identifier carries is judged off the generated schema of the
        // structure behind the union member, as a bad request, before the instance is
        // looked up or anything is removed: an ARN those bounds refuse is one no put
        // could have stored an integration under. A BACKSTOP rather than the first line
        // of defence — the boundary validator reaches that structure's members and
        // refuses such a value before `handle` runs — but it is what keeps this
        // handler's own contract if that walk ever narrows.
```

The opposite statement of the same mechanism, at three call sites. src/ops/create_campaign.rs:218-221:

```
        // The ARNs behind the `Source` union sit where the centralized constraint
        // walk does not reach either, so the shared check holds them to the
        // `@length` and `@pattern` of the shape they target before the campaign is
        // stored — exactly as `UpdateCampaignSource` does when it replaces one.
```

src/ops/update_campaign_source.rs:187-191:

```
        // The ARNs the `Source` union carries sit behind a union member, which the
        // centralized constraint walk does not reach, so they are held to the
        // `@length` and `@pattern` of the shape they target here — as a bad request,
        // before anything is looked up or stored, so no campaign comes to hold a
        // source `DescribeCampaign` would project outside the model's own shape.
```

src/ops/put_outbound_request_batch.rs:175-180:

```
        // The source-number and template ARNs a request carries, and the keys of the
        // attribute map every channel carries, sit behind the
        // `channelSubtypeParameters` union, which the centralized constraint walk does
        // not reach — and a map key is one it never validates even where it does reach
        // — so the shared check holds each of them to the `@length` and `@pattern` of
        // the shape it targets.
```

The helper those call sites invoke documents the structure arm the FIRST way, contradicting the comment above its own call — src/state.rs:665-675:

```
        Source::customerProfilesSegmentArn(arn) => {
            string_member_refusal(member, &SEGMENT_ARN_SCHEMA, arn)
        }
        // A structure behind the union: its generated member schemas carry the
        // model's constraints, so the walk judges it as the boundary would have —
        // including the optional domain ARN, absent from a trigger that carries none.
        Source::eventTrigger(trigger) => structure_refusal(
```

The affected members are structures, per the model: `Source` is a union of `customerProfilesSegmentArn` (a bare string) and `eventTrigger` (`EventTrigger`, a structure carrying `customerProfilesDomainArn`), and `ChannelSubtypeParameters` is a union of four structures (`smithy_query(query="shape", shape="Source,ChannelSubtypeParameters,EventTrigger")`).

Test coverage of the behaviour that must not change: `tests/all/union_member_arn_constraints.rs` (every union-borne ARN, both refused and accepted at each published bound, over `CreateCampaign`, `UpdateCampaignSource`, `PutConnectInstanceIntegration`, `DeleteConnectInstanceIntegration` and `PutOutboundRequestBatch`), `tests/all/channel_subtype_config_is_one_rule.rs` (the leaves behind `outboundMode`/`PacingStrategy`, which no handler checks), `tests/all/integration_identifier_precedes_lookup.rs` and `tests/all/outbound_batch_is_one_rule.rs`.

## Steps to Reproduce

1. Read `services/service-com-amazonaws-connectcampaignsv2-amazonconnectcampaignservicev2/src/state.rs:524-553`: the crate's one rule is that a STRUCTURE behind a union member is descended into by the centralized walk, so its leaves "arrive checked", and only a bare-string union member or a constraint on the union member itself escapes it.
2. Read `src/ops/delete_connect_instance_integration.rs:177-183`: the identical structure check is labelled "A BACKSTOP rather than the first line of defence — the boundary validator reaches that structure's members".
3. Read `src/ops/create_campaign.rs:218-221`, `src/ops/update_campaign_source.rs:187-191` and `src/ops/put_outbound_request_batch.rs:175-182`: all three say the walk "does not reach" the ARNs behind `Source` / `channelSubtypeParameters`, whose members `smithy_query(query="shape", shape="Source,ChannelSubtypeParameters")` shows to be structures (apart from `Source$customerProfilesSegmentArn`).
4. Read `src/state.rs:661-676`: `source_violation`'s own `eventTrigger` arm states the first rule ("the walk judges it as the boundary would have"), directly contradicting the comments above its two call sites.
5. Observe that the two statements cannot both hold, so the rule deciding whether a union-borne helper is needed at all is recorded twice and has already drifted in how it labels the same check.
