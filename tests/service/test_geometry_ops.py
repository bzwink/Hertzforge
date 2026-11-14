"""Editing a geometry and what is drawn on it, and the project's structure:
entity tables and edits, the coordinate system gizmo, DOF arrows and
sensors, Work Up, channel roles and DOF names, a
channel table's control loops, and the object groups."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

import hertzforge
from hertzforge.core.report import Report
from hertzforge.service import projects
from hertzforge.service.ops_geometry import OPERATIONS, id_runs_text, parse_id_runs
from hertzforge.service.protocol import ProtocolError

from .conftest import wire_views

PLATE = Path(__file__).resolve().parents[2] / 'testdata' / 'plate'
OPS = {op.id: op for op in OPERATIONS}


@pytest.fixture
def project():
    state, _ = projects.import_file(None, {'path': str(PLATE / 'random.nc4')})
    state, _ = projects.import_file(state, {'path': str(PLATE / 'geometry.unv'),
                                            'units': {'length_unit': 'mm'}})
    return state.project


def run(project, op_id, **given):
    op = OPS[op_id]
    values = op.resolve(given, objects={name: projects.kind_of(obj)
                                        for name, obj in project.items()})
    return op.run(project, values)


def cells(table):
    return [dict(zip(table['columns'], row, strict=True)) for row in table['rows']]


def test_the_descriptors_validate(validate):
    for op in OPERATIONS:
        validate(op.to_json(), 'operation')


def test_id_runs_read_both_ways():
    assert id_runs_text([1, 2, 3, 7, 9, 10]) == '1-3 7 9-10'
    assert parse_id_runs('1-3, 7 10-9') == [1, 2, 3, 7, 9, 10]
    with pytest.raises(ProtocolError, match='not an id'):
        parse_id_runs('a-3')


def test_view_entities_lists_every_node_and_draws_them(project, validate):
    view, table = run(project, 'project.view_entities', kind='nodes')
    wire_views([view, table], validate)
    geometry = project['Geometry']
    assert len(table['rows']) == geometry.num_nodes == len(view['nodes'])
    second = cells(table)[1]
    assert second['Node'] == '102'
    assert float(second['X [m]']) == pytest.approx(float(geometry.node_xyz[1, 0]))
    view, table = run(project, 'project.view_entities', kind='groups')
    assert cells(table)[0]['Elements'] == '1-144'


def test_a_node_is_added_at_typed_coordinates(project, validate):
    view, table = run(project, 'project.add_node', x=0.1, y=0.2, z=0.3)
    wire_views([view, table], validate)
    last = cells(table)[-1]
    assert last['Node'] == '1314' and float(last['Z [m]']) == pytest.approx(0.3)
    assert view['nodes'][-1] == pytest.approx([0.1, 0.2, 0.3])
    with pytest.raises(ProtocolError, match='already exists'):
        run(project, 'project.add_node', id=101)
    with pytest.raises(ProtocolError, match='no coordinate system 7'):
        run(project, 'project.add_node', placement_cs=7)


def test_a_coordinate_system_is_added_turned_and_drawn_with_its_axes(project, validate):
    view, table = run(project, 'project.add_coordinate_system', x=0.1, rz=90.0,
                      name='turned')
    wire_views([view, table], validate)
    added = cells(table)[-1]
    assert added['Name'] == 'turned' and float(added['Rotation Z [°]']) == pytest.approx(90.0)
    geometry = project['Geometry']
    assert geometry.cs_matrix[1, 0] == pytest.approx([0.0, 1.0, 0.0], abs=1e-12)
    # two systems, each drawn as the gimbal is: its origin and three axes
    assert len(view['frames']) == 2
    assert view['frames'][1]['origin'] == pytest.approx([0.1, 0.0, 0.0])
    assert view['frames'][1]['axes'][0] == pytest.approx([0.0, 1.0, 0.0], abs=1e-12)
    assert len(view['nodes']) == geometry.num_nodes
    with pytest.raises(ProtocolError, match='already exists'):
        run(project, 'project.add_coordinate_system', id=1)


def test_elements_are_added_by_family(project, validate):
    view, table = run(project, 'project.add_element', family='triangles',
                      nodes='101 102 201')
    wire_views([view, table], validate)
    added = cells(table)[-1]
    assert added['Type'] == 'tri3' and added['Nodes'] == '101 102 201'
    assert added['Element group'] == 'element group 2'
    _view, table = run(project, 'project.add_element', family='beams', nodes='101-104')
    assert [row['Type'] for row in cells(table)[-3:]] == ['beam2'] * 3
    with pytest.raises(ProtocolError, match='joins 8 nodes; 4 are named'):
        run(project, 'project.add_element', family='quads', type='quad8',
            nodes='101 102 202 201')
    with pytest.raises(ProtocolError, match='holds quads'):
        run(project, 'project.add_element', family='triangles', nodes='101 102 201',
            group=1)
    with pytest.raises(ProtocolError, match='unknown nodes'):
        run(project, 'project.add_element', family='quads', nodes='101 102 202 9999')


def test_a_click_adds_what_the_table_in_hand_holds(project, validate):
    """The Add mode: the view says what a click adds, and
    running that with what was clicked adds it."""
    view, _table = run(project, 'project.view_entities', kind='nodes')
    wire_views([view], validate)
    assert view['pick']['op'] == 'project.add_node' and view['pick']['target'] == 'point'
    pick = view['pick']
    view, table = run(project, pick['op'], **pick['params'], x=0.01, y=0.02, z=0.0)
    assert cells(table)[-1]['Node'] == '1314'
    assert view['pick']['op'] == 'project.add_node', 'the next click adds another'
    view, _table = run(project, 'project.view_entities', kind='coordinate_systems')
    assert view['pick']['op'] == 'project.add_coordinate_system'
    _view, table = run(project, 'project.view_entities', kind='groups')
    assert 'pick' not in _view


def test_clicked_nodes_make_an_element_of_the_family_in_hand(project, validate):
    view, _table = run(project, 'project.view_entities', kind='elements')
    wire_views([view], validate)
    pick = view['pick']
    geometry = project['Geometry']
    assert pick['target'] == 'nodes' and pick['param'] == 'nodes'
    assert pick['ids'] == [int(n) for n in geometry.node_id], 'one id a drawn row'
    assert pick['count'] == 4 and pick['params']['family'] == 'quads'
    view, table = run(project, 'project.add_element', family='beams', nodes='101 102')
    pick = view['pick']
    assert pick['count'] == 0 and pick['params']['family'] == 'beams', 'a chain, then Enter'
    _view, table = run(project, pick['op'], **pick['params'], nodes='102, 103, 104')
    assert [row['Type'] for row in cells(table)[-2:]] == ['beam2'] * 2


def test_an_element_group_is_added_empty(project):
    _view, table = run(project, 'project.add_element_group', name='brackets')
    added = cells(table)[-1]
    assert (added['Element group'], added['Name'], added['Elements']) == ('2', 'brackets', '')
    assert (added['Elements are'], added['Count'], added['Material']) == ('empty', '0', '')
    with pytest.raises(ProtocolError, match='already exists'):
        run(project, 'project.add_element_group', id=1)


def test_a_field_is_set_and_an_inconsistent_edit_is_put_back(project, validate):
    view, table = run(project, 'project.set_entity', kind='nodes', entity=101,
                      field='z', value='0.5')
    wire_views([view, table], validate)
    assert float(cells(table)[0]['Z [m]']) == pytest.approx(0.5)
    _view, table = run(project, 'project.set_entity', kind='nodes', entity=102, field='id',
                       value='5000')
    assert '5000' in [row['Node'] for row in cells(table)]
    assert 5000 in project['Geometry'].elem_conn[0]
    before = int(project['Geometry'].elem_type[0])
    with pytest.raises(ProtocolError, match='element 1, type'):
        run(project, 'project.set_entity', kind='elements', entity=1, field='type',
            value='tri3')
    assert int(project['Geometry'].elem_type[0]) == before
    with pytest.raises(ProtocolError, match='has the fields'):
        run(project, 'project.set_entity', kind='groups', entity=1, field='x', value='1')
    with pytest.raises(ProtocolError, match='no node 7'):
        run(project, 'project.set_entity', kind='nodes', entity=7, field='x', value='1')


def test_deleting_keeps_measured_nodes_and_the_last_system(project, validate):
    view, table = run(project, 'project.delete_entities', kind='nodes', ids='102')
    wire_views([view, table], validate)
    assert table['title'].endswith('removed 1 node, 2 elements'), 'said once, in the title'
    assert len(table['rows']) == 168
    with pytest.raises(ProtocolError, match='measured at node 101'):
        run(project, 'project.delete_entities', kind='nodes', ids='101')
    with pytest.raises(ProtocolError, match='at least one coordinate system'):
        run(project, 'project.delete_entities', kind='coordinate_systems', ids='1')
    with pytest.raises(ProtocolError, match='no element 9999'):
        run(project, 'project.delete_entities', kind='elements', ids='9999')
    _view, table = run(project, 'project.delete_entities', kind='elements', ids='3-12')
    # the rest of the first row of quads: their own nodes go, the measured stay
    assert 'removed 10 elements' in table['title']
    assert {101, 104, 110, 113} <= set(project['Geometry'].node_id.tolist())


def test_a_coordinate_system_turns_slides_and_resets(project, validate):
    run(project, 'project.add_coordinate_system', x=0.1, name='gauge')
    view, table = run(project, 'project.rotate_coordinate_system', cs=2, axis='z',
                      angle=30.0)
    assert view['gizmo']['turn']['op'] == 'project.rotate_coordinate_system'
    assert view['gizmo']['slide']['values']['y'] == 'distance'
    assert view['gizmo']['slide']['adds']['y'] == {'axis': 'y'}
    assert view['gizmo']['slide']['params']['cs'] == 2
    wire_views([view, table], validate)
    assert float(cells(table)[1]['Rotation Z [°]']) == pytest.approx(30.0)
    _view, table = run(project, 'project.slide_coordinate_system', cs=2, axis='x',
                       distance=0.02)
    row = cells(table)[1]
    assert float(row['Origin X [m]']) == pytest.approx(0.1 + 0.02 * np.cos(np.radians(30)),
                                                       abs=1e-6)
    assert float(row['Origin Y [m]']) == pytest.approx(0.02 * np.sin(np.radians(30)),
                                                       abs=1e-6)
    _view, table = run(project, 'project.reset_rotation', cs=2)
    row = cells(table)[1]
    assert float(row['Rotation Z [°]']) == 0.0
    assert float(row['Origin Y [m]']) == pytest.approx(0.01)
    with pytest.raises(ProtocolError, match='no coordinate system 3'):
        run(project, 'project.reset_rotation', cs=3)


def test_dof_arrows_mark_a_tables_references_pointing_in(project, validate):
    view, table = run(project, 'project.dof_arrows', source='Channel Table',
                      role='reference')
    wire_views([view, table], validate)
    assert [row['DOF'] for row in cells(table)] == ['101Z+', '110Z+', '1304Z+', '1310Z+']
    assert all(row['Arrow'].startswith('in') for row in cells(table))
    geometry = project['Geometry']
    from hertzforge.service.mesh import drawn_mesh

    edges = len(drawn_mesh(geometry)[2])
    # a shaft and two barbs each, the shaft ending on the node, after the elements' edges
    assert len(view['nodes']) == geometry.num_nodes + 4 * 4
    assert len(view['lines']) == edges + 12
    tip = view['nodes'][geometry.num_nodes + 1]
    assert tip == pytest.approx(geometry.node_xyz[0])
    _view, table = run(project, 'project.dof_arrows', source='Time History',
                       measured='acceleration')
    assert len(table['rows']) == 8
    assert all(row['Arrow'].startswith('out') for row in cells(table))
    with pytest.raises(ProtocolError, match='measures acceleration, force'):
        run(project, 'project.dof_arrows', source='Time History', measured='velocity')
    _view, table = run(project, 'project.dof_arrows', source='Geometry')
    assert len(table['rows']) == 3 * project['Geometry'].num_nodes


def placed_frame(project):
    """The first sensor's frame, to read its body in its own axes."""
    return project.sensors('Channel Table')[0][0].frame


