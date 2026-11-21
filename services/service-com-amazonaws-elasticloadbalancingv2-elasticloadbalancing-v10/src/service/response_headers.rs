// SPDX-License-Identifier: CC0-1.0

//! The response headers an Application Load Balancer's LISTENER sends or removes on the
//! answer the client receives, out of that listener's own attribute collection.
//!
//! `ListenerAttribute$Key` carries them, all "only supported by Application Load
//! Balancers": one switch over the header the load balancer names itself in
//! ("`routing.http.response.server.enabled` - Enables you to allow or remove the HTTP
//! response server header"), and a family of `routing.http.response.<header>.header_value`
//! attributes, each documented as the value of one response header the browser reads —
//! `strict_transport_security` ("Informs browsers that the site should only be accessed
//! using HTTPS"), the `access_control_*` set of a cross-origin answer, the
//! `content_security_policy`, the `x_content_type_options` and the `x_frame_options`.
//!
//! So the rule is one rule, read off the attribute KEY rather than written out per header:
//! an attribute under [`RESPONSE_HEADER_PREFIX`] ending in [`HEADER_VALUE_SUFFIX`] names the
//! response header its middle spells ([`configured_header`]), and the value the listener
//! carries it at is what that header is sent with. A listener carrying such an attribute at
//! the empty string — the value the collection reports until a caller writes one — sends no
//! header at all, which is what a load balancer nobody configured one on sends. Every
//! attribute the catalog grows is covered the moment the control plane carries it, because
//! nothing here names a header of its own.
//!
//! The `Server` header is the one attribute that is not a value: the header is the LOAD
//! BALANCER's own ([`LOAD_BALANCER_SERVER`]), sent while the attribute allows it and absent
//! once a caller removes it. Either way it is the load balancer's answer that decides it, so
//! whatever a target returned under that name is replaced rather than passed on.
//!
//! Applied where the answer LEAVES the load balancer ([`super::data_plane::serve`]), to
//! every answer the listener sends — the response of a target, and the status the load
//! balancer produced itself — because the headers are the listener's, not the action's. The
//! attributes are read through the ONE catalog of listener attributes
//! ([`crate::state::LISTENER_ATTRIBUTES`]), so what `DescribeListenerAttributes` reports and
//! what a client is answered with cannot drift, and nothing here writes: serving a request
//! changes no attribute.

/// The attributes that configure one response header each: the prefix their keys share with
/// nothing else of the collection (the mutual-TLS attributes beside them rename a REQUEST
/// header, under `routing.http.request.`), and the suffix that says the attribute holds that
/// header's VALUE.
const RESPONSE_HEADER_PREFIX: &str = "routing.http.response.";
const HEADER_VALUE_SUFFIX: &str = ".header_value";

/// The attribute that decides whether the load balancer sends its own `Server` header:
/// "`routing.http.response.server.enabled` - Enables you to allow or remove the HTTP
/// response server header."
const SERVER_ENABLED_ATTRIBUTE: &str = "routing.http.response.server.enabled";

/// The response header that switch is about.
const SERVER_HEADER: &str = "server";

/// What an Application Load Balancer names itself in the `Server` header of the answers it
/// sends — its own product and major version, the one Elastic Load Balancing's v2 load
/// balancers report.
const LOAD_BALANCER_SERVER: &str = "awselb/2.0";

/// The response header the attribute `key` configures, or `None` for a key that configures
/// none: the middle of a `routing.http.response.<header>.header_value` key, with the
/// underscores it is written with as the hyphens a header name carries.
///
/// The attribute keys are spelled in the snake case every attribute of the collection is
/// spelled in, and the headers they configure are the same words hyphenated
/// (`x_frame_options` is `X-Frame-Options`), so one reading covers every attribute of the
/// family. A key whose middle is empty names no header.
fn configured_header(key: &str) -> Option<String> {
    let middle = key
        .strip_prefix(RESPONSE_HEADER_PREFIX)?
        .strip_suffix(HEADER_VALUE_SUFFIX)?;
    (!middle.is_empty()).then(|| middle.replace('_', "-"))
}

