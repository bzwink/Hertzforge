"""What a selection in the tree shows (`project.show`), by the table of readings: one
object by its kind, and the pairs that
mean something together."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from hertzforge.core.data import Frf, Psd, Specification, TimeHistory, channel_quantities
from hertzforge.core.shapes import ShapeSet
from hertzforge.service import frames, projects, show
from hertzforge.service.protocol import ProtocolError

from .conftest import wire_views
from .test_server import _session

PLATE = Path(__file__).resolve().parents[2] / 'testdata' / 'plate'


@pytest.fixture(scope='module')
def plate():
    state, _ = projects.import_file(None, {'path': str(PLATE / 'modal.nc4')})
    projects.import_file(state, {'path': str(PLATE / 'modal_spectra.nc4')})
    projects.import_file(state, {'path': str(PLATE / 'geometry.unv'),
                                 'units': {'length_unit': 'mm'}})
    return state


def named(state, kind):
    return next(name for name, obj in state.project.items() if projects.kind_of(obj) == kind)


def shown(state, *objects, records=None, part=None, validate=None, prefs=None):
    out = projects.show(state, {'objects': list(objects), 'records': records or {}, 'part': part,
                                **({'prefs': prefs} if prefs else {})})
    if validate:
        # the views carry arrays until they go on the wire; check them as the shell gets them
        wire_views(out['views'], validate)
        validate({**out, 'views': []}, 'shown')
    return out


def test_one_object_shows_by_its_kind(plate, validate):
    geometry, frf, table = named(plate, 'geometry'), named(plate, 'frf'), named(plate, 'channel_table')
    assert [v['view'] for v in shown(plate, geometry, validate=validate)['views']] == ['shape3d']
    whole = shown(plate, frf, validate=validate)
    assert [v['view'] for v in whole['views']] == ['waterfall'] and whole['note'] == ''
    # records picked keep the stage while 3D is chosen, and are curves when Flat is
    # (Brandon, 2026-11-14)
    picked = shown(plate, frf, records={frf: [0, 1, 2, 5]}, validate=validate)
    assert [v['view'] for v in picked['views']] == ['waterfall']
    assert len(picked['views'][0]['records']) == 4, 'the records picked, on the stage'
    flat = shown(plate, frf, records={frf: [0, 1, 2, 5]}, prefs={'flat': True}, validate=validate)
    assert [v['view'] for v in flat['views']] == ['spectrum']
    assert len(flat['views'][0]['series']) == 4, 'the records picked, as curves'
    assert shown(plate, table, validate=validate)['views'][0]['view'] == 'table'


def test_a_time_history_on_a_geometry_plays_back(plate, validate):
    geometry, history = named(plate, 'geometry'), named(plate, 'time_history')
    out = shown(plate, geometry, history, validate=validate)
    stage, plot = out['views']
    assert stage['view'] == 'shape3d' and stage['modes'] == [] and stage['link'] == 'time'
    assert stage['motion']['values'].shape[0] == len(plot['series'])
    assert stage['motion']['quantity'] == 'Acceleration' and stage['motion']['peak'] > 0
    assert 'another quantity' in out['note'], 'the force channels are not played'
    # the same pair the other way round, and with records picked
    assert shown(plate, history, geometry)['views'][0]['view'] == 'shape3d'
    one = shown(plate, geometry, history, records={history: [0]})
    assert len(one['views'][1]['series']) == 1


def test_an_frf_on_a_geometry_is_its_operating_shape_at_the_loudest_line(plate, validate):
    geometry, frf = named(plate, 'geometry'), named(plate, 'frf')
    out = shown(plate, frf, geometry, validate=validate)
    stage, curves = out['views']
    assert stage['view'] == 'shape3d' and len(stage['modes']) == 1
    mode = stage['modes'][0]
    assert mode['label'].startswith('ODS at') and 'shape_imag' in mode
    assert mode['frequency_hz'] > 0 and np.abs(mode['shape']).max() > 0
    assert curves['view'] == 'spectrum'


def test_a_shape_set_shows_on_its_geometry_or_as_its_table_and_mac(plate, validate):
    project = plate.project
    geometry = project[named(plate, 'geometry')]
    dofs = [f'{int(n)}Z+' for n in geometry.node_id[:6]]
    shapes = ShapeSet(frequency=[10.0, 20.0], damping=[0.01, 0.01],
                      shape_matrix=np.ones((2, 6)), coordinate=dofs)
    project.add('Modes', shapes)
    try:
        out = shown(plate, 'Modes', validate=validate)
        assert [v['view'] for v in out['views']] == ['shape3d'], 'on the active geometry'
        both = shown(plate, 'Modes', named(plate, 'geometry'), records={'Modes': [1]})
        assert [v['view'] for v in both['views']] == ['shape3d']
        project.add('Modes 2', shapes)
        mac = shown(plate, 'Modes', 'Modes 2', validate=validate)
        assert mac['views'][0]['view'] == 'matrix'
    finally:
        project.remove('Modes 2')
        project.remove('Modes')


def test_a_shape_set_with_no_geometry_shows_its_table_or_its_own_mac(validate):
    from hertzforge.project import Project

    project = Project()
    project.add('Modes', ShapeSet(frequency=[10.0, 20.0], damping=[0.01, 0.01],
                                  shape_matrix=np.eye(2), coordinate=['1Z+', '2Z+']))
    state = projects.OpenProject(project=project)
    out = shown(state, 'Modes', validate=validate)
    assert [v['view'] for v in out['views']] == ['table']
    mac = shown(state, 'Modes', prefs={'shape_view': 'mac'}, validate=validate)
    assert [v['view'] for v in mac['views']] == ['matrix']
    alone = shown(state, 'Modes', prefs={'shape_view': 'animate'}, validate=validate)
    assert [v['view'] for v in alone['views']] == ['table'], 'nothing to animate on'


def test_a_measurement_beside_its_requirement_is_the_comparison(validate):
    from hertzforge.project import Project

    project = Project()
    f = np.linspace(20, 2000, 100)
    psd = Psd(abscissa=f, ordinate=np.full((2, 100), 0.01), response_dof=['1Z+', '2Z+'],
              ordinate_unit=['g**2/Hz'] * 2)
    spec = Specification(abscissa=f, ordinate=np.full((2, 100), 0.01),
                         response_dof=['1Z+', '2Z+'], ordinate_unit=['g**2/Hz'] * 2)
    project.add('Measured', psd)
    project.add('Spec', spec)
    project.add('Other', Psd(abscissa=f, ordinate=np.full((2, 100), 0.02),
                             response_dof=['1Z+', '2Z+'], ordinate_unit=['g**2/Hz'] * 2))
    state = projects.OpenProject(project=project)
    out = shown(state, 'Spec', 'Measured', validate=validate)
    assert out['views'][0]['view'] == 'spectrum' and 'Spec' in out['title']
    two = shown(state, 'Measured', 'Other', validate=validate)
    assert [v['view'] for v in two['views']] == ['spectrum'], 'two PSDs overlaid'
    alone = shown(state, 'Spec', validate=validate)
    assert alone['views'][0]['view'] == 'waterfall'


def test_a_pair_with_no_joint_reading_shows_each_on_its_own(plate, validate):
    frf, table = named(plate, 'frf'), named(plate, 'channel_table')
    out = shown(plate, frf, table, validate=validate)
    assert [v['view'] for v in out['views']] == ['waterfall', 'table']
    assert 'each object on its own' in out['note']
    three = shown(plate, frf, table, named(plate, 'geometry'), validate=validate)
    assert len(three['views']) == 3


def test_a_part_of_a_geometry_is_drawn_alone_or_is_noted(plate, validate):
    """No table under a part's plot: its table is its Edit (Brandon, 2026-11-13)."""
    geometry = named(plate, 'geometry')
    out = shown(plate, geometry, part={'kind': 'family', 'id': 'quads'}, validate=validate)
    assert [v['view'] for v in out['views']] == ['shape3d']
    out = shown(plate, geometry, part={'kind': 'mates', 'id': 0}, validate=validate)
    assert out['views'][0]['view'] == 'shape3d' and 'mates 0' in out['note']


