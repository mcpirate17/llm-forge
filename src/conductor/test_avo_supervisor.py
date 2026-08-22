from __future__ import annotations

import json
from pathlib import Path

from conductor import avo_supervisor


def test_supervisor_records_improved(tmp_path: Path) -> None:
    state_path = tmp_path / "active_state.json"
    state_path.write_text(json.dumps({"stagnation_counter": 3}), encoding="utf-8")

    sup = avo_supervisor.StagnationSupervisor(patience=4, state_path=state_path)
    report = sup.record_step(improved=True)

    assert report.stagnated is False
    assert report.consecutive_rejections == 0
    assert sup.load_rejection_count() == 0


def test_supervisor_triggers_stagnation_alert(tmp_path: Path) -> None:
    state_path = tmp_path / "active_state.json"
    state_path.write_text(json.dumps({"stagnation_counter": 3}), encoding="utf-8")

    sup = avo_supervisor.StagnationSupervisor(patience=4, state_path=state_path)
    # 4th rejection should trigger alert
    report = sup.record_step(improved=False, lane="test_lane")

    assert report.stagnated is True
    assert report.consecutive_rejections == 4
    assert report.strategy_hint is not None
    assert "PIVOT" in report.strategy_hint
    assert report.alert_payload is not None
    assert report.alert_payload["kind"] == "stagnation-alert"
