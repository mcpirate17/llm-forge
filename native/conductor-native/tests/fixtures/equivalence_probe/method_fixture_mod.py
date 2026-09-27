import torch


class Lane:
    def __init__(self):
        self.gain = torch.tensor(3.0)

    def forward(self, x):
        return (x * self.gain).clamp_min(-1e30)
