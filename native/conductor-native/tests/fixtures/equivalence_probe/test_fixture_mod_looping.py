
import torch
from fixture_mod import load_bearing


def test_load_bearing_over_and_over():
    for _ in range(5):
        assert load_bearing(torch.tensor([-1.0, 2.0])).tolist() == [0.0, 2.0]
