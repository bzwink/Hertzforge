"""The Model workspace's tools: a geometry on its own, what its element
groups are made of, and its coincident nodes merged."""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from hertzforge.service import projects
from hertzforge.service.ops_model import OPERATIONS
from hertzforge.service.protocol import ProtocolError

from .conftest import wire_views

PLATE = Path(__file__).resolve().parents[2] / 'testdata' / 'plate'
OPS = {op.id: op for op in OPERATIONS}


@pytest.fixture
def project():
    state, _ = projects.import_file(None, {'path': str(PLATE / 'geometry.unv'),
                                           'units': {'length_unit': 'mm'}})
    return state.project


def run(project, op_id, **given):
    op = OPS[op_id]
    values = op.resolve(given, objects={name: projects.kind_of(obj)
                                        for name, obj in project.items()})
    return op.run(project, values)


def test_the_descriptors_validate(validate):
    for op in OPERATIONS:
        validate(op.to_json(), 'operation')
        assert op.workspace == 'geometry', 'the finite element work is in Geometry'


def test_a_geometry_is_drawn_on_its_own(project, validate):
    (view,) = run(project, 'project.view_geometry')
    wire_views([view], validate)
    assert view['view'] == 'shape3d' and view['modes'] == []
    assert len(view['nodes']) == len(project['Geometry'].node_id)


def test_plates_take_a_material_and_thickness_and_then_solve(project, validate):
    assert 'solve_modes' not in projects.verbs_of(project, 'Geometry')
    (reading,) = run(project, 'project.group_properties', material='7075-T6', thickness=6.35)
    wire_views([reading], validate)
    assert reading['rows'][0]['value'] == '7075-T6, 6.35 mm'
    held = project['Geometry'].group_properties[1]
    assert held.material.name == '7075-T6' and held.thickness == pytest.approx(0.00635)
    assert 'solve_modes' in projects.verbs_of(project, 'Geometry')
    shapes = project[project.solve_modes('Geometry', num_modes=8)]
    assert shapes.num_shapes == 8 and shapes.frequency[7] > 0
    assert OPS['project.group_properties'].edits


@pytest.mark.parametrize(('given', 'match'), [
    ({'groups': '7'}, 'has element groups 1; not 7'),
    ({'groups': 'one'}, 'not an element group number'),
    ({'thickness': 0.0}, 'thickness above 0'),
])
def test_what_the_groups_cannot_take_is_refused(project, given, match):
    with pytest.raises(ProtocolError, match=match) as error:
        run(project, 'project.group_properties', **given)
    assert error.value.code == 'refused'
    assert project['Geometry'].group_properties == {}


def test_coincident_nodes_are_merged_and_drawn(project):
    (view,) = run(project, 'project.merge_coincident_nodes', keep=True)
    assert view['view'] == 'shape3d', 'the geometry drawn, nothing said'


def _parts():
    """A floor plate and, 0.25 above it, a smaller foot plate, two element
    groups with nothing shared (the core's tie test), in a project."""
    import hertzforge
    from hertzforge import mesh
    from hertzforge.core import fem

    geometry = mesh.assemble(
        mesh.plane((0, 0, 0), (4, 0, 0), (0, 4, 0), 1, 'floor'),
        mesh.plane((1, 1, 0.25), (2, 0, 0), (0, 2, 0), 1, 'foot'))
    aluminum = fem.material('6061-T6')
    geometry.group_properties = {1: fem.GroupProperties(aluminum, 0.01)}
    project = hertzforge.Project('p')
    project.add('Plate', geometry)
    return project


def test_the_element_groups_table_lists_each_group(validate):
    project = _parts()
    (view,) = run(project, 'project.element_groups', geometry='Plate')
    wire_views([view], validate)
    named = [dict(zip(view['columns'], row, strict=True)) for row in view['rows']]
    assert [(g['Element group'], g['Name'], g['Elements are'], g['Count']) for g in named] == [
        ('1', 'floor', 'face', '16'), ('2', 'foot', 'face', '4')]
    assert named[0]['Material'] == '6061-T6' and float(named[0]['Thickness [m]']) == 0.01
    assert named[1]['Material'] == '' and named[1]['Thickness [m]'] == ''


