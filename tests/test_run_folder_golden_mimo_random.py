"""The bridge from a MIMO Random run to the analysis side, on a folder the
control service wrote: the golden MIMO Random run folder
(`schema/golden/runs/mimo_random/`, written by the control service's own
test `mimo_random_golden_run` on the simulator) converts with
`hertzforge.io.run_convert` into a project that `Project.open` opens,
with the channel table that was set, every enabled input and every drive
as time histories, the reference spectral density matrix the run was
controlled to as a specification with its cross terms, the
identification's transfer function matrix, coherence and spectral
matrices, each loop's spectra, the full spectral matrices and the law's
calls, and the run record; and `docs/run-folder.md` says exactly what a
MIMO Random run saves.

The golden folder's streams are cut to their first frames, and say so
(`cut_from_frames`). The run is simulated."""

from __future__ import annotations

import json
import os
import re
import shutil

import numpy as np
import pytest

from hertzforge import Project
from hertzforge.core.channel_table import ChannelTable
from hertzforge.core.data import Frf, MultipleCoherence, Psd, Specification, TimeHistory
from hertzforge.core.run_record import RunRecord
from hertzforge.io import run_convert, run_folder

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLDEN = os.path.join(ROOT, 'schema', 'golden', 'runs', 'mimo_random')
G = 9.80665


def _json(*parts):
    with open(os.path.join(GOLDEN, *parts), encoding='utf-8') as f:
        return json.load(f)


def _lines(*parts):
    with open(os.path.join(GOLDEN, *parts), encoding='utf-8') as f:
        return [json.loads(line) for line in f]


@pytest.fixture(scope='module')
def converted(tmp_path_factory):
    folder = str(tmp_path_factory.mktemp('runs') / 'mimo_random')
    shutil.copytree(GOLDEN, folder)
    done = run_convert.convert(folder)
    return folder, done, Project.open(done.path)


@pytest.fixture(scope='module')
def run():
    return run_folder.read(GOLDEN)


def test_the_folder_converts_and_opens_with_nothing_lacking(converted):
    _folder, done, project = converted
    assert done.lacked == []
    assert list(project) == done.objects
    # the workspace the analysis side opens random vibration in
    assert project.project_type == 'Random Vibration'
    assert project.name == 'Golden MIMO Random'


def test_the_channel_table_is_the_table_that_was_set(converted):
    _folder, _done, project = converted
    table = project['Channel Table']
    assert isinstance(table, ChannelTable) and table.has_controller_settings
    assert list(table['channel']) == [1, 2, 3, 4, 5, 6, 7, 8]
    assert list(table['control']) == ['True'] * 4 + ['False'] * 4
    assert list(table['drive']) == ['False'] * 4 + ['True'] * 4
    assert list(table['unit']) == ['g'] * 4 + ['V'] * 4
    rows = _json('table.json')['rows']
    back = run_convert.rows_from_channel_table(table)
    for given, read in zip(rows, back, strict=True):
        assert {k: v for k, v in read.items() if k != 'extra'} == given


def test_every_input_and_every_drive_is_a_time_history(converted):
    folder, _done, project = converted
    inputs, drives = project['inputs'], project['drives']
    assert isinstance(inputs, TimeHistory) and isinstance(drives, TimeHistory)
    side = _json('streams', 'inputs', 'stream.json')
    frames = side['frames']
    assert 0 < frames < side['cut_from_frames']
    assert inputs.ordinate.shape == (4, frames)
    assert drives.ordinate.shape == (4, _json('streams', 'drives', 'stream.json')['frames'])
    assert inputs.response_dof == ['1', '2', '3', '4']
    assert drives.response_dof == ['5', '6', '7', '8']
    assert inputs.ordinate_unit == ['g'] * 4 and drives.ordinate_unit == ['V'] * 4
    assert inputs.source == drives.source == 'SIM'
    assert set(drives.roles.values()) == {'reference'}
    raw = np.fromfile(os.path.join(folder, 'streams', 'inputs', 'telemetry.f32'),
                      dtype='<f4').reshape(-1, 4)
    assert np.array_equal(inputs.ordinate, (raw.T.astype(np.float64) * G).astype(np.float32))
    assert inputs.abscissa[1] == pytest.approx(1 / 4096)
    assert inputs.run_identity.run == _json('run.json')['run_id']


