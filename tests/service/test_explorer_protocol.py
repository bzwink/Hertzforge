"""The Explorer messages against the schema: every Explorer golden validates as a message
and as its own payload, the binary ones decode to their expected JSON, and the schema
refuses what the contract says is wrong (PROTOCOL.md, "Explorer")."""

from __future__ import annotations

import copy
import json

import jsonschema
import pytest

from hertzforge.service import frames

from .conftest import GOLDEN

#: text golden -> (path to its payload, the schema definition the payload follows)
TEXT = {
    'request_explorer_configure': ('params', 'explorerConfigureParams'),
    'response_explorer_configure': ('result', 'explorerConfigureResult'),
    'request_explorer_start': ('params', 'explorerStartParams'),
    'response_explorer_start': ('result', 'explorerState'),
    'request_explorer_steer': ('params', 'explorerSteerParams'),
    'response_explorer_steer': ('result', 'explorerSteerResult'),
    'request_explorer_sweep': ('params', 'explorerSweepParams'),
    'response_explorer_sweep': ('result', 'explorerSweepResult'),
    'event_explorer_state': ('data', 'explorerState'),
    'request_explorer_subscribe': ('params', 'explorerSubscribeParams'),
    'response_explorer_subscribe': ('result', 'explorerSubscribeResult'),
    'response_explorer_survey': ('result', 'explorerSurveyResult'),
    'response_error_explorer_config_invalid': ('error.detail', 'report'),
}

#: binary golden -> (path to its payload, the schema definition the payload follows)
BINARY = {
    'event_explorer_live': ('data', 'explorerLive'),
    'event_explorer_data': ('data', 'explorerData'),
}


def test_every_explorer_golden_is_checked_here():
    # The Rust, TypeScript and Python suites read the same files in schema/golden/.
    names = {p.name.split('.')[0] for p in GOLDEN.iterdir()
             if '_explorer_' in p.name and not p.name.endswith('.expected.json')}
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
def test_explorer_text_golden_validates(name, validate):
    message = _text(name)
    validate(message)
    path, definition = TEXT[name]
    validate(_at(message, path), definition)


@pytest.mark.parametrize('name', sorted(BINARY))
def test_explorer_binary_golden_validates_and_decodes_to_its_expected_json(name, validate):
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


def test_a_configuration_needs_only_the_ceiling_and_takes_no_other_key(validate):
    config = _text('request_explorer_configure')['params']['config']
    assert set(config) == {'title', 'acquisition', 'band_hz', 'start_hz', 'max_drive_vpk',
                           'slew', 'display', 'capture', 'metadata', 'operator', 'article',
                           'notes', 'specification', 'record'}
    validate({'max_drive_vpk': 0.5}, 'explorerConfig')
    # the run's record: the operator's words, and whether it keeps its time histories
    validate({'max_drive_vpk': 0.5, 'record': {'streams': False}, 'specification': None},
             'explorerConfig')
    _refused(validate, {'max_drive_vpk': 0.5, 'record': {'streams': 'yes'}}, 'explorerConfig')
    _refused(validate, {'max_drive_vpk': 0.5, 'record': {'samples': True}}, 'explorerConfig')
    bad = copy.deepcopy(config)
    del bad['max_drive_vpk']
    _refused(validate, bad, 'explorerConfig')
    # A level in any unit but volts peak at the drive has no key to go in.
    for key, value in (('level_g', 1), ('arm_vpk', 0.1), ('sweep', {'rate': 1})):
        bad = copy.deepcopy(config)
        bad[key] = value
        _refused(validate, bad, 'explorerConfig')
    for key, inner in (('slew', {'drive_g_per_s': 1}), ('acquisition', {'tick_ms': 10}),
                       ('display', {'window': 'hann'}), ('capture', {'auto_start': True})):
        bad = copy.deepcopy(config)
        bad[key] = {**bad[key], **inner}
        _refused(validate, bad, 'explorerConfig')
    bad = copy.deepcopy(config)
    bad['band_hz'] = [20]
    _refused(validate, bad, 'explorerConfig')


def test_the_derived_values_are_the_ni_stations_at_12800_hz():
    derived = _text('response_explorer_configure')['result']['derived']
    grid = derived['grid']
    assert (grid['fs_hz'], grid['block_frames'], grid['lookahead_frames']) == (12800, 256, 512)
    assert not grid['actual']
    # two lookaheads and the input path's 39 frames
    assert derived['pipeline_frames'] == 2 * 512 + 39
    assert derived['pipeline_s'] == round(1063 / 12800, 5)
    assert derived['rise_s'] == derived['stop_s'] == round(40 / 6, 3)
    assert derived['stop_wait_s'] == 30 and derived['sweep_max_oct_per_min'] == 60
    assert derived['display'] == {'df_hz': 3.125, 'window_s': 0.32}
    assert derived['survey']['tolerance_hz'] == 3.125 / 2
    state = _text('response_explorer_configure')['result']['state']
    assert state['state'] == 'configured' and state['phase'] == 'idle'
    assert state['t_s'] is None and state['health'] is None and state['end'] is None