def test_merging_element_groups_keeps_the_first(validate):
    project = _parts()
    with pytest.raises(ProtocolError, match='differ in material, thickness or section'):
        run(project, 'project.merge_element_groups', geometry='Plate', groups='1, 2')
    run(project, 'project.group_properties', geometry='Plate', groups='2',
        material='6061-T6', thickness=10.0)
    (view,) = run(project, 'project.merge_element_groups', geometry='Plate', groups='1, 2')
    assert [[row[0], row[1], row[3], row[4]] for row in view['rows']] == [['1', 'floor', 'face', '20']]
    with pytest.raises(ProtocolError, match='two or more'):
        run(project, 'project.merge_element_groups', geometry='Plate', groups='1')


def test_deleting_an_element_group_takes_its_elements():
    project = _parts()
    (view,) = run(project, 'project.delete_element_groups', geometry='Plate', groups='2')
    assert [row[1] for row in view['rows']] == ['floor']
    # the last group goes too, and the geometry stays (Brandon, 2026-11-13)
    (view,) = run(project, 'project.delete_element_groups', geometry='Plate', groups='1')
    assert view['rows'] == [] and 'Plate' in project
    assert len(project['Plate'].groups.ids) == 0


def test_a_patch_ties_to_a_group_by_name():
    project = _parts()
    patch = project['Plate'].elements_in('foot')[:1]
    (view,) = run(project, 'project.tie_elements', geometry='Plate', keep=True,
                  elements=str(int(patch[0])), to='floor')
    assert view['view'] == 'shape3d'
    assert len(project['Plate'].elements_in(3)) == 4, 'four links, in a group of their own'
    with pytest.raises(ProtocolError, match='name what the patch ties to'):
        run(project, 'project.tie_elements', geometry='Plate', elements='1', keep=True)


def _apart():
    """Two plates sharing a line of coincident nodes, meshed apart."""
    import hertzforge
    from hertzforge import mesh

    left = mesh.block((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1), 1.0, 'left', unit='m')
    right = mesh.block((1, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1), 1.0, 'right', unit='m')
    project = hertzforge.Project('p')
    project.add('Geometry', mesh.assemble(left, right, apart=True))
    return project


def test_merge_marks_the_places_and_a_click_leaves_one_out(validate):
    """The merge pane: every place is marked before anything merges, and a click on a
    place moves it between in and out, dim once it is out."""
    project = _apart()
    before = project['Geometry'].num_nodes
    reading, view = run(project, 'project.merge_coincident_nodes')
    wire_views([reading, view], validate)
    assert project['Geometry'].num_nodes == before, 'marking merges nothing'
    places = view['pick']['ids']
    assert len(places) == 4 and view['pick']['off'] == 0 and view['pick']['count'] == 1
    assert view['node_ids'][:4].tolist() == places, 'a pick addresses the first rows'
    _reading, view = run(project, 'project.merge_coincident_nodes', toggle=str(places[0]))
    held = view['pick']
    assert held['off'] == 1 and held['ids'][-1] == places[0]
    assert held['params']['left_out'] == str(places[0])
    assert view['node_ids'][:4].tolist() == held['ids']
    _reading, view = run(project, 'project.merge_coincident_nodes', left_out=str(places[0]),
                         toggle=str(places[0]))
    assert view['pick']['off'] == 0, 'a second click puts it back'
    (done,) = run(project, 'project.merge_coincident_nodes', left_out=str(places[0]), keep=True)
    assert done['view'] == 'shape3d'
    assert project['Geometry'].num_nodes == before - 3, 'one place of four left out'


def test_merge_leaves_every_place_out_with_all(validate):
    project = _apart()
    before = project['Geometry'].num_nodes
    _reading, view = run(project, 'project.merge_coincident_nodes', left_out='all')
    assert view['pick']['off'] == 4
    run(project, 'project.merge_coincident_nodes', left_out='all', keep=True)
    assert project['Geometry'].num_nodes == before


