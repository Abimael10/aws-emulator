// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `methodSettings` key a patch pointer that carries the WHOLE method
//! path inside ONE reference token writes.
//!
//! The RULE under test, one level above any one call:
//!
//! > A `methodSettings` key is the method path the patch pointer SPELLED — "method paths
//! > defined as {resource_path}/{http_method} for an individual method override, or /\*/\* for
//! > overriding all methods in the stage" (`Stage$methodSettings`) — and the ONE separator of
//! > that path a key writes literally is the one before the HTTP method. A pointer that carries
//! > the whole method path in one reference token escapes that separator too, as
//! > `PatchOperation$path` requires of a separator inside a path name, and it is the LAST
//! > escape the token carries because an HTTP method carries none. So such a pointer spells
//! > the key whose resource half is written exactly as the request wrote it and whose method
//! > half follows one literal separator — the SAME key the pointer that gives the method its
//! > own reference token spells. Those two pointers therefore name ONE entry, while a pointer
//! > that spells the RESOURCE half differently names another, and a stage carrying one method
//! > under two keys cannot be updated until one of them is taken away.
//!
//! The rows span what the rule generalises over: the DEPTH of the resource path (the root, a
//! child, a nested one, a deeper one), a part that is a path parameter, a part carrying a
//! character the pointer leaves alone, and the wildcard that stands for every method of the
//! stage. Each row DERIVES its pointers and its expected keys from the resource path and the
//! verb, so a row proves the form rather than one remembered string.
//!
//! Every case drives the real wire path through [`Gateway`], this crate's one wire-test
//! harness, and reuses the stage and patch helpers of the suites beside it.

use serde_json::json;

use crate::patch_updates::{patch, patch_ok, replace};
use crate::rest_api_lifecycle::{assert_failure, create_api, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};

/// One row: the method whose settings the pointers of the row address.
struct Method {
    case: &'static str,
    /// The resource path, as `Resource$path` states one — beginning with a separator — or the
    /// wildcard path `Stage$methodSettings` documents for every resource of the stage.
    resource_path: &'static str,
    /// The HTTP method, as `PutMethod$httpMethod` takes one, or the wildcard for every method.
    http_method: &'static str,
}

const METHODS: &[Method] = &[
    Method {
        case: "the root resource",
        resource_path: "/",
        http_method: "GET",
    },
    Method {
        case: "a child resource",
        resource_path: "/pets",
        http_method: "POST",
    },
    Method {
        case: "a nested resource path",
        resource_path: "/pets/{petId}",
        http_method: "DELETE",
    },
    Method {
        case: "a deeply nested resource path",
        resource_path: "/a/b/c",
        http_method: "PUT",
    },
    Method {
        case: "a resource part carrying a hyphen",
        resource_path: "/order-items",
        http_method: "PATCH",
    },
    Method {
        case: "every method of the stage",
        resource_path: "/*",
        http_method: "*",
    },
];

/// The setting a ONE-token method path is followed by: one reference token, so the setting's
/// own `MethodSetting` member name is the only spelling that fits beside it.
const MEMBER_SETTING: (&str, &str) = ("metricsEnabled", "metricsEnabled");

/// The setting a method path spelled in its own tokens is followed by: the published
/// group-and-name path, and the `MethodSetting` member it writes.
const GROUPED_SETTING: (&str, &str) = ("caching/enabled", "cachingEnabled");

impl Method {
    /// This method's resource half with EVERY separator escaped `~1`, the spelling
    /// `PatchOperation$path` requires of a separator inside a path name.
    fn escaped_half(&self) -> String {
        self.resource_path.replace('/', "~1")
    }

    /// This method's resource half with its INNER separators escaped and its leading one left
    /// to the pointer's own — which opens an empty first reference token, so the root resource
    /// contributes no token of its own at all.
    fn own_separator_half(&self) -> String {
        let inner = self
            .resource_path
            .trim_start_matches('/')
            .replace('/', "~1");
        if inner.is_empty() {
            String::new()
        } else {
            format!("/{inner}")
        }
    }

    /// The pointer that carries the whole method path in ONE reference token: the escaped
    /// resource half, the escaped separator before the HTTP method, and then the setting's
    /// member name.
    fn one_token_pointer(&self) -> String {
        format!(
            "/{}~1{}/{}",
            self.escaped_half(),
            self.http_method,
            MEMBER_SETTING.0
        )
    }

    /// The pointer that gives the HTTP method a reference token of its OWN, after the escaped
    /// resource half.
    fn escaped_pointer(&self) -> String {
        format!(
            "/{}/{}/{}",
            self.escaped_half(),
            self.http_method,
            GROUPED_SETTING.0
        )
    }

    /// The key both of those pointers spell: the escaped resource half, one literal separator,
    /// and the HTTP method.
    fn escaped_key(&self) -> String {
        format!("{}/{}", self.escaped_half(), self.http_method)
    }

    /// The pointer whose resource half leaves its leading separator to the pointer's own — the
    /// spelling `MethodSetting$resourcePath` states a path in, and another spelling of the same
    /// method.
    fn own_separator_pointer(&self) -> String {
        format!(
            "/{}/{}/{}",
            self.own_separator_half(),
            self.http_method,
            GROUPED_SETTING.0
        )
    }

    /// The key that pointer spells.
    fn own_separator_key(&self) -> String {
        format!("{}/{}", self.own_separator_half(), self.http_method)
    }

