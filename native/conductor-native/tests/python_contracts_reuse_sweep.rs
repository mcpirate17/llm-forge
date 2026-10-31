#![cfg(feature = "python-compat-tests")]
//! Contracts for broad-scope selection and observable reuse detector failures.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::ffi::CString;

#[test]
fn reuse_sweep_scope_execution_and_failure_contracts() {
    let _case = support::Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals
            .set_item("sweep", support::module(py, "conductor.reuse.sweep"))
            .unwrap();
        let script = CString::new(CONTRACTS).unwrap();
        py.run(&script, Some(&globals), None).unwrap();
    });
}

#[test]
fn reuse_sweep_retirement_plans_group_fresh_evidence() {
    let _case = support::Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals
            .set_item("sweep", support::module(py, "conductor.reuse.sweep"))
            .unwrap();
        py.run(
            &CString::new(RETIREMENT_CONTRACTS).unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
    });
}

#[test]
fn reuse_sweep_rejects_invalid_scope_and_preserves_plan_inputs() {
    let _case = support::Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals
            .set_item("sweep", support::module(py, "conductor.reuse.sweep"))
            .unwrap();
        py.run(
            &CString::new(INPUT_CONTRACTS).unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
    });
}

#[test]
fn reuse_sweep_cli_sigterm_reaps_detector_tree_and_restores_handler() {
    let _case = support::Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals
            .set_item("sweep", support::module(py, "conductor.reuse.sweep"))
            .unwrap();
        py.run(
            &CString::new(SIGTERM_CONTRACT).unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
    });
}

#[test]
fn reuse_sweep_unifies_only_fresh_evidence_and_preserves_failed_writes() {
    let _case = support::Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals
            .set_item("sweep", support::module(py, "conductor.reuse.sweep"))
            .unwrap();
        py.run(
            &CString::new(OPPORTUNITY_CONTRACTS).unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
    });
}

#[test]
fn reuse_sweep_caller_hints_require_current_unambiguous_function_identity() {
    let _case = support::Case::new();
    Python::attach(|py| {
        let globals = PyDict::new(py);
        globals
            .set_item("sweep", support::module(py, "conductor.reuse.sweep"))
            .unwrap();
        py.run(
            &CString::new(CALLER_CONTRACTS).unwrap(),
            Some(&globals),
            None,
        )
        .unwrap();
    });
}

const CALLER_CONTRACTS: &str = r#"
from pathlib import Path
from unittest.mock import MagicMock, patch

sites = [{'file': f'pkg/{name}.py', 'name': 'helper', 'line_start': 10}
         for name in ('popular', 'reference', 'unused', 'missing', 'ambiguous', 'shifted')]
functions = {'clusters': [{'kind': 'exact', 'sites': sites + sites[:1]},
                          {'kind': 'token', 'sites': [{'file': 'block.py', 'name': '<clone>', 'line_start': 1}]}],
             'abstractions': {'profiles': [*sites[:2], {'file': 'not-a-candidate.py', 'name': 'unused', 'line_start': 1}],
                              'edges': [{'left': 0, 'right': 1}]}}
rows = [{**site, 'caller_count': count} for site, count in zip(sites, (55, 4, 0))]
rows += [{**sites[4], 'caller_count': 7}, {**sites[4], 'caller_count': 9},
         {**sites[5], 'line_start': 11, 'caller_count': 99}]
fresh = {'available': True, 'complete': True, 'graph_head': 'head', 'repo_head': 'head', 'overlay_hash': 'clean'}

def graph(statuses):
    index = MagicMock()
    index.status.side_effect = [MagicMock(as_dict=lambda value=value: value) for value in statuses]
    index.native_reuse_rows.return_value = rows
    return index

index = graph([fresh, fresh])
with patch.object(sweep, 'GraphIndex', return_value=index):
    hints = sweep.caller_hints(Path('.'), functions)
