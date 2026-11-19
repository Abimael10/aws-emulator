// SPDX-License-Identifier: CC0-1.0

//! The ONE reader of a request's QUERY STRING in this service.
//!
//! Several members of this model are bound to the query (`@httpQuery`) and are read before
//! the framework's own codec has run — the `resourceArn` the authorization decision is taken
//! against ([`super::authorization`]), the `targetArn` a command-execution request addresses
//! ([`super::command_execution_target`]) — so the decoding lives here rather than once per
//! reader: a percent-encoded value is read exactly the same way whichever decision reads it,
//! and exactly as the framework's codec reads it.

/// The value the query string of `uri` carries at `member`, or `None` when it carries none.
///
/// The decoding is `form_urlencoded`'s, the one query-string decoder this crate depends on.
/// A request with no query at all, and one whose query names another member, carry none.
pub fn value(uri: &str, member: &str) -> Option<String> {
    let query = uri.split_once('?').map(|(_path, query)| query)?;
    ::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _value)| name == member)
        .map(|(_name, value)| value.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the value read is the one the query carries at that member, whichever way a
    /// client encoded it, and nothing is read from a query that does not carry it.
    #[test]
    fn a_member_is_read_out_of_the_query_string() {
        let thing = "arn:aws:iot:us-east-1:000000000000:thing/laser-1";
        // (case, request URI, the member read, the value it carries)
        let cases: Vec<(&str, String, &str, Option<String>)> = vec![
            (
                "the parameter as an SDK client encodes it",
                format!(
                    "/command-executions/exec-1?{}",
                    ::form_urlencoded::Serializer::new(String::new())
                        .append_pair("targetArn", thing)
                        .finish()
                ),
                "targetArn",
                Some(thing.to_string()),
            ),
            (
                "the parameter unencoded",
                format!("/command-executions/exec-1?targetArn={thing}"),
                "targetArn",
                Some(thing.to_string()),
            ),
            (
                "the parameter beside another",
                format!("/command-executions/exec-1?includeResult=true&targetArn={thing}"),
                "targetArn",
                Some(thing.to_string()),
            ),
            (
                "a query naming another member only",
                "/command-executions/exec-1?includeResult=true".to_string(),
                "targetArn",
                None,
            ),
            (
                "no query at all",
                "/command-executions/exec-1".to_string(),
                "targetArn",
                None,
            ),
            (
                "an empty value",
                "/command-executions/exec-1?targetArn=".to_string(),
                "targetArn",
                Some(String::new()),
            ),
        ];
        for (case, uri, member, expected) in cases {
            let actual = value(&uri, member);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} at {member} of {uri:?}, got {actual:?}"
            );
        }
    }
}
