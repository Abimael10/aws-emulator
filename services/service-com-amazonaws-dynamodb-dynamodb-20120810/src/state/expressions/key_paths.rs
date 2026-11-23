// SPDX-License-Identifier: CC0-1.0

//! What a document path may address once the addressed table's KEY SCHEMA is known: a
//! key attribute is a scalar, so no path may reach inside one.
//!
//! "KeyType ... HASH - partition key" over an `AttributeDefinition` whose
//! `AttributeType` is one of `S`, `N` or `B`
//! (`smithy_query(query="shape",shape="AttributeDefinition")`) — a key attribute is
//! declared as a scalar type, so it holds no map to look a name up in and no list to
//! index. A path that writes a `.` or a `[]` step after one therefore addresses a
//! location the table cannot hold, whatever any item holds: the request is refused
//! naming the key attribute it dereferenced.
//!
//! The rule needs the table's key schema, so it is the one path rule a request cannot be
//! held to before the table is resolved — the table-independent rules are settled where
//! the expression is read ([`super`]). The attributes a table DECLARES a type for are
//! exactly its key attributes, its own and its secondary indexes'
//! ([`Table::declared_type`]), which is what makes one reading of the schema cover an
//! index key as well as a table key.
//!
//! A read's FILTER is held to the key schema that read ADDRESSES instead — the table's own,
//! plus the named index's when the request states an `IndexName`
//! ([`check_scalar_filter_key_paths`]) — because a filter is decided over the entries the
//! read walks, and an attribute that is only another index's key is an ordinary attribute of
//! that walk.

use super::*;

/// AWS's fixed text for a path that reaches inside a key attribute, with the offending
/// key attribute's name after it.
const KEY_PATH_NOT_SCALAR_CLAUSE: &str =
    "Key attributes must be scalars; list random access '[]' and map lookup '.' are not \
     allowed: Key: ";

/// Hold every document path `paths` names to the key schema `table` declares, refusing
/// the first one that reaches INSIDE a key attribute.
///
/// The sentence names the key attribute and nothing of the member the path was read from
/// — a client matching it recognises the defect whichever member carried the path — and it
/// is stated BARE, outside the `<N> validation error(s) detected: ` envelope, because it
/// is a fact of the table's key schema rather than a violation of a rule over one member's
/// value.
pub fn check_scalar_key_paths<'p>(
    table: &Table,
    paths: impl IntoIterator<Item = &'p DocumentPath>,
) -> Result<(), RequestError> {
    for path in paths {
        if let Some(attribute) = dereferenced_key_attribute(table, path) {
            return Err(RequestError::Validation(format!(
                "{KEY_PATH_NOT_SCALAR_CLAUSE}{attribute}"
            )));
        }
    }
    Ok(())
}

/// Hold every document path a read's FILTER names to the key schema that read ADDRESSES:
/// the table's own, and the named secondary index's when the read is pointed at one.
///
/// A filter is decided over the entries the read WALKS, so the key attributes in force are
/// the ones of the key schema it walks — "A FilterExpression does not allow key attributes"
/// is about exactly those (`Query$FilterExpression`). An attribute that is only ANOTHER
/// index's key is an ordinary attribute of that walk: the read never addresses that index,
/// so no rule of its key schema is in force, and a path reaching inside the attribute is
/// held to nothing but the paths' own grammar.
///
/// `index_name` is the `IndexName` the request stated, and a name this table declares no
/// index for contributes nothing: resolving one is that read's own refusal, stated where the
/// index is resolved ([`resolve_index`]).
pub fn check_scalar_filter_key_paths<'p>(
    table: &Table,
    index_name: Option<&str>,
    paths: impl IntoIterator<Item = &'p DocumentPath>,
) -> Result<(), RequestError> {
    for path in paths {
        if path.single_attribute().is_some() {
            continue;
        }
        let attribute = path.attribute();
        if addresses_key_attribute(table, index_name, attribute) {
            return Err(RequestError::Validation(format!(
                "{KEY_PATH_NOT_SCALAR_CLAUSE}{attribute}"
            )));
        }
    }
    Ok(())
}

/// Whether `attribute` is a key attribute of the key schema a read of `table` through
/// `index_name` addresses: one of the table's own key attributes — an index entry carries
/// them too, so they are key attributes of an index read as well — or one of the named
/// index's.
fn addresses_key_attribute(table: &Table, index_name: Option<&str>, attribute: &str) -> bool {
    if table.is_table_key(attribute) {
        return true;
    }
    index_name.is_some_and(|name| {
        table.secondary_index(name).is_some_and(|index| {
            index
                .key_schema
                .iter()
                .any(|element| element.attribute_name == attribute)
        })
    })
}

