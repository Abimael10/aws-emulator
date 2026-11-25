// SPDX-License-Identifier: CC0-1.0

//! The DECLARATION a Dockerfile makes about the image built from it, resolved against the
//! build arguments the `docker build` carried.
//!
//! # The rule this module implements
//!
//! "A Dockerfile is a text document that contains all the commands a user could call on the
//! command line to assemble an image" (Dockerfile reference). Two things about an image
//! follow from that document and from the values a build was given for it:
//!
//! * the image CONFIGURATION — "`ENV` sets the environment variable … The environment
//!   variables set using `ENV` will persist … in the resulting image", `CMD` and `ENTRYPOINT`
//!   name what the container runs, `EXPOSE` "informs Docker that the container listens on
//!   the specified network ports", and `WORKDIR` and `USER` the directory and the user it
//!   runs as. Those are members of the image configuration document a registry stores beside
//!   the layers;
//! * the image's HISTORY — the image configuration's `history` array, whose entries carry
//!   `created_by`, "the command which created the layer". The instruction recorded there is
//!   the one that RAN, so it carries the values a build argument was given.
//!
//! A build argument is declared and then used: "`ARG` defines a variable that users can pass
//! at build-time to the builder with the `docker build` command using the
//! `--build-arg <varname>=<value>` flag", and "an `ARG` variable definition comes into effect
//! from the line on which it is defined". A value passed for a variable the Dockerfile does
//! not declare changes nothing about the image — Docker warns that it was unused — and a
//! declared variable the build passed no value for takes the default its own declaration
//! states. Every instruction after the declaration is then resolved with it:
//! "environment variables … are also supported for … `ADD`, `COPY`, `ENV`, `EXPOSE`, `USER`,
//! `WORKDIR`" and the rest, written `$name` or `${name}`.
//!
//! So an image's bytes depend on the build arguments exactly where the DOCKERFILE uses them:
//! a build that passes a different value for a variable the document resolves into one of its
//! instructions composes a different configuration, and therefore a different digest, while a
//! value nothing uses composes the very same image.
//!
//! # What is NOT read
//!
//! This module reads the document; it runs none of it. A `RUN`, `COPY` or `ADD` is recorded
//! in the history as the instruction that ran and performs nothing else — nothing in this
//! emulator executes a shell inside an image ([`super::container_image`]), so the layer an
//! instruction would add is not one. What the document's own `FROM` names IS read
//! ([`Declaration::base`]), because an image is built ON its base: the image the build
//! environment HOLDS under that reference is the one the build stacks its context on. Nothing
//! here opens a file or reaches the host: it computes over the text it is handed.
//!
//! # Bounds
//!
//! The document is a caller's, and resolving it MULTIPLIES — an `ENV` whose value the
//! references of the next line read stores a value as long as their product — so the length
//! of the source text bounds nothing about what it resolves to. Three bounds therefore stand
//! together: how many lines are read ([`MAX_INSTRUCTIONS`]), how long one instruction may be
//! written ([`MAX_INSTRUCTION_CHARS`]), and how much RESOLVED text a whole document may come
//! to ([`MAX_RESOLVED_CHARS`]) — the last charged against every resolution and every value
//! brought into force, with the resolution itself held to what is left, so nothing is
//! allocated past it even once. A document that spends the budget is read no further and the
//! declaration carries what was resolved before it ran out.
//!
//! Those bounds hold the TEXT a document comes to; what it costs to READ is held with them,
//! because the two multiply as well: a variable an `ENV` sets and a port an `EXPOSE` states
//! are each resolved against an index rather than against a walk of everything stated before
//! them, so one document's budget cannot be spent on work quadratic in the entries it
//! declares.

use ::std::collections::{BTreeMap, BTreeSet};

/// The instruction a Dockerfile has to open with, which [`super::container_image`] holds the
/// document to.
pub const FROM_INSTRUCTION: &str = "FROM";

/// How many lines of a Dockerfile are read.
///
/// CHOSEN, not a Docker limit: the document is a caller's, and the walk over it happens in
/// this process, so its cost is bounded here. The instructions past it are not read.
const MAX_INSTRUCTIONS: usize = 512;

/// How many characters ONE instruction may be written with, as the document writes it.
///
/// CHOSEN for the same reason: a continued line is joined in this process, so how long the
/// joined text may grow is bounded here.
const MAX_INSTRUCTION_CHARS: usize = 8 * 1024;

