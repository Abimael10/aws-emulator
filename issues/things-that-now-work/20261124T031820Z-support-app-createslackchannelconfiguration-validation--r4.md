Issue-Severity: LOW
Issue-Gap-Class: validation
Issue-Commit: 00d0ee708fc71320b0d32b5a079c70dd1110146a

# The by-hand ECMA-262 @pattern rule this crate applies to channelName and accountAlias is not applied to teamId/channelId's ^\S+$

_Filed: 2026-11-24T03:18:20Z_
_Operation: CreateSlackChannelConfiguration | Class: validation_

> _Note: this issue was identified against the code base at commit `00d0ee708fc71320b0d32b5a079c70dd1110146a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states, and implements twice, a rule of its own: where the framework's centralized constraint walk reads a modelled `@pattern`'s character class MORE widely than the ECMA-262 expression the model declares, the part the walk admits but the model does not must be applied by hand in the handler, before the store is reached, because the same shape with the same `@pattern` is also an OUTPUT member and a value the expression does not admit could not be reported back through it. The crate does this for `.` (`channelName`, and the role name after `role/`) and for `\w` (`accountAlias`).

It is NOT done for the other class this model declares: `teamId` and `channelId` are `pattern ^\S+$`. The walk compiles that expression with the Rust `regex` crate, whose `\s` is `\p{White_Space}`; U+FEFF (ZERO WIDTH NO-BREAK SPACE) is not in `White_Space`, so the walk's `\S` matches it. In ECMA-262 — the dialect a Smithy `@pattern` is, and the dialect this crate's own comments reason in — U+FEFF `<ZWNBSP>` IS WhiteSpace, so `\S` does not match it and `^\S+$` rejects the value.

Consequence on the one path where such a value becomes persisted state: `CreateSlackChannelConfiguration` accepts a `channelId` carrying U+FEFF (no handler check exists; `crate::state` has no `\S` rule at all), stores it under the pair key, and `ListSlackChannelConfigurations`/`UpdateSlackChannelConfiguration` then report it in `SlackChannelConfiguration$channelId`, a `@required` member whose declared `@pattern ^\S+$` does not admit it — exactly the outcome the crate's own `channel_name_refusal`/`account_alias_refusal` docs say those rules exist to prevent. No comment, test or doc in the crate justifies treating `^\S+$` differently from `^.+$` and `^[\w\- ]+$`.

## Evidence

The rule the crate states for `\w`, `services/service-com-amazonaws-supportapp-supportapp/src/state.rs:752-762`:
  "The member's expression is `\w`-based, and the framework's centralized constraint walk cannot apply such an expression in full: it compiles a `@pattern` with an engine whose `\w` is the Unicode class, where the modelled ECMA-262 expression's `\w` is the ASCII one ([`is_pattern_word_char`]). So the part of the expression that walk admits but the model does not is applied HERE, by the one operation that writes the member (`PutAccountAlias`), before the store is reached: the same shape with the same `@pattern` is an OUTPUT member ... so a value the expression does not admit could not be reported back through the shape it was stored in."

The same rule for `.`, `src/state.rs:658-669`:
  "The member's expression is `.`-based, and the framework's centralized constraint walk cannot apply such an expression in full: it compiles a `@pattern` with an engine whose `.` excludes a line FEED alone, where the modelled ECMA-262 expression's `.` excludes every one of the [`PATTERN_DOT_EXCLUDED_CHARS`]. So the part of the expression that walk admits but the model does not is applied HERE, by both operations that write the member (`CreateSlackChannelConfiguration` and `UpdateSlackChannelConfiguration`), before the store is reached"

The engine those comments describe is the Rust `regex` crate, `vendor/smithy-schema-server/src/constraints.rs:338-342`:
  "The `regex` is the Smithy pattern verbatim (e.g. `^[a-zA-Z0-9_.-]+$`); Smithy pattern semantics are a *find* ... which is exactly [`regex::Regex::is_match`]."
and `vendor/smithy-schema-server/src/constraints.rs:474`: `let Ok(re) = regex::Regex::new(regex) else {`.

The member the model declares with the third class, `smithy_query(query="shape",shape="SlackChannelConfiguration")`:
  "channelId": {"target": "channelId", "required": true, "length": {"min": 1, "max": 256}, "pattern": "^\\S+$", ...} — and the same `pattern ^\S+$` on `CreateSlackChannelConfigurationRequest$channelId` (`services/service-com-amazonaws-supportapp-supportapp/OPERATIONS.md:28`).

The create handler's input rules, `src/ops/create_slack_channel_configuration.rs:218-255`, cover only `channel_name` (`crate::state::channel_name_refusal`) and `channel_role_arn` (`crate::state::channel_role_arn_refusal`); `input.channel_id` is carried straight to `store.store_channel_configuration(...)` at `src/ops/create_slack_channel_configuration.rs:354-363`. `crate::state` declares `PATTERN_DOT_EXCLUDED_CHARS` (state.rs:623) and `is_pattern_word_char` (state.rs:733) but nothing for `\S`.

Test coverage: `tests/all/shared_pattern_anchor_rendering.rs:31-33` names `^\S+$` as "`teamId`'s and `channelId`'s declared `@pattern`" and its rows only cover a value the walk itself catches ("a create whose teamId carries a space", line 77-79); `tests/all/channel_name_pattern.rs:64` and `tests/all/account_alias_pattern.rs:58` cover the by-hand halves of the other two classes. No suite covers a `teamId`/`channelId` character the walk admits and ECMA-262 does not.

## Steps to Reproduce

1. Register a Slack workspace so a create is accepted (as `tests/all/organization_membership.rs` does: create an organization, authorize the workspace in the console, call RegisterSlackWorkspaceForOrganization with teamId `T012ABCDEFG`).
2. POST /control/create-slack-channel-configuration with `{"teamId":"T012ABCDEFG","channelId":"C012\uFEFF345","notifyOnCaseSeverity":"none","channelRoleArn":"arn:aws:iam::000000000000:role/SupportAppRole"}`.
3. Observe HTTP 200: the walk's `\S` (Rust regex, `\p{White_Space}`) matched U+FEFF, and no handler rule applies the modelled ECMA-262 `\S`, under which `^\S+$` rejects the value.
4. POST /control/list-slack-channel-configurations and observe the stored record reported with `channelId` carrying U+FEFF — a `@required` member declared `pattern ^\S+$`.
5. Compare with the equivalent value for the two classes the crate does handle: a `channelName` carrying U+2028 (POST create with it) is refused 400 by `crate::state::channel_name_refusal`, and an `accountAlias` of `"é"` is refused 400 by `crate::state::account_alias_refusal`.