def test_what_cannot_be_shown_is_refused_or_said(plate):
    with pytest.raises(ProtocolError, match='no object named'):
        shown(plate, 'nope')
    with pytest.raises(ProtocolError, match='needs "objects"'):
        projects.show(plate, {'objects': []})
    with pytest.raises(ProtocolError, match='"records"'):
        projects.show(plate, {'objects': [named(plate, 'frf')], 'records': {'x': [-1]}})
    with pytest.raises(ProtocolError, match='none is open'):
        projects.show(None, {'objects': ['x']})


def test_record_runs_read_as_the_shell_numbers_them():
    assert show._runs([0, 1, 2, 6]) == '1-3, 7'
    assert show._runs([4]) == '5'
    assert show._runs([2, 0, 1]) == '1-3'
    assert show._runs([0, 2, 3, 4, 7, 8]) == '1, 3-5, 8-9'


def test_time_data_on_a_geometry_that_shares_no_node_says_so(validate):

    state, (geometry,) = projects.import_file(None, {'path': str(PLATE / 'geometry.unv'),
                                                    'units': {'length_unit': 'mm'}})
    project = state.project
    project.add('Stray', TimeHistory(abscissa=np.linspace(0, 1, 16), ordinate=np.zeros((1, 16)),
                                     response_dof=['999999Z+'], ordinate_unit=['m/s**2']))
    out = shown(state, geometry, 'Stray', validate=validate)
    assert out['views'] == [] or out['views'][0]['view'] == 'shape3d'
    assert 'lands on' in out['note'] or 'each object on its own' in out['note']
    frf = Frf(abscissa=np.linspace(1, 100, 8), ordinate=np.ones((1, 8), dtype=complex) * 1j,
              response_dof=['999999Z+'], reference_dof=['1Z+'], ordinate_unit=['m/s**2/N'])
    project.add('Stray FRF', frf)
    out = shown(state, geometry, 'Stray FRF')
    assert 'lands on' in out['note'] or 'each object on its own' in out['note']


