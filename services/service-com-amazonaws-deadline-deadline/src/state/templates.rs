// SPDX-License-Identifier: CC0-1.0

//! Reading a value out of a template the CALLER supplied.
//!
//! Two of this service's resources are described by a template the request carries
//! verbatim, and each has a `name` a read has to answer with that no create takes as a
//! member: a job's (`GetJob`'s `name`) and a queue environment's (`GetQueueEnvironment`'s
//! `name`, `@required`). So the name is READ OUT of the document the request carried, in
//! the language its own `templateType` names — `JSON` or `YAML`, the only two values
//! either enum spells (`smithy_query(query="shape", shape="EnvironmentTemplateType")` and
//! `smithy_query(query="shape", shape="JobTemplateType")`).
//!
//! A job template also describes the WORK the job is made of, which the scheduler needs as
//! steps and tasks: [`template_document`] hands the whole document to
//! [`super::job_template_work`], which reads it there.
//!
//! Both readers are real parsers of their language rather than a hand-written subset.
//!
//! The JSON one is bounded by what the request already bounds (`EnvironmentTemplate, len
//! 1..15000`; `JobTemplate, len 1..1000000`, each refused at the boundary before a handler
//! runs): `serde_json` builds a value linear in the input and enforces its own recursion
//! limit, returning an error rather than growing the call stack.
//!
//! The YAML one needs bounds of its OWN, because neither of those properties holds for it:
//! a tree-building YAML loader resolves an ALIAS by COPYING the anchored node, so what a
//! document builds is NOT linear in its length. Nesting and expansion are different
//! quantities, and a document that nests shallowly can still expand enormously. So a YAML
//! template is handed to the loader only once two bounds taken BEFORE it have passed: the
//! TEXT nests no deeper than [`MAX_TEMPLATE_NESTING`] (the shared pre-parse scan, which
//! parses nothing), and the parser's own EVENT STREAM — which copies nothing and builds no
//! tree — reports no alias and no more than [`MAX_TEMPLATE_DEPTH`] collections open at
//! once. Past either, the document is refused with the same modeled 400 an unparseable one
//! earns, and no node is ever built.
//!
//! Call either reader BEFORE taking the store lock: parsing is the one step whose cost a
//! caller controls, so it must not run while every other request of this service waits on
//! that lock.

/// The deepest a caller-supplied YAML template may nest, counted on the TEXT before any
/// parser sees it.
///
/// Far past what an Open Job Description job or environment template nests, and the scan
/// that counts it is an upper bound, so a document this refuses is not one a caller writes.
pub const MAX_TEMPLATE_NESTING: usize = 256;

/// The most collections a YAML template may hold open at once, counted off the parser's own
/// event stream rather than off a loaded tree.
pub const MAX_TEMPLATE_DEPTH: usize = 32;

/// The `JSON` member of the two template-type enums.
pub const TEMPLATE_TYPE_JSON: &str = "JSON";

/// The `YAML` member of the two template-type enums.
pub const TEMPLATE_TYPE_YAML: &str = "YAML";

/// The whole template as one document tree, read with the parser `template_type` names.
///
/// Every reader in this crate goes through here, so a template is parsed by exactly one
/// parser per language and every later walk is over the same tree shape — a
/// [`::serde_json::Value`], which is what the JSON parser already builds and what a loaded
/// YAML document is converted to.
///
/// `Ok(None)` is a `template_type` that names neither language (so there is no document to
/// read); `Err` is a document that is not the language it said it was, and the caller
/// answers it as the modeled 400 for an unparseable payload.
pub fn template_document(
    template: &str,
    template_type: &str,
) -> Result<Option<::serde_json::Value>, String> {
    match template_type {
        TEMPLATE_TYPE_JSON => ::serde_json::from_str(template)
            .map(Some)
            .map_err(|error| format!("The template isn't valid JSON: {error}")),
        TEMPLATE_TYPE_YAML => yaml_document(template),
        // Neither enum spells a third language, so a validated request reaches this arm
        // only where the member is absent entirely: there is no document to read, which
        // is not the same as a document that failed to parse.
        _ => Ok(None),
    }
}

/// The string a template declares at `path` — each element one mapping key, nested left to
/// right — read with the parser `template_type` names.
///
/// `Ok(None)` is a template that parses but declares nothing there (or declares something
/// that is not a string); `Err` is a document that is not the language it said it was, and
/// the caller answers it as the modeled 400 for an unparseable payload.
pub fn template_string(
    template: &str,
    template_type: &str,
    path: &[&str],
) -> Result<Option<String>, String> {
    let Some(document) = template_document(template, template_type)? else {
        return Ok(None);
    };
    let mut node = &document;
    for key in path {
        let Some(child) = node.get(*key) else {
            return Ok(None);
        };
        node = child;
    }
    Ok(node.as_str().map(str::to_string))
}

/// The name a JOB template declares.
///
/// An Open Job Description job template carries its `name` at the top level, beside
/// `specificationVersion` and `steps`.
pub fn job_template_name(
    template: &str,
    template_type: &str,
) -> Result<Option<String>, String> {
    template_string(template, template_type, &["name"])
}

