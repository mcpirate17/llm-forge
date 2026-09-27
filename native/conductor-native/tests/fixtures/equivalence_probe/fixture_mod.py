
import dataclasses
import functools
import time

import torch


def scaled(x, gain):
    """`.clamp_min` here can never bind: gain is always positive in the tests."""
    return x * gain.clamp_min(-1e30)


def masked_softmax(scores, used=None):
    """The production shape: a guard passed as an argument the helper defaults away."""
    if used is None:
        return torch.softmax(scores, dim=-1)
    masked = torch.where(used, scores, torch.full_like(scores, -1e30))
    return torch.softmax(masked, dim=-1)


class Lane(torch.nn.Module):
    """A slot lane whose empty-slot mask is unreachable from the input alone.

    `h` is a tanh, so no input scale can saturate the router -- exactly the shape
    that hides a live guard from an ordinary test, and from a probe that only
    amplifies inputs. Scaling the PARAMETERS underflows the small route entries to
    zero, the mask starts excluding slots whose scores are competitive, and the
    result moves by order one.
    """

    def __init__(self, slots=4, dim=3):
        super().__init__()
        self.route = torch.nn.Linear(dim, slots)
        self.value = torch.nn.Parameter(torch.randn(slots, dim))

    def forward(self, x):
        h = torch.tanh(x)
        route = torch.softmax(self.route(h), dim=-1)
        used = route.cumsum(dim=0) > 1e-6
        scores = h @ self.value.t()
        return masked_softmax(scores, used) @ self.value


def renormalised(x):
    """Dividing a unit-norm vector by its own norm moves only the last bits."""
    unit = x / x.norm().clamp_min(1e-12)
    return unit / unit.norm().clamp_min(1e-12)


def load_bearing(x):
    return x.clamp_min(0.0)


def validated(x):
    """A guard whose only reachable input is the one that makes it fire."""
    if x < 0:
        raise ValueError("x must be non-negative")
    return x * 2


def doubling(fn):
    """A decorator that changes the result, so removing it has to read LIVE."""

    @functools.wraps(fn)
    def inner(*a, **k):
        return fn(*a, **k) * 2

    return inner


@doubling
def decorated(x):
    return x.sum()


@dataclasses.dataclass
class Timing:
    """A result object carrying a wall-clock field, as the real one did.

    A DATACLASS, not a dict, and that is the whole point: `_difference` recurses into
    a dict and compares the fields numerically, but for an arbitrary object it falls
    back to `==`, which is all-or-nothing. So a microsecond of timing jitter is
    scored as an INFINITE relative change and can never meet a noise threshold.
    """

    total: float = 0.0
    elapsed_ms: float = 0.0


def timed(x, scale=1.0):
    """The shape that produced 11 of 16 false findings in the first real sweep.

    Two calls with the same input never agree, because one field is a clock.
    """
    total = float((x * scale).sum())
    # The RECORDED calls agree, so the probe gets past its first screen, and only the
    # amplified regime reaches the branch that reads a clock. That is why the real
    # findings all read max_diff_recorded=0.0 with max_diff_amplified=inf.
    if abs(total) > 1e3:
        return Timing(total=total, elapsed_ms=time.perf_counter() * 1e3)
    return Timing(total=total, elapsed_ms=0.0)