def test_the_server_sends_the_selections_views_with_their_arrays():
    import asyncio

    async def check(client, socket):
        await socket.recv()        # hello
        await socket.send(frames.encode_text({'type': 'request', 'id': 7, 'method': 'project.import',
                                              'params': {'path': str(PLATE / 'geometry.unv'),
                                                         'units': {'length_unit': 'mm'}}}))
        while True:
            imported = frames.decode(await socket.recv())
            if imported.get('id') == 7:
                break
        (geometry,) = [o['name'] for o in imported['result']['project']['objects']]
        answer = await client.call('project.show', {'objects': [geometry]})
        (view,) = answer['result']['views']
        assert view['view'] == 'shape3d' and len(view['nodes']) > 0, 'the mesh came through'
        assert answer['result']['title'] == geometry

    asyncio.run(_session(check))


def test_two_geometries_are_one_scene_and_a_report_says_where_it_opens(plate, validate):
    project = plate.project
    first = named(plate, 'geometry')
    projects.import_file(plate, {'path': str(PLATE / 'geometry.unv'),
                                 'units': {'length_unit': 'mm'}})
    second = next(n for n, o in project.items() if projects.kind_of(o) == 'geometry' and n != first)
    try:
        out = shown(plate, first, second, validate=validate)
        (scene,) = out['views']
        assert scene['view'] == 'shape3d' and scene['title'] == f'{second} over {first}'
        assert len(scene['nodes']) == len(project[first].node_id) + len(project[second].node_id)
        assert out['note'] == ''
    finally:
        project.remove(second)
    from hertzforge.core.report import Report

    project.add('Report', Report(title='Plate survey'))
    try:
        out = shown(plate, 'Report', validate=validate)
        assert out['views'][0]['view'] == 'reading'
        assert any(r['value'] == 'the Report workspace' for r in out['views'][0]['rows'])
    finally:
        project.remove('Report')


def test_the_bar_reads_data_flat_in_decades_and_by_component(plate, validate):
    frf, history = named(plate, 'frf'), named(plate, 'time_history')
    stage = shown(plate, frf, validate=validate)
    assert stage['views'][0]['view'] == 'waterfall'
    def data_offers(out):
        return {k: out['offers'][k] for k in ('flat', 'complex', 'frequency', 'projection')}

    assert data_offers(stage) == {'flat': True, 'complex': True, 'frequency': True, 'projection': False}
    flat = shown(plate, frf, prefs={'flat': True}, validate=validate)
    (plot,) = flat['views']
    assert plot['view'] == 'spectrum' and len(plot['series']) == plate.project[frf].num_records
    assert plot['x']['scale'] != 'log'
    decades = shown(plate, frf, prefs={'flat': True, 'log_frequency': True})
    assert decades['views'][0]['x']['scale'] == 'log'
    phase = shown(plate, frf, prefs={'flat': True, 'component': 'phase'})
    assert 'phase' in phase['views'][0]['y']['label'].lower() or phase['views'][0]['y']['unit'] in ('°', 'deg', 'rad')
    time = shown(plate, history, validate=validate)
    assert data_offers(time) == {'flat': True, 'complex': False, 'frequency': False,
                                 'projection': False}
    assert data_offers(shown(plate, named(plate, 'geometry'))) == {'flat': False, 'complex': False,
                                                                  'frequency': False, 'projection': False}
    with pytest.raises(ProtocolError, match='"prefs"'):
        projects.show(plate, {'objects': [frf], 'prefs': {'stage': True}})
    with pytest.raises(ProtocolError, match='component'):
        projects.show(plate, {'objects': [frf], 'prefs': {'component': 'abs'}})


def test_a_geometrys_views_carry_the_drawn_nodes_ids(plate, validate):
    geometry, frf, history = named(plate, 'geometry'), named(plate, 'frf'), named(plate, 'time_history')
    project = plate.project
    for objects in ((geometry,), (geometry, frf), (geometry, history)):
        (stage, *_rest) = shown(plate, *objects, validate=validate)['views']
        ids = stage['node_ids']
        assert ids.dtype == np.uint32 and len(ids) == len(stage['nodes'])
        assert set(ids.tolist()) <= {int(n) for n in project[geometry].node_id}


