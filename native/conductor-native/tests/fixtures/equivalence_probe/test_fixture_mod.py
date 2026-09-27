
import torch
from fixture_mod import (
    Lane, decorated, load_bearing, renormalised, scaled, timed, validated,
)


def test_scaled():
    assert scaled(torch.ones(4), torch.full((4,), 2.0)).sum() == 8.0


def test_lane():
    torch.manual_seed(0)
    assert torch.isfinite(Lane()(torch.randn(5, 3))).all()


def test_renormalised():
    torch.manual_seed(0)
    assert torch.isfinite(renormalised(torch.randn(8)).sum())


def test_load_bearing():
    assert load_bearing(torch.tensor([-1.0, 2.0])).tolist() == [0.0, 2.0]


def test_decorated():
    assert decorated(torch.ones(4)) == 8.0


def test_timed():
    assert timed(torch.ones(4)).total == 4.0


def test_validated_rejects_negative():
    import pytest as _pytest
    with _pytest.raises(ValueError):
        validated(-1)
    assert validated(3) == 6
