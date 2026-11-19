//! Layer 3 of the silence filter: the credit roll whisper hallucinates.
//!
//! Whisper learned from subtitled video, and subtitled video ends with a
//! credit line. So on the breath or pause at the end of a take, the model
//! sometimes appends one nobody said: "Thanks for watching!", "Please
//! subscribe.", "Subtitles by the Amara.org community". The energy gate
//! (silence.rs) and the VAD (sidecar.rs) keep silent takes away from the
//! engine; this catches a credit glued onto the end of real speech.
//!
//! Only a credit that ENDS the text — or is the whole text — is dropped.
//! The same words mid-sentence are speech and stay. The price: a dictation
//! that genuinely signs off with "thanks for watching" loses that phrase.
//! hex (docs/hex-teardown.md §2.4) pays the same price for the same reason.
//!
//! "Thank you." and "you" are deliberately NOT on the list, even though
//! they're whisper's favorite inventions on silence. People dictate those
//! words, often as the whole message. Silence is the gate's and the VAD's
//! job; stripping real words to cover for them would trade a visible bug
//! for an invisible one.
//!
//! Matching ignores case and punctuation: the text and every credit are
//! reduced to their words (runs of letters and digits, lowercased), and a
//! credit matches when its words equal the text's last words exactly. So
//! "THANKS FOR WATCHING!!" and "thanks for watching" are the same credit.

/// Credits whisper (English, sayit's only language today) is known to
/// append. Spelling of punctuation doesn't matter — see the module doc.
const CREDITS: &[&str] = &[
    "Thanks for watching",
    "Thank you for watching",
    "Thank you so much for watching",
    "Thank you very much for watching",
    "Please subscribe",
    "Please like and subscribe",
    "Like and subscribe",
    "Don't forget to like and subscribe",
    "Please subscribe to my channel",
    "Subscribe to my channel",
    "Subtitles by the Amara.org community",
    "Transcription by CastingWords",
];

/// `text` minus any credits it ends with. Speech comes back untouched; a
/// text that was nothing but credits comes back empty, which is the
/// coordinator's "heard nothing" path. Logs a count, never the words.
pub fn strip(text: &str) -> &str {
    let (kept, dropped) = strip_counted(text);
    if dropped > 0 {
        println!("[credits] dropped {dropped} trailing word(s) of subtitle credit");
    }
    kept
}

/// The work behind `strip`, plus how many words were removed (for the log
/// and the tests).
fn strip_counted(text: &str) -> (&str, usize) {
    let said = words(text);
    let credits: Vec<Vec<String>> = CREDITS
        .iter()
        .map(|c| words(c).into_iter().map(|(_, w)| w).collect())
        .collect();

    // Peel credits off the end until none matches — whisper sometimes rolls
    // two ("Thanks for watching. Please subscribe."). At each step the
    // LONGEST matching credit wins, so "please like and subscribe" goes
    // whole instead of leaving a stray "please" behind.
    let mut end = said.len();
    loop {
        let tail = &said[..end];
        let longest = credits
            .iter()
            .filter(|credit| {
                credit.len() <= tail.len()
                    && tail[tail.len() - credit.len()..]
                        .iter()
                        .map(|(_, w)| w)
                        .eq(credit.iter())
            })
            .map(|credit| credit.len())
            .max();
        match longest {
            Some(n) => end -= n,
            None => break,
        }
    }

    let dropped = said.len() - end;
    if dropped == 0 {
        return (text, 0);
    }
    // Cut at the first dropped word, then tidy the seam: "ship it, thanks
    // for watching" keeps "ship it" without the dangling comma. A period
    // or question mark before the credit belongs to the speech and stays.
    let cut = said[end].0;
    let kept = text[..cut].trim_end_matches(|c: char| {
        c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '–' | '—')
    });
    (kept, dropped)
}

/// Each word of `text` — a run of letters or digits — lowercased, with the
/// byte offset where it starts (so we can cut the original text there).
fn words(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            out.push((s, text[s..i].to_lowercase()));
        }
    }
    if let Some(s) = start {
        out.push((s, text[s..].to_lowercase()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credit_alone_leaves_nothing() {
        assert_eq!(strip_counted("Thanks for watching!"), ("", 3));
        assert_eq!(strip_counted(" Thank you for watching. "), ("", 4));
    }

    #[test]
    fn a_trailing_credit_is_dropped_and_the_speech_kept() {
        assert_eq!(
            strip_counted("Let's ship it on Friday. Thanks for watching!"),
            ("Let's ship it on Friday.", 3)
        );
        // The seam is tidied: no dangling comma.
        assert_eq!(
            strip_counted("Let's ship it, thanks for watching"),
            ("Let's ship it", 3)
        );
    }

    #[test]
    fn matching_ignores_case_and_punctuation() {
        assert_eq!(strip_counted("THANKS FOR WATCHING!!"), ("", 3));
        assert_eq!(strip_counted("thanks, for watching"), ("", 3));
        assert_eq!(
            strip_counted("Done. Subtitles by the Amara.org community"),
            ("Done.", 6)
        );
        // An apostrophe splits "don't" the same way on both sides.
        assert_eq!(
            strip_counted("Don't forget to like and subscribe."),
            ("", 7)
        );
    }

    #[test]
    fn two_rolled_credits_both_go() {
        assert_eq!(
            strip_counted("See you at noon. Thanks for watching. Please subscribe."),
            ("See you at noon.", 5)
        );
    }

    #[test]
    fn the_longest_credit_wins() {
        // "like and subscribe" also matches the tail, but taking it would
        // leave a stray "please" behind.
        assert_eq!(strip_counted("Please like and subscribe!"), ("", 4));
    }

    #[test]
    fn the_same_words_mid_sentence_stay() {
        let mid = "Thanks for watching the kids while I was out.";
        assert_eq!(strip_counted(mid), (mid, 0));
        let mid = "Please subscribe me to the newsletter.";
        assert_eq!(strip_counted(mid), (mid, 0));
    }

    #[test]
    fn thank_you_and_you_are_real_words() {
        // Whisper invents these on silence, but people say them. They stay.
        for said in ["Thank you.", "you", "Thank you so much.", "OK, thank you."] {
            assert_eq!(strip_counted(said), (said, 0), "{said:?}");
        }
        // "Thank you." before a credit stays; only the credit goes.
        assert_eq!(
            strip_counted("Thank you. Thanks for watching!"),
            ("Thank you.", 3)
        );
    }

    #[test]
    fn a_credit_must_match_whole_words() {
        // "watchings" isn't "watching"; a partial-word match would eat speech.
        let said = "Thanks for watchings";
        assert_eq!(strip_counted(said), (said, 0));
    }

    #[test]
    fn a_genuine_sign_off_is_lost_by_design() {
        // The documented price (module doc): a real sign-off ending in a
        // credit's exact words is indistinguishable from the hallucination.
        assert_eq!(
            strip_counted("That's the demo, and thanks for watching."),
            ("That's the demo, and", 3)
        );
    }

    #[test]
    fn empty_and_wordless_text_pass_through() {
        assert_eq!(strip_counted(""), ("", 0));
        assert_eq!(strip_counted(" ... "), (" ... ", 0));
    }
}
