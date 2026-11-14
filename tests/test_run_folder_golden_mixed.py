"""The bridge from a Mixed Mode run to the analysis side, on a folder the
control service wrote: the golden Mixed Mode run folder
(`schema/golden/runs/mixed/`, written by the control service's own test
`mixed_golden_run` on the simulator) converts with
`hertzforge.io.run_convert` into a project that `Project.open` opens,
with the channel table that was set, every enabled input and the drive
as time histories, the floor as a random specification carrying the
narrowbands and the width left out around each tone, the tones as a
sine specification, the identification's transfer function and
coherence, the floor's spectra, and the run record holding the parts'
levels and the laws' calls; and `docs/run-folder.md` says exactly what a
Mixed Mode run saves.

The golden folder's streams are cut to their first frames, and say so
(`cut_from_frames`)."""

from __future__ import annotations

import json
import os
import re
import shutil

import numpy as np
import pytest

from hertzforge import Project
from hertzforge.core.channel_table import ChannelTable
from hertzforge.core.data import Coherence, Frf, Psd, Specification, TimeHistory
from hertzforge.core.mixed_mode import MixedSpecification
from hertzforge.core.run_record import RunRecord
from hertzforge.core.sine import SineSweepSpecification
from hertzforge.io import run_convert, run_folder

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN = os.path.join(ROOT, 'schema', 'golden', 'runs', 'mixed')
G = 9.80665


def _json(*parts):
    with open(os.path.join(GOLDEN, *parts), encoding='utf-8') as f:
        return json.load(f)


@pytest.fixture(scope='module')
def converted(tmp_path_factory):
    folder = str(tmp_path_factory.mktemp('runs') / 'mixed')
    shutil.copytree(GOLDEN, folder)
    done = run_convert.convert(folder)
    return folder, done, Project.open(done.path)


def test_the_mixed_folder_converts_and_opens_with_nothing_lacking(converted):
    _folder, done, project = converted
    assert done.lacked == []
    assert list(project) == done.objects
    assert project.project_type == 'Random and Sine'
    assert project.name == 'Golden Mixed Mode'


def test_the_mixed_channel_table_is_the_table_that_was_set(converted):
    _folder, _done, project = converted
    table = project['Channel Table']
    assert isinstance(table, ChannelTable) and table.has_controller_settings
    assert list(table['channel']) == [1, 2, 3]
    assert list(table['comment']) == ['Control', 'Monitor', 'Drive']
    assert list(table['control']) == ['True', 'False', 'False']
    assert list(table['drive']) == ['False', 'False', 'True']
    assert list(table['unit']) == ['g', 'g', 'V']
    rows = _json('table.json')['rows']
    back = run_convert.rows_from_channel_table(table)
    for given, read in zip(rows, back, strict=True):
        assert {k: v for k, v in read.items() if k != 'extra'} == given


def test_every_enabled_input_and_the_drive_is_a_mixed_time_history(converted):
    folder, _done, project = converted
    inputs, drives = project['inputs'], project['drives']
    assert isinstance(inputs, TimeHistory) and isinstance(drives, TimeHistory)
    side = _json('streams', 'inputs', 'stream.json')
    frames = side['frames']
    assert 0 < frames < side['cut_from_frames']
    assert inputs.ordinate.shape == (2, frames)
    assert drives.ordinate.shape == (1, _json('streams', 'drives', 'stream.json')['frames'])
    assert inputs.response_dof == ['1', '2'] and drives.response_dof == ['3']
    assert inputs.ordinate_unit == ['g', 'g'] and drives.ordinate_unit == ['V']
    assert inputs.source == drives.source == 'SIM'
    raw = np.fromfile(os.path.join(folder, 'streams', 'inputs', 'telemetry.f32'),
                      dtype='<f4').reshape(-1, 2)
    assert np.array_equal(inputs.ordinate, (raw.T.astype(np.float64) * G).astype(np.float32))
    assert inputs.abscissa[1] == pytest.approx(1 / 4096)
    assert inputs.run_identity.run == _json('run.json')['run_id']


def test_the_floor_and_its_parts_are_the_setups(converted):
    _folder, _done, project = converted
    floor, tones = project['Specification'], project['Tones']
    assert isinstance(floor, Specification) and isinstance(tones, SineSweepSpecification)
    assert floor.response_dof == ['1'] and tones.response_dof == ['1']
    resolved = _json('setup.json')['resolved']
    # the floor: flat across its band, in SI
    point = resolved['profile']['breakpoints'][0]
    level = floor.ordinate[0][np.argmin(np.abs(floor.abscissa - 300.0))]
    assert level == pytest.approx(point['psd_g2_hz'] * G * G, rel=1e-6)
    # the narrowband, swept one pass from the band's low edge, its level in SI
    parts = {p['id']: p for p in resolved['parts']}
    (band,) = floor.narrowbands
    assert band.name == 'Mesh' and band.center == parts['b1']['schedule']['band_hz'][0]
    assert band.sweep_to == parts['b1']['schedule']['band_hz'][1]
    assert band.rate == parts['b1']['schedule']['rate']['hz_per_s']
    assert band.width == parts['b1']['width_hz']
    assert band.level == pytest.approx(parts['b1']['level_g2_hz'] * G * G)
    assert floor.tone_exclusion == resolved['control']['exclusion_hz']
    # the tone, fixed, at its amplitude in SI
    (tone,) = tones.tones
    assert tone.name == 'Blade'
    assert np.allclose(tone.frequency, parts['t1']['schedule']['freq_hz'], rtol=1e-5)
    assert np.allclose(tone.amplitude, parts['t1']['amplitude_g_pk'] * G)
    # the analysis puts the two back together as one requirement
    spec = MixedSpecification(floor, tones)
    assert spec.narrowbands == (band,) and spec.exclusion == floor.tone_exclusion