/// How many characters all the RESOLVED text of one document may come to: every instruction
/// as it ran, plus every value a variable in force carries.
///
/// CHOSEN, and bounded SEPARATELY from the source text above, because resolving multiplies:
/// one `ENV` whose value is read by the references of the next line stores a value as long as
/// their product, so a document far inside the two bounds above resolves to text no bound on
/// its own length covers. This is the budget the whole walk is charged against
/// ([`Budget`]), and a document that spends it is read no further — the declaration carries
/// what was resolved before it ran out, exactly as a document past [`MAX_INSTRUCTIONS`] does.
/// It is of the order of the context one image is composed from
/// ([`super::container_image::MAX_IMAGE_BYTES`]), which is what the resolved text is written
/// into.
const MAX_RESOLVED_CHARS: usize = 256 * 1024;

/// What is left of [`MAX_RESOLVED_CHARS`] while a document is being read.
///
/// Charged for every resolved instruction and every value brought into force, so the walk
/// stops at the first one the budget cannot pay for rather than after it has allocated.
#[derive(Debug)]
struct Budget {
    left: usize,
}

impl Budget {
    /// A budget of [`MAX_RESOLVED_CHARS`].
    fn new() -> Self {
        Self {
            left: MAX_RESOLVED_CHARS,
        }
    }

    /// Whether `cost` characters can be paid for, charging them when they can.
    fn charge(&mut self, cost: usize) -> bool {
        match self.left.checked_sub(cost) {
            Some(left) => {
                self.left = left;
                true
            }
            None => false,
        }
    }

    /// How many characters are left, which is the longest a resolution may come to.
    fn left(&self) -> usize {
        self.left
    }
}

/// What a Dockerfile declares about the image built from it, with every instruction resolved
/// against the build arguments the command carried.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Declaration {
    /// `ENV`: the environment of the image, as the configuration's `Env` member spells one
    /// (`KEY=value`), in the order the instructions set them.
    environment: Vec<String>,
    /// `CMD`: the command the container runs, as the configuration's `Cmd` member spells one.
    command: Option<Vec<String>>,
    /// `ENTRYPOINT`: the configuration's `Entrypoint` member.
    entry_point: Option<Vec<String>>,
    /// `EXPOSE`: the ports the configuration's `ExposedPorts` member names, each as
    /// `<port>/<protocol>`.
    exposed_ports: Vec<String>,
    /// `WORKDIR`: the configuration's `WorkingDir` member.
    working_directory: Option<String>,
    /// `USER`: the configuration's `User` member.
    user: Option<String>,
    /// The instructions that ran, resolved — the `created_by` of the configuration's
    /// `history` entries.
    history: Vec<String>,
    /// `FROM`: the reference of the image this one is built ON, resolved, with the stage name
    /// and the platform option the instruction may carry left off — `None` for a document that
    /// states none.
    base: Option<String>,
}

impl Declaration {
    /// The `config` object of the image configuration document: the members the instructions
    /// set, and nothing for a member no instruction states.
    #[must_use]
    pub fn configuration(&self) -> ::serde_json::Value {
        let mut config = ::serde_json::Map::new();
        if !self.environment.is_empty() {
            config.insert("Env".to_string(), ::serde_json::json!(self.environment));
        }
        if let Some(entry_point) = &self.entry_point {
            config.insert("Entrypoint".to_string(), ::serde_json::json!(entry_point));
        }
        if let Some(command) = &self.command {
            config.insert("Cmd".to_string(), ::serde_json::json!(command));
        }
        if !self.exposed_ports.is_empty() {
            // "ExposedPorts" is a set, which the document writes as an object whose values
            // are empty.
            let ports: ::serde_json::Map<String, ::serde_json::Value> = self
                .exposed_ports
                .iter()
                .map(|port| (port.clone(), ::serde_json::json!({})))
                .collect();
            config.insert("ExposedPorts".to_string(), ports.into());
        }
        if let Some(directory) = &self.working_directory {
            config.insert("WorkingDir".to_string(), ::serde_json::json!(directory));
        }
        if let Some(user) = &self.user {
            config.insert("User".to_string(), ::serde_json::json!(user));
        }
        config.into()
    }

