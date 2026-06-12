//! Query intent classifier — distinguishes advice from education in critical domains.
//!
//! EX-4215: In T0 domains (medical, legal), the covenant blanket-refuses ALL queries
//! when domain confidence is low. This module classifies queries into advice vs
//! educational intent so that definitional questions ("What is a contract?") are
//! allowed while personal advice ("Should I sue?") remains blocked.

/// Classified intent of a user query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryIntent {
    /// Personal advice, diagnosis, or action recommendation — REFUSE in T0 domains.
    Advice,
    /// Definitional, explanatory, or educational question — ALLOW in T0 domains.
    Education,
    /// Cannot determine intent — conservative default is Advice (refuse).
    Ambiguous,
}

impl QueryIntent {
    /// Classify a query string into Advice, Education, or Ambiguous.
    ///
    /// Uses keyword-signal heuristics:
    /// - Advice signals: personal pronouns + action verbs, "should I", "can I"
    /// - Education signals: "what is", "how does", "explain", "define"
    ///
    /// When both signals are present, Advice wins (conservative).
    /// When neither signal is present, returns Ambiguous.
    pub fn classify(query: &str) -> Self {
        let lower = query.to_lowercase();
        let tokens: Vec<&str> = lower.split_whitespace().collect();

        let has_advice = Self::has_advice_signals(&lower, &tokens);
        let has_education = Self::has_education_signals(&lower, &tokens);

        if has_advice {
            // Advice overrides education — conservative
            QueryIntent::Advice
        } else if has_education {
            QueryIntent::Education
        } else {
            QueryIntent::Ambiguous
        }
    }

    fn has_advice_signals(lower: &str, tokens: &[&str]) -> bool {
        // Direct advice phrases
        let advice_phrases = [
            "should i",
            "can i",
            "is it safe to",
            "what should i",
            "what should we",
            "what should my",
            "do i need",
            "do i have to",
            "is it legal to",
            "is it okay to",
            "can you tell me if",
            "am i allowed to",
        ];
        for phrase in &advice_phrases {
            if lower.contains(phrase) {
                return true;
            }
        }

        // Personal pronouns near action/medical/legal verbs
        // Use token-boundary matching to avoid "i " missing "I" at end-of-input
        let personal_pronouns = ["i", "my", "me", "i'm", "i'd"];
        let action_verbs = [
            "sue",
            "take",
            "diagnose",
            "treat",
            "prescribe",
            "file",
            "arrest",
            "convict",
            "charge",
            "fire",
            "hire",
            "invest",
            "sign",
            "agree",
            "settle",
            "appeal",
            "operate",
        ];

        // Token-based pronoun matching avoids substring false positives
        // (e.g., "i " won't match "I" at end-of-input or before punctuation)
        let has_pronoun = personal_pronouns.iter().any(|p| tokens.contains(p));
        let has_action = action_verbs.iter().any(|v| tokens.contains(v));

        has_pronoun && has_action
    }