def test_several_data_objects_of_one_type_share_a_stage(plate, validate):
    project = plate.project
    history = named(plate, 'time_history')
    (twin,) = project.duplicate(history)
    try:
        out = shown(plate, history, twin, validate=validate)
        (stage,) = out['views']
        assert stage['view'] == 'waterfall' and '2 objects on one stage' in out['note']
        alone = shown(plate, history)['views'][0]
        assert len(stage['records']) == 2 * len(alone['records'])
        assert stage['title'] == f'{history} + {twin}'
        flat = shown(plate, history, twin, prefs={'flat': True}, validate=validate)
        assert flat['views'][0]['view'] == 'spectrum'
        assert len(flat['views'][0]['series']) == len(stage['records'])
    finally:
        project.remove(twin)
    others = [n for n, o in project.items() if projects.kind_of(o) == 'time_history' and n != history]
    apart = shown(plate, history, others[0], validate=validate)
    assert [v['view'] for v in apart['views']] == ['waterfall', 'waterfall'], 'abscissas differ: each alone'
    mixed = shown(plate, history, named(plate, 'frf'), validate=validate)
    assert len(mixed['views']) == 2, 'not one type: each alone'


def test_a_coherence_opens_on_the_map(plate, validate):
    out = shown(plate, named(plate, 'multiple_coherence'), validate=validate)
    assert out['views'][0]['view'] == 'waterfall' and out['views'][0]['facing'] == 'top'
    assert 'facing' not in shown(plate, named(plate, 'frf'))['views'][0]


def test_a_cpsd_on_a_geometry_is_its_principal_shape_or_one_reference_column(plate, validate):
    project = plate.project
    geometry, history = named(plate, 'geometry'), named(plate, 'time_history')
    cpsd = project.compute_cpsds(history)
    try:
        data = project[cpsd]
        assert data.reference_dof is not None
        out = shown(plate, geometry, cpsd, validate=validate)
        stage = out['views'][0]
        assert stage['modes'][0]['label'].startswith('Principal') and stage['cursor'] == 'frequency'
        assert out['views'][1]['link'] == 'frequency'
        # one reference channel's column: the DOF and its quantity, as the grid's columns are
        reference = data.reference_dof[0]
        column = [i for i in range(data.num_records) if data.reference_dof[i] == reference
                  and channel_quantities(data.known_dim(i))[1] == 'force']
        assert len(column) > 2
        picked = shown(plate, geometry, cpsd, records={cpsd: column}, validate=validate)
        assert picked['views'][0]['modes'][0]['label'].startswith('ODS')
        assert f'relative to reference {reference}' in picked['note']
    finally:
        project.remove(cpsd)


def test_the_stage_reads_at_the_line_the_cursor_asks_for(plate, validate):
    geometry, frf = named(plate, 'geometry'), named(plate, 'frf')
    loudest = shown(plate, geometry, frf, validate=validate)['views'][0]['modes'][0]['frequency_hz']
    asked = shown(plate, geometry, frf, prefs={'line': 500.3}, validate=validate)
    at = asked['views'][0]['modes'][0]['frequency_hz']
    assert at == pytest.approx(500.0, abs=1.0) and at != loudest
    assert asked['views'][0]['modes'][0]['label'] == f'ODS at {at:.4g} Hz'
    with pytest.raises(ProtocolError, match='"line"'):
        projects.show(plate, {'objects': [frf, geometry], 'prefs': {'line': 'peak'}})


def _two_sets(project):
    geometry = project[next(n for n, o in project.items() if projects.kind_of(o) == 'geometry')]
    dofs = [f'{int(n)}Z+' for n in geometry.node_id[:6]]
    rng = np.random.default_rng(3)
    project.add('Set A', ShapeSet(frequency=[10.0, 20.0, 30.0], damping=[0.01] * 3,
                                  shape_matrix=rng.normal(size=(3, 6)), coordinate=dofs))
    project.add('Set B', ShapeSet(frequency=[11.0, 21.0], damping=[0.01] * 2,
                                  shape_matrix=rng.normal(size=(2, 6)), coordinate=dofs))