    /// The reference the image this one is built ON is named by: "a Dockerfile must begin
    /// with a `FROM` instruction", and `FROM <image>[:<tag>]` "initializes a new build stage
    /// and sets the base image for subsequent instructions".
    ///
    /// The LAST stage's base, for a document declaring several: the image a build produces is
    /// the last stage's, so that is the one it is built on. A single-stage document states one
    /// either way.
    #[must_use]
    pub fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    /// The `history` array of the image configuration document: one entry per instruction
    /// that ran, carrying the instruction as it ran.
    #[must_use]
    pub fn history(&self) -> ::serde_json::Value {
        ::serde_json::json!(self
            .history
            .iter()
            .map(|created_by| ::serde_json::json!({ "created_by": created_by }))
            .collect::<Vec<::serde_json::Value>>())
    }
}

/// What `document` declares about the image, resolved against `build_arguments` — the
/// `--build-arg` values the command carried, keyed by variable name.
///
/// A value for a variable the document declares no `ARG` for is not in force (see the module
/// docs), so it changes nothing this answers.
///
/// Every resolution and every value brought into force is charged against one budget
/// ([`MAX_RESOLVED_CHARS`]), and the walk STOPS at the first instruction that budget cannot
/// pay for: resolving multiplies, so the length of the source text bounds neither.
#[must_use]
pub fn declared(document: &str, build_arguments: &BTreeMap<String, String>) -> Declaration {
    let mut declaration = Declaration::default();
    let mut budget = Budget::new();
    // The variables in force where the instruction being read sits: the build arguments whose
    // `ARG` has been declared by then, plus what `ENV` has set. `ENV` wins over an `ARG` of
    // the same name, which is the order the builder resolves them in.
    let mut in_force: BTreeMap<String, String> = BTreeMap::new();
    // WHERE each variable's entry sits in `declaration.environment`, and which ports have been
    // stated: what one instruction costs is then the keys it declares and not the keys every
    // instruction before it declared, so the resolution budget bounds the WORK a document sets
    // off as well as the text it allocates.
    let mut environment_entries: BTreeMap<String, usize> = BTreeMap::new();
    let mut exposed_ports: BTreeSet<String> = BTreeSet::new();
    for instruction in instructions(document) {
        let (name, arguments) = match instruction.split_once(char::is_whitespace) {
            Some((name, arguments)) => (name.to_string(), arguments.trim().to_string()),
            None => (instruction.clone(), String::new()),
        };
        let upper = name.to_ascii_uppercase();
        // `ARG` states which variable comes into force, so its own line is not resolved
        // against the variables: the name and the default are what it carries.
        if upper == "ARG" {
            match argument_in_force(&arguments, build_arguments) {
                Some((variable, value)) => {
                    // The value is charged twice: once for the history entry that records it,
                    // once for the value held in force.
                    if !budget.charge(
                        variable
                            .len()
                            .saturating_add(value.len().saturating_mul(2))
                            .saturating_add("ARG =".len()),
                    ) {
                        break;
                    }
                    declaration.history.push(format!("ARG {variable}={value}"));
                    in_force.insert(variable, value);
                }
                None => {
                    if !budget.charge(arguments.len().saturating_add("ARG ".len())) {
                        break;
                    }
                    declaration.history.push(format!("ARG {arguments}"));
                }
            }
            continue;
        }
        // The resolution is held to what the budget can still pay for, so the expansion
        // cannot allocate past it even once: a document that would resolve to more is read no
        // further.
        let Some(resolved) = substituted(&arguments, &in_force, budget.left()) else {
            break;
        };
        if !budget.charge(resolved.len().saturating_mul(2).saturating_add(upper.len())) {
            break;
        }
        declaration
            .history
            .push(format!("{upper} {resolved}").trim_end().to_string());
        match upper.as_str() {
            "ENV" => {
                for (key, value) in key_values(&resolved) {
                    in_force.insert(key.clone(), value.clone());
                    // One entry per variable: a later `ENV` of the same key replaces the
                    // value the image carries rather than adding a second entry. WHERE that
                    // entry sits is remembered, so a document declaring many variables costs
                    // one lookup per key rather than a walk of every entry declared so far.
                    let entry = format!("{key}={value}");
                    match environment_entries.get(&key) {
                        Some(&index) => {
                            if let Some(held) = declaration.environment.get_mut(index) {
                                *held = entry;
                            }
                        }
                        None => {
                            environment_entries.insert(key, declaration.environment.len());
                            declaration.environment.push(entry);
                        }
                    }
                }
            }
            "CMD" => declaration.command = Some(words_or_shell(&resolved)),
            "ENTRYPOINT" => declaration.entry_point = Some(words_or_shell(&resolved)),
            "EXPOSE" => {
                for port in resolved.split_whitespace() {
                    // "EXPOSE <port> [<port>/<protocol>...]", where the protocol "defaults to
                    // TCP if not specified".
                    let named = match port.contains('/') {
                        true => port.to_string(),
                        false => format!("{port}/tcp"),
                    };
                    // One entry per port, decided by the set rather than by a walk of the
                    // ports stated so far, for the reason `ENV` keeps an index.
                    if exposed_ports.insert(named.clone()) {
                        declaration.exposed_ports.push(named);
                    }
                }
            }
            FROM_INSTRUCTION => declaration.base = base_image(&resolved),
            "WORKDIR" => declaration.working_directory = Some(resolved.clone()),
            "USER" => declaration.user = Some(resolved.clone()),
            // Every other instruction is recorded as the instruction that ran and states
            // nothing about the configuration (see the module docs).
            _ => {}
        }
    }
    declaration
}

