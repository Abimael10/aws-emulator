// SPDX-License-Identifier: CC0-1.0

//! PLACING text on a denied topic: what a guardrail's topic policy reads a topic BY.
//!
//! A topic is configured as three things written about one subject — a `name`, a
//! `definition` and the sample `examples` it is asked about in
//! (`GuardrailTopicConfig`) — and Amazon Bedrock decides whether a prompt or an answer
//! is ON that subject. No language model runs in this emulator, so the three are read
//! as the VOCABULARY of the subject rather than as language to interpret: the terms
//! they are made of, with the ordinary function words of the language discounted and
//! each remaining term reduced to its stem, so a topic placed in one inflection is
//! placed in another.
//!
//! A term is weighed by WHERE the configuration puts it. A term in the topic's own
//! `name` is the subject itself — an operator names a topic by what it is about — a
//! term a sample phrase is made of is a form the subject is asked about in, and a term
//! only the prose of the `definition` carries is a word of the sentence around it. The
//! text is on the topic where the terms it shares with that vocabulary weigh
//! [`ON_TOPIC_WEIGHT`] or more: one term of the topic's name, two of a sample phrase,
//! or three of its definition.
//!
//! The vocabulary is read per topic and the submitted text ONCE for the whole policy
//! ([`submitted_terms`]), so an evaluation is linear in the text plus the
//! configuration, never in their product. Matching is over whole terms, so a longer
//! word that merely contains a configured one is not that word.

use std::collections::{BTreeMap, BTreeSet};

use super::guardrail_registry::DeniedTopic;

/// What a term of the topic's own `name` weighs: the subject itself.
pub const NAME_TERM_WEIGHT: u32 = 3;

/// What a term of one of the topic's sample phrases weighs: a form the subject is
/// asked about in.
pub const PHRASE_TERM_WEIGHT: u32 = 2;

/// What a term only the topic's `definition` carries weighs: a word of the prose
/// around the subject.
pub const DEFINITION_TERM_WEIGHT: u32 = 1;

/// The weight of shared terms at which text is ON the topic.
///
/// One term of the topic's own name reaches it, as do two terms of a sample phrase or
/// three words of the definition: the more the configuration commits a term to the
/// subject, the fewer of them place the text on it.
pub const ON_TOPIC_WEIGHT: u32 = 3;

/// The distinct content terms the submitted pieces of text are made of, together.
///
/// Read once per evaluation and consulted for every configured topic.
#[must_use]
pub fn submitted_terms(texts: &[String]) -> BTreeSet<String> {
    texts.iter().flat_map(|text| content_terms(text)).collect()
}

/// Whether text made of `terms` is on `topic`.
#[must_use]
pub fn on_topic(topic: &DeniedTopic, terms: &BTreeSet<String>) -> bool {
    if terms.is_empty() {
        return false;
    }
    let vocabulary = vocabulary_of(topic);
    let mut weight = 0u32;
    for term in terms {
        if let Some(term_weight) = vocabulary.get(term) {
            weight = weight.saturating_add(*term_weight);
            if weight >= ON_TOPIC_WEIGHT {
                return true;
            }
        }
    }
    false
}

/// The vocabulary one denied topic's configuration is, each term under what the
/// configuration commits it to.
///
/// A term the configuration states in more than one place carries the GREATEST of
/// those weights: a word of the definition that is also the topic's name is the
/// subject, not prose.
fn vocabulary_of(topic: &DeniedTopic) -> BTreeMap<String, u32> {
    let mut vocabulary: BTreeMap<String, u32> = BTreeMap::new();
    let mut weigh = |term: String, weight: u32| {
        vocabulary
            .entry(term)
            .and_modify(|held| *held = (*held).max(weight))
            .or_insert(weight);
    };
    for term in name_terms(&topic.name) {
        weigh(term, NAME_TERM_WEIGHT);
    }
    for example in &topic.examples {
        for term in content_terms(example) {
            weigh(term, PHRASE_TERM_WEIGHT);
        }
    }
    for term in content_terms(&topic.definition) {
        weigh(term, DEFINITION_TERM_WEIGHT);
    }
    vocabulary
}

