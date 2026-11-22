// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the JSON Schema document a gateway model carries, and the document a
//! gateway control plane generates from it.
//!
//! A model describes the data structure of a method request or response, and a gateway can
//! generate, from that structure, the Velocity (VTL) template content a caller starts a
//! mapping template from. Both control planes read the same schema language (JSON Schema
//! draft-04, "the schema for the model"), so the reading — what JSON type a node declares,
//! what a `$ref` resolves to, and the bounds a generation runs within — is stated ONCE
//! here instead of once per service.
//!
//! [`generate_sample_payload`] is what each of them generates from that reading: the
//! preamble followed by a SAMPLE PAYLOAD in the structure the schema declares, in which
//! every declared property carries an example value of the type declared FOR THAT PROPERTY,
//! so the generated document illustrates each property's declared type. That is what the
//! recorded calls of both control planes answer — a template whose example for an `integer`
//! property is an integer — so the one rendering is stated here rather than once per
//! service.
//!
//! A generated document is a TEMPLATE and not itself a payload, so where the schema declares
//! a value whose members a template cannot write out one by one — an ARRAY, whose element
//! count is the incoming payload's and not the schema's — what is generated is the Velocity
//! SKELETON that maps it: the loop over the array the payload carries, the example of one
//! element inside it, and the comma the loop writes between elements. Everything a schema
//! does state the members of is written out directly.
//!
//! It is a PURE function of the document — the same schema generates the same text, and
//! nothing is read or written beside it — and it is bounded in descent ([`MAX_DEPTH`]),
//! in STEPS ([`MAX_STEPS`]) and in output ([`MAX_TEMPLATE_BYTES`]). A step is every unit of
//! work a generation does on the document: each value rendered, each property name
//! considered and each segment of a `$ref` walked, so what one call costs is bounded by the
//! budget rather than by the size of the stored schema. A document that does not fit the
//! step or output bound is reported as [`Exhausted`] rather than expanded, so one call
//! costs a fixed amount of work and returns a fixed amount of output however the schema
//! branches or refers back into itself.

use ::std::collections::BTreeMap;

/// The line every generated template opens with.
///
/// It binds the Velocity reference `$inputRoot` to the root of the incoming payload
/// (`$input.path('$')`), so a mapped member is addressed relative to it. This is the
/// preamble API Gateway's own generated templates carry (see the generated templates in the
/// API Gateway "Models and mapping templates" documentation).
const INPUT_ROOT: &str = "$inputRoot";

/// One indentation step of a generated document's nested lines.
const INDENT: &str = "  ";

/// The loop variable a generated `#foreach` binds each element of the mapped array to.
const FOREACH_ELEMENT: &str = "$elem";

/// What a generated loop iterates: a member of the incoming payload the CALLER has to name,
/// because the schema declares the elements of an array and not where in the payload that
/// array sits. It is left as a placeholder member of [`INPUT_ROOT`], which is what
/// [`FOREACH_TODO`] tells the caller to replace.
const FOREACH_SOURCE_MEMBER: &str = "TODO";

/// The comment a generated loop is preceded by: the instruction to point the loop at the
/// array the incoming payload carries. A Velocity line comment (`##`), so it is none of the
/// body the template writes, and the marker a caller greps for to find the one spot a
/// generated template has to be edited at.
const FOREACH_TODO: &str = "##TODO: Update this foreach loop to reference array from input json";

/// The separator a generated loop writes BETWEEN elements and not after the last one: the
/// array it maps is JSON, whose elements are comma-separated, and how many of them there are
/// is the incoming payload's to say.
const FOREACH_SEPARATOR: &str = "#if($foreach.hasNext),#end";

/// The directive that closes a Velocity block.
const BLOCK_END: &str = "#end";

/// The example value a declared `string` carries in a generated sample payload: the
/// placeholder string the gateway fills a string leaf with, which a caller replaces with a
/// value of their own. A recorded generation shows it as `foo` — the sample is an EXAMPLE
/// payload, so a string leaf carries an example string rather than the name of its type.
const SAMPLE_STRING: &str = "foo";

/// The example value a declared `integer` carries — an integer literal, so the generated
/// document carries the JSON type the schema declares rather than a near one.
const SAMPLE_INTEGER: i64 = 0;

/// The example value a declared `number` carries: a fractional literal, for the reason
/// [`SAMPLE_INTEGER`] is an integral one.
const SAMPLE_NUMBER: f64 = 0.0;

/// The example value a declared `boolean` carries.
const SAMPLE_BOOLEAN: bool = true;

/// How deep into a schema a generation descends before it stops and renders the value as a
/// whole.
///
/// A JSON Schema may point at itself through `$ref` (a tree node whose children are the
/// same schema), so the descent needs a bound to terminate at all; past the bound the value
/// is rendered as one of unknown shape. Every `$ref` hop costs a level too, so a cycle
/// cannot be re-entered free.
pub const MAX_DEPTH: usize = 12;

