"""The bridge from a TWR run to the analysis side, on a folder the control
service wrote: the golden TWR run folder (`schema/golden/runs/twr/`,
written by the control service's own test `twr_golden_run` on the
simulator) converts with `hertzforge.io.run_convert` into a project that
`Project.open` opens, with the channel table that was set, every enabled
input and the drive as time histories of one record per play timed from
the section's start, the target as a `TransientSpecification`, the
identification's transfer function and coherence, and the run record with
the setup, the log, every play's measures and the outcome; and
`docs/run-folder.md` says exactly what a TWR run saves."""

from __future__ import annotations

import json
import os
import re
import shutil

import numpy as np
import pytest

from hertzforge import Project
from hertzforge.core.data import Coherence, Frf, TimeHistory, TransientSpecification
from hertzforge.core.run_record import RunRecord
from hertzforge.io import run_convert, run_folder

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN = os.path.join(ROOT, 'schema', 'golden', 'runs', 'twr')
G = 9.80665


def _json(*parts):
    with open(os.path.join(GOLDEN, *parts), encoding='utf-8') as f:
        return json.load(f)


def _lines(*parts):
    with open(os.path.join(GOLDEN, *parts), encoding='utf-8') as f:
        return [json.loads(line) for line in f]


@pytest.fixture(scope='module')
def converted(tmp_path_factory):
    folder = str(tmp_path_factory.mktemp('runs') / 'twr')
    shutil.copytree(GOLDEN, folder)
    done = run_convert.convert(folder)
    return folder, done, Project.open(done.path)


def test_the_folder_converts_and_opens_with_nothing_lacking(converted):
    _folder, done, project = converted
    assert done.lacked == []
    assert list(project) == done.objects
    assert project.project_type == 'Transient'
    assert project.name == 'Golden TWR'


def test_the_channel_table_is_the_table_that_was_set(converted):
    _folder, _done, project = converted
    table = project['Channel Table']
    assert table.has_controller_settings
    assert list(table['channel']) == [1, 2, 3]
    assert list(table['comment']) == ['Control', 'Monitor', 'Drive']
    assert list(table['control']) == ['True', 'False', 'False']
    assert list(table['drive']) == ['False', 'False', 'True']
    rows = _json('table.json')['rows']
    back = run_convert.rows_from_channel_table(table)
    for given, read in zip(rows, back, strict=True):
        assert {k: v for k, v in read.items() if k != 'extra'} == given


@pytest.mark.parametrize('stream, width, unit, scale', [
    ('inputs', 2, 'g', G), ('drives', 1, 'V', 1.0)])
def test_each_play_is_a_record_of_its_length_unit_and_source(converted, stream, width,
                                                            unit, scale):
    folder, _done, project = converted
    side = _json('streams', stream, 'stream.json')
    raw = np.fromfile(os.path.join(folder, 'streams', stream, 'telemetry.f32'),
                      dtype='<f4').reshape(-1, width)
    plays = [s for s in side['shots'] if s['what'] == 'play']
    identification = [s for s in side['shots'] if s['what'] == 'identification']
    rows = _lines('results', 'plays.jsonl')
    # every play has its shot, the last included, in order
    assert [s['play'] for s in plays] == [r['index'] for r in rows] == list(range(len(rows)))
    assert [s['level_db'] for s in plays] == [r['level_db'] for r in rows]
    assert [s['kind'] for s in plays] == [r['kind'] for r in rows]
    assert len(identification) == 1
    history = project[f'{stream} play']
    assert isinstance(history, TimeHistory)
    frames = plays[0]['frames']
    assert all(s['frames'] == frames for s in plays)
    assert history.ordinate.shape == (width * len(plays), frames)
    assert history.ordinate_unit == [unit] * (width * len(plays))
    assert history.source == 'SIM'
    assert history.block == [f'play {s["play"] + 1}' for _c in range(width) for s in plays]
    # timed from the section's start: the pad before it is negative time
    assert history.abscissa[0] == pytest.approx(-plays[0]['pre_frames'] / side['fs_hz'])
    assert history.abscissa[1] - history.abscissa[0] == pytest.approx(1 / side['fs_hz'])
    for k, shot in enumerate(plays):
        span = raw[shot['first_frame']:shot['first_frame'] + shot['frames']]
        for c in range(width):
            want = (span[:, c].astype(np.float64) * scale).astype(np.float32)
            assert np.array_equal(history.ordinate[c * len(plays) + k], want), (k, c)
    # the drive is a reference, every input a response
    for (dof, _dim), role in history.roles.items():
        assert role == ('reference' if stream == 'drives' else 'response'), dof
    ident = project[f'{stream} identification']
    assert ident.ordinate.shape == (width, identification[0]['frames'])
    assert ident.abscissa[0] == 0.0