assert hints['complete'] and len(hints['sites']) == len(sites)
assert index.status.call_args_list[0].args == index.status.call_args_list[1].args == (['.'],)
index.native_reuse_rows.assert_called_once_with(caller_limit=None)
counts = {site['file']: site['caller_count'] for site in hints['sites']}
assert counts == {'pkg/popular.py': 55, 'pkg/reference.py': 4, 'pkg/unused.py': 0,
                  'pkg/missing.py': None, 'pkg/ambiguous.py': None, 'pkg/shifted.py': None}
assert hints['graph_status_before'] == hints['graph_status_after'] == fresh
for after in ({**fresh, 'complete': False, 'overlay_hash': 'dirty'},
              {**fresh, 'graph_head': 'next', 'repo_head': 'next'}):
    index = graph([fresh, after])
    with patch.object(sweep, 'GraphIndex', return_value=index):
        hints = sweep.caller_hints(Path('.'), functions)
    assert not hints['complete'] and all(site['caller_count'] is None for site in hints['sites'])
index = graph([{**fresh, 'complete': False}])
with patch.object(sweep, 'GraphIndex', return_value=index):
    hints = sweep.caller_hints(Path('.'), functions)
assert not hints['complete']
index.native_reuse_rows.assert_not_called()
index = graph([fresh])
index.native_reuse_rows.side_effect = sweep.sqlite3.OperationalError('locked')
with patch.object(sweep, 'GraphIndex', return_value=index):
    hints = sweep.caller_hints(Path('.'), functions)
assert not hints['complete'] and 'locked' in hints['reason']
assert all(site['caller_count'] is None for site in hints['sites'])
"#;

const OPPORTUNITY_CONTRACTS: &str = r#"
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

with TemporaryDirectory() as directory:
    root = Path(directory)
    functions = {'clusters': [{'id': 'C001', 'kind': 'exact', 'sites': [
        {'file': 'pkg/a.py', 'line_start': 1, 'line_end': 8, 'name': 'run'},
        {'file': 'pkg/b.py', 'line_start': 1, 'line_end': 8, 'name': 'run'}]}],
        'abstractions': {'profiles': [], 'edges': [], 'coverage': {
            'candidate_generation_complete': False, 'complete': False, 'pair_limit_hit': True}}}
    (root / 'functions.json').write_text(json.dumps(functions))
    (root / 'families.json').write_text('old invalid report')
    (root / 'native-reuse.json').write_text('old invalid report')
    stages = {'functions': {'status': 'incomplete', 'fresh_report': True},
              'families': {'status': 'failed', 'fresh_report': True},
              'native_reuse': {'status': 'ok', 'fresh_report': False}}
    summary = {'source_scans_complete': False, 'graph_evidence_complete': False, 'execution_complete': False}
    with patch.object(sweep, '_native_decide', wraps=sweep._native_decide) as decide:
        result = sweep.write_opportunity_report(stages, summary, root, root)
        payload = decide.call_args_list[0].args[1]
    assert payload['functions'] == functions
    assert payload['families'] == payload['native_reuse'] == payload['retirement'] == {}
    assert payload['coverage']['detectors']['families']['status'] == 'failed'
    assert payload['coverage']['abstractions']['pair_limit_hit']
    assert not payload['caller_hints']['complete']
    assert all(site['caller_count'] is None for site in payload['caller_hints']['sites'])
    report = json.loads((root / 'opportunities.json').read_text())
    assert report['review_only'] and report['opportunities']
    assert report['summary'] == result
    assert (root / 'opportunities.md').read_text()
    hints = {'complete': True, 'sites': [
        {'file': 'pkg/a.py', 'name': 'run', 'line_start': 1, 'caller_count': 4},
        {'file': 'pkg/b.py', 'name': 'run', 'line_start': 1, 'caller_count': 55}]}
    with patch.object(sweep, 'caller_hints', return_value=hints):
        sweep.write_opportunity_report(stages, summary, root, root)
    owners = json.loads((root / 'opportunities.json').read_text())['opportunities'][0]['existing_owner_candidates']
    assert [(site['file'], site['observed_graph_callers']) for site in owners] == [('pkg/b.py', 55), ('pkg/a.py', 4)]
    # A current native report is included; a previous successful function report is not.
    native = {'status': 'failed', 'error': 'current graph unavailable'}
    (root / 'native-reuse.json').write_text(json.dumps(native))
    stages['native_reuse']['fresh_report'] = True
    stages['native_reuse']['status'] = 'failed'
    stages['functions']['fresh_report'] = False
    with patch.object(sweep, '_native_decide', wraps=sweep._native_decide) as decide:
        sweep.write_opportunity_report(stages, summary, root, root)
        payload = decide.call_args_list[0].args[1]
    assert payload['functions'] == {} and payload['native_reuse'] == native
    before = (root / 'opportunities.json').read_bytes()
    with patch('conductor.atomic_json.json.dump', side_effect=OSError('injected write failure')):
        try:
            sweep.write_opportunity_report(stages, summary, root, root)
        except OSError:
            pass
        else:
            raise AssertionError('write failure was suppressed')
    assert (root / 'opportunities.json').read_bytes() == before
    assert not list(root.glob('.opportunities.json.*.tmp'))
    args = sweep.build_parser().parse_args(['--abstraction-exhaustive', '--abstraction-max-pairs', '3'])
    options = sweep.abstraction_settings(args)
    full = sweep.detector_commands(root, root, ['pkg'], [], False, abstraction_options=options)[0][1]
    quick = sweep.detector_commands(root, root, ['pkg'], [], True, abstraction_options=options)[0][1]
    assert '--with-abstractions' in full and '--abstraction-exhaustive' in full
    assert full[full.index('--abstraction-max-pairs') + 1] == '3'
    assert '--with-abstractions' not in quick and '--abstraction-exhaustive' not in quick
