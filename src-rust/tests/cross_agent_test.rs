// Cross-Agent Communication Integration Test
// Tests the complete cross-agent messaging flow

#[cfg(test)]
mod tests {
    // Test intent detection (no NAPI dependency)
    #[test]
    fn test_detect_cross_agent_intent() {
        use ergatai::cross_agent::detect_cross_agent_intent;

        // Test @mention pattern
        assert_eq!(
            detect_cross_agent_intent("@codex 请审查这段代码"),
            Some("codex".to_string())
        );

        assert_eq!(
            detect_cross_agent_intent("@claude 帮我看看这个 bug"),
            Some("claude".to_string())
        );

        // Test "send to" pattern
        assert_eq!(
            detect_cross_agent_intent("send to opencode: 请优化这段代码"),
            Some("opencode".to_string())
        );

        // Test "ask to" pattern
        assert_eq!(
            detect_cross_agent_intent("ask codex to review this"),
            Some("codex".to_string())
        );

        // Test no intent
        assert_eq!(detect_cross_agent_intent("hello world"), None);

        println!("✅ All intent detection tests passed!");
    }
}
