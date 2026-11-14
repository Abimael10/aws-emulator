// SPDX-License-Identifier: CC0-1.0

//! The TEXT FILTERS a shell session performs: the utilities that write what they computed
//! from their input.
//!
//! # The rule this module implements
//!
//! A command of a build container, a managed node or a guest machine is written against the
//! text utilities the image provides, and each of them answers out of ONE input — the file it
//! was given, or its standard input — and writes what it computed over it. So what a pipeline
//! of them comes to follows from the bytes that went in and from nothing else: a digest is the
//! digest of those bytes, a count is their count, and a selection is the part of them the
//! option names.
//!
//! The utilities here are the ones whose whole effect is that computation, with the output
//! form each of them is specified to write:
//!
//! * `wc -c` / `-m` / `-l` / `-w` — the bytes, characters, newlines or words of its input, "one
//!   count only" when a single option selects one, and the name of the file after the count
//!   when it was given one;
//! * `cut -c LIST` / `-f LIST [-d DELIM]` — "select portions of each line of a file", the
//!   characters or the delimited fields the list names, one output line per input line;
//! * `head -n COUNT` — "the first COUNT lines of each file", ten lines when no count is given;
//! * `sha256sum` / `sha512sum` — the digest of its input, written as the lowercase hexadecimal
//!   digest, two spaces and the name of what was read (`-` for standard input).
//!
//! The option spellings and the output forms above are each utility's own, from the GNU
//! coreutils manual (`wc`, `cut`, `head`, `sha2 utilities`) — which is what the images these
//! sessions stand for carry — and POSIX for `wc`, `cut` and `head`.
//!
//! # What is NOT performed
//!
//! Only forms whose output follows from the input alone, and ONE input at a time:
//!
//! * a `wc` with no count option writes three counts in a width it computes from the files it
//!   read, so that form is not performed rather than written in a width of this module's
//!   choosing;
//! * several file operands at once are not performed either: `wc` then writes a `total` line,
//!   `head` writes a header per file, and both pad — none of which follows from one input;
//! * every other utility, option and operand form answers as a command the session could not
//!   run ([`Filter::read`] answering `None`), which is the fail-closed default the session's
//!   own catalog keeps.
//!
//! This module computes over the text it is handed: it opens nothing, reads no path of the
//! host, and holds nothing between calls. WHERE the input comes from — a file of the session's
//! tree or the stage before it in a pipeline — is the session's to resolve
//! ([`crate::posix_shell_session`]).

use ::sha2::Digest;

/// The utilities this module performs, as the names a command line invokes them by. A session
/// hands a command whose name is one of these to [`Filter::read`].
pub const TEXT_FILTER_COMMANDS: &[&str] = &["wc", "cut", "head", "sha256sum", "sha512sum"];

/// Which count a `wc` invocation asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Count {
    /// `-c`: "the number of bytes".
    Bytes,
    /// `-m`: "the number of characters".
    Characters,
    /// `-l`: "the number of newlines".
    Lines,
    /// `-w`: "the number of words", a word being "a nonempty sequence of characters delimited
    /// by white space".
    Words,
}

/// Which digest a checksum invocation writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Digester {
    /// `sha256sum`.
    Sha256,
    /// `sha512sum`.
    Sha512,
}

/// What one `cut` invocation selects out of each line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Portion {
    /// `-c LIST`: the characters the list names.
    Characters(Ranges),
    /// `-f LIST [-d DELIM]`: the delimited fields the list names.
    Fields {
        /// The fields selected.
        ranges: Ranges,
        /// The delimiter, which is a TAB when none was given.
        delimiter: char,
    },
}

/// The work one invocation of a text filter performs over ONE input.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Work {
    /// `wc`, counting one way.
    Count(Count),
    /// `cut`, selecting one portion of each line.
    Cut(Portion),
    /// `head`, keeping the first lines.
    Head(usize),
    /// `sha256sum` / `sha512sum`.
    Digest(Digester),
}

/// One invocation of a text filter: the work it performs, and the file it reads when it names
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// What this invocation computes.
    work: Work,
    /// The file operand, and `None` for an invocation that reads standard input.
    file: Option<String>,
}

/// How many lines `head` writes when the invocation names no count: "head will print the first
/// 10 lines".
const DEFAULT_HEAD_LINES: usize = 10;

/// The delimiter `cut -f` takes when the invocation names none: "the default is TAB".
const DEFAULT_FIELD_DELIMITER: char = '\t';