def test_two_shape_sets_show_their_mac_and_the_pair_it_selects(plate, validate):
    project = plate.project
    geometry = named(plate, 'geometry')
    _two_sets(project)
    try:
        out = shown(plate, 'Set A', 'Set B', validate=validate)
        matrix, stage = out['views']
        assert matrix['view'] == 'matrix' and matrix['selects'] == 'shape3d'
        assert matrix['values'].shape == (3, 2) and 'matched' not in matrix
        assert stage['view'] == 'shape3d' and len(stage['overlays']) == 2
        assert out['offers']['projection'] is False, 'one geometry: nothing to project'
        three = shown(plate, geometry, 'Set A', 'Set B', validate=validate)
        assert [v['view'] for v in three['views']] == ['matrix', 'shape3d']
        assert three['views'][0]['rows']['label'] == 'Set A'
        # the matches kept so far ride on the matrix
        from hertzforge.service.ops_tables import OPERATIONS
        add = next(op for op in OPERATIONS if op.id == 'project.add_matches')
        (table,) = add.run(project, {'first': 'Set A', 'second': 'Set B', 'pairs': '1,1; 3,2'})
        assert len(table['rows']) == 2
        (table,) = add.run(project, {'first': 'Set A', 'second': 'Set B', 'pairs': '2,2'})
        assert len(table['rows']) == 3, 'added to the same object'
        marked = shown(plate, 'Set A', 'Set B', validate=validate)
        assert marked['views'][0]['matched'] == [[0, 0], [1, 1], [2, 1]], 'by row, as the object keeps them'
        assert '3 pairs matched in' in marked['note']
        with pytest.raises(ProtocolError, match='no square 4,1'):
            add.run(project, {'first': 'Set A', 'second': 'Set B', 'pairs': '4,1'})
        with pytest.raises(ProtocolError, match='not a pair'):
            add.run(project, {'first': 'Set A', 'second': 'Set B', 'pairs': 'x'})
        with pytest.raises(ProtocolError, match='"projection"'):
            projects.show(plate, {'objects': ['Set A', 'Set B'], 'prefs': {'projection': 'yes'}})
    finally:
        for name in [n for n, o in project.items() if projects.kind_of(o) == 'matched_modes']:
            project.remove(name)
        project.remove('Set B')
        project.remove('Set A')


def test_a_lone_shape_set_has_four_views_one_at_a_time(plate, validate):
    project = plate.project
    geometry = project[named(plate, 'geometry')]
    dofs = [f'{int(n)}Z+' for n in geometry.node_id[:6]]
    project.add('Lone', ShapeSet(frequency=[10.0, 20.0], damping=[0.01, 0.01],
                                 shape_matrix=np.arange(12.0).reshape(2, 6), coordinate=dofs))
    try:
        out = shown(plate, 'Lone', validate=validate)
        assert [v['view'] for v in out['views']] == ['shape3d'], 'the animation, by default'
        assert out['offers']['shapes'] is True and out['offers']['values_dofs'] == ['all']
        listed = shown(plate, 'Lone', prefs={'shape_view': 'table'}, validate=validate)
        assert [v['view'] for v in listed['views']] == ['table']
        mac = shown(plate, 'Lone', prefs={'shape_view': 'mac'}, validate=validate)
        assert [v['view'] for v in mac['views']] == ['shape3d', 'matrix'], 'with what its squares animate'
        assert mac['views'][1]['selects'] == 'shape3d' and 'overlays' in mac['views'][0]
        values = shown(plate, 'Lone', prefs={'shape_view': 'values'}, validate=validate)
        assert [v['view'] for v in values['views']] == ['table']
        table = values['views'][0]
        assert table['title'] == 'Lone, shapes' and table['columns'] == ['DOF', 'M1', 'M2']
        assert len(table['rows']) == 6
        response = shown(plate, 'Lone', prefs={'shape_view': 'values', 'values_form': 'response'})
        assert response['views'][-1]['title'] == 'Lone, response transformation'
        assert len(response['views'][-1]['rows']) == 2, 'a row per mode'
        chosen = shown(plate, 'Lone', prefs={'shape_view': 'values', 'values_dofs': 'control'})
        assert 'shape values:' in chosen['note'] and 'channel table' in chosen['note']
        with pytest.raises(ProtocolError, match='"values_form"'):
            projects.show(plate, {'objects': ['Lone'], 'prefs': {'values_form': 'inverse'}})
        with pytest.raises(ProtocolError, match='"shape_view"'):
            projects.show(plate, {'objects': ['Lone'], 'prefs': {'shape_view': 'waterfall'}})
    finally:
        project.remove('Lone')


def test_an_frf_beside_a_shape_set_shows_its_resynthesis(plate, validate):
    project = plate.project
    frf = project[named(plate, 'frf')]
    dofs = list(dict.fromkeys(frf.response_dof))[:5]
    rng = np.random.default_rng(5)
    project.add('Fit', ShapeSet(frequency=[300.0, 900.0], damping=[0.02, 0.02],
                                shape_matrix=rng.normal(size=(2, len(dofs))), coordinate=dofs))
    try:
        out = shown(plate, named(plate, 'frf'), 'Fit', validate=validate)
        (plot,) = out['views']
        assert plot['view'] == 'spectrum' and 'resynthesized' in plot['title']
        followers = [s for s in plot['series'] if 'follows' in s]
        assert followers and all(s['name'].endswith(', synthesized') for s in followers)
        assert all(0 <= s['follows'] < len(plot['series']) - len(followers) for s in followers)
        assert 'resynthesized from 2 modes' in out['note']
        one = shown(plate, named(plate, 'frf'), 'Fit', records={'Fit': [0]})
        assert 'from 1 mode' in one['note']
    finally:
        project.remove('Fit')