def test_the_mixed_identification_and_spectra_are_there(converted):
    _folder, _done, project = converted
    frf = project['identification_frf']
    assert isinstance(frf, Frf)
    assert frf.response_dof == ['1'] and frf.reference_dof == ['3']
    assert np.iscomplexobj(frf.ordinate) and np.all(np.isfinite(frf.ordinate))
    assert frf.ordinate_dim == ['acceleration/voltage']
    coherence = project['identification_coherence']
    assert isinstance(coherence, Coherence)
    assert np.all((coherence.ordinate >= 0) & (coherence.ordinate <= 1))
    for name in ('control_psd', 'residual_psd', 'drive_psd'):
        assert isinstance(project[name], Psd), name
        assert project[name].source == 'SIM'
    lines = run_folder.read(GOLDEN).results['control_psd'].records
    assert [line['why'] for line in lines] == ['level', 'end']


def test_the_mixed_run_record_holds_the_setup_the_log_the_levels_and_the_outcome(converted):
    _folder, _done, project = converted
    record = project['Run Record']
    assert isinstance(record, RunRecord)
    assert record.setup == _json('setup.json')
    assert record.config['operator'] == 'Operator'
    with open(os.path.join(GOLDEN, 'log.jsonl')) as f:
        log = [json.loads(line) for line in f]
    assert record.log == log
    assert [e['seq'] for e in record.log] == list(range(len(log)))
    assert record.complete and record.outcome == 'completed' == _json('end.json')['end'][
        'outcome']
    assert record.source == 'SIM' and record.test == 'mixed'
    for name in ('reference', 'limit_ch2', 'tone_levels', 'band_levels', 'floor_levels',
                 'control_trace'):
        assert name in record.arrays, name
    tones = record.arrays['tone_levels']
    assert tones['phase'].shape[-1] == tones['measured'].shape[-1] == tones['time'].shape[-1]
    assert np.nanmax(tones['frequency']) == pytest.approx(200.0)
    bands = record.arrays['band_levels']
    moved = bands['frequency'][np.isfinite(bands['frequency'])]
    assert moved.min() >= 350.0 and moved.max() > moved.min()
    trace = record.arrays['control_trace']
    assert set(np.unique(trace['role'][np.isfinite(trace['role'])])) == {0.0, 1.0}
    assert record.missing == []


def test_save_and_reopen_loses_nothing_of_a_mixed_run(converted, tmp_path):
    _folder, _done, project = converted
    again = Project.open(project.save(tmp_path / 'again.hforge'))
    assert list(again) == list(project)
    for name in project:
        assert again[name] == project[name], name
    assert again.object_groups == project.object_groups


def _saved_by_the_guide():
    with open(os.path.join(ROOT, 'docs', 'run-folder.md'), encoding='utf-8') as f:
        text = f.read()
    section = text.split('## What a Mixed Mode Run Saves', 1)[1].split('\n## ', 1)[0]
    return re.findall(r'^\| `([^`]+)` \|', section, flags=re.MULTILINE)


def test_the_guide_says_exactly_what_a_mixed_run_saves():
    named = _saved_by_the_guide()
    assert named, 'the guide lists nothing'
    run = run_folder.read(GOLDEN)
    on_disk = ([f'streams/{name}' for name in run.streams]
               + [f'results/{name}' for name in run.results if not name.startswith('limit_ch')]
               + ['end.json'])
    assert sorted(named) == sorted(on_disk)
    assert [n for n in run.results if n.startswith('limit_ch')] == ['limit_ch2']


def test_a_part_no_analysis_class_holds_stays_in_the_setup_and_is_named(tmp_path):
    # a follower and a tone whose amplitude is a profile: neither is a tone row
    folder = str(tmp_path / 'mixed')
    shutil.copytree(GOLDEN, folder)
    path = os.path.join(folder, 'setup.json')
    with open(path, encoding='utf-8') as f:
        setup = json.load(f)
    parts = setup['resolved']['parts']
    parts.append({'id': 't2', 'kind': 'tone', 'name': 'Second', 'schedule':
                  {'kind': 'follow', 'of': 't1', 'ratio': 2}, 'amplitude_g_pk': 0.1})
    parts.append({'id': 't3', 'kind': 'tone', 'name': 'Shaped', 'schedule':
                  {'kind': 'fixed', 'freq_hz': 300, 'end': None}, 'profile': {}})
    with open(path, 'w', encoding='utf-8') as f:
        json.dump(setup, f)
    done = run_convert.convert(folder)
    assert len(done.lacked) == 2
    assert 'Second (follow)' in done.lacked[0] and 'stays in the setup' in done.lacked[0]
    assert 'Shaped' in done.lacked[1] and 'profile' in done.lacked[1]
    project = Project.open(done.path)
    assert [t.name for t in project['Tones'].tones] == ['Blade']
    assert project['Run Record'].setup['resolved']['parts'][-1]['id'] == 't3'