    /// This method's resource half with its inner separators escaped and NO leading separator
    /// at all.
    fn bare_half(&self) -> String {
        self.resource_path
            .trim_start_matches('/')
            .replace('/', "~1")
    }

    /// The pointer whose resource half carries no leading separator.
    fn bare_pointer(&self) -> String {
        format!(
            "/{}/{}/{}",
            self.bare_half(),
            self.http_method,
            GROUPED_SETTING.0
        )
    }

    /// The key that pointer spells.
    fn bare_key(&self) -> String {
        format!("{}/{}", self.bare_half(), self.http_method)
    }

    /// Every OTHER spelling of this method's resource half, each with the pointer that spells
    /// it and the key it writes — the leading separator left to the pointer's own, and none at
    /// all. A method whose two other spellings write the ONE key (the root resource, whose
    /// half is empty either way) is listed once.
    fn other_spellings(&self) -> Vec<(&'static str, String, String)> {
        let mut spellings = vec![(
            "the leading separator left to the pointer's own",
            self.own_separator_pointer(),
            self.own_separator_key(),
        )];
        let bare = self.bare_key();
        if spellings.iter().all(|(_, _, key)| *key != bare) {
            spellings.push((
                "no leading separator at all",
                self.bare_pointer(),
                bare,
            ));
        }
        spellings
    }
}

/// The `methodSettings` keys a stage reports, sorted.
fn reported_keys(body: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = body["methodSettings"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

#[tokio::test]
async fn a_method_path_in_one_token_names_the_entry_its_own_tokens_name() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "one-token-method-path").await;

    for (index, method) in METHODS.iter().enumerate() {
        // A stage per row, so one row's entries never stand in for another's.
        let stage = format!("one{index}");
        create_deployed_stage(&gateway, &api, &stage).await;
        let uri = format!("/restapis/{api}/stages/{stage}");

        let one_token = method.one_token_pointer();
        let own_tokens = method.escaped_pointer();
        let key = method.escaped_key();
        let case = format!(
            "{} / UpdateStage({one_token} and {own_tokens})",
            method.case
        );
        let body = patch_ok(
            &gateway,
            &case,
            &uri,
            vec![replace(&one_token, "true"), replace(&own_tokens, "true")],
        )
        .await;

        // One entry, under the key both pointers spell — and both settings in it, so each
        // pointer wrote the entry the other one names.
        let reported = reported_keys(&body);
        assert_eq!(
            reported,
            vec![key.clone()],
            "case {case}: expected the one entry {key:?} both pointers spell, got {reported:?}"
        );
        for member in [MEMBER_SETTING.1, GROUPED_SETTING.1] {
            assert_eq!(
                body["methodSettings"][key.as_str()][member],
                json!(true),
                "case {case}: expected {member} under {key:?}, got {:?}",
                body["methodSettings"][key.as_str()]
            );
        }

        // And read back through the stage's own reader, so the key is the stage's rather than
        // the answer's.
        let read = get_stage(&gateway, &api, &stage).await;
        assert_eq!(
            read.body, body,
            "case {case}: GetStage must report what UpdateStage answered, got {:?} after {:?}",
            read.body, body
        );
    }
}

#[tokio::test]
async fn a_one_token_method_path_collides_with_another_spelling_of_its_resource_half() {
    let gateway = Gateway::new();

    for (index, method) in METHODS.iter().enumerate() {
        // An API per row: a stage count is bounded per API, and each spelling pair needs a
        // stage of its own, because a stage that reached this state answers every later
        // update with the same sentence.
        let (api, _root) = create_api(&gateway, &format!("one-token-duplicate-{index}")).await;
        let one_token = method.one_token_pointer();
        let escaped_key = method.escaped_key();
        for (position, (spelling, other_pointer, other_key)) in
            method.other_spellings().into_iter().enumerate()
        {
            let stage = format!("dup{position}");
            create_deployed_stage(&gateway, &api, &stage).await;
            let uri = format!("/restapis/{api}/stages/{stage}");

            // The sentence names the two colliding method paths, the escaped spelling first,
            // the account the stage belongs to and the API the request addressed — nothing
            // else varies.
            let expected = format!(
                "Update Stage failed due to different method settings with the same resource \
                 path and http method. Please remove one of the duplicate method settings for \
                 method paths: {escaped_key}, {other_key} for accountId: {} and restApiId: \
                 {api}",
                engine::emulator_account_id()
            );

            let case = format!(
                "{} / {spelling} / UpdateStage({one_token} and {other_pointer})",
                method.case
            );
            let answer = patch(
                &gateway,
                &uri,
                vec![
                    replace(&one_token, "true"),
                    replace(&other_pointer, "true"),
                ],
            )
            .await;
            assert_failure(&answer, 400, "BadRequestException", &case);
            assert_eq!(
                answer.body["message"].as_str(),
                Some(expected.as_str()),
                "case {case}: expected the message {expected:?}, got {:?}",
                answer.body["message"]
            );

            // The two entries the sentence asks the caller to remove one of are there: the two
            // spellings were kept apart rather than folded into one.
            let read = get_stage(&gateway, &api, &stage).await;
            let mut both = vec![escaped_key.clone(), other_key.clone()];
            both.sort();
            let reported = reported_keys(&read.body);
            assert_eq!(
                reported, both,
                "case {case}: expected the stage to report both spellings {both:?}, got \
                 {reported:?}"
            );
        }
    }
}