/// What the output names for an input read from standard input, as a checksum utility writes
/// it.
pub const STANDARD_INPUT_LABEL: &str = "-";

impl Filter {
    /// The invocation `name` with `arguments` performs, or `None` for a utility, option or
    /// operand form this module does not perform (see the module docs).
    #[must_use]
    pub fn read(name: &str, arguments: &[String]) -> Option<Self> {
        // The options that carry a VALUE take it either glued to them (`-c1-64`) or as the
        // word after them (`-c 1-64`), and a utility takes both spellings; the second form is
        // folded into the first here, so what follows reads one shape — and a delimiter that
        // is a space or a digit is that option's value rather than an operand.
        let value_taking = value_taking_options(name);
        let mut options: Vec<String> = Vec::new();
        let mut operands: Vec<String> = Vec::new();
        let mut taken = arguments.iter();
        while let Some(argument) = taken.next() {
            if argument.len() > 1 && argument.starts_with('-') {
                if value_taking.contains(&argument.as_str()) {
                    // An option whose value the invocation never wrote is not a form the
                    // utility performs either.
                    options.push(format!("{argument}{}", taken.next()?));
                } else {
                    options.push(argument.clone());
                }
                continue;
            }
            operands.push(argument.clone());
        }
        // ONE input at a time: several files make an output this module does not write (see the
        // module docs), and `-` is the spelling for the standard input it would read anyway.
        let file = match operands.len() {
            0 => None,
            1 => operands
                .first()
                .filter(|operand| operand.as_str() != STANDARD_INPUT_LABEL)
                .cloned(),
            _ => return None,
        };
        let work = match name {
            "wc" => Work::Count(count_of(&options)?),
            "cut" => Work::Cut(portion_of(&options)?),
            "head" => Work::Head(head_lines(&options)?),
            "sha256sum" => Work::Digest(Digester::Sha256),
            "sha512sum" => Work::Digest(Digester::Sha512),
            _ => return None,
        };
        Some(Self { work, file })
    }

    /// The file this invocation reads, and `None` for one that reads standard input.
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// What this invocation writes to standard output for `input`, which was read from
    /// `label` — the file's own name, or [`STANDARD_INPUT_LABEL`] for standard input.
    #[must_use]
    pub fn perform(&self, label: &str, input: &str) -> String {
        match &self.work {
            Work::Count(count) => {
                let counted = counted(*count, input);
                // The count alone for standard input; the count and the file's name for a file
                // operand, which is the form the utility writes for one.
                match self.file.as_deref() {
                    Some(_) => format!("{counted} {label}\n"),
                    None => format!("{counted}\n"),
                }
            }
            Work::Cut(portion) => cut_lines(portion, input),
            Work::Head(lines) => head_of(*lines, input),
            Work::Digest(digester) => {
                format!("{}  {label}\n", digest_of(*digester, input))
            }
        }
    }
}

/// The options of `name` that carry a VALUE, which an invocation may write glued to them or
/// as the word after them ([`Filter::read`] folds the second spelling into the first).
fn value_taking_options(name: &str) -> &'static [&'static str] {
    match name {
        // `cut`'s lists and its delimiter.
        "cut" => &["-c", "-f", "-d"],
        // `head`'s line count.
        "head" => &["-n"],
        // `wc`'s counts and the checksum utilities take none.
        _ => &[],
    }
}

/// The count a `wc` invocation's options select, or `None` when they select none or more than
/// one — the forms whose output does not follow from one input (see the module docs).
fn count_of(options: &[String]) -> Option<Count> {
    let mut selected = None;
    for option in options {
        let count = match option.as_str() {
            "-c" | "--bytes" => Count::Bytes,
            "-m" | "--chars" => Count::Characters,
            "-l" | "--lines" => Count::Lines,
            "-w" | "--words" => Count::Words,
            _ => return None,
        };
        if selected.is_some_and(|held| held != count) {
            return None;
        }
        selected = Some(count);
    }
    selected
}

/// What `wc` answers for `input` under `count`.
fn counted(count: Count, input: &str) -> usize {
    match count {
        Count::Bytes => input.len(),
        Count::Characters => input.chars().count(),
        // "the number of newlines", which is what the utility counts — a last line with no
        // newline after it is not counted.
        Count::Lines => input.matches('\n').count(),
        Count::Words => input.split_whitespace().count(),
    }
}

