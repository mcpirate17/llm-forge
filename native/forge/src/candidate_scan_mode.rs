//! Candidate check identities and inclusive changed-line scope.
use crate::{
    candidate_source::lines::LineRange,
    source_checks::{Check, Finding},
};
use clap::ValueEnum;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
pub enum Mode {
    #[serde(rename = "style-scan")]
    #[value(name = "style-scan")]
    Style,
    #[serde(rename = "rust-style-scan")]
    #[value(name = "rust-style-scan")]
    RustStyle,
    #[serde(rename = "hot-loop-scan")]
    #[value(name = "hot-loop-scan")]
    HotLoop,
    #[serde(rename = "test-value-scan")]
    #[value(name = "test-value-scan")]
    TestValue,
    #[serde(rename = "verbosity-scan")]
    #[value(name = "verbosity-scan")]
    Verbosity,
}

impl Mode {
    pub fn checks(self) -> &'static [Check] {
        match self {
            Self::Style => &[Check::Style, Check::Fallback],
            Self::RustStyle => &[Check::Rust],
            Self::HotLoop => &[Check::HotLoop],
            Self::TestValue => &[Check::TestValue],
            Self::Verbosity => &[Check::Verbosity],
        }
    }

    pub fn exclusion(self, path: &str, hot_paths: &[String]) -> Option<&'static str> {
        let suffix = if self == Self::RustStyle {
            ".rs"
        } else {
            ".py"
        };
        if !path.ends_with(suffix) {
            Some("unsupported file type")
        } else if self == Self::RustStyle && slop_core::rust_scan::is_test_file(path) {
            Some("Rust test file")
        } else if self == Self::HotLoop && !slop_core::hot_loop_scan::in_hot_path(path, hot_paths) {
            Some("outside configured production hot paths")
        } else {
            None
        }
    }

    pub fn includes(self, finding: &Finding, ranges: &[LineRange]) -> bool {
        let end = if matches!(self, Self::TestValue | Self::Verbosity) {
            finding.end_line
        } else {
            finding.line
        };
        let i = ranges.partition_point(|range| (range.end as usize) < finding.line);
        ranges
            .get(i)
            .is_some_and(|range| range.start as usize <= end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inclusive_span_intersection_preserves_start_line_checks() {
        let finding = Finding {
            path: "x.py".into(),
            line: 3,
            end_line: 7,
            rule: "test",
            message: "test".into(),
        };
        for (start, end, line, span) in [
            (1, 2, false, false),
            (3, 3, true, true),
            (5, 5, false, true),
            (7, 7, false, true),
            (8, 9, false, false),
        ] {
            let ranges = [LineRange { start, end }];
            for mode in [Mode::Style, Mode::RustStyle, Mode::HotLoop] {
                assert_eq!(mode.includes(&finding, &ranges), line);
            }
            for mode in [Mode::TestValue, Mode::Verbosity] {
                assert_eq!(mode.includes(&finding, &ranges), span);
            }
        }
        assert!(!Mode::TestValue.includes(&finding, &[]));
        assert!(Mode::TestValue.includes(
            &finding,
            &[
                LineRange { start: 1, end: 2 },
                LineRange { start: 7, end: 9 }
            ]
        ));
    }
}
