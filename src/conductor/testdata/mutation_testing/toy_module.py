"""Toy module used only as fixture data for mutation-testing manifest tests.

Not part of the conductor package's runtime surface -- it exists so
test_mutation_testing.py can load a schema-valid, conductor-scoped campaign
without reaching into a host project's real corpus.
"""


def pack_mode() -> str:
    return "sequential"