"#;

const INPUT_CONTRACTS: &str = r#"
import contextlib
import io
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

def reject(argv):
    with patch.object(sweep, 'run_detector') as detector, contextlib.redirect_stderr(io.StringIO()):
        try:
            sweep.main(argv)
        except SystemExit as error:
            assert error.code == 2
        else:
            raise AssertionError('invalid input was accepted')
        detector.assert_not_called()

with TemporaryDirectory() as directory:
    base = Path(directory)
    repo = base / 'repo'
    repo.mkdir()
    (repo / 'pkg').mkdir()
    (repo / 'file.py').write_text('value = 1\n')
    outside = base / 'outside'
    outside.mkdir()
    (repo / 'escape').symlink_to(outside, target_is_directory=True)
    out = base / 'not-created'
    for target in ('missing', 'file.py', str(outside), 'escape'):
        reject(['--repo', str(repo), '--targets', target, '--quick', '--out-dir', str(out)])
        assert not out.exists()
    source_bytes = b'{"schema_version": 1, "plans": []}\n'
    for index, name in enumerate(sweep.OUTPUT_NAMES):
        out = base / f'collision{index}'
        out.mkdir()
        source = out / name
        source.write_bytes(source_bytes)
        reject(['--repo', str(repo), '--targets', 'pkg', '--quick', '--out-dir', str(out),
                '--retirement-plans', str(source)])
        assert source.read_bytes() == source_bytes
        assert list(out.iterdir()) == [source]
    source = base / 'reviewed.json'
    source.write_bytes(source_bytes)
    for kind in ('symlink', 'hardlink'):
        out = base / kind
        out.mkdir()
        alias = out / 'functions.log'
        if kind == 'symlink':
            alias.symlink_to(source)
        else:
            os.link(source, alias)
        reject(['--repo', str(repo), '--targets', 'pkg', '--quick', '--out-dir', str(out),
                '--retirement-plans', str(source)])
        assert source.read_bytes() == source_bytes
        assert list(out.iterdir()) == [alias]
    out = base / 'safe'
    out.mkdir()
    source = out / 'reviewed-input.json'
    source.write_bytes(source_bytes)
    sweep.validate_inputs(repo, out, ['pkg', '.'], source)
    assert source.read_bytes() == source_bytes