def test_the_reference_is_a_specification_with_its_cross_terms(converted, run):
    _folder, _done, project = converted
    spec = project['Specification']
    assert isinstance(spec, Specification)
    reference = run.results['reference']
    values = reference.arrays['values']
    n = values.shape[0]
    pairs = [(a, b) for a in range(n) for b in range(a, n)]
    assert list(zip(spec.response_dof, spec.reference_dof, strict=True)) == [
        (str(a + 1), str(b + 1)) for a, b in pairs]
    # in SI, cross terms as written (the file's convention), phase kept
    for k, (a, b) in enumerate(pairs):
        assert np.allclose(spec.ordinate[k], values[a, b] * G * G)
    assert np.iscomplexobj(spec.ordinate)
    assert np.any(np.abs(np.imag(spec.ordinate[1])) > 0)
    # the band's lines, and the setup's band
    resolved = _json('setup.json')['resolved']['reference']
    assert spec.abscissa[0] == pytest.approx(resolved['band_hz'][0])
    assert spec.abscissa[-1] == pytest.approx(resolved['band_hz'][1])
    # each control's alarm and abort lines on its autospectrum, none on a cross term
    for k, (a, b) in enumerate(pairs):
        if a == b:
            assert np.allclose(spec.limits['abort_upper'][k],
                               reference.arrays['abort_upper'][a] * G * G)
            assert np.allclose(spec.limits['warning_lower'][k],
                               reference.arrays['alarm_lower'][a] * G * G)
        else:
            assert np.all(np.isnan(spec.limits['abort_upper'][k]))


def test_the_identification_is_there_as_analysis_objects(converted, run):
    _folder, _done, project = converted
    frf = project['identification_frf']
    assert isinstance(frf, Frf)
    assert frf.ordinate.shape[0] == 16
    assert frf.response_dof[:4] == ['1'] * 4 and frf.reference_dof[:4] == ['5', '6', '7', '8']
    assert set(frf.ordinate_dim) == {'acceleration/voltage'}
    given = run.results['identification_frf'].arrays['values']
    assert np.allclose(frf.ordinate[1], given[0, 1] * G)
    coherence = project['identification_coherence']
    assert isinstance(coherence, MultipleCoherence)
    assert np.all((coherence.ordinate > 0.9) & (coherence.ordinate <= 1))
    for name in ('identification_response', 'identification_noise', 'identification_drive',
                 'identification_cross'):
        assert isinstance(project[name], Psd), name
    assert set(project['identification_cross'].ordinate_dim) == {
        'acceleration*voltage/frequency'}
    # the noise floor is far under the response the drives made
    response = np.abs(project['identification_response'].ordinate[0])
    noise = np.abs(project['identification_noise'].ordinate[0])
    assert np.all(noise < response)


def test_each_loop_and_the_full_matrices_are_there(converted, run):
    _folder, _done, project = converted
    loops = run.results['drive_spectra']
    count = len(loops.records)
    assert count == _json('end.json')['summary']['loops'] >= 2
    drives = project['drive_spectra']
    assert isinstance(drives, Psd) and drives.response_dof[0] == '5'
    assert drives.ordinate.shape[0] == 4 * count
    assert drives.block[:count] == [f'record {k}' for k in range(count)]
    # the record lines are the loops' summaries, in order
    assert [r['loop_index'] for r in loops.records] == sorted(
        r['loop_index'] for r in loops.records)
    assert all({'level_db', 'state', 'error_db', 'drive_rms', 'guards', 'law_misses'}
               <= set(r) for r in loops.records)
    # the loops that judged the response: the controls' spectra and their errors
    judged = run.results['control_spectra']
    assert 0 < len(judged.records) <= count
    control = project['control_spectra']
    assert isinstance(control, Psd) and control.response_dof[0] == '1'
    assert control.ordinate.shape[0] == 4 * len(judged.records)
    assert np.all(np.isfinite(judged.arrays['error_db']))
    assert np.all(np.abs(judged.arrays['error_db'][-1]) < 6)
    # the full matrices at the first and the last loop at full level
    snapshots = run.results['control_sdm']
    assert [r['why'] for r in snapshots.records] == ['first loop at full level',
                                                      'last loop at full level']
    assert isinstance(project['control_sdm'], Psd)
    assert isinstance(project['drive_sdm'], Psd)


