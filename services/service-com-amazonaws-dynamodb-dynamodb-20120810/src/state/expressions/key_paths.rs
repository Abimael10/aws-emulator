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