/// How many STEPS one generation may take.
///
/// A step is every unit of work a generation does on the document: a value rendered, a
/// property name considered while a member list is built, a segment of a `$ref` walked. The
/// budget is therefore total rather than per value — the alternative, charging only the
/// values rendered, leaves the cost of each one bounded by the size of the stored schema,
/// which is the caller's to choose, and a schema that re-renders one wide object pays that
/// cost once per render.
///
/// It is the bound the depth bound alone does not give: a schema branches (an object renders
/// one value per declared property), so the work grows as the product of the branching
/// factors, not with the depth. An EMULATOR bound, not a figure AWS publishes, and what it
/// buys is that one request costs a fixed amount of work whatever the stored schema does. It
/// is far above any schema a gateway model may carry: the published schema maxima are tens
/// to hundreds of kilobytes of text, which cannot declare more than a few thousand
/// properties, so only a schema that re-expands itself through `$ref` reaches this bound,
/// and such a schema is refused rather than expanded.
pub const MAX_STEPS: usize = 20_000;

/// The most bytes a generated document may carry.
///
/// A bound on the generator's OUTPUT, so a small request cannot be answered with an
/// arbitrarily large response. Also an emulator bound, and generous against the schema
/// maxima the gateway models publish: the generated document describes what the schema
/// declares, so this size covers every schema that is not re-expanding itself.
pub const MAX_TEMPLATE_BYTES: usize = 64 * 1024;

/// Which bound a generation hit, for the refusal that names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    /// More steps than [`MAX_STEPS`].
    Steps,
    /// More output than [`MAX_TEMPLATE_BYTES`].
    Size,
}

impl Exhausted {
    /// The bound, as the refusal states it to the caller.
    pub fn bound(self) -> String {
        match self {
            Self::Steps => format!("{MAX_STEPS} steps"),
            Self::Size => format!("{MAX_TEMPLATE_BYTES} bytes of template"),
        }
    }
}

/// The document under construction, and the budget it is generated within.
///
/// Every byte goes through [`Writer::push`] and every unit of work through
/// [`Writer::step`], so the two bounds hold by construction: a generation cannot take more
/// than [`MAX_STEPS`] steps or build more than [`MAX_TEMPLATE_BYTES`] bytes, and it stops at
/// the first of them it reaches.
#[derive(Debug)]
struct Writer {
    template: String,
    /// How many more steps may be taken.
    steps: usize,
}

impl Writer {
    fn new() -> Self {
        Self {
            template: String::new(),
            steps: MAX_STEPS,
        }
    }

    /// Charge one unit of work — a rendered value, a property name considered, a reference
    /// segment walked — against the step budget.
    fn step(&mut self) -> Result<(), Exhausted> {
        self.steps = self.steps.checked_sub(1).ok_or(Exhausted::Steps)?;
        Ok(())
    }

    /// Append `text`, or report the output bound if it would not fit.
    fn push(&mut self, text: &str) -> Result<(), Exhausted> {
        if self.template.len() + text.len() > MAX_TEMPLATE_BYTES {
            return Err(Exhausted::Size);
        }
        self.template.push_str(text);
        Ok(())
    }

    /// Open a generated document with the preamble that binds `$inputRoot`.
    fn preamble(&mut self) -> Result<(), Exhausted> {
        self.push(&format!("#set({INPUT_ROOT} = $input.path('$'))\n"))
    }
}

/// Generate the SAMPLE PAYLOAD of a model whose schema is `document`: the preamble, then an
/// example of the payload the schema declares.
///
/// Every declared property carries a value of the JSON type the schema declares for it — a
/// `string` an example string, an `integer` an integer, a `number` a fractional number, a
/// `boolean` a boolean, an `object` an object of its own declared properties — so the
/// generated document is an example of the structure a caller is mapping a payload onto. An
/// `array`, whose element COUNT the schema does not state, is generated as the Velocity loop
/// that maps it: the `#foreach` over the array the incoming payload carries, one example
/// element inside it, and the comma the loop writes between elements (see
/// [`render_sample_array`]). A schema COMPOSED of subschemas (`allOf`, `anyOf`, `oneOf`)
/// is read through them, so the example describes what the instance must satisfy and not
/// only the keywords the outer schema carries itself. Where the schema itself states a
/// value (an `enum`'s first entry, a `default`), THAT value is the example, because the
/// schema has already named the one it means. A value the schema declares nothing about is
/// rendered `null`.
///
/// Bounded in depth ([`MAX_DEPTH`]), in steps ([`MAX_STEPS`]) and in output
/// ([`MAX_TEMPLATE_BYTES`]): a schema that does not fit the step or output bound is
/// reported as [`Exhausted`] rather than expanded, so this is a fixed amount of work and a
/// fixed amount of output per call.
pub fn generate_sample_payload(document: &::serde_json::Value) -> Result<String, Exhausted> {
    let mut writer = Writer::new();
    writer.preamble()?;
    render_sample(document, document, 0, 0, &mut writer)?;
    Ok(writer.template)
}

