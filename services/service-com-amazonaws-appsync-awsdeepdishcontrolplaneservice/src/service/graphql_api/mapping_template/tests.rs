// SPDX-License-Identifier: CC0-1.0

//! What this evaluator does with text that is NOT a well-formed template.
//!
//! A mapping template is caller-supplied, so every one of these is an input this endpoint
//! will be handed: text that does not parse, text cut off in the middle of a construct, text
//! that nests deeper than any stack, and text longer than a stored template can be. The rule
//! is the same for all of them: the evaluation answers a FAULT naming what it could not do,
//! and nothing else — it never panics, never loops without end, and never renders text from a
//! template it did not understand.
//!
//! The rows span the ways text can be malformed (a block that is never closed, a directive
//! written without its parentheses, an unterminated string, an unterminated comment, a
//! construct closed that was never opened, an expression with an operand missing), the
//! truncation of a well-formed template at EVERY character boundary, nesting past the parse
//! bound (blocks, grouped expressions, prefix and binary operator chains, a reference's own
//! walk), a value grown past the size, depth and step bounds, and a template past the length
//! bound.

use ::serde_json::json;

use super::{evaluate, Evaluation, Fault, MAX_NESTING, MAX_RUN, MAX_TEMPLATE_BYTES};

/// Evaluate one template over [`context`], with the whole run budget: what the data plane
/// hands an evaluation when the request has spent none of its template time yet.
fn evaluated(template: &str) -> Result<Evaluation, Fault> {
    evaluate(template, &context(), MAX_RUN)
}

/// The context every case is evaluated over: the shape a resolver's own `$context` has, so a
/// template that does parse has something to read.
fn context() -> ::serde_json::Value {
    json!({
        "arguments": {"text": "spoken"},
        "args": {"text": "spoken"},
        "source": ::serde_json::Value::Null,
        "identity": {"accountId": "000000000000"},
        "stash": {},
        "result": {"items": [1, 2, 3]},
        "info": {"fieldName": "field", "parentTypeName": "Query"},
    })
}

/// A template that does not parse is a fault, never a panic and never a rendering.
#[test]
fn text_that_is_not_a_template_is_a_fault() {
    let malformed: &[(&str, &str)] = &[
        ("a block that is never closed", "#if($ctx.args.text)open"),
        ("a loop that is never closed", "#foreach($n in [1, 2])$n"),
        ("an #end with no block", "rendered#end"),
        ("an #else with no #if", "rendered#else"),
        ("a directive with no parentheses", "#if $ctx.args.text"),
        ("a #set that is no assignment", "#set($ctx.args.text)"),
        ("a #set with no value", "#set($x = )"),
        ("a #foreach with no element", "#foreach([1, 2])$n#end"),
        ("an unterminated string", "#set($x = \"unclosed)"),
        ("an unterminated list", "#set($x = [1, 2)"),
        ("an unterminated map", "#set($x = {\"a\": 1)"),
        ("an expression with an operand missing", "#if($x ==)yes#end"),
        ("a call whose arguments never close", "#set($x = $util.toJson(1)"),
    ];
    for (case, template) in malformed {
        let answered = evaluated(template);
        assert!(
            answered.is_err(),
            "case {case:?}: expected a fault, got {:?}",
            answered.map(|evaluation| evaluation.text)
        );
    }
}

/// An unterminated COMMENT and an unterminated unparsed block end the template rather than
/// running past it: both are read to the end of the text, which is what Velocity does with
/// one, so the template renders what is left of it and nothing more.
#[test]
fn an_unterminated_comment_ends_the_template() {
    for (case, template, expected) in [
        ("an unterminated block comment", "kept#* never closed", "kept"),
        ("an unterminated unparsed block", "kept#[[ never closed", "kept never closed"),
        ("a line comment with no newline", "kept## never closed", "kept"),
    ] {
        let evaluation = evaluated(template)
            .unwrap_or_else(|fault| panic!("case {case:?}: expected a rendering: {fault:?}"));
        assert_eq!(
            evaluation.text, expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            evaluation.text
        );
    }
}

/// A well-formed template TRUNCATED at any character boundary is answered — a rendering or a
/// fault — and never panics.
///
/// Every prefix is tried, which is what makes this a property of the parser rather than of
/// one chosen cut: the bound is the template's own length, so the case count is fixed.
#[test]
fn every_truncation_of_a_template_is_answered() {
    let whole = "#set($seen = $ctx.args.text)\
                 #if($ctx.args.text == \"spoken\")\
                 {\"version\": \"2018-05-29\", \"payload\": {\"said\": \"$seen\", \
                 \"items\": [#foreach($n in $ctx.result.items)$n#if($foreach.hasNext),#end#end]}}\
                 #else\
                 $util.error(\"unspoken\", \"Refused\")\
                 #end";
    for cut in 0..=whole.chars().count() {
        let truncated: String = whole.chars().take(cut).collect();
        // The answer is what matters only in that there IS one: a prefix of a template is
        // not required to be a template.
        let _answered = evaluated(&truncated);
    }
}

