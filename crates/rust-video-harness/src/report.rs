use std::fmt;

use crate::Mismatch;

/// One case verdict.
pub enum Outcome {
    /// Every frame matches.
    Pass,
    /// Frames diverge.
    Fail(Vec<Mismatch>),
}

/// Suite verdicts.
#[derive(Default)]
pub struct Report {
    entries: Vec<(String, Outcome)>,
}

impl Report {
    /// Empty report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one case.
    pub fn record(&mut self, name: &str, outcome: Outcome) {
        self.entries.push((name.to_owned(), outcome));
    }

    /// Failing case count.
    #[must_use]
    pub fn failures(&self) -> usize {
        self.entries
            .iter()
            .filter(|(_, outcome)| matches!(outcome, Outcome::Fail(_)))
            .count()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, outcome) in &self.entries {
            match outcome {
                Outcome::Pass => writeln!(f, "pass {name}")?,
                Outcome::Fail(gaps) => {
                    writeln!(f, "fail {name}: {} frames", gaps.len())?;
                    for gap in gaps {
                        writeln!(f, "  {gap}")?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Outcome, Report};

    #[test]
    fn failures_counts_fail_outcomes() {
        let mut report = Report::new();
        report.record("a", Outcome::Pass);
        report.record("b", Outcome::Fail(Vec::new()));
        assert_eq!(report.failures(), 1);
    }

    #[test]
    fn display_names_each_case() {
        let mut report = Report::new();
        report.record("a", Outcome::Pass);
        let text = report.to_string();
        assert!(text.contains("pass a"), "unexpected: {text}");
    }
}
