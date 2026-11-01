//! Reusable source checks through slop-core's existing detector algorithms.
//! The caller owns source/path admission and applicable-check selection. Python
//! checks share one PythonSource per file; fallback-only scanning retains its
//! existing detector entry point. This module performs no filesystem I/O.
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use slop_core::{
    detector_scan, hot_loop_scan, rust_scan, style_scan, test_value_scan, verbosity_scan,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Check {
    Style,
    Fallback,
    TestValue,
    Verbosity,
    HotLoop,
    Rust,
}

#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Finding {
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) end_line: usize,
    pub(crate) rule: &'static str,
    pub(crate) message: String,
}

impl From<style_scan::Finding> for Finding {
    fn from(f: style_scan::Finding) -> Self {
        Self {
            path: f.path,
            line: f.line,
            end_line: f.line,
            rule: f.rule,
            message: f.message,
        }
    }
}

pub(crate) fn scan(path: &str, source: &str, checks: &[Check]) -> Result<Vec<Finding>> {
    let parsed = if checks
        .iter()
        .any(|c| !matches!(c, Check::Rust | Check::Fallback))
    {
        Some(slop_core::source::PythonSource::new(source).map_err(anyhow::Error::msg)?)
    } else {
        None
    };
    let python = || {
        parsed
            .as_ref()
            .context("Python AST was not selected for this check")
    };
    let mut rows = Vec::new();
    for check in checks {
        let common = match check {
            Check::Style => Some(style_scan::scan_parsed(path, python()?)),
            Check::Fallback => Some(match parsed.as_ref() {
                Some(p) => detector_scan::scan_parsed(path, p),
                None => detector_scan::scan_source(path, source).map_err(anyhow::Error::msg)?,
            }),
            Check::HotLoop => Some(hot_loop_scan::scan_parsed(path, python()?)),
            _ => None,
        };
        if let Some(result) = common {
            rows.extend(result.into_iter().map(Finding::from));
            continue;
        }
        match check {
            Check::Rust => rows.extend(
                rust_scan::scan_source(source, path)
                    .map_err(anyhow::Error::msg)?
                    .into_iter()
                    .map(|f| Finding {
                        path: f.path,
                        line: f.line,
                        end_line: f.line,
                        rule: f.rule,
                        message: f.message,
                    }),
            ),
            Check::TestValue => rows.extend(
                test_value_scan::scan_parsed(path, python()?)
                    .into_iter()
                    .map(|f| Finding {
                        path: f.path,
                        line: f.line,
                        end_line: f.end_line,
                        rule: f.rule,
                        message: f.message,
                    }),
            ),
            Check::Verbosity => rows.extend(
                verbosity_scan::scan_parsed(path, python()?)
                    .into_iter()
                    .map(|f| Finding {
                        path: f.path,
                        line: f.line,
                        end_line: f.end_line,
                        rule: f.rule,
                        message: f.message,
                    }),
            ),
            _ => unreachable!("common check handled above"),
        }
    }
    rows.sort();
    Ok(rows)
}