def test_the_steering_verbs_take_their_own_params(validate):
    for ok in ({'hz': 250}, {'step_hz': 10}, {'step_oct': -1}, {'vpk': 0.2}, {'step_db': -6},
               {'hz': 1000, 'vpk': 0.2}, {'step_oct': 0.5, 'step_db': 3}):
        validate(ok, 'explorerSteerParams')
    for bad in ({}, {'hz': 250, 'step_hz': 10}, {'step_hz': 1, 'step_oct': 1},
                {'vpk': 0.2, 'step_db': 3}, {'level_g': 1}, {'hz': '250'}):
        _refused(validate, bad, 'explorerSteerParams')
    validate({'direction': 'down', 'rate_oct_per_min': 4}, 'explorerSweepParams')
    for bad in ({'direction': 'up'}, {'direction': 'sideways', 'rate_oct_per_min': 4},
                {'direction': 'up', 'rate_oct_per_min': 0}):
        _refused(validate, bad, 'explorerSweepParams')
    steered = _text('response_explorer_steer')['result']
    assert steered['clamped'] == {'hz': False, 'vpk': True} and steered['vpk'] == 0.5
    # the change lands two lookaheads ahead of what the station had acquired
    assert steered['frame'] - steered['input_frame'] == 1024
    swept = _text('response_explorer_sweep')['result']
    assert swept['clamped'] and swept['rate_oct_per_min'] == 60
    validate({'hz': 251.3, 'frame': 1024, 'input_frame': 0}, 'explorerHoldResult')


def test_the_state_ends_exactly_when_finished_and_a_trip_names_its_facts(validate):
    state = _text('event_explorer_state')['data']
    assert state['state'] == 'running' and state['end'] is None and state['stop'] is None
    assert state['t_s'] == state['health']['engine_ticks'] * 256 / 12800
    started = _text('response_explorer_start')['result']
    assert (started['state'], started['phase'], started['drive_vpk']) == ('running', 'quiet', 0)
    trip = {'kind': 'drive_vpk', 'at_hz': 250, 'vpk': 0.5012, 'ceiling_vpk': 0.5}
    finished = {**state, 'state': 'finished', 'phase': 'aborted',
                'end': {'outcome': 'aborted', 'message': 'A drive sample passed the ceiling.',
                        'at_s': 41.4, 'at_hz': 250, 'vpk': 0.2, 'ramp_s': None,
                        'trip': trip}}
    validate(finished, 'explorerState')
    for facts in ({'kind': 'watchdog', 'channel': 1, 'axis': 'accel', 'value': 0.152,
                   'limit': 0.15},
                  {'kind': 'limit', 'channel': 1, 'axis': 'velocity', 'value': 1.2, 'limit': 1}):
        validate({'at_hz': 250, **facts}, 'explorerTrip')
    for bad in ({'kind': 'limit', 'channel': 1, 'axis': 'accel', 'value': 1, 'limit': 1},
                {'kind': 'watchdog', 'channel': 1},
                {'kind': 'envelope', 'worst_frac': 0.6}):
        _refused(validate, {'at_hz': 250, **bad}, 'explorerTrip')
    stopped = {'outcome': 'operator_stop', 'message': 'Stopped by the operator.',
               'at_s': 48.05, 'at_hz': 250, 'vpk': 0.2, 'ramp_s': 6.667, 'trip': None}
    validate(stopped, 'explorerEnd')
    _refused(validate, {**stopped, 'outcome': 'completed'}, 'explorerEnd')
    for cause in ('operator', 'safe_stop', 'shutdown', 'limit'):
        validate({**state, 'state': 'stopping', 'phase': 'ramping_down', 'stop': cause},
                 'explorerState')
    _refused(validate, {**state, 'stop': 'timeout'}, 'explorerState')
    _refused(validate, {**state, 'state': 'armed'}, 'explorerState')
    # the run's record: named from start, with the latest problem writing it
    assert started['record'] == state['record'] and state['record']['streams'] == 'on'
    assert _text('response_explorer_configure')['result']['state']['record'] is None
    validate({**state, 'record': {**state['record'], 'problem': 'a write failed'}},
             'explorerState')
    _refused(validate, {**state, 'record': {'folder': 'x', 'streams': 'on'}}, 'explorerState')
    stalled = {**state, 'stall': {'stale_s': 3.2, 'period_s': 0.04,
                                  'message': 'The service loop has missed its periods.'}}
    validate(stalled, 'explorerState')


