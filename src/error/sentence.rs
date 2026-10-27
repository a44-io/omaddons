//! Test-only style checks shared by code meanings and action descriptions.

#![cfg(test)]

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SentenceFault {
    Empty,
    Start,
    NoClosingPeriod,
    Whitespace,
    SecondSentence,
    TooLong(usize),
    NamesCommand(String),
    NamesMilestone,
}

/// A registry writing rule, not a runtime input bound.
pub(crate) const MEANING_MAX_CHARS: usize = 200;

/// Checks one plain sentence describing a condition, without a runnable command or
/// milestone name. Whether its words accurately describe the condition is reviewed
/// against the README; this checks the mechanical writing rules.
pub(crate) fn check(text: &str) -> Result<(), SentenceFault> {
    if text.is_empty() {
        return Err(SentenceFault::Empty);
    }
    if text.trim() != text
        || text.contains("  ")
        || text.chars().any(|c| c.is_whitespace() && c != ' ')
    {
        return Err(SentenceFault::Whitespace);
    }
    let chars = text.chars().count();
    if chars > MEANING_MAX_CHARS {
        return Err(SentenceFault::TooLong(chars));
    }
    if !text.starts_with(|c: char| c.is_ascii_uppercase() || c == '`') {
        return Err(SentenceFault::Start);
    }
    if !text.ends_with('.') || text.ends_with("..") || text.contains(['?', '!']) {
        return Err(SentenceFault::NoClosingPeriod);
    }
    if text
        .split(". ")
        .skip(1)
        .any(|part| part.starts_with(|c: char| c.is_ascii_uppercase() || c == '`'))
    {
        return Err(SentenceFault::SecondSentence);
    }
    let words: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '_' | '-'))
        .filter(|word| !word.is_empty())
        .collect();
    for pair in words.windows(2) {
        if pair[0] == "omaddons"
            && crate::app::COMMANDS
                .iter()
                .any(|command| command.name.split_whitespace().next() == Some(pair[1]))
        {
            return Err(SentenceFault::NamesCommand(pair[1].to_owned()));
        }
    }
    if text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| {
            let bytes = word.as_bytes();
            bytes.len() == 2 && bytes[0] == b'M' && matches!(bytes[1], b'1'..=b'9')
        })
    {
        return Err(SentenceFault::NamesMilestone);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_check_rejects_each_fault() {
        let cases = [
            ("empty", "".to_owned(), SentenceFault::Empty),
            (
                "lowercase_start",
                "a thing failed.".to_owned(),
                SentenceFault::Start,
            ),
            (
                "no_period",
                "A thing failed".to_owned(),
                SentenceFault::NoClosingPeriod,
            ),
            (
                "question",
                "A thing failed?".to_owned(),
                SentenceFault::NoClosingPeriod,
            ),
            (
                "exclamation",
                "A thing failed!".to_owned(),
                SentenceFault::NoClosingPeriod,
            ),
            (
                "two_periods",
                "A thing failed..".to_owned(),
                SentenceFault::NoClosingPeriod,
            ),
            (
                "trailing_space",
                "A thing failed. ".to_owned(),
                SentenceFault::Whitespace,
            ),
            (
                "leading_space",
                " A thing failed.".to_owned(),
                SentenceFault::Whitespace,
            ),
            (
                "double_space",
                "A thing  failed.".to_owned(),
                SentenceFault::Whitespace,
            ),
            (
                "newline",
                "A thing\nfailed.".to_owned(),
                SentenceFault::Whitespace,
            ),
            (
                "tab",
                "A thing\tfailed.".to_owned(),
                SentenceFault::Whitespace,
            ),
            (
                "second_sentence",
                "A failed. Then B.".to_owned(),
                SentenceFault::SecondSentence,
            ),
            (
                "second_sentence_after_backtick",
                "A failed. `x` too.".to_owned(),
                SentenceFault::SecondSentence,
            ),
            (
                "too_long",
                format!("A{}.", "a".repeat(199)),
                SentenceFault::TooLong(201),
            ),
            (
                "names_a_command",
                "Run omaddons undo first.".to_owned(),
                SentenceFault::NamesCommand("undo".to_owned()),
            ),
            (
                "names_a_quoted_command",
                "Run `omaddons undo` first.".to_owned(),
                SentenceFault::NamesCommand("undo".to_owned()),
            ),
            (
                "names_a_milestone",
                "Arrives in M5.".to_owned(),
                SentenceFault::NamesMilestone,
            ),
            (
                "names_a_milestone_before_dash",
                "An M5-only feature was found.".to_owned(),
                SentenceFault::NamesMilestone,
            ),
        ];
        for (case, text, expected) in cases {
            assert_eq!(check(&text), Err(expected), "{case}: {text:?}");
        }
    }

    #[test]
    fn sentence_check_accepts_the_allowed_forms() {
        let cases = [
            (
                "backtick_start",
                "`--offline` blocked a request.".to_owned(),
            ),
            (
                "the_word_omaddons",
                "A newer omaddons wrote this file.".to_owned(),
            ),
            (
                "semicolon_and_colon",
                "A failed: the cause; retryable once.".to_owned(),
            ),
            (
                "embedded_milestone_text",
                "A field named M5state was found.".to_owned(),
            ),
            ("exactly_200_characters", format!("A{}.", "a".repeat(198))),
        ];
        for (case, text) in cases {
            assert_eq!(check(&text), Ok(()), "{case}: {text:?}");
        }
    }
}
