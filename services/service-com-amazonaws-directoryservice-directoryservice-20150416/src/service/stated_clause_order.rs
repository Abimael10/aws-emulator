// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service states the clauses of one constraint refusal in.
//!
//! ## The rule
//!
//! A request that breaks the constraints of several members is refused ONCE, with a clause
//! per member inside the one counted envelope — and the order those clauses are stated in is
//! neither the order the model declares the members nor the order the request wrote them. It
//! is the order the layer in front of these services WALKS the findings it collected: the
//! findings are held in a table keyed on the NAME of the member each clause is about — the
//! member's own name, not the dotted path the clause states it by, so a member of a structure
//! the request nests is keyed by its own name there — and the envelope enumerates that table,
//! so a clause's position is decided by that name's own string hash alone.
//!
//! That is the same derivation this tree already reads for the two other orders that layer
//! exposes — the values of an enumerated type a clause lists and the entries of a submitted
//! map it quotes — so the bucket a member name sits in is taken from the shared one
//! ([`::service_common::submitted_collections::map_entry_bucket`], the 31-polynomial over the
//! key's UTF-16 code units with its high half folded into its low half), at the width that
//! table HOLDS whatever one refusal puts in it ([`FINDINGS_TABLE_BUCKETS`]). Nothing about a
//! clause's text is decided here: the member path, the echoed value, the requirement and the
//! envelope all stay the framework's, and only WHERE each clause sits in the message moves.
//!
//! What shows the order is a hash-table walk rather than the model's member order: a recorded
//! refusal of a Managed Microsoft AD create whose administrator password and whose directory
//! name BOTH break their declared expressions states the password clause first, although the
//! model declares the name member before the password — and `password` sits in an earlier
//! bucket than `name` does. What shows the key is the member's own NAME is a recorded refusal
//! of the same create whose nested VPC placement breaks two of its members: the clause about
//! the VPC comes before the one about the subnets, which is their order by the names of those
//! two members and the reverse of their order by the paths the clauses state them by. The
//! order is computed from the member names alone, so it holds for every operation and every
//! set of members of this service rather than for the sets a recording happens to cover.
//!
//! ## Where it is applied
//!
//! On every path this service renders a constraint envelope from: the boundary refusal
//! [`super::stated_refusal`] words, the clause stated at a list member
//! ([`super::list_element_refusals`]) and the clause for an expression the boundary validator
//! could not apply ([`super::model_pattern_refusals`]) — so two clauses of one refusal are
//! ordered the same way whichever of those collected them, and a refusal that mixes classes
//! is still ONE walk of ONE table.

use ::service_common::submitted_collections::map_entry_bucket;
use ::service_common::validation::list_of_element;
use ::smithy_server_runtime::Violation;

/// How wide the table holding one refusal's findings is: a CONSTANT of the layer holding
/// them, not a width sized by how many findings one request produced.
///
/// What shows it is a recorded refusal of a Managed Microsoft AD create breaking FOUR
/// members' expressions at once — its administrator password, its directory name, and both
/// members of the VPC placement it nests — which states them in the order
/// `password`, `vpcId`, `subnetIds`, `name`. Those four names sit in that order by bucket at
/// this width and in no other order the recorded refusals admit: at the sixteen buckets a
/// table filled with four findings would start at, `subnetIds` would sit after `name`, and at
/// a wider one `name` would sit before `vpcId`. The two recorded PAIRS — the password before
/// the name, the VPC id before the subnets — come out as recorded at this width too, so one
/// width states every recorded refusal of this service and the clause order stops depending
/// on how many members a request happens to break.
pub const FINDINGS_TABLE_BUCKETS: usize = 64;

/// `violations` in the order this service states their clauses: by the bucket the NAME of the
/// member each clause is about sits in, and within one bucket by that name.
///
/// STABLE, and over the member name alone, so two requirements of ONE member keep the order the
/// framework's walk collected them in (its `@pattern` before its bound, say) and only the
/// members move. The keys are computed once, in a single pass, rather than inside the
/// comparator, and the walk hands over at most its own ceiling of violations
/// ([`::service_common::validation::MAX_VIOLATIONS`]), so the work is bounded by that ceiling
/// rather than by one request.
pub fn in_stated_clause_order(violations: &[Violation]) -> Vec<&Violation> {
    let mut keyed: Vec<((usize, &str), &Violation)> = violations
        .iter()
        .map(|violation| {
            let member = member_name(violation);
            (
                (map_entry_bucket(member, FINDINGS_TABLE_BUCKETS), member),
                violation,
            )
        })
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, violation)| violation).collect()
}

/// The NAME of the member `violation`'s clause is about: the last step of the path the clause
/// states it by, so a member of a structure the request nests is keyed by its own name in that
/// structure. For a violation the framework found at a list ELEMENT the clause is about the
/// list member this service states it at ([`super::list_element_refusals`]), so that member's
/// name is the key rather than the element step's.
fn member_name(violation: &Violation) -> &str {
    let stated_at = list_of_element(&violation.path).unwrap_or(&violation.path);
    stated_at.rsplit('.').next().unwrap_or(stated_at)
}

