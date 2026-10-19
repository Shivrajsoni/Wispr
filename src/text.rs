//! Deterministic post-processing of transcripts.
//!
//! Steps, in order: remove filler words -> resolve spoken self-corrections -> apply the user's
//! vocabulary -> capitalize -> per-app formatting -> trailing space.
//!
//! This replaces an LLM "refiner". Because nothing here generates language, a dictated question
//! or command is always typed back verbatim; it can never be answered or executed. The cost is
//! that self-corrections are resolved by explicit rules (see [`resolve_corrections`]) and
//! ambiguous cases are deliberately left untouched rather than guessed.

use crate::config::OutputConfig;
use crate::context::AppClass;
use std::collections::BTreeMap;

pub struct PostProcessor {
    /// (lowercase variant, canonical), longest variant first.
    vocabulary: Vec<(String, String)>,
    resolve_corrections: bool,
    trailing_space: bool,
    casual_chat: bool,
}

impl PostProcessor {
    pub fn new(vocabulary: &BTreeMap<String, Vec<String>>, output: &OutputConfig) -> Self {
        let mut pairs: Vec<(String, String)> = vocabulary
            .iter()
            .flat_map(|(canonical, variants)| {
                variants
                    .iter()
                    .filter(|v| !v.trim().is_empty())
                    .map(move |v| (v.trim().to_ascii_lowercase(), canonical.clone()))
            })
            .collect();
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Self {
            vocabulary: pairs,
            resolve_corrections: output.resolve_corrections,
            trailing_space: output.trailing_space,
            casual_chat: output.casual_chat,
        }
    }

    pub fn process(&self, raw: &str, app: AppClass) -> String {
        let mut tokens: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
        remove_fillers(&mut tokens);
        if self.resolve_corrections {
            resolve_corrections(&mut tokens);
        }
        let mut text = apply_vocabulary(&tokens.join(" "), &self.vocabulary);
        if text.is_empty() {
            return text;
        }
        if app != AppClass::Terminal {
            text = capitalize(&text);
        }

        let single_sentence = !text
            .trim_end_matches(['.', '?', '!'])
            .contains(['.', '?', '!']);
        let strip_period = match app {
            AppClass::Terminal => true,
            AppClass::Chat => self.casual_chat,
            AppClass::Code | AppClass::Prose => false,
        };
        if strip_period && single_sentence && text.ends_with('.') && !text.ends_with("..") {
            text.pop();
        }
        if self.trailing_space && app != AppClass::Terminal {
            text.push(' ');
        }
        text
    }
}

const FILLERS: &[&str] = &[
    "um", "umm", "uh", "uhh", "uhm", "er", "erm", "ah", "hmm", "mm", "mmm",
];

pub fn remove_fillers(tokens: &mut Vec<String>) {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut capitalize_next = false;
    for tok in tokens.drain(..) {
        if FILLERS.contains(&norm(&tok).as_str()) {
            let at_start = out.last().is_none_or(|p| ends_sentence(p));
            if at_start && tok.starts_with(char::is_uppercase) {
                capitalize_next = true;
            }
            // "It's ready, um." -> keep the sentence end the filler was carrying.
            if let Some(end) = tok.chars().last().filter(|c| matches!(c, '.' | '?' | '!'))
                && let Some(prev) = out.last_mut()
                && !ends_sentence(prev)
            {
                while prev.ends_with([',', ';', ':']) {
                    prev.pop();
                }
                prev.push(end);
            }
            continue;
        }
        out.push(if capitalize_next {
            capitalize_next = false;
            capitalize(&tok)
        } else {
            tok
        });
    }
    *tokens = out;
}

#[derive(Clone, Copy, PartialEq)]
enum Marker {
    /// Replace the corrected phrase with what follows.
    Replace { needs_boundary: bool },
    /// Delete the current sentence.
    Scratch,
}

