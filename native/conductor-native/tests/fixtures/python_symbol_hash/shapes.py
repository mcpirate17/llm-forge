@outer.flag
async def operation[T: int, *Ts, **P](a, /, b=3.5, *rest, flag, optional=None, **kw) -> T:
    """\"single\" and 'double', café.\nSecond line."""
    if a:
        return a + b
    elif b:
        return obj.attr[1:3]
    else:
        return {"data": [x for x in rest if x is not None], **kw}


class Lane[T](Base, metaclass=Meta):
    def forward(self, x: T = None):
        try:
            x.clamp_min(0)
        except ValueError as error:
            raise RuntimeError("failed") from error
        return x
