"""The SRS shock messages against the schema: every SRS golden validates as a
message and as its own payload, the binary ones decode to their expected JSON,
and the schema refuses what the contract says is wrong (PROTOCOL.md, "SRS Shock"
and "Control Laws")."""

from __future__ import annotations

import copy
import json

import jsonschema
import pytest

from hertzforge.service import frames

from .conftest import GOLDEN

#: text golden -> (path to its payload, the schema definition the payload follows)
TEXT = {
    'request_srs_configure': ('params', 'srsConfigureParams'),
    'response_srs_configure': ('result', 'srsConfigureResult'),
    'event_srs_state': ('data', 'srsState'),
    'request_srs_subscribe': ('params', 'srsSubscribeParams'),
    'response_srs_subscribe': ('result', 'srsSubscribeResult'),
    'request_srs_fire': ('params', None),
    'request_srs_level': ('params', 'srsLevelParams'),
    'response_srs_target_import': ('result', 'srsTargetImportResult'),
    'response_srs_table_import': ('result', 'srsTableImportResult'),
    'response_laws_list_shock_srs': ('result', 'lawsListResult'),
    'response_laws_check_shock_srs': ('result', 'lawsCheckResult'),
    'response_error_srs_config_invalid': ('error.detail', 'report'),
}

#: binary golden -> (path to its payload, the schema definition the payload follows)
BINARY = {
    'event_srs_live': ('data', 'srsLive'),
    'event_srs_pulse': ('data', 'srsPulseEvent'),
    'event_srs_data': ('data', 'srsData'),
    'response_srs_preview': ('result', 'srsPreviewResult'),
}