def test_align_marks_each_move_and_a_click_leaves_one_out(validate):
    import hertzforge
    from hertzforge import mesh

    fine = mesh.block((0, 0, -0.5), (1, 0, 0), (0, 1, 0), (0, 0, 0.5), 0.25, 'frame',
                      unit='in')
    coarse = mesh.block((0.02, 0.01, 0.05), (1, 0, 0), (0, 1, 0), (0, 0, 1 / 3), 1 / 3,
                        'wing', unit='in')
    project = hertzforge.Project('p')
    project.add('Wing', mesh.assemble(fine, coarse, apart=True))
    reading, view = run(project, 'project.align_meshes', geometry='Wing')
    wire_views([reading, view], validate)
    moves = view['pick']['ids']
    assert len(moves) == 16
    before = project['Wing'].node_xyz.copy()
    (done,) = run(project, 'project.align_meshes', geometry='Wing', left_out=str(moves[0]),
                  keep=True)
    assert done['view'] == 'shape3d'
    moved = np.linalg.norm(project['Wing'].node_xyz - before, axis=1) > 0
    assert moved.sum() == 15


def test_align_meshes_moves_the_coarser_mesh_onto_the_finer():
    import hertzforge
    from hertzforge import mesh

    fine = mesh.block((0, 0, -0.5), (1, 0, 0), (0, 1, 0), (0, 0, 0.5), 0.25, 'frame',
                      unit='in')
    coarse = mesh.block((0.02, 0.01, 0.05), (1, 0, 0), (0, 1, 0), (0, 0, 1 / 3), 1 / 3,
                        'wing', unit='in')
    project = hertzforge.Project('p')
    project.add('Wing', mesh.assemble(fine, coarse, apart=True))
    before = project['Wing'].node_xyz.copy()
    run(project, 'project.align_meshes', geometry='Wing', keep=True)
    moves = np.linalg.norm(project['Wing'].node_xyz - before, axis=1)
    assert (moves > 0).sum() == 16
    assert 0 < moves.max() <= 0.5 / 3 * 0.0254


def test_tie_picks_its_patch_by_clicking_and_lists_the_groups(validate):
    project = _parts()
    reading, view = run(project, 'project.tie_elements', geometry='Plate')
    wire_views([reading, view], validate)
    rows = {row['label']: row['value'] for row in reading['rows']}
    assert rows['Patch'] == 'none yet' and rows['Tie to'] == 'not set'
    pick = view['pick']
    assert pick['target'] == 'faces' and pick['kept'] == []
    first = int(project['Plate'].elem_id[0])
    assert first in pick['faces'].tolist()
    assert len(pick['faces']) == len(view['triangles']) // (3 if view['triangles'].ndim == 1 else 1)
    _reading, view = run(project, 'project.tie_elements', geometry='Plate', add=str(first))
    assert view['pick']['kept'] == [first]
    assert view['pick']['params']['elements'] == str(first)
    _reading, view = run(project, 'project.tie_elements', geometry='Plate',
                         elements=str(first), drop=str(first))
    assert view['pick']['kept'] == []
    _reading, view = run(project, 'project.tie_elements', geometry='Plate', elements=str(first),
                         side='second', add=str(first + 1))
    assert view['pick']['kept'] == [first + 1], 'the second patch is what shows'
    assert view['pick']['params']['second'] == str(first + 1)
    assert view['pick']['params']['elements'] == str(first)


def _stacked():
    """Two blocks a unit apart in Z, meshed apart: elements 1-8 and 9-16, groups 1 and 2."""
    import hertzforge
    from hertzforge import mesh

    geometry = mesh.assemble(
        mesh.block((0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1), 0.5, 'low'),
        mesh.block((0, 0, 1), (1, 0, 0), (0, 1, 0), (0, 0, 1), 0.5, 'high'), apart=True)
    assert list(geometry.groups.ids) == [1, 2] and len(geometry.elem_id) == 16
    project = hertzforge.Project('p')
    project.add('G', geometry)
    return project


def _link_spans(project, before):
    geometry = project['G']
    spans = []
    for conn in geometry.elem_conn[before:]:
        a, b = geometry.node_xyz[geometry.node_index(conn)]
        spans.append(float(np.linalg.norm(a - b)))
    return spans


def test_a_second_patch_of_one_element_is_that_element_and_not_a_group():
    project = _stacked()
    before = len(project['G'].elem_id)
    run(project, 'project.tie_elements', geometry='G', elements='5', second='2', keep=True)
    through_the_form = project['G'].elem_conn[before:]
    direct = _stacked()
    direct.tie_elements('G', [5], [2])
    assert [c.tolist() for c in through_the_form] == [
        c.tolist() for c in direct['G'].elem_conn[before:]]
    grouped = _stacked()
    run(grouped, 'project.tie_elements', geometry='G', elements='5', to='2', keep=True)
    assert max(_link_spans(project, before)) < max(_link_spans(grouped, before)), (
        'element 2 sits beside it; element group 2 is the other block')