/// Longest markers first. "Weak" markers also occur in normal speech ("there's no actually good
/// option"), so they only count right after a punctuation boundary.
const MARKERS: &[(&[&str], Marker)] = &[
    (
        &["sorry", "i", "mean"],
        Marker::Replace {
            needs_boundary: false,
        },
    ),
    (
        &["no", "i", "mean"],
        Marker::Replace {
            needs_boundary: false,
        },
    ),
    (&["scratch", "that"], Marker::Scratch),
    (
        &["wait", "no"],
        Marker::Replace {
            needs_boundary: false,
        },
    ),
    (
        &["no", "wait"],
        Marker::Replace {
            needs_boundary: false,
        },
    ),
    (
        &["or", "rather"],
        Marker::Replace {
            needs_boundary: false,
        },
    ),
    (
        &["i", "meant"],
        Marker::Replace {
            needs_boundary: false,
        },
    ),
    (
        &["no", "actually"],
        Marker::Replace {
            needs_boundary: true,
        },
    ),
    (
        &["actually", "no"],
        Marker::Replace {
            needs_boundary: true,
        },
    ),
    (
        &["actually"],
        Marker::Replace {
            needs_boundary: true,
        },
    ),
];

/// How far back (in words) a correction may reach.
const WINDOW: usize = 10;

/// Resolves spoken self-corrections, e.g.
/// "meet at four, no actually five" -> "meet at five",
/// "the DNS resolver, wait no, the BitTorrent client" -> "the BitTorrent client",
/// "deploy on port 8080, wait no, port 3000" -> "deploy on port 3000",
/// "send it to John, no actually Mike" -> "send it to Mike",
/// "The meeting is at 3. Scratch that. See you Monday." -> "See you Monday."
///
/// The corrected phrase is located by, in order: the same leading word ("port", "the", "use"),
/// a number for a number ("four" -> "five", "3pm" -> "4pm"), or a 1-3 word replacement of the
/// words right before the marker. If none applies the text is left as spoken.
pub fn resolve_corrections(tokens: &mut Vec<String>) {
    let mut start = 0;
    // Each successful pass shortens the text, so this terminates; the cap is belt and braces.
    for _ in 0..16 {
        let Some(next) = resolve_one(tokens, start) else {
            return;
        };
        start = next;
    }
}

/// Resolves the first correction at or after `from`; returns where to continue scanning.
fn resolve_one(tokens: &mut Vec<String>, from: usize) -> Option<usize> {
    let norms: Vec<String> = tokens.iter().map(|t| norm(t)).collect();
    for m in from..tokens.len() {
        let Some((len, kind)) = MARKERS.iter().find_map(|(words, kind)| {
            let end = m + words.len();
            (end <= norms.len() && norms[m..end].iter().zip(words.iter()).all(|(a, b)| a == b))
                .then_some((words.len(), *kind))
        }) else {
            continue;
        };

        let rest = m + len;
        match kind {
            Marker::Scratch => {
                let sentence_start = if m == 0 {
                    0
                } else {
                    (0..m - 1)
                        .rev()
                        .find(|&i| ends_sentence(&tokens[i]))
                        .map_or(0, |i| i + 1)
                };
                tokens.drain(sentence_start..rest);
                let starts_sentence =
                    sentence_start == 0 || ends_sentence(&tokens[sentence_start - 1]);
                if starts_sentence && let Some(t) = tokens.get_mut(sentence_start) {
                    *t = capitalize(t);
                }
                return Some(sentence_start);
            }
            Marker::Replace { needs_boundary } => {
                if m == 0 || rest >= tokens.len() {
                    continue;
                }
                let before = &tokens[m - 1];
                // Weak markers after a question are an answer, not a correction:
                // "Is it done? No, actually it isn't." Strong ones ("wait no") still apply, since
                // speech-to-text often punctuates "the DNS resolver? Wait, no, ..." that way.
                if needs_boundary
                    && (before.ends_with('?')
                        || !before.ends_with([',', '.', ';', ':', '!', '-', '—']))
                {
                    continue;
                }
                let Some((anchor, keep_case)) =
                    find_anchor(tokens, &norms, m, rest, needs_boundary)
                else {
                    continue;
                };

                let mut replacement: Vec<String> = tokens.drain(rest..).collect();
                tokens.truncate(anchor);
                let at_sentence_start = anchor == 0 || ends_sentence(&tokens[anchor - 1]);
                let first = &replacement[0];
                replacement[0] = if at_sentence_start {
                    capitalize(first)
                } else if keep_case {
                    lowercase_first_unless_acronym(first)
                } else {
                    first.clone()
                };
                tokens.extend(replacement);
                return Some(anchor);
            }
        }
    }
    None
}