/// The name an ENVIRONMENT template declares.
///
/// An Open Job Description environment template (`specificationVersion:
/// environment-2023-09`) describes ONE environment, under the `environment` key, and that
/// environment is what carries the `name` — unlike a job template, whose name is at the
/// top level. That name is what `GetQueueEnvironment` reports as the queue environment's
/// `@required` `name`, which `CreateQueueEnvironment` takes no member for.
pub fn environment_template_name(
    template: &str,
    template_type: &str,
) -> Result<Option<String>, String> {
    template_string(template, template_type, &["environment", "name"])
}

/// [`template_document`] for a YAML document: the FIRST document of the stream, which is
/// the whole of a template that carries one.
///
/// The bounds this module's header states are taken FIRST, so the tree-building loader is
/// reached only by a document whose expansion is already known to be bounded.
fn yaml_document(template: &str) -> Result<Option<::serde_json::Value>, String> {
    yaml_within_bounds(template)?;
    let documents = ::yaml_rust2::YamlLoader::load_from_str(template)
        .map_err(|error| format!("The template isn't valid YAML: {error}"))?;
    Ok(documents.first().map(yaml_as_json))
}

/// A loaded YAML node as the document tree every reader here walks.
///
/// The two languages a template may be written in describe the same documents, so they are
/// read through one tree rather than one walk per language. The recursion is bounded before
/// this runs: [`yaml_within_bounds`] has already refused a document that holds more than
/// [`MAX_TEMPLATE_DEPTH`] collections open at once, so the depth here is not
/// caller-controlled. A node of a kind no template declares a value with (a tag-only node,
/// an unresolved alias) becomes `null`, which every reader treats as "nothing declared".
fn yaml_as_json(node: &::yaml_rust2::Yaml) -> ::serde_json::Value {
    use ::serde_json::Value;
    if let Some(text) = node.as_str() {
        return Value::String(text.to_string());
    }
    if let Some(value) = node.as_bool() {
        return Value::Bool(value);
    }
    if let Some(value) = node.as_i64() {
        return Value::Number(value.into());
    }
    if let Some(value) = node.as_f64() {
        return ::serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number);
    }
    if let Some(items) = node.as_vec() {
        return Value::Array(items.iter().map(yaml_as_json).collect());
    }
    if let Some(entries) = node.as_hash() {
        return Value::Object(
            entries
                .iter()
                .filter_map(|(key, value)| {
                    key.as_str().map(|key| (key.to_string(), yaml_as_json(value)))
                })
                .collect(),
        );
    }
    Value::Null
}

/// Whether a YAML template is flat enough, and free enough of expansion, to build a tree
/// from at all.
///
/// Two readings of the text, neither of which builds a node: the shared pre-parse nesting
/// scan every service that reads a caller-supplied YAML document this way uses
/// (`service_common::yaml_nesting`), and a pass over the parser's own event stream, which
/// reports the collections a document opens and every ALIAS it uses. An alias is refused
/// outright rather than counted: the loader resolves one by copying the anchored node, so a
/// chain of them expands multiplicatively, and neither an Open Job Description job template
/// nor an environment template needs one.
fn yaml_within_bounds(template: &str) -> Result<(), String> {
    let too_deep = || {
        format!(
            "The template nests deeper than the {MAX_TEMPLATE_NESTING} levels this emulator \
             reads a template within."
        )
    };
    if ::service_common::yaml_nesting::nesting_depth_bound(template, MAX_TEMPLATE_NESTING)
        > MAX_TEMPLATE_NESTING
    {
        return Err(too_deep());
    }

    /// What the event stream reports about a document: whether it aliases, and how many
    /// collections it holds open at once.
    #[derive(Default)]
    struct Guard {
        aliases: bool,
        open: usize,
        deepest: usize,
    }
    impl ::yaml_rust2::parser::EventReceiver for Guard {
        fn on_event(&mut self, event: ::yaml_rust2::parser::Event) {
            use ::yaml_rust2::parser::Event;
            match event {
                Event::Alias(_) => self.aliases = true,
                Event::SequenceStart(..) | Event::MappingStart(..) => {
                    self.open = self.open.saturating_add(1);
                    self.deepest = self.deepest.max(self.open);
                }
                Event::SequenceEnd | Event::MappingEnd => {
                    self.open = self.open.saturating_sub(1);
                }
                _ => {}
            }
        }
    }

    let mut guard = Guard::default();
    ::yaml_rust2::parser::Parser::new_from_str(template)
        .load(&mut guard, true)
        .map_err(|error| format!("The template isn't valid YAML: {error}"))?;
    if guard.aliases {
        return Err(
            "The template uses a YAML alias, which this emulator doesn't expand.".to_string(),
        );
    }
    if guard.deepest > MAX_TEMPLATE_DEPTH {
        return Err(too_deep());
    }
    Ok(())
}