def test_sensors_are_drawn_at_a_multiple_of_true_size(project, validate):
    with pytest.raises(ProtocolError, match='names no sensor model'):
        run(project, 'project.sensors')
    project['Channel Table'].set_cell('model', 0, '352A24')
    view, table, reading = run(project, 'project.sensors', size=10)
    wire_views([view, table, reading], validate)
    assert cells(table)[0]['Model'] == '352A24' and cells(table)[0]['Kind'] == 'uniaxial'
    bodies = view['bodies']
    assert len(view['nodes']) == project['Geometry'].num_nodes
    assert bodies['triangles'].dtype == np.uint32 and bodies['colors'].dtype == np.uint8
    assert len(bodies['nodes']) == len(bodies['colors']) == len(bodies['follow'])
    geometry = project['Geometry']
    row = int(np.flatnonzero(np.asarray(geometry.node_id) == 101)[0])
    # every point moves with node 101, the one the sensor sits on
    assert set(bodies['follow'].tolist()) == {row}
    # a 9.9 x 7.1 x 4.8 mm teardrop at ten times its size, centered on node 101, with
    # its cable out of the tail
    reach = np.ptp(bodies['nodes'] @ placed_frame(project), axis=0)
    # the round end is a polygon, so its width is a little short of 7.1 mm
    assert reach[1:] == pytest.approx([0.071, 0.048], rel=1e-3)
    assert reach[0] > 0.099


