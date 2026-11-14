"""The MIMO Random and control-law messages against the schema: every golden
validates as a message and as its own payload, and the schema refuses what
the contract says is wrong (PROTOCOL.md, "MIMO Random" and "Control Laws")."""

from __future__ import annotations

import copy
import json

import jsonschema
import pytest

from .conftest import GOLDEN

#: golden -> (path to its payload, the schema definition the payload follows)
PAYLOADS = {
    'request_mimo_random_setup': ('params', 'mimoSetupParams'),
    'response_mimo_random_setup': ('result', 'mimoSetupResult'),
    'response_error_setup_invalid': ('error.detail', 'mimoValidation'),
    'event_mimo_random_status': ('data', 'mimoStatus'),
    'request_mimo_random_override': ('params', 'mimoOverrideParams'),
    'response_mimo_random_report': ('result', 'mimoReportResult'),
    'request_laws_list': ('params', 'lawsListParams'),
    'response_laws_list': ('result', 'lawsListResult'),
    'request_laws_import': ('params', 'lawsImportParams'),
    'response_laws_import': ('result', 'lawsImportResult'),
    'request_laws_check': ('params', 'lawsCheckParams'),
    'response_laws_check': ('result', 'lawsCheckResult'),
    'event_laws_check': ('data', 'lawsCheckEvent'),
    'request_mimo_random_subscribe_coh_error': ('params', 'mimoSubscribeParams'),
    'request_mimo_random_identification_load': ('params', 'mimoIdentificationParams'),
    'response_mimo_random_identification_load': ('result', 'mimoIdentificationResult'),
    'request_mimo_random_reference_build_breakpoints': ('params', 'mimoReferenceBuildParams'),
    'request_mimo_random_reference_build_rigid_body': ('params', 'mimoRigidParams'),
    'response_mimo_random_reference_build_rigid_body': ('result', 'mimoRigidResult'),
    'request_mimo_random_reference_import': ('params', 'mimoReferenceImportParams'),
    'response_mimo_random_reference_import': ('result', 'mimoReferenceResult'),
    'request_mimo_random_override_prediction': ('params', 'mimoOverrideParams'),
    'request_mimo_random_setup_virtual': ('params', 'mimoSetupParams'),
    'request_mimo_random_subscribe_pred_sdm': ('params', 'mimoSubscribeParams'),
    'request_mimo_random_exclude': ('params', 'mimoExcludeParams'),
}


def test_every_mimo_text_golden_is_checked_here():
    # The Rust, TypeScript and Python suites read the same files in schema/golden/.
    names = {p.stem for p in GOLDEN.glob('*.json')
             if ('mimo_random' in p.stem or '_laws_' in p.stem)
             and 'shock' not in p.stem and 'sine' not in p.stem and 'twr' not in p.stem
             and not p.name.endswith('.expected.json')}
    names.add('response_error_setup_invalid')
    assert names == set(PAYLOADS), sorted(names ^ set(PAYLOADS))


def _text(name):
    return json.loads((GOLDEN / f'{name}.json').read_text(encoding='utf-8'))


def _at(message, path):
    for key in path.split('.'):
        message = message[key]
    return message


def _refused(validate, instance, name):
    with pytest.raises(jsonschema.ValidationError):
        validate(instance, name)


@pytest.mark.parametrize('name', sorted(PAYLOADS))
def test_mimo_golden_validates(name, validate):
    message = _text(name)
    validate(message)
    path, definition = PAYLOADS[name]
    validate(_at(message, path), definition)


def test_the_packed_reference_holds_the_upper_triangle_of_each_line():
    ref = _text('request_mimo_random_setup')['params']['setup']['reference']
    n, lines = ref['n'], len(ref['freqs_hz'])
    assert len(ref['re']) == len(ref['im']) == lines * n * (n + 1) // 2
    # the diagonal of every line is real; G01 carries the cross spectrum's phase
    assert all(ref['im'][k * 3] == 0 and ref['im'][k * 3 + 2] == 0 for k in range(lines))
    g01 = complex(ref['re'][1], ref['im'][1])
    coherence = abs(g01) ** 2 / (ref['re'][0] * ref['re'][2])
    assert coherence == pytest.approx(0.25)