/// The JSON Schema types a mapping template is generated for: the two COMPOSITE types,
/// whose members are what a template maps.
const TEMPLATED_DATA_TYPES: [&str; 2] = ["object", "array"];

/// How a gateway words the refusal of a model whose declared data type no mapping template
/// maps: the fixed text and the data type the model's own schema declares.
pub fn unsupported_template_data_type_message(data_type: &str) -> String {
    format!("Unsupported data type for mapping template: {data_type}")
}

/// The message a model whose schema declares a data type no mapping template maps is
/// refused with, or `None` when a template can be generated for it.
///
/// A template MAPS a payload onto the structure the schema declares, so every value the
/// generation has to DESCEND INTO has to declare a structure: an `object` is mapped property
/// by property and an `array` item by item, while a value declaring a SCALAR type (`string`,
/// `integer`, `number`, `boolean`, `null`) has no members at all. Two kinds of value are
/// descended into: the document's ROOT, which is the payload being mapped, and a value
/// STATED BY A REFERENCE — draft-04 section 7 makes the referenced schema what the instance
/// must satisfy, and a gateway's references name the structures a model is composed of. A
/// scalar reached at either of those is reported as a data type no mapping template is
/// generated for, naming that type. A scalar declared INLINE is a leaf the generation
/// renders in place, which is the ordinary case and no refusal.
///
/// A value that declares NO type names no data type to refuse — the generation above reads
/// such a value's structure from its `properties` or `items`, and describes a value of
/// unknown shape as one it knows nothing about — and a reference that resolves to nothing is
/// such a value. A `type` written as a union of names is read as that generation reads it,
/// through the first name that is not `null`.
///
/// Bounded exactly as a generation is: the descent is bounded by [`MAX_DEPTH`], the work by
/// [`MAX_STEPS`] and the references resolved OUTSIDE the document by
/// [`MAX_RESOLVED_REFERENCES`], and a document that does not fit those bounds is reported as
/// generating no refusal — the generation that follows is what refuses it, on the bound it
/// hit.
///
/// Stated HERE, beside the generations themselves, because the question belongs to the
/// schema language both gateway control planes read rather than to either one's operation.
pub fn unsupported_template_data_type(document: &::serde_json::Value) -> Option<String> {
    unsupported_template_data_type_resolving(document, &|_| None)
}

/// How many references one reading resolves through the CALLER's own resolution — a
/// reference to a schema that is no fragment of the document being read.
///
/// Each such resolution brings a whole further document into the reading, so the count is
/// bounded as well as the work done inside it: a reference past the bound is read as one that
/// resolves to nothing. The same figure a flattened read inlines references within, so a
/// reading covers the references a generation would have inlined.
pub const MAX_RESOLVED_REFERENCES: usize = 16;

/// [`unsupported_template_data_type`], with the references the DOCUMENT cannot resolve
/// resolved by `resolve`.
///
/// A gateway model's schema may reference another model of the same api by that model's own
/// URL, which is no fragment of this document: `resolve` is handed the reference exactly as
/// the schema spells it and ANSWERS A DOCUMENT IT ALREADY HOLDS, so one reading resolves a
/// reference by a lookup rather than by reading a stored schema again — however many times
/// the document names it. A caller that has no other schemas to resolve against passes
/// [`unsupported_template_data_type`].
pub fn unsupported_template_data_type_resolving<'doc>(
    document: &'doc ::serde_json::Value,
    resolve: &dyn Fn(&str) -> Option<&'doc ::serde_json::Value>,
) -> Option<String> {
    let mut writer = Writer::new();
    let mut resolutions = MAX_RESOLVED_REFERENCES;
    unsupported_data_type(
        document,
        document,
        0,
        true,
        resolve,
        &mut resolutions,
        &mut writer,
    )
    .ok()
    .flatten()
}

