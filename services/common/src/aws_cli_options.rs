// SPDX-License-Identifier: CC0-1.0

//! The parts of an AWS CLI command line that are the CLI's own rather than one command group's:
//! how a line's NAMED PARAMETERS are read, and the option that decides what the answer to a call
//! is PRINTED as.
//!
//! # The rule this module implements
//!
//! Several emulated services run a command line a client supplied — a container's `command`, a
//! cluster step's arguments, a notebook hook's script — and such a line reaches a service of the
//! account through the AWS CLI, which "provides direct access to the public APIs of Amazon Web
//! Services" one command at a time. Every command of every group is written the same way: the
//! group, the command, and then NAMED parameters only ("you can specify the parameter value with
//! a space or an equals sign", the AWS CLI's command-structure reference), where a parameter that
//! takes a LIST is written as the words after it and a parameter that takes no value at all is a
//! switch. And whatever the command, the CLI prints the service's answer in one of "the following
//! output formats", which `--output` selects (the AWS CLI's "controlling command output"
//! reference).
//!
//! That reading and that printing are therefore the CLI's, so they live here once rather than per
//! command group: two groups must not read a line differently or print an answer differently.
//! What each group keeps is its own — WHICH parameters its command takes and WHICH call carries
//! it ([`crate::aws_cloudwatch_cli`], [`crate::aws_xray_cli`], [`crate::aws_ssm_cli`],
//! [`crate::aws_sns_cli`]). The two SPELLINGS a parameter's value is written in are read through
//! the one function this workspace has always read them with
//! ([`crate::aws_s3_cli::parameter_of`]), which this reader adds the switch and list forms around;
//! and the format an answer is written in is the one rendering of each of the CLI's two output
//! formats ([`printed_answer`], which [`crate::aws_s3_cli::printed_document`] and
//! [`crate::aws_s3_cli::printed_values`] build their member list's document for and render
//! through).
//!
//! A line this module cannot read as named parameters — one carrying an operand, a parameter with
//! no value after it, or a parameter given twice — is answered `None`, which the caller's session
//! reports as a command it could not run rather than as a call: a line the CLI would refuse must
//! not become a call with a member missing or a member of this emulator's choosing.
//!
//! # What this reading does NOT perform
//!
//! `--query`, "a JMESPath query to use in filtering the response data", is not a parameter any of
//! these commands takes here: a line carrying it is not an invocation this program performs, and
//! the caller's session reports it as a command it could not run — the same answer every other
//! option a command does not take gets. Filtering an answer would mean EVALUATING an expression a
//! caller wrote, which is an execution substrate this reading deliberately does not bring: nothing
//! here interprets caller text, and the only thing a command line can make this module do is read
//! its own words and write the answer the service gave.

use ::std::collections::BTreeMap;

/// What the CLI prints the answer to one call as, which `--output` selects: "The AWS CLI supports
/// the following output formats" — `json`, "the output is formatted as a JSON string", and
/// `text`, "the output is formatted as multiple lines of tab-separated string values" (AWS CLI,
/// controlling command output). Every other format is one this program does not print, so a line
/// asking for it is not an invocation it performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// `json`, the CLI's default output format.
    #[default]
    Json,
    /// `text`.
    Text,
}

impl OutputFormat {
    /// The format `written` names, or `None` for one this program does not print.
    #[must_use]
    pub fn of(written: &str) -> Option<Self> {
        match written {
            "json" => Some(Self::Json),
            "text" => Some(Self::Text),
            _ => None,
        }
    }
}

/// The option that names the Region a command's calls are addressed to, as the CLI spells it
/// without its dashes: "`--region` (string) The region to use. Overrides config/env settings".
pub const REGION_PARAMETER: &str = "region";

/// `--output`, the format the answer is printed in.
pub const OUTPUT_PARAMETER: &str = "output";

/// The two options above: the ones every command of every group takes here, which a group adds to
/// the parameters of its own command.
///
/// `--query` is not among them ([the module docs](self) say why), so a line carrying it is not an
/// invocation any of these commands performs.
pub const GLOBAL_PARAMETERS: [&str; 2] = [REGION_PARAMETER, OUTPUT_PARAMETER];

/// How deeply the printer walks the answer it writes.
///
/// THIS NUMBER IS THE EMULATOR'S: the walk below is recursive, so it is bounded explicitly rather
/// than by the stack of the thread it runs on. Far above the nesting of any response document of
/// these groups, which is one service's own answer and no caller's.
pub const MAX_PRINTED_DEPTH: usize = 64;