/// The image a `FROM` instruction's own arguments name: "FROM [--platform=<platform>]
/// <image> [AS <name>]", so the reference is the first word that is not one of the
/// instruction's options, and the stage name after it is not part of it.
///
/// `None` for an instruction naming no image at all, which names no base.
fn base_image(arguments: &str) -> Option<String> {
    arguments
        .split_whitespace()
        .find(|word| !word.starts_with("--"))
        .map(str::to_string)
}

/// The instructions `document` states: its lines with the comments and the blank lines
/// dropped and a line continued with a trailing `\` joined to the one after it.
fn instructions(document: &str) -> Vec<String> {
    let mut read: Vec<String> = Vec::new();
    let mut continued: Option<String> = None;
    for line in document.lines() {
        let line = line.trim();
        if continued.is_none() && (line.is_empty() || line.starts_with('#')) {
            continue;
        }
        let (held, carries_on) = match line.strip_suffix('\\') {
            Some(held) => (held.trim_end(), true),
            None => (line, false),
        };
        let joined = match continued.take() {
            Some(mut started) => {
                started.push(' ');
                started.push_str(held);
                started
            }
            None => held.to_string(),
        };
        if joined.len() > MAX_INSTRUCTION_CHARS {
            break;
        }
        if carries_on {
            continued = Some(joined);
            continue;
        }
        let joined = joined.trim().to_string();
        if !joined.is_empty() {
            read.push(joined);
        }
        if read.len() >= MAX_INSTRUCTIONS {
            break;
        }
    }
    if let Some(started) = continued {
        let started = started.trim().to_string();
        if !started.is_empty() {
            read.push(started);
        }
    }
    read
}

/// The variable one `ARG` brings into force and the value it has: the `--build-arg` the
/// command passed for it, and otherwise the default its own declaration states
/// (`ARG name=default`).
///
/// `None` for a declaration the build passed no value for that states no default either:
/// such a variable has no value, so an instruction referencing it resolves to nothing, as it
/// does in the builder.
fn argument_in_force(
    arguments: &str,
    build_arguments: &BTreeMap<String, String>,
) -> Option<(String, String)> {
    let written = arguments.split_whitespace().next()?;
    let (variable, default) = match written.split_once('=') {
        Some((variable, default)) => (variable, Some(unquoted(default).to_string())),
        None => (written, None),
    };
    let value = build_arguments.get(variable).cloned().or(default)?;
    Some((variable.to_string(), value))
}

