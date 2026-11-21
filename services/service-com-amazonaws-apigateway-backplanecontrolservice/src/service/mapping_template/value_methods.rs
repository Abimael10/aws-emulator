// SPDX-License-Identifier: CC0-1.0

//! Which methods a reference may CALL on the value it has reached, as opposed to the methods a
//! VARIABLE of the call documents.
//!
//! A mapping template's variables resolve to the values the call carries, and those values are
//! Java objects: `$input.path(…)` is documented as returning "a JSON object representation of
//! the result", `$input.params(…)` a parameter's string, `#set` binds one of them to a name.
//! The language reaches a method of whatever it is standing on, so a call written after such a
//! value — `$input.path('$.name').length()`, `$name.toUpperCase()`, `$items.size()` — is a
//! method of the TYPE that value has, and not of the variable the reference opened with. That
//! is why these are read here rather than in the variable surface: the surface resolves the
//! value, and this applies what the value's own type answers.
//!
//! WHAT each method comes to is the LANGUAGE's over its value model, so it is the tree's one
//! reading of it (`::service_common::velocity_template::method`, which the AppSync resolver
//! templates call through as well). What lives here is this plane's own POLICY over that
//! reading — [`VALUE_METHODS`], the methods a mapping template of this plane answers:
//!
//! * the accessors of `java.lang.String`, `java.util.List` and `java.util.Map` whose result is
//!   a function of the value alone — `length()`, `size()`, `isEmpty()`, `toUpperCase()`,
//!   `toLowerCase()`, `trim()`, `contains()`, `startsWith()`, `endsWith()`, `indexOf()`.
//!
//! Every other call stays UNANSWERED, and the caller leaves such a template unrendered rather
//! than rendering a guess, which is how this plane answers every reference it resolves no value
//! for. Three kinds are deliberately outside the policy: a method that MUTATES the value it is
//! called on (`put`, `add`, `remove`, `clear`), because this plane resolves a reference to a
//! COPY of the value, so a mutation would be made to something nothing reads afterwards; a
//! method whose result depends on a regular-expression dialect (`replaceAll`, `split`,
//! `matches`); and `toString`, because what an object or a list renders as is what this plane
//! leaves unresolved everywhere else.

use super::Step;

/// The methods of a value this plane's mapping templates answer; see the module docs. A
/// trailing call naming one of them is split off the reference and applied to the value the
/// rest of it resolved ([`split`]); every other call is left to the variable surface, which is
/// where the methods a VARIABLE documents (`$input.path`, `$util.urlEncode`, a map's `get`) are
/// answered.
const VALUE_METHODS: [&str; 10] = [
    "length",
    "size",
    "isEmpty",
    "toUpperCase",
    "toLowerCase",
    "trim",
    "contains",
    "startsWith",
    "endsWith",
    "indexOf",
];

/// Split `steps` into the steps that name a value of the call and the trailing method calls
/// this plane answers on that value, in the order they are written.
pub(super) fn split(steps: &[Step]) -> (&[Step], Vec<(&str, &[super::Argument])>) {
    let trailing = steps
        .iter()
        .rev()
        .take_while(|step| {
            matches!(step, Step::Call(method, _) if VALUE_METHODS.contains(&method.as_str()))
        })
        .count();
    let (head, tail) = steps.split_at(steps.len() - trailing);
    let calls = tail
        .iter()
        .filter_map(|step| match step {
            Step::Call(method, arguments) => Some((method.as_str(), arguments.as_slice())),
            Step::Member(_) => None,
        })
        .collect();
    (head, calls)
}