/// The first data type under `schema` that no mapping template maps, as the refusal states
/// it.
///
/// `mapped` is whether the value at this node is one the generation DESCENDS INTO — the
/// document's root, or a value stated by a reference — rather than a leaf it renders in
/// place. `depth` is the descent [`MAX_DEPTH`] bounds, counted as a generation counts it, and
/// every node charges one step, so this reading costs no more work than the generation it
/// precedes. `resolutions` is how many references it may still resolve outside the document
/// ([`MAX_RESOLVED_REFERENCES`]), spent across the whole reading, so the documents one reading
/// draws in are bounded by a constant rather than by how many times the schema names them.
fn unsupported_data_type<'doc>(
    root: &'doc ::serde_json::Value,
    schema: &'doc ::serde_json::Value,
    depth: usize,
    mapped: bool,
    resolve: &dyn Fn(&str) -> Option<&'doc ::serde_json::Value>,
    resolutions: &mut usize,
    writer: &mut Writer,
) -> Result<Option<String>, Exhausted> {
    writer.step()?;
    if depth >= MAX_DEPTH {
        // Past the descent bound the generation renders the value without reading further,
        // so there is nothing more to report on either.
        return Ok(None);
    }
    // A REFERENCE states the value: what the instance must satisfy is the schema it names,
    // so the reading continues there — in this document when the reference addresses it, and
    // through the caller's own resolution otherwise, while that budget lasts.
    if let Some(reference) = schema.get("$ref").and_then(::serde_json::Value::as_str) {
        if let Some(target) = referenced(root, schema, writer)? {
            return unsupported_data_type(
                root,
                target,
                depth + 1,
                true,
                resolve,
                resolutions,
                writer,
            );
        }
        if *resolutions == 0 {
            return Ok(None);
        }
        return match resolve(reference) {
            // The referenced schema is a document of its own, so its own fragments resolve
            // against it.
            Some(target) => {
                *resolutions -= 1;
                unsupported_data_type(
                    target,
                    target,
                    depth + 1,
                    true,
                    resolve,
                    resolutions,
                    writer,
                )
            }
            // A reference that resolves to nothing names no data type.
            None => Ok(None),
        };
    }
    // What the instance must satisfy is the schema together with the subschemas it is
    // composed of, which is how the generations read a declaration too.
    let sources = composed(root, schema, writer)?;
    if mapped {
        if let Some(declared) = sources
            .iter()
            .find_map(|source| declared_data_type(source))
            .filter(|declared| !TEMPLATED_DATA_TYPES.contains(declared))
        {
            return Ok(Some(unsupported_template_data_type_message(declared)));
        }
    }
    // Then DOWN the structure, exactly where the generation descends: an object through the
    // properties it declares, an array through the schema of its items.
    match sources
        .iter()
        .map(|source| kind(source))
        .find(|kind| *kind != Kind::Unknown)
        .unwrap_or(Kind::Unknown)
    {
        Kind::Object => {
            for (_, property) in declared_properties_of(&sources, writer)? {
                if let Some(message) = unsupported_data_type(
                    root,
                    property,
                    depth + 1,
                    false,
                    resolve,
                    resolutions,
                    writer,
                )? {
                    return Ok(Some(message));
                }
            }
            Ok(None)
        }
        Kind::Array => match sources.iter().find_map(|source| declared_items(source)) {
            Some(items) => unsupported_data_type(
                root,
                items,
                depth + 1,
                false,
                resolve,
                resolutions,
                writer,
            ),
            None => Ok(None),
        },
        _ => Ok(None),
    }
}

/// The data type `schema` declares for its value, as the NAME the document spells: the
/// `type` keyword, or the first name of a union that is not `null` (JSON Schema draft-04
/// §5.5.2 admits both forms).
///
/// The one reading of that keyword in this module, so what [`kind`] generates from and what
/// [`unsupported_template_data_type`] reports cannot disagree.
fn declared_data_type(schema: &::serde_json::Value) -> Option<&str> {
    match schema.get("type") {
        Some(::serde_json::Value::String(name)) => Some(name.as_str()),
        Some(::serde_json::Value::Array(names)) => names
            .iter()
            .filter_map(::serde_json::Value::as_str)
            .find(|name| *name != "null"),
        _ => None,
    }
}

/// The JSON type a schema node declares for its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Object,
    Array,
    Str,
    /// An `integer`: a whole number.
    Integer,
    /// A `number`, which a fractional value satisfies and an `integer` does not.
    Number,
    Boolean,
    Null,
    /// The schema declares nothing this generator can key on.
    Unknown,
}