/// Finds where the corrected phrase starts. The bool says whether the replacement's first word
/// should take on lowercase (it only got a capital from following "No." / "Actually,").
fn find_anchor(
    tokens: &[String],
    norms: &[String],
    m: usize,
    rest: usize,
    weak: bool,
) -> Option<(usize, bool)> {
    // A weak marker never reaches back past a question ("Is it done? No, actually it isn't.").
    let after_question = if weak {
        (0..m)
            .rev()
            .find(|&i| tokens[i].ends_with('?'))
            .map_or(0, |i| i + 1)
    } else {
        0
    };
    let lo = m.saturating_sub(WINDOW).max(after_question);
    let first = &norms[rest];

    if let Some(i) = (lo..m).rev().find(|&i| &norms[i] == first) {
        return Some((i, tokens[i].starts_with(char::is_lowercase)));
    }
    if is_number(first)
        && let Some(i) = (lo..m).rev().find(|&i| is_number(&norms[i]))
    {
        return Some((i, true));
    }
    let head = tokens[rest..]
        .iter()
        .position(|t| t.ends_with([',', '.', ';', ':', '!', '?']))
        .map_or(tokens.len() - rest, |p| p + 1);
    if head <= 3 && head <= m && m - head >= lo {
        let anchor = m - head;
        let crosses_sentence = (anchor..m - 1).any(|i| ends_sentence(&tokens[i]));
        if !crosses_sentence {
            return Some((anchor, false));
        }
    }
    None
}

const NUMBER_WORDS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
    "thirty",
    "forty",
    "fifty",
    "sixty",
    "seventy",
    "eighty",
    "ninety",
    "hundred",
    "thousand",
    "million",
    "noon",
    "midnight",
    "half",
];

fn is_number(w: &str) -> bool {
    w.starts_with(|c: char| c.is_ascii_digit()) || NUMBER_WORDS.contains(&w)
}

