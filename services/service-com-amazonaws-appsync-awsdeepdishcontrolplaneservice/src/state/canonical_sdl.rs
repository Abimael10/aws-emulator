// SPDX-License-Identifier: CC0-1.0

//! How a whole SCHEMA is written back as GraphQL SDL: the canonical document
//! `GetIntrospectionSchema` answers with for `format=SDL`.
//!
//! A schema is reported as the service's own PRINTING of the type system it holds rather
//! than as the document some request uploaded, so one schema reads the same however its
//! author wrote it — whatever order the document declared its types and members in, and
//! whether or not it wrote a `schema` definition at all. That is what lets a client diff,
//! hash or round-trip the schema it reads: the same type system always renders to the same
//! bytes.
//!
//! The layout is recorded behaviour:
//!
//! * the document opens with the `schema` definition that binds the root operation types
//!   the type system holds, written `query`, then `mutation`, then `subscription`;
//! * then the directive definitions the schema itself defines;
//! * then the type definitions, grouped by kind in the order [`KIND_ORDER`] states and in
//!   ascending name order within each group;
//! * every definition's members — a type's fields, an input object's input fields, an enum's
//!   values, a field's arguments, the interfaces an object implements, the types a union
//!   names and the directives written on any of them — in ascending NAME order rather than
//!   in the order the author wrote them;
//! * a body indented by [`INDENT`], one member per line, with no blank line between members;
//! * a directive ARGUMENT written with a space before its colon (`@deprecated(reason : "…")`),
//!   which is how the printed form of an applied directive writes one;
//! * the blocks separated by a blank line, and the document terminated by a newline.
//!
//! Whether the DIRECTIVES are part of the document at all is the call's own choice
//! ([`Printer::directives`]): a call that asks for none is answered with the type system
//! alone — no directive definitions and no directive written on any definition — and every
//! other call with the directives the schema states.
//!
//! Everything the document says comes from the parsed definitions
//! ([`super::TypeDefinition`]) and from the root operation types the schema binds. Nothing
//! is taken from the request, and nothing of the uploaded text survives into it.
//!
//! This is the whole schema's rendering. ONE type read on its own is reported in the
//! layout that read records ([`super::type_sdl`]), which is a different document — a
//! member on a line of its own with a blank line around it, no indentation, the members in
//! the order the definition declares them, a directive argument written with no space
//! before its colon, and a single-line description in block quotes. The two modules
//! therefore share what is GraphQL's GRAMMAR rather than either layout — the keyword a kind
//! is declared with and the way a type reference is written ([`keyword`], [`type_text`]) —
//! and each states its own layout: the order, the indentation, the spacing and the quoting
//! are exactly what the two recorded renderings differ in.

use super::introspection_schema::DirectiveDeclaration;
use super::schema_types::{
    DirectiveUse, EnumValueDefinition, FieldDefinition, InputValueDefinition, TypeDefinition,
    ENUM_KIND, INPUT_OBJECT_KIND, INTERFACE_KIND, OBJECT_KIND, SCALAR_KIND, UNION_KIND,
};
use super::type_sdl::{keyword, type_text};

/// The indentation one level of a rendered body carries.
const INDENT: &str = "  ";

/// The order the document writes its type definitions in, by the kind each declares.
///
/// Recorded behaviour: the kinds are written in this order and the definitions of one kind
/// in ascending name order, so where a type sits in the document follows from the type
/// system alone.
const KIND_ORDER: &[&str] = &[
    INTERFACE_KIND,
    UNION_KIND,
    OBJECT_KIND,
    ENUM_KIND,
    SCALAR_KIND,
    INPUT_OBJECT_KIND,
];

/// One root operation type a schema binds: the operation it is the root of, and the type.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RootOperation<'a> {
    /// `query`, `mutation` or `subscription`, as a `schema` definition writes it.
    pub(crate) operation: &'static str,
    pub(crate) type_name: &'a str,
}

/// How one schema is printed: the type system as it stands, with or without the directives
/// it states.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Printer {
    /// Whether the directives the schema states are part of the document: the definitions
    /// it writes, and the directives written on its definitions and their members.
    pub(crate) directives: bool,
}