/// Render an example of the value `schema` declares into `writer`.
///
/// `root` is the whole schema document, so a `$ref` can be resolved against it; `indent` is
/// how deep the rendered lines sit and `depth` the descent bounded by [`MAX_DEPTH`].
/// Following a `$ref` costs a level of that descent, so a schema that refers back into
/// itself is not re-expanded for free.
fn render_sample(
    root: &::serde_json::Value,
    schema: &::serde_json::Value,
    indent: usize,
    depth: usize,
    writer: &mut Writer,
) -> Result<(), Exhausted> {
    writer.step()?;
    if depth >= MAX_DEPTH {
        // Past the descent bound nothing more is known about the value, which is what
        // `null` renders.
        return writer.push("null");
    }
    if let Some(target) = referenced(root, schema, writer)? {
        return render_sample(root, target, indent, depth + 1, writer);
    }
    // What the instance must satisfy is the schema together with the subschemas it is
    // COMPOSED of, so the example describes all of them rather than only the keywords the
    // outer schema happens to carry itself.
    let declared = composed(root, schema, writer)?;
    // A value the schema itself names — the entry an `enum` restricts it to, or the
    // `default` it supplies (JSON Schema draft-04 sections 5.5.1 and 6.2) — is the example,
    // because the schema has already stated the value it means.
    if let Some(stated) = declared.iter().find_map(|source| stated_value(source)) {
        return writer.push(&compact(stated));
    }
    match declared
        .iter()
        .map(|source| kind(source))
        .find(|kind| *kind != Kind::Unknown)
        .unwrap_or(Kind::Unknown)
    {
        Kind::Object => render_sample_object(root, &declared, indent, depth, writer),
        Kind::Array => render_sample_array(root, &declared, indent, depth, writer),
        Kind::Str => writer.push(&quoted(SAMPLE_STRING)),
        Kind::Integer => writer.push(&SAMPLE_INTEGER.to_string()),
        Kind::Number => writer.push(&compact(&::serde_json::json!(SAMPLE_NUMBER))),
        Kind::Boolean => writer.push(&SAMPLE_BOOLEAN.to_string()),
        Kind::Null | Kind::Unknown => writer.push("null"),
    }
}

/// `schema` and the subschemas an instance of it must ALSO satisfy, as the sources the
/// declaration is read from — the schema itself first, so what it declares itself wins.
///
/// draft-04 section 5.5.3: an instance satisfying `allOf` satisfies EVERY subschema, so all
/// of them describe it and their declarations are read together. An instance satisfying
/// `anyOf` or `oneOf` satisfies at least one branch (sections 5.5.4, 5.5.5), so the first
/// branch is the one the example describes. A subschema that is itself a reference is
/// resolved one hop, which is how a composition of named definitions reads.
///
/// Borrows throughout: the sources live in the document, so reading a composed schema
/// copies nothing. Every source charges one step, which is what bounds a composition —
/// however many subschemas it names, reading them spends the one budget, so a composition
/// too large to read is REFUSED on that bound rather than read in part and answered as if
/// it had been read whole.
fn composed<'doc>(
    root: &'doc ::serde_json::Value,
    schema: &'doc ::serde_json::Value,
    writer: &mut Writer,
) -> Result<Vec<&'doc ::serde_json::Value>, Exhausted> {
    let mut sources = vec![schema];
    let Some(object) = schema.as_object() else {
        return Ok(sources);
    };
    let every = object
        .get("allOf")
        .and_then(::serde_json::Value::as_array)
        .map(::std::vec::Vec::as_slice)
        .unwrap_or_default()
        .iter();
    let first_branch = ["anyOf", "oneOf"].into_iter().filter_map(|keyword| {
        object
            .get(keyword)
            .and_then(::serde_json::Value::as_array)
            .and_then(|branches| branches.first())
    });
    for subschema in every.chain(first_branch) {
        writer.step()?;
        sources.push(referenced(root, subschema, writer)?.unwrap_or(subschema));
    }
    Ok(sources)
}

/// Render an example object: a JSON object whose members are the properties `declared`
/// states — the schema's own and those of the subschemas it is composed of — each carrying
/// an example of its own declared value.
///
/// The properties are emitted in lexicographic order, so one model has ONE generated
/// document however the schema was written and however a JSON map iterates. A schema that
/// declares no properties constrains no member, so there is nothing to describe and the
/// object is rendered empty.
fn render_sample_object(
    root: &::serde_json::Value,
    declared: &[&::serde_json::Value],
    indent: usize,
    depth: usize,
    writer: &mut Writer,
) -> Result<(), Exhausted> {
    let properties = declared_properties_of(declared, writer)?;
    if properties.is_empty() {
        return writer.push("{ }");
    }

    let inner = INDENT.repeat(indent + 1);
    writer.push("{\n")?;
    for (position, (name, property)) in properties.into_iter().enumerate() {
        if position > 0 {
            writer.push(",\n")?;
        }
        writer.push(&inner)?;
        writer.push(&quoted(name))?;
        writer.push(" : ")?;
        render_sample(root, property, indent + 1, depth + 1, writer)?;
    }
    writer.push("\n")?;
    writer.push(&INDENT.repeat(indent))?;
    writer.push("}")
}