/// Lowercased word with surrounding punctuation removed ("Actually," -> "actually").
fn norm(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

fn ends_sentence(token: &str) -> bool {
    token.ends_with(['.', '?', '!'])
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn lowercase_first_unless_acronym(s: &str) -> String {
    let letters: Vec<char> = s.chars().filter(|c| c.is_alphabetic()).collect();
    let acronym = letters.len() > 1 && letters.iter().all(|c| c.is_uppercase());
    if acronym || s == "I" || s.starts_with("I'") {
        return s.to_string();
    }
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Case-insensitive, whole-word replacement of vocabulary variants with their canonical form.
pub fn apply_vocabulary(text: &str, pairs: &[(String, String)]) -> String {
    let mut text = text.to_string();
    for (variant, canonical) in pairs {
        let mut from = 0;
        loop {
            // ASCII lowercasing keeps byte offsets identical to `text`.
            let lower = text.to_ascii_lowercase();
            let Some(pos) = lower[from..].find(variant.as_str()).map(|p| p + from) else {
                break;
            };
            let end = pos + variant.len();
            let bounded_left = text[..pos]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            let bounded_right = text[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            if bounded_left && bounded_right {
                text.replace_range(pos..end, canonical);
                from = pos + canonical.len();
            } else {
                from = end;
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(s: &str) -> String {
        let mut t: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        remove_fillers(&mut t);
        resolve_corrections(&mut t);
        t.join(" ")
    }

    #[test]
    fn fillers() {
        assert_eq!(fix("Um, so let's go."), "So let's go.");
        assert_eq!(fix("we uh need to, um, ship it"), "we need to, ship it");
        assert_eq!(fix("It's ready, um."), "It's ready.");
        assert_eq!(fix("um uh"), "");
    }

    #[test]
    fn number_corrections() {
        assert_eq!(
            fix("So let's meet at four. No. Actually, five."),
            "So let's meet at five."
        );
        assert_eq!(fix("meet at 4pm wait no 5pm"), "meet at 5pm");
        assert_eq!(
            fix("Let's meet at 4, no, actually 5, and bring the docs."),
            "Let's meet at 5, and bring the docs."
        );
    }

    #[test]
    fn same_word_corrections() {
        assert_eq!(
            fix(
                "can you review the pull request for the dns resolver wait no the bittorrent client"
            ),
            "can you review the pull request for the bittorrent client"
        );
        assert_eq!(
            fix("deploy on port 8080, wait no, port 3000 instead."),
            "deploy on port 3000 instead."
        );
        assert_eq!(
            fix("Use a mutex, actually no, use an RwLock."),
            "Use an RwLock."
        );
        assert_eq!(
            fix("We use Postgres. The cache is Redis. Wait, no, the cache is Memcached."),
            "We use Postgres. The cache is Memcached."
        );
    }

    #[test]
    fn strong_marker_after_question_mark() {
        assert_eq!(
            fix(
                "UM, can you review the pull request for the DNS resolver? Wait, no, the BitTorrent client? And how do we optimize the database query?"
            ),
            "Can you review the pull request for the BitTorrent client? And how do we optimize the database query?"
        );
    }

    #[test]
    fn short_replacements() {
        assert_eq!(
            fix("Send it to John, no actually Mike."),
            "Send it to Mike."
        );
        assert_eq!(
            fix("It's on Tuesday, sorry I mean Wednesday."),
            "It's on Wednesday."
        );
    }

    #[test]
    fn scratch_that() {
        assert_eq!(
            fix("The meeting is at 3. Scratch that. See you Monday."),
            "See you Monday."
        );
        assert_eq!(
            fix("Hello team. The meeting is at 3, scratch that, see you Monday."),
            "Hello team. See you Monday."
        );
    }

    #[test]
    fn leaves_normal_speech_alone() {
        for s in [
            "I actually think we should go.",
            "There's no actually good option here.",
            "Is it done? No, actually it isn't.",
            "That works. Actually, I think we should wait until Friday.",
            "No, I don't think so.",
            "I mean it.",
        ] {
            assert_eq!(fix(s), s, "changed: {s}");
        }
    }

    #[test]
    fn dictated_questions_and_commands_stay_verbatim() {
        let p = PostProcessor::new(&BTreeMap::new(), &OutputConfig::default());
        for s in [
            "How do we optimize the database query?",
            "Delete all the files in the build directory.",
            "Write me a poem about the ocean.",
            "What is the difference between a Mutex and an RwLock in Rust?",
        ] {
            assert_eq!(p.process(s, AppClass::Prose).trim_end(), s);
        }
    }

    #[test]
    fn vocabulary_is_whole_word_and_case_insensitive() {
        let mut v = BTreeMap::new();
        v.insert(
            "PostgreSQL".to_string(),
            vec!["postgres".to_string(), "post gres".to_string()],
        );
        v.insert("Kubernetes".to_string(), vec!["cube netties".to_string()]);
        let p = PostProcessor::new(&v, &OutputConfig::default());
        let out = p.process(
            "Move postgres and Post Gres to cube netties. PostgreSQL stays.",
            AppClass::Code,
        );
        assert_eq!(
            out,
            "Move PostgreSQL and PostgreSQL to Kubernetes. PostgreSQL stays. "
        );
        // Must not corrupt a correct transcription (the old substring matcher produced "PostgreSQLQL").
        assert_eq!(p.process("PostgreSQL", AppClass::Code), "PostgreSQL ");
    }

    #[test]
    fn code_jargon_survives() {
        let p = PostProcessor::new(&BTreeMap::new(), &OutputConfig::default());
        let s = "Wrap the HashMap in an Arc<RwLock<_>> and call tokio::spawn on fetch_user().";
        assert_eq!(p.process(s, AppClass::Code).trim_end(), s);
    }

    #[test]
    fn app_formatting() {
        let p = PostProcessor::new(&BTreeMap::new(), &OutputConfig::default());
        assert_eq!(p.process("git status.", AppClass::Terminal), "git status");
        assert_eq!(p.process("Sounds good.", AppClass::Chat), "Sounds good ");
        assert_eq!(
            p.process("Sounds good. See you then.", AppClass::Chat),
            "Sounds good. See you then. "
        );
        assert_eq!(p.process("Are you free?", AppClass::Chat), "Are you free? ");
        assert_eq!(p.process("Dear team.", AppClass::Prose), "Dear team. ");
        assert_eq!(p.process("um", AppClass::Prose), "");
    }
}