def test_dof_arrows_and_sensors_mark_every_stage(plate, validate):
    project = plate.project
    geometry, history = named(plate, 'geometry'), named(plate, 'time_history')
    plain = shown(plate, geometry, history, validate=validate)
    assert plain['offers']['arrows'] is True and plain['offers']['sensors'] is False
    assert 'marks' not in plain['views'][0]
    marked = shown(plate, geometry, history, prefs={'arrows': True}, validate=validate)
    stage = marked['views'][0]
    assert stage['view'] == 'shape3d' and stage['marks']['nodes'].shape[1] == 3
    assert stage['marks']['lines'].dtype == np.uint32 and len(stage['marks']['lines']) > 0
    assert 'DOF arrows' in marked['note']
    table_name = next(n for n, o in project.items() if projects.kind_of(o) == 'channel_table')
    table = project[table_name]
    was = table['model'][0]
    table.set_cell('model', 0, 'generic')
    try:
        offered = shown(plate, geometry, history)
        assert offered['offers']['sensors'] is True
        with_sensors = shown(plate, geometry, history, prefs={'sensors': True, 'arrows': False},
                             validate=validate)
        assert '1 sensors' in with_sensors['note'] and 'marks' not in with_sensors['views'][0]
        stage = with_sensors['views'][0]
        bodies = stage['bodies']
        assert bodies['triangles'].dtype == np.uint32 and len(bodies['triangles']) > 0
        # each point follows the drawn node its sensor sits on
        sensor = project.sensors(table_name)[0][0]
        follow = set(bodies['follow'].tolist())
        assert len(follow) == 1 and int(stage['node_ids'][follow.pop()]) == sensor.node
    finally:
        table.set_cell('model', 0, was)


def test_picked_modes_limit_every_view_of_a_lone_shape_set(plate, validate):
    """Modes picked in the tree are what the panes show: the animation
    steps through them, the table lists them, the auto-MAC is theirs and
    the values keep their columns, each still numbered as the set numbers
    it (Brandon, 2026-11-05)."""
    project = plate.project
    geometry = project[named(plate, 'geometry')]
    dofs = [f'{int(n)}Z+' for n in geometry.node_id[:6]]
    project.add('Three', ShapeSet(frequency=[10.0, 20.0, 30.0], damping=[0.01, 0.02, 0.03],
                                  shape_matrix=np.arange(18.0).reshape(3, 6), coordinate=dofs))
    try:
        picks = {'Three': [2, 0]}
        (stage,) = shown(plate, 'Three', records=picks, validate=validate)['views']
        assert [m['index'] for m in stage['modes']] == [1, 3]
        (table,) = shown(plate, 'Three', records=picks, prefs={'shape_view': 'table'},
                         validate=validate)['views']
        assert [row[0] for row in table['rows']] == ['1', '3']
        assert table['edit']['rows'] == [1, 3], 'an edit of the second shown row is mode 3'
        stage, matrix = shown(plate, 'Three', records=picks, prefs={'shape_view': 'mac'},
                              validate=validate)['views']
        assert [o['index'] for o in stage['overlays']] == [1, 3], 'the MAC picks a pair to overlay'
        assert matrix['selects'] == 'shape3d'
        (values,) = shown(plate, 'Three', records=picks, prefs={'shape_view': 'values'},
                          validate=validate)['views']
        assert matrix['values'].shape == (2, 2)
        assert matrix['rows']['ticks'] == ['10.0 Hz', '30.0 Hz']
        assert values['columns'] == ['DOF', 'M1', 'M3']
        assert '2 modes picked' in shown(plate, 'Three', records=picks)['note']
        one = shown(plate, 'Three', records={'Three': [1]}, prefs={'shape_view': 'mac'},
                    validate=validate)
        assert [m['index'] for m in one['views'][0]['modes']] == [2]
        assert one['views'][1]['values'].shape == (1, 1)
        every = shown(plate, 'Three', prefs={'shape_view': 'mac'}, validate=validate)
        assert len(every['views'][0]['modes']) == 3 and every['views'][1]['values'].shape == (3, 3)
    finally:
        project.remove('Three')


def test_a_finite_element_set_offers_no_dof_arrows(plate, validate):
    """A measured set's few DOFs are worth marking; a mesh's hundreds of
    thousands are not (`ARROW_DOF_LIMIT`)."""
    from hertzforge.service import show
    from hertzforge.service.show import ARROW_DOF_LIMIT

    project = plate.project
    geometry = project[named(plate, 'geometry')]
    few = [f'{int(n)}Z+' for n in geometry.node_id[:6]]
    n = ARROW_DOF_LIMIT + 1
    project.add('Few', ShapeSet(frequency=[10.0], damping=[0.01],
                                shape_matrix=np.ones((1, 6)), coordinate=few))
    project.add('Dense', ShapeSet(frequency=[10.0], damping=[0.01], shape_matrix=np.ones((1, n)),
                                  coordinate=[f'{k + 1}X+' for k in range(n)]))
    try:
        # the plate's channel table would mark its own channels; a set with no table marks its DOFs
        assert len(show._arrow_dofs(project, ['Few'], None)[0]) == 6
        assert show._arrow_dofs(project, ['Dense'], None) == ([], [])
        assert shown(plate, 'Few', validate=validate)['offers']['arrows'] is True
    finally:
        project.remove('Dense')
        project.remove('Few')