/// `written` with every `$name` and `${name}` reference replaced by the value that variable
/// has, and by nothing for a variable that has none — which is how the builder resolves one.
///
/// One pass over the characters, so a value that itself spells a reference is not expanded
/// again; a `$` written `\$` is the character itself.
///
/// `budget` is the longest the RESULT may come to: resolving multiplies, so the length of
/// `written` bounds nothing about it. `None` for a resolution that would be longer, which is
/// the one answer that cannot allocate past the budget — the resolution stops at the
/// character that would exceed it rather than building the whole string first.
fn substituted(
    written: &str,
    in_force: &BTreeMap<String, String>,
    budget: usize,
) -> Option<String> {
    let mut resolved = String::with_capacity(written.len().min(budget));
    let mut characters = written.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(escaped) = characters.next() {
                if escaped == '$' {
                    held(&mut resolved, "$", budget)?;
                } else {
                    held(&mut resolved, "\\", budget)?;
                    held_char(&mut resolved, escaped, budget)?;
                }
                continue;
            }
            held(&mut resolved, "\\", budget)?;
            continue;
        }
        if character != '$' {
            held_char(&mut resolved, character, budget)?;
            continue;
        }
        let braced = characters.peek() == Some(&'{');
        if braced {
            characters.next();
        }
        let mut name = String::new();
        while let Some(next) = characters.peek() {
            if braced && *next == '}' {
                characters.next();
                break;
            }
            if !braced && !(next.is_ascii_alphanumeric() || *next == '_') {
                break;
            }
            if name.len() >= MAX_INSTRUCTION_CHARS {
                return None;
            }
            name.push(*next);
            characters.next();
        }
        // `${name:-default}` and the other modifiers are not read here: the name is taken up
        // to the first character that is not one a variable name carries, so a reference
        // written with a modifier resolves to the variable's own value.
        let name = name.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .next()
            .unwrap_or_default()
            .to_string();
        if name.is_empty() {
            held(&mut resolved, "$", budget)?;
            continue;
        }
        if let Some(value) = in_force.get(&name) {
            held(&mut resolved, value.as_str(), budget)?;
        }
    }
    Some(resolved)
}

/// Append `text` to `resolved` while that keeps it within `budget`, and answer `None` where
/// it would not — the one answer that allocates nothing past the budget.
///
/// Every append the resolution above makes goes through this or [`held_char`], so no branch
/// of it can grow the result past what the walk can pay for.
fn held(resolved: &mut String, text: &str, budget: usize) -> Option<()> {
    if resolved.len().saturating_add(text.len()) > budget {
        return None;
    }
    resolved.push_str(text);
    Some(())
}

/// [`held`] for one character, whose length is the bytes it is encoded with.
fn held_char(resolved: &mut String, character: char, budget: usize) -> Option<()> {
    if resolved.len().saturating_add(character.len_utf8()) > budget {
        return None;
    }
    resolved.push(character);
    Some(())
}