    fn has_education_signals(lower: &str, tokens: &[&str]) -> bool {
        // Educational question starters — checked at START of query only
        // to avoid matching "how are you?" (greeting) or "my dog does X"
        let edu_starters = [
            "what is",
            "what are",
            "what was",
            "what were",
            "what should",
            "how does",
            "how do",
            "how did",
            "what does",
            "what do",
            "why does",
            "why do",
            "why is",
            "why are",
            "who is",
            "who was",
            "who are",
            "when did",
            "when was",
            "difference between",
            "types of",
        ];
        for starter in &edu_starters {
            if lower.starts_with(starter) {
                return true;
            }
        }

        // Mid-sentence educational signals — these are safe to match anywhere
        // because they are unambiguous education verbs
        let mid_sentence_edu = ["explain", "describe", "define", "tell me about"];
        for signal in &mid_sentence_edu {
            if lower.contains(signal) {
                return true;
            }
        }

        // Standalone educational verbs
        let edu_verbs = [
            "explain",
            "describe",
            "define",
            "compare",
            "contrast",
            "distinguish",
            "summarize",
            "outline",
            "list",
        ];
        tokens.iter().any(|t| edu_verbs.contains(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Education queries ───────────────────────────────────────────────────

    #[test]
    fn test_what_is_classified_education() {
        assert_eq!(
            QueryIntent::classify("What is a contract?"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_what_are_classified_education() {
        assert_eq!(
            QueryIntent::classify("What are the types of contracts?"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_how_does_classified_education() {
        assert_eq!(
            QueryIntent::classify("How does the immune system work?"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_define_classified_education() {
        assert_eq!(
            QueryIntent::classify("Define habeas corpus"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_explain_classified_education() {
        assert_eq!(
            QueryIntent::classify("Explain the difference between civil and criminal law"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_describe_classified_education() {
        assert_eq!(
            QueryIntent::classify("Describe the process of photosynthesis"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_difference_between_education() {
        assert_eq!(
            QueryIntent::classify("What is the difference between civil and criminal law?"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_what_is_aspirin_education() {
        assert_eq!(
            QueryIntent::classify("What is aspirin?"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_what_does_education() {
        assert_eq!(
            QueryIntent::classify("What does the liver do?"),
            QueryIntent::Education
        );
    }

    // ── Advice queries ──────────────────────────────────────────────────────

    #[test]
    fn test_should_i_sue_advice() {
        assert_eq!(
            QueryIntent::classify("Should I sue my neighbor?"),
            QueryIntent::Advice
        );
    }

    #[test]
    fn test_can_i_take_advice() {
        assert_eq!(
            QueryIntent::classify("Can I take ibuprofen with alcohol?"),
            QueryIntent::Advice
        );
    }

    #[test]
    fn test_what_should_advice() {
        assert_eq!(
            QueryIntent::classify("What should I do about my headache?"),
            QueryIntent::Advice
        );
    }

    #[test]
    fn test_is_it_safe_advice() {
        assert_eq!(
            QueryIntent::classify("Is it safe to take this medication during pregnancy?"),
            QueryIntent::Advice
        );
    }

    #[test]
    fn test_medication_pronoun_action_advice() {
        assert_eq!(
            QueryIntent::classify("What medication should I take?"),
            QueryIntent::Advice
        );
    }

    #[test]
    fn test_my_treatment_advice() {
        // "What is" makes it look educational, but "my" + "treat" is advice
        assert_eq!(
            QueryIntent::classify("Should I get treatment for my condition?"),
            QueryIntent::Advice
        );
    }

    #[test]
    fn test_do_i_need_advice() {
        assert_eq!(
            QueryIntent::classify("Do I need to file a police report?"),
            QueryIntent::Advice
        );
    }

    // ── Ambiguous queries ──────────────────────────────────────────────────

    #[test]
    fn test_tell_me_about_education() {
        assert_eq!(
            QueryIntent::classify("Tell me about contracts"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_greeting_ambiguous() {
        assert_eq!(QueryIntent::classify("Hello there"), QueryIntent::Ambiguous);
    }

    #[test]
    fn test_empty_string_ambiguous() {
        assert_eq!(QueryIntent::classify(""), QueryIntent::Ambiguous);
    }

    #[test]
    fn test_single_word_ambiguous() {
        assert_eq!(QueryIntent::classify("Contracts"), QueryIntent::Ambiguous);
    }

    // ── Edge cases ──────────────────────────────────────────────────────────

    #[test]
    fn test_case_insensitive() {
        assert_eq!(
            QueryIntent::classify("WHAT IS A CONTRACT?"),
            QueryIntent::Education
        );
        assert_eq!(QueryIntent::classify("SHOULD I SUE?"), QueryIntent::Advice);
    }

    #[test]
    fn test_advice_beats_education() {
        // "What should I take?" has "what" (education) but "should I" (advice)
        assert_eq!(
            QueryIntent::classify("What should I take for my headache?"),
            QueryIntent::Advice
        );
    }

    #[test]
    fn test_what_should_without_pronoun_is_education() {
        // "What should a healthy blood pressure be?" — general factual question, not personal advice
        assert_eq!(
            QueryIntent::classify("What should a healthy blood pressure be?"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_mid_sentence_education_signal() {
        assert_eq!(
            QueryIntent::classify("Can you explain what a tort is?"),
            QueryIntent::Education
        );
    }

    #[test]
    fn test_pronoun_at_end_of_input() {
        // "I" at end of input (no trailing space) — token matching should still detect it
        // "Should I sue" → "sue" is an action verb, "i" is a pronoun token
        assert_eq!(QueryIntent::classify("Should I sue"), QueryIntent::Advice);
    }
}