/// The content terms of a topic's NAME.
///
/// A topic name is an identifier an operator writes rather than a sentence
/// (`GuardrailTopicName`, `^[0-9a-zA-Z-_ !?.]+$`), so the words it is made of are also
/// run together in the spellings identifiers are written in: the separators it admits
/// and the case changes inside it both begin a word. Its function words are discounted
/// like any other text's — a name stating the subject in a phrase says no more by the
/// phrasing.
fn name_terms(name: &str) -> BTreeSet<String> {
    name.split(|character: char| !character.is_alphanumeric())
        .flat_map(split_case)
        .filter_map(|word| content_term(&word))
        .collect()
}

/// One run of alphanumeric characters cut at the case changes inside it, so the words
/// of an identifier spelled in camel or Pascal case are each their own word.
///
/// A run carrying no case change — which is every ordinary word — is one word.
fn split_case(run: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut previous: Option<char> = None;
    for character in run.chars() {
        let boundary = character.is_uppercase()
            && previous.is_some_and(|earlier| earlier.is_lowercase() || earlier.is_numeric());
        if boundary && !word.is_empty() {
            words.push(::std::mem::take(&mut word));
        }
        word.push(character);
        previous = Some(character);
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The distinct content terms of one piece of text: its words, less the language's
/// function words, each reduced to its stem.
fn content_terms(text: &str) -> BTreeSet<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter_map(content_term)
        .collect()
}

/// One word's term, or `None` for a word that carries no subject of its own: a
/// function word of the language, or no word at all.
fn content_term(word: &str) -> Option<String> {
    if word.is_empty() {
        return None;
    }
    let lowercased = word.to_lowercase();
    if function_words().contains(&lowercased) {
        return None;
    }
    Some(stemmer().stem(&lowercased).into_owned())
}

/// The Snowball English stemmer, built once and shared.
///
/// A term is compared by its stem on BOTH sides, the configuration's and the text's,
/// so a topic configured about an "investment" is placed in text that "invests".
fn stemmer() -> &'static ::rust_stemmers::Stemmer {
    static STEMMER: ::std::sync::OnceLock<::rust_stemmers::Stemmer> = ::std::sync::OnceLock::new();
    STEMMER.get_or_init(|| ::rust_stemmers::Stemmer::create(::rust_stemmers::Algorithm::English))
}

