"""The bridge from an SRS shock run to the analysis side, on a folder the
control service wrote: the golden SRS run folder (`schema/golden/runs/srs/`,
written by the control service's own test `srs_golden_run` on the
simulator) converts with `hertzforge.io.run_convert` into a project that
`Project.open` opens, with the channel table that was set, every enabled
input and the drive as time histories of one record per shot timed from its
firing, an `Srs` for every pulse, the target SRS with its alarm and abort
bands as a `ShockSpecification`, the identification's transfer function and
coherence, and the run record with the setup, the log, every pulse's verdict
and the outcome; and `docs/run-folder.md` says exactly what an SRS run
saves."""

from __future__ import annotations

import json
import os
import re
import shutil

import numpy as np
import pytest

from hertzforge import Project
from hertzforge.core.data import Coherence, Frf, ShockSpecification, Srs, TimeHistory
from hertzforge.core.run_record import RunRecord
from hertzforge.io import run_convert, run_folder

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN = os.path.join(ROOT, 'schema', 'golden', 'runs', 'srs')
G = 9.80665


def _json(*parts):
    with open(os.path.join(GOLDEN, *parts), encoding='utf-8') as f:
        return json.load(f)


def _lines(*parts):
    with open(os.path.join(GOLDEN, *parts), encoding='utf-8') as f:
        return [json.loads(line) for line in f]


@pytest.fixture(scope='module')
def converted(tmp_path_factory):
    folder = str(tmp_path_factory.mktemp('runs') / 'srs')
    shutil.copytree(GOLDEN, folder)
    done = run_convert.convert(folder)
    return folder, done, Project.open(done.path)


def test_the_srs_folder_converts_and_opens_with_nothing_lacking(converted):
    _folder, done, project = converted
    assert done.lacked == []
    assert list(project) == done.objects
    assert project.project_type == 'Shock'
    assert project.name == 'Golden SRS'


def test_the_srs_channel_table_is_the_table_that_was_set(converted):
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
def test_each_srs_shot_is_a_record_of_its_length_unit_and_source(converted, stream, width,
                                                                 unit, scale):
    folder, _done, project = converted
    side = _json('streams', stream, 'stream.json')
    raw = np.fromfile(os.path.join(folder, 'streams', stream, 'telemetry.f32'),
                      dtype='<f4').reshape(-1, width)
    pulses = [s for s in side['shots'] if s['what'] == 'pulse']
    identification = [s for s in side['shots'] if s['what'] == 'identification']
    assert len(pulses) == len(_lines('results', 'pulses.jsonl')) == 2
    assert len(identification) == 1
    assert len(side['shots']) == 3
    history = project[f'{stream} pulse']
    assert isinstance(history, TimeHistory)
    frames = pulses[0]['frames']
    assert frames == _json('run.json')['grid']['record_len']
    assert history.ordinate.shape == (width * len(pulses), frames)
    assert history.ordinate_unit == [unit] * (width * len(pulses))
    assert history.source == 'SIM'
    assert history.block == [f'pulse {s["pulse"] + 1}' for _c in range(width) for s in pulses]
    # an SRS pulse fires at its first sample
    assert history.abscissa[0] == 0.0
    assert history.abscissa[1] - history.abscissa[0] == pytest.approx(1 / side['fs_hz'])
    for k, shot in enumerate(pulses):
        span = raw[shot['first_frame']:shot['first_frame'] + shot['frames']]
        for c in range(width):
            want = (span[:, c].astype(np.float64) * scale).astype(np.float32)
            assert np.array_equal(history.ordinate[c * len(pulses) + k], want), (k, c)
    ident = project[f'{stream} identification']
    assert ident.ordinate.shape == (width, identification[0]['frames'])
    assert ident.abscissa[0] == 0.0


