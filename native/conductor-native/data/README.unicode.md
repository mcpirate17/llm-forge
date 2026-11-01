# Candidate path Unicode profile

Candidate path casefold and printable representation use Unicode 15.0.0, the Unicode Character Database version in CPython 3.12. They match the default non-Turkic full case fold and CPython 3.12 string-representation rules for valid UTF-8. They do not normalize Unicode, reproduce lone Python surrogate code points, or claim compatibility with Python versions using another Unicode release. Paths are limited to 4096 UTF-8 bytes. Stamp this version in any derived evidence or cache identity.

- Casefold data: [Unicode 15.0 CaseFolding.txt](https://www.unicode.org/Public/15.0.0/ucd/CaseFolding.txt), SHA-256 `cdd49e55eae3bbf1f0a3f6580c974a0263cb86a6a08daa10fbf705b4808a56f7`; statuses C+F, no Turkic T. Generated data copyright © 2022 Unicode, Inc.; see [Unicode license](UNICODE-LICENSE.txt).
- Category data: exact `unicode-general-category = 0.6.0`, generated from Unicode 15.0.0. It determines the Python printable category rule (L/M/N/P/S and ASCII space).
- [Python 3.12 Unicode database](https://docs.python.org/3.12/library/unicodedata.html) and [printability rules](https://docs.python.org/3.12/library/stdtypes.html#str.isprintable) document the compatibility target.

For regeneration, obtain the exact official casefold file and run `cargo run --example generate_candidate_casefold -- PATH` from the native core, comparing its stdout byte-for-byte with `src/candidate_unicode_table.rs`. The native generator requires a regular file of at most 256 KiB and checks the exact SHA-256 above, version, scalar validity, duplicates and mapping count before generating output. On Unix it opens without following the final symlink or blocking on a FIFO. Neither generator nor tests require Python.
