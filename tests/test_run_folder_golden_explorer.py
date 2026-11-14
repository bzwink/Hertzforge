"""The bridge from an Explorer run to the analysis side, on a folder the
control service wrote: the golden Explorer run folder
(`schema/golden/runs/explorer/`, written by the control service's own
test `explorer_golden_run` on the simulator) converts with
`hertzforge.io.run_convert` into a project that `Project.open` opens,
with the channel table that was set, the inputs and the drive as time
histories in SI with their units and source, the survey line and the
spectra in the run record as named arrays (magnitudes with no phase,
which no analysis class holds), and the run record with the setup, the
log in order with every steering verb, and the outcome; and
`docs/run-folder.md` says exactly what an Explorer run saves."""

from __future__ import annotations

import json
import os
import re
import shutil

import numpy as np
import pytest

from hertzforge import Project
from hertzforge.core.channel_table import ChannelTable
from hertzforge.core.data import TimeHistory
from hertzforge.core.run_record import RunRecord
from hertzforge.io import run_convert, run_folder

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN = os.path.join(ROOT, 'schema', 'golden', 'runs', 'explorer')
G = 9.80665


def _json(*parts, root=GOLDEN):
    with open(os.path.join(root, *parts), encoding='utf-8') as f:
        return json.load(f)


@pytest.fixture(scope='module')
def converted(tmp_path_factory):
    folder = str(tmp_path_factory.mktemp('runs') / 'explorer')
    shutil.copytree(GOLDEN, folder)
    done = run_convert.convert(folder)
    return folder, done, Project.open(done.path)


def test_the_folder_converts_and_opens_with_nothing_lacking(converted):
    _folder, done, project = converted
    assert done.lacked == []
    assert list(project) == done.objects == ['Channel Table', 'inputs', 'drives',
                                             'Run Record']
    assert project.name == 'Golden Explorer'
    run = run_folder.read(GOLDEN)
    assert run.test == 'explorer' and run.source == 'SIM'
    assert _json('run.json')['complete'] is True


def test_the_channel_table_is_the_table_that_was_set(converted):
    _folder, _done, project = converted
    table = project['Channel Table']
    assert isinstance(table, ChannelTable) and table.has_controller_settings
    assert list(table['channel']) == [1, 2, 3]
    assert list(table['comment']) == ['Control', 'Drive', 'Fixture']
    assert list(table['unit']) == ['g', 'V', 'g']
    rows = _json('table.json')['rows']
    back = run_convert.rows_from_channel_table(table)
    for given, read in zip(rows, back, strict=True):
        assert {k: v for k, v in read.items() if k != 'extra'} == given


def test_the_inputs_and_the_drive_are_time_histories_in_si(converted):
    folder, _done, project = converted
    inputs, drives = project['inputs'], project['drives']
    side = _json('streams', 'inputs', 'stream.json')
    fs, frames = side['fs_hz'], side['frames']
    assert isinstance(inputs, TimeHistory) and isinstance(drives, TimeHistory)
    assert inputs.ordinate.shape == (2, frames)
    assert drives.ordinate.shape == (1, frames)
    assert inputs.response_dof == ['1', '3'] and drives.response_dof == ['2']
    assert inputs.ordinate_unit == ['g', 'g'] and drives.ordinate_unit == ['V']
    assert inputs.ordinate_dim == ['acceleration', 'acceleration']
    assert inputs.source == drives.source == 'SIM'
    # the drive is the reference
    assert drives.roles == {('2', 'voltage'): 'reference'}
    assert inputs.abscissa[1] - inputs.abscissa[0] == pytest.approx(1 / fs)
    raw = np.fromfile(os.path.join(folder, 'streams', 'inputs', 'telemetry.f32'),
                      dtype='<f4').reshape(-1, 2)
    assert np.array_equal(inputs.ordinate, (raw.T.astype(np.float64) * G).astype(np.float32))
    drive = np.fromfile(os.path.join(folder, 'streams', 'drives', 'telemetry.f32'), dtype='<f4')
    # the golden keeps the first frames, before the first steer: 0 V
    assert np.array_equal(drives.ordinate[0], drive) and not drive.any()
    assert inputs.run_identity.run == _json('run.json')['run_id']


