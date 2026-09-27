import torch
from fixture_mod import Lane


def test_lane():
    assert Lane().forward(torch.ones(3)).sum() == 9.0
