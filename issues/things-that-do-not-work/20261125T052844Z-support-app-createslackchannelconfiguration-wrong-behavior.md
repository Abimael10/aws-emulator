Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 266e6f7f0ec6488ec821cc522a9effa1af99936b

# The by-hand ECMA-262 `\S` rule corrects only one direction of the walk's divergence, so a teamId/channelId carrying U+0085 is refused although `^\S+$` admits it

_Filed: 2026-11-25T05:28:44Z_
_Operation: CreateSlackChannelConfiguration | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `266e6f7f0ec6488ec821cc522a9effa1af99936b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state` owns a by-hand application of the `\S`-based `@pattern` that `teamId`, `channelId` and `paginationToken` declare, because the engine the framework's constraint walk compiles a `@pattern` with reads the class differently from the modelled ECMA-262 expression. The rule the crate implements covers only ONE direction of that divergence — the character the walk ADMITS and the model rejects (U+FEFF), which `PATTERN_SPACE_CHARS_BEYOND_WHITE_SPACE` lists.

The divergence runs the other way too, and nothing in the crate handles it. By the crate's own enumeration of the ECMA-262 class (state.rs:756-760) the WhiteSpace/LineTerminator productions are the tab, vertical tab, form feed, space, no-break space, the `Space_Separator` category, U+FEFF and the four line terminators — U+0085 NEXT LINE is in none of them, so the modelled `^\S+$` ADMITS a value carrying U+0085. But the walk's engine reads `\s` as the Unicode `White_Space` property (state.rs:763-765), and U+0085 IS in `White_Space`, so the walk's `\S` does not match it and the request is refused with a 400 `ValidationException` pattern clause before any handler runs. A `teamId`/`channelId` the model admits is therefore rejected outright, and the crate's own `channelName` suite shows it knows U+0085 must be SERVED (it is listed among the characters "a reader would call whitespace or control but ECMA-262 does not call line terminators" and is required to round-trip).

The same engine divergence is already corrected in this crate for `channelRoleArn` by taking the member off the request boundary (`skip_validation`) and applying the whole modelled expression in the handler, so the mechanism to serve a value the model admits exists in this crate; it is simply not applied to the `\S` members. This is the reverse direction of the (already fixed) gap that added the U+FEFF check: that one let an unmodelled value through, this one refuses a modelled one.

## Evidence

The crate's own statement of the ECMA-262 class it is applying — services/service-com-amazonaws-supportapp-supportapp/src/state.rs:756-760:
```
/// A Smithy `@pattern` is an ECMA-262 regular expression, and there `\s` matches every
/// WhiteSpace and every LineTerminator: the tab, the vertical tab, the form feed, the space,
/// the no-break space, every character of the Unicode `Space_Separator` category, the ZERO
/// WIDTH NO-BREAK SPACE U+FEFF, and the four [`PATTERN_DOT_EXCLUDED_CHARS`]. `\S` is that
/// class complemented, so it matches none of them.
```
(U+0085 appears nowhere in that list, so by this reading `^\S+$` admits it.)

The engine the request boundary uses, and the one-direction scope of the correction — src/state.rs:762-766:
```
/// Stated here because such a class has to be applied by hand (see [`slack_id_refusal`]),
/// and because the engine that walk compiles with reads `\s` as the Unicode `White_Space`
/// property, which holds every one of those characters EXCEPT U+FEFF — so U+FEFF is the
/// whole of what its `\S` admits and the modelled expression's `\S` does not.
const PATTERN_SPACE_CHARS_BEYOND_WHITE_SPACE: [char; 1] = ['\u{FEFF}'];
```
U+0085 NEXT LINE is a member of the Unicode `White_Space` property, so the walk's `\S` rejects it; the constant holds only U+FEFF, so no handler rule can serve it.

The same crate requires U+0085 to be SERVED for the `.`-based member — tests/all/channel_name_pattern.rs:132-149:
```
/// Channel names made of characters the expression's `.` DOES match, including the ones a
/// reader would call whitespace or control but ECMA-262 does not call line terminators, and
/// names at both published `@length` bounds. Each is served and reported back verbatim.
fn names_the_pattern_admits() -> Vec<(&'static str, String)> {
...
        (
            "the next-line character U+0085 inside the name",
            "aws\u{0085}support".to_string(),
        ),
```

And the `\S` suite's enumeration of what the expression excludes — tests/all/slack_id_pattern.rs:78-99 — lists the tab, LF, VT, FF, CR, space, no-break space, U+1680, U+2028, U+2029, U+202F, U+205F, U+3000, U+FEFF and U+2000..U+200A, with NO U+0085 row and no U+0085 row in `ids_the_pattern_admits()` (slack_id_pattern.rs:165-177) either, so the character is untested in both tables.

The modelled constraint: `teamId`/`channelId` are `pattern "^\\S+$"` (`smithy_query(query="shape",shape="SlackChannelConfiguration")`: `"teamId": {... "pattern": "^\\S+$"}`, `"channelId": {... "pattern": "^\\S+$"}`), and only `channelRoleArn` is taken off the boundary (src/ops/types/create_slack_channel_configuration_types.rs:22-25 `skip_validation = ["channelRoleArn"]`), so `teamId`/`channelId` are refused by the walk.

## Steps to Reproduce

1. Register the workspace `T012ABCDEFG` so a create can be served (create an organization, then call RegisterSlackWorkspaceForOrganization), as tests/all/organization_membership.rs does.
2. POST /control/create-slack-channel-configuration with `{"teamId":"T012ABCDEFG","channelId":"C0123\u008545","notifyOnCaseSeverity":"none","channelRoleArn":"arn:aws:iam::000000000000:role/SupportAppRole"}`.
3. Observe HTTP 400 ValidationException with the clause `Value 'C0123\u008545' at 'channelId' failed to satisfy constraint: Member must satisfy regular expression pattern: \S+` — the walk's `\s` is the Unicode `White_Space` property (state.rs:763-765), which holds U+0085.
4. Read state.rs:756-760: U+0085 is not in the ECMA-262 WhiteSpace/LineTerminator productions the crate enumerates, so the modelled `^\S+$` admits the value and the request should have been served and the id stored.
5. Contrast with the `.`-based member, where the crate requires the same character to be served: POST a create with `"channelName":"aws\u0085support"` answers 200 and the name round-trips (tests/all/channel_name_pattern.rs:146-149, 340-384).
6. Note that `crate::state::PATTERN_SPACE_CHARS_BEYOND_WHITE_SPACE` holds only `'\u{FEFF}'`, i.e. only the direction where the walk is too permissive, and that the mechanism for the other direction already exists in this crate for `channelRoleArn` (`skip_validation` + by-hand application of the whole expression).