def test_clicks_on_the_second_patch_keep_what_was_picked():
    project = _stacked()
    _reading, view = run(project, 'project.tie_elements', geometry='G', side='second', add='1')
    assert view['pick']['kept'] == [1]
    held = view['pick']['params']
    assert held['second'] == '1' and held['to'] == ''
    _reading, view = run(project, 'project.tie_elements', geometry='G', side='second',
                         second=held['second'], add='2')
    assert view['pick']['kept'] == [1, 2], 'an id that is also a group id starts nothing over'
    _reading, view = run(project, 'project.tie_elements', geometry='G', side='second',
                         second='1, 2', drop='1')
    assert view['pick']['kept'] == [2]


def test_tie_to_is_a_group_and_says_so():
    project = _stacked()
    with pytest.raises(ProtocolError, match='no element group .* its own field'):
        run(project, 'project.tie_elements', geometry='G', elements='9', to='5, 6', keep=True)
    with pytest.raises(ProtocolError, match='not both'):
        run(project, 'project.tie_elements', geometry='G', elements='9', to='low', second='5',
            keep=True)
    links = len(project['G'].elem_id)
    run(project, 'project.tie_elements', geometry='G', elements='9', to='low', keep=True)
    assert len(project['G'].elem_id) == links + 8, 'eight links added'


def test_tie_finds_every_elements_faces_without_a_call_per_element(monkeypatch):
    from hertzforge.drawing.viz import geometry as viz
    from hertzforge.service import ops_model

    project = _stacked()
    geometry = project['G']
    expected = {}
    for row, element in enumerate(geometry.elem_id):
        faces, _lines, _points = viz.element_cells(geometry, [row])
        expected[int(element)] = sorted(tuple(sorted(f)) for group in faces.values() for f in group)
    calls = []
    original = viz.element_cells
    monkeypatch.setattr(viz, 'element_cells',
                        lambda *a, **k: calls.append(1) or original(*a, **k))
    used, triangles, owners = ops_model._element_faces(geometry)
    assert len(calls) <= 1, 'each call reads the whole mesh, so one per element is quadratic'
    assert len(owners) == 16 * 12
    for element, want in expected.items():
        mine = used[triangles[owners == element]]
        assert len(mine) == 2 * len(want)
        assert {n for tri in mine for n in tri.tolist()} == {n for f in want for n in f}


def test_a_large_mesh_answers_a_click_in_time():
    import time

    import hertzforge
    from hertzforge import mesh
    from hertzforge.service import ops_model

    geometry = mesh.block((0, 0, 0), (2, 0, 0), (0, 1, 0), (0, 0, 1), 1 / 16, 'bar')
    assert len(geometry.elem_id) >= 8192
    project = hertzforge.Project('p')
    project.add('G', geometry)
    start = time.perf_counter()
    ops_model._element_faces(geometry)
    assert time.perf_counter() - start < 3.0


def test_align_on_an_undefined_geometry_takes_its_reach_in_the_files_numbers():
    import hertzforge
    from hertzforge import mesh

    fine = mesh.block((0, 0, -3), (6, 0, 0), (0, 6, 0), (0, 0, 3), 1.5, 'frame', unit=None)
    coarse = mesh.block((0.3, 0.2, 3.5), (6, 0, 0), (0, 6, 0), (0, 0, 2), 2.0, 'wing', unit=None)
    project = hertzforge.Project('p')
    project.add('Raw', mesh.assemble(fine, coarse, apart=True))
    assert not project['Raw'].units_defined
    assert project['Raw'].align_moves(5.0)
    (reading, _view) = run(project, 'project.align_meshes', geometry='Raw', reach=5.0)
    found = {row['label']: row for row in reading['rows']}
    assert found['Moves']['value'] > 0, 'a reach of 5, not of 0.005'
    before = project['Raw'].node_xyz.copy()
    (view,) = run(project, 'project.align_meshes', geometry='Raw', reach=5.0, keep=True)
    assert view['unit'] == 'undefined'
    assert 0.5 < np.linalg.norm(project['Raw'].node_xyz - before, axis=1).max() < 5.0
