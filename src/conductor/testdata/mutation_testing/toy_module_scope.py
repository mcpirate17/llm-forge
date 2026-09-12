"""Fixture test-scope file for test_mutation_testing.py's campaign fixture.

Named without a `test_` prefix so pytest's own collection never picks it up;
its two functions exist only to be named by nodeid string inside the fixture
campaign manifest, never executed by this suite.
"""


def check_pack_mode_is_sequential() -> None:
    assert pack_mode_is_sequential()


def pack_mode_is_sequential() -> bool:
    from conductor.testdata.mutation_testing.toy_module import pack_mode

    return pack_mode() == "sequential"