/// Hold the document paths of a conditional WRITE's `ConditionExpression` to the key
/// schema its table declares ([`check_scalar_key_paths`]).
///
/// The three single-item writers read their condition for every fact of the REQUEST
/// before they look their table up, and this is the one fact of it the TABLE settles, so
/// each of them asks for it once the table is in hand. The expression has already parsed
/// by then, so the parse here cannot refuse anything the writer has not refused already;
/// what it is for is the TREE, whose paths the schema is read against.
pub fn check_scalar_condition_key_paths(
    condition_expression: Option<&str>,
    substitutions: Substitutions<'_>,
    table: &Table,
) -> Result<(), RequestError> {
    let Some(expression) = condition_expression else {
        return Ok(());
    };
    let condition = ParsedCondition::parse(CONDITION_EXPRESSION_MEMBER, expression, substitutions)?;
    check_scalar_key_paths(table, condition.document_paths())
}

/// The member a conditional write states its condition through, which is what its
/// expression is read as.
const CONDITION_EXPRESSION_MEMBER: &str = "ConditionExpression";

/// The key attribute `path` reaches INSIDE, or `None` for a path that reaches inside no
/// key attribute — one naming a whole attribute, and one that reaches inside an attribute
/// the table declares no key type for.
fn dereferenced_key_attribute<'p>(table: &Table, path: &'p DocumentPath) -> Option<&'p str> {
    // A path that IS one top-level attribute addresses that attribute itself, whatever
    // role the schema gives it: reading or comparing a key attribute is ordinary.
    if path.single_attribute().is_some() {
        return None;
    }
    let attribute = path.attribute();
    table.declared_type(attribute).map(|_| attribute)
}

impl ParsedCondition {
    /// Every document path this condition addresses, in the order the expression wrote
    /// them — what the paths of a condition are held to once the table is known
    /// ([`check_scalar_key_paths`]).
    pub fn document_paths(&self) -> Vec<&DocumentPath> {
        let mut paths = Vec::new();
        collect_node_paths(&self.node, &mut paths);
        paths
    }
}

/// Collect the document paths of one condition node into `paths`.
///
/// Recursion follows the parsed tree, whose depth the parser holds at
/// [`MAX_EXPRESSION_DEPTH`], so the number of frames is a constant of this service rather
/// than a function of the request.
fn collect_node_paths<'c>(node: &'c ConditionNode, paths: &mut Vec<&'c DocumentPath>) {
    match node {
        ConditionNode::All(nodes) | ConditionNode::Any(nodes) => {
            for node in nodes {
                collect_node_paths(node, paths);
            }
        }
        ConditionNode::Not(inner) => collect_node_paths(inner, paths),
        ConditionNode::Compare(left, _, right)
        | ConditionNode::BeginsWith(left, right)
        | ConditionNode::Contains(left, right) => {
            collect_operand_path(left, paths);
            collect_operand_path(right, paths);
        }
        ConditionNode::Between(value, low, high) => {
            for operand in [value, low, high] {
                collect_operand_path(operand, paths);
            }
        }
        ConditionNode::In(value, candidates) => {
            collect_operand_path(value, paths);
            for candidate in candidates {
                collect_operand_path(candidate, paths);
            }
        }
        ConditionNode::Exists(path, _) => paths.push(path),
        ConditionNode::AttributeType(path, operand) => {
            paths.push(path);
            collect_operand_path(operand, paths);
        }
    }
}

/// The path one operand addresses, when it addresses one: a literal value the request
/// carried addresses nothing.
fn collect_operand_path<'c>(operand: &'c Operand, paths: &mut Vec<&'c DocumentPath>) {
    match operand {
        Operand::Path(path) | Operand::Size(path) => paths.push(path),
        Operand::Value(_) => {}
    }
}

impl ParsedUpdate {
    /// Every top-level attribute this update's actions ADDRESS, whichever member stated
    /// them and whatever applying them would change.
    ///
    /// What it is for: "You cannot use UpdateItem to update any primary key attributes"
    /// (`UpdateItem$Key`), which is a rule about what an action ADDRESSES — a path that
    /// reaches inside a key attribute addresses that attribute, and an action that would
    /// leave its value exactly as it stands addresses it too — so the writer reads it off
    /// the update's own text rather than off the change it produced.
    pub fn stated_attributes(&self) -> BTreeSet<String> {
        self.actions
            .iter()
            .map(|action| action.path().attribute().to_string())
            .collect()
    }

    /// Every document path this update's actions address, which is what the addressed
    /// table's key schema holds them to ([`check_scalar_key_paths`]).
    pub fn stated_paths(&self) -> Vec<&DocumentPath> {
        self.actions.iter().map(UpdateAction::path).collect()
    }
}