/// `document` written in the format `output` selects — the ONE rendering of each of the CLI's two
/// output formats in this workspace.
///
/// A group whose response members are KNOWN states them as a member list and prints them through
/// [`crate::aws_s3_cli::printed_document`] / [`crate::aws_s3_cli::printed_values`], which build
/// their document and render it HERE; this one takes the whole document a group built.
#[must_use]
pub fn printed_answer(document: &::serde_json::Value, output: OutputFormat) -> String {
    match output {
        OutputFormat::Json => printed_json(document),
        OutputFormat::Text => printed_text(document, MAX_PRINTED_DEPTH),
    }
}

/// `selected` in the CLI's default format: "the output is formatted as a JSON string", pretty
/// printed as the CLI prints it. A selection of nothing at all prints nothing, which is what the
/// CLI writes for a command whose response document has no members.
fn printed_json(selected: &::serde_json::Value) -> String {
    if selected.is_null() {
        return String::new();
    }
    if let Some(object) = selected.as_object() {
        if object.is_empty() {
            return String::new();
        }
    }
    ::serde_json::to_string_pretty(selected).unwrap_or_default()
}

/// `selected` in the CLI's `text` format: "multiple lines of tab-separated string values".
///
/// A string, a number and a boolean are written as themselves; the members of an object are
/// written separated by tabs, by member NAME (which is the order this emulator's own response
/// documents hold their members in, and the order that format writes them in); each element of a
/// list is written on its own line; and a value the service answered nothing for is written
/// `None`, which is what that format writes for one.
///
/// `depth_left` is how many levels further this walk goes: the counter that makes the recursion
/// bounded explicitly, rather than by the stack of the thread it runs on. No response document of
/// these groups reaches it, and past it nothing more is written.
fn printed_text(selected: &::serde_json::Value, depth_left: usize) -> String {
    use ::serde_json::Value;

    match selected {
        Value::Null => "None".to_string(),
        Value::Bool(held) => if *held { "True" } else { "False" }.to_string(),
        Value::Number(held) => held.to_string(),
        Value::String(held) => held.clone(),
        Value::Array(_) | Value::Object(_) if depth_left == 0 => String::new(),
        Value::Array(held) => held
            .iter()
            .map(|element| printed_text(element, depth_left - 1))
            .collect::<Vec<String>>()
            .join("\n"),
        Value::Object(held) => held
            .values()
            .map(|member| printed_text(member, depth_left - 1))
            .collect::<Vec<String>>()
            .join("\t"),
    }
}

/// The NAMED parameters of one command line, in the order they were written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandLine {
    /// The values held per parameter, each under the parameter's own name without its dashes. A
    /// switch holds no value, a parameter holds one, and a list parameter holds the words after
    /// it.
    held: BTreeMap<String, Vec<String>>,
}

impl CommandLine {
    /// What the words of one command line AFTER its group and command name — `words` — ask for,
    /// or `None` for a line this program reads no call out of.
    ///
    /// `switches` are the parameters that take no value ("`--with-decryption` … (boolean)"), and
    /// `lists` the ones that take several ("(list)"); every other named parameter takes exactly
    /// one value, in either of the two spellings the CLI takes it in. A line carrying an operand,
    /// a parameter with no value after it, a list with no element, or any parameter twice is
    /// answered `None`: each is a line the CLI would refuse rather than one it would turn into a
    /// call.
    #[must_use]
    pub fn read<'a>(
        words: impl IntoIterator<Item = &'a str>,
        switches: &[&str],
        lists: &[&str],
    ) -> Option<Self> {
        let mut held: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut words = words.into_iter().peekable();
        while let Some(word) = words.next() {
            // An operand is not a form these commands take: every member of the call is named.
            let parameter = word.strip_prefix("--")?;
            let (parameter, attached) = match parameter.split_once('=') {
                Some((parameter, value)) => (parameter, Some(value.to_string())),
                None => (parameter, None),
            };
            let values = match (switches.contains(&parameter), lists.contains(&parameter)) {
                // A switch takes no value; one written with a value attached is not this
                // invocation.
                (true, _) => {
                    if attached.is_some() {
                        return None;
                    }
                    Vec::new()
                }
                // A list takes the words after it, up to the next named parameter.
                (_, true) => {
                    let mut values: Vec<String> = attached.into_iter().collect();
                    while let Some(next) = words.peek() {
                        if next.starts_with("--") {
                            break;
                        }
                        values.push((*next).to_string());
                        words.next();
                    }
                    if values.is_empty() {
                        return None;
                    }
                    values
                }
                // Every other parameter takes exactly one value: the word after it, or the one
                // attached with an equals sign — both read by this workspace's one reading of the
                // two spellings the CLI takes a value in.
                (false, false) => match crate::aws_s3_cli::parameter_of(word, &mut words) {
                    Some((_, value)) => vec![value],
                    None => return None,
                },
            };
            // A parameter given twice is not an invocation either.
            if held.insert(parameter.to_string(), values).is_some() {
                return None;
            }
        }
        Some(Self { held })
    }

    /// Whether every parameter the line names is one of `taken` — a parameter outside that set
    /// changes what the call is, so a line carrying one is not the invocation at all.
    #[must_use]
    pub fn names_only(&self, taken: &[&str]) -> bool {
        self.held
            .keys()
            .all(|named| taken.contains(&named.as_str()))
    }

    /// The one value `parameter` was given, and `None` for a parameter the line does not name or
    /// gave several values to.
    #[must_use]
    pub fn one(&self, parameter: &str) -> Option<&str> {
        match self.held.get(parameter)?.as_slice() {
            [value] => Some(value.as_str()),
            _ => None,
        }
    }

    /// Every value `parameter` was given, and `None` for a parameter the line does not name.
    #[must_use]
    pub fn many(&self, parameter: &str) -> Option<&[String]> {
        self.held.get(parameter).map(Vec::as_slice)
    }

    /// Whether the line carries the switch `parameter`.
    #[must_use]
    pub fn carries(&self, parameter: &str) -> bool {
        self.held.contains_key(parameter)
    }

    /// The Region the line addresses its call to ([`REGION_PARAMETER`]), and `None` for a line
    /// naming none — addressed to the Region its environment is in.
    #[must_use]
    pub fn region(&self) -> Option<String> {
        self.one(REGION_PARAMETER)
            .filter(|region| !region.is_empty())
            .map(ToString::to_string)
    }
}

