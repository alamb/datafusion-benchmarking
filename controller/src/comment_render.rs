//! Render the trigger comment body from per-job benchmark sections.
//!
//! Each runner job contributes one markdown "section" that describes its
//! current state (running, completed, failed). Sibling jobs that share a
//! trigger comment get their sections concatenated into a single block,
//! delimited by HTML comment markers so we can re-extract the user's
//! original body on subsequent updates.

pub const MARKER_BEGIN: &str = "<!-- datafusion-benchmarking:begin -->";
pub const MARKER_END: &str = "<!-- datafusion-benchmarking:end -->";

/// Strip a previously-appended bot block from `body`, returning whatever the
/// user originally wrote. If the markers aren't present (first edit, or
/// human-edited comment), the entire body is treated as user content.
pub fn extract_user_body(body: &str) -> &str {
    match body.find(MARKER_BEGIN) {
        Some(idx) => body[..idx].trim_end_matches(['\n', ' ']),
        None => body.trim_end_matches(['\n', ' ']),
    }
}

/// Build the full trigger-comment body: the user's original text, followed by
/// the bot block containing one section per job. Returns `original_body`
/// unchanged when there are no sections to render.
pub fn render(original_body: &str, sections: &[String], footer: &str) -> String {
    let user = extract_user_body(original_body);
    if sections.iter().all(|s| s.trim().is_empty()) {
        return user.to_string();
    }
    let mut out = String::with_capacity(user.len() + 256);
    if !user.is_empty() {
        out.push_str(user);
        out.push_str("\n\n");
    }
    out.push_str(MARKER_BEGIN);
    out.push('\n');
    for (i, section) in sections.iter().enumerate() {
        let trimmed = section.trim();
        if trimmed.is_empty() {
            continue;
        }
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str(trimmed);
    }
    if !footer.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(footer.trim_start_matches('\n'));
    }
    out.push('\n');
    out.push_str(MARKER_END);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_returns_original_when_no_marker() {
        assert_eq!(
            extract_user_body("run benchmark tpch"),
            "run benchmark tpch"
        );
    }

    #[test]
    fn extract_strips_bot_block() {
        let body = format!("run benchmark tpch\n\n{MARKER_BEGIN}\nstuff\n{MARKER_END}",);
        assert_eq!(extract_user_body(&body), "run benchmark tpch");
    }

    #[test]
    fn extract_handles_marker_at_start() {
        let body = format!("{MARKER_BEGIN}\nstuff\n{MARKER_END}");
        assert_eq!(extract_user_body(&body), "");
    }

    #[test]
    fn render_with_no_sections_returns_user_body() {
        assert_eq!(render("hello", &[], ""), "hello");
        assert_eq!(render("hello", &["".to_string()], ""), "hello");
    }

    #[test]
    fn render_appends_single_section() {
        let out = render("run benchmark tpch", &["section A".to_string()], "");
        assert!(out.starts_with("run benchmark tpch\n\n"));
        assert!(out.contains(MARKER_BEGIN));
        assert!(out.contains("section A"));
        assert!(out.trim_end().ends_with(MARKER_END));
    }

    #[test]
    fn render_multiple_sections_separated() {
        let out = render(
            "trigger",
            &["A".to_string(), "B".to_string(), "C".to_string()],
            "",
        );
        let a = out.find('A').unwrap();
        let b = out.find('B').unwrap();
        let c = out.find('C').unwrap();
        assert!(a < b && b < c);
    }

    #[test]
    fn render_replaces_prior_bot_block() {
        let prior = format!("trigger\n\n{MARKER_BEGIN}\nold section\n{MARKER_END}",);
        let out = render(&prior, &["new section".to_string()], "");
        assert!(out.contains("new section"));
        assert!(!out.contains("old section"));
        // Only one marker pair
        assert_eq!(out.matches(MARKER_BEGIN).count(), 1);
        assert_eq!(out.matches(MARKER_END).count(), 1);
    }

    #[test]
    fn render_includes_footer_inside_block() {
        let out = render("trigger", &["section".to_string()], "\n\n---\n[issue]");
        let begin = out.find(MARKER_BEGIN).unwrap();
        let end = out.find(MARKER_END).unwrap();
        let issue = out.find("[issue]").unwrap();
        assert!(begin < issue && issue < end);
    }
}
