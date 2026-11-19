// SPDX-License-Identifier: CC0-1.0

//! The rules a domain create is admitted under, and the text each refusal carries.
//!
//! A CloudWatchOmni domain belongs to the ACCOUNT, not to the request: `ListDomains`'s
//! model doc reads "Returns the caller's domains: the account-scoped domain and the
//! organization-scoped domain, if either exists. At most two domains are returned", so
//! an account holds at most ONE account-scoped domain. A create that would mint a second
//! one is therefore a conflict with the domain the account already holds rather than a
//! second record, whatever name it asks for.
//!
//! The create is also idempotent by token — `clientToken` carries `@idempotencyToken`,
//! whose doc reads "Repeated requests with the same token return the original result
//! instead of creating a duplicate" — and the service admits the create only WITH that
//! token: a singleton an account cannot safely retry a create for is one a timed-out
//! caller can neither re-create nor tell apart from someone else's, so a request that
//! carries no token is refused before anything is minted.
//!
//! The model fixes the text of neither refusal, so each is recorded behavior and lives
//! here as ONE template, reached by every operation that applies the rule.

/// The message a create that carries no `clientToken` earns, naming the operation that
/// required one.
///
/// Fixed text apart from the operation's own name, which is the only thing that differs
/// between two operations stating it: the sentence is about the member the request left
/// out, so nothing else of the request reaches it.
pub fn client_token_required_message(operation: &str) -> String {
    format!("A clientToken is required for {operation}")
}

/// The message a create that would mint a second account-scoped domain earns.
///
/// Wholly fixed text: the conflict is with the ACCOUNT's domain rather than with either
/// name, so neither the stored domain nor the requested one is named.
pub const DOMAIN_ALREADY_EXISTS_FOR_ACCOUNT: &str = "A domain already exists for this account";

#[cfg(test)]
mod tests {
    use super::*;

    /// The token refusal names the operation it was required for and nothing else.
    #[test]
    fn the_token_refusal_names_the_operation_that_required_one() {
        // Rows span the operations of this model whose input declares an
        // `@idempotencyToken` member (`smithy_query(query="operation",
        // operation="CreateDomain,CreateDomainForOrganization")`), including one not yet
        // implemented, because the template takes the operation name verbatim.
        for (operation, expected) in [
            ("CreateDomain", "A clientToken is required for CreateDomain"),
            (
                "CreateDomainForOrganization",
                "A clientToken is required for CreateDomainForOrganization",
            ),
        ] {
            let actual = client_token_required_message(operation);
            assert_eq!(
                actual, expected,
                "case {operation:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