def test_a_channel_table_shows_typed_into_with_its_widgets(plate, validate):
    import numpy as np

    from hertzforge.core.channel_table import Environment

    project = plate.project
    name = next(n for n, o in project.items() if projects.kind_of(o) == 'channel_table')
    out = shown(plate, name, validate=validate)
    (table,) = out['views']
    edit = table['edit']
    assert edit['op'] == 'project.set_channel_cell' and edit['kinds']['control'] == 'flag'
    assert edit['kinds']['channel_type'] == 'choice' and 'acceleration' in edit['choices']['channel_type']
    assert edit['kinds']['expiration'] == 'date' and edit['kinds']['sensitivity'] == 'number'
    assert edit['kinds']['unit'] == 'unit' and 'g' in edit['choices']['unit']
    assert edit['choices']['model'], 'the catalog\'s models'
    assert out['offers']['environments'] == [] and out['offers']['table_views'] == ['channels']
    # a table with two loops: the Control column follows the picked one, and a
    # loop's transformation stands in the table's place
    held = project[name]
    numbers = [int(c) for c in held['channel'][:3]]
    held.environments['Random'] = Environment(controls=numbers[:2], drives=[],
                                              response_transformation=np.eye(2))
    held.environments['Sine'] = Environment(controls=numbers[2:], drives=[])
    try:
        two = shown(plate, name, validate=validate)
        assert two['offers']['environments'] == ['Random', 'Sine']
        assert two['offers']['table_views'] == ['channels', 'response']
        sine = shown(plate, name, prefs={'environment': 'Sine'}, validate=validate)
        assert sine['offers']['table_views'] == ['channels'], 'Sine holds no transformation'
        matrix = shown(plate, name, prefs={'environment': 'Random', 'table_view': 'response'},
                       validate=validate)
        (view,) = matrix['views']
        assert view['title'].endswith('Random, response transformation')
        assert view['columns'][0] == 'Row' and len(view['rows']) == 2 and 'edit' not in view
        with pytest.raises(ProtocolError, match='"table_view"'):
            projects.show(plate, {'objects': [name], 'prefs': {'table_view': 'inverse'}})
        with pytest.raises(ProtocolError, match='"environment"'):
            projects.show(plate, {'objects': [name], 'prefs': {'environment': 3}})
    finally:
        held.environments.clear()


# ---- photos and sine objects -----------------------------------------------------


def test_photos_show_as_images_and_picks_restrict_them(tmp_path, validate):
    from .test_projects import tiny_png

    for name, color in (('setup', (255, 0, 0)), ('closeup', (0, 255, 0))):
        (tmp_path / f'{name}.png').write_bytes(tiny_png(color))
        state, _ = projects.import_file(locals().get('state'), {'path': str(tmp_path / f'{name}.png')})
    out = shown(state, 'Photos', validate=validate)
    assert [(v['view'], v['title'], v['format']) for v in out['views']] == [
        ('image', 'setup', 'png'), ('image', 'closeup', 'png')]
    assert bytes(out['views'][0]['bytes']) == tiny_png((255, 0, 0)), 'the file\'s bytes, as read'
    picked = shown(state, 'Photos', records={'Photos': [1]}, validate=validate)
    assert [v['title'] for v in picked['views']] == ['closeup'] and picked['note'] == '1 photo'
    assert picked['offers']['tones'] == [] and not picked['offers']['sine_readings']


@pytest.fixture(scope='module')
def swept():
    from .test_sine_shock import _sweep

    project = _sweep()
    project.extract_sine('Sweep')
    return projects.OpenProject(project)