def test_the_target_is_a_transient_specification_in_si(converted):
    folder, _done, project = converted
    spec = project['Specification']
    assert isinstance(spec, TransientSpecification)
    run = run_folder.read(folder)
    target = run.results['target']
    assert np.allclose(spec.ordinate[0], target.arrays['reference'] * G)
    assert spec.ordinate_unit == ['g']
    assert spec.response_dof == ['1']
    assert spec.source is None
    record = _json('setup.json')['resolved']['record']
    assert spec.ordinate.shape == (1, record['record_len'])
    assert spec.abscissa[record['section_start']] == pytest.approx(0.0)
    # the reference as imported stays in the run record, at its own rate
    reference = project['Run Record'].arrays['reference']
    resolved = _json('setup.json')['resolved']['reference']
    assert reference['values'].shape == (resolved['samples'],)
    assert reference['time'][1] == pytest.approx(1 / resolved['fs_hz'])
    assert isinstance(project['identification_frf'], Frf)
    assert isinstance(project['identification_coherence'], Coherence)
    assert project['identification_frf'].reference_dof == ['3']


def test_the_run_record_holds_the_setup_the_log_every_play_and_the_outcome(converted):
    _folder, _done, project = converted
    record = project['Run Record']
    assert isinstance(record, RunRecord)
    assert record.setup == _json('setup.json')
    assert record.resolved['schedule']['levels_db'] == [-6, 0]
    assert record.config['operator'] == 'Operator'
    log = _lines('log.jsonl')
    assert record.log == log
    assert [e['seq'] for e in record.log] == list(range(len(log)))
    rows = _lines('results', 'plays.jsonl')
    played = [e for e in record.log if e['kind'] == 'play']
    assert [e['data']['play'] for e in played] == [r['index'] for r in rows]
    assert [e['data']['level_db'] for e in played] == [r['level_db'] for r in rows]
    held = record.arrays['plays']
    assert held['rms_error'].shape == (len(rows), 1)
    assert list(held['gated'][:, 0]) == [1.0 if r['gated'] else 0.0 for r in rows]
    assert list(held['level_db'][:, 0]) == [r['level_db'] for r in rows]
    assert np.all(np.isfinite(held['rms_error'])) and np.all(held['drive_peak'] > 0)
    assert record.results['plays']['records'] == rows
    calls = _lines('results', 'law_calls.jsonl')
    assert calls[0]['call'] == 'seed'
    assert record.arrays['law_calls']['drive'].shape == (
        len(calls), _json('setup.json')['resolved']['record']['record_len'])
    assert record.complete and record.outcome == 'completed' == _json('end.json')['end'][
        'outcome']
    assert record.source == 'SIM' and record.test == 'twr'
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
    section = text.split('## What a TWR Run Saves', 1)[1].split('\n## ', 1)[0]
    return re.findall(r'^\| `([^`]+)` \|', section, flags=re.MULTILINE)


def test_the_guide_says_exactly_what_a_twr_run_saves():
    named = _saved_by_the_guide()
    assert named, 'the guide lists nothing'
    run = run_folder.read(GOLDEN)
    on_disk = ([f'streams/{name}' for name in run.streams]
               + [f'results/{name}' for name in run.results] + ['end.json'])
    assert sorted(named) == sorted(on_disk)
