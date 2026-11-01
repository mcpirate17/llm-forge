//! Candidate check identities and inclusive changed-line scope.
use crate::{
    candidate_source::lines::LineRange,
    source_checks::{Check, Finding},
};
use clap::ValueEnum;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    StyleScan,
    RustStyleScan,
    HotLoopScan,
    TestValueScan,
    VerbosityScan,
}

impl Mode {
    pub fn checks(self) -> &'static [Check] {
        match self {
            Self::StyleScan => &[Check::Style, Check::Fallback],
            Self::RustStyleScan => &[Check::Rust],
            Self::HotLoopScan => &[Check::HotLoop],
            Self::TestValueScan => &[Check::TestValue],
            Self::VerbosityScan => &[Check::Verbosity],
        }
    }

    pub fn exclusion(self, path: &str, hot_paths: &[String]) -> Option<&'static str> {
        let suffix = if self == Self::RustStyleScan {
            ".rs"
        } else {
            ".py"
        };
        if !path.ends_with(suffix) {
            Some("unsupported file type")
        } else if self == Self::RustStyleScan && slop_core::rust_scan::is_test_file(path) {
            Some("Rust test file")
        } else if self == Self::HotLoopScan
            && !slop_core::hot_loop_scan::in_hot_path(path, hot_paths)
        {
            Some("outside configured production hot paths")
        } else {
            None
        }
    }

    pub fn includes(self, finding: &Finding, ranges: &[LineRange]) -> bool {
        let end = if matches!(self, Self::TestValueScan | Self::VerbosityScan) {
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
            for mode in [Mode::StyleScan, Mode::RustStyleScan, Mode::HotLoopScan] {
                assert_eq!(mode.includes(&finding, &ranges), line);
            }
            for mode in [Mode::TestValueScan, Mode::VerbosityScan] {
                assert_eq!(mode.includes(&finding, &ranges), span);
            }
        }
        assert!(!Mode::TestValueScan.includes(&finding, &[]));
        assert!(Mode::TestValueScan.includes(
            &finding,
            &[
                LineRange { start: 1, end: 2 },
                LineRange { start: 7, end: 9 }
            ]
        ));
    }
}
