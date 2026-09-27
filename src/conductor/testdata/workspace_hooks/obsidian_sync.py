#!{python}
# PostToolUse no-op standing in for the vault mirror.
import sys

sys.stdin.read()
print('{"hookSpecificOutput":{"hookEventName":"PostToolUse"}}')
