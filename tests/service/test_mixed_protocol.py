"""The Mixed Mode messages against the schema: every Mixed Mode golden validates as
a message and as its own payload, the binary ones decode to their expected JSON,
and the schema refuses what the contract says is wrong (PROTOCOL.md, "Mixed
Mode")."""

from __future__ import annotations

import copy
import json

import jsonschema
import pytest

from hertzforge.service import frames

from .conftest import GOLDEN

#: text golden -> (path to its payload, the schema definition the payload follows)
TEXT = {
    'request_mixed_configure': ('params', 'mixedConfigureParams'),
    'response_mixed_configure': ('result', 'mixedConfigureResult'),
    'event_mixed_state': ('data', 'mixedState'),
    'request_mixed_subscribe': ('params', 'mixedSubscribeParams'),
    'response_mixed_subscribe': ('result', 'mixedSubscribeResult'),
    'request_mixed_level': ('params', 'mixedLevelParams'),
    'request_mixed_part': ('params', 'mixedPartParams'),
    'request_mixed_adjust': ('params', 'mixedAdjustParams'),
    'response_error_mixed_config_invalid': ('error.detail', 'report'),
    'response_error_mixed_part_refused': ('error', 'error'),
}

#: binary golden -> (path to its payload, the schema definition the payload follows)
BINARY = {
    'event_mixed_live': ('data', 'mixedLive'),
    'event_mixed_data': ('data', 'mixedData'),
}


def test_every_mixed_golden_is_checked_here():
    # The Rust, TypeScript and Python suites read the same files in schema/golden/.
    names = {p.name.split('.')[0] for p in GOLDEN.iterdir()
             if '_mixed_' in p.name and not p.name.endswith('.expected.json')}
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
def test_mixed_text_golden_validates(name, validate):
    message = _text(name)
    validate(message)
    path, definition = TEXT[name]
    if definition != 'error':
        validate(_at(message, path), definition)


@pytest.mark.parametrize('name', sorted(BINARY))
def test_mixed_binary_golden_validates_and_decodes_to_its_expected_json(name, validate):
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


def test_a_part_names_its_kind_and_a_tone_needs_its_schedule(validate):
    config = _text('request_mixed_configure')['params']['config']
    validate(config, 'mixedConfig')
    tone = copy.deepcopy(config['parts'][1])
    del tone['schedule']
    _refused(validate, tone, 'mixedPart')
    bad_id = copy.deepcopy(config['parts'][1])
    bad_id['id'] = 'has space'
    _refused(validate, bad_id, 'mixedPart')
    no_limits = copy.deepcopy(config)
    del no_limits['limits']['drive_vpk_abort']
    _refused(validate, no_limits, 'mixedConfig')


def test_a_mixed_configuration_carries_every_field_and_takes_no_capture(validate):
    config = _text('request_mixed_configure')['params']['config']
    assert set(config) == {'title', 'parts', 'exclusion_hz', 'acquisition', 'schedule',
                           'control', 'limits', 'feasibility', 'operator', 'article', 'notes',
                           'specification', 'record', 'metadata'}
    validate(config, 'mixedConfig')
    state = _text('event_mixed_state')['data']
    validate(state, 'mixedState')
    assert set(state['record']) == {'folder', 'streams', 'problem'}


def test_the_state_carries_one_row_per_part_in_the_parts_order():
    state = _text('event_mixed_state')['data']
    config = _text('request_mixed_configure')['params']['config']
    assert [p['id'] for p in state['parts']] == [p['id'] for p in config['parts']]
    floor = state['parts'][0]
    assert floor['kind'] == 'floor' and floor['lines'] is not None and floor['freq_hz'] is None
    # the lines the floor's rms abort is judged on are clean lines
    assert 0 <= floor['lines']['rms_judged'] <= floor['lines']['clean']
    for row in state['parts']:
        # a tone's drive in volts peak, the floor's in volts rms; a narrowband
        # has none of its own (the core reports no share of the random part for it)
        want = {'tone': (True, False), 'floor': (False, True), 'narrowband': (False, False)}
        has = (row['drive_vpk'] is not None, row['drive_vrms'] is not None)
        assert has == want[row['kind']], row['id']
        # one rate at most, in its schedule's law
        assert row['rate_oct_per_min'] is None or row['rate_hz_per_s'] is None, row['id']
    follower = next(r for r in state['parts'] if r['id'] == 't3')
    assert follower['sweeps'] is None and follower['rate_oct_per_min'] is not None
    # a follower names its leader and ratio, and moves in its leader's direction
    assert follower['follows'] == {'of': 't2', 'ratio': 2}
    assert follower['direction'] == 'up'
    assert all(r['follows'] is None for r in state['parts'] if r['id'] != 't3')
    # a tone states how its alarm and abort are judged; the floor and a band do not
    judged = ('judge_s', 'judged_error_db', 'judged_alarm_db', 'judged_abort_db')
    for row in state['parts']:
        if row['kind'] == 'tone':
            assert isinstance(row['tolerance_widened'], bool), row['id']
        else:
            assert row['tolerance_widened'] is None and all(row[k] is None for k in judged)
    assert follower['tolerance_widened'] is True and follower['judge_s'] == 8
    lo, hi = follower['judged_abort_db']
    assert follower['judged_alarm_db'][0] < lo and follower['judged_alarm_db'][1] < hi


