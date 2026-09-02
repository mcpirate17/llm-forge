"""Single-process Claude Code hook dispatcher.

One interpreter per hook event reads the payload once, runs every registered hook
for that event and matcher in-process (shell-only bodies as bounded subprocesses),
merges their decisions and emits one JSON response. ``registry`` is the table of
hooks, ``merge`` the decision algebra, ``runner`` the execution engine, ``doctor``
the health check and ``bench`` the latency probe.
"""