def test_work_up_goes_by_the_module_it_is_run_in(validate):
    """Each kind of test's module has its own Work Up, and nothing has to be
    set first."""
    state, _ = projects.import_file(None, {'path': str(PLATE / 'random.nc4')})
    project = state.project
    project.project_type = None
    (reading,) = run(project, 'project.work_up_random')
    wire_views([reading], validate)
    added = [row['value'] for row in reading['rows'] if row['label'] == 'Added']
    assert 'Time History PSDs' in added and added[-1] == 'Report'
    assert set(added) <= set(project)
    assert project.project_type == 'Random Vibration'
    (reading,) = run(project, 'project.work_up_random')
    assert reading['rows'][0]['value'] == 'nothing to compute'


def test_work_up_says_what_the_report_still_lacks():
    """A report whose optional slots are empty and whose blocks are unbound
    does not have everything it needs, whatever is required."""
    state, _ = projects.import_file(None, {'path': str(PLATE / 'random.nc4')})
    (reading,) = run(state.project, 'project.work_up_sysid')
    said = [(row['label'], row['value']) for row in reading['rows']]
    assert ('Missing', 'nothing required') not in said, 'one stream leaves a required PSD'
    assert ('Optional', 'Geometry') in said and ('Optional', 'Photos') in said
    unbound = state.project['Report'].unbound(state.project,
                                               state.project.object_groups)
    assert unbound and ('Unbound', f'{len(unbound)} blocks of Report') in said