impl Printer {
    /// The canonical SDL of one schema: its root operation types, the directives it defines
    /// and every type it holds.
    pub(crate) fn render_schema(
        &self,
        roots: &[RootOperation<'_>],
        directives: &[&DirectiveDeclaration],
        types: &[&TypeDefinition],
    ) -> String {
        let mut blocks: Vec<String> = Vec::new();
        // A type system that binds no root operation type at all declares no `schema`
        // definition: there is no root to write in one.
        if !roots.is_empty() {
            let mut lines = vec![format!("schema {OPEN_BRACE}")];
            lines.extend(
                roots
                    .iter()
                    .map(|root| format!("{INDENT}{}: {}", root.operation, root.type_name)),
            );
            lines.push(CLOSE_BRACE.to_string());
            blocks.push(lines.join("\n"));
        }
        if self.directives {
            blocks.extend(
                directives
                    .iter()
                    .map(|directive| self.render_directive(directive)),
            );
        }
        for kind in KIND_ORDER {
            let mut group: Vec<&&TypeDefinition> = types
                .iter()
                .filter(|definition| definition.kind == *kind)
                .collect();
            group.sort_by(|left, right| left.name.cmp(&right.name));
            blocks.extend(
                group
                    .into_iter()
                    .map(|definition| self.render_type(definition)),
            );
        }
        let mut document = blocks.join("\n\n");
        document.push('\n');
        document
    }

    /// One directive DEFINITION the schema states, as GraphQL's SDL declares one: the
    /// directive's name, the arguments it takes, whether it is repeatable and the locations
    /// it may be applied at.
    fn render_directive(&self, directive: &DirectiveDeclaration) -> String {
        let mut lines = description_lines(directive.description.as_deref(), "");
        let mut line = format!("directive @{}", directive.name);
        line.push_str(&self.arguments_text(&directive.arguments));
        if directive.is_repeatable {
            line.push_str(" repeatable");
        }
        if !directive.locations.is_empty() {
            line.push_str(" on ");
            line.push_str(&directive.locations.join(" | "));
        }
        lines.push(line);
        lines.join("\n")
    }

    /// One type definition, as the document writes it.
    fn render_type(&self, definition: &TypeDefinition) -> String {
        let mut lines = description_lines(definition.description.as_deref(), "");
        let mut line = format!("{} {}", keyword(definition.kind), definition.name);
        if !definition.interfaces.is_empty() {
            line.push_str(" implements ");
            line.push_str(&in_name_order(&definition.interfaces).join(" & "));
        }
        line.push_str(&self.directives_text(&definition.directives));
        if definition.kind == UNION_KIND && !definition.members.is_empty() {
            line.push_str(" = ");
            line.push_str(&in_name_order(&definition.members).join(" | "));
        }
        let members = self.member_lines(definition);
        if members.is_empty() {
            lines.push(line);
            return lines.join("\n");
        }
        line.push(' ');
        line.push_str(OPEN_BRACE);
        lines.push(line);
        lines.extend(members);
        lines.push(CLOSE_BRACE.to_string());
        lines.join("\n")
    }

    /// The lines of a definition's body: the members the kind carries, each in ascending
    /// name order, and none at all for a kind whose definition has no body.
    fn member_lines(&self, definition: &TypeDefinition) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        match definition.kind {
            OBJECT_KIND | INTERFACE_KIND => {
                for field in sorted_by_name(&definition.fields, |field| &field.name) {
                    lines.extend(description_lines(field.description.as_deref(), INDENT));
                    lines.push(format!("{INDENT}{}", self.field_text(field)));
                }
            }
            INPUT_OBJECT_KIND => {
                for input in sorted_by_name(&definition.input_fields, |input| &input.name) {
                    lines.extend(description_lines(input.description.as_deref(), INDENT));
                    lines.push(format!("{INDENT}{}", self.input_value_text(input)));
                }
            }
            ENUM_KIND => {
                for value in sorted_by_name(&definition.enum_values, |value| &value.name) {
                    lines.extend(description_lines(value.description.as_deref(), INDENT));
                    lines.push(format!("{INDENT}{}", self.enum_value_text(value)));
                }
            }
            _ => {}
        }
        lines
    }

    /// One field: its name, the arguments it takes, the type it resolves to and the
    /// directives written on it.
    fn field_text(&self, field: &FieldDefinition) -> String {
        let mut text = field.name.clone();
        text.push_str(&self.arguments_text(&field.arguments));
        if let Some(value_type) = field.value_type.as_ref() {
            text.push_str(": ");
            text.push_str(&type_text(value_type));
        }
        text.push_str(&self.directives_text(&field.directives));
        text
    }

