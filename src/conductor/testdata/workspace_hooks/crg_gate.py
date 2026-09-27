#!/usr/bin/env python3
import os
import sys
from pathlib import Path

PROJECT_DIR = Path(__file__).resolve().parents[1]
os.environ["PROJECT_DIR"] = str(PROJECT_DIR)
BODY = PROJECT_DIR / "tooling/hooks/agent/crg_gate.py"
os.execv(sys.executable, [sys.executable, str(BODY), *sys.argv[1:]])
