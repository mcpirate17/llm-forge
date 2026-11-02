use super::*;
use crate::candidate_source::lines::LineRange;
#[test]
fn changed_line_ranges_are_ordered_bounded_and_crlf_aware() {
    assert_eq!(
        lines(
            &[
                LineRange { start: 2, end: 3 },
                LineRange { start: 5, end: 5 }
            ],
            &mut 0
        )
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>(),
        [2, 3, 5]
    );
    assert!(lines(
        &[LineRange {
            start: 1,
            end: 65_537
        }],
        &mut 0
    )
    .is_err());
    assert!(lines(
        &[
            LineRange { start: 2, end: 3 },
            LineRange { start: 3, end: 4 }
        ],
        &mut 0
    )
    .is_err());
    assert!(only_lf(b"a\r\nb\n"));
    assert!(!only_lf(b"a\rb"));
}
