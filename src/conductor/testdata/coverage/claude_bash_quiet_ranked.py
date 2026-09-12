"""Fixture module for the ``claude_bash_quiet`` mutation-campaign manifest.

This file exists only so ``conductor/mutation_campaigns/claude_bash_quiet.json``
has a real, on-disk source path to pin a ``source_sha256`` entry against. The
manifest is a self-contained schema fixture for CLI-level tests
(``test_mutation_coverage.py::test_inspect_cli_returns_not_ready``,
``test_mutation_coverage.py::test_mutation_testing_cli_inspect_verify_and_refuse``,
and the campaign-loading smoke tests in ``test_mutation_testing.py`` /
``test_mutation_testing_support.py``); nothing here ever runs as part of a real
mutation-testing baseline. Its filename deliberately avoids the ``test_``
prefix so pytest's own collection never picks it up.
"""

VALUE = 1


def test_value_is_one() -> None:
    assert VALUE == 1
