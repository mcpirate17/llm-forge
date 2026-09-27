def unary(x):
    return x + 1


def binary(x, y=2):
    return x + y


def caller(v):
    return unary(v) + binary(v)