def test_every_srs_golden_is_checked_here():
    # The Rust, TypeScript and Python suites read the same files in schema/golden/.
    names = {p.name.split('.')[0] for p in GOLDEN.iterdir()
             if ('_srs_' in p.name or p.name.endswith('_srs.json'))
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
def test_srs_text_golden_validates(name, validate):
    message = _text(name)
    validate(message)
    path, definition = TEXT[name]
    if definition:
        validate(_at(message, path), definition)


@pytest.mark.parametrize('name', sorted(BINARY))
def test_srs_binary_golden_validates_and_decodes_to_its_expected_json(name, validate):
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


def test_the_live_frame_is_binary_with_no_arrays():
    raw = (GOLDEN / 'event_srs_live.bin').read_bytes()
    assert raw[:4] == b'SDS1'
    header = json.loads(raw[8:8 + int.from_bytes(raw[4:8], 'little')].decode('utf-8'))
    assert header['arrays'] == []
    assert header['data']['fire'] == {'done': 4096, 'of': 14336}


def test_a_configuration_carries_every_field_and_the_drive_limits_are_required(validate):
    config = _text('request_srs_configure')['params']['config']
    assert set(config) == {'title', 'target', 'bands', 'synthesis', 'envelope', 'acquisition',
                           'schedule', 'spacing', 'identification', 'control', 'limits',
                           'limit_channels', 'feasibility', 'operator', 'article', 'notes',
                           'specification', 'record', 'metadata'}
    for missing in ('drive_vpk_ceiling', 'drive_vpk_abort'):
        bad = copy.deepcopy(config)
        del bad['limits'][missing]
        _refused(validate, bad, 'srsConfig')
    for key, value in (('target', {'kind': 'ramp'}),
                       ('target', {'kind': 'breakpoints', 'points': [[100, 40]]}),
                       ('bands', {'f_lo_hz': 100}),
                       ('synthesis', {'engine': 'fourier'})):
        bad = copy.deepcopy(config)
        bad[key] = value
        _refused(validate, bad, 'srsConfig')
    seds = copy.deepcopy(config)
    seds['synthesis']['engine'] = 'seds'
    seds['synthesis']['components'] = {'engine': 'seds', 'rows': [
        {'freq_hz': 100, 'amplitude_g': 1, 'zeta': 0.05, 'delay_s': 0, 'polarity': 'negative'}]}
    validate(seds, 'srsConfig')
    seds['synthesis']['components']['rows'][0]['half_cycles'] = 5
    _refused(validate, seds, 'srsConfig')
    per_band = copy.deepcopy(config)
    per_band['limit_channels']['channels'][0]['bound'] = {'kind': 'per_band',
                                                          'bounds_g': [50, None, 40]}
    validate(per_band, 'srsConfig')
    # the minimal configuration: the target, the bands, the schedule and the drive limits
    validate({'target': {'kind': 'flat', 'accel_g': 50},
              'bands': {'f_lo_hz': 100, 'f_hi_hz': 2000},
              'schedule': {'mode': 'manual', 'levels_db': [0], 'pulses': [1]},
              'limits': {'drive_vpk_ceiling': 0.5, 'drive_vpk_abort': 0.8}}, 'srsConfig')


def test_the_state_ends_exactly_when_finished_and_a_trip_names_its_facts(validate):
    state = _text('event_srs_state')['data']
    assert state['state'] == 'running' and state['end'] is None
    finished = {**state, 'state': 'finished', 'phase': 'aborted', 'next_pulse_s': None}
    _refused(validate, finished, 'srsState')
    trip = {'kind': 'drive_vpk', 'pulse': 0, 'at_level_db': 0, 'vpk': 0.9, 'abort_vpk': 0.8,
            'predicted': True}
    finished['end'] = {'outcome': 'aborted', 'message': 'The predicted drive passes the abort.',
                       'at_s': 4.2, 'level_db': 0, 'pulse': 0, 'trip': trip}
    validate(finished, 'srsState')
    for facts in ({'kind': 'envelope', 'abort_bands': 2, 'worst_band_hz': 630,
                   'worst_err_db': -6.4},
                  {'kind': 'kinematic', 'axis': 'accel', 'value': 31, 'limit': 25,
                   'predicted': False},
                  {'kind': 'accel', 'predicted_g': 61, 'limit_g': 25},
                  {'kind': 'monitor', 'channel': 2, 'value': 84, 'limit': 40},
                  {'kind': 'not_converged', 'limit': 'per_level', 'pulses': 5},
                  {'kind': 'law_fault', 'reason': 'gain_rise', 'consecutive': 3},
                  {'kind': 'law_fault', 'reason': 'not_positive', 'consecutive': 3}):
        validate({'pulse': 4, 'at_level_db': -12, **facts}, 'srsTrip')
    for bad in ({'kind': 'tolerance', 'envelope_pass': False, 'velocity_change_pass': True,
                 'error_pct': 18.2, 'worst_deviation_frac': 0.19},
                {'kind': 'law_fault', 'reason': 'peak_rise', 'consecutive': 3},
                {'kind': 'drive_vpk', 'vpk': 0.4}):
        _refused(validate, {'pulse': 4, 'at_level_db': -12, **bad}, 'srsTrip')
    done = {'outcome': 'completed', 'message': 'The schedule finished.', 'at_s': 30,
            'level_db': 0, 'pulse': 6, 'trip': None}
    validate(done, 'srsEnd')
    _refused(validate, {**done, 'outcome': 'aborted'}, 'srsEnd')
    no_notch = {**state, 'notch': None}
    validate(no_notch, 'srsState')
    without = dict(state)
    del without['notch']
    _refused(validate, without, 'srsState')


def test_a_row_is_judged_or_unmeasured_and_its_law_call_says_how_it_was_used(validate):
    row = _text('event_srs_state')['data']['last_pulse']
    validate(row, 'srsRow')
    unmeasured = {**row, 'peak_g': None, 'velocity_pk_ips': None, 'displacement_pk_pk_in': None,
                  'worst_err_db': None, 'worst_band_hz': None, 'alarm_bands': None,
                  'abort_bands': None, 'passed': None, 'limited_components': None,
                  'worst_limit_db': None, 'law': None}
    validate(unmeasured, 'srsRow')
    missed = {**row, 'law': {**row['law'], 'used': False, 'miss': 'gain_rise'}}
    validate(missed, 'srsRow')
    _refused(validate, {**row, 'law': {**row['law'], 'miss': 'bin_rise'}}, 'srsRow')
    _refused(validate, {**row, 'origin': 'train'}, 'srsRow')


def test_a_level_is_one_step_or_a_rung(validate):
    validate({'delta_db': 1}, 'srsLevelParams')
    validate({'rung': 2}, 'srsLevelParams')
    for bad in ({'delta_db': 3}, {'rung': -1}, {}):
        _refused(validate, bad, 'srsLevelParams')


def test_a_subscription_takes_points_on_the_time_histories_only(validate):
    validate({'plots': [{'id': 'p', 'quantity': 'pulse', 'points': 512}]}, 'srsSubscribeParams')
    validate({'default': True}, 'srsSubscribeParams')
    for bad in ({'id': 'p', 'quantity': 'srs', 'points': 512},
                {'id': 'p', 'quantity': 'limit_frf', 'points': 512},
                {'id': 'p', 'quantity': 'spectrum'}):
        _refused(validate, {'plots': [bad]}, 'srsSubscribeParams')
    result = _text('response_srs_subscribe')['result']
    assert result['quantities'] == ['srs', 'pulse', 'velocity', 'displacement', 'identification',
                                    'components', 'limit_channels', 'limit_frf', 'law',
                                    'readings', 'pulses']


def test_the_data_frame_carries_each_plot_with_its_own_views():
    data = _wire('event_srs_data')['data']
    by_id = {p['id']: p for p in data['plots']}
    assert [v['title'] for v in by_id['srs']['views']] == ['SRS']
    assert [s['role'] for s in by_id['srs']['views'][0]['series']] == [
        'reference', 'tolerance', 'tolerance', 'abort', 'abort', 'measured']
    assert [v['title'] for v in by_id['pulse']['views']] == ['Pulse', 'Drive']
    assert [v['title'] for v in by_id['id']['views']] == ['Identification Gain', 'Coherence']
    assert [v['title'] for v in by_id['comp']['views']] == ['Components', 'Gains']
    assert [v['title'] for v in by_id['lim']['views']] == ['Limit Fixture']
    assert [v['title'] for v in by_id['frf']['views']] == ['Gain Fixture']
    assert [v['view'] for v in by_id['table']['views']] == ['table']


def test_the_pulse_event_carries_the_row_and_the_default_views():
    data = _wire('event_srs_pulse')['data']
    assert data['pulse']['index'] == 2
    assert [v['title'] for v in data['views']] == ['SRS', 'Pulse', 'Drive']
    assert data['views'][0]['x']['scale'] == 'log' and data['views'][0]['y']['unit'] == 'g'
    assert data['views'][2]['y']['unit'] == 'V'


def test_the_preview_is_a_computation_with_five_views():
    result = _wire('response_srs_preview')['result']
    assert [v['title'] for v in result['views']] == [
        'Target', 'Synthesis', 'Pulse', 'Velocity', 'Displacement']
    assert [s['name'] for s in result['views'][1]['series']] == ['Target', 'Synthesis']
    assert all(s['role'] != 'measured' for v in result['views'] for s in v['series'])


def test_the_imports_return_what_a_configuration_takes_or_name_the_row(validate):
    target = _text('response_srs_target_import')['result']
    validate(target['target'], 'srsTarget')
    table = _text('response_srs_table_import')['result']
    validate(table['components'], 'srsComponents')
    refused = {'target': None, 'errors': [{'reason': 'target_file', 'row': 2,
                                           'message': 'Line 3 is not ascending.'}],
               'warnings': []}
    validate(refused, 'srsTargetImportResult')
    _refused(validate, {**refused, 'errors': []}, 'srsTargetImportResult')
    _refused(validate, {**target, 'errors': refused['errors']}, 'srsTargetImportResult')
    validate({'path': '/data/table.tsv', 'engine': 'seds'}, 'srsTableImportParams')
    _refused(validate, {'path': '/data/table.tsv'}, 'srsTableImportParams')


def test_the_srs_law_entry_and_its_check_say_which_world_they_are_for(validate):
    entry = _text('response_laws_list_shock_srs')['result']['laws'][0]
    assert entry['world'] == 'shock_srs' and entry['builtin'] and entry['check']['hardware']
    assert entry['shapes'] is None and entry['needs'] is None
    validate({'world': 'shock_srs'}, 'lawsListParams')
    report = _text('response_laws_check_shock_srs')['result']['report']
    assert report['tool_version'] == 'shock-srs-check 1' and not report['recorded']
    assert len(report['cases']) == 6
    for case in report['cases']:
        validate(case, 'srsLawCheckCase')
        _refused(validate, case, 'shockLawCheckCase')
    event = {'law_id': 'srs_ratio', 'index': 5, 'of': 6, 'case': 'limit channel',
             'state': 'done', 'result': report['cases'][5]}
    validate(event, 'lawsCheckEvent')


def test_every_new_item_is_a_listed_reason(validate):
    error = _text('response_error_srs_config_invalid')
    validate(error)
    for item in ('bands_empty', 'target_not_covering', 'target_not_ascending',
                 'per_band_length_mismatch', 'components_engine_mismatch', 'component_invalid',
                 'limit_no_frf', 'target_file', 'table_file', 'synthesis_not_converged',
                 'alarm_wider_than_abort', 'limit_unknown_channel', 'record_too_long'):
        validate({'reason': item, 'field': 'bands', 'message': 'x'}, 'reportItem')