/// Text that nests deeper than the parse bound is a fault, so the parser's own recursion
/// cannot be driven past the host's stack by a caller-supplied template.
#[test]
fn nesting_past_the_bound_is_a_fault() {
    let deep = MAX_NESTING * 4;
    let cases: Vec<(&str, String)> = vec![
        (
            "nested blocks",
            format!(
                "{}rendered{}",
                "#if(true)".repeat(deep),
                "#end".repeat(deep)
            ),
        ),
        (
            "nested expressions",
            format!("#if({}true{})yes#end", "(".repeat(deep), ")".repeat(deep)),
        ),
        (
            "a chain of prefix operators",
            format!("#if({}true)yes#end", "!".repeat(deep)),
        ),
        (
            "a chain of binary operators",
            format!("#set($x = 1{})", " + 1".repeat(deep)),
        ),
        (
            "a reference that walks further than a reference may",
            format!("$ctx{}", ".member".repeat(deep)),
        ),
    ];
    for (case, template) in &cases[..4] {
        let answered = evaluated(template);
        assert!(
            answered.is_err(),
            "case {case:?}: expected a fault, got {:?}",
            answered.map(|evaluation| evaluation.text)
        );
    }
    // A reference past the walk bound is no reference at all, so it renders as its own text
    // rather than being walked — which is what Velocity renders text that names nothing as.
    let (case, template) = &cases[4];
    let evaluation = evaluated(template)
        .unwrap_or_else(|fault| panic!("case {case:?}: expected a rendering: {fault:?}"));
    assert_eq!(
        evaluation.text, *template,
        "case {case:?}: expected the reference's own text, got {:?}",
        evaluation.text
    );
}

/// A template that GROWS a value is refused by the size bound rather than allowed to
/// allocate: each row doubles what it holds for a handful of steps, which is the shape that
/// outruns a budget counted in operations alone.
#[test]
fn a_value_that_doubles_is_refused() {
    let rows: &[(&str, &str)] = &[
        (
            "a list built from itself, doubling each pass",
            "#set($a = [1])#foreach($n in $ctx.result.items)#set($a = [$a, $a])\
             #set($a = [$a, $a])#set($a = [$a, $a])#set($a = [$a, $a])#end$a.size()",
        ),
        (
            "a string added to itself, doubling each pass",
            "#set($s = \"xxxxxxxx\")#foreach($n in $ctx.result.items)#set($s = $s + $s)\
             #set($s = $s + $s)#set($s = $s + $s)#set($s = $s + $s)#set($s = $s + $s)\
             #set($s = $s + $s)#set($s = $s + $s)#end$s.length()",
        ),
        (
            "a list added to itself in place, doubling each pass",
            "#set($a = [1])#foreach($n in $ctx.result.items)$util.qr($a.addAll($a))\
             $util.qr($a.addAll($a))$util.qr($a.addAll($a))$util.qr($a.addAll($a))\
             $util.qr($a.addAll($a))$util.qr($a.addAll($a))#end\
             $a.size()",
        ),
        (
            "a value nested one level deeper each pass",
            "#set($a = [1])#foreach($n in $ctx.result.items)#set($a = [$a])#set($a = [$a])\
             #set($a = [$a])#set($a = [$a])#set($a = [$a])#set($a = [$a])#set($a = [$a])\
             #set($a = [$a])#set($a = [$a])#set($a = [$a])#set($a = [$a])#set($a = [$a])\
             #end$a.size()",
        ),
    ];
    for (case, template) in rows {
        let answered = evaluated(template);
        assert!(
            answered.is_err(),
            "case {case:?}: expected the growth to be refused, got {:?}",
            answered.map(|evaluation| evaluation.text)
        );
    }
}

/// A template longer than a stored one can be is refused before it is parsed at all.
#[test]
fn a_template_past_the_length_bound_is_refused() {
    let oversized = "$ctx.args.text".repeat(MAX_TEMPLATE_BYTES);
    assert!(
        oversized.len() > MAX_TEMPLATE_BYTES,
        "the case must be past the bound, got {} bytes",
        oversized.len()
    );
    let answered = evaluated(&oversized);
    assert!(
        answered.is_err(),
        "case an oversized template: expected a fault, got a rendering"
    );
}

/// A `#foreach` whose body does more work than the step budget allows is stopped by it, so a
/// loop a template writes cannot hold the endpoint.
///
/// Eleven loops over the same three elements, nested: the WORK is their product, which is
/// what the budget bounds — no single level of it is large.
#[test]
fn a_loop_past_the_step_budget_is_stopped() {
    let mut template = "$ctx.args.text".to_string();
    for _ in 0..11 {
        template = format!("#foreach($n in $ctx.result.items){template}#end");
    }
    let fault = evaluated(&template)
        .err()
        .unwrap_or_else(|| panic!("a loop past the step budget must be stopped"));
    assert!(
        fault.message().contains("steps"),
        "expected the step budget to stop the run, got {:?}",
        fault.message()
    );
}