def test_the_srs_of_every_pulse_and_the_target_with_its_bands_are_there(converted):
    folder, _done, project = converted
    srs = project['pulse_srs']
    assert isinstance(srs, Srs) and srs.kind == 'maximax'
    pulses = _lines('results', 'pulses.jsonl')
    assert srs.ordinate.shape[0] == len(pulses)
    assert srs.block == [f'pulse {p["index"] + 1}' for p in pulses]
    assert srs.response_dof == ['1'] * len(pulses)
    run = run_folder.read(folder)
    measured = run.results['pulse_srs'].arrays['values'][:, 0, :]
    assert np.allclose(srs.ordinate, measured * G)
    assert np.allclose(srs.abscissa, run.results['pulse_srs'].arrays['frequency'][0])
    spec = project['srs_reference']
    assert isinstance(spec, ShockSpecification)
    assert spec.q == _json('setup.json')['resolved']['bands']['q'] == 10
    reference = run.results['srs_reference'].arrays
    assert np.allclose(spec.abscissa, _json('setup.json')['resolved']['bands']['centres_hz'])
    assert np.allclose(spec.ordinate[0], reference['reference'] * G)
    for array, limit in (('band_low', 'warning_lower'), ('band_high', 'warning_upper'),
                         ('abort_low', 'abort_lower'), ('abort_high', 'abort_upper')):
        assert np.allclose(spec.limits[limit][0], reference[array] * G), limit
    assert np.all(spec.limits['abort_upper'] > spec.limits['warning_upper'])
    frf = project['identification_frf']
    assert isinstance(frf, Frf)
    assert np.iscomplexobj(frf.ordinate) and np.any(frf.ordinate.imag != 0)
    assert frf.reference_dof == ['3']
    assert isinstance(project['identification_coherence'], Coherence)


def test_the_srs_run_record_holds_the_setup_the_log_every_verdict_and_the_outcome(converted):
    _folder, _done, project = converted
    record = project['Run Record']
    assert isinstance(record, RunRecord)
    assert record.setup == _json('setup.json')
    assert record.resolved['target']['kind'] == 'flat'
    assert record.config['operator'] == 'Operator'
    log = _lines('log.jsonl')
    assert record.log == log
    assert [e['seq'] for e in record.log] == list(range(len(log)))
    fired = [e for e in record.log if e['kind'] == 'pulse']
    assert [e['data']['pulse'] for e in fired] == [0, 1]
    pulses = _lines('results', 'pulses.jsonl')
    assert [e['data']['passed'] for e in fired] == [p['passed'] for p in pulses]
    held = record.arrays['pulses']
    assert held['passed'].shape == (len(pulses), 1)
    assert list(held['passed'][:, 0]) == [1.0 if p['passed'] else 0.0 for p in pulses]
    # the golden run has a pulse that failed and one that passed
    assert [p['passed'] for p in pulses] == [False, True]
    assert np.all(np.isfinite(held['worst_error'])) and np.all(held['peak'] > 0)
    assert record.results['pulses']['records'] == pulses
    # the synthesis and the drive tables have no analysis class: arrays
    synthesis = record.arrays['synthesis']
    assert set(synthesis) == {'time', 'waveform', 'velocity', 'displacement',
                              'band_frequency', 'achieved'}
    assert synthesis['time'][0] == 0.0 and np.max(np.abs(synthesis['waveform'])) > 0
    tables = record.arrays['drive_tables']
    assert tables['amplitude'].shape[0] == len(pulses)
    # the target's band and the per-pulse band are kept with the record too
    assert set(record.arrays['pulse_srs']) == {'reference', 'band_low', 'band_high',
                                               'abort_low', 'abort_high'}
    assert record.complete and record.outcome == 'completed' == _json('end.json')['end'][
        'outcome']
    assert record.source == 'SIM' and record.test == 'srs'
    assert record.missing == []


def test_save_and_reopen_an_srs_project_loses_nothing(converted, tmp_path):
    _folder, _done, project = converted
    again = Project.open(project.save(tmp_path / 'again.hforge'))
    assert list(again) == list(project)
    for name in project:
        assert again[name] == project[name], name
    assert again.object_groups == project.object_groups


def _saved_by_the_guide():
    with open(os.path.join(ROOT, 'docs', 'run-folder.md'), encoding='utf-8') as f:
        text = f.read()
    section = text.split('## What an SRS Run Saves', 1)[1].split('\n## ', 1)[0]
    return re.findall(r'^\| `([^`]+)` \|', section, flags=re.MULTILINE)


def test_the_guide_says_exactly_what_an_srs_run_saves():
    named = _saved_by_the_guide()
    assert named, 'the guide lists nothing'
    run = run_folder.read(GOLDEN)
    on_disk = ([f'streams/{name}' for name in run.streams]
               + [f'results/{name}' for name in run.results] + ['end.json'])
    assert sorted(named) == sorted(on_disk)