/// What a call a SERVICE refused reports, as the CLI words a service's own refusal — the one
/// spelling of that sentence in this workspace ([`crate::aws_s3_cli::store_answered`]).
pub use crate::aws_s3_cli::store_answered as service_answered;

/// The status one of these commands answers when the call was made: "0 -- Command was successful"
/// (AWS CLI return codes).
pub use crate::aws_s3_cli::TRANSFER_SUCCEEDED as COMMAND_SUCCEEDED;

/// The status it answers when the service REFUSED the call: "254 -- The command was successfully
/// parsed and a request was made to the specified service but the service returned an error". It
/// is the status a shell reads, so a program written `if aws xray put-trace-segments …; then …
/// else … fi` takes its other branch on a refusal.
pub use crate::aws_s3_cli::API_SERVICE_ERROR as COMMAND_SERVICE_ERROR;

/// The status it answers when the CLI itself could not carry the command out — no credential to
/// sign with, and an answer it could not print, among them: "255 -- General error".
pub use crate::aws_s3_cli::API_COMMAND_ERROR as COMMAND_ERROR;

#[cfg(test)]
mod tests {
    use super::{CommandLine, OutputFormat};

    /// The words of a command line after the group and the command, as a session hands them to a
    /// program.
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(ToString::to_string).collect()
    }

    /// One line read with the switch and list parameters below.
    fn read(line: &str) -> Option<CommandLine> {
        let held = words(line);
        CommandLine::read(
            held.iter().map(String::as_str),
            &["with-decryption"],
            &["trace-segment-documents"],
        )
    }

    /// One row of the reading cases: the case, the words after the command, and what each
    /// parameter of the line holds — or `None` for a line no call is read out of.
    type ReadingCase = (&'static str, &'static str, Option<Vec<(&'static str, Vec<&'static str>)>>);

    /// RULE: a command line is NAMED parameters only, each written in either of the two spellings
    /// the CLI takes a value in, where a switch takes none and a list takes the words after it —
    /// and a line that is not written that way is one no call is read out of.
    ///
    /// The rows differ along what the rule generalises over: the spelling of a value, the KIND of
    /// parameter (one value, a switch, a list), how many values a list was given, and the four
    /// forms that are no invocation at all.
    #[test]
    fn the_lines_a_call_is_read_out_of() {
        // (case, the words after the command, what each parameter holds)
        let rows: [ReadingCase; 9] = [
            (
                "a value written as the next word",
                "--name /flint/mode",
                Some(vec![("name", vec!["/flint/mode"])]),
            ),
            (
                "a value written with an equals sign",
                "--name=/flint/mode",
                Some(vec![("name", vec!["/flint/mode"])]),
            ),
            (
                "a switch, which takes no value",
                "--name /flint/mode --with-decryption",
                Some(vec![("name", vec!["/flint/mode"]), ("with-decryption", vec![])]),
            ),
            (
                "a list given one element",
                "--trace-segment-documents {}",
                Some(vec![("trace-segment-documents", vec!["{}"])]),
            ),
            (
                "a list given three elements, up to the next parameter",
                "--trace-segment-documents one two three --region us-west-2",
                Some(vec![
                    ("region", vec!["us-west-2"]),
                    ("trace-segment-documents", vec!["one", "two", "three"]),
                ]),
            ),
            ("an operand, which no command here takes", "/flint/mode", None),
            ("a parameter with no value after it", "--name", None),
            ("a parameter given twice", "--name one --name two", None),
            ("a list given no element", "--trace-segment-documents --region us-west-2", None),
        ];
        for (case, line, expected) in rows {
            let read = read(line);
            match expected {
                None => assert!(
                    read.is_none(),
                    "case {case:?}: expected no call to be read, got {read:?}"
                ),
                Some(expected) => {
                    let read =
                        read.unwrap_or_else(|| panic!("case {case:?}: expected a line to be read"));
                    for (parameter, values) in expected {
                        let held = read.many(parameter);
                        let wanted: Vec<String> =
                            values.iter().map(|value| (*value).to_string()).collect();
                        assert_eq!(
                            held,
                            Some(wanted.as_slice()),
                            "case {case:?}: expected {parameter:?} to hold {wanted:?}, got \
                             {held:?}"
                        );
                    }
                }
            }
        }
    }

    /// RULE: the answer to a call is printed in the format `--output` selects, over the response
    /// document the group built — a JSON string, or its values tab-separated, with a member the
    /// service answered nothing for written as that format writes one.
    ///
    /// The rows differ along the SHAPE of what is printed (an object, a nested object, a list, a
    /// number, nothing at all) and the format asked for.
    #[test]
    fn the_answer_is_printed_in_the_format_the_line_asked_for() {
        let document = ::serde_json::json!({
            "Parameter": {
                "Name": "/flint/mode",
                "Type": "String",
                "Value": "degraded",
                "Version": 3,
            },
        });
        // (case, the document, the format, what is printed)
        let rows: [(&str, ::serde_json::Value, OutputFormat, &str); 5] = [
            (
                "a document whose members are written tab-separated, each level in turn",
                document.clone(),
                OutputFormat::Text,
                "/flint/mode\tString\tdegraded\t3",
            ),
            (
                "one member of it, printed as text",
                ::serde_json::json!("degraded"),
                OutputFormat::Text,
                "degraded",
            ),
            (
                "one member of it, printed as JSON",
                ::serde_json::json!("degraded"),
                OutputFormat::Json,
                "\"degraded\"",
            ),
            (
                "a list, whose elements are written one per line",
                ::serde_json::json!(["one", "two"]),
                OutputFormat::Text,
                "one\ntwo",
            ),
            (
                "a response with no member at all, which prints nothing",
                ::serde_json::json!({}),
                OutputFormat::Json,
                "",
            ),
        ];
        for (case, printed, output, expected) in rows {
            let written = super::printed_answer(&printed, output);
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
        // A member the service answered nothing for is written the way that format writes one.
        let written = super::printed_answer(&::serde_json::json!(null), OutputFormat::Text);
        assert_eq!(
            written, "None",
            "case \"a member answered nothing for\": expected \"None\", got {written:?}"
        );
    }

    /// RULE: the printer's own walk is bounded by a COUNTER rather than by the stack of the thread
    /// it runs on, whatever it is handed.
    #[test]
    fn the_printer_walks_no_deeper_than_its_bound() {
        let mut deeper = ::serde_json::json!("leaf");
        for _ in 0..=super::MAX_PRINTED_DEPTH {
            deeper = ::serde_json::json!([deeper]);
        }
        // Nothing past the counter is walked, so a document nested deeper than the bound is
        // written down to it and no further — the walk ENDS rather than recursing on.
        let written = super::printed_answer(&deeper, OutputFormat::Text);
        assert!(
            written.is_empty(),
            "case \"a document nested past the bound\": expected the walk to stop, got \
             {written:?}"
        );
        let written = super::printed_text(&::serde_json::json!([["leaf"]]), 1);
        assert_eq!(
            written, "",
            "case \"the walk at its last level\": expected it to write nothing more, got \
             {written:?}"
        );
        let written = super::printed_text(&::serde_json::json!([["leaf"]]), 8);
        assert_eq!(
            written, "leaf",
            "case \"the walk inside its bound\": expected the leaf, got {written:?}"
        );
    }
}