"#;

const SIGTERM_CONTRACT: &str = r#"
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path
from tempfile import TemporaryDirectory

if os.name == 'posix' and Path('/proc').is_dir():
    with TemporaryDirectory() as directory:
        root = Path(directory)
        (root / 'pkg').mkdir()
        pids_path = root / 'pids.json'
        restored = root / 'restored'
        python = os.environ.get('PYO3_PYTHON') or sys.executable
        detector = ('import json,os,subprocess,sys,time; from pathlib import Path; '
                    'child=subprocess.Popen([sys.executable,"-c","import time; time.sleep(30)"]); '
                    f'Path({str(pids_path)!r}).write_text(json.dumps([os.getpid(),child.pid])); '
                    'time.sleep(30)')
        runner = f'''
import signal, sys
from pathlib import Path
from conductor.reuse import sweep
previous = lambda signum, frame: None
signal.signal(signal.SIGTERM, previous)
with open({str(root / 'success.log')!r}, 'w') as log:
    assert sweep.execute_detector([sys.executable, '-c', 'pass'], log, 5, environment=None) == 0
assert signal.getsignal(signal.SIGTERM) is previous
sweep.detector_commands = lambda *args: [('functions', [sys.executable, '-c', {detector!r}])]
try:
    sweep.main(sys.argv[1:])
finally:
    assert signal.getsignal(signal.SIGTERM) is previous
    Path({str(restored)!r}).write_text('restored')
'''
        environment = dict(os.environ)
        source_root = str(Path(sweep.__file__).resolve().parents[2])
        environment['PYTHONPATH'] = source_root + os.pathsep + environment.get('PYTHONPATH', '')
        process = subprocess.Popen([python, '-c', runner, '--repo', str(root), '--targets', 'pkg',
                                    '--quick', '--out-dir', str(root / 'out')],
                                   env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        pids = []
        try:
            deadline = time.monotonic() + 10
            while not pids_path.exists() and process.poll() is None and time.monotonic() < deadline:
                time.sleep(0.01)
            assert pids_path.exists(), 'detector did not start'
            pids = json.loads(pids_path.read_text())
            process.terminate()
            stdout, stderr = process.communicate(timeout=5)
            assert process.returncode == 128 + signal.SIGTERM, (stdout, stderr)
            assert restored.read_text() == 'restored'
            for pid in pids:
                stat = Path(f'/proc/{pid}/stat')
                deadline = time.monotonic() + 2
                while stat.exists() and stat.read_text().split()[2] != 'Z' and time.monotonic() < deadline:
                    time.sleep(0.01)
                assert not stat.exists() or stat.read_text().split()[2] == 'Z', f'descendant {pid} survived'
        finally:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=5)
            if pids:
                try:
                    os.killpg(pids[0], signal.SIGKILL)
                except ProcessLookupError:
                    pass
"#;

const RETIREMENT_CONTRACTS: &str = r#"
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