/// Render an example array: the Velocity LOOP that maps it, around one example element.
///
/// An array is the one declaration whose members the schema does not state: how many
/// elements the mapped array has is the incoming payload's to say, so what describes it is
/// not a list of examples but the loop that writes one per element — the [`FOREACH_TODO`]
/// comment naming what the caller has to point the loop at, the `#foreach` over that array,
/// the example of ONE element (which is what the `items` schema declares every element to
/// look like), the [`FOREACH_SEPARATOR`] that commas them, and the `#end` that closes the
/// loop. The brackets around it are the array's own, so a generated loop sits exactly where
/// the array it maps does.
///
/// `items` may be a single schema (every element) or, in draft-04, a tuple of schemas
/// (section 5.3.1), of which the first describes the element rendered here. An array
/// declaring no `items` constrains no element, so there is no element expression to loop
/// over and the example is the empty array.
fn render_sample_array(
    root: &::serde_json::Value,
    declared: &[&::serde_json::Value],
    indent: usize,
    depth: usize,
    writer: &mut Writer,
) -> Result<(), Exhausted> {
    let Some(items) = declared.iter().find_map(|source| declared_items(source)) else {
        return writer.push("[ ]");
    };
    // Written as the two halves of the skeleton around the element, so the exact lines and
    // the spacing the generated document carries are readable in one place.
    writer.push(&format!(
        "[\n\
         {FOREACH_TODO}\n\
         #foreach({FOREACH_ELEMENT} in {INPUT_ROOT}.{FOREACH_SOURCE_MEMBER})\n "
    ))?;
    render_sample(root, items, indent, depth + 1, writer)?;
    writer.push(&format!(
        " \n\
         {FOREACH_SEPARATOR}\n\
         {BLOCK_END}\n\
         ]"
    ))
}

/// The schema a `$ref` names, when it resolves inside this document.
///
/// JSON Schema draft-04 section 7 addresses a referenced schema by URI fragment, and the
/// fragment of a reference into the same document is a JSON Pointer (`#/definitions/Pet`),
/// which is what a model's schema carries: API Gateway resolves a `$ref` within the model.
/// A reference to another document, or one this document does not hold, resolves to
/// nothing — its value is then of unknown shape.
///
/// Each segment of the pointer charges one step: walking a reference costs the document as
/// many lookups as the reference names, and the reference is the caller's text, so the cost
/// of resolving one is covered by the budget rather than by the reference's own length.
fn referenced<'doc>(
    root: &'doc ::serde_json::Value,
    schema: &::serde_json::Value,
    writer: &mut Writer,
) -> Result<Option<&'doc ::serde_json::Value>, Exhausted> {
    let Some(pointer) = schema
        .get("$ref")
        .and_then(::serde_json::Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
    else {
        return Ok(None);
    };
    for _ in pointer.split('/').skip(1) {
        writer.step()?;
    }
    Ok(root.pointer(pointer))
}

/// The value `schema` itself states for its instance: the first entry of an `enum`, which
/// restricts the instance to that set (draft-04 section 5.5.1), or the `default` the schema
/// supplies for it (section 6.2).
fn stated_value(schema: &::serde_json::Value) -> Option<&::serde_json::Value> {
    let object = schema.as_object()?;
    object
        .get("enum")
        .and_then(::serde_json::Value::as_array)
        .and_then(|values| values.first())
        .or_else(|| object.get("default"))
}

/// The properties `schema` declares, in the order the document's member map holds them.
///
/// Every name considered charges one step, so building a member list is covered by the same
/// budget the rendering is: a schema declaring a wide object cannot be re-read for free.
/// The ORDER is the caller's to settle — [`declared_properties_of`] merges the sources a
/// composed schema is read from through an ordered map, which is what settles it.
fn declared_properties<'doc>(
    schema: &'doc ::serde_json::Value,
    writer: &mut Writer,
) -> Result<Vec<(&'doc str, &'doc ::serde_json::Value)>, Exhausted> {
    let declared = schema
        .get("properties")
        .and_then(::serde_json::Value::as_object);
    let mut properties: Vec<(&'doc str, &'doc ::serde_json::Value)> = Vec::new();
    for (name, property) in declared.into_iter().flatten() {
        writer.step()?;
        properties.push((name.as_str(), property));
    }
    Ok(properties)
}

/// The properties every source in `declared` declares together, in lexicographic order.
///
/// Where two sources declare one name the FIRST wins, so what a schema declares itself is
/// what its example carries, whatever the subschemas it is composed of also say about that
/// member. The names are merged through an ordered MAP, which both settles that order and
/// keeps the merge's cost proportional to the names considered rather than to their square.
fn declared_properties_of<'doc>(
    declared: &[&'doc ::serde_json::Value],
    writer: &mut Writer,
) -> Result<Vec<(&'doc str, &'doc ::serde_json::Value)>, Exhausted> {
    let mut merged: BTreeMap<&'doc str, &'doc ::serde_json::Value> = BTreeMap::new();
    for source in declared {
        for (name, property) in declared_properties(source, writer)? {
            merged.entry(name).or_insert(property);
        }
    }
    Ok(merged.into_iter().collect())
}