/// The portion a `cut` invocation's options select, or `None` for an invocation that selects
/// none, selects both kinds at once, or carries an option this module does not perform.
///
/// Every option here carries its value glued to it ([`Filter::read`]).
fn portion_of(options: &[String]) -> Option<Portion> {
    let mut characters = None;
    let mut fields = None;
    let mut delimiter = None;
    for option in options {
        if let Some(list) = non_empty(option.strip_prefix("-c")) {
            characters = Some(ranges_of(list)?);
            continue;
        }
        if let Some(list) = non_empty(option.strip_prefix("-f")) {
            fields = Some(ranges_of(list)?);
            continue;
        }
        if let Some(written) = non_empty(option.strip_prefix("-d")) {
            // One character, which is what the utility's delimiter is.
            let mut written = written.chars();
            let only = written.next()?;
            if written.next().is_some() {
                return None;
            }
            delimiter = Some(only);
            continue;
        }
        return None;
    }
    match (characters, fields) {
        (Some(ranges), None) if delimiter.is_none() => Some(Portion::Characters(ranges)),
        (None, Some(ranges)) => Some(Portion::Fields {
            ranges,
            delimiter: delimiter.unwrap_or(DEFAULT_FIELD_DELIMITER),
        }),
        _ => None,
    }
}

/// `value` when an option carried one, and `None` for an option that is not this one or that
/// carried nothing.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// How many lines a `head` invocation keeps, or `None` for an option form this module does not
/// perform.
fn head_lines(options: &[String]) -> Option<usize> {
    let mut lines = None;
    for option in options {
        let written = non_empty(option.strip_prefix("-n"))?;
        lines = Some(written.trim().parse::<usize>().ok()?);
    }
    Some(lines.unwrap_or(DEFAULT_HEAD_LINES))
}

/// The positions one `cut` list names, as the ranges it is written with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ranges(Vec<(usize, Option<usize>)>);

impl Ranges {
    /// Whether the position `index` — ONE-based, as the utility's lists are written — is
    /// selected.
    fn holds(&self, index: usize) -> bool {
        self.0
            .iter()
            .any(|(low, high)| index >= *low && high.is_none_or(|high| index <= high))
    }
}

/// The ranges `list` names — `N`, `N-M`, `-M` and `N-`, separated by commas — or `None` for a
/// list this module does not read.
fn ranges_of(list: &str) -> Option<Ranges> {
    let mut ranges = Vec::new();
    for written in list.split(',') {
        let written = written.trim();
        if written.is_empty() {
            return None;
        }
        let range = match written.split_once('-') {
            // `N`: that one position.
            None => {
                let only = written.parse::<usize>().ok()?;
                (only, Some(only))
            }
            // `-M`: from the first position through M.
            Some(("", high)) => (1, Some(high.parse::<usize>().ok()?)),
            // `N-`: from N through the end of the line.
            Some((low, "")) => (low.parse::<usize>().ok()?, None),
            // `N-M`.
            Some((low, high)) => (low.parse::<usize>().ok()?, Some(high.parse::<usize>().ok()?)),
        };
        // A position is one-based, and a range whose end is before its start names nothing the
        // utility would select.
        if range.0 == 0 || range.1.is_some_and(|high| high < range.0) {
            return None;
        }
        ranges.push(range);
    }
    (!ranges.is_empty()).then_some(Ranges(ranges))
}

/// Every line of `input`, without the newline that ended it — a last line with no newline is
/// still a line.
fn lines_of(input: &str) -> Vec<&str> {
    if input.is_empty() {
        return Vec::new();
    }
    input
        .strip_suffix('\n')
        .unwrap_or(input)
        .split('\n')
        .collect()
}

/// `cut`'s selection out of each line of `input`, each written on its own line — which the
/// utility ends with a newline whether or not the input's last line carried one.
fn cut_lines(portion: &Portion, input: &str) -> String {
    let mut written = String::new();
    for line in lines_of(input) {
        match portion {
            Portion::Characters(ranges) => {
                for (index, character) in line.chars().enumerate() {
                    if ranges.holds(index.saturating_add(1)) {
                        written.push(character);
                    }
                }
            }
            Portion::Fields {
                ranges,
                delimiter,
            } => {
                let fields: Vec<&str> = line.split(*delimiter).collect();
                // "Lines with no delimiters are passed through untouched", which is the
                // utility's default without `-s`.
                if fields.len() == 1 {
                    written.push_str(line);
                } else {
                    let selected: Vec<&str> = fields
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| ranges.holds(index.saturating_add(1)))
                        .map(|(_, field)| *field)
                        .collect();
                    written.push_str(&selected.join(&delimiter.to_string()));
                }
            }
        }
        written.push('\n');
    }
    written
}

