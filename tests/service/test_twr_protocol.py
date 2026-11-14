"""The TWR messages against the schema: every TWR golden validates as a message
and as its own payload, the binary ones decode to their expected JSON, and the
schema refuses what the contract says is wrong (PROTOCOL.md, "Time Waveform
Replication" and "Control Laws")."""

from __future__ import annotations

import copy
import json

import jsonschema
import pytest

from hertzforge.service import frames

from .conftest import GOLDEN

#: text golden -> (path to its payload, the schema definition the payload follows)
TEXT = {
    'request_twr_configure': ('params', 'twrConfigureParams'),
    'response_twr_configure': ('result', 'twrConfigureResult'),
    'event_twr_state': ('data', 'twrState'),
    'request_twr_subscribe': ('params', 'twrSubscribeParams'),
    'response_twr_subscribe': ('result', 'twrSubscribeResult'),
    'response_twr_plays': ('result', 'twrPlaysResult'),
    'response_laws_list_twr': ('result', 'lawsListResult'),
    'response_laws_check_twr': ('result', 'lawsCheckResult'),
    'response_error_twr_config_invalid': ('error.detail', 'report'),
}

#: binary golden -> (path to its payload, the schema definition the payload follows)
BINARY = {
    'event_twr_live': ('data', 'twrLive'),
    'event_twr_data': ('data', 'twrData'),
    'response_twr_reference_import': ('result', 'twrReferenceImportResult'),
    'response_twr_reference_preview': ('result', 'twrReferencePreviewResult'),
}


def test_every_twr_golden_is_checked_here():
    # The Rust, TypeScript and Python suites read the same files in schema/golden/.
    names = {p.name.split('.')[0] for p in GOLDEN.iterdir()
             if ('_twr_' in p.name or p.name.split('.')[0].endswith('_twr'))
             and not p.name.endswith('.expected.json')}
    assert names == set(TEXT) | set(BINARY), sorted(names ^ (set(TEXT) | set(BINARY)))


def _text(name):
    return json.loads((GOLDEN / f'{name}.json').read_text(encoding='utf-8'))


def _wire(name):
    wire, _arrays = frames.to_wire(frames.decode((GOLDEN / f'{name}.bin').read_bytes()))
    return wire


def _at(message, path):
    for key in path.split('.'):
        message = message[key]
    return message


def _refused(validate, instance, name):
    with pytest.raises(jsonschema.ValidationError):
        validate(instance, name)


@pytest.mark.parametrize('name', sorted(TEXT))
def test_twr_text_golden_validates(name, validate):
    message = _text(name)
    validate(message)
    path, definition = TEXT[name]
    validate(_at(message, path), definition)


@pytest.mark.parametrize('name', sorted(BINARY))
def test_twr_binary_golden_validates_and_decodes_to_its_expected_json(name, validate):
    wire = _wire(name)
    validate(wire)
    path, definition = BINARY[name]
    validate(_at(wire, path), definition)
    decoded = frames.decode((GOLDEN / f'{name}.bin').read_bytes())
    expected = json.loads((GOLDEN / f'{name}.expected.json').read_text(encoding='utf-8'))

    def same(a, b, where):
        if isinstance(b, dict) and set(b) == {'dtype', 'shape', 'values'}:
            assert a.dtype.name == {'f64': 'float64', 'f32': 'float32', 'u32': 'uint32',
                                    'i32': 'int32', 'u8': 'uint8'}[b['dtype']], where
            assert list(a.shape) == b['shape'], where
            assert a.reshape(-1).tolist() == b['values'], where
        elif isinstance(b, dict):
            assert list(a) == list(b), where
            for k in b:
                same(a[k], b[k], f'{where}.{k}')
        elif isinstance(b, list):
            assert len(a) == len(b), where
            for i, (u, v) in enumerate(zip(a, b)):
                same(u, v, f'{where}[{i}]')
        else:
            assert a == b, where

    same(decoded, expected, name)