def test_the_derived_values_sum_the_parts():
    derived = _text('response_mixed_configure')['result']['derived']
    ref = derived['reference']
    parts = {p['kind']: 0 for p in derived['parts']}
    for p in derived['parts']:
        parts[p['kind']] += p['ms_g2']
    assert ref['floor_ms_g2'] == pytest.approx(parts['floor'], rel=1e-3)
    assert ref['tones_ms_g2'] == pytest.approx(parts['tone'], rel=1e-3)
    assert ref['bands_ms_g2'] == pytest.approx(parts['narrowband'], rel=1e-3)
    assert ref['total_ms_g2'] == pytest.approx(
        ref['floor_ms_g2'] + ref['tones_ms_g2'] + ref['bands_ms_g2'], rel=1e-3)
    assert set(derived['law']) == {'floor', 'tones'}
    assert derived['law']['floor']['live'] == []
    # one judging prediction per tone, in the parts' order
    tones = [p['id'] for p in derived['parts'] if p['kind'] == 'tone']
    assert [j['part'] for j in derived['tone_judging']] == tones
    for j in derived['tone_judging']:
        assert j['judge_s'] > 0 and j['sigma_db'] >= 0
        assert j['abort_db'][0] >= j['alarm_db'][0] and j['abort_db'][1] >= j['alarm_db'][1]


def test_the_live_frame_has_its_four_views_in_order():
    data = _wire('event_mixed_live')['data']
    assert [v['title'] for v in data['views']] == ['Control', 'Residual', 'Drive', 'Run']
    assert all(r['kind'] == 'cut' for r in data['views'][0]['regions'])


def test_a_subscription_takes_channels_parts_and_points_where_they_apply(validate):
    validate({'plots': [{'id': 'a', 'quantity': 'amplitude', 'parts': ['t1']}]},
             'mixedSubscribeParams')
    validate({'plots': [{'id': 't', 'quantity': 'time', 'channels': 'all', 'points': 128}]},
             'mixedSubscribeParams')
    _refused(validate, {'plots': [{'id': 'a', 'quantity': 'control', 'parts': 'all'}]},
             'mixedSubscribeParams')
    _refused(validate, {'plots': [{'id': 'a', 'quantity': 'level', 'channels': [1]}]},
             'mixedSubscribeParams')
    _refused(validate, {'plots': [{'id': 'a', 'quantity': 'phase'}]}, 'mixedSubscribeParams')


def test_the_data_frame_names_what_each_plot_resolved_to():
    data = _wire('event_mixed_data')['data']
    by_id = {p['id']: p for p in data['plots']}
    assert by_id['amp']['parts'] == ['t2'] and by_id['amp']['channels'] is None
    assert by_id['frq']['parts'] == ['b1', 'b2']
    assert by_id['lvl']['parts'] == ['b1', 'b2']
    assert by_id['rsp']['channels'] == [1] and by_id['rsp']['parts'] is None


def test_every_new_reason_is_listed(validate):
    for item in ('no_floor', 'too_many_floors', 'unknown_part_kind', 'duplicate_part_id',
                 'part_out_of_band', 'parts_too_close', 'too_many_tones', 'too_many_bands',
                 'follow_unknown_part', 'follow_self', 'follow_loop', 'schedule_unsupported'):
        validate({'reason': item, 'field': 'parts[1]', 'message': 'x'}, 'reportItem')
    validate({'type': 'response', 'id': 1,
              'error': {'code': 'refused', 'reason': 'part_refused', 'message': 'x'}})