/// Whether an attribute value says the switch is on, in the words a boolean attribute of
/// this service is written in.
fn switched_on(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

/// Send `name` with `value` on this answer, replacing whatever it already carried under that
/// name: the header is the load balancer's, so one value of it travels.
fn sent(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    removed(headers, name);
    headers.push((name.to_string(), value.to_string()));
}

/// Take `name` off this answer, however the hop that produced it spelled the name.
fn removed(headers: &mut Vec<(String, String)>, name: &str) {
    headers.retain(|(held, _value)| !held.eq_ignore_ascii_case(name));
}

/// Apply one listener's response-header attributes to the headers of the answer it is
/// sending: every configured header value sent, and the load balancer's own `Server` header
/// sent or removed as the switch says.
///
/// `attributes` is that listener's WHOLE attribute collection
/// ([`crate::state::ListenerRecord::effective_attributes`]) — each attribute at the value a
/// caller wrote or the value the collection carries it at — so a listener of a family that
/// carries none of these attributes (a Network or Gateway Load Balancer's) has nothing
/// applied, and the answer leaves exactly as the action produced it.
pub(super) fn applied(headers: &mut Vec<(String, String)>, attributes: &[(String, String)]) {
    for (key, value) in attributes {
        if key == SERVER_ENABLED_ATTRIBUTE {
            match switched_on(value) {
                true => sent(headers, SERVER_HEADER, LOAD_BALANCER_SERVER),
                false => removed(headers, SERVER_HEADER),
            }
            continue;
        }
        let Some(header) = configured_header(key) else {
            continue;
        };
        // The empty string is what the collection carries a header-value attribute at until
        // a caller writes one, and a header with no value is no header: such a listener
        // sends none.
        if value.trim().is_empty() {
            continue;
        }
        sent(headers, &header, value);
    }
}

#[cfg(test)]
mod tests {
    //! The reading of an attribute key and the rewriting of one answer's headers. What a
    //! CLIENT is answered is pinned end to end in
    //! `tests/all/alb_listener_response_headers.rs`.

    use super::*;

    /// RULE: a `routing.http.response.<header>.header_value` attribute names the response
    /// header its middle spells, and nothing else of the collection names one.
    #[test]
    fn an_attribute_key_names_the_header_it_configures() {
        // The rows are the attribute keys the catalog carries
        // (`crate::state::LISTENER_ATTRIBUTES`, from `ListenerAttribute$Key`): one of each
        // word shape of the response family, the switch beside them, a request attribute
        // that renames a header of the other direction, an attribute of another load
        // balancer family, and the degenerate keys.
        let rows: &[(&str, Option<&str>)] = &[
            (
                "routing.http.response.strict_transport_security.header_value",
                Some("strict-transport-security"),
            ),
            (
                "routing.http.response.access_control_allow_origin.header_value",
                Some("access-control-allow-origin"),
            ),
            (
                "routing.http.response.access_control_max_age.header_value",
                Some("access-control-max-age"),
            ),
            (
                "routing.http.response.content_security_policy.header_value",
                Some("content-security-policy"),
            ),
            (
                "routing.http.response.x_content_type_options.header_value",
                Some("x-content-type-options"),
            ),
            (
                "routing.http.response.x_frame_options.header_value",
                Some("x-frame-options"),
            ),
            ("routing.http.response.server.enabled", None),
            (
                "routing.http.request.x_amzn_mtls_clientcert.header_name",
                None,
            ),
            ("tcp.idle_timeout.seconds", None),
            ("routing.http.response..header_value", None),
            ("", None),
        ];
        for (key, expected) in rows {
            let named = configured_header(key);
            assert_eq!(
                named.as_deref(),
                *expected,
                "case {key:?}: expected {expected:?}, got {named:?}"
            );
        }
    }

    /// RULE: a configured header is sent with the value the listener carries, one header per
    /// name whatever the answer already carried, and an attribute at the empty string sends
    /// nothing.
    #[test]
    fn the_configured_headers_replace_what_the_answer_carried() {
        let attributes: Vec<(String, String)> = [
            ("routing.http.response.server.enabled", "true"),
            (
                "routing.http.response.strict_transport_security.header_value",
                "max-age=31536000; includeSubDomains",
            ),
            ("routing.http.response.x_frame_options.header_value", ""),
            (
                "routing.http.response.access_control_allow_origin.header_value",
                "https://example.com",
            ),
        ]
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
        let mut headers: Vec<(String, String)> = [
            ("Server", "gunicorn/20.1.0"),
            ("x-app", "kept"),
            ("Strict-Transport-Security", "max-age=1"),
        ]
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
        applied(&mut headers, &attributes);
        let sent = |name: &str| -> Vec<&str> {
            headers
                .iter()
                .filter(|(held, _)| held.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
                .collect()
        };
        let rows: &[(&str, Vec<&str>)] = &[
            ("server", vec![LOAD_BALANCER_SERVER]),
            (
                "strict-transport-security",
                vec!["max-age=31536000; includeSubDomains"],
            ),
            ("access-control-allow-origin", vec!["https://example.com"]),
            ("x-frame-options", Vec::new()),
            ("x-app", vec!["kept"]),
        ];
        for (name, expected) in rows {
            let answered = sent(name);
            assert_eq!(
                &answered, expected,
                "case {name:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// RULE: the switch at `false` takes the header off the answer, whatever produced it.
    #[test]
    fn the_server_header_is_removed_once_the_switch_is_off() {
        let attributes: Vec<(String, String)> =
            vec![(SERVER_ENABLED_ATTRIBUTE.to_string(), "false".to_string())];
        let mut headers: Vec<(String, String)> = [("SERVER", "nginx"), ("server", "awselb/2.0")]
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        applied(&mut headers, &attributes);
        assert!(
            headers.is_empty(),
            "case the switch off: expected no server header left, got {headers:?}"
        );
    }

    /// RULE: a listener carrying none of these attributes — one of a family that does not
    /// have them — leaves the answer exactly as the action produced it.
    #[test]
    fn a_listener_with_no_such_attribute_changes_nothing() {
        let mut headers: Vec<(String, String)> = vec![("Server".to_string(), "nginx".to_string())];
        applied(&mut headers, &[("tcp.idle_timeout.seconds".to_string(), "350".to_string())]);
        assert_eq!(
            headers,
            vec![("Server".to_string(), "nginx".to_string())],
            "case a listener with no response attribute: expected the answer untouched, got \
             {headers:?}"
        );
    }
}