with TemporaryDirectory() as directory:
    root = Path(directory)
    (root / 'pkg').mkdir()
    source = root / 'plans.json'
    source.write_text(json.dumps({'schema_version': 1, 'plans': [{
        'id': 'merge', 'remove': ['pkg/a.py', 'pkg/b.py'], 'add': ['pkg/shared.py'],
        'evidence_ids': ['functions:C001', 'functions:C001']
    }]}))
    plans = sweep.load_retirement_plans(source)
    commands = sweep.detector_commands(root, root, ['pkg'], [], False, source)
    assert commands[1][1][-2:] == ['--retirement-plans', str(source)]
    assert '--retirement-plans' not in commands[0][1]
    functions = {'clusters': [{'id': 'C001', 'kind': 'exact', 'sites': [
        {'file': 'pkg/a.py', 'name': 'run', 'line_start': 1, 'line_end': 8},
        {'file': 'pkg/b.py', 'name': 'run', 'line_start': 1, 'line_end': 8}]},
        {'id': 'C002', 'kind': 'exact', 'sites': [
        {'file': 'pkg/a.py', 'name': 'other', 'line_start': 10, 'line_end': 18}]}]}
    (root / 'functions.json').write_text(json.dumps(functions))
    (root / 'families.json').write_text(json.dumps({'families': [{'id': 'F001', 'files': ['pkg/a.py', 'pkg/b.py']}]}))
    stages = {'functions': {'fresh_report': True, 'status': 'ok'},
              'families': {'fresh_report': True, 'status': 'ok'}}
    summary = sweep.write_retirement_report(plans, stages, root)
    assert summary['net_files_removed'] == 1
    report = json.loads((root / 'retirement-plans.json').read_text())
    assert report['plans'][0]['evidence_ids'] == ['families:F001', 'functions:C001', 'functions:C002']
    # A failed/malformed report and a stale report cannot contribute detector evidence.
    (root / 'families.json').write_text('malformed')
    stages['families']['status'] = 'failed'
    stages['functions']['fresh_report'] = False
    sweep.write_retirement_report(plans, stages, root)
    report = json.loads((root / 'retirement-plans.json').read_text())
    assert report['plans'][0]['evidence_ids'] == ['functions:C001']  # declared provenance only

    def run(name, command, output, *args):
        (output / 'functions.json').write_text(json.dumps(functions))
        return {'status': 'ok', 'fresh_report': True}
    out = root / 'quick'
    with patch.object(sweep, 'run_detector', side_effect=run), patch.object(sweep, 'native_inventory',
            return_value={'status': 'ok', 'graph_status': {'complete': True}}):
        assert sweep.main(['--repo', str(root), '--targets', 'pkg', '--quick',
                           '--out-dir', str(out), '--retirement-plans', str(source)]) == 0
    summary = json.loads((out / 'summary.json').read_text())
    assert summary['retirement_summary']['net_files_removed'] == 1
    assert summary['retirement_summary']['measurement_status'] == 'not_measured'
    assert (out / 'retirement-plan-input.json').is_file()
    assert json.loads((out / 'retirement-plans.json').read_text())['plans'][0]['evidence_ids'] == ['functions:C001', 'functions:C002']
"#;

const CONTRACTS: &str = r#"
import json
import subprocess
import os
import sys
import time
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import MagicMock, patch