/// The schema an array declares its elements by, for one that declares them.
fn declared_items(schema: &::serde_json::Value) -> Option<&::serde_json::Value> {
    match schema.get("items") {
        Some(::serde_json::Value::Array(schemas)) => schemas.first(),
        other => other,
    }
}

/// The type `schema` declares for its value.
///
/// `type` is the keyword that states it, and draft-04 allows a union of types there
/// (section 5.5.2), of which the first that is not `null` is taken. A schema that states no
/// `type` still declares one implicitly: `properties` only applies to an object and `items`
/// only to an array (sections 5.4.4, 5.3.1), and every value of an `enum` has the JSON type
/// its first entry does.
fn kind(schema: &::serde_json::Value) -> Kind {
    let Some(object) = schema.as_object() else {
        return Kind::Unknown;
    };
    if let Some(declared) = declared_data_type(schema) {
        return match declared {
            "object" => Kind::Object,
            "array" => Kind::Array,
            "string" => Kind::Str,
            "integer" => Kind::Integer,
            "number" => Kind::Number,
            "boolean" => Kind::Boolean,
            "null" => Kind::Null,
            _ => Kind::Unknown,
        };
    }
    if object.contains_key("properties") {
        return Kind::Object;
    }
    if object.contains_key("items") {
        return Kind::Array;
    }
    match object.get("enum").and_then(::serde_json::Value::as_array) {
        Some(values) => match values.first() {
            Some(::serde_json::Value::String(_)) => Kind::Str,
            Some(::serde_json::Value::Object(_)) => Kind::Object,
            Some(::serde_json::Value::Array(_)) => Kind::Array,
            Some(::serde_json::Value::Bool(_)) => Kind::Boolean,
            Some(::serde_json::Value::Number(number)) => {
                if number.is_f64() {
                    Kind::Number
                } else {
                    Kind::Integer
                }
            }
            Some(::serde_json::Value::Null) => Kind::Null,
            None => Kind::Unknown,
        },
        None => Kind::Unknown,
    }
}

/// `value` as a JSON string literal, with whatever it carries escaped.
fn quoted(value: &str) -> String {
    ::serde_json::Value::String(value.to_string()).to_string()
}