def test_a_setup_carries_its_channels_reference_and_schedule(validate):
    setup = _text('request_mimo_random_setup')['params']['setup']
    assert set(setup) == {'title', 'channels', 'space', 'reference', 'law', 'estimation',
                          'identification', 'control', 'schedule', 'backend', 'operator',
                          'article', 'notes', 'specification', 'record'}
    _refused(validate, {**setup, 'record': {'streams': 'yes'}}, 'mimoSetup')
    _refused(validate, {**setup, 'specification': {'document': 'x', 'page': 3}}, 'mimoSetup')
    for field in ('channels', 'reference', 'schedule'):
        _refused(validate, {k: v for k, v in setup.items() if k != field}, 'mimoSetup')
    drives = copy.deepcopy(setup)
    del drives['channels']['drives'][0]['ceiling_vrms']
    _refused(validate, drives, 'mimoSetup')
    file_ref = {**setup, 'reference': {'kind': 'file', 'path': '/data/ref.hforge',
                                       'sha256': 'ab' * 32}}
    validate(file_ref, 'mimoSetup')
    _refused(validate, {**file_ref, 'reference': {**file_ref['reference'], 'sha256': 'AB' * 32}},
             'mimoSetup')
    virtual = {**setup, 'space': {'kind': 'virtual', 'dofs': ['X', 'Y'],
                                  'response_transform': {'rows': 2, 'cols': 2,
                                                         'values': [1, 0, 0, 1]}}}
    validate(virtual, 'mimoSetup')
    _refused(validate, {**setup, 'space': {'kind': 'virtual'}}, 'mimoSetup')


def test_a_validation_is_ok_exactly_when_it_has_no_errors(validate):
    good = _text('response_mimo_random_setup')['result']['validation']
    bad = _text('response_error_setup_invalid')['error']['detail']
    assert good['ok'] and not bad['ok']
    _refused(validate, {**good, 'ok': False}, 'mimoValidation')
    _refused(validate, {**bad, 'ok': True}, 'mimoValidation')


def test_the_status_names_its_record(validate):
    status = _text('event_mimo_random_status')['data']
    assert set(status['record']) == {'folder', 'streams', 'problem'}
    validate({**status, 'record': None}, 'mimoStatus')
    validate({**status, 'record': {**status['record'], 'problem': 'the disk is full'}},
             'mimoStatus')
    _refused(validate, {**status, 'record': {**status['record'], 'streams': True}},
             'mimoStatus')
    _refused(validate, {k: v for k, v in status.items() if k != 'record'}, 'mimoStatus')


def test_the_status_ends_exactly_when_finished(validate):
    status = _text('event_mimo_random_status')['data']
    _refused(validate, {**status, 'state': 'finished'}, 'mimoStatus')
    _refused(validate, {**status, 'phase': 'coherence_gate'}, 'mimoStatus')
    report = _text('response_mimo_random_report')['result']['report']
    validate({**status, 'state': 'finished', 'phase': 'stopped', 'end': report['end']},
             'mimoStatus')


def test_an_override_names_the_operators_words(validate):
    _refused(validate, {'ack': ''}, 'mimoOverrideParams')
    _refused(validate, {'ack': '   '}, 'mimoOverrideParams')
    _refused(validate, {}, 'mimoOverrideParams')


def test_law_entries_say_which_world_they_are_for(validate):
    entry = _text('response_laws_list')['result']['laws'][0]
    assert entry['world'] == 'mimo' and entry['builtin'] and entry['check']['hardware']
    _refused(validate, {**entry, 'shapes': None}, 'lawEntry')
    single = {**entry, 'world': 'single', 'shapes': None, 'needs': None}
    validate(single, 'lawEntry')
    imported = _text('response_laws_import')['result']['law']
    assert imported['sha256'] == imported['id'] and not imported['check']['hardware']
    _refused(validate, {**imported['params'][0], 'kind': 'slider'}, 'lawParam')


def test_every_new_refusal_is_a_listed_reason(validate):
    error = _text('response_error_setup_invalid')
    validate(error)
    for reason in ('gate_failed', 'prediction_exceeded', 'law_import_failed',
                   'catalog_unavailable', 'identification_mismatch', 'law_unknown',
                   'law_params_invalid', 'law_check_unsupported', 'law_check_running',
                   'law_check_not_recorded', 'law_not_checked'):
        e = copy.deepcopy(error)
        e['error']['reason'] = reason
        validate(e)
    e = copy.deepcopy(error)
    e['error']['reason'] = 'setup_rejected'
    _refused(validate, e, None)


def _mimo_data():
    from hertzforge.service import frames
    wire, _arrays = frames.to_wire(frames.decode((GOLDEN / 'event_mimo_random_data.bin').read_bytes()))
    return wire


