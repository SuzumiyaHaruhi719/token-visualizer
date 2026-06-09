//! Shared helpers for turning a raw user-message string into a clean, capped
//! single-line prompt for the live-session "last user message" field.
//!
//! Both the Claude parser ([`crate::parser`]) and the Codex live reader
//! ([`crate::codex`]) need the SAME cleaning (collapse whitespace, cap length)
//! and the SAME notion of "this is an injected wrapper, not a real prompt", so
//! the logic lives here once rather than being duplicated per agent.

/// Maximum length (in CHARS, not bytes) of a surfaced user message. Long prompts
/// are truncated with an ellipsis so the UI shows a one-line snippet.
pub const MAX_USER_MESSAGE_CHARS: usize = 140;

/// Injected/wrapper prefixes that mark a "user" line as NOT a typed prompt.
///
/// Claude Code and Codex both inject synthetic user-role content: slash-command
/// wrappers, the environment/context preamble, skill bootstraps, and the
/// interrupt marker. A real prompt never starts with one of these, so a
/// prefix match is a reliable, conservative reject (we match known tags rather
/// than any leading `<`, so a prompt that legitimately begins with HTML/XML or
/// code is still surfaced).
const INJECTED_PREFIXES: &[&str] = &[
    "<command-message>",
    "<command-name>",
    "<command-args>",
    "<local-command-stdout>",
    "<local-command-caveat>",
    "<environment_context>",
    "<user_instructions>",
    "<system-reminder>",
    // Sub-agent / Task tooling wrappers (real shapes seen in ~/.claude jsonl):
    // a dispatched Task surfaces its bootstrap + completion pings as synthetic
    // `role:user` lines, none of which are a human prompt.
    "<task-notification>",
    "<task-prompt>",
    "<task-id>",
    "<tool-use-id>",
    "<output-file>",
    // Context-compaction continuation summary (`/compact` and auto-compaction
    // both inject a long summary as a synthetic `role:user` message).
    "This session is being continued",
    "[Request interrupted",
    "Base directory for this skill",
    "Caveat: The messages below",
];

/// Whether a raw user string is an injected wrapper rather than a typed prompt.
/// Matching is done on the trimmed leading text against [`INJECTED_PREFIXES`].
pub fn is_injected_user_text(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    INJECTED_PREFIXES
        .iter()
        .any(|p| starts_with_ignore_ascii_case(trimmed, p))
}

/// Collapse a raw prompt to a single trimmed line and cap it to
/// [`MAX_USER_MESSAGE_CHARS`] characters (appending `…` when truncated).
/// Returns an empty string when the input is blank after trimming.
pub fn clean_user_text(raw: &str) -> String {
    // Collapse ALL runs of whitespace (including newlines/tabs) to single
    // spaces so a multi-line prompt renders as one line.
    let collapsed: String = {
        let mut out = String::with_capacity(raw.len());
        let mut in_ws = false;
        for ch in raw.trim().chars() {
            if ch.is_whitespace() {
                if !in_ws {
                    out.push(' ');
                    in_ws = true;
                }
            } else {
                out.push(ch);
                in_ws = false;
            }
        }
        out
    };
    cap_chars(&collapsed, MAX_USER_MESSAGE_CHARS)
}

/// Truncate `s` to at most `max` characters, appending an ellipsis when cut.
/// Char-based (never splits a multibyte codepoint).
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", kept.trim_end())
}

/// ASCII-case-insensitive prefix test (the injected tags are all ASCII).
fn starts_with_ignore_ascii_case(haystack: &str, prefix: &str) -> bool {
    haystack.len() >= prefix.len()
        && haystack
            .get(..prefix.len())
            .map(|h| h.eq_ignore_ascii_case(prefix))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_whitespace_to_single_line() {
        assert_eq!(clean_user_text("  hello\n\n  world\t!  "), "hello world !");
    }

    #[test]
    fn empty_after_trim_is_empty() {
        assert_eq!(clean_user_text("   \n\t  "), "");
    }

    #[test]
    fn caps_long_text_with_ellipsis() {
        let long = "a".repeat(200);
        let out = clean_user_text(&long);
        assert_eq!(out.chars().count(), MAX_USER_MESSAGE_CHARS);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn short_text_unchanged() {
        assert_eq!(clean_user_text("fix the bug"), "fix the bug");
    }

    #[test]
    fn cap_is_char_based_not_byte_based() {
        // Multibyte chars (each 3 bytes in UTF-8) must be counted as one char,
        // and truncation must never split a codepoint.
        let s = "中".repeat(200);
        let out = clean_user_text(&s);
        assert_eq!(out.chars().count(), MAX_USER_MESSAGE_CHARS);
        // Round-trips as valid UTF-8 (no panic / no broken codepoint).
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn detects_injected_wrappers() {
        assert!(is_injected_user_text("<command-message>foo</command-message>"));
        assert!(is_injected_user_text("<environment_context>\n  <cwd>x</cwd>"));
        assert!(is_injected_user_text("  <user_instructions> hi"));
        assert!(is_injected_user_text("[Request interrupted by user]"));
        assert!(is_injected_user_text("Base directory for this skill: C:/x"));
    }

    #[test]
    fn detects_task_subagent_and_continuation_wrappers() {
        // Real shapes captured from ~/.claude jsonl. A dispatched Task's
        // notification/bootstrap and the compaction continuation summary are
        // synthetic `role:user` lines, never a human prompt — they must be
        // rejected so they never leak in as the surfaced "last user message".
        assert!(is_injected_user_text(
            "<task-notification>\n<task-id>b7j6vzswj</task-id>\n<tool-use-id>toolu_01</tool-use-id>"
        ));
        assert!(is_injected_user_text("<task-prompt>do the thing</task-prompt>"));
        assert!(is_injected_user_text("<task-id>abc</task-id>"));
        assert!(is_injected_user_text(
            "This session is being continued from a previous conversation that ran out of context."
        ));
    }

    #[test]
    fn real_prompt_is_not_injected() {
        assert!(!is_injected_user_text("fix the bug in state.rs"));
        assert!(!is_injected_user_text("按照你的边聊边画来"));
        // A prompt that legitimately starts with a non-injected angle bracket is
        // NOT rejected (we only match known tags, not any leading '<').
        assert!(!is_injected_user_text("<div> should this render?"));
    }
}
