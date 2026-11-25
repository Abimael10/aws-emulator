Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 8d09b3a58159015972e86c55f5567866e2de773a

# The ChannelRef→registry-key resolution rule is written three times in state.rs, and only the read copy is pinned by a test

_Filed: 2026-11-25T09:36:11Z_
_Operation: PutAuditEvents | Class: maintainability_

> _Note: this issue was identified against the code base at commit `8d09b3a58159015972e86c55f5567866e2de773a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ComAmazonawsCloudtraildataCloudtraildataserviceState` resolves a `ChannelRef` to the key its `channels` map is keyed by in THREE separate places — `channel`, `channel_mut` and `remove_channel` — each repeating the same two-armed match (an `Arn` reference uses the ARN verbatim; an `Id` reference goes through `arn_for_channel_id`, which deliberately answers `None` for an ambiguous suffix). `channel_mut` and `remove_channel` are token-for-token identical; `channel` differs only in borrowing `&str` instead of cloning a `String`.

The rule those copies encode is not incidental: `arn_for_channel_id`'s own doc states the ambiguity decision ("`None` ... when SEVERAL do: the same suffix under two accounts or regions is ambiguous, and resolving it by `HashMap` iteration order would pick a different channel from run to run"), and the crate's test suite pins that decision on the READ copy only (`channel`). A later edit to how a reference becomes a key — a normalisation, an extra arm, a different ambiguity verdict — applied to one copy leaves the other two resolving references differently, so the path `PutAuditEvents` ingests through (`channel_mut`) and the path `cloudtrail:DeleteChannel` retires through (`remove_channel`) can silently disagree with the path every read resolves through.

The fix is one private key-resolution helper the three methods call; every response and every existing test stays byte-for-byte unchanged.

## Evidence

Copy 1 — services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/state.rs:499-505:

```
    pub fn channel(&self, reference: &ChannelRef) -> Option<&Channel> {
        let key = match reference {
            ChannelRef::Arn(arn) => arn.arn.as_str(),
            ChannelRef::Id(id) => self.arn_for_channel_id(id)?,
        };
        self.channels.get(key)
    }
```

Copy 2 — src/state.rs:508-514:

```
    pub fn channel_mut(&mut self, reference: &ChannelRef) -> Option<&mut Channel> {
        let key = match reference {
            ChannelRef::Arn(arn) => arn.arn.clone(),
            ChannelRef::Id(id) => self.arn_for_channel_id(id)?.to_string(),
        };
        self.channels.get_mut(&key)
    }
```

Copy 3 — src/state.rs:519-525:

```
    pub fn remove_channel(&mut self, reference: &ChannelRef) -> Option<Channel> {
        let key = match reference {
            ChannelRef::Arn(arn) => arn.arn.clone(),
            ChannelRef::Id(id) => self.arn_for_channel_id(id)?.to_string(),
        };
        self.channels.remove(&key)
    }
```

The rule the copies carry, stated once — src/state.rs:483-486: "The ARN of the one registered channel whose ARN suffix is `id`, or `None` when no channel carries that suffix or when SEVERAL do: the same suffix under two accounts or regions is ambiguous, and resolving it by `HashMap` iteration order would pick a different channel from run to run."

All three copies are reached in production: `channel_mut` by the handler (src/ops/put_audit_events.rs:427 `let Some(channel) = guard.channel_mut(&channel_ref) else {`), `channel` by the registration and reporting seams (src/state.rs:579 `if store.channel(&reference).is_some()`, src/state.rs:669 `let channel = store.channel(&ChannelRef::Arn(parsed))?;`) and `remove_channel` by the deregistration seam (src/state.rs:696-699 `state.lock().remove_channel(&ChannelRef::Arn(parsed))`).

Test coverage that must stay unchanged: tests/all/put_audit_events.rs:789 `a_channel_resolves_by_its_arn_or_by_its_arn_suffix` — which pins the ambiguity verdict on the `channel` copy ALONE (tests/all/put_audit_events.rs:861-874: "A second registration sharing the ARN suffix makes the bare-id form AMBIGUOUS: it must resolve to nothing, deterministically ... `state.lock().channel(&bare).is_none()`") — plus tests/all/put_audit_events.rs:283 `ingest_accumulates_in_the_channel_across_calls` and tests/all/put_audit_events.rs:503 `channel_resolution_answers_the_modelled_error_for_each_arn_form`, which reach the `channel_mut` copy through the handler, and tests/all/put_audit_events.rs:890 `the_ingest_log_is_bounded_by_its_retention_limits`. `grep` over tests/ for `channel_mut|remove_channel|arn_for_channel_id` returns no matches, so neither mutating copy is exercised against the ambiguity rule at all.

## Steps to Reproduce

1. Open services/service-com-amazonaws-cloudtraildata-cloudtraildataservice/src/state.rs and read `channel` (499-505), `channel_mut` (508-514) and `remove_channel` (519-525): the same `ChannelRef` → map-key match is written three times.
2. Edit the `ChannelRef::Id` arm of `channel_mut` only — e.g. resolve an ambiguous suffix to the first matching channel instead of to `None` — and run the crate's test suite.
3. Observe that every test still passes: `a_channel_resolves_by_its_arn_or_by_its_arn_suffix` (tests/all/put_audit_events.rs:861-874) asserts the ambiguity verdict through `channel`, so the ingest path and the read path have drifted with no failure at the edit site.
4. Repeat for `remove_channel`: `grep -n 'remove_channel' tests/` finds nothing, so the retire path's copy of the rule is unpinned as well.