def test_the_survey_and_the_spectra_are_kept_as_arrays(converted):
    _folder, _done, project = converted
    record = project['Run Record']
    survey, spectra = record.arrays['survey'], record.arrays['spectra']
    assert survey['frequency'].tolist() == [200.0]
    assert survey['level'].shape == (1,)
    assert record.results['survey']['kind'] == 'survey'
    assert record.results['survey']['channels'] == {'responses': [1]}
    assert [a['unit'] for a in record.results['survey']['arrays']] == ['Hz', 'g']
    lines = 1024 // 2 + 1
    assert spectra['values'].shape == (2, lines)
    described = record.results['spectra']
    assert described['kind'] == 'amplitude_spectrum'
    assert described['channels'] == {'responses': [1, 3]}
    assert described['abscissa']['step'] == pytest.approx(2048 / 1024)
    assert described['arrays'][0]['unit'] == 'g'
    # no analysis object stands for either: neither has a phase
    assert not any(name in project for name in ('survey', 'spectra'))


def test_the_run_record_holds_the_setup_the_log_and_the_outcome(converted):
    _folder, _done, project = converted
    record = project['Run Record']
    assert isinstance(record, RunRecord)
    assert record.test == 'explorer' and record.source == 'SIM'
    assert record.setup == _json('setup.json')
    assert record.config['start_hz'] == 200 and record.config['max_drive_vpk'] == 0.5
    assert record.manifest['operator'] == 'Operator'
    assert record.manifest['specification'] == {'document': 'Example procedure',
                                                'clause': '4.2'}
    with open(os.path.join(GOLDEN, 'log.jsonl'), encoding='utf-8') as f:
        log = [json.loads(line) for line in f]
    assert record.log == log
    assert [e['seq'] for e in record.log] == list(range(len(log)))
    intents = [e['data'] for e in record.log if e['kind'] == 'intent']
    assert [i['verb'] for i in intents] == ['steer', 'stop']
    assert intents[0]['vpk'] == {'asked': 0.1, 'adopted': 0.1, 'clamped': False}
    end = _json('end.json')
    assert record.complete and record.outcome == 'operator_stop' == end['end']['outcome']
    assert end['end']['at_hz'] == 200 and end['end']['vpk'] == 0.1
    assert [c['index'] for c in end['measured']['channels']] == [1, 3]
    assert record.streams['inputs'] == _json('streams', 'inputs', 'stream.json')
    assert record.missing == []


def test_save_and_reopen_loses_nothing(converted, tmp_path):
    _folder, _done, project = converted
    again = Project.open(project.save(tmp_path / 'again.hforge'))
    assert list(again) == list(project)
    for name in project:
        assert again[name] == project[name], name
    assert again.object_groups == project.object_groups


def _saved_by_the_guide():
    with open(os.path.join(ROOT, 'docs', 'run-folder.md'), encoding='utf-8') as f:
        text = f.read()
    section = text.split('## What an Explorer Run Saves', 1)[1].split('\n## ', 1)[0]
    return dict(re.findall(r'^\| `([^`]+)` \| (.+) \|$', section, flags=re.MULTILINE))


def test_the_guide_says_exactly_what_an_explorer_run_saves():
    named = _saved_by_the_guide()
    assert named, 'the guide lists nothing'
    run = run_folder.read(GOLDEN)
    on_disk = {f'streams/{n}' for n in run.streams} | {f'results/{n}' for n in run.results}
    on_disk.add('end.json')
    assert set(named) == on_disk
    for name, result in run.results.items():
        assert f'`kind` `{result.kind}`' in named[f'results/{name}'], name
    for name, stream in run.streams.items():
        assert f'`kind` `{stream.kind}`' in named[f'streams/{name}'], name