def test_work_up_refuses_an_empty_project():
    with pytest.raises(ProtocolError, match='Nothing is loaded'):
        run(hertzforge.Project(), 'project.work_up_modal')


def test_each_kind_of_test_makes_its_own_report():
    """A report from Random does not count as Transient's, so working up a
    mixed run in both modules leaves one of each, and once more adds nothing."""
    state, _ = projects.import_file(None, {'path': str(PLATE / 'random.nc4')})
    project = state.project
    run(project, 'project.work_up_random')
    run(project, 'project.work_up_transient')
    titles = sorted(obj.title for _name, obj in project.items() if isinstance(obj, Report))
    assert len(titles) == 2 and len(set(titles)) == 2
    (reading,) = run(project, 'project.work_up_transient')
    assert reading['rows'][0]['value'] == 'nothing to compute'


def test_a_channel_role_moves_on_the_history_and_its_table(project, validate):
    (table,) = run(project, 'project.set_channel_role', source='Time History', record=2,
                   role='monitor')
    wire_views([table], validate)
    assert cells(table)[1]['Role'] == 'monitor'
    assert table['title'].endswith('4 references, 7 responses, 1 monitor')
    assert project['Channel Table'].roles()[1] == 'monitor'
    (table,) = run(project, 'project.set_channel_role', source='Channel Table', record=2,
                   role='response')
    assert project['Time History'].channel_roles()[('104Z+', 'acceleration')] == 'response'
    with pytest.raises(ProtocolError, match='no channel 13'):
        run(project, 'project.set_channel_role', source='Time History', record=13)