def test_the_mimo_data_frame_validates_and_carries_a_packed_block(validate):
    from hertzforge.service import frames
    raw = (GOLDEN / 'event_mimo_random_data.bin').read_bytes()
    decoded = frames.decode(raw)
    wire = _mimo_data()
    validate(wire)
    validate(wire['data'], 'mimoData')
    plots = wire['data']['plots']
    assert [p['quantity'] for p in plots] == ['sdm', 'coh', 'error_db']
    sdm = decoded['data']['plots'][0]['views'][0]
    n = len(sdm['channels'])
    assert sdm['measured']['re'].shape == (3, n * (n + 1) // 2)
    assert sdm['measured']['re'].dtype.name == 'float64'
    heat = decoded['data']['plots'][2]['views'][0]['values']
    assert heat.shape == (2, 4) and heat.dtype.name == 'float32'
    # the frame re-encodes to the same bytes
    expected = json.loads((GOLDEN / 'event_mimo_random_data.expected.json').read_text())
    assert expected['data']['plots'][1]['views'][0]['series'][0]['y']['values'] == \
        list(decoded['data']['plots'][1]['views'][0]['series'][0]['y'])


def test_a_mimo_plot_takes_the_selector_its_quantity_names(validate):
    validate({'plots': [{'id': 'a', 'quantity': 'coh', 'pair': [0, 1]}]}, 'mimoSubscribeParams')
    validate({'plots': [{'id': 'a', 'quantity': 'psd', 'diagonal': True, 'points': 400}]},
             'mimoSubscribeParams')
    validate({'plots': [{'id': 'a', 'quantity': 'error_db', 'rows': 'all'}]}, 'mimoSubscribeParams')
    validate({'plots': [{'id': 'a', 'quantity': 'sdm', 'block': [0, 1, 2]}]}, 'mimoSubscribeParams')
    validate({'default': True}, 'mimoSubscribeParams')
    for bad in ({'id': 'a', 'quantity': 'coh', 'index': 0},
                {'id': 'a', 'quantity': 'coh'},
                {'id': 'a', 'quantity': 'psd'},
                {'id': 'a', 'quantity': 'psd', 'pair': [0, 1]},
                {'id': 'a', 'quantity': 'sdm', 'block': list(range(9))},
                {'id': 'a', 'quantity': 'trace', 'index': 0},
                {'id': 'a', 'quantity': 'psd', 'index': 0, 'points': 10},
                {'id': 'a', 'quantity': 'control'}):
        _refused(validate, {'plots': [bad]}, 'mimoSubscribeParams')


def test_a_decimated_series_is_an_envelope(validate):
    series = {'name': 'Control', 'role': 'measured', 'x': {'$array': 'a0'}, 'y': {'$array': 'a1'},
              'y_min': {'$array': 'a2'}, 'shape': 'steps', 'edges': {'$array': 'a3'}}
    validate(series, 'series')
    _refused(validate, {**series, 'y_min': [1, 2]}, 'series')


def test_a_rigid_body_answer_shares_the_spare_readings(validate):
    """Seven sensors, six degrees of freedom: the check shares sum to one spare reading."""
    result = _text('response_mimo_random_reference_build_rigid_body')['result']
    assert result['ok'] is True
    assert result['transform']['rows'] == 6
    assert result['transform']['cols'] == len(result['sensors']) == 7
    assert sum(s['check_share'] for s in result['sensors']) == pytest.approx(1.0)


def test_the_setup_extensions_are_checked(validate):
    setup = copy.deepcopy(_text('request_mimo_random_setup')['params']['setup'])
    setup['channels']['drives'][0].update({'peak_v': 6, 'shaker': None, 'moving_mass_lb': 12.5})
    setup['channels']['limits'] = [{'channel': 5, 'ceiling': 0.2}]
    setup['control'].update(
        {'frf_blend': 0.5, 'cap_up_db': 3.5, 'law_budget': 0.25, 'law_misses': 3})
    validate(setup, 'mimoSetup')
    bad = copy.deepcopy(setup)
    bad['control']['cap_up_db'] = 7
    _refused(validate, bad, 'mimoSetup')


def test_a_law_check_reports_every_case_and_clears_the_law(validate):
    result = _text('response_laws_check')['result']
    report, law = result['report'], result['law']
    assert report['passed'] and report['recorded'] and len(report['cases']) == 6
    assert law['check']['status'] == 'passed' and law['check']['hardware']
    assert [c['name'] for c in law['check']['cases']] == [c['name'] for c in report['cases']]
    _refused(validate, {**report['cases'][0], 'requirement': 'converged'}, 'lawCheckCase')
    event = _text('event_laws_check')['data']
    assert event['result'] == report['cases'][event['index']]
    _refused(validate, {**event, 'state': 'running'}, 'lawsCheckEvent')
    validate({**event, 'state': 'running', 'result': None}, 'lawsCheckEvent')
    _refused(validate, {'id': 'pinv_open', 'level': 3}, 'lawsCheckParams')
