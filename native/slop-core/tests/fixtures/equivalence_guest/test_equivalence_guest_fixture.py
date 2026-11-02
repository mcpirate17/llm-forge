import torch
from equivalence_guest_fixture import inert, load_bearing


def test_drives_real_inputs():
    assert load_bearing(torch.tensor([-2.0])).item() == 0.0
    assert inert(torch.tensor([1.0])).item() == 1.0


def test_method():
    from equivalence_guest_fixture import Lane

    assert Lane().forward(torch.tensor([-2.0])).item() == 0.0