def test_a_dof_is_renamed_for_one_quantity(project, validate):
    table, reading = run(project, 'project.rename_dof', source='Time History', old='101Z+',
                         measured='force', new='102Z+')
    wire_views([table, reading], validate)
    dofs = project['Time History'].response_dof
    assert list(dofs).count('101Z+') == 1 and '102Z+' in list(dofs)
    assert reading['rows'][0]['value'] == 'Time History'
    with pytest.raises(ProtocolError, match='Time History'):
        run(project, 'project.rename_dof', source='Time History', old='999Z+', new='998Z+')
    with pytest.raises(ProtocolError, match='name the DOF'):
        run(project, 'project.rename_dof', source='Time History', old='101Z+')


def test_a_tables_control_loop_and_its_transformation(project, validate):
    from hertzforge.core.channel_table import Environment

    (table,) = run(project, 'project.channel_table_view', table='Channel Table')
    wire_views([table], validate)
    assert [row['Control'] for row in cells(table)] == ['True'] * 8 + ['False'] * 4
    (table,) = run(project, 'project.set_control', table='Channel Table', record=1,
                   control=False)
    assert cells(table)[0]['Control'] == 'False'
    with pytest.raises(ProtocolError, match='holds no response transformation'):
        run(project, 'project.channel_table_view', table='Channel Table', form='response')
    channels = project['Channel Table']
    channels.environments['Random'] = Environment(
        controls=[1, 2], drives=[9], response_transformation=[[0.5, 0.5], [1.0, -1.0]])
    (table,) = run(project, 'project.channel_table_view', table='Channel Table',
                   environment='Random', form='response')
    assert table['columns'] == ['Row', '101Z+', '104Z+']
    assert table['rows'][1][1:] == ['1', '-1']
    with pytest.raises(ProtocolError, match='not \'Sine\''):
        run(project, 'project.channel_table_view', table='Channel Table',
            environment='Sine')


def test_object_groups_are_linked_named_placed_and_given_the_basis(project, validate):
    (table,) = run(project, 'project.link', first='Geometry', second='Time History',
                   name='Run 1')
    wire_views([table], validate)
    group = cells(table)[0]
    assert group['Name'] == 'Run 1' and group['Members'].endswith('Geometry')
    (table,) = run(project, 'project.set_basis', object='Time History')
    assert cells(table)[0]['Basis'] == 'yes' and cells(table)[0]['Role'] == 'Basis'
    (table,) = run(project, 'project.unlink', object='Specification')
    assert 'Specification' not in cells(table)[0]['Members']
    (table,) = run(project, 'project.place', object='Specification', side='other')
    assert cells(table)[1]['Members'] == 'Specification'
    (table,) = run(project, 'project.set_role', object='Time History', role='none')
    assert cells(table)[0]['Role'] == ''
    (table,) = run(project, 'project.name_object_group', object='Time History', name='')
    assert cells(table)[0]['Name'] == ''
    with pytest.raises(ProtocolError, match='two or more different objects'):
        run(project, 'project.link', first='Geometry', second='Geometry')
    run(project, 'project.unlink', object='Specification')
    with pytest.raises(ProtocolError, match='in no object group'):
        run(project, 'project.unlink', object='Specification')
    with pytest.raises(ProtocolError, match='not in an object group'):
        run(project, 'project.set_basis', object='Specification')


