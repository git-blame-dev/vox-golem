//! App-owned presentation policy shared by generation and continuation adapters.

pub const PLAIN_TEXT_POLICY: &str = "Answer directly in plain text, without Markdown or HTML, headings, bullet or numbered-list formatting, tables, emphasis markers, or code fences. Use concise natural sentences and short paragraphs. Give the shortest complete answer that preserves essential facts, qualifications, uncertainty, sources, and necessary steps. Avoid repetition, unnecessary introductions, and a lead summary that duplicates the answer. Preserve refusals and their meaning, and preserve literal symbols, equations, code, identifiers, and quoted text when required for accuracy. Present essential source URLs as plain text. This presentation rule applies only to user-facing answer text, including answer text inside structured stage responses; required tool calls and structured stage JSON keep their required formats.";

/// Append the policy without rewriting external instructions or protocol requirements.
/// Composing an already composed request leaves it unchanged.
pub fn compose_instructions(instructions: &str) -> String {
    if instructions.is_empty() {
        return PLAIN_TEXT_POLICY.to_owned();
    }
    if instructions == PLAIN_TEXT_POLICY
        || instructions
            .strip_suffix(PLAIN_TEXT_POLICY)
            .is_some_and(|prefix| prefix.ends_with("\n\n"))
    {
        return instructions.to_owned();
    }
    format!("{instructions}\n\n{PLAIN_TEXT_POLICY}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_preserves_external_text_and_structured_tool_requirements_verbatim() {
        let external = "  Synthetic SOUL instructions.\nCall synthetic_tool when needed.\nReturn {\"answer\":\"…\",\"refusal\":null} as stage JSON.\nPreserve code `synthetic_id = -2` and sources.\n ";
        let composed = compose_instructions(external);
        assert_eq!(composed, format!("{external}\n\n{PLAIN_TEXT_POLICY}"));
        assert_eq!(compose_instructions(&composed), composed);
    }

    #[test]
    fn empty_instructions_use_policy_and_whitespace_is_not_rewritten() {
        assert_eq!(compose_instructions(""), PLAIN_TEXT_POLICY);
        assert_eq!(compose_instructions(PLAIN_TEXT_POLICY), PLAIN_TEXT_POLICY);
        assert_eq!(
            compose_instructions(" \n\t"),
            format!(" \n\t\n\n{PLAIN_TEXT_POLICY}")
        );
    }
}