def test_a_sine_specification_and_its_levels_read_by_tone(swept, validate):
    """The sine readings: the stage by default (frequency across, time
    receding, amplitude up, every point at its own time), the flat plot on
    the bar's Flat: a specification alone draws the tone's requirement with
    its limits, the levels alone the extracted level, and both together the
    levels over the requirement, or the Level Error bars."""
    levels = named(swept, 'sine_level_set')
    stage = shown(swept, 'Profile', validate=validate)
    (path,) = stage['views']
    assert path['view'] == 'waterfall' and path['y']['unit'] == 's'
    assert len(path['y_values']) == len(path['x_values']) and path['roles'] == ['reference']
    assert path['y_range'][0] >= 0.5, 'the tone starts at its start time'
    assert stage['offers']['tones'] == ['Up'] and not stage['offers']['sine_readings']
    assert stage['offers']['flat'] is True
    both = shown(swept, 'Profile', levels, validate=validate)
    (path,) = both['views']
    assert path['roles'] == ['reference', 'measured'], 'the level is a path of its own'
    assert both['offers']['sine_readings'] is True
    bars = shown(swept, levels, 'Profile', prefs={'sine_reading': 'bars'}, validate=validate)
    assert [view['view'] for view in bars['views']] == ['waterfall', 'reading']

    flat = {'flat': True}
    spec = shown(swept, 'Profile', prefs=flat, validate=validate)
    (plot,) = spec['views']
    assert plot['view'] == 'spectrum' and [s['role'] for s in plot['series']] == ['reference']
    alone = shown(swept, levels, prefs=flat, validate=validate)
    assert [s['role'] for s in alone['views'][0]['series']] == ['measured']
    assert alone['views'][0]['title'].startswith('Up')
    both = shown(swept, 'Profile', levels, prefs=flat, validate=validate)
    assert [s['role'] for s in both['views'][0]['series']] == ['measured', 'reference']
    bars = shown(swept, levels, 'Profile', prefs={**flat, 'sine_reading': 'bars'}, validate=validate)
    (reading,) = bars['views']
    assert reading['view'] == 'reading'
    rows = {row['label']: row for row in reading['rows']}
    assert rows['101Z+']['unit'] == 'dB' and rows['101Z+']['status'] in ('pass', 'fail')
    picked = shown(swept, 'Profile', records={'Profile': [0]}, prefs=flat, validate=validate)
    assert 'at 101Z+' in picked['views'][0]['title']
    unknown = shown(swept, 'Profile', prefs={**flat, 'tone': 'Down'}, validate=validate)
    assert unknown['views'][0]['title'].startswith('Up'), 'a tone the set lacks falls to the first'


def _played_modes():
    """A plate's rigid modes and modal responses computed through them."""
    state, _ = projects.import_file(None, {'path': str(PLATE / 'geometry.unv'),
                                           'units': {'length_unit': 'mm'}})
    project = state.project
    shapes = project.generate_rigid_body_modes('Geometry')
    set_ = project[shapes]
    time = np.arange(50) / 100.0
    rows = np.vstack([np.sin(2 * np.pi * (k + 1) * time) for k in range(set_.num_shapes)])
    project.add('Responses', TimeHistory(
        time, rows, [f'M{k + 1}' for k in range(set_.num_shapes)], ordinate_dim='modal_length',
        comment=[f'{shapes}: modal displacement, mode {k + 1}' for k in range(set_.num_shapes)]))
    return state, shapes


def test_modal_responses_play_on_a_geometry_with_or_without_their_set(validate):
    """The geometry and the responses are enough, since the responses name
    the set they came from, and the set can be selected with them."""
    state, shapes = _played_modes()
    project = state.project
    out = projects.show(state, {'objects': ['Geometry', 'Responses'], 'records': {}, 'prefs': {}})
    wire_views(out['views'], validate)
    stage = out['views'][0]
    assert stage['view'] == 'shape3d' and 'motion' in stage and out['views'][1]['view'] == 'spectrum'
    motion = stage['motion']
    held = project[shapes]
    assert motion['modes'].shape == (held.num_shapes, len(motion['node']), 3)
    assert motion['values'].shape == (held.num_shapes, 50)
    assert motion['quantity'] == 'Length' and motion['value_unit'] == 'm'
    # one sample by hand: the offsets the shell sums are u = Phi q
    k = 7
    summed = np.tensordot(motion['values'][:, k], motion['modes'], axes=1)
    q = project['Responses'].ordinate[:, k]
    u = held.shape_matrix.T @ q
    first = list(held.coordinate).index(f'{int(stage["node_ids"][motion["node"][0]])}X+')
    assert summed[0, 0] == pytest.approx(u[first], rel=1e-5)
    assert motion['peak'] > 0
    three = projects.show(state, {'objects': ['Geometry', 'Responses', shapes], 'records': {},
                                  'prefs': {}})
    assert three['views'][0]['motion']['modes'].shape == motion['modes'].shape


def test_modal_responses_with_no_set_to_name_say_what_to_select():
    state, shapes = _played_modes()
    project = state.project
    project['Responses'].comment = ['nothing here'] * project['Responses'].num_records
    out = projects.show(state, {'objects': ['Geometry', 'Responses'], 'records': {}, 'prefs': {}})
    assert 'select their shape set' in ' '.join(out.get('notes', [out.get('note', '')]))
    three = projects.show(state, {'objects': ['Geometry', 'Responses', shapes], 'records': {},
                                  'prefs': {}})
    assert 'motion' in three['views'][0]