def test_an_entity_table_is_edited_cell_by_cell(validate):
    """The entity tables carry their edit; a cell typed over goes through
    Set Entity for the entity on that row, and a bad value changes nothing."""
    state, _ = projects.import_file(None, {'path': str(PLATE / 'geometry.unv'),
                                           'units': {'length_unit': 'mm'}})
    project = state.project
    ops = {op.id: op for op in OPERATIONS}
    _drawn, table = ops['project.view_entities'].run(project, {'geometry': 'Geometry',
                                                               'kind': 'nodes', 'family': '',
                                                               'groups': ''})
    validate(table, 'tableView')
    assert table['edit']['op'] == 'project.set_entity_cell'
    assert table['edit']['columns'][1] == 'x'
    _drawn, after = ops['project.set_entity_cell'].run(project, {
        'geometry': 'Geometry', 'kind': 'nodes', 'row': 1, 'column': 'x', 'value': '0.5'})
    assert float(after['rows'][0][1]) == pytest.approx(0.5)
    with pytest.raises(ProtocolError, match="'wide' is not a number"):
        ops['project.set_entity_cell'].run(project, {
            'geometry': 'Geometry', 'kind': 'nodes', 'row': 1, 'column': 'x', 'value': 'wide'})
    with pytest.raises(ProtocolError, match='there is no row 99999'):
        ops['project.set_entity_cell'].run(project, {
            'geometry': 'Geometry', 'kind': 'nodes', 'row': 99999, 'column': 'x', 'value': '1'})


def _brick():
    import hertzforge
    from hertzforge import mesh

    project = hertzforge.Project('p')
    project.add('G', mesh.block((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1), 1.0, 'b'))
    return project


def test_a_node_with_data_keeps_its_id_as_it_cannot_be_deleted():
    project = _brick()
    project.generate_rigid_body_modes('G')
    geometry = project['G']
    node = int(geometry.node_id[0])
    with pytest.raises(ProtocolError, match='data in the project is measured at node'):
        run(project, 'project.set_entity', kind='nodes', entity=node, field='id', value='999')
    assert node in geometry.node_id and 999 not in geometry.node_id


@pytest.mark.parametrize(('kind', 'entity', 'field', 'value'), [
    ('nodes', 1, 'x', 'nan'), ('nodes', 1, 'y', 'inf'),
    ('coordinate_systems', 1, 'origin_x', 'nan'), ('coordinate_systems', 1, 'rotation_z', 'inf'),
    ('elements', 1, 'nodes', '1 2 3'), ('elements', 1, 'nodes', '1 1 1 1 1 1 1 1'),
    ('elements', 1, 'type', 'quad4')])
def test_a_cell_that_would_break_the_model_is_refused_and_left(kind, entity, field, value):
    project = _brick()
    geometry = project['G']
    xyz = geometry.node_xyz.copy()
    matrix = geometry.cs_matrix.copy()
    conn = [c.copy() for c in geometry.elem_conn]
    types = geometry.elem_type.copy()
    with pytest.raises(ProtocolError) as error:
        run(project, 'project.set_entity', kind=kind, entity=entity, field=field, value=value)
    assert error.value.code == 'refused'
    assert (geometry.node_xyz == xyz).all() and (geometry.cs_matrix == matrix).all()
    assert all((a == b).all() for a, b in zip(geometry.elem_conn, conn, strict=True))
    assert (geometry.elem_type == types).all()


def test_a_cell_typed_on_a_row_of_a_repeated_id_changes_that_row():
    import hertzforge
    from hertzforge.core.geometry import Geometry

    geometry = Geometry(node_id=[1, 2, 3, 4], node_xyz=np.eye(4)[:, :3].astype(float),
                        elem_id=[7, 7], elem_type=[21, 21],
                        elem_conn=[np.array([1, 2]), np.array([3, 4])], elem_group=[1, 1],
                        group_id=[1], group_name=['b'])
    project = hertzforge.Project('p')
    project.add('Q', geometry)
    run(project, 'project.set_entity_cell', geometry='Q', kind='elements', row=2, column='nodes',
        value='1 4')
    assert [c.tolist() for c in geometry.elem_conn] == [[1, 2], [1, 4]]