/// The first `lines` lines of `input`, each with the newline it carried.
fn head_of(lines: usize, input: &str) -> String {
    let mut written = String::new();
    let ends_with_newline = input.ends_with('\n');
    let held = lines_of(input);
    let kept = held.len().min(lines);
    for (index, line) in held.iter().take(lines).enumerate() {
        written.push_str(line);
        // The last line keeps a newline only if the input's own last line did.
        if index + 1 < kept || ends_with_newline {
            written.push('\n');
        }
    }
    written
}

/// The lowercase hexadecimal digest of `input`.
fn digest_of(digester: Digester, input: &str) -> String {
    match digester {
        Digester::Sha256 => {
            let mut hasher = ::sha2::Sha256::new();
            hasher.update(input.as_bytes());
            ::hex::encode(hasher.finalize())
        }
        Digester::Sha512 => {
            let mut hasher = ::sha2::Sha512::new();
            hasher.update(input.as_bytes());
            ::hex::encode(hasher.finalize())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One invocation, as a command line's words.
    fn words(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|word| (*word).to_string()).collect()
    }

    /// RULE: a filter writes what it computed over its ONE input, in the output form its own
    /// utility is specified to write.
    ///
    /// The rows differ along what the rule generalises over: which utility, which option, how
    /// the option's value is written, what the input holds and whether it came from a file or
    /// from standard input. The digests are of the input bytes, so each row's expectation is
    /// computed the same way the utility's own specification states — see the per-row note.
    #[test]
    fn a_filter_writes_what_it_computed() {
        // (case, the utility, its arguments, the label, the input, what it writes)
        /// One row: the case, the utility, its arguments, the label the input was read from,
        /// the input itself, and what the filter writes for it.
        type Row = (
            &'static str,
            &'static str,
            Vec<String>,
            &'static str,
            &'static str,
            String,
        );
        let rows: Vec<Row> = vec![
            (
                "wc -c counts the bytes of standard input",
                "wc",
                words(&["-c"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "6\n".to_string(),
            ),
            (
                "wc -c counts bytes, not characters",
                "wc",
                words(&["-c"]),
                STANDARD_INPUT_LABEL,
                "héllo",
                "6\n".to_string(),
            ),
            (
                "wc -m counts characters",
                "wc",
                words(&["-m"]),
                STANDARD_INPUT_LABEL,
                "héllo",
                "5\n".to_string(),
            ),
            (
                "wc -l counts newlines",
                "wc",
                words(&["-l"]),
                STANDARD_INPUT_LABEL,
                "one\ntwo\nthree",
                "2\n".to_string(),
            ),
            (
                "wc -w counts words",
                "wc",
                words(&["-w"]),
                STANDARD_INPUT_LABEL,
                "  one   two\tthree\n",
                "3\n".to_string(),
            ),
            (
                "wc names the file it read",
                "wc",
                words(&["-c", "notes.txt"]),
                "notes.txt",
                "abc",
                "3 notes.txt\n".to_string(),
            ),
            (
                "cut -c with a glued range",
                "cut",
                words(&["-c1-3"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "abc\n".to_string(),
            ),
            (
                "cut -c with the range as the next word",
                "cut",
                words(&["-c", "2-4"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "bcd\n".to_string(),
            ),
            (
                "cut -c with an open range and a list",
                "cut",
                words(&["-c1,4-"]),
                STANDARD_INPUT_LABEL,
                "abcdef",
                "adef\n".to_string(),
            ),
            (
                "cut -c runs over every line",
                "cut",
                words(&["-c1"]),
                STANDARD_INPUT_LABEL,
                "abc\ndef\n",
                "a\nd\n".to_string(),
            ),
            (
                "cut -f with a delimiter",
                "cut",
                words(&["-d", ":", "-f2"]),
                STANDARD_INPUT_LABEL,
                "one:two:three\n",
                "two\n".to_string(),
            ),
            (
                "cut -f passes a line with no delimiter through",
                "cut",
                words(&["-d:", "-f2"]),
                STANDARD_INPUT_LABEL,
                "undelimited\n",
                "undelimited\n".to_string(),
            ),
            (
                "cut -f defaults to a tab delimiter",
                "cut",
                words(&["-f2"]),
                STANDARD_INPUT_LABEL,
                "one\ttwo\n",
                "two\n".to_string(),
            ),
            (
                "cut -f with a space delimiter written as the word after the option",
                "cut",
                words(&["-d", " ", "-f2"]),
                STANDARD_INPUT_LABEL,
                "one two\n",
                "two\n".to_string(),
            ),
            (
                "head keeps ten lines by default",
                "head",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n",
                "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n".to_string(),
            ),
            (
                "head -n keeps the count it names",
                "head",
                words(&["-n", "2"]),
                STANDARD_INPUT_LABEL,
                "1\n2\n3\n",
                "1\n2\n".to_string(),
            ),
            (
                "head -n keeps a last line with no newline as it was",
                "head",
                words(&["-n2"]),
                STANDARD_INPUT_LABEL,
                "1\n2",
                "1\n2".to_string(),
            ),
            (
                // The digest of no bytes at all, which is the published SHA-256 of the empty
                // input.
                "sha256sum of standard input",
                "sha256sum",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  -\n"
                    .to_string(),
            ),
            (
                // "abc", whose SHA-256 is the vector published with the algorithm.
                "sha256sum names the file it read",
                "sha256sum",
                words(&["payload.txt"]),
                "payload.txt",
                "abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  \
                 payload.txt\n"
                    .to_string(),
            ),
            (
                // "abc", whose SHA-512 is the other vector published with the algorithm.
                "sha512sum of standard input",
                "sha512sum",
                words(&[]),
                STANDARD_INPUT_LABEL,
                "abc",
                "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a8\
                 36ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f  -\n"
                    .to_string(),
            ),
        ];
        for (case, name, arguments, label, input, expected) in rows {
            let filter = Filter::read(name, &arguments)
                .unwrap_or_else(|| panic!("case {case:?}: expected {name} {arguments:?} to be a \
                                           form this module performs"));
            let written = filter.perform(label, input);
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected:?}, got {written:?}"
            );
        }
    }

    /// RULE: a utility, option or operand form whose output does not follow from ONE input is
    /// not performed at all.
    #[test]
    fn a_form_outside_the_catalog_is_not_performed() {
        // (case, the utility, its arguments)
        let rows: Vec<(&str, &str, Vec<String>)> = vec![
            ("wc with no count option", "wc", words(&[])),
            ("wc with two different counts", "wc", words(&["-c", "-l"])),
            ("wc with an option it has none of", "wc", words(&["-L"])),
            ("two file operands", "wc", words(&["-c", "one", "two"])),
            ("cut with no list", "cut", words(&["-d:"])),
            ("cut selecting both kinds at once", "cut", words(&["-c1", "-f1"])),
            ("cut with a list that names nothing", "cut", words(&["-c0"])),
            ("cut with a reversed range", "cut", words(&["-c4-2"])),
            ("cut with a list that is not one", "cut", words(&["-cone"])),
            ("cut with a multi-character delimiter", "cut", words(&["-d::", "-f1"])),
            ("an option whose value was never written", "cut", words(&["-c"])),
            ("head with a count that is not one", "head", words(&["-n", "two"])),
            ("head with an option it has none of", "head", words(&["-c", "2"])),
            ("a utility this module has none of", "tail", words(&["-n1"])),
        ];
        for (case, name, arguments) in rows {
            let read = Filter::read(name, &arguments);
            assert!(
                read.is_none(),
                "case {case:?}: expected {name} {arguments:?} not to be performed, got {read:?}"
            );
        }
    }

    /// RULE: a file operand is the input the filter reads, and `-` names standard input — so a
    /// session knows which to hand it.
    #[test]
    fn the_operand_names_where_the_input_comes_from() {
        // (case, the utility, its arguments, the file it reads)
        let rows: Vec<(&str, &str, Vec<String>, Option<&str>)> = vec![
            ("no operand at all", "sha256sum", words(&[]), None),
            (
                "the standard-input spelling",
                "sha256sum",
                words(&["-"]),
                None,
            ),
            (
                "a file operand",
                "sha256sum",
                words(&["tree/file.txt"]),
                Some("tree/file.txt"),
            ),
            (
                "a file operand after an option",
                "cut",
                words(&["-c1", "file.txt"]),
                Some("file.txt"),
            ),
            (
                "an option whose value is the next word is not an operand",
                "cut",
                words(&["-c", "1-2"]),
                None,
            ),
        ];
        for (case, name, arguments, expected) in rows {
            let filter = Filter::read(name, &arguments)
                .unwrap_or_else(|| panic!("case {case:?}: expected a performed form"));
            let file = filter.file();
            assert_eq!(
                file, expected,
                "case {case:?}: expected {expected:?}, got {file:?}"
            );
        }
    }
}
