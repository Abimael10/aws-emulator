// SPDX-License-Identifier: CC0-1.0

//! The introspection schema of a GraphQL API: the type system it holds, reported in
//! either representation the model names, and the refusal for an API that holds none.
//!
//! `GetIntrospectionSchema` "Retrieves the introspection schema for a GraphQL API", so
//! what it answers is the type system the API holds AT THE INSTANT of the call: the
//! types stored on the API, whatever installed them — a schema creation or a single
//! `CreateType` — together with what the created document declares that no type holds,
//! the root operation types a `schema` definition binds and the directives the schema
//! defines. Nothing of the request takes part in the answer beyond the representation
//! and the directive flag it asks for.
//!
//! The representation the `OutputType` the call names chooses is the same distinction
//! one type is reported under (`super::schema_types`): the canonical SDL the schema's
//! types are printed as (`super::canonical_sdl`), or the introspection JSON they render
//! to. Both are PRINTED from the stored definitions rather than from the document that
//! installed them, so one type system is reported the same way however its author wrote
//! it, the two representations cannot disagree about one schema, and neither can disagree
//! with what `GetType` answers for a type of it.
//!
//! Both representations report the schema's own types. The introspection JSON reports what
//! a schema holds BESIDE them too, because an introspection document is the result of the
//! introspection query over the whole schema: the types GraphQL's introspection system
//! defines, the built-in scalars the schema references, and the directives it supports
//! (`super::introspection_meta_schema`).
//!
//! An API whose types are empty holds no type system, so there is no schema to
//! introspect: what such a call reads as the API's schema is an EMPTY document, and the
//! refusal carries the parse of that document — which stops at its origin — whatever
//! left the API without a type system.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use apollo_parser::cst::{self, CstNode};
use apollo_parser::Parser;
use serde_json::{json, Value};

use super::canonical_sdl::{Printer, RootOperation};
use super::introspection_meta_schema::{
    built_in_scalar_description, meta_type, SCHEMA_DIRECTIVES_SDL, SCHEMA_META_TYPE,
};
use super::schema_types::{input_value_definition, input_value_json};
use super::{
    invalid_syntax_detail, GraphqlApiRecord, InputValueDefinition, TypeDefinition, TypeRecord,
    BUILT_IN_SCALARS, DEFINITION_RECURSION_LIMIT, DEFINITION_TOKEN_LIMIT, DOCUMENT_ORIGIN,
    JSON_FORMAT, SCALAR_KIND,
};

/// The operation each root operation type is the root OF, as a `schema` definition writes
/// it.
const QUERY_OPERATION: &str = "query";
const MUTATION_OPERATION: &str = "mutation";
const SUBSCRIPTION_OPERATION: &str = "subscription";

/// The name the query root operation type carries in a schema that binds none.
///
/// GraphQL's own default: "the query root operation type must be provided and must be an
/// Object type", and a schema that writes no `schema` definition takes the type named
/// `Query` as it. The same rule gives [`DEFAULT_MUTATION_TYPE`] and
/// [`DEFAULT_SUBSCRIPTION_TYPE`] their names.
pub const DEFAULT_QUERY_TYPE: &str = "Query";

/// See [`DEFAULT_QUERY_TYPE`].
pub const DEFAULT_MUTATION_TYPE: &str = "Mutation";

/// See [`DEFAULT_QUERY_TYPE`].
pub const DEFAULT_SUBSCRIPTION_TYPE: &str = "Subscription";

/// What a schema document declares that its types do not: the root operation types its
/// `schema` definition binds, that definition's own text, and the directives it defines.
///
/// Read from the stored document rather than kept beside it, so the declarations reported
/// are the ones the document that was CREATED writes and cannot drift from it.
#[derive(Debug, Default)]
struct SchemaDeclarations {
    /// The type the document binds as its query root, or `None` where it binds none.
    query: Option<String>,
    /// The type the document binds as its mutation root.
    mutation: Option<String>,
    /// The type the document binds as its subscription root.
    subscription: Option<String>,
    /// The directives the document defines, in document order.
    directives: Vec<DirectiveDeclaration>,
}