def _picked(project, part, validate):
    from hertzforge.service import show

    views = show.show(project, ['Geometry'], {}, part)['views']
    wire_views(views, validate)
    return views


def _edited(project, part, validate):
    """The part's Edit, as its row in the action table runs it."""
    from hertzforge.service.action_table import action_table

    rows = action_table(project, ['Geometry'], {}, part)['columns'][0]['rows']
    row = next(row for row in rows if row['verb'] == 'Edit')
    assert row['id'] == 'project.view_entities' and row['ready']
    assert {'kind', 'family', 'groups'} <= set(row['omit']), 'the pick says them all'
    assert {'geometry', 'kind', 'family', 'groups'} <= set(row['fill']), 'so it opens at once'
    views = run(project, row['id'], **row['fill'])
    wire_views(views, validate)
    return views


def test_a_part_picked_is_drawn_and_its_edit_holds_its_table(project, validate):
    """Brandon, 2026-11-13: the table is editing's, not the plot's."""
    for part in ('nodes', 'coordinate_systems'):
        (drawn,) = _picked(project, {'kind': 'part', 'id': part}, validate)
        assert drawn['view'] == 'shape3d' and 'pick' not in drawn
        assert ('frames' in drawn) is (part == 'coordinate_systems')
    drawn, nodes = _edited(project, {'kind': 'part', 'id': 'nodes'}, validate)
    assert drawn['view'] == 'shape3d' and drawn['pick']['op'] == 'project.add_node'
    assert len(nodes['rows']) == len(project['Geometry'].node_id)
    # Delete takes a row by its id, the first column
    assert nodes['edit']['remove'] == {
        'op': 'project.delete_entities', 'params': {'geometry': 'Geometry', 'kind': 'nodes'},
        'column': 0}
    drawn, systems = _edited(project, {'kind': 'part', 'id': 'coordinate_systems'}, validate)
    assert systems['title'] == 'Coordinate systems of Geometry' and 'frames' in drawn


def test_a_family_or_group_shows_its_own_elements_only(project, validate):
    geometry = project['Geometry']
    run(project, 'project.add_element', family='beams', nodes='101 102')
    (plot,) = _picked(project, {'kind': 'family', 'id': 'beams'}, validate)
    assert plot['view'] == 'shape3d'
    drawn, beams = _edited(project, {'kind': 'family', 'id': 'beams'}, validate)
    assert len(beams['rows']) == 1
    assert drawn['pick']['params']['family'] == 'beams'
    # its edit says the family, so its row 1 is the beam, not the geometry's first element
    assert beams['edit']['params'] == {'geometry': 'Geometry', 'kind': 'elements', 'family': 'beams'}
    edited = run(project, 'project.set_entity_cell', **beams['edit']['params'], row=1,
                 column='nodes', value='102 103')
    beam = int(np.flatnonzero(np.asarray(geometry.elem_type) == 21)[0])
    assert [int(n) for n in geometry.elem_conn[beam]] == [102, 103]
    assert len(edited[-1]['rows']) == 1, 'the family\'s table again'
    group = int(geometry.group_id[0])
    (plot,) = _picked(project, {'kind': 'group', 'id': group}, validate)
    assert plot['view'] == 'shape3d'
    _drawn, groups, elements = _edited(project, {'kind': 'group', 'id': group}, validate)
    assert [row[0] for row in groups['rows']] == [str(group)]
    assert len(elements['rows']) == int(np.sum(np.asarray(geometry.elem_group) == group))


def test_delete_in_a_parts_table_takes_its_rows(project, validate):
    _drawn, nodes = _edited(project, {'kind': 'part', 'id': 'nodes'}, validate)
    remove = nodes['edit']['remove']
    before = len(project['Geometry'].node_id)
    run(project, 'project.add_node', x=1.0, y=1.0, z=1.0)
    assert len(project['Geometry'].node_id) == before + 1
    added = str(int(project['Geometry'].node_id[-1]))
    run(project, remove['op'], **remove['params'], ids=added)
    assert len(project['Geometry'].node_id) == before


