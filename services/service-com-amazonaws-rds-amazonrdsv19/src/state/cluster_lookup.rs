// SPDX-License-Identifier: CC0-1.0

//! How a member that NAMES an existing DB cluster is read.
//!
//! The members that name a DB cluster to look up take "the user-supplied DB cluster
//! identifier or the Amazon Resource Name (ARN) of the DB cluster"
//! (`DescribeDBClustersMessage$DBClusterIdentifier`), so the reference a request carries is
//! reduced to the IDENTIFIER it names ([`db_cluster_identifier_of`]) before anything is done
//! with it — the handle the store is keyed by, and the handle a refusal names the cluster by.
//!
//! What this module adds is the GATE in front of that lookup. A reference whose identifier
//! is outside the grammar every DB cluster identifier is assigned under ("Must contain from 1
//! to 63 letters, numbers, or hyphens. First character must be a letter. Can't end with a
//! hyphen or contain two consecutive hyphens") cannot name a DB cluster at all — not one that
//! exists, and not one that ever could — so it is refused as a parameter VALUE the member
//! does not accept, with the subject wording this front door words a REFERENCE's refusals
//! with for every clause ([`check_identifier_reference`]), before any record is read. The
//! modelled
//! `DBClusterNotFoundFault` is then what it is modelled for: a WELL-FORMED identifier that
//! names nothing. The two refusals are different answers to a caller — a not-found invites a
//! create-or-retry, while a refused value says the request itself has to change — so a
//! request that can never succeed must not be answered with the one a caller waits on.
//!
//! One gate for every operation whose `DBClusterIdentifier` names the cluster the call
//! addresses, so none of them can drift from the others.

use super::*;

/// The member whose published rule every DB cluster reference is held to, and whose subject
/// wording its refusal carries ([`crate::state::check_identifier_reference`]).
///
/// One spelling of the member name for this crate: the pre-boundary reference gate
/// (`crate::service::unresolved_reference`) names the same member and reads it from here, so
/// the two cannot drift.
pub const DB_CLUSTER_IDENTIFIER_MEMBER: &str = "DBClusterIdentifier";

/// The longest identifier any stored DB cluster can carry: `DBClusterIdentifier` is "1 to 63
/// (for Aurora DB clusters) or 1 to 52 (for Multi-AZ DB clusters) letters, numbers, or
/// hyphens", so 63 is the widest of the two bounds the member publishes.
///
/// A LOOKUP is held to that widest bound rather than to the per-engine one
/// ([`db_cluster_identifier_max_len`]): which engine a reference would have belonged to is
/// not known before the cluster is resolved, and a reference inside the wide bound that no
/// cluster of its engine could carry simply resolves to nothing — which is the modelled
/// not-found answer, not a refused value.
pub const MAX_DB_CLUSTER_LOOKUP_IDENTIFIER_LEN: usize = 63;

/// The DB cluster identifier the reference `named` names — `named` itself, or the identifier
/// its ARN carries — or the sentence a reference no DB cluster identifier could be is refused
/// with.
///
/// The CALLER answers an `Err` with `InvalidParameterValue`, the Query API common error for a
/// value the parameter it was given for does not accept, and reports an `Ok` resolving to
/// nothing with the operation's own modelled not-found fault. The case of the identifier is
/// left exactly as the request spelled it: the members are documented "this parameter isn't
/// case-sensitive", so folding for the lookup is the store's business
/// ([`ComAmazonawsRdsAmazonrdsv19State::resolve_db_cluster`]) and a caller matching a refusal
/// against what it sent reads its own spelling back.
///
/// A reference carrying NO identifier at all — an empty value, or an ARN whose resource field
/// is empty, which `RdsDataApiConfig$resourceArn`'s published pattern admits — is not a value
/// the gate judges: there is nothing in it to be outside the grammar. It names no cluster, so
/// it resolves to nothing and keeps the operation's modelled not-found answer, which is what a
/// caller resolving such an ARN branches on.
pub fn db_cluster_lookup_identifier(named: &str) -> Result<&str, String> {
    let identifier = db_cluster_identifier_of(named);
    if identifier.is_empty() {
        return Ok(identifier);
    }
    check_identifier_reference(
        DB_CLUSTER_IDENTIFIER_MEMBER,
        identifier,
        MAX_DB_CLUSTER_LOOKUP_IDENTIFIER_LEN,
    )?;
    Ok(identifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference the gate admits is reduced to the identifier it names, whichever of the
    /// two published forms the request used and whichever case it was written in.
    #[test]
    fn an_admitted_reference_is_reduced_to_the_identifier_it_names() {
        // (case, the reference as sent, the identifier it names)
        let rows = [
            ("a plain identifier", "emu-cluster", "emu-cluster"),
            ("a single letter", "c", "c"),
            ("digits and hyphens", "emu-cluster-8-0", "emu-cluster-8-0"),
            ("mixed case, which the member is not sensitive to", "EMU-Cluster", "EMU-Cluster"),
            (
                "the longest identifier the member publishes",
                "aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffggg",
                "aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffggg",
            ),
            (
                "an ARN of the cluster",
                "arn:aws:rds:us-east-1:000000000000:cluster:emu-cluster",
                "emu-cluster",
            ),
            // A reference that carries no identifier at all names no cluster, and there is
            // nothing in it to judge against the grammar: it resolves to nothing, which is
            // the modelled not-found answer.
            ("the empty value", "", ""),
            (
                "an ARN whose resource field is empty",
                "arn:aws:rds:us-east-1:000000000000:cluster:",
                "",
            ),
        ];

        for (case, named, expected) in rows {
            let resolved = db_cluster_lookup_identifier(named);
            assert_eq!(
                resolved.as_deref(),
                Ok(expected),
                "case {case:?}: expected the identifier {expected:?}, got {resolved:?}"
            );
        }
    }

    /// A reference no DB cluster identifier could be is refused, with the subject wording
    /// this member carries over the value exactly as the request sent it.
    #[test]
    fn a_reference_outside_the_grammar_is_refused_by_its_subject() {
        let rows = [
            ("a character the alphabet excludes", "Bad_Name!"),
            ("a space inside the value", "emu cluster"),
            ("a leading digit", "1-emu-cluster"),
            ("a leading hyphen", "-emu-cluster"),
            ("a trailing hyphen", "emu-cluster-"),
            ("two consecutive hyphens", "emu--cluster"),
            (
                "one character past the widest published bound",
                "aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffgggg",
            ),
        ];

        for (case, named) in rows {
            let refused = db_cluster_lookup_identifier(named);
            assert_eq!(
                refused,
                Err(format!("Invalid database cluster identifier:  {named}")),
                "case {case:?}: expected the subject refusal, got {refused:?}"
            );
        }
    }
}