/// Whether stating `violations` in this order MOVES any clause — read by a caller that only
/// rewords a refusal it has something to change about, so a request whose clauses the
/// framework already lists in this order keeps the framework's own answer untouched.
pub fn reorders(violations: &[Violation]) -> bool {
    in_stated_clause_order(violations)
        .iter()
        .zip(violations)
        .any(|(stated, found)| stated.path != found.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::smithy_server_runtime::ViolationKind;

    /// One violation of the member `path`, which is all this order reads.
    fn at(path: &str) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::Pattern("^.*$".to_string()),
            value: None,
            sensitive: false,
        }
    }

    /// RULE: the clauses come out in the order the table holding them is walked — a function of
    /// the member paths alone, so the order does not depend on the order the walk found them.
    #[test]
    fn the_clauses_come_out_in_the_tables_own_order() {
        // (case, the paths as the walk found them, the paths as the refusal states them)
        //
        // The rows differ along what the rule generalises over: the set of members, the order
        // the walk found them in, how many members the refusal is about, and a path with a
        // nested member in it. Each says whether its order is one a RECORDED refusal of this
        // service states or one the same rule PREDICTS for a member set no recording covers:
        // a change to [`FINDINGS_TABLE_BUCKETS`] may move a predicted row and must never move
        // a recorded one.
        let rows: [(&str, &[&str], &[&str]); 7] = [
            (
                // RECORDED.
                "the administrator password beside the directory name",
                &["name", "password"],
                &["password", "name"],
            ),
            (
                // RECORDED, found the other way round: the order is not the walk's.
                "the same two members, found the other way round",
                &["password", "name"],
                &["password", "name"],
            ),
            (
                "one member alone",
                &["name"],
                &["name"],
            ),
            (
                // PREDICTED.
                "three members of one create",
                &["name", "password", "shortName"],
                &["password", "name", "shortName"],
            ),
            (
                // RECORDED.
                "the two members of a nested placement",
                &["vpcSettings.subnetIds", "vpcSettings.vpcId"],
                &["vpcSettings.vpcId", "vpcSettings.subnetIds"],
            ),
            (
                // PREDICTED.
                "a nested member beside a plain one",
                &["vpcSettings.subnetIds", "name"],
                &["vpcSettings.subnetIds", "name"],
            ),
            // RECORDED: the four members a recorded refusal of one create states together,
            // found in the order the walk collects them (the input's own members first, then
            // the nested placement's): the password's clause first, the name's last.
            (
                "the four members of a create breaking every expression it declares",
                &[
                    "name",
                    "vpcSettings.vpcId",
                    "vpcSettings.subnetIds",
                    "password",
                ],
                &[
                    "password",
                    "vpcSettings.vpcId",
                    "vpcSettings.subnetIds",
                    "name",
                ],
            ),
        ];
        for (case, found, expected) in rows {
            let violations: Vec<Violation> = found.iter().map(|path| at(path)).collect();
            let stated: Vec<&str> = in_stated_clause_order(&violations)
                .into_iter()
                .map(|violation| violation.path.as_str())
                .collect();
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the clauses in {expected:?}, got {stated:?}"
            );
        }
    }

    /// RULE: two requirements of ONE member keep the order the walk collected them in — only
    /// the members move.
    #[test]
    fn two_requirements_of_one_member_keep_the_walks_order() {
        let violations = vec![
            Violation {
                path: "name".to_string(),
                kind: ViolationKind::Pattern("^first$".to_string()),
                value: None,
                sensitive: false,
            },
            Violation {
                path: "name".to_string(),
                kind: ViolationKind::Pattern("^second$".to_string()),
                value: None,
                sensitive: false,
            },
        ];
        let stated = in_stated_clause_order(&violations);
        assert_eq!(
            stated
                .iter()
                .map(|violation| violation.kind.clone())
                .collect::<Vec<_>>(),
            violations
                .iter()
                .map(|violation| violation.kind.clone())
                .collect::<Vec<_>>(),
            "expected the two requirements of one member in the order the walk found them"
        );
        assert!(
            !reorders(&violations),
            "expected a refusal about one member to move nothing"
        );
    }

    /// RULE: a refusal whose clauses are already in this order is not reworded at all.
    #[test]
    fn nothing_moves_where_the_walk_already_agrees() {
        for (case, paths, moves) in [
            ("the walk's order is the stated one", ["password", "name"], false),
            ("the walk's order is not", ["name", "password"], true),
        ] {
            let violations: Vec<Violation> = paths.iter().map(|path| at(path)).collect();
            assert_eq!(
                reorders(&violations),
                moves,
                "case {case:?}: expected reorders to answer {moves}"
            );
        }
    }
}
