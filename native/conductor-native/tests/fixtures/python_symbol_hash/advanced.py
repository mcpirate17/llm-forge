def advanced(K, π, value):
    """Surrogate-aware serializer fixture, not campaign evidence."""
    a = b"\x00\xff'quote\"\n" b"tail"
    b = 0x123456789abcdef123456789abcdef0123456789abcdef
    c = 2.345e-17j
    d = "\ud800,\udfff,\U0000dabc,\ue000" " café"
    e = r"\ud800"
    text = f"prefix {value=!r} {π:{K}.{2}f} {{braces}}" "suffix"
    more = f"{(value + 2) = :{K}}"
    match value:
        case {"key": int() as x, **rest} if x > 0:
            return a, b, c, d, e, text, rest
        case [first, *tail]:
            return first, tail
        case Point(x, y=z) | Point(x, z=z):
            return x, z
        case None | True:
            return None
        case _:
            return more
    type Alias[T, *Ts, **P] = tuple[T, *Ts]


def fstring_edges(x, y, width):
    a = u"" f"plain" u"tail"
    b = u"start" f"{x}" u"end" f"{y=}"
    c = rf"literal\ud800 {x!a:{width}}" f"{{escaped}}"
    d = f"{x=:}" f"{y=}" f"{(x + y) = !s}"
    e = f"{x:{width}.{2}f}" f"{y!r}"
    return a, b, c, d, e


def numerical_edges():
    return (0.0, 1.0, 1e20, 1e-4, 1e-5, 1e-300, 5e-324,
            1.7976931348623157e308, 1e309, 1e20j, 0j,
            0b100000000000000000000000000000000000000000000000000000000000000000001,
            0o7777777777777777777777777777777777777777777777777,
            12345678901234567890123456789012345678901234567890)
