
import os  # noqa: F401


def helper(v, scale=1.0):
    if v is None:
        raise ValueError("v is required")
    return v * scale


def caller(x, flag=True):
    y = helper(x)
    return y.clamp(min=0) if flag else y