with TemporaryDirectory() as directory:
    root = Path(directory)
    listing = b'pkg/core.py\0pkg/tests/test_core.py\0pkg/tests/nested/tests/test_nested.py\0native/src/lib.rs\0research/reports/generated.py\0'
    response = subprocess.CompletedProcess([], 0, stdout=listing)
    with patch.object(sweep.subprocess, 'run', return_value=response):
        scope = sweep.discover_scope(root, set(sweep.EXCLUDE))
    assert scope['targets'] == ['native', 'pkg', 'pkg/tests', 'pkg/tests/nested/tests']
    assert scope['eligible_python_files'] == 3
    assert scope['excluded_source_files'] == ['research/reports/generated.py']
    assert scope['source_files_discovered'] == 5

    commands = sweep.detector_commands(root, root, scope['targets'], scope['exclude'], False)
    assert [name for name, _ in commands] == ['functions', 'families']
    assert '--token-clone-full-files' in commands[0][1]
    assert commands[1][1][commands[1][1].index('--max-candidates') + 1] == '10000'
    quick = sweep.detector_commands(root, root, ['pkg'], [], True)
    assert len(quick) == 1 and '--with-token-clones' not in quick[0][1]

    def success(argv, *args, **kwargs):
        (root / 'functions.json').write_text(json.dumps({'summary': {'files_unparsable': 0}}))
        return 0
    with patch.object(sweep, 'execute_detector', side_effect=success):
        result = sweep.run_detector('functions', ['fixture'], root, 10)
    assert result['status'] == 'ok' and result['fresh_report']

    with patch.dict(sweep.os.environ, {}, clear=True), patch.object(sweep, 'execute_detector', side_effect=success) as child:
        result = sweep.run_detector('functions', ['fixture', '--with-token-clones'], root, 10)
    assert child.call_args.kwargs['environment']['NODE_OPTIONS'] == '--max-old-space-size=8192'
    assert result['node_options'] == '--max-old-space-size=8192'
    with patch.dict(sweep.os.environ, {'NODE_OPTIONS': '--max-old-space-size=2048'}), patch.object(sweep, 'execute_detector', side_effect=success) as child:
        sweep.run_detector('functions', ['fixture', '--with-token-clones'], root, 10)
    assert child.call_args.kwargs['environment']['NODE_OPTIONS'] == '--max-old-space-size=2048'

    with patch.object(sweep, 'execute_detector', return_value=0):
        result = sweep.run_detector('functions', ['fixture'], root, 10)
    assert result['status'] == 'failed' and not result['fresh_report']
    assert 'new report' in result['error']

    def partial(argv, *args, **kwargs):
        (root / 'functions.json').write_text(json.dumps({'complete': False, 'summary': {'files_unparsable': 1}}))
        return 2
    with patch.object(sweep, 'execute_detector', side_effect=partial):
        result = sweep.run_detector('functions', ['fixture'], root, 10)
    assert result['status'] == 'incomplete'

    with patch.object(sweep, 'execute_detector', side_effect=subprocess.TimeoutExpired(['fixture'], 10)):
        result = sweep.run_detector('functions', ['fixture'], root, 10)
    assert result['status'] == 'failed' and 'timed out' in result['error']

    (root / 'native-reuse.json').write_text('{"candidates":["old"]}')
    with patch.object(sweep, 'GraphIndex', side_effect=RuntimeError('missing graph')):
        result = sweep.native_inventory(root, ['pkg'], root)
    assert result['status'] == 'failed'
    assert json.loads((root / 'native-reuse.json').read_text())['error'] == 'missing graph'

    # A real absent database must produce a failed artifact, not escape with a
    # sqlite3.OperationalError and leave the preceding report behind.
    result = sweep.native_inventory(root, ['pkg'], root)
    assert result['status'] == 'failed' and 'unavailable' in result['error']
    index = MagicMock()
    index.status.return_value.as_dict.return_value = {'available': True, 'complete': False}
    with patch.object(sweep, 'GraphIndex', return_value=index), patch.object(sweep, 'native_reuse_candidates', return_value=[]):
        result = sweep.native_inventory(root, ['pkg'], root)
    index.status.assert_called_once_with(['.'])
    assert result['graph_status']['complete'] is False

    if os.name == 'posix' and Path('/proc').is_dir():
        # An actual grandchild must stop with its detector on timeout.
        pids_path = root / 'pids.json'
        python = os.environ.get('PYO3_PYTHON') or sys.executable
        script = ('import json,os,subprocess,sys,time; from pathlib import Path; '
                  'child=subprocess.Popen([sys.executable,"-c","import time; time.sleep(30)"]); '
                  f'Path({str(pids_path)!r}).write_text(json.dumps([os.getpid(),child.pid])); '
                  'time.sleep(30)')
        result = sweep.run_detector('functions', [python, '-c', script], root, 1)
        assert result['status'] == 'failed'
        pids = json.loads(pids_path.read_text())
        for pid in pids:
            stat = Path(f'/proc/{pid}/stat')
            deadline = time.monotonic() + 1
            while stat.exists() and stat.read_text().split()[2] != 'Z' and time.monotonic() < deadline:
                time.sleep(0.01)
            assert not stat.exists() or stat.read_text().split()[2] == 'Z', f'detector descendant {pid} survived'
"#;