/// `value` as its own compact JSON text — how a value the schema states is carried into the
/// generated document.
fn compact(value: &::serde_json::Value) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines a generated loop carries, written out here rather than read from the
    /// constants above: a case that re-asserted the module's own literal would pass however
    /// that literal changed, and what these pin is the recorded text.
    const LOOP_COMMENT: &str = "##TODO: Update this foreach loop to reference array from input json";
    const LOOP_OPEN: &str = "#foreach($elem in $inputRoot.TODO)";
    const LOOP_SEPARATOR: &str = "#if($foreach.hasNext),#end";
    const LOOP_CLOSE: &str = "#end";

    /// Every array a generated document carries, WHEREVER the schema declares it, is the
    /// loop that maps it rather than a collapsed list of one example.
    #[test]
    fn an_array_is_generated_as_the_loop_that_maps_it() {
        // The rows span where the array sits and how it is declared: at the document's
        // root, as a property of an object, nested inside another array, with its elements
        // declared inline, as a tuple (draft-04 section 5.3.1), through a reference
        // (section 7), by a union naming the array type (section 5.5.2), and by `items`
        // alone (section 5.3.1).
        let rows: &[(&str, ::serde_json::Value)] = &[
            (
                "a root array of strings",
                ::serde_json::json!({ "type": "array", "items": { "type": "string" } }),
            ),
            (
                "a root array of objects",
                ::serde_json::json!({
                    "type": "array",
                    "items": { "type": "object", "properties": { "id": { "type": "string" } } }
                }),
            ),
            (
                "an array declared as a property of an object",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "lines": { "type": "array", "items": { "type": "integer" } } }
                }),
            ),
            (
                "an array whose items are declared as a tuple",
                ::serde_json::json!({ "type": "array", "items": [{ "type": "boolean" }] }),
            ),
            (
                "an array whose items are stated by a reference",
                ::serde_json::json!({
                    "type": "array",
                    "items": { "$ref": "#/definitions/Leg" },
                    "definitions": { "Leg": { "type": "number" } }
                }),
            ),
            (
                "an array declared by a union naming the array type",
                ::serde_json::json!({
                    "type": ["null", "array"],
                    "items": { "type": "string" }
                }),
            ),
            (
                "an array typed only by declaring items",
                ::serde_json::json!({ "items": { "type": "string" } }),
            ),
        ];
        for (case, document) in rows {
            let generated = generate_sample_payload(document)
                .unwrap_or_else(|_| panic!("case {case:?}: expected a generated template"));
            for required in [LOOP_COMMENT, LOOP_OPEN, LOOP_SEPARATOR, LOOP_CLOSE] {
                assert!(
                    generated.contains(required),
                    "case {case:?}: expected the generated template to carry {required:?}, \
                     got {generated:?}"
                );
            }
        }
    }

    /// Each array of a document is one loop, and an array that constrains no element has no
    /// element expression for a loop to wrap.
    #[test]
    fn a_document_carries_one_loop_per_array_it_declares() {
        let rows: &[(&str, ::serde_json::Value, usize)] = &[
            (
                "no array at all",
                ::serde_json::json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } }
                }),
                0,
            ),
            (
                "an array declaring no items, which constrains no element",
                ::serde_json::json!({ "type": "array" }),
                0,
            ),
            (
                "one array",
                ::serde_json::json!({ "type": "array", "items": { "type": "string" } }),
                1,
            ),
            (
                "an array nested in an array",
                ::serde_json::json!({
                    "type": "array",
                    "items": { "type": "array", "items": { "type": "string" } }
                }),
                2,
            ),
            (
                "two arrays declared beside each other",
                ::serde_json::json!({
                    "type": "object",
                    "properties": {
                        "first": { "type": "array", "items": { "type": "string" } },
                        "second": { "type": "array", "items": { "type": "integer" } }
                    }
                }),
                2,
            ),
        ];
        for (case, document, loops) in rows {
            let generated = generate_sample_payload(document)
                .unwrap_or_else(|_| panic!("case {case:?}: expected a generated template"));
            let counted = generated.matches(LOOP_COMMENT).count();
            assert_eq!(
                counted, *loops,
                "case {case:?}: expected {loops} loop(s), got {counted} in {generated:?}"
            );
            let separators = generated.matches(LOOP_SEPARATOR).count();
            assert_eq!(
                separators, *loops,
                "case {case:?}: expected {loops} separator(s), got {separators} in \
                 {generated:?}"
            );
        }
    }

    #[test]
    fn the_roots_data_type_is_read_through_the_references_and_compositions_it_is_stated_by() {
        // The rows span HOW a document states its root's type: inline, through an
        // in-document reference to a definition, through a chain of them, through the
        // composition keywords draft-04 sections 5.5.3 to 5.5.5 name, and through a
        // reference that resolves to nothing or back to itself — each paired with a
        // composite and with a scalar target, which is what the rule turns on.
        let rows: &[(&str, ::serde_json::Value, Option<&str>)] = &[
            (
                "an inline object",
                ::serde_json::json!({ "type": "object" }),
                None,
            ),
            (
                "an inline scalar",
                ::serde_json::json!({ "type": "string" }),
                Some("string"),
            ),
            (
                "a root referencing a definition that declares an object",
                ::serde_json::json!({
                    "$ref": "#/definitions/Pet",
                    "definitions": { "Pet": { "type": "object", "properties": {} } }
                }),
                None,
            ),
            (
                "a root referencing a definition that declares a scalar",
                ::serde_json::json!({
                    "$schema": "http://json-schema.org/draft-04/schema#",
                    "title": "Scalar",
                    "$ref": "#/definitions/Measure",
                    "definitions": { "Measure": { "type": "string" } }
                }),
                Some("string"),
            ),
            (
                "a chain of references ending at a scalar",
                ::serde_json::json!({
                    "$ref": "#/definitions/First",
                    "definitions": {
                        "First": { "$ref": "#/definitions/Second" },
                        "Second": { "type": "integer" }
                    }
                }),
                Some("integer"),
            ),
            (
                "a root composed with allOf of a scalar",
                ::serde_json::json!({ "allOf": [{ "type": "number" }] }),
                Some("number"),
            ),
            (
                "a root composed with oneOf of an object",
                ::serde_json::json!({ "oneOf": [{ "type": "object" }, { "type": "string" }] }),
                None,
            ),
            (
                "a root declaring its own type beside a composition",
                ::serde_json::json!({ "type": "object", "allOf": [{ "type": "string" }] }),
                None,
            ),
            (
                "a root whose reference resolves to nothing",
                ::serde_json::json!({ "$ref": "#/definitions/Absent" }),
                None,
            ),
            (
                "a root that references itself",
                ::serde_json::json!({ "$ref": "#" }),
                None,
            ),
        ];
        for (case, document, unsupported) in rows {
            let refused = unsupported_template_data_type(document);
            let expected = unsupported.map(unsupported_template_data_type_message);
            assert_eq!(
                refused.as_deref(),
                expected.as_deref(),
                "case {case:?}: expected unsupported={unsupported:?}, got {refused:?}"
            );
        }
    }
}