/// The English function words, lowercased: the published list the `stop-words` crate
/// carries, rather than a list written out here. Built once and shared.
fn function_words() -> &'static BTreeSet<String> {
    static FUNCTION_WORDS: ::std::sync::OnceLock<BTreeSet<String>> = ::std::sync::OnceLock::new();
    FUNCTION_WORDS.get_or_init(|| {
        ::stop_words::get(::stop_words::LANGUAGE::English)
            .into_iter()
            .map(|word| word.to_lowercase())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::super::guardrail_registry::{PolicyAction, Sides, TOPIC_TYPE_DENY};
    use super::*;

    /// One configured topic, as a row below writes it.
    fn topic(name: &str, definition: &str, examples: &[&str]) -> DeniedTopic {
        DeniedTopic {
            name: name.to_string(),
            kind: TOPIC_TYPE_DENY.to_string(),
            definition: definition.to_string(),
            examples: examples.iter().map(|phrase| phrase.to_string()).collect(),
            sides: Sides::both(PolicyAction::Block),
        }
    }

    /// Whether `text` is on `topic`, through the one reading an evaluation does.
    fn placed(topic: &DeniedTopic, text: &str) -> bool {
        on_topic(topic, &submitted_terms(&[text.to_string()]))
    }

    /// RULE: a term of the topic's own NAME places the text on it, whatever spelling
    /// the name runs its words together in, and whatever inflection either side
    /// spells the term in.
    #[test]
    fn a_term_of_the_topics_name_places_the_text_on_it() {
        // (case, the topic name, the text, on topic)
        let rows: [(&str, &str, &str, bool); 9] = [
            (
                "a hyphenated name, one term carried",
                "sim2real-crypto-investment-advice",
                "Should I buy ethereum as an investment this month?",
                true,
            ),
            (
                "a hyphenated name, another term carried",
                "sim2real-crypto-investment-advice",
                "Tell me which crypto will go up.",
                true,
            ),
            (
                "a Pascal-case name, one term carried",
                "MedicalDosageAdvice",
                "How many milligrams of this medicine is a safe dosage for a child?",
                true,
            ),
            (
                "an underscored name",
                "weapon_manufacturing",
                "Explain how a weapon is made at home.",
                true,
            ),
            (
                "a spaced name",
                "Fiduciary Advice",
                "Can you give me fiduciary advice?",
                true,
            ),
            (
                "the name shouted",
                "Fiduciary Advice",
                "FIDUCIARY ADVICE please",
                true,
            ),
            (
                "a longer word that merely contains the name",
                "Fiduciary Advice",
                "fiduciaryadvice",
                false,
            ),
            (
                "nothing of the subject",
                "sim2real-crypto-investment-advice",
                "What is the capital of France?",
                false,
            ),
            (
                "nothing of the subject, another name",
                "MedicalDosageAdvice",
                "Summarize the water cycle in one sentence.",
                false,
            ),
        ];
        for (case, name, text, expected) in rows {
            let configured = topic(name, "", &[]);
            let actual = placed(&configured, text);
            assert_eq!(
                actual, expected,
                "case {case:?}: {text:?} against {name:?} expected on-topic={expected}, got \
                 {actual}"
            );
        }
    }

    /// RULE: a topic the text carries no term of the NAME of is still placed by the
    /// sample phrases it was configured with — two of their terms — and by its
    /// definition — three of its words — so a topic named by an opaque label is
    /// placed by what it was described as.
    #[test]
    fn the_sample_phrases_and_the_definition_place_the_topic_too() {
        let labelled = topic(
            "t-0417",
            "Any request for a recommendation about buying or selling a security.",
            &["Which shares should I pick for my pension?"],
        );
        // (case, the text, on topic)
        let rows: [(&str, &str, bool); 5] = [
            (
                "two terms of a sample phrase",
                "Which shares do I pick?",
                true,
            ),
            ("one term of a sample phrase alone", "Pick a number.", false),
            (
                "three words of the definition",
                "Give me a recommendation about selling this security.",
                true,
            ),
            (
                "one word of the definition alone",
                "What is a security camera?",
                false,
            ),
            ("nothing of the topic", "How tall is Mount Fuji?", false),
        ];
        for (case, text, expected) in rows {
            let actual = placed(&labelled, text);
            assert_eq!(
                actual, expected,
                "case {case:?}: {text:?} expected on-topic={expected}, got {actual}"
            );
        }
    }

    /// RULE: the function words of the language carry no subject, so text that shares
    /// only those with a topic's configuration is on no topic however many of them it
    /// shares.
    #[test]
    fn function_words_place_nothing() {
        let configured = topic(
            "t-0418",
            "Requests for help with filing a tax return.",
            &["How do I file my taxes?"],
        );
        // Shares "how", "do", "i", "to", "the", "my" and "with" with the configuration,
        // and no term of the subject at all.
        let text = "How do I get to the airport with my luggage?";
        let actual = placed(&configured, text);
        assert!(
            !actual,
            "case \"function words only\": {text:?} shares only function words, expected \
             on-topic=false, got {actual}"
        );
    }

    /// RULE: a topic configured with nothing to read — no name terms, no definition and
    /// no phrases — places nothing, and neither does text carrying no term.
    #[test]
    fn a_topic_with_no_vocabulary_places_nothing() {
        // (case, the topic, the text)
        let rows: [(&str, DeniedTopic, &str); 3] = [
            (
                "no vocabulary at all",
                topic("", "", &[]),
                "anything at all",
            ),
            (
                "a name of function words alone",
                topic("the of and", "", &[]),
                "the of and",
            ),
            ("no text", topic("Fiduciary Advice", "", &[]), "        "),
        ];
        for (case, configured, text) in rows {
            let actual = placed(&configured, text);
            assert!(
                !actual,
                "case {case:?}: expected on-topic=false, got {actual}"
            );
        }
    }
}