/// The `KEY=value` pairs an `ENV` line sets, in the order it writes them.
///
/// "ENV <key>=<value> ..." sets each pair, and the one-pair form "ENV <key> <value>" takes
/// the whole rest of the line as the value, which is what lets that form carry spaces.
fn key_values(written: &str) -> Vec<(String, String)> {
    let written = written.trim();
    if written.is_empty() {
        return Vec::new();
    }
    if !first_word_carries_assignment(written) {
        let Some((key, value)) = written.split_once(char::is_whitespace) else {
            return Vec::new();
        };
        return vec![(key.to_string(), unquoted(value.trim()).to_string())];
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    for word in split_outside_quotes(written) {
        if let Some((key, value)) = word.split_once('=') {
            if !key.is_empty() {
                pairs.push((key.to_string(), unquoted(value).to_string()));
            }
        }
    }
    pairs
}

/// Whether the first word of an `ENV` line carries an `=`, which is what tells the
/// `KEY=value` form from the `KEY value` one.
fn first_word_carries_assignment(written: &str) -> bool {
    written
        .split_whitespace()
        .next()
        .is_some_and(|word| word.contains('='))
}

/// `written` split on the whitespace OUTSIDE its quotes, so a quoted value carrying spaces
/// stays one word.
fn split_outside_quotes(written: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for character in written.chars() {
        match quote {
            Some(open) if character == open => {
                quote = None;
                word.push(character);
            }
            Some(_) => word.push(character),
            None if character == '"' || character == '\'' => {
                quote = Some(character);
                word.push(character);
            }
            None if character.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            None => word.push(character),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// `value` without the one pair of matching quotes it may be written in.
fn unquoted(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(held) = value
            .strip_prefix(quote)
            .and_then(|held| held.strip_suffix(quote))
        {
            return held;
        }
    }
    value
}

/// The argument vector a `CMD` or `ENTRYPOINT` names: the JSON array of its exec form, and
/// otherwise the shell form, which the builder runs as `/bin/sh -c "<the line>"`.
fn words_or_shell(written: &str) -> Vec<String> {
    let written = written.trim();
    if written.starts_with('[') {
        if let Ok(::serde_json::Value::Array(entries)) =
            ::serde_json::from_str::<::serde_json::Value>(written)
        {
            let words: Vec<String> = entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect();
            if words.len() == entries.len() {
                return words;
            }
        }
    }
    vec![
        SHELL_PROGRAM.to_string(),
        SHELL_COMMAND_OPTION.to_string(),
        written.to_string(),
    ]
}

/// The shell the builder runs a `CMD` or `ENTRYPOINT` written in its shell form with:
/// "`CMD <command>` … will execute in a shell … `/bin/sh -c`".
const SHELL_PROGRAM: &str = "/bin/sh";

/// See [`SHELL_PROGRAM`].
const SHELL_COMMAND_OPTION: &str = "-c";

#[cfg(test)]
mod tests {
    use super::*;

    /// The build arguments a row passes, as the command carries them.
    fn passed(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    /// RULE: a build argument is in force only where the Dockerfile DECLARES it, and the
    /// instructions after that declaration are resolved with it — so what the image declares
    /// follows from the document and the values passed for the variables it declares.
    ///
    /// The rows differ along what the rule generalises over: whether the document declares
    /// the variable, whether the build passes a value for it, whether the declaration states
    /// a default, which instruction uses it, and how the reference is written.
    #[test]
    fn a_build_argument_is_resolved_where_the_dockerfile_declares_it() {
        /// One row: the case, the document, the build arguments passed and the history the
        /// image carries.
        type Row = (&'static str, &'static str, Vec<(&'static str, &'static str)>, Vec<&'static str>);

        // (case, the document, the build arguments passed, the history the image carries)
        let rows: Vec<Row> = vec![
            (
                "a declared variable the build passes a value for",
                "FROM scratch\nARG BODY\nENV BODY=$BODY",
                vec![("BODY", "one")],
                vec!["FROM scratch", "ARG BODY=one", "ENV BODY=one"],
            ),
            (
                "the same document with another value",
                "FROM scratch\nARG BODY\nENV BODY=$BODY",
                vec![("BODY", "two")],
                vec!["FROM scratch", "ARG BODY=two", "ENV BODY=two"],
            ),
            (
                "a declaration whose default the build overrides",
                "FROM scratch\nARG BODY=fallback\nENV BODY=${BODY}",
                vec![("BODY", "passed")],
                vec!["FROM scratch", "ARG BODY=passed", "ENV BODY=passed"],
            ),
            (
                "a declaration whose default stands",
                "FROM scratch\nARG BODY=fallback\nENV BODY=${BODY}",
                vec![],
                vec!["FROM scratch", "ARG BODY=fallback", "ENV BODY=fallback"],
            ),
            (
                "a value for a variable the document declares no ARG for",
                "FROM scratch\nENV BODY=$BODY",
                vec![("BODY", "unused")],
                vec!["FROM scratch", "ENV BODY="],
            ),
            (
                "a reference used by a RUN, which is recorded as it ran",
                "FROM scratch\nARG BODY\nRUN echo \"$BODY\" > /index.html",
                vec![("BODY", "served bytes")],
                vec![
                    "FROM scratch",
                    "ARG BODY=served bytes",
                    "RUN echo \"served bytes\" > /index.html",
                ],
            ),
            (
                "a declaration with neither a value nor a default",
                "FROM scratch\nARG BODY\nENV BODY=$BODY",
                vec![],
                vec!["FROM scratch", "ARG BODY", "ENV BODY="],
            ),
            (
                "comments, blank lines and a continued line",
                "# a build\n\nFROM scratch\nARG BODY=x\nENV A=1 \\\n    B=$BODY",
                vec![],
                vec!["FROM scratch", "ARG BODY=x", "ENV A=1 B=x"],
            ),
        ];
        for (case, document, arguments, expected) in rows {
            let declaration = declared(document, &passed(&arguments));
            assert_eq!(
                declaration.history, expected,
                "case {case:?}: expected the history {expected:?}, got {:?}",
                declaration.history
            );
        }
    }

    /// RULE: the image configuration carries exactly what the instructions set.
    #[test]
    fn the_configuration_carries_what_the_instructions_set() {
        let declaration = declared(
            "FROM scratch\nARG PORT=80\nENV BODY=\"a b\" LANG=C\nENV BODY=later\n\
             EXPOSE $PORT 443/udp\nWORKDIR /srv\nUSER app\nCMD [\"httpd\", \"-f\"]",
            &BTreeMap::new(),
        );
        let configuration = declaration.configuration();
        let expected = ::serde_json::json!({
            "Env": ["BODY=later", "LANG=C"],
            "Cmd": ["httpd", "-f"],
            "ExposedPorts": {"80/tcp": {}, "443/udp": {}},
            "WorkingDir": "/srv",
            "User": "app",
        });
        assert_eq!(
            configuration, expected,
            "expected the configuration {expected}, got {configuration}"
        );
    }

    /// RULE: the RESOLVED text of a document is bounded, whatever the document's own length —
    /// a document whose references multiply is read only as far as the budget pays for, and
    /// nothing past it is allocated.
    ///
    /// The rows differ along how the growth is written: a value doubled by a line of its own
    /// references, a single line carrying thousands of them, and the value of a long build
    /// argument repeated — the three shapes whose output is a PRODUCT rather than a function of
    /// the source length. Each is far inside [`MAX_INSTRUCTIONS`] and
    /// [`MAX_INSTRUCTION_CHARS`], which is why those two cannot be what bounds it.
    #[test]
    fn the_resolved_text_of_a_document_is_bounded() {
        // (case, the document, the build arguments passed)
        let long_value = "x".repeat(1_000);
        /// One row of the table below: the case, the document and the arguments passed.
        type GrowthRow<'a> = (&'static str, String, Vec<(&'static str, &'a str)>);
        let rows: Vec<GrowthRow<'_>> = vec![
            (
                "a value doubled by forty lines of its own references",
                format!(
                    "FROM scratch\nARG A=start\n{}",
                    "ENV A=$A$A\n".repeat(40)
                ),
                vec![],
            ),
            (
                "one line carrying two thousand references to a long value",
                format!(
                    "FROM scratch\nARG A\nENV B={}\nENV C={}",
                    "$A".repeat(2_000),
                    "$B".repeat(2_000)
                ),
                vec![("A", long_value.as_str())],
            ),
            (
                "a braced reference repeated across three lines",
                format!(
                    "FROM scratch\nARG A=seed\nENV A=${{A}}{}\nENV A=${{A}}{}\nENV A=${{A}}{}",
                    "${A}".repeat(500),
                    "${A}".repeat(500),
                    "${A}".repeat(500)
                ),
                vec![],
            ),
        ];
        for (case, document, arguments) in rows {
            let declaration = declared(&document, &passed(&arguments));
            let resolved: usize = declaration
                .history
                .iter()
                .chain(declaration.environment.iter())
                .map(String::len)
                .sum();
            assert!(
                resolved <= MAX_RESOLVED_CHARS,
                "case {case:?}: expected the resolved text to stay within \
                 {MAX_RESOLVED_CHARS} characters, got {resolved}"
            );
            // The walk STOPS rather than carrying a half-resolved instruction: the first
            // instruction is still read, so the declaration is the document's own prefix.
            assert_eq!(
                declaration.history.first().map(String::as_str),
                Some("FROM scratch"),
                "case {case:?}: expected the instructions read before the budget ran out, got \
                 {:?}",
                declaration.history
            );
        }
    }

    /// RULE: a `CMD` or `ENTRYPOINT` written in its shell form runs through the shell, and one
    /// written as a JSON array is the argument vector itself.
    #[test]
    fn a_command_is_read_in_the_form_it_is_written() {
        // (case, the document, the Cmd the configuration carries)
        let rows: Vec<(&str, &str, Vec<&str>)> = vec![
            (
                "the exec form",
                "FROM scratch\nCMD [\"httpd\", \"-f\", \"-p\", \"80\"]",
                vec!["httpd", "-f", "-p", "80"],
            ),
            (
                "the shell form",
                "FROM scratch\nCMD httpd -f -p 80",
                vec!["/bin/sh", "-c", "httpd -f -p 80"],
            ),
            (
                "an array that is not one of strings",
                "FROM scratch\nCMD [1, 2]",
                vec!["/bin/sh", "-c", "[1, 2]"],
            ),
        ];
        for (case, document, expected) in rows {
            let declaration = declared(document, &BTreeMap::new());
            let command = declaration.command.clone().unwrap_or_default();
            assert_eq!(
                command, expected,
                "case {case:?}: expected {expected:?}, got {command:?}"
            );
        }
    }
}
