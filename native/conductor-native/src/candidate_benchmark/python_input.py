"""Frozen Python analysis workload; never a review coordinator."""


def bounded_total(values: list[int], limit: int = 100) -> int:
    total = 0
    for value in values:
        if value > 0 and total + value <= limit:
            total += value
    return total
