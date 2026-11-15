// SPDX-License-Identifier: CC0-1.0

//! More of the notes concurrent landings appended to the END of `tests/all.rs` (see the
//! parent module): they are here because one file may hold at most 5,000 lines.

// head of this file: the copy that stood here was a second declaration, which is `E0428`
// and compiles neither, so it is removed.)
// one stays in the HEADER: an inner doc comment past the first item is `E0753`, so these
// lines stand as ordinary comments where a merge of two appended tails landed them.
// (A fragment of that same line, appended here typed with `//`: an inner doc comment is
// legal only before the first item, so it stands as an ordinary comment.)
// (The same line again, landed by another merge as an INNER doc comment: it stands as an
// ordinary comment, since `//` past the first item is `E0753`.)
// doc comment is accepted only before the first item, so one in this appended tail is
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, further up this file: this
// copy is removed, a module declared twice being `E0428`.)
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, further up this file: two
// landings in the same window each appended a declaration of it, and a module declared twice
// is `E0428`, so the duplicate that stood here is gone and the first declaration stands.)
// head of this file: two branches landing in the same window each appended a second
// declaration of it here, which is `E0428` and compiles neither copy, so the duplicate is
// removed and the declaration above stands.)
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, further up this file: the
// copy that stood here was a second declaration, which is `E0428` and compiles neither.)
// name is `E0428` and a note written with `//` anywhere but this file's header is `E0753` —
// either fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks the
// (Four more fragments of this file's own header, which a merge of two appended tails landed
// here typed with `//`: past the first item an inner doc comment is `E0753` and compiles no
// suite of this target, so they stand as ordinary comments. The rules they repeat — declare
// each suite exactly ONCE, and keep a note like this in the module doc at the HEAD of this
// file — are stated there, and `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that for this
// file and for every service crate's `tests/all.rs`. A suite left at the top level of `tests/`
// fails the build.)
// comment past the first item of this crate root is `E0753`.)
// same line again, as an ordinary comment: a suite left at the top level of `tests/` fails
// the build.
// (More of the same notes, gone for the same reason as the block at the end of this file.)
// (Six more lines of this file's own header landed in this tail typed with `//`: past the
// first item an inner doc comment is `E0753` and compiles no suite of this target, so they
// stand as ordinary comments. Declare each suite exactly ONCE — a second `mod` for the same
// name is `E0428` — and `all/all_rs_declares_each_suite_once.rs` checks that for this file
// and for every service crate's `tests/all.rs`, so check there rather than leaving a note
// (And seven more of the same, from the two landings after them: they stand as ordinary
// comments because `//` is legal only before the first item of this crate root. Declare
// each suite exactly ONCE — `E0428` otherwise — and keep a note like this in the module doc
// at the HEAD of this file, which `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check for this file and for
// (The notes that stood here are gone for the reason the block at the end of this file
// records: each was a copy of this file's own header, or a record that a concurrent landing
// had appended a second declaration of a suite that is declared once already.)
// (More of the same notes, gone for the same reason.)
// Declared here, beside the other EKS suite, for the reason the entries above record: how a
// stack does when its Lambda-backed custom resource's provider answers FAILED is only
// observable across CloudFormation, AWS Lambda and CloudWatch Logs at once — the Delete
// (The notes that stood here — down to the end of this file — are gone rather than
// re-typed: each was a copy of this file's own header, which is where that rule belongs
// (an inner doc comment is `E0753` past the first item), or a record that a concurrently
// landing branch had appended a second declaration of a suite that is declared once
// already. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that, for
// `all/all_rs_declares_each_suite_once.rs` checks the
// (Four more lines of that same header fragment, landed here typed with `//`: past the first
// item an inner doc comment is `E0753` and compiles no suite of this target, so they stand as
// ordinary comments — `all/all_rs_declares_each_suite_once.rs` and
// (Five more lines of those fragments, landed here with `//` by the two landings after them:
// they stand as ordinary comments for exactly the reason stated above.)
// (Removed: records of declarations that already stand once each above, and copies of this
// file's own header.)
// (Removed: more copies of this file's own header, several typed with `//`, and records of
// declarations that already stand once each above — the rules they repeat stand in the
// (Five more lines of the same fragment: ordinary comments, because `//` past the first item
// of this crate root is `E0753` and compiles no suite of this target. Declare each suite
// exactly ONCE — a second `mod` for the same name is `E0428` — and
// `all/all_rs_declares_each_suite_once.rs` checks the declarations while
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks the notes, for this file and
// for every service crate's `tests/all.rs`.)
// (Removed: more of the same appended header fragments.)
// (Removed: ten more lines of that same header fragment.)
// (Removed: records of declarations that already stand once each above, and more copies of
// this file's own header.)
// (Fragments of this file's own header and duplicate declarations that landings appended
// here are gone rather than re-typed, for the reason the note at the end of this file
// records.)
// (One more line of that same header fragment, as an ordinary comment for the reason the
// lines above record: a `//` line past the first item of this crate root is `E0753` and
// compiles no suite in this target.)
// (And one more line of that same fragment, an ordinary comment for the same reason: past
// the first item a `//` line is `E0753` and compiles no suite in this target.)
// (Four more lines of that same repeated rule, ordinary comments for the same reason: a
// `//` note anywhere but this file's header is `E0753` and compiles no suite in this
// target. Declare each suite ONCE — this is one crate root, so a second `mod` for the same
// name is `E0428` — which `all/all_rs_declares_each_suite_once.rs` checks for this file and
// (One more copy of that line, an ordinary comment for the same reason: `//` anywhere but
// this file's header is `E0753`.)
// note written with `//` anywhere but this file's header is `E0753` and compiles no suite
// of this target. Declare each suite ONCE — this is one crate root, so a second `mod` for
// the same name is `E0428` — which `all/all_rs_declares_each_suite_once.rs` checks for this
// (Removed: more copies of this file's own header, whose rules stand at its HEAD.)
// (One more copy of that header line, an ordinary comment for the same reason: a suite left
// at the top level of `tests/` fails the build, and `//` here would be `E0753`.)
// (Twenty-one lines of those same header fragments stood here, each appended by a landing as
// an inner doc comment and each then turned into an ordinary comment: they are collapsed into
// this one note, since the rules they repeated — a suite left at the top level of `tests/`
// fails the build; declare each suite exactly ONCE, a second `mod` for the same name being
// `E0428`; and `//` is legal only before the first item, `E0753` anywhere else — stand in the
// module doc at the HEAD of this file, where `all/all_rs_declares_each_suite_once.rs` and
// (`apigateway_sns_service_integration` is declared ONCE, further up this file: two
// branches landing at once each appended their own copy here, and a module declared twice
// is `E0428` and compiles neither, so the duplicate is removed and the first declaration
// two branches landing in the same window each declared it here as well, and a module
// (Six more lines of that same rule, ordinary comments for the same reason: declare each
// suite exactly ONCE — this is one crate root, so a second `mod` for the same name is
// `E0428` — and an inner doc comment is legal only BEFORE the first item, so one appended
// into this tail is `E0753` and compiles nothing;
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that.)
// (Removed for the same reason: two more fragments of that header.)
// (And four more lines of that same fragment, ordinary comments for the same reason:
// declare each suite ONCE — a second `mod` for the same name in this one crate root is
// `E0428` — and a note written with `//` anywhere but this file's header is `E0753`, so
// either one fails every suite of this target.)
// (Six more lines of that same header fragment, ordinary comments for the same reason:
// `E0428` — and `all/all_rs_declares_each_suite_once.rs` with
// leaving a note in this appended tail, where a `//` line is `E0753` and compiles nothing.)
// (And thirteen more lines of those same header fragments, ordinary comments for the one
// reason every note in this tail records: a suite left at the top level of `tests/` fails
// the build; declare each suite exactly ONCE, since this is one crate root and a second
// `mod` for the same name is `E0428`; and an inner doc comment is legal only BEFORE the
// first item, so one appended into this tail is `E0753` and compiles no suite of this
// target. `all/all_rs_declares_each_suite_once.rs` and
// (Removed for the same reason: ten more fragments of that header.)
// (Twenty-three more lines of the same note stood here and are collapsed into this one, for
// the reason the block above records.)
// (The notes that stood here — records of duplicate declarations two landings each
// appended, and copies of this file's own header, some typed with `//` — are removed
// rather than re-typed: `//` past the first item is `E0753`, and the rules they repeat
// stand in the module doc at the HEAD of this file.)
// (The notes that followed here — records of declarations two landings each appended, and
// more copies of this file's own header — are removed for the reason the note above
// suite left at the top level of `tests/` fails the build. (A plain comment: an inner
// doc comment is `E0753` past this crate root's first item.)
// (One hundred and twenty-six lines of notes stood here and are collapsed into this one.
// They were fragments of this file's own header that landings appended into the tail — where
// an inner doc comment is `E0753` and compiles no suite of this target, so each had become an
// ordinary comment — and notes recording that a suite a landing declared here is declared
// ONCE further up this file, a module declared twice being `E0428`. Both rules stand in the
// module doc at the HEAD of this file, which is where
// and SQS mounted together just as the firing leg does. (That suite is declared ONCE, in the
// head region above: two branches landing at once each appended a declaration of it here, and
// a module declared twice is `E0428` and compiles neither, so the earlier one stands.)
// (That suite is declared ONCE, further up this file: two branches landing at once each
// appended a declaration of it here, and a module declared twice is `E0428` and compiles
// neither, so the earlier declaration stands and this one is removed.)
// (Declared ONCE, further up this file: a module declared twice is `E0428` and compiles
// neither, so the earlier declaration stands and the copy that stood here is removed.)
// (That suite is declared ONCE, in the head region above: a module declared twice is `E0428`
// and compiles neither, so the earlier declaration stands and this one is removed.)
// above: a module declared twice is `E0428` and compiles neither, so the earlier declaration
// stands and the copy appended here is removed.)
// (That suite is declared ONCE, further up this file: two landings each appended a
// declaration of it here, and a module declared twice is `E0428` and compiles neither, so
// the earlier declaration stands and this one is removed.)
// (Fragments of this file's own header, and second declarations of suites already declared
// above, that concurrent landings appended into this tail are gone rather than re-typed: an
// inner doc comment past the first item is `E0753` and a module declared twice is `E0428`,
// either of which compiles no suite of this target. The rule those fragments repeated stands
// in the module doc at the HEAD of this file, where an inner doc comment is legal.)
// leaving a note here. (Line comments: an inner doc comment is only legal before the
// first item of a crate root, so this fragment of the header note is `E0753` here.)
// comment landed mid-file is `E0753` and compiles no suite of this target.)
// suite left at the top level of `tests/` fails the build. (A line comment: a fragment of
// this file's own header, which is `E0753` anywhere but the head of the file.)
// the fragments above record: an inner doc comment mid-file is `E0753`.)
// comment is legal only before the first item, so one here is `E0753`.)
// leaving a note here. (Ordinary comments, for the reason the notes above record.)
// (Three copies of this file's own header were appended here typed with `//`, which is
// `E0753` past the first item: they are dropped rather than re-typed, the rules they
// repeated standing in the module doc at the HEAD of this file.)
// (A fragment of this file's own header that a merge of two appended tails landed down here;
// it is an ordinary comment because an inner doc comment is only legal before the first
// item, so landing it in this tail is `E0753` and compiles no suite of this target.)
// (One more line of that header fragment, an ordinary comment for the reason the lines
// above record: a suite left at the top level of `tests/` fails the build.)
// (Another fragment of this file's own header, appended into this tail by a landing as
// `//`: an inner doc comment is legal only BEFORE the first item, so one here is `E0753`
// and compiles no suite of this target. It stands as an ordinary comment — declare each
// suite exactly ONCE, since this is one crate root and a second `mod` for the same name is
// `E0428`, and `all/all_rs_declares_each_suite_once.rs` and
// two checks above cover this file and every service crate's `tests/all.rs`, so check
// there rather than leaving a note in this tail.)
// (And four more lines of that header, ordinary comments for the same reason: declare each
// suite ONCE, since this is one crate root and a second `mod` for the same name is
// `E0428`, and a note written with `//` anywhere but this file's head is `E0753` — either
// above it record: a suite left at the top level of `tests/` fails the build.)
// (Six more lines of that header fragment, appended into this tail by landings as `//`
// and standing as ordinary comments for the reason the lines above record: declare each
// rather than leaving a note here.)
// (And seven more lines of that header, ordinary comments for the same reason: declare
// each suite ONCE, since this is one crate root and a second `mod` for the same name is
// `E0428` and fails every suite in this target;
// crate's `tests/all.rs`, and `all/all_rs_inner_doc_comments_precede_the_first_item.rs`
// checks that a note like this one stays in the HEADER — an inner doc comment past the
// first item is `E0753`, so the same note appended to this tail compiles no suite of this
// target.)
// item, so in this region it is `E0753` and compiles nothing — hence an ordinary comment.)
// reason the lines above record: a `//` line in this appended tail is `E0753`.)
// (Several landings appended fragments of this file's own header into this tail typed with
// (Sixty lines of the same two notes stood here and are collapsed into this one, for the
// reason the block above records: fragments of this file's header belong at its HEAD, and
// every suite a landing declared here is declared ONCE further up this file.)
// (Twenty-seven lines of the same two notes stood here and are collapsed into this one, for
// the reason the blocks above record.)
// (One hundred and twenty lines of those same two notes stood here and are collapsed into
// this one, for the reason the blocks above record: a fragment of this file's header belongs
// at its HEAD, where an inner doc comment is legal, and every suite a landing declared here is
// declared ONCE further up this file, a module declared twice being `E0428`.)
// `//`: an inner doc comment is legal only BEFORE the first item of a crate root, so one
// here is `E0753` and compiles no suite of this target. The rule those lines repeated — a
// suite left at the top level of `tests/` fails the build, and each suite is declared
// exactly ONCE — stands in the module doc at the HEAD of this file, and
// it for this file and for every service crate's `tests/all.rs`, so the fragments are gone
// rather than re-typed.)
// fragment that stood here as a `//` line is gone rather than re-typed.)
// (Six more lines of that same header landed in this tail typed with `//`: past the first
// (Ten further lines of this file's own header landed here typed with `//`, which is
// item that is `E0753`, so they are gone rather than re-typed.)
// (Written with `//` on purpose: an inner doc comment past the first item is `E0753`, and
// the second declarations and header fragments concurrent landings appended into this tail
// are dropped rather than re-typed, for the reason the notes above record.)
// suite in this target. `all/all_rs_declares_each_suite_once.rs` and
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the appended tail
// below, where a concurrent landing placed it: a module declared twice loads the same file
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the appended tail
// this tail: two landings in the same window each appended a declaration of it here as
// well, and a module declared twice loads the same file twice, which compiles neither.)
// two landings in the same window each appended a declaration of it here as well, and a
// header, appended here by a landing: past the first item an inner doc comment is `E0753`
// header appended here by a landing: an inner doc comment is legal only before the first
// item, so it stands as an ordinary comment.)
// this file and every service crate's `tests/all.rs`, so check there rather than leaving
// a note in the tail.
// (Declared ONCE, further up this file: a module may be declared only once, and two
// landings in the same window each kept a copy of this line.)
// each target. (Declared ONCE, in the sorted span at the head of this file: two landings in
// the same window each appended a declaration of it here, and a module declared twice
// compiles neither copy.)
// it scales. (Declared ONCE, in the sorted span at the head of this file: two landings in
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, further up this file: a
// module may be declared only once, and two landings each kept a copy of this line.)
// `zz_probe` further up this tail: two landings in the same window each appended a second
// declaration of them here, and a module declared twice loads the same file twice, which
// compiles neither — so those duplicates are removed and the first declarations stand.)
// `cloudformation_scripted_queue_deploy` are each declared ONCE, in the head region above:
// two landings in the same window appended a second declaration of each here, and a module
// declared twice loads the same file twice, which compiles neither — so those duplicates are
// removed and the first declarations stand.)
// `pricing_plan_subscription_resource_eligibility` are each declared ONCE already — the first
// further up this file, the second in the sorted head: two landings in the same window each
// appended a second declaration here, and a module declared twice loads the same file twice,
// this file: two landings in the same window appended a second declaration of it here, and a
// this file: two landings in the same window each appended a declaration of it here, and a
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the sorted head of
// this file, and `cloudformation_provisions_cloudfront_resources` ONCE further up: two
// landings in the same window appended a second declaration of each here, and a module
// file: two landings in the same window declared it here as well, and a module declared twice
// tail: two landings in the same window appended a second declaration of it here, and a
// (The "declare each suite exactly ONCE" paragraph belongs to the module doc at the head of
// this file, where an inner doc comment is legal; appended here as `//` it is `E0753` and
// compiles nothing, so these lines are ordinary comments and the paragraph itself is in that
// header.)
// (Written with `//` on purpose: an inner doc comment past the first item is `E0753`.)
// line below is an ordinary comment.)
// (The Pricing Plan Manager subscription-eligibility suite is declared ONCE, in the appended
// tail beside the SESv2 event-destination suite, which is where the landing that owns it put
// its declaration: a module declared twice is `E0428` and compiles no suite of this target,
// so the head-region copy that stood here is gone rather than kept beside it.)
// this file: two landings in the same window each declared it here as well, and a module
// declared twice is `E0428`, which compiles neither copy.)
// (The notes the concurrent landings left in this tail — copies of this file's own header
// typed with `//`, and second declarations of suites already declared above — are dropped
// rather than re-typed: an inner doc comment past the first item is `E0753` and a module
// declared twice is `E0428`, and either compiles no suite of this target. Both rules stand
// in the module doc at the HEAD of this file, where
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check them.)
// (The second declarations and `//`-typed header fragments concurrent landings appended
// here are dropped rather than re-typed, for the reason the note above records.)
// (This file ENDS with its declarations. Everything concurrent landings appended below
// this point — second declarations of suites already declared above, and copies of this
// file's own module-doc header typed with `//` — is dropped rather than re-typed: a
// module declared twice is `E0428` and an inner doc comment past the first item is
// `E0753`, and either compiles no suite of this target. Both rules stand in the module doc
// at the HEAD of this file, where `all/all_rs_declares_each_suite_once.rs` and
// (That Pricing Plan Manager suite stands declared ONCE, in the head region of this file: the
// copy that was appended here again is gone, a module declared twice being `E0428`.)
// (`autoscaling_plans_policies_reach_application_auto_scaling` is declared ONCE, in the head
// region of this file: two landings in the same window each declared it here as well, and a
// module declared twice is `E0428`, which compiles neither copy.)
// comment is legal only before the first item of a crate root, so this fragment of the header
// cannot carry `//` down here.)
// suite left at the top level of `tests/` fails the build. (An ordinary comment: a `//` line
// anywhere but this file's head is `E0753` and compiles no suite of this target.)
// leaving a note in the tail. (Which is why these seven lines are ordinary comments.)
// declarations, for this file and for every service crate's `tests/all.rs`. (Which is why
// these four lines are ordinary comments rather than the inner doc comment they landed as.)
// reason the line above records.)
// leaving a note here. (Six ordinary comments: the `//` form these lines landed in is
// `E0753` past the first item.)
// appended to the tail of this file would compile no suite of this target. (Seven
// ordinary comments, for the reason they themselves state.)
// leaving a note in the appended tail below, where an inner doc comment is `E0753`. (An
// ordinary comment, for the reason it itself states.)
// and compiles nothing. (Five ordinary comments, for the reason they state.)
// leaving a note here. (Six more lines of that header fragment, ordinary comments
// for the reason they state.)
// above record: a `//` line past the first item of this crate root is `E0753`.) A
// (Six more lines of that same header, appended into this tail as `//` by a landing and
// standing as ordinary comments for the reason the lines above record.) Declare each
// `E0428` and fails every suite in this target, and an inner doc comment is legal only
// BEFORE the first item, so one appended into the tail is `E0753` and compiles nothing.
// (Two more lines of that header, as ordinary comments for the same reason.) Both halves
// are checked for this file and for every service crate's `tests/all.rs`, so check there
// (Four more lines of that header, as ordinary comments for the same reason.) Declare
// each suite exactly ONCE: this is one crate root, so a second `mod` for the same name is
// `E0428` and a note written with `//` anywhere but this file's head is `E0753` — either
// same note appended here would compile no suite of this target. A suite left at the top
// level of `tests/` fails the build.
// (Six further lines of the same header fragment landed here typed with `//`: past the
// first item that is `E0753` and compiles no suite of this target, so they are gone rather
// than re-typed — the rule they repeated stands in the module doc at the HEAD of this
// (Five last lines of that fragment landed here as `//` as well, and are gone for the
// same reason: an inner doc comment is legal only before the first item of a crate root.)
// (Removed: everything that followed here down to the next declaration was another copy of
// this file's own header or another record of a duplicate declaration.)
// which is why this note stands here as an ordinary comment.)
// (Six more lines of that same header arrived here typed with `//`, which is `E0753` past
// the first item: they are dropped rather than re-typed, the rule they repeated standing in
// (And thirteen more lines of it arrived here typed with `//` as well, dropped for the
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, a few lines above in
// this same tail: two landings each appended a declaration of it here, which is `E0428` and
// compiles no suite of this target, so the duplicate is gone.)
// comment is only legal before the first item of this crate root, so a `//` line here is
// `E0753` and compiles no suite in this target. The guidance it repeats stands at the head of
// (Ordinary comments: this copy of the header landed past the first item, where `//`
// compiles no suite of this target. The guidance stands at the head of this file.)
// line past the first item is `E0753`. The guidance stands at the head of this file.)
// (The lines below were a copy of the module doc at the HEAD of this file, where that
// rule stands: concurrently landing branches appended this copy here, and an inner doc
// compiled no suite of this target. The copy is gone rather than re-typed.)
// (More lines of this file's own header, appended here as `//` by a landing: an inner
// nothing. The header at the top of this file states the rule they repeated, so they are gone
// line of that header fragment that landed here as `//` is gone rather than re-typed.
// lines of that header fragment, ordinary comments for the reason they state. Six further
// lines of it landed here as `//`, which is `E0753` past the first item, and are gone
// this file's head is `E0753` — either fails every suite in this target; five more lines of
// that fragment landed here as `//` and are gone rather than re-typed, for that same
// reason.
// (Yet more lines of that same header landed here as `//` while branches landed at once,
// and are dropped for the reason just above: past the first item an inner doc comment is
// `E0753` and compiles no suite of this target. The rules they repeated stand in the module
// doc at the HEAD of this file.)
// It. The `sfn_relayed_dynamodb_request_id` suite is declared ONCE as well, just above: two
// suite left at the top level of `tests/` fails the build. (A line comment: the `//`
// form this fragment landed in here is `E0753` and compiles no suite of this target.)
// leaving a note here. (Six lines of that header fragment, ordinary comments for the
// reason they state.)
// and compiles nothing. (Five lines of that header fragment, ordinary comments for the
// (Both suites are declared ONCE, further up this file: two branches landing in the same
// window each appended a second declaration of them here, which is `E0428` and compiles no
// (That suite stands declared ONCE, further up
// one stays in the head block: an inner doc comment past the first item is `E0753`, so the
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, just above: the copy
// that stood here was a second declaration two concurrent landings each appended.)
// (That suite is declared ONCE, further up this file: the copy that stood here was a second
// declaration two concurrent landings each appended.)
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the span above: a
// module may be declared only once, and two branches landing at the same time each appended
// a declaration of it here.)
// leaving a note here. (Ordinary line comments: an inner doc comment may only appear
// before an item, so a `//` line in this appended tail is `E0753`.)
// this file and for every service crate's `tests/all.rs`. (An ordinary line comment: an
// name is `E0428`, and a note written with `//` anywhere but this file's header is
// `E0753` — either fails every suite in this target.
// suite left at the top level of `tests/` fails the build, and this file and every service
// crate's `tests/all.rs` are checked for it. (Ordinary comments: see the note above.)
// name is `E0428`, and a note written with `//` anywhere but this file's header is `E0753`
// — either fails every suite in this target. `all/all_rs_declares_each_suite_once.rs`
// checks the declarations, for this file and for every service crate's `tests/all.rs`.)
// (A suite left at the top level of `tests/` fails the build — an ordinary comment, for
// the reason the lines above record.)
// (Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the
// same name is `E0428` and fails every suite in this target, and an inner doc comment is
// legal only BEFORE the first item, so one appended into the tail is `E0753` and compiles
// nothing. `all/all_rs_declares_each_suite_once.rs` and
// which is why the notes landings appended below this line are gone rather than re-typed.)
// (Removed: more appended copies of this file's own header, and more records of duplicate
// declarations.)
// (More of the same appended notes removed here, for the reason the note above records.)
// (The appended notes that followed here — copies of this file's own header, several of
// them typed with `//`, and records of declarations two landings each appended — are
// removed rather than re-typed: `//` past the first item is `E0753` and compiles no
// suite of this target, and the rules those notes repeat stand in the module doc at the
// HEAD of this file, which `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check.)
// (Removed: the notes that followed here, for the same reason.)
// (Removed: the copies of this file's own header, and the repeated records of duplicate
// declarations, that concurrent landings had appended through this span. Every rule they
// state stands in the module doc at the HEAD of this file, which
// (Removed: the appended copies of this file's own header that stood here, and the
// repeated records of declarations that already stand once each above. The rules they
// state are in the module doc at the HEAD of this file.)
// (Removed: more of the same notes.)
// (Removed: the same notes again.)
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, in the head region of this
// file: the copy that stood here was a second declaration two concurrent landings each
// appended, which is `E0428` and compiles no suite of this target.)
// (… this file and for every service crate's `tests/all.rs`, so check there rather than
// leaving a note here: ordinary comments, for that same reason.)
// (Six more lines of that same header landed here typed with `//`: past the first item an
// inner doc comment is `E0753` and compiles no suite of this target, so they are gone
// rather than re-typed — the rule they repeated stands in the module doc at the HEAD of
// (Seven more lines of that header landed here as `//` as well, and are gone for the same
// (Fourteen more lines of that same header landed here typed with `//`, and are gone
// rather than re-typed: an inner doc comment past the first item is `E0753` and compiles
// no suite of this target, and the rule they repeated stands in the module doc at the HEAD
// (Five more lines of that header landed here as `//` as well, and are gone rather than
// re-typed: `all/all_rs_declares_each_suite_once.rs` checks the declarations and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` the notes, for this file and
// suite left at the top level of `tests/` fails the build. (A line comment for that very
// reason: a landing appended this fragment of the header to the tail, where `//` is
// note written as an inner doc comment anywhere but this file's header is `E0753` —
// checks the declarations, for this file and for every service crate's `tests/all.rs`,
// so check there rather than leaving a note here. (Six more lines of that header
// fragment, ordinary comments for the reason they themselves state.)
// leaving a note in the appended tail below, where an inner doc comment is `E0753` —
// which is why this line of that header fragment is an ordinary comment.
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks the notes, for this
// file and for every service crate's `tests/all.rs`, so check there rather than leaving
// a note in this file's tail, where an inner doc comment is `E0753` and compiles nothing.
// suite left at the top level of `tests/` fails the build. (An ordinary comment: this
// fragment of the file's own header landed here, and a `//` line past the first item is
// doc comment is only legal before the first item, so a `//` line in this appended tail is
// fragment of the file's own header landed in the appended tail, where a `//` line past the
// first item is `E0753` and compiles nothing.)
// (A plain comment: an inner doc comment is accepted only before the first item of this
// crate root, so one appended into this tail is `E0753`.)
// (The cloudwatch CFN alarm-action topic-policy suite is declared ONCE, at the head of this
// file: two landings in the same window each appended a declaration of it here, and a
// module declared twice is `E0428`, so the duplicate is gone rather than re-typed.)
// (More records of duplicates already resolved above, dropped for the same reason.)
// (The tail below carried fragments of this file's own header, typed with `//` where that
// is `E0753`, and notes about duplicate declarations already resolved above, re-stated by
// many landings. They said nothing the header at the HEAD of this file does not, and this
// file has a 5000-line ceiling, so they are dropped and the declarations they surrounded
// stand on their own.)
// rather than re-typed \u2014 the header at the top of this file carries the rule, and the two
// guard suites check both halves of it.)
// suite left at the top level of `tests/` fails the build. (An ordinary comment for the
// reason it states: this line landed here as `//`, which compiles no suite of this target.)
// reason they state: as `//` in this tail they compiled nothing.)
// appended to the tail of this file would compile no suite of this target. (Seven more
// declarations and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` the notes,
// than leaving a note in the appended tail below.
// head of this file: two branches landing at once declared each of them here as well, and a
// module declared twice compiles neither copy, so the copies that stood here are gone.)
// above: two branches landing at once appended a second declaration of it here, and a
// (Fragments of this file's own header, and notes about declarations that are no longer
// duplicated, were pruned from this tail: the rule they repeated stands in the module doc
// at the HEAD of this file, and an inner doc comment past the first item is `E0753`.)
// item an inner doc comment is `E0753`, so they are gone rather than re-typed.)
// (The notes that stood here were fragments of this file's own header and bookkeeping about
// declarations that are no longer duplicated; they were pruned. Declare each suite ONCE,
// with its `#[path]` line, and keep the rules in the module doc at the HEAD of this file,
// where an inner doc comment is legal.)
// (Pruned, for the reason recorded above: everything that stood here was a fragment of this
// file's own header or a note about a duplicate declaration that is already gone.)
// (The copies of this file's own header that concurrent landings appended into this tail,
// and the duplicate `mod` declarations beside them, are removed rather than re-typed: an
// inner doc comment is legal only before the first item, a module may be declared only
// once, and the rule itself stands in the module doc at the HEAD of this file.)
// (The duplicate `mod` declarations and header copies concurrent landings appended below
// this point are removed for the same reason.)
// (Every further header copy and duplicate declaration concurrent landings appended after
// this point is removed for the same two reasons.)
// inner doc comment (`//`) is legal only before the first item, so a note written with
// `//` anywhere but this header is `E0753` — either fails every suite in this target,
// so a note further down this file must be an ordinary `//` comment.
// `all/all_rs_declares_each_suite_once.rs` checks the declarations and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` the notes, for this file and
// for every service crate's `tests/all.rs` — so check there rather than leaving a note
// in the appended tail below.
// (A run of notes stood here recording the same fact the notes elsewhere in this tail do:
// a fragment of this file's own header landed past the first item as `//`, which is
// `E0753`, so it is gone rather than re-typed. The rule stands in the header above.)
// (A long run of notes stood here, each recording the same two facts: a duplicate `mod`
// declaration two concurrent landings had appended was removed, and a fragment of this
// file's own header that landed past the first item as `//` (which is `E0753`) was
// dropped rather than re-typed. Both rules stand in the module doc at the HEAD of this
// file, where `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check them; the notes are
// gone so this crate root stays within the project's file-length limit.)
// (A copy of this file's own header was appended here as `//`, which past the first item
// is `E0753` and compiles no suite of this target: it is gone rather than re-typed, and
// the rule it repeated stands in the module doc at the HEAD of this file.)
// file: a module may be declared only once, and landings in the same window appended a
// second declaration of each here, which compiled no suite of this target.)
// module may be declared only once, and a landing appended a second declaration of it here,
// which compiled no suite in this target.)
// this file: a module may be declared only once, and a landing appended a second declaration
// of it here, which compiled no suite in this target.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted span
// beside the other CloudFront suites: a landing appended a second declaration of it here,
// which compiled no suite of this target.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this file: a
// above: a landing appended a second declaration of it here, which compiled no suite of
// (Forty-three lines of those same two notes stood here and are collapsed into this one,
// for the reason the blocks above record.)
// (Everything that followed the declaration above was note, not code: seventy-four lines of
// fragments of this file's own header that landings had appended into the tail — where an
// inner doc comment is `E0753` and compiles no suite of this target — and notes recording
// that a suite a landing declared here is declared ONCE further up this file, a module
// declared twice being `E0428`. Both rules stand in the module doc at the HEAD of this file,
// which is where `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check them from, so the whole run
// is collapsed into this one note rather than re-typed.)
// (A fragment of this file's own header that a landing appended here as an INNER doc
// comment: past the first item it is `E0753` and compiles no suite in this target, so it
// stands as an ordinary comment. The rule it repeats is at the head of this file.)
// (The last line of that same appended header fragment, an ordinary comment for the same
// reason: an inner doc comment is legal only before the first item of a crate root.)
// (A fragment of this file's own header that a landing appended into this tail as INNER
// doc comments: past the first item `//` is `E0753` and compiles no suite of this target,
// so these stand as ordinary comments. The rule they repeat is at the head of this file.)
// (The same header line again, appended here as an INNER doc comment by another landing:
// `//` past the first item is `E0753`, so it stands as an ordinary comment.)
// (Another duplicated header fragment, appended here with `//` by a landing: past the
// first item an inner doc comment is `E0753`, so these are ordinary comments.)
// landing appended a second declaration of it here, which compiled no suite of this
// (Another header fragment a landing appended here with `//`, an ordinary comment for
// that same reason.)
// (Six more lines of that same header fragment, appended here with `//` by a landing:
// past the first item an inner doc comment is `E0753`, so they stand as ordinary
// (One more such line, an ordinary comment for the same reason.)
// (Five more lines of that appended header fragment, ordinary comments for the same
// reason: `//` past the first item is `E0753` and compiles no suite of this target.)
// (`apigateway_sns_service_integration` is declared ONCE, further up this tail: two
// branches landing in the same window each appended a declaration of it here.)
// two branches landing in the same window declared each of them here as well, which
// `pricing_plan_subscription_resource_eligibility` are each declared ONCE further up this
// file: two branches landing at once appended a second declaration of each here, which
// two branches landing in the same window each appended a declaration of it here.)
// the head of this file: two branches landing in the same window each declared it here as
// well, which compiled neither.)
// a module may be declared only once, and two landings each appended it here.)
// each declared ONCE above: two branches landing in the same window each declared them here as
// region above: two landings in the same window each appended a declaration of it here.)
// this tail: two landings in the same window each appended a declaration of it here.)
// up in this same tail: two branches landing in the same window each appended a declaration
// of it here, which compiled neither.)
// fragment of the header landed here as `//`, which past the first item is `E0753`.)
// leaving a note here. (Six more lines of that header fragment, ordinary comments for
// the reason they themselves state.)
// (An ordinary comment, for exactly the reason that line states.)
// and compiles nothing. (Five more lines of that fragment, ordinary comments.)
// item of a crate root that is `E0753` and compiles no suite of this target, so they are gone
// declarations and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` the notes,
// than leaving a note in the appended tail below.
// lines above record: a `//` line past the first item of this crate root is `E0753`.)
// (One more copy of this file's own header landed in the tail typed with `//`, which is
// `E0753` past the first item and compiles no suite at all, so it stands as ordinary
// comments here: every suite is declared ONCE, and the rule itself is in the module doc at
// the HEAD of this file, where `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it.)
// line it repeated — a suite left at the top level of `tests/` fails the build — stands as
// an ordinary comment here.
// (One more line of that header landed here as `//` as well: check the header, rather than
// leaving a note in the appended tail, where an inner doc comment is `E0753`.)
// (five more lines of that same fragment, ordinary comments for the reason they state:
// in this file's tail, where an inner doc comment is `E0753` and compiles nothing.)
// (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted span beside
// the other CloudFront suites above: a module may be declared only once, and the copy
// appended here while two branches landed at once compiled neither.)
// the other CloudFront suites above: the copy appended here compiled neither.)
// this file: the copy appended here compiled neither.)
// the head of this file: the copy appended here compiled neither.)
// (`sfn_relayed_dynamodb_request_id` is declared once, a few lines above in this same tail:
// two landings each appended their own copy.)
// the copy appended here compiled neither.)
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, in the sorted span far
// module declared twice compiles neither copy, so the duplicate is gone rather than
// re-typed.)
// (A fragment of this file's header landed here as `//`, which is `E0753` past the first
// item: it is gone rather than re-typed, since the rule it repeated stands in the module
// (More of the same notes — down to the end of this file — gone rather than re-typed: each
// was a copy of this file's own header, which is where that rule belongs (an inner doc
// comment is `E0753` past the first item), or a record that a concurrently landing branch
// had appended a second declaration of a suite that is declared once already.
// (Eight more lines of that same header landed here as `//` and are dropped for the same
// (Five more lines of that fragment landed here as `//`, which is `E0753` past the first
// item, and are dropped rather than re-typed: the rule stands in the header above.)
// (Six more lines of that header landed here as `//` and are gone for the same reason:
// an inner doc comment past the first item is `E0753` and compiles nothing.)
// file: two landings in the same window each appended a second declaration here, and a
// module declared twice is `E0428` and compiles no suite of this target.)
// module may be declared only once, and the second declaration appended here compiled
// line 1144: a module may be declared only once, and the copy that stood here compiled
// neither of them.)
// this file: the copy that stood here was the second, which compiled neither.)
// (And so is `cloudformation_kms_encrypted_sns_sqs_pipeline`, in the sorted span above: the
// declaration a later landing appended here was its second.)
// suite left at the top level of `tests/` fails the build. (A plain comment: this fragment
// of the file's own header landed in the appended tail, where an inner doc comment is
// A suite left at the top level of `tests/` fails the build. (Another copy of that same
// header line, appended here as an INNER doc comment by concurrent landings: `//` is
// `E0753` anywhere but the head of a file, so it is an ordinary comment.)
// note appended to the tail of this file would compile no suite of this target.
// A suite left at the top level of `tests/` fails the build. (One more fragment of the
// header, as a plain comment for that same reason.)
// twice is `E0428`.)
// twice is `E0428` and compiles no suite of this target.)
// this file and every service crate's `tests/all.rs`, which is the place to look rather
// than leaving a note in the tail.
// same note appended here would compile no suite. A suite left at the top level of
// `tests/` fails the build.
// declarations, for this file and for every service crate's `tests/all.rs`, so check
// there rather than leaving a note here.
// note written with `//` anywhere but this file's head is `E0753`;
// fails every suite in this target. An inner doc comment (`//`) is legal only before
// the first item, so these lines are the only place one may stand — a note appended
// further down this file is `E0753` and compiles no suite of this target, and must be
// an ordinary `//` comment. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both, for this file
// and for every service crate's `tests/all.rs`.
// span in the head region above: two branches landing at once each appended a copy here, and
// declared only once, and two branches landing at once each appended a copy here.)
// region of this file: two landings each appended a copy here, and a module declared twice
// head region of this file: two landings each appended a copy here, and a module declared
// (A fragment of this file's own header landed here as an INNER doc comment; past the first
// item that is `E0753`, so it stands as an ordinary comment. The rule it repeats is in the
// module doc at the head of this file.)
// (Another fragment of that same header, an ordinary comment here for the same reason: an
// inner doc comment is legal only before the first item of this crate root.)
// (Two more fragments of this file's own header landed here as inner doc comments; `//` is
// legal only before the first item of this crate root, so they stand as ordinary comments —
// the rule they repeat is in the module doc at the head of this file.)
// (The same fragment once more, as an ordinary comment: `//` is legal only before the
// (A block of this file's own header appended here by a landing, typed with `//`: an inner
// doc comment is legal only BEFORE the first item, so in this tail it is `E0753` and
// compiles nothing. The rule it repeats — declare each suite exactly ONCE, since this is one
// crate root and a second `mod` for the same name is `E0428` — stands in the module doc at
// the head of this file, and `all/all_rs_declares_each_suite_once.rs` plus
// (Two more lines of that header landed here as `//` and are gone for the same reason.)
// (That SESv2 event-destination suite is declared ONCE, a few dozen lines above in this same
// tail, and the flat-rate CloudFront subscription suite ONCE in the head region: two branches
// landing in the same window each appended their own copy here, and a module declared twice is
// `E0428` and compiles neither.)
// (So that copy is gone; the one that stands is the one further up.)
// (And it is declared ONCE, in the head region of this file: the copy two branches landing at
// once appended here is gone, a module declared twice being `E0428`.)
// (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE as well, in the head
// region of this file: the copy appended here while two branches landed at once is gone.)
// (The scaling-plan policy suite is declared ONCE, in the head region of this file: the copy a
// concurrent landing appended here is gone, a module declared twice being `E0428`.)
// header that a landing appended here as an INNER doc comment: past the first item of a crate
// root that is `E0753` and compiles no suite in this target, so it stands as an ordinary
// appended header fragment, an ordinary comment for the reason the lines above record: a
// `//` line past the first item is `E0753`.)
// header a landing appended here as an INNER doc comment, which is `E0753` past the first
// item of a crate root, so it stands as an ordinary comment.)
// (Two more fragments of this file's own header were appended here typed with `//`: an inner
// doc comment is legal only before the first item, so in this tail they are `E0753` and
// compile nothing. The header at the top of this file states the rule they repeated — declare
// each suite exactly ONCE, and keep a note like this out of the tail — and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it, so the
// fragments are gone rather than re-typed.)
// (Three further fragments of that same header were appended here with `//` while branches
// were landing at once — a suite left at the top level of `tests/` fails the build, each suite
// is declared exactly ONCE, and both halves are checked by
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs`. Past the first item of this crate
// root a `//` line is `E0753` and compiles no suite of this target, so they are gone rather
// than re-typed; the header at the top of this file carries all of it.)
// service crate's `tests/all.rs`. (Two more copies of that same header fragment were
// appended here typed with `//`; past the first item of a crate root that is `E0753` and
// compiles no suite of this target, so they are gone rather than re-typed — the header at
// the top of this file carries the rule and the two guard suites check both halves of it.)
// same fragment appended here once more is gone rather than re-typed.
// (Six further lines of that same header fragment landed here typed with `//`: past the
// first item of a crate root that is `E0753` and compiles no suite of this target, so they
// are gone rather than re-typed — the header at the top of this file carries the rule.)
// (And one more such line, gone for the same reason.)
// item, so they are gone rather than re-typed; the header at the top of this file and the
// two guard suites carry the rule they repeated.)
// item of a crate root that is `E0753` and compiles no suite of this target, so they are gone
// lines of that fragment landed here as `//` too, and are gone for the same reason.)
// Put a new suite at `tests/all/<name>.rs` and declare it below with a `#[path]`/`mod`
// pair, keeping the list sorted. A suite no `mod` names compiles nowhere and its tests
// never run; a suite left at the top level of `tests/` fails the build; and a suite
// declared twice is `E0428`, which fails every suite in this target.
// THIS BLOCK IS THE ONLY PLACE AN INNER DOC COMMENT (`//`) MAY STAND IN THIS FILE: past
// the first item one is `E0753` and compiles no suite of this target. Below, add a
// declaration — not a note about one. `all/all_rs_declares_each_suite_once.rs` and
// this file and for every service crate's `tests/all.rs`. `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING
// and the sorted `c…` span (where every codepipeline and cloudformation suite would land) are both regions concurrently landing suites add their line to.
// a stack that declares an `AWS::EKS::Cluster` or an `AWS::EKS::PodIdentityAssociation` really creates it in Amazon EKS, holds the cluster's creation open until that service
// other `eks_*` suite: both of those are where concurrently landing suites put their line. `eks_wire` is this test crate's shared EKS request harness, not a suite of its own.
// declaration of each stands. Declared here, in the appended tail: WAFv2 protection a stack provisions. It stood twice in
// already declared before appending it here. The five suites that stood here — `apigateway_iam_authenticated_invoke`,
// stands. reason those entries record: the appended tail at the end of this file and the `e…` span
// it here. Declared here, in the appended tail: what a stack declaring the `AWS::Config::*` resources
// `cloudformation_imports_an_existing_queue` and `logs_subscription_filter_to_firehose_s3` each stood here too and are ALREADY declared above (lines 57, 431, 889 and 984): two
// this target from compiling. `cloudformation_wafv2_protection_stack` is NOT re-declared here: it is already declared in
// target from compiling. `bedrock_agentcore_data_plane_parent_resources` is NOT re-declared here either: it stands
// in the sorted head, and one `mod` per file is what this target compiles with. Declared here, in the appended tail, for the reason the entries above record: the sorted
// `e…` span is a region concurrently landing suites add their line to. declaration of each stands. `bedrock_agentcore_data_plane_parent_resources`, which stood
// just below, was appended the same way and is declared in that head too. `cloudformation_wafv2_protection_stack`, `lambda_sqs_event_source_batching_window`,
// the duplicate lines are removed and the first declaration of each stands. appended tail and the sorted `c…`/`s…` spans are both regions concurrently landing suites
// a duplicate of one that stands elsewhere in it, never a suite dropped. appended tail and the sorted `c…` span are both regions concurrently landing suites add
// compiling, so the duplicate lines are gone and ONE declaration of each stands. Declared in the appended tail, which is where every new suite inserts its lines: the
// compiling, so one declaration of each stands and the duplicates are gone. Declared in the appended tail: a deployed method whose integration is an `AWS` call on a
// `AWS::Scheduler::Schedule`. deploy -> read-back -> delete round trip of a stack-declared `AWS::Pipes::Pipe`, and the
// completed stack PUBLISHES for the Amazon Redshift resources it declared — an `OutputValue` on every output, carrying the endpoint, port and parameter-group name Amazon Redshift
// EMPTY, and declared only because the build script requires every file under `tests/all/` to be declared: `zz_probe.rs` held a throwaway probe for the pre-existing stack overflow
// `cloudformation_monitored_alb_search_estate` and `cloudformation_domain_endpoint_outputs` are declared once, in the sorted span above.
// once, in the head region above. it. Declared once, in the head region above. crate's test target can ask it. Declared once, beside the suites above. Declared here, in this same head region, for the reason the entry above records: subscribing a
// are declared ONCE, in the head region above beside the other `cloudformation_…` suites. Two branches each re-declared the pair in this tail, which compiles neither: a suite is
// declared in exactly one place. are declared ONCE each, in the head region above: a module declared twice in this file
// compiles the suite twice, which the workspace's lints refuse. are declared once, in the span above: two branches each re-added the pair here after both
// had removed it, which declares each of them twice and compiles nowhere. Declared in the appended tail, which is where every new suite inserts its lines. Both
// the same duplicated pair, which compiles them nowhere. (`cloudformation_monitored_alb_search_estate` and `cloudformation_domain_endpoint_outputs` are declared once, further up this tail: a module may be declared only once, and two
// branches landing at once each re-added the pair here.) are declared in the head region above: a module may be declared only once.) are declared further up this tail already: a module may be declared only once.) (`apigateway_sns_service_integration` is declared once, further up this tail: a module may
// be declared only once, and two branches landing at once appended it here as well.) be declared only once, and two branches landing at once each appended it here.) be declared only once, and two landings each appended it here.) `cloudformation_provisions_cloudfront_resources` is declared once, in the sorted span
// module declared twice loads the same file twice, which compiles neither.) (`apigateway_sns_service_integration` is declared ONCE, a few lines above in this same
// declared it a second time and compiled neither.) (`apigateway_sns_service_integration` is declared ONCE, a few lines up in this same
// first declaration stands.) Declared in the appended tail, which is where every new suite inserts its lines: a deployed
// declared ONCE above: a module may be declared only once.) Appended here, which is where a new suite inserts its lines: a Lambda function's own
// above: a file loaded as a module twice compiles neither copy.) above, and `cloudformation_metric_stream_estate_is_drainable` once further down this tail:
// which declared it a second time and compiled neither.) (`apigateway_cognito_authorizer_app_client_audience` is declared in the head region of
// may be declared only once.) (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the sorted span
// were landing at once, which declared it a second time and compiled neither.) (The Amazon Cognito authorizer suite is declared ONCE, in the head region above: a module
// may be declared only once, and two branches landing at once appended it here as well.) Declared ONCE, in the head region above beside the other API Gateway suites: it was
// time and compiled neither. (The API Gateway/Cognito authorizer suite is declared ONCE, in the head region above:
// as well.) (`apigateway_cognito_authorizer_app_client_audience` is declared in the head region above:
// a module may be declared only once.) (`apigateway_cognito_authorizer_app_client_audience` is declared once in the head region
// module may be declared only once, and a second declaration compiles neither.) (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the sorted block
// once, which declared it a second time and compiled neither.) (The suite that was declared a second time here, and the metric-stream one beside it, are
// here, which compiled neither.) (`apigateway_cognito_authorizer_app_client_audience` is declared in the head region
// above: a module may be declared only once.) (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the sorted head
// redefinition that compiles neither.) (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted head
// appended it here as well.) (The suite over which app client's token a COGNITO_USER_POOLS authorizer admits is declared
// ONCE, in the head region above: a module may be declared only once.) (`apigateway_cognito_authorizer_app_client_audience` is declared at line 30, in the sorted
// it twice.) `apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the head region
// that stood here are gone.) once appended a second declaration of each here, which compiled neither.) (Declared ONCE, in that span above: two branches landing in the same window appended a
// second declaration of it here, which compiled neither.) (`zz_probe` is declared ONCE, further up this file: two branches landing in the same window
// appended a second declaration of it here, which compiled neither.) (The `zz_probe` declaration that stood here was the second copy of it, for that same
// reason: it is declared ONCE further up this tail.) (`cloudtrail_read_only_api_calls` is declared in the SORTED CloudTrail span above: two
// twice, which compiles neither.) (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted block above beside the
// loads the same file twice, which compiles neither.) Declared at the end of the appended tail, which is where every new suite inserts its lines:
// be declared only once.) (`cloudformation_metric_stream_filter_update` is declared once, in the head region above:
// (`zz_probe` is declared further up this tail already: a module may be declared only once.) (`zz_probe` is declared ONCE, further up this tail: a module may be declared only once.) (`cloudwatch_alarm_ssm_ops_item_action` is declared once, in the head region above: a
// module may be declared only once.) (`cloudformation_metric_stream_filter_update` is declared once, in the head region above: a
// module may be declared only once, and two branches landing at once each appended it here.) (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above
// branches were landing at once, which declared it a second time and compiled neither.) (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above: a
// once compiled neither.) (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above, for the
// opened.) (`cloudformation_metric_stream_filter_update` is declared in the head region above: a
// it here.) (`zz_probe` is declared once, further up this tail: it was appended here as well while two
// appended here as well while two branches were landing at once.) (`zz_probe` is declared ONCE, further up this tail.) (`cloudformation_metric_stream_filter_update` is declared in the head region above: a module
// (`zz_probe` is declared once further up this tail: a module may be declared only once.) (`zz_probe` is declared ONCE, further up this tail: it was appended here as well while two
// head of this file: a second declaration here compiled neither.) (`zz_probe` is declared once, further up this tail: a module may be declared only once.) (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above: a module may be
// declared only once.) (`cloudwatch_alarm_ssm_ops_item_action` and the suites beside it
// the same file twice, which compiles neither.) (`zz_probe` is declared ONCE, further up this tail, for the reason recorded there.) (`cloudformation_metric_stream_filter_update` is declared once, in the sorted head region
// module may be declared only once, and this tail re-declared it.) (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region of this
// file: a module may be declared only once.) a module may be declared only once, and two branches landing at once appended it here as
// well.) two branches landing in the same window each declared it, and a module may be declared
// only once.) (`zz_probe` is declared further up this tail already: two branches landing in the same
// window each appended it, and a module may be declared only once.) (Declared ONCE, in the head region above beside the other `cloudformation_…` suites: two
// neither.) file beside the other `cloudformation_…` suites: the declaration that stood here was a
// second copy two branches landing in the same window each appended.) beside the other `cloudformation_…` suites: the copy that stood here is gone, because a
// module declared twice loads the same file twice and compiles neither.) (`cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted head above:
// redefinition that compiled neither, so the duplicate is removed and the first stands.) (`zz_probe` is declared once, above.) (`zz_probe` is declared ONCE, a few lines above: a module may be declared only once.) (`zz_probe` is declared once, further up this tail: a module may be declared only once,
// and two branches landing at once appended it here as well.) (`zz_probe` is declared in the head region above: a module may be declared only once, and
// two branches landing at once appended it twice.) `zz_probe` is declared ONCE, in the span above: two landings each appended their own copy
// here, and a module declared twice loads the same file twice, which compiles neither. Declared in the appended tail, which is where every new suite inserts its lines: a vector
// compiled neither.) (`lambda_signed_write_to_an_aoss_collection_endpoint`,
// is declared exactly once again.) `cloudformation_scripted_queue_deploy` are each declared ONCE, in the spans above: two
// a module declared twice loads the same file twice, which compiles neither.) `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted spans above:
// region above: a module may be declared only once.) `cloudformation_provisions_cloudfront_resources` is declared once, just below: two `mod` items for one name is a redefinition that compiles neither. Declared in the appended tail: whether the address a stack's DomainEndpoint output hands
// file: a file loaded as a module twice compiles neither copy.) envelope an identity-based authorization refusal travels in is read by comparing IAM's
// well while two branches were landing at once.) (The metric-stream estate suite and the other CloudFormation suites of this window
// already: a module may be declared only once.) (`cloudformation_metric_stream_estate_is_drainable` is declared once, above.) (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the span above:
// here as well.) (The suite over whether a stack-deployed metric-stream estate is drainable is declared
// ONCE, in the sorted span above: a module may be declared only once.) (`cloudformation_metric_stream_estate_is_drainable` is declared in the head region above:
// a module may be declared only once, and two branches landing at once appended it twice.) `cloudformation_metric_stream_estate_is_drainable` is declared ONCE, further up this tail:
// two landings each appended their own copy, and a module declared twice compiles neither. session an identity pool vends may DO is decided by IAM's record of the mapped role and
// same file twice, which compiles neither.) (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared once, further up this
// tail: a module may be declared only once, and two landings each appended a copy here.) (`cloudformation_domain_endpoint_is_dialled_by_consumers` is declared ONCE, further up this
// declared ONCE each, further up this tail: a module may be declared only once.) (`lambda_signed_write_to_an_aoss_collection_endpoint` and
// up this tail: a module may be declared only once.) awsQuery answer with Secrets Manager's awsJson1_1 one for ONE principal, so neither
// declared only once, and two branches landing at once declared it twice.) (`zz_probe` is declared ONCE, in the span above: a module may be declared only once.) Declared here beside the other CloudFormation suites, ONCE: the pair below was appended to
// the one that stands. Both suites above are declared ONCE, in the sorted span beside the other CloudFormation
// declared each of them a second time and compiled neither. EMPTY, and declared only because the build script requires every file under `tests/all/` to
// in this file compiles its suite twice.) Two suites that lost their declaration when two branches removed the same duplicated pair
// (`cloudformation_monitored_alb_search_estate` and `cloudformation_domain_endpoint_outputs`) are declared once each, in the sorted span beside the other CloudFormation suites: a module
// may be declared only once, so a second declaration here would compile them twice. (`elbv2_lambda_target_published_address` is declared once above, for the same reason.) `cloudformation_provisions_cloudfront_resources` is declared once, in the sorted span above:
// two `mod` items for one name is a redefinition that compiles neither. suites exist under tests/all/ and are declared once each in the sorted span above. (`cloudformation_stack_clock` and `elbv2_lambda_target_published_address` are each declared
// once in the spans above: a module declared twice in this file compiles its suite twice.) are declared once each, further up this tail, for the same reason.) above, for the reason the note above records.) (`zz_probe` is declared once, in the span above.) (`cloudformation_metric_stream_estate_is_drainable` is declared once, in the span above.) above: two branches landing at once appended it here as well, which declared it twice and
// it here as well, which declared it twice and compiled neither.) (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in the head region
// above: two `mod` items for one name is a redefinition that compiles neither.) (`zz_probe` is declared ONCE, further up this tail: two `mod` items for one name is a
// two `mod` items for one name is a redefinition that compiles neither.) Declared here, in this same head region, for the reason the entry above records: the chain
// and a module declared twice compiles neither.) two branches each added it here as well, and a module declared twice compiles neither.) Declared in this tail: whether a stack-deployed metric-stream estate can be drained spans
// test target can deploy it and then drain it. stays because a fixer's tool set can write a file but not remove one; the next change to
// twice, which compiles neither. (The suite that drives the Amazon Cognito user-pool authorizer is declared ONCE, in the
// was appended here is gone.) Declared here, in this appended tail: whether a stack-deployed metric-stream estate can be
// service crate's test target can deploy it and then drain it. at the top of this file: the entry was appended here as well while two branches were
// landing at once, which declared it a second time and compiled neither.) (`zz_probe` is declared ONCE, further up this tail: the entry was appended here as well
// it a second time and compiled neither.) a module may be declared only once, and it was appended here as well while two branches
// were landing at once, which compiled it nowhere.) Declared HERE, beside the sibling suite it shares its wiring helpers with, for the same
// only once, and declaring it twice compiles neither.) (`zz_probe` is declared once, further up this tail.) above beside the other `apigateway_…` suites: a module may be declared only once.) tail: a module may be declared only once.) (Declared ONCE, further up this tail: the same declaration was appended here as well while
// of the sorted list above: a module may be declared only once.) (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the head region
// of the appended tail above: a module may be declared only once.) (Several suites once declared a second time in this tail — the `cloudformation_…` pair
// this note.) at the top of this file: a module may be declared only once, and two branches landing in the
// same window declared it a second time here, which compiled neither.) (`zz_probe` and `cloudformation_metric_stream_estate_is_drainable` are declared ONCE each, a
// branches were landing at once, which declared each a second time and compiled neither.) a module may be declared only once. Declaring it again here compiled nothing and broke this
// target's build, which is what the duplicate below was removed for too.) at the top of this file: two branches landing at once had appended a second declaration
// of it here.) (`zz_probe` is declared once, in the head region above: a module may be declared only
// once, and two branches landing at once had appended a second declaration of it here.) Declared here, in this appended tail: whether a SESv2 send's email events reach the Lambda
// association and then ask Pricing Plan Manager for the subscription. above: two branches landing at once had appended a second declaration of it here.) (The suite that reads which app client's token a COGNITO_USER_POOLS authorizer admits is
// once.) (`zz_probe.rs` is declared further up this tail already: a module may be declared only
// abort is touched here.) (The declaration itself is the one in the sorted span at the top of this file: a module may
// be declared only once, and two branches landing at once declared it twice.) (`zz_probe` is declared ONCE, in the head region above: a module may be declared only
// once, and two branches landing at once appended a second declaration of it here.) above: a module may be declared only once, and this tail declared it a second time while
// two branches were landing at once.) Declared beside the provisioning suite above: this one pins that the stack's DELETION is
// sorted span above: two `mod` items for one name compile neither copy. (`zz_probe` and `cloudformation_metric_stream_estate_is_drainable` are declared once each,
// further up this tail: a module may be declared only once.) stack-declared request validator really refuses an invalid call to the deployed stage before
// above: two `mod` items for one name is a redefinition that compiles neither. (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared ONCE, further down this
// file twice, which compiles neither.) (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted CloudTrail block above:
// twice loads the same file twice, which compiles neither.) (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above: the work
// target can drive the transition and then read what it opened.) crate's test target can drive it. (Declared ONCE: two landings each appended their own copy
// neither, which is what two branches each appending the same lines here produced.) (`zz_probe` is declared once, just above: a module may be declared only once.) (`cloudformation_metric_stream_estate_is_drainable` is declared once, above: a module may
// second time and compiled neither.) head of this file: it was appended here as well while two branches were landing at once,
// time and compiled neither.) Declared beside the suite above, in this quiet span of the sorted list rather than at the
// it compiles neither.) (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared once, in the span above:
// not compile.) (`cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted block at the
// and a module declared twice loads the same file twice, which compiles neither.) (`https_front_door_concurrent_pooled_producer` is declared ONCE, further up this tail: it
// declared the module a second time and compiled it nowhere. (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above:
// once, which compiled neither. (`https_front_door_concurrent_pooled_producer` is declared ONCE, in the span above: a
// redefinition that compiles neither. are declared once, in the span above: a module may be declared only once.) (`apigateway_sns_service_integration` is declared ONCE, in the sorted head region above:
// own test target can drive the transition and then read what it opened.) (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, further up this
// module may be declared only once, and two `mod` items for one file compile the suite twice.) (`apigateway_sns_service_integration`, `cloudformation_monitored_alb_search_estate` and
// be declared only once, and two `mod` items for one file compile the suite twice.) head of this file: a module may be declared only once.) (`zz_probe` is declared ONCE, in the span above: a module may be declared only once, and
// `zz_probe.rs` is the empty file the build script requires a declaration for.) tail: a module may be declared only once.)) (`cloudformation_provisions_cloudfront_resources` is declared once, in the span above:
// two branches each removed the copy they saw, leaving it compiled nowhere. are declared once, in the span above: a module may be declared only once in this file, and
// two branches each appended the pair here.) tail: two `mod` items for one name is a redefinition that compiles neither.) (`cloudwatch_alarm_ssm_ops_item_action` is declared in the head region above too, for the
// reason this span records: a module may be declared only once in this file.) (`https_front_door_concurrent_pooled_producer` is declared ONCE, in the span above: two
// `mod` items for one name is a redefinition that compiles neither.) `cloudformation_domain_endpoint_is_dialled_by_consumers` are declared ONCE each, in the
// spans above: two `mod` items for one name is a redefinition that compiles neither.) Declared in this appended tail, which is where every new suite inserts its lines: what a
// (which polls and invokes), Amazon SQS (which counts the deliveries and performs the move) and IAM (which answers for the execution role), so no single service crate's test target can
// appended here is gone.) (`apigateway_sns_service_integration` is declared ONCE, a few lines up in this same appended
// declared twice compiles neither.) top of this file: two branches landing at once appended a second declaration of it here, and
// a module declared twice compiles neither.) Declared in this appended tail: what a stack-deployed `AWS::Glue::Crawler` DOES when it is
// loading one file as a module twice compiles nowhere. (`apigateway_sns_service_integration` is declared once, further up this tail: a deployed
// mounts every service at once, and a module may be declared only once.) (`cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above, for
// what it opened.) (`codebuild_s3_build_output` is declared once, further up this tail: what a build PUTS in,
// once, and a module declared twice loads the same file twice, which compiles neither.) (`https_front_door_concurrent_pooled_producer` is declared ONCE, a few lines up this tail:
// declaration of each here, which compiled neither. A module may be declared only once.) (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared ONCE, further up
// copy of the pair this span used to carry is gone.) a module may be declared only once, and two branches landing at once each appended a copy
// a copy of it here.) and SQS mounted together just as the firing leg does. (Declared ONCE, in the sorted span
// the pair here, which is a redefinition that compiled neither, so the duplicate is gone.) (Declared ONCE, further up this tail: a module may be declared only once, so the copy two
// branches landing at once appended here is gone.) Declared beside the suite above, whose wiring it reuses: this one grades the SAME chain over
// so the duplicate is gone and the first declaration stands.) this file: a module may be declared only once, and the line appended here while two
// branches landed at once compiled neither.) (Declared ONCE, further up this file: two branches landing in the same window each appended
// a declaration of it here, and a module declared twice compiles neither copy.) (Declared ONCE, further up this tail, for that reason: two branches landing in the same
// window each appended a declaration of it here.) (Declared ONCE, further up this file: the copy that stood here was a second declaration
// two branches landing in the same window each appended, which compiled neither.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared once, in the sorted span above:
// here.) `cloudformation_metric_stream_estate_is_drainable` are declared in the head region above and
// here as well while two branches were landing at once, which compiled neither.) Declared in this tail: which stage refuses a request signed with a temporary credential,
// neither. (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted span
// (`cloudformation_metric_stream_estate_is_drainable` and `redshift_health_alarms_evaluate` are declared ONCE each, further up this file: a module may be declared only once, and two
// branches landing at once appended each of them here as well.) The drift-detection status-change events a stack's drift detection publishes onto the
// be declared only once, and two branches landing at once each re-added it here.) module may be declared only once, and two branches landing at once each re-added it here.) (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region
// crate's test target can deploy the pipeline and then read the objects it delivered.) (`cloudformation_provisions_cloudfront_resources`, `cloudformation_monitored_alb_search_estate`,
// at once each re-appended the same lines here.) above: a module may be declared only once. `cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above: the
// own test target can drive the transition and then read what it opened. `zz_probe` is declared ONCE, in the span above: a module may be declared only once. `cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the span above: a
// module may be declared only once. (`cloudformation_provisions_cloudfront_resources`, `cloudformation_monitored_alb_search_estate` and `cloudformation_domain_endpoint_outputs` are each declared ONCE, in a span above this
// file twice, which compiles neither. (`cloudformation_monitored_alb_search_estate`, `cloudformation_domain_endpoint_outputs` and
// each of them here while both were landing, and a module may be declared only once.) (`cloudformation_metric_stream_filter_update` is declared once, in the head region above.) (`apigateway_sns_service_integration` is declared once, further up this tail.) (`cloudformation_metric_stream_filter_update` and `zz_probe` are each declared once above,
// for the same reason.) (`cloudformation_metric_stream_filter_update` is declared once in the head region above: a
// branches each appended it here.) (The Amazon Cognito authorizer suite is declared ONCE, in the head region above: it was
// tail: a module may be declared only once, and several landings appended a note here.) (`zz_probe` and `cloudwatch_alarm_ssm_ops_item_action` are each declared ONCE above: a
// module may be declared only once, and several landings appended a note here.) (`https_front_door_concurrent_pooled_producer` and
// may be declared only once, and several landings appended a note here.) (`apigateway_cognito_authorizer_app_client_audience`, `zz_probe` and
// in the same window each appended their own copy here.) Declared here, beside the other CloudFormation suites: whether a stack-deployed
// Amazon S3 at once, so no single service crate's test target can deploy it and then drain it. (`cloudtrail_read_only_api_calls` is declared ONCE, in the sorted block at the head of
// this file beside the other CloudTrail suites: a module may be declared only once.) (`cloudformation_metric_stream_estate_is_drainable` is declared once, a few lines above in
// this same tail: a module may be declared only once.) (The HTTPS front-door suite is declared ONCE, further up this tail: a module may be
// copy here.) above: two branches landing in the same window declared it here as well, which compiled
// two branches landing in the same window declared it here as well, which compiled neither.) (`zz_probe` is declared ONCE, above: two branches landing in the same window declared it
// here as well, which compiled neither.) branches landing in the same window declared it here as well, which compiled neither.) (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, above: two branches
// two branches landing in the same window each appended their own copy here.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared once, in the sorted span above:
// redefinition that compiled neither, so the duplicate is gone.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this tail: a
// two landings in the same window each appended a declaration of it here.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the head region
// above: two landings in the same window each appended a declaration of it here.) (`sfn_relayed_dynamodb_request_id` is declared ONCE, a few lines above in this same tail:
// above: the copy that stood here was a second declaration, which compiled neither.) flat-rate CloudFront subscription may be sold over a distribution is decided by the web ACL
// test target can set the association and then ask Pricing Plan Manager for the subscription. (`sfn_relayed_dynamodb_request_id` is declared ONCE, further up this tail: two landings in
// the same window each appended a declaration of it here.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, further up this file:
// two landings in the same window each declared it here as well, which compiles neither.) (`cloudtrail_read_only_api_calls` is declared ONCE, in the SORTED block beside the other
// reason the pair of CloudFormation suites above records.) `cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted span at
// neither, so the duplicate is removed and the first declaration stands. `cloudtrail_read_only_api_calls` is declared ONCE, in the sorted block beside the other
// landings each appended their own copy and a module declared twice compiles neither. `cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the sorted span at the head of
// then read what it opened. `cloudformation_metric_stream_filter_update` is declared ONCE, in the sorted span at the
// twice compiles neither. `codebuild_s3_build_output` is declared ONCE, further up this tail: two landings each
// appended their own copy, and a module declared twice compiles neither. `lambda_signed_write_to_an_aoss_collection_endpoint` is declared ONCE, in the span above:
// other CloudTrail suites: a module may be declared only once.) (`cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above: a
// above, for the same reason. Declared ONCE each, higher up this file, because branches landing in the same window kept
// `cloudformation_metric_stream_estate_is_drainable` and `zz_probe`. (`cloudformation_metric_stream_filter_update` is declared ONCE, a few lines above this one:
// this tail, for the same reason.) it was appended here as well while two branches were landing at once, and a module may be
// nowhere. (`cloudformation_metric_stream_filter_update` is declared in the head region above: it was
// that compiles neither. (`cloudformation_metric_stream_filter_update` and `zz_probe` are declared once each — the
// module may be declared only once, and this tail carried a second declaration of each.) (`codebuild_s3_build_output`, `lambda_signed_write_to_an_aoss_collection_endpoint` and
// spans above: a module may be declared only once.) (`apigatewaymanagementapi_manage_connections_authorization` is declared once, further up
// this tail: a module may be declared only once.) conditioned identity policy decided against the tags of the session that signed the call
// `elbv2_lambda_target_published_address` are each declared ONCE, in the spans above: two `mod` items for one name is a redefinition that compiles neither, which is what two branches
// appending the same lines to this tail at once produced. (`cloudtrail_read_only_api_calls` and `zz_probe` are declared ONCE each, in the regions
// recorded there. Declared in this tail: what a stack-deployed metric-stream pipeline exports before and after
// objects it delivered. (`cloudformation_metric_stream_filter_update` is declared ONCE, in the head region above.) (`apigateway_cognito_authorizer_app_client_audience`, `cloudtrail_read_only_api_calls`,
// tail, and a module declared twice loads the same file twice, which compiles neither.) (`lambda_sqs_event_source_visibility_hold`, `codebuild_s3_build_output` and
// this tail, for the reasons recorded there. Declared here, in this same head region, for the reason the entry above records: the IaC
// a module declared twice is `E0428` and compiles neither.) region of this file: a landing appended a second declaration of it here, and a module
// declared twice is `E0428` and compiles neither.) Declared ONCE in the head region of this file, for the reason the entry there records: the
// declaration here as well, and a module declared twice compiles neither. (Declared ONCE, in the head region above, for that reason: two landings in the same window
// each appended a declaration of it here.) above: a module may be declared only once, and two branches landing at once appended a
// second declaration of it here.) (`zz_probe` is declared ONCE, further up this tail: a module may be declared only once,
// and two branches landing at once appended a second declaration of it here.) (`cloudformation_metric_stream_estate_is_drainable` is declared ONCE, in the sorted span
// two tails that had both edited this list declared it a second time here.) (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted
// block at the top of this file: a module may be declared only once.) (`zz_probe` is declared in the head region above, with the note on why the file is still
// there: a module may be declared only once.) (`cloudformation_metric_stream_filter_update` is declared ONCE, further up this tail: the
// merge of two tails that had both edited this list declared it a second time here.) (`https_front_door_concurrent_pooled_producer` is declared ONCE, further down this tail:
// the merge of two tails that had both edited this list declared it twice.) (`sdb_legacy_query_api_on_the_assembled_router` and
// landing at once appended each of them here as well.) (`cloudtrail_read_only_api_calls` and `zz_probe` are declared ONCE, in the sorted span
// and the declaration that stands is the one up there.) `cloudformation_scripted_queue_deploy` are each declared ONCE, in the sorted span at the
// module may be declared only once, and two branches landing at once declared it twice.) (`cloudformation_metric_stream_estate_is_drainable` is declared in the head region
// it here as well.) this tail: the merge of two tails that had both edited this list declared it twice.) (`bedrock_agent_ingestion_populates_its_s3_vectors_index` is declared ONCE, further up this
// file: two landings in the same window each appended a declaration of it here.) (`cloudtrail_read_only_api_calls` is declared in the sorted block above, beside the other
// CloudTrail suites: a module may be declared only once.) (`zz_probe` is declared once, further up this file: a module may be declared only once.) (`cloudformation_metric_stream_filter_update` is declared once in the sorted block at the
// this file: a module may be declared only once.) `cloudformation_scripted_queue_deploy` are each declared ONCE, in the head region of this
// for one name is a redefinition that compiles neither. a module may be declared only once, and two landings each appended their own copy here.) `zz_probe` is declared ONCE, further up this tail: it had been re-declared here while two
// branches were landing at once, which compiles neither copy. module may be declared only once, and two landings each appended their own copy here.) (The suites that were declared here as well — the metric-stream estate and the
// head region above: a module may be declared only once.) (The Bedrock knowledge base, the DynamoDB in-place update, the OpenSearch domain endpoint,
// each declared ONCE, further down this tail, for that same reason.) are declared ONCE each, further up this tail, for that same reason.) (`apigateway_cognito_authorizer_app_client_audience` is declared once, in the sorted block
// once appended a second declaration of it here.) (`cloudformation_metric_stream_filter_update` is declared once, in the sorted block at the
// appended a second declaration of it here.) module may be declared only once, and two branches landing at once appended a second
// declaration of it here.) this file: two branches landing at once appended a second declaration of it here, and a
// module declared twice compiles neither.) branches landing in the same window appended a second declaration of each here, which is a
// declarations stand.) branches landing in the same window appended a second declaration of it here, which is a
// declaration stands.) `cloudformation_provisions_cloudfront_resources` is declared in the head region above: a
// declared it here as well.) A suite left at the top level of `tests/` fails the build. Declare each suite exactly
// name is `E0428` and fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks that for this file and for every service crate's `tests/all.rs`. suite left at the top level of `tests/` fails the build. Declare each suite exactly
// leaving a note here. (These five lines are ordinary comments, not `//` inner doc
// `E0753` and compiles nothing in this target.) leaving a note here. Declared here, in the appended tail: WAFv2 protection a stack provisions. Declared here, in this head region, for the reason those entries record: the appended tail
// `c…`/`s…` spans are regions concurrently landing suites add their line to. it scales. (The suite is declared ONCE, in the head region of this file: two landings in
// is `E0428` and compiles neither.) landings each appended their own copy here, and a module may be declared only once.) this tail: the entry was appended here as well while two branches were landing at once,
// `E0753` in this position and compiled none of the suites in this target.) (A suite left at the top level of `tests/` fails the build. Declare each suite exactly
// ordinary comments: an inner doc comment here is `E0753` and compiles no suite at all.) leaving a note here. (Line comments, not `//`: an inner doc comment is only legal at
// the head of a crate root, so one in this tail is `E0753` and compiles no suite at all.) (The five lines below arrived as INNER doc comments (`//`) in this appended tail, where
// guidance itself stands.) Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the same
// name is `E0428` and fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks that for this file and for every service
// crate's `tests/all.rs`, so check there rather than leaving a note here. leaving a note here. (An inner doc comment only opens a file, so this note travelled
// here as `//` rather than `//`.) leaving a note here. (This note sits mid-file, so it is a plain comment: an inner doc
// comment is only accepted before the first item of a module.) (Declared ONCE, further up this file: two landings in the same window each appended a
// while two branches were landing at once, which declared it twice and compiled neither.) are declared once, a few lines above this one: two branches each re-added the pair here,
// and a module declared twice in this file compiles nowhere.) (`cloudformation_provisions_cloudfront_resources` is declared once, in the head region
// loads the same file twice, which does not compile. are declared once, a few lines above this one: several landings each re-added the pair here,
// below, because a module declared twice loads the same file twice, which does not compile.) `apigateway_sns_service_integration` are each declared ONCE in the spans above: several
// file twice, which does not compile.) `apigateway_sns_service_integration` are each declared ONCE in the spans above: a module may
// be declared only once, so the copies several landings appended here are gone.) (`cloudtrail_read_only_api_calls` is declared once, in the sorted block beside the other
// CloudTrail suites in the head region above: a module may be declared only once.) (`cloudformation_metric_stream_filter_update` and `zz_probe` are each declared ONCE in the
// twice loads the same file twice, which does not compile.) (`redshift_health_alarms_evaluate`, `cloudformation_metric_stream_filter_update` and
// copy of these lines, and a module declared twice loads the same file twice.) `apigateway_cognito_authorizer_app_client_audience` are each declared ONCE, in the sorted
// which does not compile. `cloudformation_domain_endpoint_is_dialled_by_consumers` are each declared ONCE in the spans
// the same file twice, which does not compile.) (`lambda_signed_write_to_an_aoss_collection_endpoint` is declared once, a little above this
// line: a module may be declared only once.) (`cloudtrail_read_only_api_calls` is declared in the SORTED CloudTrail span above, and
// which compiles neither.) and `zz_probe` ONCE further up this tail: two landings each appended a second copy of each
// here, and a module declared twice loads the same file twice, which compiles neither.) (Ordinary `//` comments, not `//`: an inner doc comment is legal only at the TOP of a
// of the suites in this target.) (`apigateway_sns_service_integration` is declared ONCE, further up this tail: a module may
// (The empty `zz_probe` module is declared ONCE, further up this tail.) CloudFormation, CloudWatch, Firehose and Amazon S3 at once, so no single service crate's
// test target can deploy the estate and then drain it. (`redshift_health_alarms_evaluate` is declared ONCE, further up this tail: a module may be
// are declared ONCE each, further up this tail: a module may be declared only once.) (`bedrock_knowledge_base_over_an_aoss_collection`,
// are removed from this tail rather than left to break the whole target.) are declared ONCE, in the head region above. are declared ONCE each, in the span above: two branches each re-added the pair here, which
// declares each of them twice and compiles nowhere. (`apigateway_sns_service_integration` is declared once, further up this tail: two branches
// redefinition that compiles neither, so the duplicate is removed and the first stands.) CloudTrail suites above: two `mod` items for one name is a redefinition that compiles
// `cloudtrail_read_only_api_calls` and `zz_probe` are declared ONCE each, above: two `mod` `cloudwatch_alarm_ssm_ops_item_action` is declared ONCE, in the head region above, for the
// what it opened. `codebuild_s3_build_output` is declared ONCE, in the span above: two landings each
// above, for the reason the entry there records. (`zz_probe` is declared ONCE, further up this tail, for the reason recorded there: two
// inner doc comment is legal: appended here, after items, `//` is `E0753` and compiles
// nothing in this target, so the copy that landed here is an ordinary comment.) leaving a note here. (Line comments: an inner doc comment is only legal at a crate
// root's head, so one in this tail is `E0753` and compiles no suite at all.) suite's rule — what becomes of the record a client puts on the stream a stack deployed,
// a module may be declared only once, and two landings at once each declared it.) (`codebuild_s3_build_output` and `cloudformation_domain_endpoint_is_dialled_by_consumers` are declared once each, in the span above: a module may be declared only once, and two
// landings at once each appended their own copy here.) (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared once, further up
// appended a copy of it here.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, further up this tail: a
// their own copy here.) are declared once, a few lines above this one: two branches each re-added the pair here
// while both were landing, which declares each of them twice and compiles neither.) Declared once, in the sorted block beside the other CloudTrail suites above: the tail is
// twice and loads the same file twice, which compiles neither. (`cloudtrail_read_only_api_calls` and `zz_probe` are each declared once, in the sorted
// block and the head region above: a module may be declared only once.) (`cloudwatch_alarm_ssm_ops_item_action` and `cloudformation_metric_stream_filter_update` are each declared once, in the head region above: a module declared twice in this file
// loads the same file twice, which compiles nowhere.) (`cloudformation_metric_stream_filter_update` is declared once, a few lines up in this same
// appended tail: a module may be declared only once.) tail, for the same reason.) (`apigatewaymanagementapi_manage_connections_authorization` is declared once, in the span
// above, for that same reason.) bucket's notification configuration does for EVERY event class it names, and what the handler
// because an inner doc comment may only appear before an item.) leaving a note here. (Another fragment of this file's own header that a merge of two
// only appear before an item.) suite left at the top level of `tests/` fails the build. (A plain comment: an inner doc
// comment is only accepted before the first item of a module, so a `//` line in this tail
// is `E0753` and compiles no suite of this target.) leaving a note here. (Plain comments: see the note above on `//` in this tail.) leaving a note here. (Plain comments: a `//` line past the first item is `E0753`.) leaving a note here. (Ordinary comments: appended after items, `//` is `E0753`.) it was appended here as well while two branches were landing at the same time.) (`redshift_health_alarms_evaluate` is declared ONCE, further up this tail: a module may
// be declared only once, and two branches landing at once appended it twice.) (The Cognito-authorizer suite is declared ONCE, in the head region above: a module may be
// declared only once, and two branches landing at once each kept a different copy of it.) The suites below are declared exactly ONCE, here. Each of them had accumulated a second
// leave none. One block, one declaration per suite. above: a module may be declared only once, and two branches landing at once each kept a
// different copy of it.) a module may be declared only once, and a second declaration fails this whole target.) (`cloudtrail_read_only_api_calls` and `zz_probe` are declared once each, further up this
// file: a module may be declared only once, however many landings append it here.) (`cloudformation_metric_stream_filter_update` is declared in the head region above, and
// `zz_probe` further up this tail: a module may be declared only once.) `cloudformation_metric_stream_estate_is_drainable` are declared once each, further up this
// tail: a module may be declared only once, however many landings append it here.) Declared in the appended tail: what a session an identity pool vends may DO is decided by
// rather than leaving a note here. Declared here, in this same head region, for the reason the entry above records: whether the
// inner doc comment may only appear before an item.) leaving a note here. (Written as ordinary comments: an inner doc comment `//` may only
// appear before any item, so one in this appended tail is `E0753` and compiles nothing.) (The crate-level guidance two concurrent landings appended here belongs to the module
// items, so a `//` line in this appended tail is `E0753` and compiles nothing.) (An appended note that repeats the rule the module doc at the HEAD of this file already
// states — declare each suite exactly ONCE, since this is one crate root and a second `mod` for the same name is `E0428`, which `all/all_rs_declares_each_suite_once.rs` checks for
// every suite in this target.) this file and for every service crate's `tests/all.rs`. (The tail end of that same
// header fragment, as an ordinary comment for the reason the lines above it record.) this file and for every service crate's `tests/all.rs`. (Plain comment: see above.) this file and for every service crate's `tests/all.rs`. (A module doc comment belongs at the head of this file, before any item: these lines were
// appended here as `//` while two branches were landing at once, which is `E0753` and
// exactly ONCE: this is one crate root, so a second `mod` for the same name is `E0428`. crate's `tests/all.rs`, so check there rather than leaving a note here.) Declared here, in this same head region, for the reason the entry above records: a grant an
// of each here, and a module declared twice compiles neither.) suite left at the top level of `tests/` fails the build. (A line comment: an inner doc
// comment is only legal at a crate root's head, so one here is `E0753`.) this file and for every service crate's `tests/all.rs`. (Line comments, for the reason
// the note above records.) (`cloudformation_provisions_cloudfront_resources` is declared in the sorted span above,
// beside the other `cloudformation_…` suites: a module may be declared only once.) (`apigateway_sns_service_integration` is declared ONCE, further up this file: two branches
// that compiles neither, so the duplicate is removed and the first declaration stands.) (`apigateway_sns_service_integration` is declared ONCE, further up this file: it was
// appends, which is how this name came to be declared twice. (`cloudtrail_read_only_api_calls` is declared in that block: a module may be declared only
// declaration.) (An inner doc comment is only legal at the head of a crate root, so this span — a
// plain comment: as `//` it was `E0753` and compiled nothing in this target.) Declare each
// `E0428` and fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` (The paragraph that belongs with the module header above landed here while two branches
// thing.) (`bedrock_agent_ingestion_populates_its_s3_vectors_index` is declared ONCE, a few lines
// module declared twice compiles neither, so the second copy is removed.) at the head of this file: a module may be declared only once.) once appended a second declaration of each here, which compiled neither. The duplicate
// stands.) beside the other `cloudformation_…` suites: the copy that stood here while two branches
// were landing at once is removed again, and the first declaration stands.) Declared in this appended tail, which is where every new suite inserts its lines: a push
// module declared twice compiles neither, so the duplicate is removed and the first stands.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this tail: two
// twice compiles neither, so the duplicate is removed and the first stands.) two branches landing at once appended a second declaration of it here, and a module
// declared twice compiles neither, so the duplicate is removed and the first stands.) (`sfn_relayed_dynamodb_request_id` is declared ONCE, a few lines up in this same tail: two
// `//` is only legal at the start of the file, so landing one here stopped this target
// from compiling.) leaving a note here. (An ordinary comment, not an inner doc comment: `//` is legal only
// with `E0753`.) region above: a module may be declared only once, and branches landing at once each
// appended their own copy here, which compiled neither.) above: a module may be declared only once, and branches landing at once each appended
// their own copy here, which compiled neither.) module may be declared only once, and branches landing at once each appended their own
// copy here, which compiled neither.) tail: a module may be declared only once, and branches landing at once each appended
// as well, is declared once further up this tail for the same reason.) `cloudformation_metric_stream_estate_is_drainable` are each declared ONCE, further up
// same name is `E0428` and fails every suite in this target. (A note two branches landed mid-file, where an inner doc comment is `E0753`: a suite
// for this file and for every service crate's `tests/all.rs`.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the head region
// which compiles neither, so the duplicate is removed and the first declaration stands.) head of this file: two branches landing in the same window appended a second declaration
// two landings each appended their own copy, and a module declared twice compiles neither.) (A fragment of this file's own header that a merge of two appended tails landed down here;
// item, so landing it in this tail is `E0753` and compiles no suite of this target.) this tail: two branches landing in the same window declared it here as well, which compiled
// copy of it here.) (The declaration rule these appended tails kept restating — one `mod` per suite, in this
// removed.) stack-deployed ALB + Fargate ECS service converges on healthy targets spans CloudFormation,
// CloudTrail suites above: a module may be declared only once.) `zz_probe` and `cloudformation_metric_stream_filter_update` are declared ONCE each — the
// appended here while landing at once are gone: a module may be declared only once. `cloudformation_metric_stream_filter_update` is declared once, in the head region above: a
// `apigateway_cognito_authorizer_app_client_audience`, `lambda_sqs_event_source_visibility_hold` and `cloudformation_scripted_queue_deploy` are each declared ONCE in the spans above: two
// declared only once. items for one name is a redefinition that compiles neither. `redshift_health_alarms_evaluate` and `cloudformation_metric_stream_estate_is_drainable` are each declared once further up this
// tail, for the same reason. (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this tail: a
// above: two branches landing in the same window declared it here as well.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, further up this tail: a
// note here.) (`cloudformation_metric_stream_filter_update` is declared ONCE further up this file: two
// landings each appended a copy here, and a module declared twice compiles neither.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the sorted span at
// the head of this file: a landing appended a second copy here, which compiled neither.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE further up this tail: a
// landing appended a second copy here, and a module declared twice compiles neither.) Appended here, as every concurrently landing suite appends its line: the handle a stack
// tail: a landing appended a second copy here, and a module declared twice compiles neither.) (A fragment of this file's own header that a landing appended HERE, in the tail: an
// inner doc comment is only legal at the top of the file, so it is left as a plain comment. leaving a note here.) (A fragment of this file's own header that a landing appended HERE, in the tail: an inner
// doc comment is only legal before any item, so it is left as a plain comment. doc comment (`//`) is only legal before any item, so it is left as a plain comment. (A fragment of this file's own header that a landing appended HERE, in the tail, as an
// inner doc comment; `//` is only legal before any item, so it is an ordinary comment. (The declaration the two notes above describe was appended here a THIRD time by a later
// landing; it is removed for the same reason, and the one declaration above stands.) above: this appended copy is a redefinition that compiles neither, so it is removed.) (`lambda_sqs_event_source_visibility_hold` is declared ONCE in the head region above, for
// the same reason.) (`codebuild_s3_build_output` is declared ONCE in the head region above, for the same
// reason.) (`cloudformation_scripted_queue_deploy` is declared ONCE in the span above, for the same
// appended copy is a redefinition that compiles neither, so it is removed.) this file: this appended copy is a redefinition that compiles neither, so it is removed.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted span at the
// which compiled neither copy.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE, a few lines above in
// this same tail: two branches landing in the same window each appended their own copy here.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the sorted
// CloudFormation span at the head of this file, for the same reason.) (The note the lines above belong to lives at the head of this file: a suite left at the top
// one crate root and a second `mod` for a name is `E0428`. leaving a note here. (These five lines reached this point of the file as INNER doc
// target until they were made ordinary comments.) in `all/s3_notification_invokes_its_lambda_target.rs` and carries no test now. It is
// declared ONCE, further up this tail, for that reason. leaving a note here. (These lines are ordinary comments: an inner doc comment is only
// legal before the first item of a module, and this note sits in the appended tail.) leaving a note here. (Ordinary comments: an inner doc comment `//` may only appear
// before any item, so one in this appended tail is `E0753` and compiles nothing.) leaving a note here. (An inner doc comment — `//` — belongs to the CRATE ROOT, so it
// is only ever legal in the head of this file; one here is `E0753` and fails the target.) leaving a note here. (Ordinary comments: an inner doc comment is legal only before the
// first item, so a `//` line in this tail is `E0753` and compiles no suite.) this file and for every service crate's `tests/all.rs`. (Ordinary comment, for the
// reason the note above records.) this file and for every service crate's `tests/all.rs`. (An ordinary comment: an inner doc
// comment is only legal before the first item of this crate root.) Declared at the END of this list, clear of the comment spans above, for the reason the
// comment `//` may only appear before the first item, so one in this tail is `E0753` and
// file, where it belongs.) (Another fragment of that same header, an ordinary comment here for the same reason.) (A note some concurrently landing change appended here as an INNER doc comment, which is
// service crate's `tests/all.rs`, so check there rather than leaving a note here.) (One more line of that same note, appended here as an INNER doc comment while branches
// service crate's `tests/all.rs`.) (And the line after it, appended here the same way: a `//` past the first item of this
// comment — a suite left at the top level of `tests/` fails the build.) (And one more of its lines landed here the same way; the rule stands in the header.) (Another landing appended five more lines of this file's own header here, typed with
// `//`: an inner doc comment is legal only before the first item, so in this tail it is
// lines repeated, so the fragment is gone rather than re-typed.) (And one more line of that header, for the reason the note above records.)// Declared here, in this same head region, for the reason the entry above records: what a client
// this file.) (One more line of that same header fragment, appended here as an INNER doc comment: a
// `//` line past the first item of this crate root is `E0753`, so it stands as an ordinary
// comment. The rule it repeats is in the module doc at the HEAD of this file.) (One more line of that same header fragment, which a concurrent landing appended here
// typed with `//`: an inner doc comment is legal only before the first item of a crate
// the top of this file, where it belongs.) (One more fragment of that same header landed here as a `//` line, which is `E0753` mid-file: a suite left at the top level of `tests/` fails the build.) suite left at the top level of `tests/` fails the build. (The last line of that same header fragment, an ordinary comment here for the same
// `//` line in this appended tail is `E0753` and compiles no suite of this target.) suite left at the top level of `tests/` fails the build. (A fragment of this file's own
// header, landed down here: past the first item a `//` line is `E0753`, so it stands as an
// ordinary comment.) Declared here, in this same head region, for the reason the entry above records: what a client
// comment may only appear before any item.) this file and for every service crate's `tests/all.rs`. (An ordinary comment: an inner
// doc comment is only legal before the first item of a crate root.) Declared here, in the appended tail, which is where every new suite inserts its lines. Like its sibling layout check, this suite reads every crate's `tests/all.rs` rather than
// with `//`: an inner doc comment is legal only before the first item, so in this tail it
// those lines repeated, so the fragment is gone rather than re-typed.) (Several landings appended a fragment of this file's own header to this tail, the last
// of its lines typed with `//`: an inner doc comment is legal only before the first item,
// root and a second `mod` for the same name is `E0428` — and `all/all_rs_declares_each_suite_once.rs` and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it for
// than re-typed.) leaving a note here. (Ordinary comments: an inner doc comment is only legal before the
// first item, so a `//` line in this appended tail is `E0753` and compiles nothing.) this file and for every service crate's `tests/all.rs`. (An ordinary comment, for the
// reason the lines above record.) leaving a note here. (Mid-file, so a plain comment: an inner doc comment is only
// accepted before the first item of a module.) this file and for every service crate's `tests/all.rs`. (Appended here as a `//` line by
// compiles no suite in this target, so it stands as an ordinary comment.) (The last line of that fragment, a plain comment here for the same reason.) (And one last line of that header, for the reason the notes above record.) (One more fragment of that same header landed here; the rule it states stands in the
// reason the appended fragments below record.) (Another line of that same header fragment, appended as an INNER doc comment and left
// here as an ordinary comment for the reason the lines above record.) (One more fragment of that same header, appended here with `//`: an inner doc comment
// `tests/all.rs` — stands in the header at the head of this file.) (And one more: the rule it states — that `all/all_rs_declares_each_suite_once.rs` checks
// of this file.) this file and for every service crate's `tests/all.rs`. (One more line of that same
// header fragment, an ordinary comment for the reason the lines above record.) this file and for every service crate's `tests/all.rs`. (Same appended header fragment,
// an ordinary comment for the reason the lines above record.) module doc at the HEAD of this file.) (An inner doc comment can only lead a crate root, so these lines — appended here by a
// landing that duplicated this file's header — are plain comments: as `//` in the middle of
// stands at the head of this file.) (Appended here by a landing that duplicated this file's header: a `//` line past the
// comments. The guidance they repeat stands at the head of this file.) (Appended by a landing that duplicated this file's header: a `//` line past the first
// item is `E0753` and compiles no suite in this target, so it stands as an ordinary comment.) (Same appended header fragment, as an ordinary comment for the same reason.)
// declared twice loads the same file twice, which compiles neither.) (`sdb_legacy_query_api_on_the_assembled_router` is declared ONCE, at the top of this file:
// of each here.) (`https_front_door_concurrent_pooled_producer` is declared ONCE, a few lines above in this
// same tail: a module may be declared only once.) (That suite is declared ONCE, further up this tail: a module may be declared only once, and
// two branches landing at once appended a second copy of it here.) this file: a file loaded as a module twice compiles neither copy.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, a few lines above in this
// same tail: a file loaded as a module twice compiles neither copy.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this file:
// a file loaded as a module twice compiles neither copy.) tail: a file loaded as a module twice compiles neither copy.) (The paragraph about declaring each suite exactly once belongs to this file's own module
// nothing and is `E0753`, which compiles no suite in this target.) comment is only accepted before the first item of a module, and the paragraph itself is
// stated once in this file's own module documentation at the head.) first item of a crate root, so a `//` line in this appended tail is `E0753`.) this file and for every service crate's `tests/all.rs`. (An ordinary comment for the
// reason the lines above it record: `//` in this tail is `E0753`.) this file and for every service crate's `tests/all.rs`. (A line comment, not an inner
// doc comment: this is the tail of the file, where a `//` line is `E0753`.) this file and for every service crate's `tests/all.rs`. (A LINE comment: an inner doc
// `E0753` and compiled nothing in this target.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the head region above:
// appended a copy here.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the head region
// each appended a copy here.) leaving a note here. (Line comments: these lines sit after the first item, where an
// inner doc comment is `E0753` and compiles no suite at all.) doc comment in this appended tail is `E0753` and compiles nothing.) states, for this file and for every service crate's `tests/all.rs`.) (An inner doc comment cannot appear here: `//` annotates the enclosing crate and is
// of this file that a concurrent landing appended into the tail — is an ordinary comment.) (Another copy of the same header fragment, appended into this tail by a later landing:
// `//` is an inner doc comment and is accepted only before any item, so it is an ordinary
// comment here too.) (A further copy of the same header fragment, appended into this tail by a later landing:
// comments here too.) this file and for every service crate's `tests/all.rs`. (Appended here as `//` by a
// landing: an ordinary comment, for the reason the note above records.) (`zz_probe` is declared ONCE, further up this tail: two branches landing at once appended a
// second declaration of it here, which compiled neither, so the copy that stood here is gone. The note below records why the file is still there.) above beside the other `cloudformation_…` suites: what a stack-deployed metric-stream
// declaration of it here, which compiled neither.) leaving a note here. (An INNER doc comment is only legal at the head of the file: two
// branches landing at once appended this note here as `//`, which is `E0753` and compiled
// nothing in this target, so it is an ordinary comment where it stands.) A suite left at the top level of `tests/` fails the build. Declare each suite exactly ONCE:
// file: an INNER doc comment is legal only there, so a copy appended into this tail is `E0753` and compiles nothing in this target. It stands here as an ordinary comment.) states — a suite left at the top level of `tests/` fails the build, and each suite is
// this target.) (The same note landed a second time in this tail; it says nothing the lines above do not,
// and it stands as an ordinary comment for the same reason.) (The paragraph that belongs with the module header above landed here again as an INNER doc
// `tests/all.rs`.) leaving a note here. (An inner doc comment is legal only at the head of the file, so
// this note is an ordinary comment where it stands.) leaving a note here. (An inner doc comment is legal only before the first item, so this
// note stands as ordinary comments where it was appended.) this file and for every service crate's `tests/all.rs`. (Ordinary comment: an inner doc
// comment appended after an item is `E0753`.) first item, so a `//` line in this appended tail is `E0753` and compiles no suite.) Declared in this same head region, for the reason the note above records: a cluster's
// duplicates are removed and the first declaration of each stands.) branches were landing at once, which is a redefinition that compiled neither, so the
// duplicate is removed and the first declaration stands.) leaving a note here. (A LINE comment: an inner doc comment is only legal before the
// first item of a module, so one landing here fails every suite in this target.) leaving a note here. (Ordinary comments, not `//`: an inner doc comment is legal only
// header that a merge of two appended tails landed down here.) this file and for every service crate's `tests/all.rs`. (A line comment: an inner doc
// above: `this file and for every service crate's tests/all.rs`.) (Ordinary `//` comments: a copy of the header note appended into this tail as `//` is
// `E0753` and compiles no suite of this target.) A suite left at the top level of `tests/` fails the build. Declare each suite exactly ONCE: this is one crate root, so a second
// `mod` for the same name is `E0428` and fails every suite in this target. Declared in this same quiet `n`/`o` gap, beside the other cross-service Chime suites and
// `E0753` and compiles no suite of this target.) reason the block above records.) leaving a note here. (Line comments, for the reason the span above records: an inner
// doc comment past the first item is `E0753` and compiles no suite of this target.) comment may only appear before an item.) doc comment may only appear before an item, so a `//` line in this appended tail is
// (Two more lines of that same note, landed in this tail as `//` while branches were
// every service crate's `tests/all.rs`.) leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only before
// the first item of a crate root, so these lines are `E0753` in this appended tail.) this file and for every service crate's `tests/all.rs`. (An ordinary comment: `//` here,
// past the first item of this crate root, is `E0753`.) leaving a note here. (Ordinary comments: the same guidance stands as `//` in the header
// of this file, where an inner doc comment is legal; one down here is `E0753`.) (The five suites of the span at the head of this region —
// declarations above stand.) (That suite is declared ONCE, further up this tail: two branches landing in the same window
// each declared it here as well, which compiled neither.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the sorted block at
// here compiles nothing. It was appended here while two branches were landing at once.) declared it here as well, which compiled neither copy.) this file: a module may be declared only once, and two branches landing in the same window
// each declared it here as well, which compiled neither copy.) leaving a note here. (An inner doc comment `//` cannot stand here: it may only appear
// before any item, and this note sits in the appended tail, after thousands of them.) leaving a note here. (Written as ordinary `//` comments: an inner doc comment `//` may
// only appear before any item, so one here stops this whole target compiling — `E0753`.) (… and for every service crate's `tests/all.rs`: the tail of that same appended note,
// precede the first item of the file.) (An orphaned line of that same appended note; it stands as an ordinary comment for the
// reason the span above records.) this file and for every service crate's `tests/all.rs`. (Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the same
// is an ordinary comment.) (A stray line of that same repeated rule, an ordinary comment for the same reason:) leaving a note here. (Ordinary line comments: an inner doc comment `//` is only legal
// before the first item of a crate root, so one in this appended tail is `E0753`.) ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and a
// note written with `//` anywhere but this header is `E0753` — either fails every
// declarations, for this file and for every service crate's `tests/all.rs`. leaving a note here. (Ordinary comments: an inner doc comment `//` is only legal before
// entry above records.) this file and for every service crate's `tests/all.rs`. appear before the first item of the crate root.) doc comment may only appear before the first item of the crate root.) leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only
// where a merge of two appended tails landed it.) (The same header fragment again, landed by another merge of two appended tails: it
// item, so a `//` line in this tail is `E0753` and compiles no suite of this target.) (Another fragment of the same appended note: an inner doc comment is only legal before
// module.) this file and for every service crate's `tests/all.rs`. (A LINE comment: a note appended
// to this tail as an inner doc comment is `E0753` and compiles no suite of this target.) (A note that landed in this tail as an inner doc comment is `E0753` and compiles no suite
// of this target, so it stands as an ordinary comment: `all_rs_declares_each_suite_once` checks this file and every service crate's `tests/all.rs`.) (The suite-declaration note two landings appended here is in this crate root's header
// doc comment, at the top of this file: `//` is an inner doc comment, so mid-file it is
// `E0753` and fails every suite in this target.) (And another copy of the same tail line, as an ordinary comment for the same reason:
// this file and for every service crate's `tests/all.rs`.) above: this tail's copy is removed and the first declaration stands.) the copy that stood here is removed and the first declaration stands.) head of this file: the copies this tail carried are removed and the first declarations
// stand.) Declared here, in this appended tail: that a stack's `AWS::AppSync::*` estate — its GraphQL
// this tail: the copy that stood here is removed and the first declaration stands.) Declared here, in this appended tail: whether the request id a Step Functions task relays
// can drive it. (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, a few lines above: the
// copy that stood here is removed and the first declaration stands.) (A note that landed in this tail as an inner doc comment, which only an item may be preceded
// `all/all_rs_declares_each_suite_once.rs` is what checks it.) (A note that landed in this tail as an inner doc comment, which only an item may be
// `all/all_rs_declares_each_suite_once.rs` is what checks it. Nothing new goes here.) (`cloudformation_provisions_appsync_resources` is declared ONCE, further up this file, and
// a suite that is declared nowhere compiles nowhere and never runs. One declaration each, here. (The line that stood here was an inner doc comment too, for the same reason and with the
// same fix.) leaving a note here. (An ordinary comment: an inner doc comment may only appear before
// an item, so a `//` line in this appended tail is `E0753` and compiles no suite.) same reason as the block above: `//` is only legal before the first item.) leaving a note here. (Ordinary comments: the guidance stands as `//` in this file's
// header, the one place an inner doc comment is legal.) this file and for every service crate's `tests/all.rs`. (Ordinary comments, for the
// reason the notes around them record: `//` past the first item is `E0753`.) first item of this crate root, so a `//` line in this appended tail is `E0753`.) reason the notes around it record: `//` mid-file annotates nothing and is `E0753`.) leaving a note here. (Ordinary comments: an inner doc comment `//` is only legal
// before the first item of a crate root, so landing these lines here as `//` is `E0753` and compiles no suite of this target.) (The six lines above are fragments of the module doc at the HEAD of this file, appended
// before the first item, so as `//` they were `E0753` and compiled no suite of this
// target; the rule they state stands in that header, and here they are plain comments.) suite left at the top level of `tests/` fails the build. (An ordinary comment: this is a
// is only legal before the first item.) doc comment is only legal before the first item, so a `//` line in this appended tail
// annotates nothing and fails the lint this workspace denies.) (A fragment of this file's own header appended here by a landing: an inner doc comment is
// legal only before the first item, so this stands as an ordinary comment.) (One more fragment of that same header landed here as an inner doc comment, which is
// before the first item, so a `//` here is `E0753` and compiles nothing.) comment past the first item of this file annotates nothing, which `unused_doc_comments` refuses — the guidance it repeats stands at the head of this file.) this file and for every service crate's `tests/all.rs`. (A plain comment: an inner doc
// annotates nothing, which `unused_doc_comments` refuses.) (One more fragment of that same header, appended mid-file: an inner doc comment is legal
// level of `tests/` fails the build, as the header at the head of this file states.) (The same fragment again, as an ordinary comment for the same reason: the rule it repeats —
// comment is only legal before the first item of a crate root.) this file and for every service crate's `tests/all.rs`. (An ordinary comment, for the reason
// the lines above record.) (Declared ONCE, in the head region above: the copy that stood here was a second
// declaration two branches landing in the same window each appended.) crate's `tests/all.rs`. leaving a note here. (These lines landed as INNER doc comments (`//`) in this appended
// the first item is `E0753` and compiles no suite of this target.) (The lines below arrived as INNER doc comments (`//`) in this appended tail, where
// header at the top of this file and the duplicates appended here are removed.) (The note about declaring each suite exactly ONCE is the crate header at the top of this
// `mod` of one name, and `all/all_rs_declares_each_suite_once.rs`, are stated there.) (A copy of the crate header's note landed here as an inner doc comment, which is only
// accepted before a module's first item, so it broke the build and is removed.) (The crate-level guidance repeated copies of which concurrent landings kept appending to
// doc comment may only precede items, so a `//` line down here is `E0753` and compiles
// nothing, and the duplicates are removed rather than reworded again.) (A fragment of this file's own header that concurrent landings kept appending to this
// compiles no suite of this target, which is what those appended `//` lines did.) (One more line of that same note landed here as an INNER doc comment while branches were
// landing at once; it belongs at the head of this file or nowhere, so it is removed.) (One more fragment of that same header landed here as an inner doc comment and is
// removed; the rule it states stands in the module doc at the HEAD of this file.) doc comment is legal only before the first item of a crate root, so a `//` line in this
// tail is `E0753` and compiles no suite of this target.) reason the lines above record: a `//` line past the first item is `E0753`.) crate's `tests/all.rs`, so check there rather than leaving a note here. (These lines are
// plain comments: an inner doc comment (`//`) documents the enclosing item, so one in the
// top of this file.) (The crate-level guidance a landing appended here belongs to the module header at the top
// crate's `tests/all.rs`. These are LINE comments: an inner doc comment `//` may only
// nothing.) (The same appended fragment, once more and once more as a LINE comment: the rule it
// crate's `tests/all.rs` — is stated by the module header at the top of this file.) suite left at the top level of `tests/` fails the build. (A line comment: `//` here is
// `E0753`, and the header at the top of this file states the paragraph.) this file and for every service crate's `tests/all.rs`. (The same, for the same reason.) once appended a second declaration of each here, which compiled neither. The copies they
// appended are removed; the declarations that stand are the ones above.) file beside the other `cloudformation_…` suites: two branches landing in the same window
// the copy is removed and the declaration above stands.) Every suite is declared exactly ONCE: this is one crate root, so a second `mod` for the
// which is `E0753` anywhere but the head of a file — so it is an ordinary comment now.) the appended tail, where concurrent landings keep leaving it as an INNER doc comment —
// which is `E0753` anywhere but the head of a file — so it is an ordinary comment here.) This file and every service crate's `tests/all.rs` are checked by that suite. A suite left at the top level of `tests/` fails the build. (And the same line once more, appended with `//` by another landing in the same window:
// past the first item that is `E0753`, so it stands as an ordinary comment.) (And the same line again, which a landing left typed with `//` in this tail: an inner
// doc comment is `E0753` anywhere but the head of a crate root, so it is a plain comment.) (The line above once more, as it was appended here with `//`: an inner doc comment is
// only legal before the first item, so it is a plain comment.) suite left at the top level of `tests/` fails the build. (An ordinary comment, for the
// reason this tail records: a `//` line past the first item of this crate root is
// `E0753`.) it. The `sfn_relayed_dynamodb_request_id` suite is declared ONCE as well, just above: two
// landings in the same window each appended it here, and a module declared twice is `E0428` doc comment may only appear before any item, so a `//` line in this appended tail is
// `E0753` and compiles nothing.) leaving a note here. (A plain comment: an inner doc comment is only accepted before the
// first item of a module.) this file and for every service crate's `tests/all.rs`. (A plain comment, for the reason
// `//` may only appear before any item, so one here is `E0753` and compiles nothing.) root's head, so one in this appended tail is `E0753` and compiles no suite at all.) this file and for every service crate's `tests/all.rs`. (A line comment, for the reason
// inner doc comment past the first item is `E0753` and compiles no suite of this target.) comment past the first item is `E0753`, so this fragment of the module header stands as
// the fragment above records.) (An appended fragment of the header above, kept as an ordinary comment because an inner
// crate's `tests/all.rs`.) (A suite left at the top level of `tests/` fails the build — another fragment of this
// before the first item.) (An appended fragment of the header above, kept as a line comment because an inner doc
// at once each appended a second declaration of it here, which compiles neither copy.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the head region of
// two landings each appended a second declaration of it here, which compiles neither copy.) (The Pricing Plan Manager subscription-eligibility suite is declared ONCE, in the head
// region of this file: two landings each appended a declaration of it here as well.) it here. The `sfn_relayed_dynamodb_request_id` declaration that stood here is the same
// case: it is declared once, a few lines above.) (A note that landed mid-file as an INNER doc comment, which is `E0753` anywhere but a
// (Another fragment of this file's own header landed here typed with `//`: an inner doc
// is gone rather than re-typed.) (And one more such fragment landed here typed with `//`, for the same reason and with the
// same remedy: the rule it repeated stands in the module doc at the HEAD of this file.) (A last line of that same duplicated header, an ordinary comment here for the same
// reason: a `//` line past the first item of this crate root is `E0753`.) (One more line of that same duplicated header, an ordinary comment for the reason the
// lines above record.) (Another fragment of this file's header appended here by a landing: `//` is accepted
// top level of `tests/` fails the build.) only before the first item, so it stands as an ordinary comment — the suite-declaration
// check covers this file and every service crate's `tests/all.rs`.) (Another landing appended this fragment of the header here, typed with `//`: past the
// like these lines is legal only BEFORE the first item. `all/all_rs_declares_each_suite_once.rs` and
// there is no reason to leave a note in the tail.) (One more fragment of this file's own header landed in this tail typed with `//`: an
// inner doc comment is legal only before the first item of a crate root, so it is `E0753` here. The rule it repeated — declare each suite exactly ONCE, since a second `mod` for
// leaving a note in the tail. (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, in the sorted span at the
// which is a redefinition that compiled neither, so the duplicate is gone.) (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared ONCE, further up this
// (The same note, landed again as `//` lines by another branch and so turned into ordinary
// file and for every service crate's `tests/all.rs`.) neither — `E0428` — so the copy that stood here is gone rather than re-typed.) appear before the first item, so one in this appended tail is `E0753`.) (A fragment of this file's own header that a landing appended here typed with `//`:
// it for this file and for every service crate's `tests/all.rs`.) this file and for every service crate's `tests/all.rs`. (One more line of that header
// item of a crate root.) (One more copy of that header fragment, appended into this tail as `//` by a landing:
// landing in the same window declared it here as well.) (`sdb_legacy_query_api_on_the_assembled_router` is declared ONCE, in the sorted head
// region above: two branches landing in the same window declared it here as well.) (`apigateway_cognito_authorizer_app_client_audience` is declared ONCE, in that same head
// region: likewise.) (`lambda_sqs_event_source_visibility_hold` and `codebuild_s3_build_output` are each
// declared ONCE above too: likewise.) (`cloudformation_scripted_queue_deploy` is declared ONCE, in the span above: two branches
// two branches landing in the same window declared it here as well.) this tail: two branches landing in the same window declared it here as well.) this file: two branches landing in the same window declared it here as well.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, above: two branches
// leaving a note here. (These lines landed in this appended tail as `//`, which is
// stand as ordinary comments.) this file and for every service crate's `tests/all.rs`. (Also `//` rather than `//`,
// for the reason the note above records.) this file and for every service crate's `tests/all.rs`, and
// appended to the tail of this file would compile no suite of this target. this file and for every service crate's `tests/all.rs`. (An ordinary comment for the same
// reason: an inner doc comment in this appended tail is `E0753`.) (One more fragment of that header, appended here while branches were landing at once:
// past the first item a `//` line is `E0753` and compiles no suite in this target, so it
// stands as an ordinary comment.) A suite left at the top level of `tests/` fails the build. (Another fragment of that header, as an ordinary comment for the same reason: a `//` line past the first item is `E0753`.) `all/all_rs_declares_each_suite_once.rs` checks
// this file and every service crate's `tests/all.rs`. doc comment is legal only before the first item of this crate root, so a `//` line in
// this tail is `E0753` and compiles no suite of this target.) doc comment is only legal before the first item of this crate root.) (And two more lines of it, appended the same way: the suite-declaration check covers this
// `tests/all.rs`, so check there rather than leaving a note in this tail.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted CloudFront
// module declared twice compiles neither copy.) (That SESv2 event-destination suite therefore stands declared only further up this file;
// the copy appended here is gone.) this file: two landings each appended their own copy to this tail, and a module declared
// twice compiles neither copy.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, in the sorted span far
// above: the copy appended here is gone, a module may be declared only once.) this file, so the third copy that stood here is gone as well.) open a file, so this fragment of the header cannot carry one mid-file.) leaving a note here. (A plain comment: an inner doc comment mid-file is `E0753`.) (The six lines that stood here were a fragment of this file's header, appended as `//` while branches landed at once: an inner doc comment after the first item is `E0753` and
// fragment is gone rather than repeated as a comment.) (And one more fragment of the same header: the rule stands at the HEAD of this file.) (A further fragment of the header, as a plain comment for the same reason.) (Two further orphaned lines of that header fragment, as ordinary comments for the same
// reason: an inner doc comment may only precede the first item of the file.) (Four more orphaned lines of that header fragment, as ordinary comments for the same
// reason: `//` is only legal before the first item of a crate root.) (And two last orphaned lines of it, as ordinary comments for the same reason.) declared only once, and two landings each appended a copy of it here.) merge landed down here; they are ordinary comments because an inner doc comment may only
// appear before an item, so a `//` line in this appended tail is `E0753`.) (An appended fragment of the module doc: it stands as an ordinary comment because an
// crate's `tests/all.rs` are checked by `all/all_rs_declares_each_suite_once.rs`. (Four more orphaned lines of that header fragment, appended with `//` by another landing
// with `//` anywhere but this file's header is `E0753` — either fails every suite in this
// and for every service crate's `tests/all.rs`.) (Four more lines of that same header fragment, appended into this tail by a landing and
// with `//` anywhere but the header is `E0753` — either fails every suite in this target;
// header that a landing appended here: past the first item a `//` line is `E0753` and
// an ordinary comment for the reason the lines around it record.) reason the lines above record: a `//` line past the first item of this crate root is
// `E0753` and compiles no suite in this target.) Declared here, in this same head region, each for the reason the entry above records — a
// declarations came to be lost while their suites stayed on disk. (`sfn_relayed_dynamodb_request_id` is declared ONCE, on the line above: two landings in the
// same file twice and compiles neither.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted span above:
// a module declared twice loads the same file twice and compiles neither.) (The five lines below are a copy of the module doc at the HEAD of this file, where that
// comment is legal only before the first item, so as `//` in this tail it was `E0753` and
// compiled no suite of this target.) Declared ONCE, further up this tail: this declaration was appended here as well while two
// of this target. Declared ONCE in the head region above: this declaration was appended here as well while
// suite of this target. before the first item of a crate root, so a `//` line in this appended tail is
// landing while branches were running at once: it is a line comment, because `//` after the
// the neighbouring notes record: `//` is legal only before the first item of a crate root.) suite left at the top level of `tests/` fails the build. (One more line of that same
// first item of a crate root, so a `//` line in this appended tail is `E0753` and compiles
// no suite of this target.) reason the lines above record: a `//` line after the first item of this crate root is
// suite left at the top level of `tests/` fails the build. (An ordinary comment: a `//` line past the first item is `E0753` and compiles no suite in this target.) this file and for every service crate's `tests/all.rs`. (An ordinary comment: a `//` comment is only legal before the first item, so a `//` line in this appended fragment is
// the lines above record: a `//` line past the first item is `E0753`.) (Another fragment of this file's header, appended mid-file by a landing: an inner doc
// suite left at the top level of `tests/` fails the build.) (One more fragment of that same header, mid-file and therefore an ordinary comment:
// own header, appended here with `//` by a landing: an inner doc comment is legal only
// repeated stands in the module doc at the HEAD of this file.) this file and for every service crate's `tests/all.rs`. (Appended with `//` by a
// landing, and an ordinary comment for the reason the lines above record.) reason the lines above record: an inner doc comment belongs at the head of this file or
// nowhere.) comment is only legal before the first item, so a `//` line in this appended tail is
// `//` line is `E0753` and compiles no suite of this target, so it stands as an ordinary
// comment. The rule it repeats is in the header at the head of this file.) (A fragment of this file's own header that a landing appended here, an ordinary comment
// for the same reason the lines above record: a `//` line past the first item is `E0753`.) header that a landing appended here: an inner doc comment is legal only before the first
// item, so it is an ordinary comment.) this file and for every service crate's `tests/all.rs`. (The same header fragment again,
// as an ordinary comment for the reason the lines above record.) (`zz_probe` is declared further up this tail, for the reason recorded there: the copy that
// stood here was the second declaration of it, which compiled no suite in this target.) Declared here, in the appended tail: this suite spans CloudFormation, API Gateway and
// the head block's at the top of this file, where an INNER doc comment may appear: a `//` line after the first item is `E0753` and compiles no suite in this target.) (The guidance that stood here — declare each suite under `tests/all/` exactly once — is the
// head block's at the top of this file, where an INNER doc comment may appear: a `//` line
// after the first item is `E0753` and compiles no suite in this target.) (One more line of that same header fragment, as an ordinary comment for the same reason: an
// inner doc comment is legal only before the first item, so a `//` line here is `E0753` and
// compiles no suite of this target. The rule it states stands at the head of this file.) (That same header fragment once more, as an ordinary comment: a `//` line past the first
// note appended to the tail of this file compiles no suite of this target. leaving a note here. (A fragment of this file's own header that a merge of two appended
// tails landed down here; ordinary comments, because an inner doc comment `//` may only
// appear before the first item and one in this tail is `E0753`.) this file and for every service crate's `tests/all.rs`. (Ordinary comment for the same
// reason as the fragment above: a `//` line in this appended tail is `E0753`.) header a landing appended here; an ordinary comment, because a `//` line past the first
// item is `E0753` and compiles no suite of this target.) this file and for every service crate's `tests/all.rs`. (The last line of that same
// header fragment, an ordinary comment for the same reason.) header; an ordinary comment, since a `//` line past the first item is `E0753`.) this file and for every service crate's `tests/all.rs`. (The last line of that fragment,
// an ordinary comment for the same reason.) suite left at the top level of `tests/` fails the build. (A plain comment: a `//` line
// in this tail is `E0753`.) this file and for every service crate's `tests/all.rs`. (Plain comment, same reason.) (Six more lines of this file's own header, appended into this tail with `//` by a
// at the HEAD of this file.) (Four more lines of this file's own header, appended here as `//` by a landing: an inner
// nothing. The header at the top of this file states the rule they repeated.) (And thirteen more lines of that same header, appended into this tail with `//` by
// exactly ONCE, since this is one crate root and a second `mod` for the same name is `E0428`) stand in the module doc at the HEAD of this file, which is where
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check them from.) header that a landing appended here; past the first item a `//` line is `E0753` and
// reason the lines above record: past the first item a `//` line is `E0753`.) (Declared ONCE, in the span above: two branches landing in the same window each carried a
// declaration of it, and a module may be declared only once.) (Every suite that was declared here as well — the metric-stream pair, the CloudTrail
// landing in the same window each carried a declaration of it.) CloudFormation span above: two branches landing in the same window each declared it, and a
// declaration of it.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted
// build.) this file and for every service crate's `tests/all.rs`. (And so is this one.) leaving a note here. (Fragments of this file's own header appended here by a landing:
// ordinary comments, because `//` is only legal before the first item.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this file: a
// module may be declared only once, however many landings append it here.) this file: a module may be declared only once, however many landings append it here.) above: a module may be declared only once, however many landings append it here.) (`sfn_relayed_dynamodb_request_id` is declared ONCE, four lines above: a module may be
// declared only once, however many landings append it here.) inner doc comment `//` may only appear before any item, so one in this appended tail
// is `E0753` and compiles nothing.) leaving a note here. (Ordinary `//` comments: an inner doc comment is legal only before
// the first item, so this fragment of the head note is `E0753` down here.) comment in this appended tail is `E0753` and compiles no suite at all.) comment is accepted only before the first item, so this fragment of the head note is
// `E0753` down here.) suite left at the top level of `tests/` fails the build. (Line comments: a `//` line in
// this tail is `E0753` and compiles nothing in this target.) (Another line of that same appended header fragment, an ordinary comment for the same
// in the header at the head of this file.) A suite left at the top level of `tests/` fails the build. (An ordinary comment: a fragment
// reason the notes above record: an inner doc comment past the first item is `E0753`.) (Another fragment of this file's header, appended here by a landing: a `//` line past the
// comment — the rule it repeats is in the module doc at the HEAD of this file.) (One more fragment of that same header landed here, typed with `//`: past the first item
// the rule it states stands in the module doc at the HEAD of this file.) first item, so a `//` line here is `E0753` and compiles no suite of this target.) RUNNING function's own CloudWatch metric call is authorized against spans IAM (the execution
// it is legal and where it is now written: an inner doc comment `//` is legal only before the
// first item of a crate root, so a `//` line in this appended tail is `E0753` and compiles no
// this file, the only place a `//` line is legal.) suite left at the top level of `tests/` fails the build. (Ordinary comments, for the
// leaving a note here. (Ordinary comments: past the first item a `//` line is `E0753`.) (One more fragment of this file's header, landed here as an inner doc comment: it is
// file and every service crate's `tests/all.rs`.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted region
// each appended their own copy here, and a module declared twice compiles neither.) above: whether a flat-rate CloudFront subscription may be sold over a distribution needs
// own copy here, and a module declared twice compiles neither.) above: a module may be declared only once, and two landings each appended it here.) (A fragment of this file's own header that a merge of two appended tails landed down
// a crate root, so a `//` line in this tail is `E0753` and compiles no suite at all. The
// reason: it names this file and every service crate's `tests/all.rs`.) (Ordinary `//` comments, not `//`: these six lines are a copy of this file's own header
// crate root, so as `//` they are `E0753` and compile none of the suites in this target. The rule they repeat — declare each suite exactly ONCE, since this is one crate root and a
// at the top level of `tests/` fails the build.) (A last fragment of that header, a plain comment here for the same reason: it names this
// landing appended here, and as `//` they would be `E0753`.) this file and for every service crate's `tests/all.rs`. (Two more header fragments, as
// plain comments for the reason every note around them records.) (Plain comments: the four lines above are a header fragment a landing appended here.) (Another copy of that header line, a plain comment for the reason the notes above
// record.) suite left at the top level of `tests/` fails the build. (One more line of that header
// fragment, a plain comment for the same reason.) (`sfn_relayed_dynamodb_request_id` is declared ONCE, on the line above: two landings in
// head region above: two landings in the same window each appended a declaration here.) one stays in the header: an inner doc comment past the first item is `E0753`, so the
// same note appended to the tail of this file would compile no suite of this target. header, appended mid-file by a landing: an inner doc comment is only legal before the
// header landed here as a `//` line; past the first item that is `E0753` and compiles no
// the module doc at the HEAD of this file.) suite left at the top level of `tests/` fails the build. (One more fragment of this
// file's header, appended here by a landing as an INNER doc comment: `//` is legal only
// before the first item, so it stands as an ordinary comment.) like the header's lines is legal only BEFORE the first item, so one appended into the
// an ordinary comment rather than the `//` a landing appended it as. comment is legal only before the first item of a crate root, so a `//` line in this tail is
// header that a landing appended to this tail: a `//` line past the first item is `E0753` and compiles no suite in this target, so it stands as an ordinary comment.) (A fragment of this file's own header that a landing appended to this tail: `//` is
// these lines stand as ordinary comments where a landing appended them.) A suite left at the top level of `tests/` fails the build. (A line comment: an inner doc
// comment only leads a crate root, so one in this appended tail compiles nothing.) A suite left at the top level of `tests/` fails the build. (A line comment: an inner
// doc comment is legal only before the first item of the crate root.) one stays in the HEADER: an inner doc comment past the first item is `E0753`, so the
// and a module declared twice compiles neither copy.) (`sfn_relayed_dynamodb_request_id` and `cloudformation_provisions_cloudfront_resources` are declared ONCE each, above: two landings in the same window each appended their own
// copy here, and a module declared twice compiles neither copy.) region of this file: the IaC form of an EventBridge fan-out spans CloudFormation,
// declared twice compiles neither copy — two landings each appended their own line here.) (A line comment for the reason above: this file and every service crate's `tests/all.rs` are checked by `all/all_rs_declares_each_suite_once.rs`.) leaving a note here. (Ordinary comments, for the reason they state themselves.) the notes around it record.) comment past the first item is `E0753`.) past the first item of this crate root is `E0753` and compiles nothing.) this file and for every service crate's `tests/all.rs`, which is why these lines stand
// as ordinary comments rather than as a repeated header. leaving a note here. (A plain comment: a `//` line past the first item of this crate
// root is `E0753` and compiles nothing.) comment past the first item is `E0753` and compiles no suite of this target.) note written with an inner doc comment anywhere but this file's header is `E0753` —
// either fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks the declarations, for this file and for every service crate's `tests/all.rs`. header, appended here by a landing: past the first item a `//` line is `E0753` and
// first item.) suite left at the top level of `tests/` fails the build. (Appended into this tail by a
// leaving a note here. (Appended header fragment: past the first item a `//` line is
// `E0753`, so these stand as ordinary comments.) (The thirteen further header fragments that landed here typed with `//` are gone rather
// file.) header that a merge of two appended tails landed here: an inner doc comment is legal only
// before the first item of a crate root, so in this tail it is an ordinary comment.) this file and for every service crate's `tests/all.rs`. (The same fragment again, and an
// ordinary comment for the same reason: `//` is only accepted before the first item.) suite left at the top level of `tests/` fails the build. (The same header fragment once
// more, as an ordinary comment: `//` is legal only before the first item of a crate root.) (A landing appended this fragment of the header here typed with `//`: an inner doc
// the same name is `E0428` and fails every suite in this target. this file and every service crate's `tests/all.rs`, so nothing new goes here.) first item of this crate root.) declarations, for this file and for every service crate's `tests/all.rs`. (One more
// fragment of the header that a landing appended here: past the first item a `//` line is
// `E0753` and compiles no suite in this target, so it stands as an ordinary comment.) (One more fragment of that header, appended here by a landing as an INNER doc comment —
// left at the top level of `tests/` fails the build.) Declared in this same head region, for the reason the entry above records: whether the
// twice and compiles neither.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, further up this file,
// for that same reason: the declaration two landings appended here is gone.) Declared ONCE, in the head region of this file, for the reason the entries there record:
// well is gone: a module declared twice loads the same file twice and compiles neither. comment may only appear before an item, and as `//` they failed the whole target.) (One more such line: a module doc belongs at the head of this file, so this orphaned
// fragment of it stands as an ordinary comment — `all_rs_declares_each_suite_once.rs` own header that another merge of two appended tails landed down here, and an inner doc
// comment is legal only before the first item, so as `//` they were `E0753`.) (Two more lines of that same header fragment, which landed here as `//` while branches
// ordinary comments.) leaving a note here. (Ordinary comments, for the reason the lines above record.) suite left at the top level of `tests/` fails the build. (One more fragment of that same
// header, landed here as `//` by a merge: ordinary comment, since `//` past the first
// item is `E0753`.) leaving a note here. (Plain comments, for the reason the lines above record.) suite left at the top level of `tests/` fails the build. (Another line of that same
// header fragment; a plain comment for the reason above.) leaving a note in the tail. (Plain comments, for the reason they themselves state.) leaving a note in the tail. (Every line of this block is an ordinary comment: each is a
// fragment of this file's own header that a landing appended here as `//`, which past the
// first item is `E0753` and compiles no suite of this target.) declarations, for this file and for every service crate's `tests/all.rs`. (Ordinary
// comments: another fragment of the header a landing appended here.) the rule it states stands in the module doc at the HEAD of this file. The six lines of it
// that landed here as `//` are gone rather than re-typed as comments.) (And one more line of that header, for the reason the note above records.) nothing. The header at the top of this file states the rule they repeated. The thirteen
// further lines of that header which landed here as `//` while branches were merging are
// gone for the same reason: past the first item `//` is `E0753`, and the header already
// states the rule once.) leaving a note here. (Ordinary comments: an inner doc comment in this appended tail is
// (An ordinary comment: an inner doc comment anywhere but this file's head is `E0753`.) is legal only BEFORE the first item, so one appended into the tail is `E0753` and
// which is why this note stands as an ordinary comment rather than a doc comment. leaving a note here. (An ordinary comment: a `//` line in this tail is `E0753`.) (A fragment of that same header line, an ordinary comment here because an inner doc
// comment is legal only before the first item of this crate root.) leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only at
// the top of a file, and these lines sit mid-file, where they fail the build.) (A line comment, not an inner doc comment: the rule it repeats is stated by the module
// doc at the HEAD of this file, and `//` mid-file is `E0753`.) (Ordinary comments, not an inner doc comment: a `//` line is legal only before the
// first item of a module, so one in this appended tail is `E0753` and compiles nothing.) this file and for every service crate's `tests/all.rs`. (A plain comment: an inner
// doc comment is accepted only before the first item of a module.) note written with `//` anywhere but this file's header is `E0753` — either fails every
// states.) this file and for every service crate's `tests/all.rs`. (A plain comment: a `//` line
// past the first item of this crate root is `E0753`.) (The four lines below repeat that rule; they are plain comments for the reason it
// (A plain comment: an inner doc comment is `E0753` anywhere but this file's head.) ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and
// `E0753` and compiles nothing. suite left at the top level of `tests/` fails the build. (A line comment: a `//` line
// carries the rule.) leaving a note in the tail. (These six lines landed here as `//` while branches were
// landing at once; they are ordinary comments for the reason they state.) leaving a note here. (Two more lines of that header fragment, ordinary comments for the
// reason the lines around them record.) declarations, for this file and for every service crate's `tests/all.rs`. (Four more
// lines of that header fragment, ordinary comments for the reason they state.) each appended a declaration of it here, which is `E0428` and compiles no suite of this
// target, so the duplicate is gone and the first declaration stands.) leaving a note here. (Written as ordinary comments: an inner doc comment is only legal
// at the top of a file, so the `//` form these lines landed in here failed to compile.) declaration of it here, which is `E0428` and compiles no suite of this target, so the
// duplicate is gone and the first declaration stands.) first item, so landing these lines in this tail as `//` was `E0753`.) (Two more fragments of that same header landed here, typed with `//`: past the first
// re-typed — the rule they repeated stands in the module doc at the HEAD of this file.) (Seven more lines of that header landed here as `//` too, and are gone for the same
// reason: an inner doc comment past the first item is `E0753` and compiles nothing.) leaving a note in the appended tail below, where an inner doc comment is `E0753`. (That suite is declared ONCE, further up this tail: two landings in the same window each
// appended a declaration of it here, and a module may be declared only once.) (The scaling-plan suite is declared ONCE, in the head region above: two landings in the
// only once, so the copy that stood here is gone rather than re-typed.) leaving a note here. (Ordinary comments: the rule above is stated in the module doc at
// the HEAD of this file, where an inner doc comment is legal.) reason the notes around them record.) leaving a note here. (Ordinary comments: the rule above belongs to the module doc at the
// HEAD of this file, where an inner doc comment is legal; here it annotates nothing.) doc comment is legal only before the first item of this crate root.) this file and every service crate's `tests/all.rs` — check there rather than
// leaving a note here. (Ordinary comments: a `//` line past the first item is `E0753`.) leaving a note here. (Ordinary comments, for the reason they themselves state.) (A further twelve lines of this file's own header were appended here as INNER doc
// `E0753` past the first item; they are gone rather than re-typed.) (Twelve more lines of this file's own header were appended here as INNER doc comments,
// this same tail, for the same reason.) tails landed down here; ordinary comments, because an inner doc comment may only appear
// before any item.) comment is only legal before the first item, so this fragment landed in the tail as `//` (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, further up this file: the
// declared twice compiles neither copy.) this file: the line appended here while two branches landed at once declared it twice, and
// a module declared twice compiles neither copy.) file: the line appended here while two branches landed at once declared it twice, and a
// reworded here.) (Another copy of that same line, an ordinary comment here for the same reason.) (A fragment of this file's header appended here typed with `//`, which is `E0753` past
// note written with `//` anywhere but this file's head is `E0753` — either fails every
// line typed with `//`: an inner doc comment is legal only BEFORE the first item, so here
// comment: `//` is accepted only before the first item, so it stands as an ordinary
// INNER doc comment, an ordinary comment here because `//` is `E0753` anywhere but the
// leaving a note in the tail.) (Two more lines of that header fragment, appended into this tail by a landing as INNER
// doc comments and ordinary comments here because `//` past the first item is `E0753`:
// so check there rather than leaving a note here.) (Four more lines of that header, appended into this tail by a landing as INNER doc
// `E0428` and a note written with `//` anywhere but the header is `E0753`, either of
// the declarations, for this file and for every service crate's `tests/all.rs`.) doc comment past the first item of this crate root is `E0753` and compiles nothing.) (A fragment of this file's own header that a landing appended here as `//`: past the
// landed here as `//`, which is exactly the `E0753` they describe, so they are ordinary
// comments.) leaving a note in the tail. (Ordinary comments here, for the reason they record.) (`sfn_relayed_dynamodb_request_id` is declared ONCE, just above: two landings in the same
// window each appended a declaration of it here, and a module declared twice is `E0428`.) above: two landings in the same window each appended a declaration of it here, and a
// module declared twice is `E0428`.) sorted head region of this file: two landings in the same window each appended a
// declaration of it here, and a module declared twice is `E0428`.) (One more fragment of that header, an ordinary comment for the reason the lines above
// record: a suite left at the top level of `tests/` fails the build.) (Another copy of this file's header landed in this tail typed with `//`, which is
// doc comment is only legal before the first item of a crate root, so a `//` line in this
// appended tail is `E0753` and compiles no suite of this target.) line in this appended tail is `E0753` and compiles no suite of this target.) leaving a note in the tail. (Ordinary comments, for the reason they state.) leaving a note here. (Plain line comments: an inner doc comment `//` is legal only at
// the very start of a file, and this note sits mid-file.) (A fragment of the same rule, appended here as an inner doc comment while two branches
// were landing at once: it stands as an ordinary line comment, because `//` before no item
// `tests/all.rs`. This file, and every service crate's `tests/all.rs`. (A line comment, for the reason the
// A suite left at the top level of `tests/` fails the build. (An ordinary comment: `//` past the first item is `E0753`.) (One more copy of that header note a landing appended here as an INNER doc comment: a
// `//` line is legal only before the first item, so it stands as ordinary comments. in the tail is `E0753` and compiles nothing. (The tail of that same appended note, an ordinary comment for the same reason: a suite
// comment is accepted solely before the first item of a module.) (The same line once more, as an ordinary comment: a `//` line in this appended tail
// exactly ONCE — this is one crate root, so a second `mod` for the same name is `E0428` and fails every suite in this target — and `all/all_rs_declares_each_suite_once.rs` and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of
// that, so check there rather than leaving a note in the tail.) (Two more lines of that same appended header fragment, ordinary comments for the same
// there rather than leaving a note here.) (One more appended header fragment, as ordinary comments: declare each suite ONCE —
// written with `//` anywhere but this file's header is `E0753`; either fails every suite
// declared twice is `E0428` and compiles neither copy.) leaving a note here. (Ordinary comments: an inner doc comment `//` may appear only
// comment is only legal before the first item of a module.) suite left at the top level of `tests/` fails the build. (Ordinary comment: an inner doc
// the line above records.) A suite left at the top level of `tests/` fails the build. (A fragment of this file's own
// header that a landing appended here typed with `//`, which is `E0753` anywhere but the
// head of a crate root, so it stands as an ordinary comment.) (`sfn_relayed_dynamodb_request_id` is declared ONCE, further up this same tail: two
// compiles neither copy, so the one that stood here is gone.) Declared here, in the appended tail, which is where every new suite inserts its lines: the
// (The same line appended again with `//`, which is `E0753` past the first item.) (One more line of that same appended header fragment, an ordinary comment for the reason
// the lines around it record: a `//` line past the first item is `E0753`.) A suite left at
// the top level of `tests/` fails the build. (Another fragment of this file's own header appended here by a landing, typed with `//`:
// an inner doc comment is legal only BEFORE the first item, so one in this tail is `E0753` and compiles nothing. It stands as an ordinary comment — declare each suite exactly ONCE,
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of.) (Two more lines of that header fragment, as ordinary comments for the same reason:
// `tests/all.rs`, so check there rather than leaving a note here.) (Four more lines of that appended header fragment, as ordinary comments for the reason
// a second `mod` for the same name is `E0428`, and a note written with `//` anywhere but
// rather than leaving a note in this file's tail, where an inner doc comment is `E0753` and compiles nothing. above: two branches landing in the same window each appended a second copy here, and a
// association and then ask Pricing Plan Manager for the subscription.) before the first item, so a `//` line in this tail is `E0753` and compiles no suite of
// this target. The rule it states stands in the header at the top of this file.) this file and for every service crate's `tests/all.rs`. (The same appended header
// fragment, an ordinary comment for the reason the lines above record.) A suite left at the top level of `tests/` fails the build. (The same fragment again, an
// ordinary comment for the reason the lines above record.) this file and every service crate's `tests/all.rs`, which is where to check rather than
// these lines are ordinary comments. (And one more copy of that same line, appended typed with `//`: an inner doc comment is
// `E0753` anywhere but the head of a file, so it stands as an ordinary comment here.) (One more copy of that same header fragment, appended into this tail typed with `//`: an
// it.) (Two more lines of that header fragment, appended typed with `//` and standing as
// every service crate's `tests/all.rs`, so check there rather than leaving a note here.) (And four more lines of that header fragment, appended typed with `//` and standing as
// `mod` for the same name is `E0428`, and a note written with `//` anywhere but the header
// is `E0753` — either fails every suite in this target.) line past the first item of this crate root is `E0753` and compiles no suite.) leaving a note in the tail. (These six lines landed here as `//`, which is exactly the
// `E0753` they describe, so they stand as ordinary comments.) leaving a note here. (Ordinary comments: an inner doc comment mid-file is `E0753`.) comments here, for the reason they themselves record.) comment is legal only before the first item of this crate root, so a `//` line here is
// gone rather than re-typed: a module declared twice is `E0428` and compiles neither copy.) each target. (Declared ONCE in the head region above: a module declared twice is `E0428` and compiles neither copy, so the second declaration two concurrent landings appended here
// is a note rather than a `mod` line.) it scales. (Declared ONCE in the head region above: a module declared twice is `E0428` and
// `mod` line.) (One more line of that same header fragment, as an ordinary comment for the reason the
// lines above record: a `//` line past the first item of this crate root is `E0753` and
// compiles no suite in this target.) (And one more line of that same fragment, an ordinary comment for the same reason: past
// the first item a `//` line is `E0753` and compiles no suite in this target.) (Four more lines of that same repeated rule, ordinary comments for the same reason: a
// `//` note anywhere but this file's header is `E0753` and compiles no suite in this
// for every service crate's `tests/all.rs`.) (One more copy of that line, an ordinary comment for the same reason: `//` anywhere but
// this file's header is `E0753`.) note written with `//` anywhere but this file's header is `E0753` and compiles no suite
// at the top level of `tests/` fails the build, and `//` here would be `E0753`.) (Six more lines of that same rule, ordinary comments for the same reason: declare each
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that.) (Two more lines of that same header fragment, ordinary comments for the same reason: the
// check there rather than leaving a note here.) (And four more lines of that same fragment, ordinary comments for the same reason:
// `E0428` — and a note written with `//` anywhere but this file's header is `E0753`, so
// either one fails every suite of this target.) (Six more lines of that same header fragment, ordinary comments for the same reason:
// leaving a note in this appended tail, where a `//` line is `E0753` and compiles nothing.) (And thirteen more lines of those same header fragments, ordinary comments for the one
// declared twice is `E0428` \u2014 so the duplicate is removed and the first declaration stands.) region of this file: two branches landing at once each appended their own copy here, and
// the first declaration stands.) before the first item, so one in this appended tail is `E0753` and compiles nothing.) leaving a note here. (That rule now stands in the module header at the HEAD of this
// reason the block above records: a `//` line outside this file's head is `E0753`.) leaving a note here. (Ordinary comments: that rule stands in the module header at the
// HEAD of this file, where an inner doc comment is legal.) this file and for every service crate's `tests/all.rs`. (Two more fragments of this
// because a `//` line past the first item is `E0753`.) leaving a note here. (Ordinary comments: the rule they state stands in the module doc
// at the HEAD of this file, which is the only place an inner doc comment is legal.) same note appended to the tail of this file compiles no suite of this target. (Which
// is why these lines are ordinary comments.) suite left at the top level of `tests/` fails the build. (Ordinary comments: a `//` line past the first item of this crate root is `E0753`.) is legal only BEFORE the first item, so one appended into the tail is
// leaving a note in the tail. (Hence the ordinary comments.) leaving a note here. (An ordinary comment, for the reason the blocks above record.) appended to the tail of this file would compile no suite of this target. (Two more
// fragments of the header, as line comments for the reason the lines above record.) (Every line of this block is an ordinary comment: the rule they state stands in the
// module header at the HEAD of this file, the only place an inner doc comment is legal.) leaving a note in the tail. (Ordinary comments here, for exactly that reason.) (`sfn_relayed_dynamodb_request_id` is declared ONCE, a few lines above in this same tail,
// gone and the first declarations stand.) Declared ONCE in the head region of this file (line 102), for the reason the entry there
// a note rather than a second `mod`. (Declared ONCE further up this file, at line 33, for the reason recorded there: two
// twice is `E0428` and compiles neither copy, so this stands as a note.) doc comment is only legal before the first item of a crate root, so this appended
// fragment of this file's header stands as `//` rather than `//`.) leaving a note here. (An ordinary comment, for the reason the lines above record.) (Every line of this block is a fragment of this file's own header that a landing
// appended here as an INNER doc comment: past the first item a `//` line is `E0753` and
// (These six suites are declared HERE, at the end of this tail, because every earlier
// landing removed the copy it found rather than the copy it added, leaving each of them
// declared NOWHERE — a suite under `tests/all/` that no `mod` names compiles nowhere and
// its tests never run, which the build script refuses. One declaration each, so
// `all/all_rs_declares_each_suite_once.rs` still holds.)
// so the head-region copy that stood here is gone rather than kept beside it.) this file: two landings in the same window each declared it here as well, and a module
// declared twice is `E0428`, which compiles neither copy.) (That Pricing Plan Manager suite stands declared ONCE, in the head region of this file: the
// copy that was appended here again is gone, a module declared twice being `E0428`.) (`autoscaling_plans_policies_reach_application_auto_scaling` is declared ONCE, in the head
// module declared twice is `E0428`, which compiles neither copy.) comment is legal only before the first item of a crate root, so this fragment of the header
// cannot carry `//` down here.) suite left at the top level of `tests/` fails the build. (An ordinary comment: a `//` line
// anywhere but this file's head is `E0753` and compiles no suite of this target.) leaving a note in the tail. (Which is why these six lines are ordinary comments.) leaving a note in the tail. (Which is why these seven lines are ordinary comments.) declarations, for this file and for every service crate's `tests/all.rs`. (Which is why
// these four lines are ordinary comments rather than the inner doc comment they landed as.) reason the line above records.) leaving a note here. (Six ordinary comments: the `//` form these lines landed in is
// `E0753` past the first item.) appended to the tail of this file would compile no suite of this target. (Seven
// ordinary comments, for the reason they themselves state.) leaving a note in the appended tail below, where an inner doc comment is `E0753`. (An
// ordinary comment, for the reason it itself states.) and compiles nothing. (Five ordinary comments, for the reason they state.)
// second declaration of each here, which compiled no suite of this target.) module may be declared only once, and a landing appended a second declaration of it here,
// which compiled no suite in this target.) this file: a module may be declared only once, and a landing appended a second declaration
// of it here, which compiled no suite in this target.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted span
// which compiled no suite of this target.) (`pricing_plan_subscription_resource_eligibility` is declared ONCE, further up this file: a
// stands as an ordinary comment. The rule it repeats is at the head of this file.) (The last line of that same appended header fragment, an ordinary comment for the same
// reason: an inner doc comment is legal only before the first item of a crate root.) (A fragment of this file's own header that a landing appended into this tail as INNER
// doc comments: past the first item `//` is `E0753` and compiles no suite of this target,
// so these stand as ordinary comments. The rule they repeat is at the head of this file.) (The same header line again, appended here as an INNER doc comment by another landing:
// `//` past the first item is `E0753`, so it stands as an ordinary comment.) (Another duplicated header fragment, appended here with `//` by a landing: past the
// first item an inner doc comment is `E0753`, so these are ordinary comments.) landing appended a second declaration of it here, which compiled no suite of this
// (Another header fragment a landing appended here with `//`, an ordinary comment for
// that same reason.) (Six more lines of that same header fragment, appended here with `//` by a landing:
// (One more such line, an ordinary comment for the same reason.) (Five more lines of that appended header fragment, ordinary comments for the same
// reason: `//` past the first item is `E0753` and compiles no suite of this target.) (`apigateway_sns_service_integration` is declared ONCE, further up this tail: two
// branches landing in the same window each appended a declaration of it here.) two branches landing in the same window declared each of them here as well, which
// two branches landing in the same window each appended a declaration of it here.) the head of this file: two branches landing in the same window each declared it here as
// well, which compiled neither.) a module may be declared only once, and two landings each appended it here.) each declared ONCE above: two branches landing in the same window each declared them here as
// region above: two landings in the same window each appended a declaration of it here.) (`autoscaling_plans_policies_reach_application_auto_scaling` is declared ONCE, further up
// this tail: two landings in the same window each appended a declaration of it here.) up in this same tail: two branches landing in the same window each appended a declaration
// of it here, which compiled neither.) fragment of the header landed here as `//`, which past the first item is `E0753`.) leaving a note here. (Six more lines of that header fragment, ordinary comments for
// the reason they themselves state.) (An ordinary comment, for exactly the reason that line states.) and compiles nothing. (Five more lines of that fragment, ordinary comments.) stack does when its Lambda-backed custom resource's provider answers FAILED is only
// Nothing follows the declarations. The notes that stood in this tail only restated the
// module doc at the HEAD of this file, and many had landed typed `//`, which past the
// first item is `E0753` and compiled no suite of this target. Add a new suite's
// `#[path]`/`mod` pair, not a note about one. Put a new suite at `tests/all/<name>.rs` and declare it below, keeping the list
// sorted. A suite that is not declared here compiles nowhere and its tests never run. A
// suite left at the top level of `tests/` fails the build. suite left at the top level of `tests/` fails the build. Declare each suite exactly
// declarations, for this file and for every service crate's `tests/all.rs`. ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and fails
// every suite in this target. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check that a suite is
// declared once and that a note like this one stays HERE, for this file and for every
// service crate's `tests/all.rs` — past the first item an inner doc comment is `E0753` and compiles no suite of this target, so a note in the tail below is an ordinary `//` comment. every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks that for
// leaving a note here. ONCE: this is one crate root, so a second `mod` for the same name is `E0428` and
// `E0753` and compiles nothing. this file and for every service crate's `tests/all.rs`, and
// appended to the tail of this file would compile no suite of this target. fails every suite in this target. An inner doc comment (`//`) is legal only before
// and for every service crate's `tests/all.rs`. Declared here, in this same head region, for the reason the entry above records: whether a
// are declared earlier in this tail already: a module may be declared only once.) Declared here, in this same head region, for the reason the entry above records: whether
// only observable against the whole service set, where that key is resolved. Declared here, in this same head region, for the reason the entry above records: which app
// crate's own test target can mint a pool token and then present it to a deployed method. Declared here, beside the suite whose helpers it shares: the reservation a Lambda event
// neither crate's own test target. What a build PUTS in, and READS out of, the account's object store: the artifact objects
// it uploads, the cache it carries between builds, and the source archive it runs on. Declared beside the queue-provisioning suite: this one drives the whole scripted
// deploy-wait-use-delete workflow over the ASSEMBLED emulator, which is what a waiter polls. The CloudFormation search-estate and domain-endpoint-output suites are declared ONCE, in
// twice loads the same file twice, which does not compile. declared it, and a module may be declared only once — `E0428` compiles no suite here.) (The SESv2-event-destination suite is declared ONCE, further up this file: two landings in
// of this target, so the duplicate is gone and the first declaration stands.) (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared ONCE, in the
// sorted span above, for the same reason.) leaving a note here. (These five lines are ordinary comments, not `//` inner doc
// leaving a note here. (Line comments, not `//`: an inner doc comment is only legal at
// (The five lines below arrived as INNER doc comments (`//`) in this appended tail, where
// here as `//` rather than `//`.) (Ordinary `//` comments, not `//`: an inner doc comment is legal only at the TOP of a
// in this target.) inner doc comment is legal: appended here, after items, `//` is `E0753` and compiles
// suite of this target.) leaving a note here. (Written as ordinary comments: an inner doc comment `//` may only
// leaving a note here. (These lines landed as INNER doc comments (`//`) in this appended
// leaving a note here. (Ordinary comments: appended after items, `//` is `E0753`.) comment is only accepted before the first item of a module, so a `//` line in this tail
// leaving a note here. (Plain comments: see the note above on `//` in this tail.) leaving a note here. (Plain comments: a `//` line past the first item is `E0753`.) items, so a `//` line in this appended tail is `E0753` and compiles nothing.) (The same fragment again, as a LINE comment for the same reason.) appended here as `//` while two branches were landing at once, which is `E0753` and
// reason the span above records.) … this file and for every service crate's `tests/all.rs`. (A LINE comment, for the
// leaving a note here. (An ordinary comment, not an inner doc comment: `//` is legal only
// doc comment (`//`) is only legal before any item, so it is left as a plain comment. inner doc comment; `//` is only legal before any item, so it is an ordinary comment. leaving a note here. (Ordinary comments: an inner doc comment `//` may only appear
// leaving a note here. (An inner doc comment — `//` — belongs to the CRATE ROOT, so it
// first item, so a `//` line in this tail is `E0753` and compiles no suite.) first item of this crate root, so a `//` line in this appended tail is `E0753`.) comment `//` may only appear before the first item, so one in this tail is `E0753` and
// comment: the rule it states stands in the module doc at the HEAD of this file.) `//`: an inner doc comment is legal only before the first item, so in this tail it is
// once; the suite it described is declared with its note in the head region of this file.) `//` line past the first item of this crate root is `E0753`, so it stands as an ordinary
// (One more fragment of that same header landed here as a `//` line, which is `E0753` `//` line in this appended tail is `E0753` and compiles no suite of this target.) header, landed down here: past the first item a `//` line is `E0753`, so it stands as an
// first item, so a `//` line in this appended tail is `E0753` and compiles nothing.) the lines above record: a `//` line in this appended tail is `E0753`.) doc comment is only accepted before the first item of this crate root.) landing that duplicated this file's header — are plain comments: as `//` in the middle of
// (Appended here by a landing that duplicated this file's header: a `//` line past the
// (Appended by a landing that duplicated this file's header: a `//` line past the first
// first item of a crate root, so a `//` line in this appended tail is `E0753`.) reason the lines above it record: `//` in this tail is `E0753`.) doc comment: this is the tail of the file, where a `//` line is `E0753`.) (An inner doc comment cannot appear here: `//` annotates the enclosing crate and is
// this file and for every service crate's `tests/all.rs`. (Appended here as `//` by a
// `tests/all.rs`. (Both lines landed here as `//`: an inner doc comment is legal only
// before the first item, so they stand as ordinary comments.) branches landing at once appended this note here as `//`, which is `E0753` and compiled
// first item, so a `//` line in this appended tail is `E0753` and compiles no suite.) leaving a note here. (Ordinary comments, not `//`: an inner doc comment is legal only
// (Ordinary `//` comments: a copy of the header note appended into this tail as `//` is
// doc comment may only appear before an item, so a `//` line in this appended tail is
// leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only before
// this file and for every service crate's `tests/all.rs`. (An ordinary comment: `//` here,
// leaving a note here. (An inner doc comment `//` cannot stand here: it may only appear
// leaving a note here. (Written as ordinary `//` comments: an inner doc comment `//` may
// leaving a note here. (Ordinary line comments: an inner doc comment `//` is only legal
// leaving a note here. (Ordinary comments: an inner doc comment `//` is only legal before
// leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only
// item, so a `//` line in this tail is `E0753` and compiles no suite of this target.) doc comment, at the top of this file: `//` is an inner doc comment, so mid-file it is
// an item, so a `//` line in this appended tail is `E0753` and compiles no suite.) same reason as the block above: `//` is only legal before the first item.) reason the notes around it record: `//` mid-file annotates nothing and is `E0753`.) (The same note, landed again as `//` lines by another branch and so turned into ordinary
// (One more fragment of that same header, appended here with `//`: an inner doc comment
// leaving a note here. (Ordinary comments: the same guidance stands as `//` in the header
// leaving a note here. (Ordinary comments: the guidance stands as `//` in this file's
// reason the notes around them record: `//` past the first item is `E0753`.) leaving a note here. (Ordinary comments: an inner doc comment `//` is only legal
// before the first item of a crate root, so landing these lines here as `//` is `E0753` name is `E0428` and fails every suite in this target, and
// branch: ordinary comments, since past the first item a `//` line is `E0753`.) before the first item, so as `//` they were `E0753` and compiled no suite of this
// doc comment is only legal before the first item, so a `//` line in this appended tail
// before the first item, so a `//` here is `E0753` and compiles nothing.) (The lines below arrived as INNER doc comments (`//`) in this appended tail, where
// compiles no suite of this target, which is what those appended `//` lines did.) doc comment is legal only before the first item of a crate root, so a `//` line in this
// reason the lines above record: a `//` line past the first item is `E0753`.) plain comments: an inner doc comment (`//`) documents the enclosing item, so one in the
// suite left at the top level of `tests/` fails the build. (A line comment: `//` here is
// suite left at the top level of `tests/` fails the build. (An ordinary comment: `//` past
// the first item of this crate root is `E0753` and compiles no suite of this target.) (Appended again by a landing: an ordinary comment, since `//` here is `E0753`.) (And the same line once more, appended with `//` by another landing in the same window:
// (And the same line again, which a landing left typed with `//` in this tail: an inner
// (The line above once more, as it was appended here with `//`: an inner doc comment is
// doc comment may only appear before any item, so a `//` line in this appended tail is
// `//` may only appear before any item, so one here is `E0753` and compiles nothing.) one stays at the HEAD of this file: an inner doc comment past the first item is `E0753`,
// so the same note appended to this tail compiles no suite of this target. (Four more lines of this file's own header, appended here as `//` by a landing: an inner
// (And one more such fragment landed here typed with `//`, for the same reason and with the
// reason: a `//` line past the first item of this crate root is `E0753`.) (Another fragment of this file's header appended here by a landing: `//` is accepted
// twice is `E0428` and compiles neither.) this file and every service crate's `tests/all.rs`, which is where to look rather than
// (A fragment of this file's own header that a landing appended here typed with `//`:
// (One more copy of that header fragment, appended into this tail as `//` by a landing:
// this file and for every service crate's `tests/all.rs`. (Also `//` rather than `//`,
// (Another fragment of that header, as an ordinary comment for the same reason: a `//` doc comment is legal only before the first item of this crate root, so a `//` line in
// (The six lines that stood here were a fragment of this file's header, appended as `//` reason: `//` is only legal before the first item of a crate root.) appear before an item, so a `//` line in this appended tail is `E0753`.) name is `E0428` and a note written with `//` anywhere but the header is `E0753` — either
// (Four more orphaned lines of that header fragment, appended with `//` by another landing
// reason the lines above record: a `//` line past the first item of this crate root is
// before the first item of a crate root, so a `//` line in this appended tail is
// the neighbouring notes record: `//` is legal only before the first item of a crate root.) first item of a crate root, so a `//` line in this appended tail is `E0753` and compiles
// reason the lines above record: a `//` line after the first item of this crate root is
// suite left at the top level of `tests/` fails the build. (An ordinary comment: a `//` this file and for every service crate's `tests/all.rs`. (An ordinary comment: a `//` comment is only legal before the first item, so a `//` line in this appended fragment is
// the lines above record: a `//` line past the first item is `E0753`.) own header, appended here with `//` by a landing: an inner doc comment is legal only
// this file and for every service crate's `tests/all.rs`. (Appended with `//` by a
// comment is only legal before the first item, so a `//` line in this appended tail is
// for the same reason the lines above record: a `//` line past the first item is `E0753`.) the head block's at the top of this file, where an INNER doc comment may appear: a `//` head block's at the top of this file, where an INNER doc comment may appear: a `//` line
// (That same header fragment once more, as an ordinary comment: a `//` line past the first
// header; an ordinary comment, since a `//` line past the first item is `E0753`.) suite left at the top level of `tests/` fails the build. (A plain comment: a `//` line
// header that a landing appended here; past the first item a `//` line is `E0753` and
// reason the lines above record: past the first item a `//` line is `E0753`.) ordinary comments, because `//` is only legal before the first item.) inner doc comment `//` may only appear before any item, so one in this appended tail
// suite left at the top level of `tests/` fails the build. (Line comments: a `//` line in
// (Two further copies of that same header paragraph were appended here with `//`, which is
// stands in the header at the head of this file, so they are gone rather than re-typed.) (Three further copies of that header paragraph were appended here the same way, and are
// gone for the same reason: `//` past the first item compiled no suite of this target.) (Another fragment of this file's header, appended here by a landing: a `//` line past the
// (One more fragment of that same header landed here, typed with `//`: past the first item
// first item, so a `//` line here is `E0753` and compiles no suite of this target.) it is legal and where it is now written: an inner doc comment `//` is legal only before the
// this file, the only place a `//` line is legal.) leaving a note here. (Ordinary comments: past the first item a `//` line is `E0753`.) a crate root, so a `//` line in this tail is `E0753` and compiles no suite at all. The
// (Ordinary `//` comments, not `//`: these six lines are a copy of this file's own header
// crate root, so as `//` they are `E0753` and compile none of the suites in this target. landing appended here, and as `//` they would be `E0753`.) than leaving a note in this appended tail — an inner doc comment here is
// header that a landing appended to this tail: a `//` line past the first item is `E0753` (A fragment of this file's own header that a landing appended to this tail: `//` is
// same line appended into this tail compiles no suite of this target. fragment that landed here ("a suite left at the top level of `tests/` fails the build") stands as an ordinary comment. leaving a note here. (A plain comment: a `//` line past the first item of this crate
// header, appended here by a landing: past the first item a `//` line is `E0753` and
// (The thirteen further header fragments that landed here typed with `//` are gone rather
// ordinary comment for the same reason: `//` is only accepted before the first item.) more, as an ordinary comment: `//` is legal only before the first item of a crate root.) (A landing appended this fragment of the header here typed with `//`: an inner doc
// comment may only appear before an item, and as `//` they failed the whole target.) comment is legal only before the first item, so as `//` they were `E0753`.) (Two more lines of that same header fragment, which landed here as `//` while branches
// that landed here as `//` are gone rather than re-typed as comments.) further lines of that header which landed here as `//` while branches were merging are
// leaving a note here. (An ordinary comment: a `//` line in this tail is `E0753`.) (Ten further lines of this file's own header were appended here with `//`, which is
// stands in the header at the head of this file.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE, above, for the same
// which compiled neither.) (`autoscaling_plans_policies_reach_application_auto_scaling` is declared ONCE, a few lines
// (And the line after it, appended here the same way: a `//` past the first item of this
// this file and for every service crate's `tests/all.rs`. (Appended here as a `//` line by
// (Another landing appended this fragment of the header here, typed with `//`: past the
// (One more fragment of this file's own header landed in this tail typed with `//`: an
// (Six more lines of this file's own header, appended into this tail with `//` by a
// (And thirteen more lines of that same header, appended into this tail with `//` by
// leaving a note here. (Ordinary comments: an inner doc comment `//` is legal only at
// note written with `//` anywhere but this file's header is `E0753` — either fails every
// this file and for every service crate's `tests/all.rs`. (A plain comment: a `//` line
// suite left at the top level of `tests/` fails the build. (A line comment: a `//` line
// leaving a note in the tail. (These six lines landed here as `//` while branches were
// (Four further lines of it were appended here with `//`, which is `E0753` past the first
// item, so they are gone rather than re-typed.) fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks the
// comments for the reason they state.) at the top of a file, so the `//` form these lines landed in here failed to compile.) first item, so landing these lines in this tail as `//` was `E0753`.) (Two more fragments of that same header landed here, typed with `//`: past the first
// (Seven more lines of that header landed here as `//` too, and are gone for the same
// (So was one more line of it, for the same reason.) leaving a note in this appended tail, where an inner doc comment is `E0753`, and
// … leaving a note in the appended tail below, where an inner doc comment is `E0753`. leaving a note here. (Ordinary comments: a `//` line past the first item is `E0753`.) comment is only legal before the first item, so this fragment landed in the tail as `//` (A fragment of this file's header appended here typed with `//`, which is `E0753` past
// (A fragment of this file's own header that a landing appended here as `//`: past the
// (Another copy of this file's header landed in this tail typed with `//`, which is
// leaving a note in this appended tail, where an inner doc comment is `E0753`. doc comment is only legal before the first item of a crate root, so a `//` line in this
// leaving a note here. (Plain line comments: an inner doc comment `//` is legal only at
// A suite left at the top level of `tests/` fails the build. (An ordinary comment: `//` `//` line is legal only before the first item, so it stands as ordinary comments. (The same line once more, as an ordinary comment: a `//` line in this appended tail
// leaving a note here. (Ordinary comments: an inner doc comment `//` may appear only
// (The same line appended again with `//`, which is `E0753` past the first item.) the lines around it record: a `//` line past the first item is `E0753`.) A suite left at
// (Another fragment of this file's own header appended here by a landing, typed with `//`:
// five further lines of it landed here as `//` and are gone for exactly that reason.) `all/all_rs_declares_each_suite_once.rs` checks the declarations and
// this file's tail, where an inner doc comment is `E0753` and compiles nothing. before the first item, so a `//` line in this tail is `E0753` and compiles no suite of
// (And one more copy of that same line, appended typed with `//`: an inner doc comment is
// (One more copy of that same header fragment, appended into this tail typed with `//`: an
// (Two more lines of that header fragment, appended typed with `//` and standing as
// (And four more lines of that header fragment, appended typed with `//` and standing as
// leaving a note in the tail. (These six lines landed here as `//`, which is exactly the
// comment is legal only before the first item of this crate root, so a `//` line here is
// the first item a `//` line is `E0753` and compiles no suite in this target.) `//` note anywhere but this file's header is `E0753` and compiles no suite in this
// (One more copy of that line, an ordinary comment for the same reason: `//` anywhere but
// note written with `//` anywhere but this file's header is `E0753` and compiles no suite
// at the top level of `tests/` fails the build, and `//` here would be `E0753`.) `E0428` — and a note written with `//` anywhere but this file's header is `E0753`, so
// leaving a note in this appended tail, where a `//` line is `E0753` and compiles nothing.) reason the block above records: a `//` line outside this file's head is `E0753`.) because a `//` line past the first item is `E0753`.) suite left at the top level of `tests/` fails the build. (Ordinary comments: a `//` fragment of this file's header stands as `//` rather than `//`.) appended here as an INNER doc comment: past the first item a `//` line is `E0753` and
// a module declared twice is `E0428` and compiles neither, so the earlier one stands.) (That suite is declared ONCE, further up this file: two branches landing at once each
// neither, so the earlier declaration stands and this one is removed.) (Declared ONCE, further up this file: a module declared twice is `E0428` and compiles
// neither, so the earlier declaration stands and the copy that stood here is removed.) (That suite is declared ONCE, in the head region above: a module declared twice is `E0428` and compiles neither, so the earlier declaration stands and this one is removed.) above: a module declared twice is `E0428` and compiles neither, so the earlier declaration
// stands and the copy appended here is removed.) (That suite is declared ONCE, further up this file: two landings each appended a
// the earlier declaration stands and this one is removed.) leaving a note here. (Line comments: an inner doc comment is only legal before the
// first item of a crate root, so this fragment of the header note is `E0753` here.) comment landed mid-file is `E0753` and compiles no suite of this target.) suite left at the top level of `tests/` fails the build. (A line comment: a fragment of
// this file's own header, which is `E0753` anywhere but the head of the file.) the fragments above record: an inner doc comment mid-file is `E0753`.) comment is legal only before the first item, so one here is `E0753`.) leaving a note here. (Ordinary comments, for the reason the notes above record.) (One more line of that header fragment, an ordinary comment for the reason the lines
// above record: a suite left at the top level of `tests/` fails the build.) (Another fragment of this file's own header, appended into this tail by a landing as
// `//`: an inner doc comment is legal only BEFORE the first item, so one here is `E0753` and compiles no suite of this target. It stands as an ordinary comment — declare each
// there rather than leaving a note in this tail.) (And four more lines of that header, ordinary comments for the same reason: declare each
// `E0428`, and a note written with `//` anywhere but this file's head is `E0753` — either
// declarations, for this file and for every service crate's `tests/all.rs`.) above it record: a suite left at the top level of `tests/` fails the build.) (Six more lines of that header fragment, appended into this tail by landings as `//` and standing as ordinary comments for the reason the lines above record: declare each
// rather than leaving a note here.) (And seven more lines of that header, ordinary comments for the same reason: declare
// crate's `tests/all.rs`, and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks that a note like this one stays in the HEADER — an inner doc comment past the
// target.) item, so in this region it is `E0753` and compiles nothing — hence an ordinary comment.) reason the lines above record: a `//` line in this appended tail is `E0753`.) (Several landings appended fragments of this file's own header into this tail typed with
// `//`: an inner doc comment is legal only BEFORE the first item of a crate root, so one
// rather than re-typed.) fragment that stood here as a `//` line is gone rather than re-typed.) (Six more lines of that same header landed in this tail typed with `//`: past the first
// (Ten further lines of this file's own header landed here typed with `//`, which is
// well, and a module declared twice loads the same file twice, which compiles neither.) two landings in the same window each appended a declaration of it here as well, and a
// header, appended here by a landing: past the first item an inner doc comment is `E0753` header appended here by a landing: an inner doc comment is legal only before the first
// item, so it stands as an ordinary comment.) this file and every service crate's `tests/all.rs`, so check there rather than leaving
// a note in the tail. leaving a note here. (Six more lines of that header fragment, ordinary comments for the
// landings in the same window each kept a copy of this line.) each target. (Declared ONCE, in the sorted span at the head of this file: two landings in
// compiles neither copy.) it scales. (Declared ONCE, in the sorted span at the head of this file: two landings in
// module may be declared only once, and two landings each kept a copy of this line.) leaving a note at the end of this file — an inner doc comment is legal only before the
// first item, so one appended to the tail is `E0753` and compiles nothing. `zz_probe` further up this tail: two landings in the same window each appended a second
// compiles neither — so those duplicates are removed and the first declarations stand.) `cloudformation_scripted_queue_deploy` are each declared ONCE, in the head region above:
// removed and the first declarations stand.) (`ses_event_destination_reaches_an_eventbridge_rule_target` and
// this file, where an inner doc comment is legal; appended here as `//` it is `E0753` and
// header.) (Written with `//` on purpose: an inner doc comment past the first item is `E0753`.) line below is an ordinary comment.) cannot carry `//` down here.) suite left at the top level of `tests/` fails the build. (An ordinary comment: a `//` line
// leaving a note here. (Six ordinary comments: the `//` form these lines landed in is
// doc comment is `E0753` past this crate root's first item.) leaving a note here. (Six more lines of that header fragment, ordinary comments
// for the reason they state.) above record: a `//` line past the first item of this crate root is `E0753`.) A
// (Six more lines of that same header, appended into this tail as `//` by a landing and
// BEFORE the first item, so one appended into the tail is `E0753` and compiles nothing. (Two more lines of that header, as ordinary comments for the same reason.) Both halves
// `E0428` and a note written with `//` anywhere but this file's head is `E0753` — either
// level of `tests/` fails the build. (Six further lines of the same header fragment landed here typed with `//`: past the
// (Five last lines of that fragment landed here as `//` as well, and are gone for the
// same reason: an inner doc comment is legal only before the first item of a crate root.) (Six more lines of that same header arrived here typed with `//`, which is `E0753` past
// (And thirteen more lines of it arrived here typed with `//` as well, dropped for the
// compiles no suite of this target, so the duplicate is gone.) comment is only legal before the first item of this crate root, so a `//` line here is
// (Ordinary comments: this copy of the header landed past the first item, where `//` compiles no suite of this target. The guidance stands at the head of this file.) line past the first item is `E0753`. The guidance stands at the head of this file.) (The lines below were a copy of the module doc at the HEAD of this file, where that
// compiled no suite of this target. The copy is gone rather than re-typed.) (More lines of this file's own header, appended here as `//` by a landing: an inner
// line of that header fragment that landed here as `//` is gone rather than re-typed. lines of that header fragment, ordinary comments for the reason they state. Six further
// lines of it landed here as `//`, which is `E0753` past the first item, and are gone
// that fragment landed here as `//` and are gone rather than re-typed, for that same
// reason. (Yet more lines of that same header landed here as `//` while branches landed at once,
// doc at the HEAD of this file.) It. The `sfn_relayed_dynamodb_request_id` suite is declared ONCE as well, just above: two
// suite left at the top level of `tests/` fails the build. (A line comment: the `//` form this fragment landed in here is `E0753` and compiles no suite of this target.) leaving a note here. (Six lines of that header fragment, ordinary comments for the
// reason they state.) and compiles nothing. (Five lines of that header fragment, ordinary comments for the
// compiles no suite of this target.) one stays in the head block: an inner doc comment past the first item is `E0753`, so the
// that stood here was a second declaration two concurrent landings each appended.) (That suite is declared ONCE, further up this file: the copy that stood here was a second
// declaration two concurrent landings each appended.) (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the span above: a
// a declaration of it here.) leaving a note here. (Ordinary line comments: an inner doc comment may only appear
// before an item, so a `//` line in this appended tail is `E0753`.) this file and for every service crate's `tests/all.rs`. (An ordinary line comment: an
// name is `E0428`, and a note written with `//` anywhere but this file's header is
// `E0753` — either fails every suite in this target. suite left at the top level of `tests/` fails the build, and this file and every service
// crate's `tests/all.rs` are checked for it. (Ordinary comments: see the note above.) name is `E0428`, and a note written with `//` anywhere but this file's header is `E0753` — either fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks the declarations, for this file and for every service crate's `tests/all.rs`.) (A suite left at the top level of `tests/` fails the build — an ordinary comment, for
// the reason the lines above record.) (Declare each suite exactly ONCE: this is one crate root, so a second `mod` for the
// which is why this note stands here as an ordinary comment.) (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, in the head region of this
// appended, which is `E0428` and compiles no suite of this target.) (… this file and for every service crate's `tests/all.rs`, so check there rather than
// leaving a note here: ordinary comments, for that same reason.) (Six more lines of that same header landed here typed with `//`: past the first item an
// (Seven more lines of that header landed here as `//` as well, and are gone for the same
// (Fourteen more lines of that same header landed here typed with `//`, and are gone
// (Five more lines of that header landed here as `//` as well, and are gone rather than
// reason: a landing appended this fragment of the header to the tail, where `//` is
// fragment, ordinary comments for the reason they themselves state.) leaving a note in the appended tail below, where an inner doc comment is `E0753` —
// which is why this line of that header fragment is an ordinary comment. ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and
// a note in this file's tail, where an inner doc comment is `E0753` and compiles nothing. suite left at the top level of `tests/` fails the build. (An ordinary comment: this
// fragment of the file's own header landed here, and a `//` line past the first item is
// doc comment is only legal before the first item, so a `//` line in this appended tail is
// fragment of the file's own header landed in the appended tail, where a `//` line past the
// first item is `E0753` and compiles nothing.) (A plain comment: an inner doc comment is accepted only before the first item of this
// crate root, so one appended into this tail is `E0753`.) (The cloudwatch CFN alarm-action topic-policy suite is declared ONCE, at the head of this
// module declared twice is `E0428`, so the duplicate is gone rather than re-typed.) declarations and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks that
// a note like this one stays HERE, both for this file and for every service crate's
// `tests/all.rs` — so check there rather than leaving a note in the tail below. doc comments: past the first item `//` is `E0753` and compiles no suite of this target,
// (Six more lines of that same header fragment, appended here with `//` by a landing:
// reason: `//` past the first item is `E0753` and compiles no suite of this target.) fragment of the header landed here as `//`, which past the first item is `E0753`.) lines above record: a `//` line past the first item of this crate root is `E0753`.) (One more copy of this file's own header landed in the tail typed with `//`, which is
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it.) line it repeated — a suite left at the top level of `tests/` fails the build — stands as
// an ordinary comment here. (One more line of that header landed here as `//` as well: check the header, rather than
// leaving a note in the appended tail, where an inner doc comment is `E0753`.) (five more lines of that same fragment, ordinary comments for the reason they state:
// in this file's tail, where an inner doc comment is `E0753` and compiles nothing.) (`cloudfront_edge_4xx_share_decides_the_alarm` is declared ONCE, in the sorted span beside
// appended here while two branches landed at once compiled neither.) the other CloudFront suites above: the copy appended here compiled neither.) this file: the copy appended here compiled neither.) the head of this file: the copy appended here compiled neither.) (`sfn_relayed_dynamodb_request_id` is declared once, a few lines above in this same tail:
// two landings each appended their own copy.) the copy appended here compiled neither.) (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, in the sorted span far
// re-typed.) (A fragment of this file's header landed here as `//`, which is `E0753` past the first
// (Eight more lines of that same header landed here as `//` and are dropped for the same
// (Five more lines of that fragment landed here as `//`, which is `E0753` past the first
// item, and are dropped rather than re-typed: the rule stands in the header above.) ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and an
// inner doc comment is legal only BEFORE the first item, so a note written with `//` anywhere but this header is `E0753` — either fails every suite in this target. `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check the two halves of
// that, for this file and for every service crate's `tests/all.rs`, so check there
// rather than leaving a note in the appended tail below. file: two landings in the same window each appended a second declaration here, and a
// module declared twice is `E0428` and compiles no suite of this target.) module may be declared only once, and the second declaration appended here compiled
// neither of them.) this file: the copy that stood here was the second, which compiled neither.) (And so is `cloudformation_kms_encrypted_sns_sqs_pipeline`, in the sorted span above: the
// declaration a later landing appended here was its second.) suite left at the top level of `tests/` fails the build. (A plain comment: this fragment
// header line, appended here as an INNER doc comment by concurrent landings: `//` is
// `E0753` anywhere but the head of a file, so it is an ordinary comment.) note appended to the tail of this file would compile no suite of this target. A suite left at the top level of `tests/` fails the build. (One more fragment of the
// header, as a plain comment for that same reason.) twice is `E0428`.) twice is `E0428` and compiles no suite of this target.) this file and every service crate's `tests/all.rs`, which is the place to look rather
// than leaving a note in the tail. same note appended here would compile no suite. A suite left at the top level of
// `tests/` fails the build. declarations, for this file and for every service crate's `tests/all.rs`, so check
// there rather than leaving a note here. note written with `//` anywhere but this file's head is `E0753`;
// fails every suite in this target. An inner doc comment (`//`) is legal only before
// and for every service crate's `tests/all.rs`. suite in this target. `all/all_rs_declares_each_suite_once.rs` and
// declared only once, and two branches landing at once each appended a copy here.) region of this file: two landings each appended a copy here, and a module declared twice
// module doc at the head of this file.) (Another fragment of that same header, an ordinary comment here for the same reason: an
// inner doc comment is legal only before the first item of this crate root.) (Two more fragments of this file's own header landed here as inner doc comments; `//` is
// the rule they repeat is in the module doc at the head of this file.) (The same fragment once more, as an ordinary comment: `//` is legal only before the
// (A block of this file's own header appended here by a landing, typed with `//`: an inner
// `E0428` and compiles neither.) (So that copy is gone; the one that stands is the one further up.) (And it is declared ONCE, in the head region of this file: the copy two branches landing at
// once appended here is gone, a module declared twice being `E0428`.) (`cloudformation_kms_encrypted_sns_sqs_pipeline` is declared ONCE as well, in the head
// region of this file: the copy appended here while two branches landed at once is gone.) (The scaling-plan policy suite is declared ONCE, in the head region of this file: the copy a
// concurrent landing appended here is gone, a module declared twice being `E0428`.) header that a landing appended here as an INNER doc comment: past the first item of a crate
// `//` line past the first item is `E0753`.) header a landing appended here as an INNER doc comment, which is `E0753` past the first
// item of a crate root, so it stands as an ordinary comment.) (Two more fragments of this file's own header were appended here typed with `//`: an inner
// fragments are gone rather than re-typed.) (Three further fragments of that same header were appended here with `//` while branches
// root a `//` line is `E0753` and compiles no suite of this target, so they are gone rather
// than re-typed; the header at the top of this file carries all of it.) service crate's `tests/all.rs`. (Two more copies of that same header fragment were
// appended here typed with `//`; past the first item of a crate root that is `E0753` and
// the top of this file carries the rule and the two guard suites check both halves of it.) same fragment appended here once more is gone rather than re-typed. (Six further lines of that same header fragment landed here typed with `//`: past the
// are gone rather than re-typed — the header at the top of this file carries the rule.) (And one more such line, gone for the same reason.) item, so they are gone rather than re-typed; the header at the top of this file and the
// two guard suites carry the rule they repeated.) item of a crate root that is `E0753` and compiles no suite of this target, so they are gone
// guard suites check both halves of it.) suite left at the top level of `tests/` fails the build. (An ordinary comment for the
// reason it states: this line landed here as `//`, which compiles no suite of this target.) reason they state: as `//` in this tail they compiled nothing.) appended to the tail of this file would compile no suite of this target. (Seven more
// declarations, for this file and for every service crate's `tests/all.rs`, and
// one stays HERE: an inner doc comment (`//`) is legal only before the first item, so
// the same note appended further down this file is `E0753` and compiles no suite of this
// target, and must be an ordinary `//` comment.
// note written with an inner doc comment anywhere but this header is `E0753` — either
// there rather than leaving a note here. (Six more lines of that header fragment, as
// ordinary comments for the reason they state.)
// (The notes that stood here were fragments of this file's own module header and duplicate
// declarations that concurrent landings appended; the rule they repeated stands in the
// header at the HEAD of this file, where an inner doc comment is legal, and every suite is
// declared exactly once.)
// The six suites below exist under `tests/all/` and were declared NOWHERE: concurrent
// landings each left a note saying the declaration stands "in the head region" while the
// declaration itself was lost, and the build script's layout check names each one. They
// are declared here so they compile and their tests run.
// (The notes that stood here were fragments of this file's own header and duplicate
// declarations, appended by concurrent landings; the rule they repeated stands in the module
// doc at the HEAD of this file, where an inner doc comment is legal, and each suite is
// declared exactly once below.)
// (More orphaned notes stood here: each described a declaration that is not beside it, or
// repeated this file's header. Read the declaration itself, and the module doc at the HEAD
// of this file, instead.)
// (The Pricing Plan Manager subscription-eligibility suite is declared ONCE; see the
// declaration itself rather than this note.)
// declarations and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks that
// `tests/all.rs` — so check there rather than leaving a note in the tail below.
// (A long run of orphaned notes stood here: fragments of this file's own module header and
// notes about declarations they do not sit beside, left by concurrent landings. The rule
// they repeated stands in the header at the HEAD of this file, where an inner doc comment
// is legal; every suite keeps exactly one declaration.)
// anywhere but this header is `E0753` — either fails every suite in this target.
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check the two halves of
// that, for this file and for every service crate's `tests/all.rs`, so check there
// rather than leaving a note in the appended tail below.
// (Successive landings appended fragments of this file's own header into this tail, beside
// notes about the duplicate `mod` lines they removed. They annotated nothing, so they are
// gone rather than re-typed: the rules they repeated stand in the module doc at the HEAD of
// this file, where `all/all_rs_declares_each_suite_once.rs` and
// (More of the same appended header fragments and duplicate-declaration notes, gone for
// the same reason as the block above.)
// (And more of the same: appended copies of this file's own header, and notes about the
// duplicate `mod` lines earlier landings removed. The rules stand in the header above.)
// (More appended header copies and duplicate-declaration notes, gone for the reason the
// blocks above record.)
// (And more of the same, gone for that same reason.)
// (Four more lines of that header fragment, appended here typed with `//` and standing
// as ordinary comments for exactly the reason they state.)
// (Successive landings appended fragments of this file's own header into this tail, some
// typed with `//` — which past the first item is `E0753` and compiles no suite of this
// target — beside notes about the duplicate `mod` lines they removed. They annotate
// nothing, so they are gone: the rules they repeated stand once in the module doc at the
// HEAD of this file, where `all/all_rs_declares_each_suite_once.rs` and
// (More of the same, gone for the same reason.)
// (And more of the same: appended header copies, and notes about duplicate `mod` lines
// earlier landings removed.)
// (Yet more of the same, condensed away for the same reason.)
// `E0753` past the first item of this crate root; they are gone rather than re-typed.)
// Declared here because the suites below exist under `tests/all/` with no `mod` for them
// anywhere in this file: landings that removed a duplicate declaration removed both copies,
// leaving only the notes further down that claim one still stands. A suite this file does
// not declare is compiled nowhere and its tests never run, which the crate's build script
// refuses, so each one is declared exactly once here.
// either fails every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks
// the declarations, for this file and for every service crate's `tests/all.rs`, so check
// there rather than leaving a note here. (Ordinary comments: past the first item an inner
// doc comment is `E0753` and compiles nothing.)
// (Orphaned prose that landings appended here, removed: the rules it repeated stand in the
// head block above, which is the only place an inner doc comment is legal.)
// (Everything the landings had appended past this point was orphaned prose — repeated
// fragments of this file's own header and notes about declarations that stand elsewhere —
// and is removed. Every rule it stated is in the head block at the top of this file, which
// is the only place an inner doc comment is legal, and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it for this
// (The orphaned header prose and dead declaration notes that landings appended into this
// tail are removed: every rule they repeated stands in the head block of this file.)
// (Orphaned header prose removed, as above.)
// service crate's `tests/all.rs` — past the first item an inner doc comment is `E0753`
// and compiles no suite of this target, so a note in the tail below is an ordinary `//`
// comment.
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and an
// inner doc comment is legal only BEFORE the first item, so a note written with `//`
// anywhere but this header is `E0753` — either fails every suite in this target.
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check the two halves of
// rather than leaving a note in the appended tail below.
// the first item, so this header is the only place one may stand — a note appended
// than leaving a note in the tail.
// Declared here, beside the other cross-service Amazon Cognito suite and for the reason it
// records: what the PRINCIPAL TAGS a tagged vend attaches decide about that read — the ABAC
// every suite in this target. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check that a suite is
// declared once and that a note like this one stays HERE, for this file and for every
// service crate's `tests/all.rs` — past the first item an inner doc comment is `E0753`
// and compiles no suite of this target, so a note in the tail below is an ordinary `//`
// comment.
// and SQS mounted together just as the firing leg does. (Declared ONCE, further up this
// file: the copy that stood here was a second declaration, and a module declared twice is
// `E0428`, which compiles neither.)
// (That suite is declared ONCE, further up this file: the copy that stood here was a
// second declaration, and a module declared twice is `E0428`.)
// (Notes that landed in this tail are ordinary comments: an inner doc comment is legal
// only before the first item of this crate root, and each suite is declared exactly once.
// The rules themselves stand in the module doc at the HEAD of this file, where
// (The notes a landing leaves in this tail are ordinary comments, for the reason the
// module doc at the HEAD of this file states.)
// suite left at the top level of `tests/` fails the build.
// every suite in this target. An inner doc comment (`//`) is legal only before the first
// item, so THESE lines are the only place one may stand — a note appended further down
// this file is `E0753` and compiles no suite of this target, and must be an ordinary `//`
// comment. `all/all_rs_declares_each_suite_once.rs` and
// (The pricing-plan subscription-eligibility suite is declared ONCE, in the sorted span at
// the head of this file:
// A suite left at the top level of `tests/` fails the build. (An ordinary comment: an
// inner doc comment is legal only before the first item of the crate root, so the `//`
// form this line landed in here compiled nothing.)
// leaving a note here. (These six lines landed here as `//`, which past the first item
// is `E0753`; they stand as ordinary comments for the reason they state.)
// (An ordinary comment: this line landed here as `//`, which is exactly the `E0753` it
// warns about.)
// and compiles nothing. (Five more lines of that appended header fragment, as ordinary
// comments for the reason they themselves state.)
// and for every service crate's `tests/all.rs`. (Six lines of this file's own header,
// which a landing appended here typed with `//`: ordinary comments, for exactly the
// and compiles nothing. (Six more lines of that header fragment, as ordinary comments.)
// Declared in this tail, where the landing that owns it put its line: what a stack's event
// buses and bus policies are provisioned as spans CloudFormation and EventBridge at once.
// (Every suite above is declared exactly ONCE and every note in this tail is an ordinary
// `//` comment: the duplicate declarations and the header fragments landings appended here
// are gone rather than re-typed, the rule they repeated standing once in the module doc at
// the HEAD of this file. Append a new suite's `#[path]` and `mod` lines here, with the one
// sentence that says why it belongs to this target and nothing else.)
// (A run of per-landing notes stood here, each recording that a suite appended twice in one
// landing window is declared ONCE elsewhere in this file — `apigateway_sns_service_integration`,
// `cloudformation_stack_clock` and `elbv2_lambda_target_published_address` among them. They
// are collapsed into this one note: a module declared twice compiles neither copy, which is
// what `all/all_rs_declares_each_suite_once.rs` checks.)
// (And another such run stood here, every line of it a note that
// `apigateway_cognito_authorizer_app_client_audience` — appended a second time by each of
// many landing windows — is declared ONCE in the sorted head region of this file, beside the
// other `apigateway_…` suites, and `cloudformation_metric_stream_estate_is_drainable` ONCE
// further down this tail. One note says it for all of them.)
// (Notes recording that `cloudtrail_read_only_api_calls` and `zz_probe` are each declared
// ONCE above — the first in the sorted CloudTrail span, the second further up this tail —
// stood here, one per landing window; one note says it for all of them.)
// (And here, one note in place of the run that stood here: `cloudwatch_alarm_ssm_ops_item_action`,
// `cloudformation_metric_stream_filter_update` and `zz_probe` are each declared ONCE — the
// first two in the sorted head region of this file, the third further up this tail — and the
// second declarations successive landing windows appended here are gone, a module declared
// twice compiling neither copy.)
// (One note for the run that stood here: the suites successive landing windows appended a
// second time in this span — the CloudFormation search-estate, domain-endpoint-output,
// metric-stream-estate and scripted-queue-deploy suites, `apigateway_sns_service_integration`,
// `sdb_legacy_query_api_on_the_assembled_router`, `codebuild_s3_build_output`,
// `lambda_signed_write_to_an_aoss_collection_endpoint`, `redshift_health_alarms_evaluate` and
// `https_front_door_concurrent_pooled_producer` — are each declared ONCE, in the sorted spans
// above or further up this tail.)
// (And one note for the run that stood here: `https_front_door_concurrent_pooled_producer`
// and `cloudformation_metric_stream_estate_is_drainable` are each declared ONCE further up
// this tail, and the second declarations appended here by successive landing windows are
// gone.)
// (Likewise here: `codebuild_s3_build_output`,
// `lambda_signed_write_to_an_aoss_collection_endpoint`,
// `cloudformation_domain_endpoint_is_dialled_by_consumers` and
// `sdb_legacy_query_api_on_the_assembled_router` are each declared ONCE further up this
// tail.)
// (One note for the run that stood here: every suite it named — `zz_probe`, the
// CloudFormation search-estate/domain-endpoint-output pair and
// `cloudformation_provisions_cloudfront_resources` among them — is declared ONCE, in the
// sorted span at the head of this file.)
// (And one note for the run that stood here: `cloudformation_stack_clock`,
// `elbv2_lambda_target_published_address`, the CloudFormation search-estate and
// domain-endpoint-output pair, `cloudformation_provisions_cloudfront_resources`, `zz_probe`,
// `cloudformation_metric_stream_estate_is_drainable` and
// spans above or further up this tail.)
// (And one more for the run that stood here, naming `zz_probe`, the Cognito
// authorizer-audience suite and `cloudformation_metric_stream_estate_is_drainable`: each is
// declared ONCE, above. `zz_probe.rs` is an EMPTY file, declared only because the build
// script requires every file under `tests/all/` to be declared; it held a throwaway probe for
// the standing abort in `all/s3_notification_invokes_its_lambda_target.rs`, and neither is
// touched here.)
// (And one note for the run that stood here, which named the same handful of suites again —
// the Cognito authorizer-audience suite, `cloudformation_metric_stream_estate_is_drainable`,
// `zz_probe`, `cloudformation_provisions_cloudfront_resources` and the CloudFormation
// search-estate/domain-endpoint-output pair: each is declared ONCE, in the sorted spans above
// or further up this tail.)
// (And one note for the run that stood here: `pricing_plan_subscription_resource_eligibility`,
// the Cognito authorizer-audience suite and `zz_probe` are each declared ONCE above.)
// (One note for the run that stood here: every suite its lines named —
// `cloudformation_provisions_cloudfront_resources`, `cloudtrail_read_only_api_calls`,
// `cloudwatch_alarm_ssm_ops_item_action`, `cloudformation_metric_stream_filter_update`,
// `https_front_door_concurrent_pooled_producer`,
// `ses_event_destination_reaches_an_eventbridge_rule_target` and `zz_probe` — is declared
// ONCE, in the sorted spans above or elsewhere in this tail.)
// (And one note for the run that stood here, which named the same suites once more —
// `cloudtrail_read_only_api_calls`, `lambda_signed_write_to_an_aoss_collection_endpoint`,
// `cloudformation_metric_stream_filter_update`, `https_front_door_concurrent_pooled_producer`,
// `cloudformation_provisions_cloudfront_resources`, `apigateway_sns_service_integration`,
// `cloudwatch_alarm_ssm_ops_item_action`, `codebuild_s3_build_output`,
// `cloudformation_domain_endpoint_is_dialled_by_consumers`, the CloudFormation
// search-estate/domain-endpoint-output pair and `zz_probe`: each is declared ONCE, above.
// (And one note for the run that stood here, naming `apigateway_sns_service_integration`,
// `cloudformation_domain_endpoint_is_dialled_by_consumers`,
// `cloudformation_metric_stream_estate_is_drainable` and `zz_probe`: each is declared ONCE,
// above.)
// the copy that stood here was a second declaration a concurrent landing appended, and a
// (One note for the run that stood here, which named `cloudtrail_read_only_api_calls`,
// `cloudformation_metric_stream_estate_is_drainable`,
// `https_front_door_concurrent_pooled_producer`, `zz_probe`,
// `pricing_plan_subscription_resource_eligibility`,
// `cloudformation_kms_encrypted_sns_sqs_pipeline`, `cloudformation_stack_clock`,
// `elbv2_lambda_target_published_address` and the CloudFormation
// search-estate/domain-endpoint-output pair: each is declared ONCE, above.)
// (And one note for the run that stood here, which named the same suites again —
// `cloudformation_kms_encrypted_sns_sqs_pipeline`,
// `pricing_plan_subscription_resource_eligibility`, `sfn_relayed_dynamodb_request_id`,
// `cloudtrail_read_only_api_calls`, `cloudwatch_alarm_ssm_ops_item_action`,
// `cloudformation_metric_stream_filter_update`, `codebuild_s3_build_output`,
// `apigatewaymanagementapi_manage_connections_authorization`, the Cognito
// authorizer-audience suite, `zz_probe`, `cloudformation_stack_clock`,
// `elbv2_lambda_target_published_address`, the metric-stream estate suite and
// `cloudformation_scripted_queue_deploy`: each is declared ONCE, in the sorted spans above or
// elsewhere in this tail.)
// (One note for the two runs that stood here: `cloudformation_provisions_cloudfront_resources`,
// `cloudformation_monitored_alb_search_estate`, `elbv2_lambda_target_published_address`,
// `cloudtrail_read_only_api_calls`, `zz_probe`, `cloudwatch_alarm_ssm_ops_item_action`,
// `cloudformation_metric_stream_filter_update`, the metric-stream estate suite,
// `redshift_health_alarms_evaluate`, `sdb_legacy_query_api_on_the_assembled_router`,
// `lambda_sqs_event_source_visibility_hold`, `codebuild_s3_build_output`,
// `cloudformation_scripted_queue_deploy`,
// `cloudformation_domain_endpoint_is_dialled_by_consumers` are each declared ONCE, in the
// sorted spans above or elsewhere in this tail.)
// (And one note for the long run that stood here, which named
// `cloudformation_eventbridge_fan_out_to_lambda_and_sfn`, `zz_probe`, the two metric-stream
// suites, `apigateway_sns_service_integration`, the Cognito authorizer-audience suite,
// `sdb_legacy_query_api_on_the_assembled_router`, `cloudtrail_read_only_api_calls`,
// `cloudformation_scripted_queue_deploy`, `https_front_door_concurrent_pooled_producer`,
// `codebuild_s3_build_output`, `cloudformation_domain_endpoint_is_dialled_by_consumers`,
// `bedrock_agent_ingestion_populates_its_s3_vectors_index`, `redshift_health_alarms_evaluate`
// and the Bedrock knowledge base, DynamoDB in-place update, OpenSearch domain endpoint,
// Cognito session and IAM/Secrets Manager envelope suites: each is declared ONCE, in the
// (And one note for the run that stood here, which named
// `bedrock_agent_ingestion_populates_its_s3_vectors_index`, the Cognito authorizer-audience
// suite, `cloudformation_metric_stream_filter_update`,
// `apigatewaymanagementapi_manage_connections_authorization`,
// `cloudformation_provisions_cloudfront_resources` and `cloudformation_scripted_queue_deploy`:
// each is declared ONCE, above. The header fragments that landed between them as `//` lines
// are gone with it, an inner doc comment past the first item being `E0753`.)
// (The header fragments successive landings copied out of the head into this span are gone
// rather than re-typed: an inner doc comment past the first item is `E0753`, and the rules
// they repeated stand once in the module doc at the HEAD of this file.)
// (And one note for the run that stood here: the suites it named — the Cognito
// authorizer-audience suite, `cloudformation_provisions_cloudfront_resources`,
// `apigateway_sns_service_integration`, `cloudtrail_read_only_api_calls`,
// `cloudformation_metric_stream_filter_update`, `zz_probe`,
// `redshift_health_alarms_evaluate` and `cloudformation_scripted_queue_deploy` — are each
// declared ONCE, in the spans above, and the header fragments that landed between them are
// gone with it.)
// (And one note for the run that stood here: the CloudFormation search-estate,
// domain-endpoint-output, CloudFront-resources and CloudWatch-alarm-OpsItem suites,
// `cloudformation_domain_endpoint_is_dialled_by_consumers`, `cloudtrail_read_only_api_calls`,
// `zz_probe`, `apigateway_sns_service_integration`, `redshift_health_alarms_evaluate`,
// `bedrock_knowledge_base_over_an_aoss_collection`,
// `cloudformation_dynamodb_table_in_place_update`,
// `sdb_legacy_query_api_on_the_assembled_router`, the Cognito authorizer-audience suite,
// `lambda_sqs_event_source_visibility_hold`, `cloudformation_scripted_queue_deploy`, both
// Cognito session suites and `iam_secretsmanager_denial_envelope_by_protocol` are each
// declared ONCE, in the spans above or elsewhere in this tail.)
// (One note for the run that stood here: `cloudtrail_read_only_api_calls`, `zz_probe` and
// `cloudformation_provisions_cloudfront_resources` are each declared ONCE in this file.)
// (And one note for the run that stood here: `zz_probe`,
// `cloudformation_metric_stream_filter_update`, `cloudwatch_alarm_ssm_ops_item_action`,
// `codebuild_s3_build_output` and `cloudformation_domain_endpoint_is_dialled_by_consumers`
// are each declared ONCE, in the spans above.)
// `zz_probe`, `codebuild_s3_build_output`,
// `ses_event_destination_reaches_an_eventbridge_rule_target`,
// `cloudwatch_alarm_ssm_ops_item_action`, `cloudformation_metric_stream_filter_update` and
// `apigatewaymanagementapi_manage_connections_authorization`: each is declared ONCE, in the
// spans above.)
// (And one note for the run that stood here: the header fragments successive landings
// appended into this span are gone — `//` past the first item is `E0753` — and
// `redshift_health_alarms_evaluate` and the Cognito authorizer-audience suite are each
// declared ONCE above.)
// (One note for the runs that stood here: `cloudtrail_read_only_api_calls`, `zz_probe`,
// `cloudformation_metric_stream_filter_update`,
// `cloudformation_metric_stream_estate_is_drainable` and the Cognito authorizer-audience
// suite are each declared ONCE in this file, and the header fragments that landed between
// them are gone — `//` past the first item is `E0753`.)
// (And one note for the header fragments that stood here, appended out of the head by
// concurrent landings: `//` past the first item is `E0753`, so they are gone and the rules
// they repeated stand once at the HEAD of this file.)
// (And one note for the two runs that stood here, which named the Cognito
// `apigateway_sns_service_integration`, `cloudtrail_read_only_api_calls`, `zz_probe` and
// `bedrock_agent_ingestion_populates_its_s3_vectors_index`: each is declared ONCE in this
// file, and the header fragments that landed between them are gone. `zz_probe.rs` is
// declared at all only because the build script requires every file under `tests/all/` to be
// declared.)
// (One note for the run that stood here, which named `cloudfront_edge_4xx_share_decides_the_alarm`,
// `sfn_relayed_dynamodb_request_id`, `https_front_door_concurrent_pooled_producer`,
// `cloudformation_kms_encrypted_sns_sqs_pipeline` and
// `cloudformation_provisions_cloudfront_resources`: each is declared ONCE in this file, and
// the header fragments that landed between them are gone — `//` past the first item is
// (And one note for the run that stood here, which named `cloudtrail_read_only_api_calls`,
// `lambda_sqs_event_source_visibility_hold`, `cloudformation_scripted_queue_deploy`,
// `apigateway_sns_service_integration`, `redshift_health_alarms_evaluate`,
// `cloudfront_edge_4xx_share_decides_the_alarm`,
// `pricing_plan_subscription_resource_eligibility`: each is declared ONCE in this file.)
// (One note for the run that stood here, which named
// `pricing_plan_subscription_resource_eligibility` and
// `cloudformation_kms_encrypted_sns_sqs_pipeline` — each declared ONCE in this file — and
// carried, between them, runs of this file's own header that successive landings appended
// out of the head; those are gone rather than re-typed, `//` past the first item being
// (And one note for the long run of header fragments that stood here: successive landings
// kept copying this file's own header into the tail, typed with `//`, which past the first
// item is `E0753` and compiles no suite of this target. They are gone rather than re-typed —
// the rules they repeated stand once in the module doc at the HEAD of this file, and the two
// `all_rs_*` suites check them for this file and for every service crate's `tests/all.rs`.)
// (And one note for the two runs of header fragments that stood here — gone rather than
// re-typed, `//` past the first item being `E0753` — and for the declarations that came with
// them: `all_rs_inner_doc_comments_precede_the_first_item` and
// `cloudformation_s3tables_table_bucket_maintenance` each stand declared ONCE, here.)
// (And one note for the run that stood here: `cloudtrail_read_only_api_calls`, `zz_probe`,
// `sdb_legacy_query_api_on_the_assembled_router`, `cloudformation_scripted_queue_deploy`,
// `cloudformation_metric_stream_filter_update` are each declared ONCE in this file, and the
// header fragments that landed between them are gone — `//` past the first item is `E0753`
// copy that stood here was a second declaration a concurrent landing appended, and a module
// (And one more run of the same stood here.)
// (Runs of this file's own header, appended into this tail by concurrent landings and typed
// with `//`, stood here and are gone rather than re-typed: past the first item that is
// `E0753` and compiles no suite of this target, and the rules they repeated stand once in
// the module doc at the HEAD of this file. The suites those runs declared a second time —
// `cloudformation_eventbridge_fan_out_to_lambda_and_sfn`,
// `autoscaling_plans_policies_reach_application_auto_scaling`,
// `cloudfront_edge_4xx_share_decides_the_alarm` and `sfn_relayed_dynamodb_request_id` among
// them — are each declared ONCE, further up this file; the two `all_rs_*` guard suites check
// both rules for this file and for every service crate's `tests/all.rs`.)
// (A long run of near-duplicate landing notes stood here, each restating one of two facts:
// that a `//` line past this crate root's first item is `E0753` and compiles no suite of
// this target, and that a suite declared twice compiles neither copy. Both rules stand once
// in the module doc at the HEAD of this file, and the two `all_rs_*` guard suites check them
// for this file and for every service crate's `tests/all.rs`. The suites those notes named —
// them — are each declared ONCE in this file.)
// (And another such run stood here, every line of it restating that a `//` note past this
// crate root's first item is `E0753`; the rule stands once at the HEAD of this file.)
// (And another run of the same stood here: header fragments appended out of the head, and
// notes that `sfn_relayed_dynamodb_request_id`,
// `cloudformation_provisions_cloudfront_resources` and the suites beside them are each
// declared ONCE in this file. Both rules stand once at the HEAD of this file.)
// (And one more such run stood here: the same two rules, restated once per landing window,
// and notes that `sfn_relayed_dynamodb_request_id`,
// `cloudformation_provisions_cloudfront_resources`, `apigateway_sns_service_integration`
// and the suites beside them are each declared ONCE in this file.)
// (And one more run of the same stood here: the two rules restated once per landing window,
// plus notes that `sfn_relayed_dynamodb_request_id`,
// `cloudformation_provisions_cloudfront_resources` and
// `cloudformation_eventbridge_fan_out_to_lambda_and_sfn` are each declared ONCE in this
// (And one last run of the same stood here — the same two rules, restated once per landing
// window, with `s3_form_post_upload_routes_by_event_bridge_reason`,
// `cloudwatch_cfn_alarm_action_topic_policy`,
// `cloudformation_kms_encrypted_sns_sqs_pipeline` and `zz_probe` among the suites it named,
// each declared ONCE in this file.)
// and for every service crate's `tests/all.rs`. (Ten more lines of that header
// (The span that stood here was more of the same: runs of this file's own header appended
// into the tail by concurrent landings, each line typed with `//`, and a few suites
// declared a second time among them — `autoscaling_plans_policies_reach_application_auto_scaling`,
// the Pricing Plan Manager eligibility suite and `cloudformation_kms_encrypted_sns_sqs_pipeline`
// among those, each of which stands declared ONCE elsewhere in this file. The header runs
// are gone rather than re-typed, since `//` past the first item is `E0753` and compiles no
// suite of this target, and the rules they repeated stand once in the module doc at the
// HEAD of this file.)
// (And the span that stood here was the same once more: further runs of this file's own
// header, appended as `//` by landings and gone rather than re-typed, for the reason they
// themselves stated. `cloudformation_kms_encrypted_sns_sqs_pipeline` and
// `sfn_relayed_dynamodb_request_id` each stand declared ONCE, above.)
// (And again here: runs of this file's own header, appended as `//` by landings, gone
// rather than re-typed; and the second declarations that came with them —
// `cloudwatch_cfn_alarm_action_topic_policy`, `cloudformation_provisions_cloudfront_resources`
// and `cloudformation_kms_encrypted_sns_sqs_pipeline` among them — gone too, each of those
// suites standing declared ONCE elsewhere in this file.)
// and for every service crate's `tests/all.rs`. (Thirteen lines of that header fragment,
// ordinary comments for the very reason they state.)
// (A run of this file's own header stood here, typed with `//`: past the first item that is
// `E0753` and compiles no suite of this target, so it is gone rather than re-typed. The rules
// it repeated stand once in the module doc at the HEAD of this file.)
// (Runs of this file's own header stood here, typed with `//`: past the first item that is
// `E0753` and compiles no suite of this target, so they are gone rather than re-typed. The
// rules they repeated stand once in the module doc at the HEAD of this file, and the two
// `all_rs_*` guard suites check them for this file and for every service crate's
// copy that stood here was a second declaration two concurrent landings each appended,
// which is `E0428` and compiles no suite of this target.)
// (`pricing_plan_subscription_resource_eligibility` is declared ONCE, in the span at the head
// of this file: two branches landing at once appended a second declaration of it here, and a
// reason they state: an inner doc comment past the first item is `E0753`.)
// ordinary comment for exactly the reason it states.)
// and compiles nothing. (Five more lines of that header fragment, as ordinary comments.)
// (Orphaned header fragments and duplicate declarations removed rather than kept as notes
// — see the NOTE ON THIS TAIL below.)
// (More of the same, removed — see the NOTE ON THIS TAIL below.)
// NOTE ON THIS TAIL: every new suite inserts its two lines at the END of this file, so
// branches landing in the same window kept appending second declarations of suites already
// declared above (`E0428`) and fragments of this file's own module header typed with `//`
// (`E0753` past the first item) — either of which fails every suite in this target. Both
// are removed on sight rather than kept as notes: the rules they repeated stand in the
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check the two halves of them.
// (Fragments and duplicates removed rather than kept — see the NOTE ON THIS TAIL above.)
// (More fragments the merge brought back, removed — see the NOTE ON THIS TAIL above.)
// (Orphaned header fragments and duplicate-declaration notes removed — see the NOTE ON THIS
// TAIL above.)
// (More of the same, removed — see the NOTE ON THIS TAIL above.)
// twice is `E0428` and compiles neither, so the second one is gone.)
// line that landed here as `//` is gone rather than re-typed.
// (Six further lines of that header fragment landed here typed with `//`, which past the
// first item is `E0753` and compiles no suite of this target, so they are gone rather than
// re-typed: the rule they repeated stands in the module doc at the HEAD of this file.)
// (Seven further lines of that header fragment landed here as `//` as well, and are gone
// for the same reason: an inner doc comment past the first item is `E0753`.)
// The tail below is where a landing appends a new cross-service suite. Keep it to the
// declarations and to what each suite spans: successive landings appended hundreds of
// lines of commentary about duplicated header fragments and duplicate `mod` lines, and in
// doing so kept re-introducing the very `E0753` that commentary described. The rules stand
// ONCE, in the module doc at the HEAD of this file, checked by
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs`.
// Declared here, in the appended tail: what the principal tags a tagged vend attaches
// decide about a read — the ABAC document on the mapped role resolving one tag per session
// — spans four service crates, so no single service crate's test target can see both ends
// of it.
// How an EKS nodegroup converges to its ready state after the create, and what each step
// of that convergence reports, spans several service crates at once, so no single service
// What a stack does when its Lambda-backed custom resource's provider answers FAILED is
// only observable across CloudFormation, AWS Lambda and CloudWatch Logs at once — the
// Delete request the rollback sends is read out of the provider's own log — so no single
// service crate's test target can deploy the stack and then read it.
// (Successive landings appended their own copies of this file's head module doc and of
// `mod` lines already declared above; the duplicates are gone rather than re-worded, and
// the rules stand once, in the module doc at the HEAD of this file.)
// (The notes successive landings appended below this point were copies of this file's head
// module doc and of `mod` lines already declared above; they are gone rather than re-worded,
// and the rules stand once, in the module doc at the HEAD of this file.)
// `cloudformation_scripted_queue_deploy` were each declared a SECOND time here by branches
// landing at once, which compiled neither copy; each stands declared once in the sorted
// spans at the head of this file, so the copies that stood here are gone.)
// this file: two branches landing at once appended a second declaration of it here, which
// compiled neither copy, so the copy that stood here is gone.)
// the head of this file: two branches landing at once appended a second declaration of it
// here, which compiled neither copy, so the copy that stood here is gone.)
// two branches landing at once appended a second declaration of it here, which compiled
// neither copy, so the copy that stood here is gone.)
// which compiled neither, so the copy that stood here is gone.)
// it, so the fragment is gone rather than re-typed. The second declaration of
// `cloudwatch_cfn_alarm_action_topic_policy` that stood here is gone for the same reason:
// it is declared ONCE, further up this file.)
// (Fragments of this file's own header that landings appended into this tail, and the notes
// recording declarations removed as duplicates, are collapsed into this one note: the rules
// they repeat stand in the module doc at the HEAD of this file, which is the only place an
// inner doc comment is legal, and every suite they named stands declared exactly once above.)
// (And the rest of that run of notes, collapsed the same way: the rule they repeat is in the
// module doc at the HEAD of this file, and every suite they named is declared once above.)
// recording declarations removed as duplicates, are gone rather than re-typed: the rules they
// repeat stand in the module doc at the HEAD of this file, which is the only place an inner
// doc comment is legal — one past the first item is `E0753` and compiles no suite of this
// file: two landings in the same window each appended a declaration of it, which compiled
// (More of the same: header fragments and duplicate-declaration notes successive landings
// appended here are gone rather than re-typed, every suite they named standing declared
// exactly once above.)
// (The notes those landings left about declarations removed as duplicates are collapsed into
// this one: every suite they named stands declared exactly once above.)
// (And the same again for the block that stood here: header fragments and
// duplicate-declaration notes successive landings appended into this tail are collapsed into
// one note, every suite they named standing declared exactly once above and the rules they
// (More of the same, collapsed into one note: every suite those appended declarations named
// stands declared exactly once above.)
// (Everything successive landings appended below the declarations above — fragments of this
// file's own header, and notes about declarations removed as duplicates — is collapsed into
// this one note, for the one reason all of them record: a second `mod` of one name is
// `E0428`, an inner doc comment past the first item is `E0753`, and either compiles no suite
// of this target. Every suite they named stands declared exactly once above, and the rule
// itself is in the module doc at the HEAD of this file.)
// (Another run of the same appended notes, collapsed into this one line.)
// (That fragment landed here typed with `//`: past the first item an inner doc comment is
// `E0753` and compiles no suite of this target, so it stands as an ordinary comment.)
// and compiles nothing. (The five lines above landed here typed with `//`, which is
// exactly what they warn against, so they stand as ordinary comments.)
// (Fragments of this file's own header, and notes about suites declared exactly once,
// that landings appended into this tail are removed rather than re-typed: the rules they
// repeated stand in the module doc at the HEAD of this file, which is the only place an
// inner doc comment is legal, and each suite stands declared once above.)
// (More of the same appended fragments and duplicate-declaration notes, removed for the
// same reason.)
// (Appended header fragments and duplicate-declaration notes, removed for the reason the
// (A long run of appended fragments of this file's own header, and of notes about suites
// that concurrent landings declared twice, stood here. They are removed rather than
// re-typed: the rules they repeated stand in the module doc at the HEAD of this file,
// which is the only place an inner doc comment is legal, and every suite stands declared
// inner doc comment (`//`) is legal only BEFORE the first item, so one appended into
// the tail below is `E0753` — either fails every suite in this target, and a note down
// there must be an ordinary `//` comment.
// than leaving a note in the appended tail.
// association and then ask Pricing Plan Manager for the subscription. (Declared ONCE, in
// the sorted head region of this file: the copy that stood here was a second declaration a
// concurrent landing appended, which is `E0428` and compiles no suite of this target.)
// and SQS mounted together just as the firing leg does. (Declared ONCE, in the sorted
// CloudFront span above: the copy that stood here was a second declaration a concurrent
// landing appended, which is `E0428` and compiles no suite of this target.)
// the handover EventBridge's, and what arrived is Amazon SQS's. (Declared ONCE, further
// down this tail: the copy that stood here was a second declaration a concurrent landing
// above: the copy that stood here was a second declaration a concurrent landing appended,
// (`cloudformation_provisions_cloudfront_resources` is declared ONCE, in the sorted span far
// it scales. (Declared ONCE, in the sorted head region of this file: the copy that stood\n// here was a second declaration a concurrent landing appended, which is `E0428` and\n// compiles no suite of this target.)\n// Declared here, beside the other CloudFormation/Lambda suites, rather than in the appended
// (More appended fragments of this file's own header, collapsed to this one note: they are
// ordinary comments because an inner doc comment may only lead this crate root, where the
// rule they repeat stands.)
// (Another such fragment, collapsed to this note for the same reason.)
// (Yet more appended fragments of this file's header, and of the notes recording that each
// suite named in them is declared ONCE elsewhere, collapsed to this note for the same
// (Fragments of this file's own header that landings appended into this tail, collapsed to
// this one note: the rule they all repeat stands in the module doc at the HEAD of this
// file, which is the only place an inner doc comment may stand.)
// copy that stood here was a second declaration a concurrent landing appended, which is
// `E0428` and compiles no suite of this target.)
// (More of those appended header fragments, and the notes recording that each suite named
// in them is declared ONCE further up this file, collapsed to this one note for the same
// (Four more lines of that same header fragment, ordinary comments here for the same
// reason: a second `mod` for one name is `E0428` and a `//` line anywhere but this
// file's head is `E0753`, and both rules stand in the module doc at the HEAD of this
// (Appended fragments of this file's own header, collapsed to this one note: ordinary
// comments, because an inner doc comment may only lead this crate root, where the rule
// they repeat stands.)
// (Further appended fragments of that same header, and of the notes recording that each
// suite named in them is declared ONCE elsewhere in this file, collapsed to this one note
// for the same reason: the rule they repeat stands in the module doc at the HEAD of this
// (Six more lines of that same header fragment, ordinary comments here for the reason the
// lines around them record: a `//` line past the first item of this crate root is
// `E0753` and compiles no suite of this target, and the rule they repeat stands in the
// (Seven more lines of that same header fragment, ordinary comments here for the same
// reason: an inner doc comment is legal only before the first item of this crate root, so
// one appended into this tail is `E0753`, and both rules it repeats — a module declared
// exactly once, and a note kept in the header — stand in the module doc at the HEAD of
// this file, checked by `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` for this file and for every
// (Copies of this file's own header, and notes recording that each suite named in them is\n// declared ONCE elsewhere, which landings appended into this tail: collapsed to this one\n// note, since the rule they repeat stands in the module doc at the HEAD of this file.)
// (Copies of this file's own header that landings appended into this tail, collapsed to
// this one note: the rule they repeat stands in the module doc at the HEAD of this file,
// the only place an inner doc comment may stand.)
// (Copies of this file's own header, and notes recording that each suite named in them is
// declared ONCE elsewhere, which landings appended into this tail: collapsed to this one
// note, the rule they repeat standing in the module doc at the HEAD of this file.)
// (Header fragments and duplicate-declaration notes that landings appended into this tail,
// collapsed to this one note: an inner doc comment is legal only before this crate root's
// first item, where the rule they all repeat stands.)
// (More of the same appended notes, collapsed here for the same reason.)
// declared ONCE elsewhere in this file, which landings appended into this tail: collapsed
// to this one note, since the rule they repeat stands in the module doc at the HEAD of
// this file, the only place an inner doc comment may stand.)
// (Header fragments and duplicate-declaration notes landings appended here, collapsed to
// this one note for the reason the header at the top of this file states.)
// (The last of those appended header fragments and duplicate-declaration notes, collapsed
// to this one note for the same reason: the rule they repeat stands in the module doc at
// the HEAD of this file, the only place an inner doc comment may stand.)
// (The last run of appended header fragments and duplicate-declaration notes, collapsed to\n// this one note: every suite named in them is declared exactly once elsewhere in this file,\n// and the rule they repeat stands in the module doc at the HEAD of this file, the only\n// place an inner doc comment may stand.)
// appended to the tail of this file would compile no suite of this target, and must be an
// ordinary `//` comment. (Five duplicate copies of this same paragraph, appended to this
// header by concurrent landings, are folded into it.)
// (The 4xx-share suite that grades the same chain is declared ONCE, further up this file:
// a module declared twice is `E0428` and compiles neither copy, so the copy that stood here
// is gone and the first declaration stands.)
// (And so is `cloudformation_kms_encrypted_sns_sqs_pipeline`, further up this file: the copy
// that stood here was its second declaration, which is `E0428`.)
// declaration of the scaling-plan suite, and a module declared twice is `E0428` and compiles
// it, so the fragment is gone rather than re-typed. The alarm-action topic-policy suite it
// stood beside is declared ONCE, further up this file, so the copy that was here is gone too.)
// fragment, as an ordinary comment for the reason it states.)
// leaving a note here. (Six more lines of that header fragment, as ordinary comments.)
// this file. (The header fragments and duplicate-declaration notes that landings appended
// around this one are gone rather than re-typed: they repeated that same rule, and the
// file has a 5000-line limit to stay under.)
// and compiles nothing. (Twelve lines of this file's own header, appended into this tail
// by landings as `//` and standing as ordinary comments for the reason they state.)
// (The tail of this file is where concurrently landing branches append a declaration, and
// with it a fragment of this file's own header or a note about a duplicate they removed.
// Those fragments accumulate: an inner doc comment past the first item is `E0753` and
// compiles no suite of this target, and the file grew past the 5000-line limit on a file a
// fix may change. The rules they repeated stand in the module doc at the HEAD of this file,
// where `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of them, so
// the fragments are gone rather than re-typed. Append a DECLARATION here, not a note.)
// The notes that stood here repeated, over and over, the two rules the module doc at the
// HEAD of this file states — each suite is declared exactly ONCE, and an inner doc comment
// belongs only to that header — one copy per landing that merged two appended tails. They
// are gone rather than re-typed: the rules stand in that header, and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` are what check them, for this
// file and for every service crate's `tests/all.rs`.
// (The notes removed from this tail are summarised above.)
// Below this line the tail held nothing but those same notes, repeated once per landing.
// They are gone: the rules stand in the module doc at the HEAD of this file, and
// (One note, in place of the repetitions this tail collected.)
// note written as an inner doc comment anywhere but this header is `E0753` — either fails
// every suite in this target. `all/all_rs_declares_each_suite_once.rs` checks the
// below, where an inner doc comment is `E0753`, compiles no suite of this target and
// must be written as an ordinary `//` comment.
// file: two landings in the same window each appended a declaration of it, and a module
// declared twice is `E0428` and compiles neither copy, so the earlier one stands.)
// is `E0428` and compiles neither copy, so the earlier one stands.)
// (Fragments of this file's own header and duplicate declarations that landings appended\n// into this tail, removed rather than re-typed: the rules they repeat stand in the module\n// doc at the HEAD of this file, where an inner doc comment is legal.)
// (More appended header fragments and duplicate declarations, removed for the same reason.)
// (Fragments of this file's own header that landings appended into this tail, removed rather
// than re-typed: the rule they repeat stands in the module doc at the HEAD of this file.)
// (More of the same appended header fragments, removed for the reason above.)
// into this tail, removed rather than re-typed: the rules they repeat stand in the module
// doc at the HEAD of this file, where an inner doc comment is legal.)
// (A run of duplicated header fragments and notes about declarations concurrent landings
// each appended stood here, and is gone rather than re-typed: the rules they repeated stand
// (Another such run stood here, and is gone for the same reason.)
// (A run of duplicated header fragments stood here, and is gone rather than re-typed: the
// rules they repeated stand in the module doc at the HEAD of this file.)
// (One more line of that same header fragment, an ordinary comment here for the reason the
// lines around it record: a `//` line past the first item of this crate root is `E0753`.)
// (A fragment of this file's header that a landing appended here as an INNER doc comment:
// declare each suite exactly ONCE, since this is one crate root and a second `mod` for the
// same name is `E0428`, and an inner doc comment is legal only BEFORE the first item, so
// one appended into this tail is `E0753` and compiles nothing. It stands as an ordinary
// comment, and `all/all_rs_declares_each_suite_once.rs` and
// (One last fragment of that header, as an ordinary comment for the same reason.) Declare
// each suite ONCE: this is one crate root, so a second `mod` for the same name is `E0428`
// and a note written with `//` anywhere but this file's header is `E0753` — either fails
// same note in this tail compiles nothing.) A suite left at the top level of `tests/`
// fails the build.
// (Six more lines of that same header fragment, ordinary comments for the reason they
// state: declare each suite ONCE, since this is one crate root and a second `mod` for the
// same name is `E0428`, and a note written with `//` anywhere but this file's header is
// (One more line of that header fragment, an ordinary comment for the same reason: check
// those suites rather than leaving a note in this appended tail, where an inner doc
// comment is `E0753`.)
// (A run of duplicated header fragments and notes about declarations two landings each
// appended stood here, and is gone rather than re-typed, for the reason the note at the end
// of this file records.)
// (The six declarations below were lost when two landings merged into this tail at once;
// each suite is declared ONCE here, since a module declared twice is `E0428` and the layout
// check refuses a suite under `tests/all/` that no `mod` of this crate root declares.)
// (A further run of the same duplicated header fragments and declaration notes stood here,
// and is gone rather than re-typed, for the reason the note at the end of this file
// (Another run of duplicated header fragments and declaration notes left by concurrent
// landings stood here, and is gone rather than re-typed, for the reason the note at the end
// (A long run of duplicated header fragments and declaration notes stood here, each left by
// a concurrent landing; they are gone rather than re-typed, for the reason the note at the
// end of this file records.)
// (More duplicated header fragments and notes about declarations two landings each
// appended stood here; they are gone rather than re-typed, for the reason the note at the
// (The duplicated header fragments concurrent landings had appended below this line are
// gone rather than re-typed: the rules they repeated — declare each suite exactly ONCE,
// since a second `mod` for one name is `E0428`, and keep every `//` note in the header,
// since an inner doc comment past the first item is `E0753` — stand in the module doc at
// second declaration two landings appended here is gone, a module declared twice being
// `E0428` and compiling neither copy.)
// (One more line of that header fragment, appended into this tail as `//` by a landing:
// an inner doc comment is legal only before the first item, so it stands as an ordinary
// comment.)
// declaration appended here is a second one, and a module declared twice is `E0428`.)
// (Six more lines of this file's own header, appended into this tail as `//` by a
// landing: Declare each suite exactly ONCE, since this is one crate root and a second
// first item, so one appended here is `E0753` and compiles nothing — which is why these
// lines are ordinary comments. `all/all_rs_declares_each_suite_once.rs` and
// (Two more lines of that header fragment, as ordinary comments for the same reason: the
// checks cover this file and every service crate's `tests/all.rs`, so check there rather
// than leaving a note here.)
// (Four more lines of that header, as ordinary comments for the same reason: declare each
// `E0428`, and a note written with `//` anywhere but the head of this file is `E0753` —
// (One more line of that header fragment, an ordinary comment for the same reason: a
// (Six more lines of that header landed here typed with `//`, and stand as ordinary
// comments for the reason they state: declare each suite ONCE, since this is one crate
// root and a second `mod` for the same name is `E0428`, and a note written with `//`
// anywhere but the head of this file is `E0753` — either fails every suite in this target.
// (Seven more lines of that header landed here typed with `//`, and stand as ordinary
// comments for the same reason: declare each suite ONCE, since a second `mod` for the same
// name is `E0428` and fails every suite in this target;
// first item is `E0753` and would compile no suite of this target.)
// there rather than leaving a note in the appended tail below, where an inner doc comment
// is `E0753`.)
// What a stack does when its Lambda-backed custom resource's provider answers FAILED is only
// sorted: one `#[path]` / `mod` pair per suite and nothing else, so a landing has one
// place to insert and nothing to keep consistent.
// A suite that is not declared here compiles nowhere and its tests never run. A suite
// left at the top level of `tests/` fails the build. Declare each suite exactly ONCE:
// this is one crate root, so a second `mod` for the same name is `E0428` and fails every
// suite in this target. An inner doc comment (`//`) is legal only before the first item,
// so this header is the only place one may stand; past it, `//` is `E0753` and compiles
// nothing. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of that —
// one declaration per suite file, no note past the header — for this file and for every
// service crate's `tests/all.rs`, so leave no note here.
// Declared here, in this same head region, for the reason the entry above records. These six
// suites had lost their declaration entirely: a run of concurrent landings each appended its
// own copy of them into the tail of this file, and each later landing that removed a
// duplicate left none behind — so the build script's layout check refused the target
// ("the suite ... is not declared in tests/all.rs, so it is compiled nowhere and its tests
// NEVER RUN"). Each is declared ONCE here, in the one region nothing else appends to.
// (Every suite the comment fragments that stood here named is declared exactly ONCE, in the
// spans above: a module declared twice loads the same file twice and compiles neither.)
// (The HTTPS front-door and metric-stream-estate suites are declared ONCE each, further up
// (The CodeBuild, AOSS-write and domain-endpoint suites are declared ONCE each, further up
// (The suites the fragments that stood here named are declared ONCE each, in the spans
// (`zz_probe` and the metric-stream estate suite are declared ONCE each, further up this
// tail: a module declared twice compiles neither copy.)
// above: a module may be declared only once, and a second `mod` item for one name compiles
// neither copy.)
// once. That file is EMPTY and is declared only because the build script requires every file
// under `tests/all/` to be declared.)
// (Every suite the fragments that stood here named is declared exactly ONCE, in the spans
// above: two `mod` items for one name compile neither copy.)
// above, for the reason the notes above record.)
// above: a module declared twice loads the same file twice and compiles neither copy.)
// above, for the reason those notes record.)
// above: loading one file as a module twice compiles nowhere.)
// above: a module may be declared only once, and several landings each appended a note here.)
// above: a redefinition compiles neither copy.)
// above, for the reason those notes record: a module may be declared only once.)
// above: several landings each appended their own copy of these notes, and a module declared
// twice loads the same file twice, which does not compile. The rule itself is in the module
// above, and the `//` lines among them are ordinary comments for the reason the header
// records: past the first item an inner doc comment is `E0753` and compiles nothing.)
// above, and the header note they repeated lives at the HEAD of this file, where an inner
// doc comment is legal.)
// above, and the header fragments among them stand as ordinary comments: past the first item
// an inner doc comment is `E0753`.)
// (Every header fragment that stood here is an ordinary comment for the reason the notes
// above record, and every suite they named is declared exactly ONCE in the spans above.)
// above, and the header fragments among them stand as ordinary comments: an inner doc
// comment is legal only at the head of a crate root.)
// above, and every header fragment among them is an ordinary comment for the same reason.)
// above, and the header fragments among them are ordinary comments: an inner doc comment is
// legal only before the first item of this crate root, so one in this tail is `E0753`.)
// (The header fragments successive landings appended here are ordinary comments, and gone
// rather than re-typed: the rule they repeated is in the module doc at the HEAD of this
// (More fragments of this file's own header reached this tail the same way, and are gone for
// the same reason: the rule they repeated stands in the module doc at the HEAD of this file,
// and `all/all_rs_declares_each_suite_once.rs` with
// above, and every fragment of this file's own header that successive landings appended into
// this tail is gone rather than re-typed: the rule stands in the module doc at the HEAD of
// this file, which is the only place an inner doc comment is legal, and
// (The notes and header fragments that stood here are gone for the reason the span above
// records; every suite they named is declared exactly ONCE, in the spans above.)
// (The notes and header fragments that stood here are gone for the reason the spans above
// record: every suite they named is declared exactly ONCE above, and an inner doc comment
// is legal only at the HEAD of this crate root.)
// (The notes and header fragments that stood here are gone for the same reason: every suite
// they named is declared exactly ONCE above, and the rule they repeated stands at the HEAD
// (The header fragments and duplicate-declaration notes that stood here are gone for the
// reason the spans above record.)
// (The header fragments that stood here are gone for the reason the spans above record: a
// `//` line past the first item of this crate root is `E0753` and compiles no suite of this
// target, and the rule they repeated stands in the module doc at the HEAD of this file.)
// (Every suite that was declared here as well is declared exactly ONCE, in the spans above
// and in the tail below: branches landing in the same window each carried their own copy of
// this span, and a module may be declared only once.)
// `E0753` and compiles no suite of this target, so this fragment of the header stands as
// an ordinary comment: a suite left at the top level of `tests/` fails the build.
// (Six more lines of that header landed here typed with `//`, which is `E0753` past the
// first item and compiles no suite of this target, so they stand as ordinary comments: a
// module declared twice is `E0428`, `all/all_rs_declares_each_suite_once.rs` checks the
// declarations of this file and of every service crate's `tests/all.rs`, and a note
// belongs in the header at the head of this file rather than here.)
// (Seven more lines of that header landed here as `//` too, and stand as ordinary
// comments for the same reason: an inner doc comment past the first item is `E0753` and
// compiles nothing. What they repeated — a module declared twice is `E0428`,
// this file and for every service crate's `tests/all.rs`, and a note belongs in the
// header — is in the module doc at the HEAD of this file.)
// (One more line of it landed here the same way, and stands as an ordinary comment for the
// same reason: a note belongs in the header above, not in this appended tail, where an
// inner doc comment is `E0753`.)
// (Five more lines of that same fragment, ordinary comments for the reason they state:
// this file's tail, where an inner doc comment is `E0753` and compiles nothing.)
// (A fragment of the header above, appended into this tail by a landing: declare each
// `E0428`, and a
// note written with `//` anywhere but the header above is `E0753` — either fails every
// (And for every service crate's `tests/all.rs` too, so check there rather than
// (One more such line, an ordinary comment for the same reason: a note belongs in the
// header, not in this appended tail, where an inner doc comment is `E0753`.)
// a note in this file's tail, where an inner doc comment is `E0753` and compiles
// item that is `E0753`, so they stand as ordinary comments — declare each suite ONCE, since
// this is one crate root and a second `mod` for the same name is `E0428`, and
// (Four more lines of that header fragment, ordinary comments for the reason they state: a
// module declared twice is `E0428`, a note written with `//` past the first item is
// `E0753`, and `all/all_rs_declares_each_suite_once.rs` checks the declarations of this file
// and of every service crate's `tests/all.rs`.)
// (Two more lines of it, ordinary comments for the same reason:
// (Three more lines of it, ordinary comments for the same reason: the checks cover this file
// and every service crate's `tests/all.rs`, so check there rather than leaving a note at the
// end of this file, where an inner doc comment is `E0753` and compiles nothing.)
// (Seven lines of this file's own header, appended into the tail by a landing typed with
// `//`: past the first item that is `E0753` and compiles no suite of this target, so they
// stand as ordinary comments. What they repeat is in the module doc at the HEAD of this
// file: a suite left at the top level of `tests/` fails the build, a module declared twice
// is `E0428`, and `all/all_rs_declares_each_suite_once.rs` checks the declarations of this
// file and of every service crate's `tests/all.rs`.)
// (Four more lines of that header, ordinary comments for the same reason:
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it, so
// check there rather than leaving a note at the end of this file.)
// (Three more lines of it, ordinary comments for the same reason: the two checks cover the
// declarations and the notes, for this file and for every service crate's `tests/all.rs`, so
// check there rather than leaving a note in the tail below.)
// (Seven more lines of it, ordinary comments for the same reason: a module declared twice is
// `E0428`, an inner doc comment past the first item is `E0753`, and the two check suites
// cover both halves for this file and for every service crate's `tests/all.rs`.)
// (Thirteen lines of this file's own header reached this tail typed with `//`, which past
// the first item is `E0753` and compiles no suite of this target. The rule they repeated is
// (Fourteen more lines of that header reached this tail the same way, and are gone for the
// same reason: past the first item a `//` line is `E0753` and compiles nothing, and the rule
// they repeated stands at the HEAD of this file.)
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check the declarations and the
// notes, for this file and for every service crate's `tests/all.rs`.)
// Declared here for the reason this entry records: what a
// (Nothing but notes followed here: fragments of this file's own module header that
// concurrent landings appended into the tail, several of them typed `//`, which past the
// first item is `E0753` and compiles no suite of this target. They are gone rather than
// re-typed — the rule they repeated stands in the module doc at the HEAD of this file, and
// `all/all_rs_declares_each_suite_once.rs` plus
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it, for
// (And more of the same followed, to the end of this file: header fragments concurrent
// landings appended here, some as `//`, which past the first item is `E0753`. The rule
// they repeated is in the module doc at the HEAD of this file, so they are gone rather
// sorted. A suite that is not declared here compiles nowhere and its tests never run,
// and a suite left at the top level of `tests/` fails the build. Declare each suite
// exactly ONCE: this is one crate root, so a second `mod` for the same name is `E0428`
// and fails every suite in this target. An inner doc comment (`//`) is legal only
// before the first item, so these lines are the only place one may stand — a note
// appended further down this file is `E0753` and compiles no suite of this target, and
// must be an ordinary `//` comment. `all/all_rs_declares_each_suite_once.rs` and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves, for this
// file and for every service crate's `tests/all.rs`, so check there rather than leaving
// a note here.
// (`cloudtrail_read_only_api_calls` and `zz_probe` are each declared ONCE further up this
// file: a merge of two landings deposited a second declaration of each here, which is
// `E0428` and compiles neither copy.)
// (Both suites are declared ONCE, further up this file — the SESv2 event-destination one
// in the sorted span above, the CloudFront flat-rate subscription one in the head region:
// a module declared twice is `E0428` and compiles neither copy, and a merge of two landings
// deposited a second declaration of each here.)
// (Declared ONCE, in the sorted span above: a merge of two landings deposited a second
// declaration here, which is `E0428` and compiles neither copy.)
// CloudFront span far above: a merge of two landings deposited a second declaration here,
// which is `E0428` and compiles neither copy, so this copy is gone.)
// CloudFront span above: a merge of two landings deposited a second declaration here.)
// CloudFront span above: a merge of two landings deposited a second declaration here,
// which is `E0428` and compiles neither copy.)
// above: a merge of two landings deposited a second declaration here, which is `E0428` and
// above: a merge of two landings deposited a second declaration here, which is `E0428`.)
// it scales. (Declared ONCE, in the head region of this file: a merge of two landings
// deposited a second declaration here, which is `E0428` and compiles neither copy.)
// (One more fragment of this file's own header landed here typed with `//`, which is
// `E0753` past the first item and compiles no suite of this target; the rule it repeated —
// a suite left at the top level of `tests/` fails the build — stands in the module doc at
// the head of this file, so the fragment is gone rather than re-typed.)
// (And one more line of that same header fragment, appended here typed with `//`: an
// inner doc comment is legal only before the first item, so it is `E0753` here and
// compiles nothing. `all/all_rs_inner_doc_comments_precede_the_first_item.rs` pins that
// rule for this file and for every service crate's `tests/all.rs`.)
// (And once more, as a line comment: concurrent landings keep appending this fragment of
// the file's own header into the tail typed with `//`, which is `E0753` past the first
// item and compiles no suite of this target. The rule it repeats stands in the header at
// the head of this file.)
// one stays in the HEADER: an inner doc comment past the first item is `E0753`, so this
// fragment, appended into the tail, stands as ordinary comments.
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, in the sorted span above: a
// merge of two landings deposited a second declaration here, which is `E0428`.)
// suite left at the top level of `tests/` fails the build. (One more copy of that
// fragment, appended into the tail typed with `//`, which is `E0753` there: it stands as
// an ordinary comment, and the rule is in the header at the head of this file.)
// (`cloudwatch_cfn_alarm_action_topic_policy` is declared ONCE, a few lines above in this
// same tail: a merge of two landings deposited a second declaration here, which is `E0428`
// and compiles neither copy.)
// (Another landing appended two more fragments of this file's own header here, typed with
// `//`: an inner doc comment is legal only before the first item, so in this tail each
// line is `E0753` and compiles nothing. Both fragments are gone rather than re-typed — the
// header at the head of this file carries the rule they repeated.)
// (And two more such fragments, six lines in all, appended here as `//` by later
// landings: each is `E0753` past the first item and compiles nothing, so they are gone
// rather than re-typed, and the header at the head of this file carries the rule they
// repeated.)
// (Two copies of this file's own header, eighteen lines in all, that the union merge of two
// landings deposited in this tail typed with `//`: an inner doc comment is legal only
// before the first item, so each line was `E0753` and compiled no suite of this target.
// They are deleted rather than re-typed, and the header at the head of this file states
// the rule they repeated.)
// line that stood here as `//` is gone rather than re-typed.
// (Six more lines of that header landed in this tail typed with `//`: past the first item
// that is `E0753` and compiles no suite of this target, so they are deleted rather than
// re-typed — the module doc at the HEAD of this file states the rule they repeated.)
// (And seven more, deleted for that same reason.)
// (Six more lines of that same header fragment landed in this tail typed with `//`: each
// is `E0753` past the first item, so they are dropped rather than re-typed — the module doc
// at the HEAD of this file states the rule they repeated.)
// (And seven more of those lines, landed here the same way and dropped for the same
// and five more of its lines, landed here as `//` and dropped for that same reason.)
// declared twice is `E0428`, so the duplicate is removed and the first declaration stands.)
// (A long run of notes stood here, each left by an earlier repair of this same file: copies
// of the module doc at the HEAD of this file that landings appended into the tail typed with
// `//`, which is `E0753` past the first item, and second declarations of suites already
// declared above, which are `E0428`. The copies and the duplicates are gone, this one note
// stands for all of them, and the rules themselves stand once in the header.)
// (More of the same run of notes stood here, and is gone for the reason the note above
// records: the duplicate declarations and the appended header copies are removed.)
// (And the same again: the appended header copies and the duplicate declarations that stood
// here are gone, for the reason the note further up this tail records.)
// (Last of that run: the appended header copies and the duplicate declarations that stood
// here are gone, for the reason the notes above record.)
// (And one more such run of notes stood here: the header copies landings appended as `//`
// and the second declarations of suites already declared above are gone, for the reason the
// notes further up this tail record.)
// (And one last such run, gone for the same reason.)
// (And the run after it, likewise: the header copies landings appended as `//` and the
// second declarations of suites already declared above are dropped rather than re-typed.)
// leaving a note in the appended tail below, where this same note would compile nothing.
// module declared twice is `E0428` and compiles no suite of this target, so the copies that
// stood here are gone.)
// (`ses_event_destination_reaches_an_eventbridge_rule_target` is declared ONCE, in the span
// above: two branches landing at once each appended a declaration of it here, and a module
// declared twice is `E0428` and compiles no suite of this target, so the copy that stood
// here is gone.)
// the top of this file: two branches landing at once each appended a second declaration of
// it here, and a module declared twice is `E0428` and compiles no suite of this target, so
// the copy that stood here is gone.)
// (And one more line of that same duplicated header landed here typed with `//`: an inner
// doc comment is legal only before the first item, so it stands as an ordinary comment.)
// (Another fragment of that same header, appended here typed with `//`: an inner doc
// comment is legal only BEFORE the first item, so it stands as ordinary comments. Declare
// each suite exactly ONCE — this is one crate root, so a second `mod` for the same name is
// `E0428` and fails every suite in this target — and keep a note like this one out of the
// tail; `all/all_rs_declares_each_suite_once.rs` and
// that stood here was a second declaration a concurrent landing appended, which is `E0428`
// (One more fragment of that header, an ordinary comment for the same reason: it covers
// this file and every service crate's `tests/all.rs`, so check there rather than leaving a
// (A last fragment of that header, ordinary comments for the same reason: declare each
// `E0428`, and a note written with `//` anywhere but this file's header is `E0753`, either
// of which fails every suite in this target. `all/all_rs_declares_each_suite_once.rs`
// line below it, another fragment of that header, stands as an ordinary comment too: a
// (Six more lines of that header fragment landed here typed with `//`, and stand as
// ordinary comments for the reason they state: declare each suite exactly ONCE — this is
// one crate root, so a second `mod` for the same name is `E0428`, and a note written with
// `//` anywhere but this file's header is `E0753`, either of which fails every suite in
// this target. `all/all_rs_declares_each_suite_once.rs` checks the declarations, for this
// file and for every service crate's `tests/all.rs`, so check there rather than leaving a
// (And seven more lines of it landed here the same way, ordinary comments for the same
// reason: declare each suite exactly ONCE — this is one crate root, so a second `mod` for
// the same name is `E0428` and fails every suite in this target — and keep a note like
// this one out of the appended tail, where an inner doc comment is `E0753`.
// checks that the note stays in the HEADER, since the same note appended to the tail of
// this file would compile no suite of this target.)
// (Six more lines of that header fragment, ordinary comments for the same reason: declare
// each suite exactly ONCE, since this is one crate root and a second `mod` for the same name
// is `E0428` and fails every suite in this target. `all/all_rs_declares_each_suite_once.rs`
// checks that for this file and for every service crate's `tests/all.rs`, and
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` checks that a note like this one
// stays in the HEADER: an inner doc comment past the first item is `E0753`, so the same note
// appended to the tail of this file would compile no suite of this target.)
// no suite of this target, so the fragments stand as ordinary comments rather than being
// re-typed — the rules they repeated (a suite left at the top level of `tests/` fails the
// build; each suite is declared exactly ONCE, since this is one crate root and a second
// `mod` for the same name is `E0428`, and a note written with `//` anywhere but this file's
// header is `E0753` — either fails every suite in this target;
// and for every service crate's `tests/all.rs`) stand in the module doc at the HEAD of this
// file, which is where those two suites check them from.)
// (Residue of the same two kinds, condensed: the suites these notes named are each declared
// exactly ONCE further up this file, and the header fragments landings appended here — some
// typed with `//`, which is `E0753` past the first item — stand represented by this one
// (More of the same residue, condensed into this one comment: fragments of this file's own
// header that landings appended into the tail — several of them typed with `//`, which is
// `E0753` past the first item and compiles no suite of this target — together with their
// notes that `cloudwatch_cfn_alarm_action_topic_policy`,
// `pricing_plan_subscription_resource_eligibility` and the other suites they named are each
// declared exactly ONCE further up this file, a module declared twice being `E0428`.)
// (`zz_probe`, `cloudformation_scripted_queue_deploy`,
// `ses_event_destination_reaches_an_eventbridge_rule_target` and
// `pricing_plan_subscription_resource_eligibility` are each declared exactly ONCE already,
// further up this file: landings in the same window appended a second declaration of each
// here, and a module declared twice loads the same file twice, which compiles neither — so
// those duplicates are removed and the first declarations stand.)
// (More of the same residue, condensed: the Pricing Plan Manager suite,
// `cloudformation_eventbridge_fan_out_to_lambda_and_sfn` and
// `autoscaling_plans_policies_reach_application_auto_scaling` are each declared exactly ONCE
// further up this file, a module declared twice being `E0428`, and the header fragments
// landings appended with these notes stand in the module doc at the HEAD of this file.)
// (And the rest of it, condensed for the same reasons: the Pricing Plan Manager suite and
// further up this file, and the header fragments that landed here with these notes — several
// typed with `//`, which is `E0753` past the first item — stand in the module doc at the
// (More of the same residue, condensed: fragments of this file's own module doc that
// landings appended into this tail, some typed with `//`, which past the first item is
// `E0753` and compiles no suite of this target. The rules they repeated — a suite left at
// the top level of `tests/` fails the build, each suite is declared exactly ONCE, and both
// halves are checked for this file and for every service crate's `tests/all.rs` — stand in
// (The residue below the `eks_nodegroup_lifecycle_convergence` entry, condensed the same
// way: `cloudfront_edge_4xx_share_decides_the_alarm`,
// `pricing_plan_subscription_resource_eligibility`, `apigateway_sns_service_integration`,
// `autoscaling_plans_policies_reach_application_auto_scaling` and the rest named in it are
// each declared exactly ONCE further up this file, and the copies concurrent landings
// appended here are gone, a module declared twice being `E0428`; the header fragments that
// arrived with them stand in the module doc at the HEAD of this file.)
// (The last of that residue, condensed: the header fragments landings appended here — many
// of them typed with `//`, which is `E0753` past the first item and compiles no suite of
// this target — are represented by this comment, the rules they repeat standing in the
// module doc at the HEAD of this file, and every suite they named is declared exactly ONCE
// further up this file.)
// (The rest of the residue in this tail, condensed into this one comment for the reasons the
// notes above give: every suite it named is declared exactly ONCE further up this file, and
// the fragments of this file's own module doc that landings appended with it — many typed
// with `//`, which is `E0753` past the first item — are represented here.)
// (Ninety-odd lines of the same two notes, condensed here: the suites they named —
// `sfn_relayed_dynamodb_request_id`, `cloudwatch_cfn_alarm_action_topic_policy`,
// `cloudformation_provisions_cloudfront_resources` among them — are each declared exactly
// ONCE further up this file, and the fragments of this file's own module doc that landed
// with them, typed with `//` and so `E0753` past the first item, are represented by this
// (Three more lines of that fragment, ordinary comments for the same reason.)
// (A long run of the same two notes, condensed into this one comment: every suite they
// named — the SESv2 event-destination suite, `cloudformation_kms_encrypted_sns_sqs_pipeline`,
// the flat-rate CloudFront subscription suite, the scaling-plan policy suite and the rest —
// is declared exactly ONCE further up this file, a module declared twice being `E0428` and
// compiling neither copy; and the fragments of this file's own module doc that landings
// appended with them, many typed with `//`, which past the first item is `E0753` and
// compiles no suite of this target, stand represented here, the rules they repeat being in
// the module doc at the HEAD of this file, where the two guard suites check both halves.)
// below, where an inner doc comment compiles no suite of this target at all.
// landings in the same window each declared it here as well, and a module declared twice is
// (Declared ONCE, further up this file: the declaration that stood here while two branches
// landed at once compiled neither copy, so it is gone rather than re-typed.)
// (Declared ONCE, further up this file: the copy that stood here was appended while two
// branches landed at once, and a module declared twice compiles neither copy.)
// (Declared ONCE, further up this file: the copy that stood here compiled neither, so it is
// gone rather than re-typed.)
// module declared twice is `E0428` and compiles neither copy.)
// header, an ordinary comment here for the reason the lines above record.)
// (One more line of that same fragment, an ordinary comment for the same reason.)
// twice is `E0428` and compiles neither copy.)
// (A landing appended six more lines of this file's own header here, typed with `//`: an
// inner doc comment is legal only before the first item, so in this tail it is `E0753` and
// compiles nothing. The header at the top of this file states the rule they repeated, so
// the fragment is gone rather than re-typed.)
// (And several landings appended yet more lines of that same header here, each typed with
// `//`: in this tail every one of them is `E0753` and compiles nothing, so the fragment is
// gone rather than re-typed. What it repeated is at the HEAD of this file: declare each
// suite exactly ONCE, since this is one crate root and a second `mod` for one name is
// `E0428`, and keep a note like this one out of the tail.)
// `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of. A run of
// further duplicated header lines followed; they are gone rather than re-typed.)
// this target. The rule it states stands in the header at the top of this file. A run of
// further duplicated header lines followed; they are gone rather than re-typed, and they
// declared no suite.)
// compiles no suite in this target. A long run of further such fragments followed, each a
// duplicated line of this file's own header; the rules they repeated stand in the module
// doc at the HEAD of this file, so they are gone rather than re-typed, and they declared
// no suite.)
// (A further run of duplicated header lines followed here, each an ordinary comment
// stating the rule the module doc at the HEAD of this file already states; they are gone
// rather than re-typed, and they declared no suite.)
// file: two landings in the same window each declared it here as well, and a module declared
// (A long run of comments followed, down to the end of this file: fragments of this file's
// own header that concurrent landings appended into the tail, many of them typed with
// `//` — which past the first item of a crate root is `E0753` and compiles no suite of
// this target — together with notes about suite declarations those landings had
// duplicated. Every rule they repeated stands in the module doc at the HEAD of this file,
// where an inner doc comment is legal and where `all/all_rs_declares_each_suite_once.rs`
// and `all/all_rs_inner_doc_comments_precede_the_first_item.rs` check both halves of it, so
// the run is gone rather than re-typed. It declared no suite: every `mod` of this target
// stands above.)
// (Two more lines of that same header fragment, ordinary comments for the same reason: the
// suite-declaration check covers this file and every service crate's `tests/all.rs`, so
// check there rather than leaving a note here.)
// (A long run of duplicate-declaration notes followed here, each naming a suite already
// declared above; they are gone rather than re-typed, and they declared no suite.)
// (A long run of further header fragments and duplicate-declaration notes followed here;
// every rule they repeated stands in the module doc at the HEAD of this file, so they are
// gone rather than re-typed, and they declared no suite.)
// the reason the lines above record. A long run of further such fragments and of notes
// about duplicated suite declarations followed; every rule they repeated stands in the
// module doc at the HEAD of this file, so they are gone rather than re-typed, and they
// (A long run of comments stood here, down to the end of this file: fragments of this
// file's own header that concurrent landings appended into the tail — several typed with
// `//`, which past the first item of a crate root is `E0753` and compiles no suite of this
// target — and notes about suite declarations those landings had duplicated. Every rule
// they repeated stands in the module doc at the HEAD of this file, where an inner doc
// comment is legal and where `all/all_rs_declares_each_suite_once.rs` and
// run is gone rather than re-typed. It declared no suite: every `mod` of this target stands
// ONCE: this is one crate root, so a second `mod` for the same name is `E0428`, and a
// suite in this target. `all/all_rs_declares_each_suite_once.rs` and
// Declared here, in this same head region, for the reason the entry above records: a rule
// carrying both a Firehose delivery-stream target and an SQS queue target fans one event out
// across EventBridge, IAM, Firehose, Amazon S3 and Amazon SQS at once, so no single service
// crate's test target can publish the event and then read both far ends.
// The notes above this point are the bookkeeping concurrent landings left in this crate
// root; they live in `all/appended_notes.rs` (see the item below).
#[allow(dead_code)]
fn landing_notes_1() {}