def test_a_configuration_carries_every_field_and_the_ceiling_is_required(validate):
    config = _text('request_twr_configure')['params']['config']
    assert set(config) == {'title', 'reference', 'prep', 'section', 'pads', 'acquisition',
                           'identification', 'inverse', 'correction', 'control', 'schedule',
                           'run', 'adaptation', 'limits', 'feasibility', 'operator', 'article',
                           'notes', 'specification', 'record', 'metadata'}
    # one way to record: the module's own capture is not a member any more
    bad = copy.deepcopy(config)
    bad['capture'] = {'auto_start': True, 'run_name': 'twr_run'}
    _refused(validate, bad, 'twrConfig')
    bad = copy.deepcopy(config)
    del bad['limits']['drive_vpk_ceiling']
    _refused(validate, bad, 'twrConfig')
    for key, value in (('reference', {'sha256': 'r1.csv'}),
                       ('run', {'schedule': 'loop'}),
                       ('run', {'schedule': 'forever'}),
                       ('prep', {'detrend': 'quadratic'}),
                       ('schedule', {'on_gate_fail': 'retry'}),
                       ('section', {'start': 0})):
        bad = copy.deepcopy(config)
        bad[key] = value
        _refused(validate, bad, 'twrConfig')
    for run in ({'schedule': 'once'}, {'schedule': 'loop', 'count': 3}):
        good = copy.deepcopy(config)
        good['run'] = run
        validate(good, 'twrConfig')
    # the minimal configuration: the reference and the drive ceiling
    validate({'reference': {'sha256': config['reference']['sha256']},
              'limits': {'drive_vpk_ceiling': 0.5}}, 'twrConfig')


def test_the_derived_values_say_the_grid_the_coverage_and_the_budget():
    derived = _text('response_twr_configure')['result']['derived']
    assert derived['grid']['record_len'] == 4096 and derived['grid']['df_hz'] == 3.125
    assert derived['coverage']['fraction'] > 0.99
    assert derived['call_budget_s'] == 1 and derived['plays_target'] == 375
    assert derived['reference']['sha256'] == _text('request_twr_configure')[
        'params']['config']['reference']['sha256']


def test_the_state_ends_exactly_when_finished_and_a_trip_names_its_facts(validate):
    state = _text('event_twr_state')['data']
    assert state['state'] == 'running' and state['end'] is None
    assert state['last_play']['lag_frames'] is None
    trip = {'kind': 'drive_vpk', 'play': None, 'at_level_db': 0, 'vpk': 0.2,
            'ceiling_vpk': 0.15, 'predicted': True}
    finished = {**state, 'state': 'finished', 'phase': 'aborted',
                'end': {'outcome': 'aborted', 'message': 'The drive passed its ceiling.',
                        'at_s': 4.2, 'level_db': 0, 'play': None, 'frames_done': None,
                        'trip': trip}}
    validate(finished, 'twrState')
    for facts in ({'kind': 'envelope', 'in_play': True, 'worst_frac': 0.6, 'abort_frac': 0.5,
                   'samples': 8, 'at_sample': 1200},
                  {'kind': 'drive_rms', 'vrms': 0.3, 'limit_vrms': 0.2},
                  {'kind': 'response_rms', 'rms': 0.3, 'limit': 0.2},
                  {'kind': 'alignment', 'lag_frames': 43.1, 'identification_lag_frames': 41,
                   'drift_frames': 2.1, 'limit_frames': 1},
                  {'kind': 'coverage', 'coverage': 0.8, 'min_band_energy': 0.9,
                   'error_floor_pct': 44.7},
                  {'kind': 'divergence', 'plays': 3, 'error_pct': 40},
                  {'kind': 'not_converged', 'iterations': 8, 'error_pct': 12, 'gate_pct': 10},
                  {'kind': 'law_fault', 'reason': 'first_drive', 'consecutive': 1}):
        validate({'play': 3, 'at_level_db': -6, **facts}, 'twrTrip')
    for bad in ({'kind': 'law_fault', 'reason': 'gain_rise', 'consecutive': 3},
                {'kind': 'envelope', 'worst_frac': 0.6},
                {'kind': 'tolerance', 'error_pct': 3}):
        _refused(validate, {'play': 3, 'at_level_db': -6, **bad}, 'twrTrip')
    stopped = {'outcome': 'operator_stop', 'message': 'Stopped by the operator.', 'at_s': 5.5,
               'level_db': 0, 'play': 4, 'frames_done': 2048, 'trip': None}
    validate(stopped, 'twrEnd')
    _refused(validate, {**stopped, 'outcome': 'held'}, 'twrEnd')


def test_the_live_frame_carries_the_play_in_flight_and_its_scope():
    data = _wire('event_twr_live')['data']
    assert [v['title'] for v in data['views']] == ['Scope']
    assert data['play']['frames_done'] <= data['play']['frames']
    assert data['last']['gated']


