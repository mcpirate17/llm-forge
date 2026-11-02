def load_bearing(x):
    return x.clamp_min(0)


def inert(x):
    return x.clamp_min(-1e30)


class Lane:
    def forward(self, x):
        return x.clamp_min(0)