def test_the_run_record_holds_the_setup_the_log_the_outcome_and_the_arrays(converted):
    _folder, _done, project = converted
    record = project['Run Record']
    assert isinstance(record, RunRecord)
    assert record.setup == _json('setup.json')
    assert record.config['operator'] == 'Operator'
    log = _lines('log.jsonl')
    assert record.log == log
    assert [e['seq'] for e in record.log] == list(range(len(log)))
    assert record.complete and record.outcome == 'completed'
    assert record.source == 'SIM' and record.test == 'mimo_random'
    assert record.law['id'] == 'pinv_closed'
    for name in ('control_spectra', 'identification', 'law_calls'):
        assert name in record.arrays, name
    assert record.arrays['control_spectra']['error_db'].shape[1:] == (4, 4)
    calls = record.arrays['law_calls']
    assert set(calls) == {'response', 'frf', 'multiple_coherence', 'last_drive', 'drive'}
    assert calls['response'].shape[1:] == (10, 4)
    assert record.missing == []


def test_save_and_reopen_loses_nothing(converted, tmp_path):
    _folder, _done, project = converted
    again = Project.open(project.save(tmp_path / 'again.hforge'))
    assert list(again) == list(project)
    for name in project:
        assert again[name] == project[name], name
    assert again.object_groups == project.object_groups


def test_everything_the_retired_converter_gave_is_in_the_conversion(converted, run):
    """The retired MIMO converter's `run/` group, item by item, and where each
    is now: the floor this conversion keeps."""
    _folder, _done, project = converted
    record = project['Run Record']
    results = record.results
    # run/ attributes and run/settings: test, folder, complete, source, setup hash
    assert record.test == 'mimo_random' and record.complete and record.source == 'SIM'
    assert record.setup['resolved']['setup_hash']
    assert record.manifest['start']['unix'] > 0
    # run/reference: the matrix, its four lines, the spacing, the band, the space,
    # the controls
    assert isinstance(project['Specification'], Specification)
    assert set(project['Specification'].limits) == {
        'warning_upper', 'warning_lower', 'abort_upper', 'abort_lower'}
    ref = results['reference']
    assert ref['abscissa']['step'] == 2.0 and ref['band'] == [80, 84]
    assert ref['channels']['responses'] == [1, 2, 3, 4]
    assert record.setup['resolved']['reference']['space'] == 'physical'
    # run/identification: frf, g_rd, g_cc, g_dd, noise, coherence, condition,
    # drive_vrms, and the setup hash, level, frames, steps and lag
    for name in ('identification_frf', 'identification_cross', 'identification_response',
                 'identification_drive', 'identification_noise',
                 'identification_coherence'):
        assert name in project, name
    assert set(record.arrays['identification']) == {'condition', 'drive_vrms'}
    facts = results['identification']
    assert {'setup_hash', 'level_db', 'frames', 'steps', 'lag_frames'} <= set(facts)
    # run/loops/diagonal: each loop's control PSDs, errors and drive PSDs
    assert 'control_spectra' in project and 'drive_spectra' in project
    assert 'error_db' in record.arrays['control_spectra']
    # run/loops/summary: each loop's index, level, state, misses and the rest
    assert all('state' in r for r in results['drive_spectra']['records'])
    # run/loops/sdm: the full response and drive matrices
    assert 'control_sdm' in project and 'drive_sdm' in project
    # run/law and run/law/calls: the law, its values, every call's inputs and output
    assert record.law['values'] is not None
    calls = results['law_calls']['records']
    assert calls[0]['call'] == 'seed' and calls[0]['law']['id'] == 'pinv_closed'
    assert all(c['outcome'] in ('ok', 'miss') for c in calls)
    # run/report: the end and its report
    assert record.end['report']['gates']['passed'] is True
    assert record.end['end']['outcome'] == 'completed'


def _saved_by_the_guide():
    with open(os.path.join(ROOT, 'docs', 'run-folder.md'), encoding='utf-8') as f:
        text = f.read()
    section = text.split('## What a MIMO Random Run Saves', 1)[1].split('\n## ', 1)[0]
    return re.findall(r'^\| `([^`]+)` \|', section, flags=re.MULTILINE)


def test_the_guide_says_exactly_what_a_mimo_random_run_saves(run):
    named = _saved_by_the_guide()
    assert named, 'the guide lists nothing'
    on_disk = ([f'streams/{name}' for name in run.streams]
               + [f'results/{name}' for name in run.results]
               + ['end.json'])
    assert sorted(named) == sorted(on_disk)