/// The value `method` answers when called on `value` with `argument`, or `None` for a call this
/// plane does not answer on it — which leaves the template unrendered.
///
/// `argument` is the one argument the method was called with, as its text, or `None` for a call
/// with no argument: those are the two forms the reference reader takes. The call itself is the
/// shared reading of the language's value methods, so a method it reports as no method of this
/// value's type, and one that answers nothing at all, are both this plane's unresolved.
pub(super) fn called(
    value: &::serde_json::Value,
    method: &str,
    argument: Option<&str>,
) -> Option<::serde_json::Value> {
    if !VALUE_METHODS.contains(&method) {
        return None;
    }
    let arguments: Vec<::serde_json::Value> = argument
        .map(|argument| ::serde_json::Value::String(argument.to_string()))
        .into_iter()
        .collect();
    // The shared reading takes the value MUTABLY, because the language's collection methods
    // mutate what they are called on. None of the methods this plane answers does, and the copy
    // handed over is dropped here either way, so the value a reference resolved is untouched.
    let mut target = value.clone();
    ::service_common::velocity_template::method(&mut target, method, &arguments)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::serde_json::json;

    /// RULE: a trailing call naming one of the methods this plane answers on a VALUE is split
    /// off the reference, and every other step stays with the variable surface.
    #[test]
    fn only_the_trailing_calls_a_values_type_answers_are_split_off() {
        // Rows differ along what the reference ends with: nothing, one value method, two of
        // them, a method of the variable surface, and a member.
        let rows: [(&str, Vec<Step>, usize, Vec<&str>); 6] = [
            ("no steps at all", vec![], 0, vec![]),
            (
                "a surface method alone",
                vec![Step::Call("path".to_string(), vec![])],
                1,
                vec![],
            ),
            (
                "one value method after a surface method",
                vec![
                    Step::Call("path".to_string(), vec![]),
                    Step::Call("length".to_string(), vec![]),
                ],
                1,
                vec!["length"],
            ),
            (
                "two value methods in a row",
                vec![
                    Step::Call("path".to_string(), vec![]),
                    Step::Call("trim".to_string(), vec![]),
                    Step::Call("toUpperCase".to_string(), vec![]),
                ],
                1,
                vec!["trim", "toUpperCase"],
            ),
            (
                "a value method on the variable itself",
                vec![Step::Call("size".to_string(), vec![])],
                0,
                vec!["size"],
            ),
            (
                "a member after a value method",
                vec![
                    Step::Call("length".to_string(), vec![]),
                    Step::Member("x".to_string()),
                ],
                2,
                vec![],
            ),
        ];
        for (case, steps, kept, split_off) in rows {
            let (head, calls) = split(&steps);
            assert_eq!(
                head.len(),
                kept,
                "case {case:?}: expected {kept} steps left with the variable surface, got {}",
                head.len()
            );
            let names: Vec<&str> = calls.iter().map(|(method, _)| *method).collect();
            assert_eq!(
                names, split_off,
                "case {case:?}: expected the calls {split_off:?} split off, got {names:?}"
            );
        }
    }

    /// RULE: a call this plane answers comes to what the method of the value's own type states,
    /// and a call outside the policy — or one the value's type does not carry — answers nothing
    /// at all, so the template is left unrendered.
    #[test]
    fn each_call_answers_what_the_values_own_type_states() {
        /// One row: the case, the method called, the value it is called on, the argument it
        /// carries, and what the call answers.
        type Row<'a> = (
            &'a str,
            &'a str,
            ::serde_json::Value,
            Option<&'a str>,
            Option<::serde_json::Value>,
        );
        let rows: [Row<'_>; 22] = [
            ("a string's length", "length", json!("abcd"), None, Some(json!(4))),
            ("an empty string's length", "length", json!(""), None, Some(json!(0))),
            // A character outside the basic plane is one character of the string, which is
            // what the shared reading counts.
            ("a character past the basic plane", "length", json!("a\u{1F600}"), None, Some(json!(2))),
            ("a list's size", "size", json!([1, 2, 3]), None, Some(json!(3))),
            ("a map's size", "size", json!({ "a": 1 }), None, Some(json!(1))),
            ("a list has no length", "length", json!([1]), None, None),
            ("a number carries none of them", "length", json!(7), None, None),
            ("an empty string is empty", "isEmpty", json!(""), None, Some(json!(true))),
            ("a filled list is not empty", "isEmpty", json!([1]), None, Some(json!(false))),
            ("an empty map is empty", "isEmpty", json!({}), None, Some(json!(true))),
            ("upper case", "toUpperCase", json!("aBc"), None, Some(json!("ABC"))),
            ("lower case", "toLowerCase", json!("aBc"), None, Some(json!("abc"))),
            ("trimmed", "trim", json!("  a b \n"), None, Some(json!("a b"))),
            ("a substring present", "contains", json!("abcd"), Some("bc"), Some(json!(true))),
            ("a substring absent", "contains", json!("abcd"), Some("bd"), Some(json!(false))),
            (
                "an element of a list",
                "contains",
                json!(["a", "b"]),
                Some("b"),
                Some(json!(true)),
            ),
            // `contains` is no member of a map at all; `containsKey` is, and this plane's
            // policy does not answer it.
            ("a map carries no contains", "contains", json!({ "a": 1 }), Some("a"), None),
            ("a prefix", "startsWith", json!("abcd"), Some("ab"), Some(json!(true))),
            ("a suffix", "endsWith", json!("abcd"), Some("cd"), Some(json!(true))),
            ("an index that occurs", "indexOf", json!("abcd"), Some("cd"), Some(json!(2))),
            ("an index that does not", "indexOf", json!("abcd"), Some("x"), Some(json!(-1))),
            // Outside the policy: a method whose result depends on a regular-expression
            // dialect, so the template is left unrendered rather than answered.
            (
                "a method outside this plane's policy",
                "replaceAll",
                json!("abcd"),
                Some("a"),
                None,
            ),
        ];
        for (case, method, value, argument, expected) in rows {
            let answered = called(&value, method, argument);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?} from {method} on {value}, got \
                 {answered:?}"
            );
        }
    }

    /// RULE: a call leaves the value the reference resolved exactly as it was — the methods
    /// this plane answers compute over a value rather than change it.
    #[test]
    fn a_call_leaves_the_value_it_was_called_on_unchanged() {
        let value = json!(["a", "b"]);
        let answered = called(&value, "size", None);
        assert_eq!(
            answered,
            Some(json!(2)),
            "case \"a list's size\": expected 2, got {answered:?}"
        );
        assert_eq!(
            value,
            json!(["a", "b"]),
            "case \"a list's size\": expected the value unchanged, got {value}"
        );
    }
}