def test_a_subscription_takes_points_where_they_apply(validate):
    validate({'plots': [{'id': 'r', 'quantity': 'response', 'points': 512}]},
             'twrSubscribeParams')
    validate({'plots': [{'id': 's', 'quantity': 'scope'}]}, 'twrSubscribeParams')
    validate({'default': True}, 'twrSubscribeParams')
    for bad in ({'id': 'f', 'quantity': 'frf', 'points': 512},
                {'id': 'p', 'quantity': 'pulse'},
                {'id': 'r', 'quantity': 'response', 'channels': [1]}):
        _refused(validate, {'plots': [bad]}, 'twrSubscribeParams')
    result = _text('response_twr_subscribe')['result']
    assert result['quantities'] == ['target', 'response', 'error', 'drive', 'envelope', 'frf',
                                    'coherence', 'mask', 'spectra', 'convergence', 'law',
                                    'scope', 'readings', 'plays']


def test_the_data_frame_carries_each_plot_with_its_own_views():
    data = _wire('event_twr_data')['data']
    by_id = {p['id']: p for p in data['plots']}
    assert [v['title'] for v in by_id['frf']['views']] == ['Identification Gain',
                                                          'Identification Phase']
    assert [s['role'] for s in by_id['env']['views'][0]['series']] == [
        'measured', 'tolerance', 'tolerance', 'abort', 'abort']
    assert [v['view'] for v in by_id['law']['views']] == ['spectrum', 'reading']
    assert by_id['plays']['views'][0]['view'] == 'table'


def test_an_import_and_a_preview_are_computations(validate):
    result = _wire('response_twr_reference_import')['result']
    assert result['reference']['format'] == 'text' and result['reference']['path'] is None
    preview = _wire('response_twr_reference_preview')['result']
    assert [v['title'] for v in preview['views']] == ['Section', 'Spectrum', 'Velocity',
                                                      'Displacement']
    assert all(s['role'] != 'measured' for v in preview['views'] for s in v['series'])
    validate({'path': '/data/r1.csv', 'fs_hz': 12800, 'column': 1}, 'twrReferenceImportParams')
    validate({'text': '0\n0.25\n'}, 'twrReferenceImportParams')
    _refused(validate, {'text': 'x', 'path': '/data/r1.csv'}, 'twrReferenceImportParams')
    _refused(validate, {}, 'twrReferenceImportParams')
    validate({'reference': {'sha256': result['reference']['sha256']}},
             'twrReferencePreviewParams')


def test_the_run_verbs_take_their_own_params(validate):
    for ok, name in (({'delta_db': -1}, 'twrLevelParams'), ({'rung': 2}, 'twrLevelParams'),
                     ({'frozen': True}, 'twrFreezeParams'), ({'index': 3}, 'twrRecordParams'),
                     ({'index': 3, 'points': 0}, 'twrRecordParams'),
                     ({'override_ack': 'reviewed'}, 'twrArmParams')):
        validate(ok, name)
    for bad, name in (({}, 'twrLevelParams'), ({'delta_db': 2}, 'twrLevelParams'),
                      ({'delta_db': 1, 'rung': 2}, 'twrLevelParams'),
                      ({'frozen': 'yes'}, 'twrFreezeParams'), ({}, 'twrRecordParams')):
        _refused(validate, bad, name)


def test_a_twr_law_entry_and_its_check(validate):
    entry = _text('response_laws_list_twr')['result']['laws'][0]
    assert entry['world'] == 'twr' and entry['builtin'] and 'live' not in entry
    validate(entry, 'lawEntry')
    validate({'world': 'twr'}, 'lawsListParams')
    report = _text('response_laws_check_twr')['result']['report']
    assert report['tool_version'] == 'twr-check 1' and not report['recorded']
    assert [c['name'] for c in report['cases']] == [
        'flat', 'one mode', 'station plant', 'transport delay', 'deep queue',
        'identification error', 'measurement noise', 'anti-resonance']
    for case in report['cases']:
        validate(case, 'twrLawCheckCase')
        _refused(validate, case, 'sineLawCheckCase')
    assert report['cases'][4]['lag_frames'] == 41
    event = {'law_id': 'twr_correct', 'index': 4, 'of': 8, 'case': 'deep queue',
             'state': 'done', 'result': report['cases'][4]}
    validate(event, 'lawsCheckEvent')


def test_every_new_reason_is_listed(validate):
    validate(_text('response_error_twr_config_invalid'))
    for item in ('reference_unknown', 'reference_changed', 'units_mismatch', 'rate_mismatch',
                 'section_outside_reference', 'record_too_long', 'band_outside_identification',
                 'highpass_above_band', 'identification_too_short', 'reference_resampled'):
        validate({'reason': item, 'field': 'reference.sha256', 'message': 'x'}, 'reportItem')
    for reason in ('reference_unreadable', 'not_converged', 'coherence_hold', 'wrong_mode',
                   'drive_ceiling'):
        validate({'type': 'response', 'id': 1,
                  'error': {'code': 'refused', 'reason': reason, 'message': 'x'}})
