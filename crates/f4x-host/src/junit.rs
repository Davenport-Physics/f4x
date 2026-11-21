//! JUnit XML for in-game test runs: one `<testsuite>` per run (game session).
//!
//! A run that could not complete (launch failure, crash, timeout) gets a synthetic
//! `<testcase name="session">` with an `<error>`, so CI shows it as a failure even
//! when no test reported.

use f4x_proto::{TestResult, TestStatus};

/// One game session's results.
#[derive(Clone, Debug, Default)]
pub struct Suite {
    pub name: String,
    /// ISO-8601 start time, if known.
    pub timestamp: Option<String>,
    pub results: Vec<TestResult>,
    /// Why the run failed outside any test (crash summary, launch error...).
    pub error: Option<String>,
    pub seconds: f64,
    /// Properties shown with the suite (host, session dir, ...).
    pub properties: Vec<(String, String)>,
}

/// Escape text for XML attributes and character data, dropping characters XML 1.0
/// cannot carry.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

fn seconds(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

pub fn render(suites: &[Suite]) -> String {
    let mut total = (0usize, 0usize, 0usize);
    let mut body = String::new();
    let mut all_seconds = 0.0;
    for suite in suites {
        let failures = suite.results.iter().filter(|r| r.status != TestStatus::Passed).count();
        let errors = usize::from(suite.error.is_some());
        let tests = suite.results.len() + errors;
        total.0 += tests;
        total.1 += failures;
        total.2 += errors;
        all_seconds += suite.seconds;
        body += &format!(
            "  <testsuite name=\"{}\" tests=\"{tests}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"0\" time=\"{:.3}\"",
            escape(&suite.name),
            suite.seconds
        );
        if let Some(timestamp) = &suite.timestamp {
            body += &format!(" timestamp=\"{}\"", escape(timestamp));
        }
        body += ">\n";
        if !suite.properties.is_empty() {
            body += "    <properties>\n";
            for (name, value) in &suite.properties {
                body += &format!("      <property name=\"{}\" value=\"{}\"/>\n", escape(name), escape(value));
            }
            body += "    </properties>\n";
        }
        for result in &suite.results {
            body += &format!(
                "    <testcase name=\"{}\" classname=\"{}\" time=\"{}\"",
                escape(&result.name),
                escape(&result.plugin),
                seconds(result.duration_ms)
            );
            let kind = match result.status {
                TestStatus::Passed => {
                    body += "/>\n";
                    continue;
                }
                TestStatus::Failed => "failed",
                TestStatus::TimedOut => "timed_out",
                TestStatus::Panicked => "panicked",
            };
            let message = result.message.clone().unwrap_or_else(|| kind.replace('_', " "));
            let first_line = message.lines().next().unwrap_or("");
            body += &format!(
                ">\n      <failure type=\"{kind}\" message=\"{}\">{}</failure>\n    </testcase>\n",
                escape(first_line),
                escape(&message)
            );
        }
        if let Some(error) = &suite.error {
            let first_line = error.lines().next().unwrap_or("");
            body += &format!(
                "    <testcase name=\"session\" classname=\"f4x\" time=\"0.000\">\n      <error type=\"session\" message=\"{}\">{}</error>\n    </testcase>\n",
                escape(first_line),
                escape(error)
            );
        }
        body += "  </testsuite>\n";
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"f4x\" tests=\"{}\" failures=\"{}\" errors=\"{}\" time=\"{all_seconds:.3}\">\n{body}</testsuites>\n",
        total.0, total.1, total.2
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(name: &str, status: TestStatus, message: Option<&str>) -> TestResult {
        TestResult {
            name: name.into(),
            plugin: "smoke_tests".into(),
            status,
            duration_ms: 1500,
            pumps: 10,
            message: message.map(Into::into),
        }
    }

    #[test]
    fn renders_passes_failures_and_session_errors() {
        let suites = vec![
            Suite {
                name: "f4x run 1".into(),
                timestamp: Some("2026-11-21T18:00:00Z".into()),
                results: vec![
                    result("main_menu", TestStatus::Passed, None),
                    result("cast <form> & \"check\"", TestStatus::Failed, Some("expected 1\ngot 2")),
                    result("slow", TestStatus::TimedOut, None),
                ],
                error: None,
                seconds: 12.5,
                properties: vec![("host".into(), "f4x".into())],
            },
            Suite {
                name: "f4x run 2".into(),
                results: vec![result("main_menu", TestStatus::Passed, None)],
                error: Some("crash: EXCEPTION_ACCESS_VIOLATION at Fallout4.exe+0x10\u{1}".into()),
                seconds: 3.0,
                ..Suite::default()
            },
        ];
        let xml = render(&suites);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"f4x\" tests=\"5\" failures=\"2\" errors=\"1\" time=\"15.500\">"), "{xml}");
        assert!(xml.contains("<testsuite name=\"f4x run 1\" tests=\"3\" failures=\"2\" errors=\"0\" skipped=\"0\" time=\"12.500\" timestamp=\"2026-11-21T18:00:00Z\">"));
        assert!(xml.contains("<property name=\"host\" value=\"f4x\"/>"));
        assert!(xml.contains("<testcase name=\"main_menu\" classname=\"smoke_tests\" time=\"1.500\"/>"));
        assert!(xml.contains("name=\"cast &lt;form&gt; &amp; &quot;check&quot;\""));
        assert!(xml.contains("<failure type=\"failed\" message=\"expected 1\">expected 1\ngot 2</failure>"));
        assert!(xml.contains("<failure type=\"timed_out\" message=\"timed out\">timed out</failure>"));
        assert!(xml.contains("<error type=\"session\" message=\"crash: EXCEPTION_ACCESS_VIOLATION at Fallout4.exe+0x10\">"));
        assert!(!xml.contains('\u{1}'));
        assert_eq!(xml.matches("<testsuite ").count(), 2);
        assert_eq!(xml.matches("</testcase>").count(), 3);
    }
}