def test_the_solids_row_or_a_solid_shows_the_solids(validate):
    from hertzforge.service import show
    from hertzforge.service.ops_solids import OPERATIONS as SOLIDS

    state = projects.new({'title': 'x'})
    project = state.project
    project.new_geometry('Geometry', unit='m')
    block = next(op for op in SOLIDS if op.id == 'project.add_solid_block')
    block.run(project, block.resolve({'geometry': 'Geometry'}, objects={'Geometry': 'geometry'}))
    for part in ({'kind': 'solids', 'id': 0}, {'kind': 'solid', 'id': 1}):
        views = show.show(project, ['Geometry'], {}, part)['views']
        wire_views(views, validate)
        (drawn,) = views
        # the block drawn before it is meshed: lit in the bodies when it is the one picked
        lit = part['kind'] == 'solid'
        assert len(drawn['bodies']['triangles'] if lit else drawn['triangles']) == 12
        # a solid picked carries the gimbal, whose drag moves it; the row as a whole does not
        assert ('gizmo' in drawn) is (part['kind'] == 'solid')
    assert drawn['gizmo']['slide']['op'] == 'project.move_solids'
    assert drawn['gizmo']['slide']['params'] == {'geometry': 'Geometry', 'solids': '1'}


def test_a_long_entity_table_sends_a_page_and_the_rest_on_asking(validate):
    """A million elements whole took 2 s to write and 80 MB to send: past a thousand
    rows a table sends its first and its count, and `project.entity_rows` the rest."""
    import asyncio

    from hertzforge.service.ops_geometry import PAGE_ROWS

    from .test_projects import _exchange

    out = asyncio.run(_exchange([
        ('op.run', {'op': 'project.new_geometry', 'params': {}}),
        ('op.run', {'op': 'project.add_solid_plate', 'params': {'geometry': 'Geometry'}}),
        ('op.run', {'op': 'project.mesh_solids', 'params': {
            'geometry': 'Geometry', 'elements': 'quads', 'size': 0.02}}),
        ('op.run', {'op': 'project.view_entities', 'params': {
            'geometry': 'Geometry', 'kind': 'elements', 'family': 'quads'}}),
        ('project.entity_rows', {'geometry': 'Geometry', 'kind': 'elements', 'family': 'quads',
                                 'start': PAGE_ROWS, 'count': 5}),
        ('project.entity_rows', {'geometry': 'Geometry', 'kind': 'elements', 'start': 0,
                                 'count': 99999}),
    ]))
    table = out[3][-1]['result']['views'][-1]
    validate(table, 'tableView')
    assert len(table['rows']) == PAGE_ROWS and table['total'] == 50 * 50
    assert table['more'] == {'method': 'project.entity_rows', 'params': {
        'geometry': 'Geometry', 'kind': 'elements', 'family': 'quads'}}
    page = out[4][-1]['result']['rows']
    assert [row[0] for row in page] == [str(PAGE_ROWS + k + 1) for k in range(5)]
    assert out[5][-1]['error']['code'] == 'bad_request'


def test_a_geometry_alone_draws_its_solids_as_its_parts_do():
    """The geometry picked and a part of it picked draw the same model, so the camera stays."""
    from hertzforge.service import show
    from hertzforge.service.ops_solids import OPERATIONS as SOLIDS

    state = projects.new({'title': 'x'})
    project = state.project
    project.new_geometry('Geometry', unit='m')
    block = next(op for op in SOLIDS if op.id == 'project.add_solid_block')
    block.run(project, block.resolve({'geometry': 'Geometry'}, objects={'Geometry': 'geometry'}))
    alone = show.show(project, ['Geometry'], {}, None)['views'][0]
    row = show.show(project, ['Geometry'], {}, {'kind': 'solids', 'id': 0})['views'][0]
    assert len(alone['triangles']) == len(row['triangles']) == 12