    /// The arguments a field or a directive definition takes, in ascending name order, or
    /// nothing at all for one that takes none.
    fn arguments_text(&self, arguments: &[InputValueDefinition]) -> String {
        if arguments.is_empty() {
            return String::new();
        }
        let written: Vec<String> = sorted_by_name(arguments, |argument| &argument.name)
            .into_iter()
            .map(|argument| self.input_value_text(argument))
            .collect();
        format!("({})", written.join(", "))
    }

    /// One argument or input field: its name, the type it takes, the default value it states
    /// and the directives written on it.
    fn input_value_text(&self, input: &InputValueDefinition) -> String {
        let mut text = input.name.clone();
        if let Some(value_type) = input.value_type.as_ref() {
            text.push_str(": ");
            text.push_str(&type_text(value_type));
        }
        if let Some(default) = input.default_value.as_ref() {
            text.push_str(" = ");
            text.push_str(default);
        }
        text.push_str(&self.directives_text(&input.directives));
        text
    }

    /// One enum value: the value, and the directives written on it.
    fn enum_value_text(&self, value: &EnumValueDefinition) -> String {
        let mut text = value.name.clone();
        text.push_str(&self.directives_text(&value.directives));
        text
    }

    /// The directives written on a definition or on one of its members, in ascending name
    /// order, each preceded by the space that separates it from what it is written on — and
    /// nothing at all for a document the call asked for no directives in.
    ///
    /// RECORDED behaviour: an applied directive's ARGUMENT is written with a space before
    /// its colon, unlike a field's or an argument's own type, which carries none.
    fn directives_text(&self, directives: &[DirectiveUse]) -> String {
        if !self.directives {
            return String::new();
        }
        let mut text = String::new();
        for directive in sorted_by_name(directives, |directive| &directive.name) {
            text.push_str(" @");
            text.push_str(&directive.name);
            if directive.arguments.is_empty() {
                continue;
            }
            let stated: Vec<String> =
                sorted_by_name(&directive.arguments, |argument| &argument.name)
                    .into_iter()
                    .map(|argument| format!("{} : {}", argument.name, argument.value))
                    .collect();
            text.push('(');
            text.push_str(&stated.join(", "));
            text.push(')');
        }
        text
    }
}

/// The line a rendered body opens and closes with.
const OPEN_BRACE: &str = "{";
const CLOSE_BRACE: &str = "}";

/// The names of the types a definition NAMES, in ascending name order.
fn in_name_order(names: &[String]) -> Vec<&str> {
    let mut ordered: Vec<&str> = names.iter().map(String::as_str).collect();
    ordered.sort_unstable();
    ordered
}

/// `members` in ascending order of the name `named` reads off each of them.
fn sorted_by_name<M>(members: &[M], named: impl Fn(&M) -> &String) -> Vec<&M> {
    let mut ordered: Vec<&M> = members.iter().collect();
    ordered.sort_by(|left, right| named(left).cmp(named(right)));
    ordered
}

/// The lines a description is written over, at `indent`, or none at all where there is no
/// description: a single-line description as the one quoted line GraphQL writes it on, and a
/// description that spans lines between block-string delimiters.
fn description_lines(description: Option<&str>, indent: &str) -> Vec<String> {
    let Some(description) = description else {
        return Vec::new();
    };
    if description.contains('\n') {
        let mut lines = vec![format!("{indent}{BLOCK_QUOTE}")];
        lines.extend(
            description
                .replace(BLOCK_QUOTE, ESCAPED_BLOCK_QUOTE)
                .lines()
                .map(|line| format!("{indent}{line}")),
        );
        lines.push(format!("{indent}{BLOCK_QUOTE}"));
        return lines;
    }
    let quoted = description.replace('\\', "\\\\").replace('"', "\\\"");
    vec![format!("{indent}\"{quoted}\"")]
}

/// The quotation a GraphQL block string is delimited by, and how a description that carries
/// that quotation itself writes it.
const BLOCK_QUOTE: &str = "\"\"\"";
const ESCAPED_BLOCK_QUOTE: &str = "\\\"\"\"";