/// One directive a schema document defines, as its definition declares it.
///
/// What the definition DECLARES is kept rather than the text that declared it, because both
/// representations of a schema are written from the declaration: the introspection
/// `__Directive` and the canonical SDL rendering ([`super::canonical_sdl`]).
#[derive(Debug)]
pub(crate) struct DirectiveDeclaration {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// Whether the definition marks the directive `repeatable`.
    pub(crate) is_repeatable: bool,
    /// The locations the definition names a directive may be applied at, as GraphQL's
    /// `__DirectiveLocation` spells them.
    pub(crate) locations: Vec<String>,
    /// The arguments the definition declares, which are `__InputValue`s like a field's.
    pub(crate) arguments: Vec<InputValueDefinition>,
}

/// Read `document` for everything a schema declares beside its types.
///
/// The parse is bounded by the same two limits one definition is read under, so neither
/// the depth nor the amount of work reading one document costs comes from the request.
fn schema_declarations(document: &str) -> SchemaDeclarations {
    let tree = Parser::new(document)
        .recursion_limit(DEFINITION_RECURSION_LIMIT)
        .token_limit(DEFINITION_TOKEN_LIMIT)
        .parse();
    let mut declarations = SchemaDeclarations::default();
    for node in tree.document().definitions() {
        match node {
            cst::Definition::SchemaDefinition(schema) => {
                bind_root_types(&mut declarations, schema.root_operation_type_definitions());
            }
            // An extension of the schema binds root operation types the same way the
            // definition does, so a document that extends its own schema is read for
            // them too.
            cst::Definition::SchemaExtension(extension) => {
                bind_root_types(&mut declarations, extension.root_operation_type_definitions());
            }
            cst::Definition::DirectiveDefinition(directive) => {
                let Some(name) = directive.name().map(|name| name.text().to_string()) else {
                    continue;
                };
                declarations.directives.push(DirectiveDeclaration {
                    name,
                    description: directive
                        .description()
                        .and_then(|description| description.string_value())
                        .map(String::from),
                    is_repeatable: directive.repeatable_token().is_some(),
                    locations: directive
                        .directive_locations()
                        .map(|locations| {
                            locations
                                .directive_locations()
                                .map(|location| node_text(location.syntax()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    arguments: directive
                        .arguments_definition()
                        .map(|arguments| {
                            arguments
                                .input_value_definitions()
                                .map(input_value_definition)
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
            _ => {}
        }
    }
    declarations
}

/// Bind every root operation type `roots` names onto `declarations`.
fn bind_root_types(
    declarations: &mut SchemaDeclarations,
    roots: impl Iterator<Item = cst::RootOperationTypeDefinition>,
) {
    for root in roots {
        let Some(named) = root
            .named_type()
            .and_then(|named| named.name())
            .map(|name| name.text().to_string())
        else {
            continue;
        };
        let Some(operation) = root.operation_type() else {
            continue;
        };
        if operation.query_token().is_some() {
            declarations.query = Some(named);
        } else if operation.mutation_token().is_some() {
            declarations.mutation = Some(named);
        } else if operation.subscription_token().is_some() {
            declarations.subscription = Some(named);
        }
    }
}

/// The source text of one parsed node, as the document writes it.
fn node_text(node: &apollo_parser::SyntaxNode) -> String {
    node.text().to_string().trim().to_string()
}

impl GraphqlApiRecord {
    /// The type this API's schema binds as the root of its QUERY operations.
    ///
    /// Which type holds them is the schema's own binding: the type its `schema` definition
    /// binds, and the type named [`DEFAULT_QUERY_TYPE`] for a schema that binds none, which
    /// is GraphQL's own default. The same rule gives [`Self::mutation_root_type`] and
    /// [`Self::subscription_root_type`] theirs, so every reader of a root operation type
    /// reads it one way.
    pub fn query_root_type(&self) -> String {
        self.bound_root_type(
            |declarations| declarations.query.clone(),
            DEFAULT_QUERY_TYPE,
        )
    }

    /// The type this API's schema binds as the root of its MUTATION operations; see
    /// [`Self::query_root_type`].
    pub fn mutation_root_type(&self) -> String {
        self.bound_root_type(
            |declarations| declarations.mutation.clone(),
            DEFAULT_MUTATION_TYPE,
        )
    }

    /// The type this API's schema binds as the root of its SUBSCRIPTION operations; see
    /// [`Self::query_root_type`].
    pub fn subscription_root_type(&self) -> String {
        self.bound_root_type(
            |declarations| declarations.subscription.clone(),
            DEFAULT_SUBSCRIPTION_TYPE,
        )
    }

    /// The type the stored schema document binds for one root operation, or GraphQL's
    /// default name for it where the document binds none.
    fn bound_root_type(
        &self,
        bound: impl Fn(&SchemaDeclarations) -> Option<String>,
        default_name: &str,
    ) -> String {
        self.schema_document
            .as_deref()
            .and_then(|document| bound(&schema_declarations(document)))
            .unwrap_or_else(|| default_name.to_string())
    }

    /// The SUBSCRIPTION fields this API's schema declares, in the order the schema
    /// declares them — the fields a real-time client may start a subscription on.
    ///
    /// Which type holds them is the schema's own binding: the type its `schema` definition
    /// binds as the subscription root, and the type named [`DEFAULT_SUBSCRIPTION_TYPE`] for
    /// a schema that binds none, which is GraphQL's own default. An API whose schema holds
    /// no such type declares no subscription field at all, which is what a client
    /// subscribing against it finds.
    ///
    /// Read from the stored document and the stored types, so the fields reported are the
    /// ones the schema that was CREATED declares and cannot drift from it.
    pub fn subscription_fields(&self) -> Vec<String> {
        let named = self.subscription_root_type();
        self.types
            .get(&named)
            .map(|record| {
                record
                    .parsed
                    .fields
                    .iter()
                    .map(|field| field.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The SUBSCRIPTION fields of this API's schema that the MUTATION field `mutation_field`
    /// publishes to, in the order the schema declares them.
    ///
    /// AppSync links the two with a directive on the subscription field:
    /// `@aws_subscribe(mutations: ["<mutationField>"])` names the mutations whose result that
    /// subscription receives, so a mutation publishes to every subscription field whose
    /// directive names it and to no other. A schema that writes the directive on no field
    /// links no mutation to any subscription, which is what a client subscribing to such a
    /// schema observes: a subscription nothing ever delivers to.
    ///
    /// Read from the stored types, so the links reported are the ones the schema that was
    /// CREATED declares.
    pub fn subscriptions_published_by(&self, mutation_field: &str) -> Vec<String> {
        let named = self.subscription_root_type();
        self.types
            .get(&named)
            .map(|record| {
                record
                    .parsed
                    .fields
                    .iter()
                    .filter(|field| {
                        subscribed_mutations(&field.directives)
                            .iter()
                            .any(|named| named == mutation_field)
                    })
                    .map(|field| field.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// This API's introspection schema in `format`, or the text a
    /// `GraphQLSchemaException` carries for an API that holds no schema to introspect.
    ///
    /// What the call reads as the schema of an API that holds no type system is an EMPTY
    /// document, so the refusal is the parse of that document — the same text whatever
    /// left the API without one, because the API holds the same nothing either way.
    ///
    /// `include_directives` is the call's own flag for "whether the schema introspection
    /// should contain directives": the directives the schema defines are reported only
    /// when it asks for them.
    pub fn introspection_schema(
        &self,
        format: &str,
        include_directives: bool,
    ) -> Result<Vec<u8>, String> {
        // A type system with no type in it defines no schema — whatever left it empty: an
        // API no creation has run for, one whose creation FAILED, and one whose creation
        // is still in progress all hold no schema yet.
        if self.types.is_empty() {
            return Err(invalid_syntax_detail(DOCUMENT_ORIGIN.0, DOCUMENT_ORIGIN.1));
        }
        let declarations = schema_declarations(self.schema_document.as_deref().unwrap_or(""));
        let schema = if format == JSON_FORMAT {
            self.introspection_json(&declarations, include_directives)
        } else {
            self.introspection_sdl(&declarations, include_directives)
        };
        Ok(schema.into_bytes())
    }

    /// This API's type system as introspection's `__Schema`, for the DATA plane's own
    /// `__schema` field (`crate::service::graphql_api::introspection`), or `None` for an
    /// API that holds no type system — there is nothing to introspect then, which is what
    /// the call above refuses such an API with.
    ///
    /// The same rendering the call answers with, so the two cannot disagree about one API.
    pub fn introspection_schema_document(&self, include_directives: bool) -> Option<Value> {
        if self.types.is_empty() {
            return None;
        }
        let declarations = schema_declarations(self.schema_document.as_deref().unwrap_or(""));
        Some(self.schema_json(&declarations, include_directives))
    }

    /// This API's schema as GraphQL SDL: the service's own CANONICAL printing of the type
    /// system the API holds ([`super::canonical_sdl`]) — the `schema` definition that binds
    /// the root operation types it holds, the directives the schema states where the call
    /// asks for them, and every type it holds.
    ///
    /// Printed from the stored definitions rather than from the text that created them, so
    /// one type system renders to the same document however its author wrote it: the layout,
    /// the order of the definitions and the order of each definition's members are the
    /// service's, and only the names, the types and the directives are the schema's.
    fn introspection_sdl(
        &self,
        declarations: &SchemaDeclarations,
        include_directives: bool,
    ) -> String {
        let bound = self.bound_root_types(declarations);
        let roots: Vec<RootOperation<'_>> = bound
            .iter()
            .map(|(operation, type_name)| RootOperation {
                operation,
                type_name,
            })
            .collect();
        let directives: Vec<&DirectiveDeclaration> = declarations.directives.iter().collect();
        let types: Vec<&TypeDefinition> = self
            .types_in_creation_order()
            .map(|record| &record.parsed)
            .collect();
        Printer {
            directives: include_directives,
        }
        .render_schema(&roots, &directives, &types)
    }

    /// The root operation types this schema binds that the type system HOLDS, each with the
    /// operation it is the root of, in the order a `schema` definition writes them.
    ///
    /// Each is the type the document BINDS for that operation, or GraphQL's default name for
    /// it where the document binds none — the same reading every reader of a root operation
    /// type takes ([`Self::query_root_type`]) — and a root bound to a type the API does not
    /// hold is no root at all, the same reading introspection reports one under
    /// ([`Self::root_type_json`]), so a type system with no query type binds nothing.
    fn bound_root_types(&self, declarations: &SchemaDeclarations) -> Vec<(&'static str, String)> {
        [
            (QUERY_OPERATION, &declarations.query, DEFAULT_QUERY_TYPE),
            (
                MUTATION_OPERATION,
                &declarations.mutation,
                DEFAULT_MUTATION_TYPE,
            ),
            (
                SUBSCRIPTION_OPERATION,
                &declarations.subscription,
                DEFAULT_SUBSCRIPTION_TYPE,
            ),
        ]
        .into_iter()
        .map(|(operation, bound, default_name)| {
            (
                operation,
                bound.clone().unwrap_or_else(|| default_name.to_string()),
            )
        })
        .filter(|(_, type_name)| self.types.contains_key(type_name))
        .collect()
    }

    /// This API's schema as GraphQL introspection JSON: the result of the introspection
    /// query over it, so the `__Schema` the introspection system defines sits under the
    /// `data` every GraphQL result carries its data under — its root operation types,
    /// every type it holds and, when the call asks for them, the directives it supports.
    ///
    /// Each type is rendered as GraphQL's introspection system defines a `__Type` —
    /// which is not the document a read of ONE type answers with (`super::type_json`) —
    /// and the introspection system's OWN types and the built-in scalars the schema
    /// references are reported beside them, because an introspection document holds every
    /// type its schema has.
    fn introspection_json(
        &self,
        declarations: &SchemaDeclarations,
        include_directives: bool,
    ) -> String {
        let schema = self.schema_json(declarations, include_directives);
        serde_json::to_string_pretty(&json!({"data": {"__schema": schema}})).unwrap_or_default()
    }

    /// The `__Schema` itself, which both that document and the data plane's own `__schema`
    /// field report ([`Self::introspection_schema_document`]).
    fn schema_json(
        &self,
        declarations: &SchemaDeclarations,
        include_directives: bool,
    ) -> Value {
        let types: Vec<Value> = self
            .introspected_types(declarations)
            .iter()
            .filter_map(|name| self.introspected_type(name))
            .collect();
        let mut schema = json!({
            "queryType": self.root_type_json(declarations.query.as_deref(), DEFAULT_QUERY_TYPE),
            "mutationType": self
                .root_type_json(declarations.mutation.as_deref(), DEFAULT_MUTATION_TYPE),
            "subscriptionType": self
                .root_type_json(declarations.subscription.as_deref(), DEFAULT_SUBSCRIPTION_TYPE),
            "types": types,
        });
        if include_directives {
            let directives = Value::Array(self.reported_directives(declarations));
            if let Some(members) = schema.as_object_mut() {
                members.insert("directives".to_string(), directives);
            }
        }
        schema
    }

    /// Every type the introspection document reports, in the order the schema REACHES it.
    ///
    /// A GraphQL schema's types are the ones reachable from what it declares, so the
    /// document reports each type the first time it is reached: from the root operation
    /// types the schema binds, outwards over the types their fields, arguments, interfaces
    /// and members resolve to — a type's own type first and then the types its arguments
    /// take — then every type the API holds that nothing reaches, and finally the
    /// introspection system's own `__Schema` and everything it names, which every schema
    /// holds ([`super::introspection_meta_schema`]).
    ///
    /// Walked ITERATIVELY over an explicit stack, so reading a type system costs one step
    /// per reference and no stack frame per level however deeply its types nest.
    fn introspected_types(&self, declarations: &SchemaDeclarations) -> Vec<String> {
        let mut roots: Vec<String> = self
            .bound_root_types(declarations)
            .into_iter()
            .map(|(_, type_name)| type_name)
            .collect();
        roots.extend(
            self.types_in_creation_order()
                .map(|record| record.parsed.name.clone()),
        );
        roots.push(SCHEMA_META_TYPE.to_string());

        let mut reported: Vec<String> = Vec::new();
        let mut reached: BTreeSet<String> = BTreeSet::new();
        for root in roots {
            let mut pending: Vec<String> = vec![root];
            while let Some(name) = pending.pop() {
                if !reached.insert(name.clone()) {
                    continue;
                }
                for referenced in self.referenced_by(&name).into_iter().rev() {
                    pending.push(referenced);
                }
                reported.push(name);
            }
        }
        reported
    }

    /// The types the definition of `name` references, in the order it reaches them, or none
    /// at all for a name no definition of this schema declares — a built-in scalar, and a
    /// reference to a type nothing defines, each of which names no further type.
    fn referenced_by(&self, name: &str) -> Vec<String> {
        self.definition_of(name)
            .map(TypeDefinition::referenced_types)
            .unwrap_or_default()
    }

    /// The definition this schema holds for `name`: the API's own stored type, or the
    /// introspection system's definition of one of ITS types, which every schema holds.
    fn definition_of(&self, name: &str) -> Option<&TypeDefinition> {
        match self.types.get(name) {
            Some(record) => Some(&record.parsed),
            None => meta_type(name),
        }
    }

    /// One type of the introspection document as a `__Type`, or `None` for a name this
    /// schema holds no type for — a reference to a type nothing defines is no type of the
    /// document.
    fn introspected_type(&self, name: &str) -> Option<Value> {
        if let Some(definition) = self.definition_of(name) {
            return Some(definition.introspection(&self.types));
        }
        BUILT_IN_SCALARS
            .contains(&name)
            .then(|| built_in_scalar_json(name))
    }

    /// The directives the introspection document reports: the ones this schema's own
    /// document DEFINES, and the ones every schema of this service holds without defining
    /// them ([`super::introspection_meta_schema::SCHEMA_DIRECTIVES_SDL`]).
    ///
    /// Each name is reported once, and a schema that defines one of those names itself is
    /// reported with its OWN definition of it: a schema holds one directive per name.
    fn reported_directives(&self, declarations: &SchemaDeclarations) -> Vec<Value> {
        let defined = &declarations.directives;
        defined
            .iter()
            .chain(
                directives_every_schema_holds()
                    .iter()
                    .filter(|held| !defined.iter().any(|own| own.name == held.name)),
            )
            .map(|directive| self.directive_json(directive))
            .collect()
    }

    /// The types this API holds, in the order they were created.
    fn types_in_creation_order(&self) -> impl Iterator<Item = &TypeRecord> {
        let mut records: Vec<&TypeRecord> = self.types.values().collect();
        records.sort_by_key(|record| record.seq);
        records.into_iter()
    }

    /// One root operation type as introspection reports it: the type the schema binds for
    /// it, or the type of GraphQL's default name for it where the schema binds none —
    /// and null where the API holds no type of that name, because a schema with no
    /// mutations has no mutation root.
    fn root_type_json(&self, bound: Option<&str>, default_name: &str) -> Value {
        let name = bound.unwrap_or(default_name);
        match self.types.contains_key(name) {
            true => json!({"name": name}),
            false => Value::Null,
        }
    }

    /// One directive the schema holds as introspection's `__Directive`: the members that
    /// type declares — the directive's name, its description, the locations it may be
    /// applied at and the arguments it takes
    /// ([`super::introspection_meta_schema`]) — and nothing else.
    fn directive_json(&self, directive: &DirectiveDeclaration) -> Value {
        json!({
            "name": directive.name,
            "description": directive.description,
            "locations": directive.locations,
            "args": directive
                .arguments
                .iter()
                .map(|argument| input_value_json(argument, &self.types))
                .collect::<Vec<Value>>(),
        })
    }
}

/// The directives EVERY schema of this service holds without defining them, read once from
/// the SDL that declares them ([`SCHEMA_DIRECTIVES_SDL`]) by the same reader a caller's own
/// document is read with, so one reading declares both.
fn directives_every_schema_holds() -> &'static Vec<DirectiveDeclaration> {
    static HELD: OnceLock<Vec<DirectiveDeclaration>> = OnceLock::new();
    HELD.get_or_init(|| schema_declarations(SCHEMA_DIRECTIVES_SDL).directives)
}

/// The mutation fields a subscription field's `@aws_subscribe` directive names, or an empty
/// list where the directives write none.
///
/// The names are the parser's own string values of the argument
/// (`DirectiveArgument::strings`), so GraphQL's escaping is applied by `apollo-parser` — the
/// same reading the group guards on a field are judged on
/// (`crate::service::graphql_api::user_pool`).
fn subscribed_mutations(directives: &[super::DirectiveUse]) -> Vec<String> {
    directives
        .iter()
        .filter(|directive| directive.name == SUBSCRIBE_DIRECTIVE)
        .flat_map(|directive| {
            directive
                .arguments
                .iter()
                .filter(|argument| argument.name == SUBSCRIBE_MUTATIONS_ARGUMENT)
                .flat_map(|argument| argument.strings.clone())
        })
        .collect()
}

/// The directive a schema links a subscription field to the mutations that publish to it
/// with, and the argument naming them.
const SUBSCRIBE_DIRECTIVE: &str = "aws_subscribe";
const SUBSCRIBE_MUTATIONS_ARGUMENT: &str = "mutations";

/// One built-in scalar as introspection's `__Type`: a scalar carries no member a type
/// system defines, so every member but its kind, its name and the description GraphQL gives
/// it ([`built_in_scalar_description`]) is null — the same rendering a DEFINED scalar of the
/// API gets.
fn built_in_scalar_json(name: &str) -> Value {
    json!({
        "kind": SCALAR_KIND,
        "name": name,
        "description": built_in_scalar_description(name),
        "fields": Value::Null,
        "inputFields": Value::Null,
        "interfaces": Value::Null,
        "enumValues": Value::Null,
        "possibleTypes": Value::Null,
    })
}