def test_the_live_frame_carries_the_tone_the_channels_and_two_views():
    data = _wire('event_explorer_live')['data']
    assert [v['title'] for v in data['views']] == ['Survey', 'Run']
    survey = data['views'][0]
    assert survey['x']['scale'] == 'log' and survey['y'] == {'label': 'Level', 'unit': 'g',
                                                             'scale': 'log'}
    assert [c['index'] for c in data['channels']] == [1, 3]
    assert data['control']['index'] == 1 and not data['control']['settling']
    assert data['peak'] == {'hz': 250, 'level': 0.1999}
    labels = [r['label'] for r in data['views'][1]['rows']]
    assert labels == ['Frequency', 'Target', 'Measured', 'Drive', 'Target drive', 'Ceiling',
                      'Sweep', 'Control rms', 'Velocity', 'Displacement', 'Peak', 'Pipeline']


def test_a_subscription_takes_channels_and_points_where_they_apply(validate):
    for ok in ({'plots': [{'id': 's', 'quantity': 'survey', 'points': 256}]},
               {'plots': [{'id': 's', 'quantity': 'survey', 'points': None}]},
               {'plots': [{'id': 'p', 'quantity': 'spectrum', 'channels': [1, 3]}]},
               {'plots': [{'id': 'p', 'quantity': 'spectrum', 'channels': 'control'}]},
               {'plots': [{'id': 't', 'quantity': 'time', 'points': 2048}]},
               {'rate_hz': 0.5, 'plots': []}, {'default': True}):
        validate(ok, 'explorerSubscribeParams')
    for bad in ({'id': 'p', 'quantity': 'spectrum', 'points': 512},
                {'id': 's', 'quantity': 'survey', 'channels': [1]},
                {'id': 't', 'quantity': 'time', 'channels': 'all'},
                {'id': 'r', 'quantity': 'readings', 'points': 64},
                {'id': 's', 'quantity': 'survey', 'points': 32},
                {'id': 'x', 'quantity': 'phase'},
                {'id': 'p', 'quantity': 'spectrum', 'channels': 'limited'}):
        _refused(validate, {'plots': [bad]}, 'explorerSubscribeParams')
    _refused(validate, {'rate_hz': 20, 'plots': []}, 'explorerSubscribeParams')
    result = _text('response_explorer_subscribe')['result']
    assert result['quantities'] == ['survey', 'spectrum', 'time', 'readings']
    by_id = {p['id']: p for p in result['plots']}
    assert by_id['srv']['points'] is None and by_id['spec']['channels'] == 'all'
    assert by_id['t']['points'] == 1024 and set(by_id['run']) == {'id', 'quantity'}


def test_the_data_frame_carries_each_plot_with_its_own_views():
    data = _wire('event_explorer_data')['data']
    by_id = {p['id']: p for p in data['plots']}
    assert by_id['spec']['channels'] == [1, 3]
    assert [v['title'] for v in by_id['spec']['views']] == ['Channel 1: Control',
                                                           'Channel 3: Fixture']
    assert by_id['t']['first_frame'] == 529408 and by_id['srv']['first_frame'] is None
    assert [v['title'] for v in by_id['t']['views']] == ['Time']
    assert by_id['run']['views'][0]['view'] == 'reading'
    decoded = frames.decode((GOLDEN / 'event_explorer_data.bin').read_bytes())
    t = next(p for p in decoded['data']['plots'] if p['id'] == 't')
    x = t['views'][0]['series'][0]['x']
    assert x[0] == 529408 / 12800 and x[1] == 529409 / 12800, 'station time of each sample'


def test_the_survey_is_a_sorted_point_list_with_its_peak(validate):
    result = _text('response_explorer_survey')['result']
    hz = [p[0] for p in result['points']]
    assert hz == sorted(hz)
    top = max(result['points'], key=lambda p: p[1])
    assert result['peak'] == {'hz': top[0], 'level': top[1]}
    validate({'revision': 58, 'channel': 1, 'unit': 'g', 'points': None, 'peak': None},
             'explorerSurveyResult')
    validate({'revision': 59}, 'explorerSurveyClearResult')
    validate({'since': 57}, 'explorerSurveyParams')
    _refused(validate, {'since': -1}, 'explorerSurveyParams')


def test_every_new_reason_is_listed(validate):
    validate(_text('response_error_explorer_config_invalid'))
    for item in ('ceiling_over_range', 'extra_drive_channels', 'no_input_channel',
                 'safe_stop_budget', 'band_above_grid', 'no_drive_channel', 'out_of_range'):
        validate({'reason': item, 'field': 'max_drive_vpk', 'message': 'x'}, 'reportItem')
    validate({'type': 'response', 'id': 1,
              'error': {'code': 'refused', 'reason': 'level_zero', 'message': 'x'}})
