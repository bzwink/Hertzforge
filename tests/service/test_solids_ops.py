"""Solid modeling: a new geometry, its solids added, changed and combined,
Mesh Solids, Mate Surfaces and the default view."""

from __future__ import annotations

import math

import numpy as np
import pytest

from hertzforge.core.fem import GroupProperties, material
from hertzforge.project import Project
from hertzforge.service import projects
from hertzforge.service.ops_geometry import OPERATIONS as GEOMETRY_OPERATIONS
from hertzforge.service.ops_solids import OPERATIONS
from hertzforge.service.protocol import ProtocolError

from .conftest import wire_views

OPS = {op.id: op for op in (*OPERATIONS, *GEOMETRY_OPERATIONS)}


@pytest.fixture
def project():
    project = Project()
    project.new_geometry('Part', unit='mm')
    return project


def run(project, op_id, **given):
    op = OPS[op_id]
    values = op.resolve(given, objects={name: projects.kind_of(obj)
                                        for name, obj in project.items()})
    return op.run(project, values)


def rows(reading):
    return {row['label']: row for row in reading['rows']}


def refused(project, op_id, match, **given):
    with pytest.raises(ProtocolError, match=match) as error:
        run(project, op_id, **given)
    assert error.value.code == 'refused'


def block(project, **given):
    sizes = {'width_x': 0.1, 'width_y': 0.05, 'width_z': 0.01}
    return run(project, 'project.add_solid_block', **{**sizes, **given})


def test_the_descriptors_validate(validate):
    for op in OPERATIONS:
        validate(op.to_json(), 'operation')
        assert op.workspace in ('geometry', 'model')


def test_a_new_geometry_is_empty_and_records_its_unit(project, validate):
    (reading,) = run(project, 'project.new_geometry', unit='in')
    wire_views([reading], validate)
    assert rows(reading)['Added']['value'] == 'Geometry'
    assert project['Geometry'].length_unit == 'in' and project['Geometry'].solids == []
    with pytest.raises(ProtocolError, match='not one of the choices'):
        run(project, 'project.new_geometry', unit='furlong')


def test_a_block_is_added_and_drawn_with_nothing_said_beside_it(project, validate):
    """Adding a solid answers with the drawing alone: no box saying it was added, no solids
    table (Brandon, 2026-11-13)."""
    from hertzforge.core import solids
    (view,) = block(project, x=0.02, name='base')
    wire_views([view], validate)
    # the block just added is drawn lit, in the view's bodies
    assert view['view'] == 'shape3d' and len(view['bodies']['triangles']) >= 12
    assert np.ptp(view['bodies']['nodes'][:, 0]) == pytest.approx(0.1, rel=1e-5)
    item = solids.solid(project['Part'], 1)
    assert (item.name, item.kind, tuple(item.widths)) == ('base', 'block', (0.1, 0.05, 0.01))
    assert tuple(item.center) == (0.02, 0.0, 0.0)
    refused(project, 'project.add_solid_block', 'width', width_x=0.0)


@pytest.mark.parametrize(('op_id', 'given', 'label', 'expected'), [
    ('project.add_solid_cylinder', {'radius': 0.01, 'length': 0.02}, 'Volume',
     math.pi * 0.01 ** 2 * 0.02),
    ('project.add_solid_cone', {'radius': 0.02, 'top_radius': 0.01, 'height': 0.03},
     'Volume', math.pi * 0.03 * (0.02 ** 2 + 0.02 * 0.01 + 0.01 ** 2) / 3),
    ('project.add_solid_sphere', {'radius': 0.02}, 'Volume', 4 / 3 * math.pi * 0.02 ** 3),
    ('project.add_solid_plate', {'width_x': 0.2, 'width_y': 0.1}, 'Area', 0.02),
    ('project.add_solid_disk', {'radius': 0.05}, 'Area', math.pi * 0.05 ** 2),
    ('project.add_solid_section', {'shape': 'I-beam', 'dimensions': '0.1, 0.05, 0.01, 0.005',
                                   'length': 0.5}, 'Volume',
     (2 * 0.05 * 0.01 + 0.08 * 0.005) * 0.5),
    ('project.add_solid_hole', {'diameter': 0.01, 'depth': 0.02}, 'Volume',
     math.pi * 0.005 ** 2 * 0.02),
])
def test_each_solid_is_made_its_size(project, validate, op_id, given, label, expected):
    from hertzforge.core import solids
    (view,) = run(project, op_id, **given)
    wire_views([view], validate)
    measure = solids.area if label == 'Area' else solids.volume
    assert measure(project['Part'], 1) == pytest.approx(expected, rel=2e-3)
    assert len(project['Part'].solids) == 1


@pytest.mark.parametrize(('op_id', 'given', 'match'), [
    ('project.add_solid_cylinder', {'radius': 0.0}, 'radius'),
    ('project.add_solid_cone', {'radius': 0.1, 'top_radius': 0.1}, 'is a cylinder'),
    ('project.add_solid_sphere', {'radius': 0.0}, 'radius'),
    ('project.add_solid_plate', {'width_y': 0.0}, 'width'),
    ('project.add_solid_disk', {'radius': 0.0}, 'radius'),
    ('project.add_solid_section', {'shape': 'angle', 'dimensions': '1, 2'}, 'takes long leg'),
    ('project.add_solid_hole', {'diameter': 0.01, 'counterbore_diameter': 0.005,
                                'counterbore_depth': 0.002}, 'counterbore wider'),
])
def test_what_a_solid_cannot_be_is_refused(project, op_id, given, match):
    refused(project, op_id, match, **given)
    assert project['Part'].solids == []


def test_a_section_opens_on_its_shape_s_dimensions(project):
    from hertzforge.core import solids
    run(project, 'project.add_solid_section', shape='round tube')
    outer, wall = 1.0, 0.1
    assert solids.volume(project['Part'], 1) == pytest.approx(
        math.pi / 4 * (outer ** 2 - (outer - 2 * wall) ** 2) * 4.0, rel=2e-3)
    item = solids.solid(project['Part'], 1)
    assert (tuple(item.dimensions), item.length) == ((1.0, 0.1), 4.0)


def test_a_cylinder_subtracted_from_a_block_leaves_a_hole(project, validate):
    block(project)
    run(project, 'project.add_solid_cylinder', radius=0.01, length=0.02)
    (view,) = run(project, 'project.combine_solids', operation='subtract',
                  solids='1, 2', name='plate')
    wire_views([view], validate)
    from hertzforge.core import solids
    assert solids.volume(project['Part'], 3) == pytest.approx(
        0.1 * 0.05 * 0.01 - math.pi * 0.01 ** 2 * 0.01, rel=1e-6)
    made = solids.solid(project['Part'], 3)
    assert (made.kind, tuple(made.operands)) == ('subtract', (1, 2))
    assert [item.id for item in solids.top_level(project['Part'])] == [3]
    refused(project, 'project.combine_solids', 'part of solid 3', solids='1, 2')
    refused(project, 'project.combine_solids', 'not 9', solids='3, 9')


def test_a_solid_is_edited_by_the_fields_typed(project):
    block(project)
    from hertzforge.core import solids
    (view,) = run(project, 'project.edit_solid', solid=1, widths='0.2, 0.05, 0.01', name='wide')
    assert view['view'] == 'shape3d'
    item = solids.solid(project['Part'], 1)
    assert (item.name, tuple(item.widths)) == ('wide', (0.2, 0.05, 0.01))
    refused(project, 'project.edit_solid', 'type what changes', solid=1)
    refused(project, 'project.edit_solid', 'has no radius', solid=1, radius='0.1')
    refused(project, 'project.edit_solid', 'no solid 4', solid=4, name='x')


def test_parts_are_moved_and_turned_whole(project):
    block(project)
    run(project, 'project.add_solid_cylinder', radius=0.01, length=0.02)
    run(project, 'project.combine_solids', operation='subtract', solids='1, 2')
    (view,) = run(project, 'project.move_solids', solids='3', offset_z=0.5)
    from hertzforge.core import solids
    part = project['Part']
    assert solids.solid(part, 1).center[2] == pytest.approx(0.5)
    assert solids.solid(part, 2).center[2] == pytest.approx(0.5)
    assert view['view'] == 'shape3d' and 'gizmo' in view, 'the move is drawn, nothing said'


def test_solids_are_copied_into_another_geometry(project):
    block(project)
    project.new_geometry('Other', unit='m')
    (view,) = run(project, 'project.copy_solids', geometry='Part', solids='1', into='Other')
    assert len(project['Other'].solids) == 1 and view['title'] == 'Other'
    refused(project, 'project.copy_solids', 'not 5', geometry='Part', solids='5',
            into='Other')


def test_the_parts_are_reordered(project):
    block(project)
    run(project, 'project.add_solid_sphere', radius=0.01)
    from hertzforge.core import solids
    run(project, 'project.reorder_solids', solids='2, 1')
    assert [item.id for item in solids.top_level(project['Part'])] == [2, 1]
    refused(project, 'project.reorder_solids', 'give each of them', solids='2')


def test_a_boolean_is_taken_apart_and_deleted(project):
    block(project)
    run(project, 'project.add_solid_sphere', radius=0.01)
    run(project, 'project.combine_solids', operation='union', solids='1, 2')
    (view,) = run(project, 'project.separate_solids', solids='3')
    assert view['view'] == 'shape3d' and len(project['Part'].solids) == 2
    refused(project, 'project.separate_solids', 'is a block', solids='1')
    (view,) = run(project, 'project.delete_solids', solids='1, 2')
    assert view['view'] == 'shape3d' and project['Part'].solids == []
    refused(project, 'project.delete_solids', 'no solids yet', solids='')


def test_a_pattern_copies_its_solids(project, validate):
    run(project, 'project.add_solid_cylinder', radius=0.01, length=0.02)
    from hertzforge.core import solids
    (view,) = run(project, 'project.pattern_solids', solids='1', count_x=3,
                  count_y=2, spacing_x=0.05, spacing_y=0.04)
    wire_views([view], validate)
    made = solids.solid(project['Part'], 2)
    assert (tuple(made.counts), tuple(made.widths)) == ((3, 2, 1), (0.05, 0.04, 0.0))
    # the pattern just made is drawn lit, in the view's bodies
    assert np.ptp(view['bodies']['nodes'][:, 0]) == pytest.approx(0.12, rel=1e-3)
    refused(project, 'project.pattern_solids', 'needs a spacing', solids='2', count_y=2,
            spacing_y=0.0)
    refused(project, 'project.pattern_solids', 'no copies yet', solids='2', count_x=1)


def test_mesh_solids_meshes_at_once(project, validate):
    """There is no preview: meshing is quick, and a preview that meshed cost as much (Brandon,
    2026-11-13)."""
    assert 'keep' not in {p.name for p in OPS['project.mesh_solids'].params}
    block(project)
    (view,) = run(project, 'project.mesh_solids', size=0.005)
    wire_views([view], validate)
    assert project['Part'].num_nodes == 21 * 11 * 3
    assert len(project['Part'].elem_id) == 400
    (view,) = run(project, 'project.mesh_solids', size=0.01)
    assert len(project['Part'].elem_id) == 50, 'the 400 meshed before are replaced'
    assert view['view'] == 'shape3d'


def test_a_plate_meshes_into_shells_only(project):
    run(project, 'project.add_solid_plate', width_x=0.1, width_y=0.05)
    run(project, 'project.mesh_solids', elements='quads', size=0.01)
    assert len(project['Part'].elem_id) == 50
    refused(project, 'project.mesh_solids', 'is flat', elements='hexes', size=0.01)


def test_a_fit_likely_to_fold_is_meshed_only_when_asked(project):
    run(project, 'project.add_solid_sphere', radius=0.05)
    with pytest.raises(ProtocolError, match='likely to fold') as error:
        run(project, 'project.mesh_solids', size=0.05)
    assert error.value.code == 'refused' and project['Part'].num_nodes == 0
    assert '1.0 bricks across its radius' in error.value.message
    assert '0.01 m is likely to hold' in error.value.message
    reading, _view = run(project, 'project.mesh_solids', size=0.05, anyway=True)
    fit = rows(reading)['Fit']
    assert fit['status'] == 'warn' and project['Part'].num_nodes


def stacked(project):
    block(project, name='base')
    block(project, z=0.01, name='lid')
    run(project, 'project.mesh_solids', size=0.025, parts_apart=True)


def _pane(project, **given):
    """Mate Surfaces as the shell runs it: the views, and the pick a click runs again."""
    views = run(project, 'project.mate_surfaces', **given)
    view = next(view for view in views if view['view'] == 'shape3d')
    return views, view['pick']


def test_mated_surfaces_are_found_picked_kept_edited_and_deleted(project, validate):
    # Brandon, 2026-11-11: one pane makes a mate and edits it, nodes of either side picked
    stacked(project)
    views, pick = _pane(project, groups='1, 2')
    wire_views(views, validate)
    reading, patches, view, mates = views
    assert rows(reading)['Patches found']['value'] == 1.0
    assert patches['rows'][0][3] == '15 on 15' and mates['rows'] == []
    assert view['opacity'] < 1, 'the model is drawn see-through, the sides inside it'
    assert pick['count'] == 1 and pick['param'] == 'toggle'
    held = len(pick['ids']) - pick['off']
    assert held == 15 and pick['others'][0] == 15, 'each side shows what it holds'
    # a click takes a node of the first side away, and nothing is kept until Apply
    node = pick['ids'][0]
    views, after = _pane(project, **{**pick['params'], 'toggle': str(node)})
    assert after['off'] == pick['off'] + 1 and not project['Part'].mates
    assert str(node) not in after['params']['nodes_a'].split(', ')
    # the second side is picked once it is chosen, the first drawn beside it
    views, second = _pane(project, **{**after['params'], 'side': 'b'})
    assert len(second['ids']) - second['off'] == 15 and second['others'][0] == 14
    views, _pick = _pane(project, **{**after['params'], 'normal': 'stiffness',
                                     'normal_stiffness': 2e6, 'keep': True})
    assert views[-1]['rows'][0][2] == 'base on lid'
    kept = project['Part'].mates[0]
    assert len(kept.nodes_a) == 14 and node not in kept.nodes_a
    assert kept.normal_stiffness == 2e6 and kept.shear_mode == 'rigid'
    # the same pane on the kept mate: what it holds, changed only as the form says
    views, pick = _pane(project, mate=1, name='seat', normal='rigid', shear='free', keep=True)
    assert [view['view'] for view in views] == ['shape3d', 'table'], 'nothing said on Apply'
    assert project['Part'].mates[0].describe() == 'normal rigid, shear free'
    assert project['Part'].mates[0].name == 'seat'
    assert project['Part'].mates[0].nodes_a == kept.nodes_a
    views, pick = _pane(project, mate=1, toggle=str(node))
    views, _pick = _pane(project, **{**pick['params'], 'normal': 'rigid', 'shear': 'free',
                                     'name': 'seat', 'keep': True})
    assert node in project['Part'].mates[0].nodes_a, 'a click puts it back'
    refused(project, 'project.mate_surfaces', 'holds nothing', mate=1, normal='free',
            shear='free', keep=True)
    refused(project, 'project.mate_surfaces', 'not 9', mate=9)
    refused(project, 'project.mate_surfaces', 'not 9', groups='9')
    refused(project, 'project.delete_mated_surfaces', 'not 4', mates='4')
    (mates,) = run(project, 'project.delete_mated_surfaces', mates='1')
    assert mates['rows'] == [] and not project['Part'].mates
def test_the_default_view_is_set_and_reset(project, validate):
    block(project)
    (view,) = run(project, 'project.set_view', eye_x=1.0, eye_y=1.0, eye_z=-1.0,
                  up_y=1.0, up_z=0.0)
    wire_views([view], validate)
    assert tuple(project['Part'].view.eye) == pytest.approx((1.0, 1.0, -1.0))
    run(project, 'project.set_view')
    assert project['Part'].view is None
    refused(project, 'project.set_view', 'line of sight', eye_z=1.0, up_z=1.0)


def test_the_solids_are_drawn_with_no_table(project, validate):
    wire_views(run(project, 'project.solids'), validate)
    block(project)
    (view,) = run(project, 'project.solids')
    wire_views([view], validate)
    assert view['view'] == 'shape3d' and len(view['triangles']) == 12


def test_move_part_offers_arrows_and_rings(project, validate):
    moved = run(project, 'project.add_solid_block', x=0.5)
    wire_views(moved, validate)
    ids = str(project['Part'].solids[-1].id)
    views = run(project, 'project.move_solids', solids=ids)
    wire_views(views, validate)
    gizmo = next(view for view in views if view['view'] == 'shape3d')['gizmo']
    assert gizmo['origin'] == pytest.approx([0.5, 0.0, 0.0])
    assert gizmo['slide']['values']['x'] == 'offset_x'
    assert gizmo['turn']['values']['z'] == 'turn_z'
    run(project, gizmo['slide']['op'], **gizmo['slide']['params'], offset_x=0.25)
    views = run(project, 'project.move_solids', solids=ids)
    gizmo = next(view for view in views if view['view'] == 'shape3d')['gizmo']
    assert gizmo['origin'] == pytest.approx([0.75, 0.0, 0.0]), 'the handles follow'


def test_a_form_previews_on_a_copy(validate):
    """The live preview of a solid: nothing reaches the project or its journal."""
    import hertzforge
    from hertzforge.service import display_units, server

    project = hertzforge.Project('p')
    name = project.new_geometry('Part', unit='m')
    project.add_solid_block(name, (0, 0, 0), (1, 1, 1))
    held = len(project[name].solids)
    journal = list(project.journal)
    scratch = server._preview_copy(project, name)
    assert next(o for o in server.OPERATIONS if o.id == 'project.add_solid_block').previews
    views = server._said_as_preview(run(scratch, 'project.add_solid_block', x=2.0),
                                    display_units.DEFAULT)
    wire_views(views, validate)
    assert [view['view'] for view in views] == ['shape3d']
    assert len(scratch[name].solids) == held + 1
    assert len(project[name].solids) == held and project.journal == journal


def test_a_preview_over_the_socket_leaves_the_project_unmodified():
    import asyncio

    from .test_projects import _exchange

    block = {'geometry': 'Part', 'x': 2.0, 'y': 0.0, 'z': 0.0, 'width_x': 1.0, 'width_y': 1.0,
             'width_z': 1.0, 'angle_x': 0.0, 'angle_y': 0.0, 'angle_z': 0.0, 'corner': 0.0,
             'name': ''}
    responses = asyncio.run(_exchange([
        ('project.new', {}),
        ('op.run', {'op': 'project.new_geometry', 'params': {'name': 'Part', 'unit': 'm'}}),
        ('op.run', {'op': 'project.add_solid_block', 'params': block, 'preview': True}),
        ('op.run', {'op': 'project.new_channel_table', 'params': {'name': 'x'}, 'preview': True}),
        ('op.run', {'op': 'project.add_solid_block', 'params': block}),
    ]))
    results = [got[-1] for got in responses]
    previewed = results[2]['result']
    assert 'project' not in previewed
    assert [view['view'] for view in previewed['views']] == ['shape3d']
    assert [view['view'] for view in results[4]['result']['views']] == ['shape3d']
    assert results[3]['error']['code'] == 'bad_request', 'an operation with no preview runs nothing'
    assert 'x' not in [o['name'] for o in results[4]['result']['project']['objects']]


def _union_in_pieces():
    """A slab with a tilted hole, which is fitted, and a bar swept on top of it: two pieces."""
    import hertzforge

    project = hertzforge.Project('p')
    name = project.new_geometry('Part', unit='in')
    block = project.add_solid_block(name, (0, 0, 0), (4, 2, 1), name='slab', unit='in')
    tilted = project.add_solid_cylinder(name, (0, 0, 0), 0.6, 3.0, angles=(30, 0, 0), unit='in')
    cut = project.combine_solids(name, 'subtract', [block, tilted], 'slab')
    bar = project.add_solid_block(name, (1.5, 0, 1.0), (1, 2, 1), name='bar', unit='in')
    return project, project.combine_solids(name, 'union', [cut, bar], 'both'), cut, bar


def test_mesh_solids_takes_a_size_per_piece(monkeypatch):
    """A union meshed in pieces takes a size per piece, typed as '3: 0.01'; a piece not
    named takes the element size. The fit itself is the core's test, stood in for here."""
    project, both, _cut, bar = _union_in_pieces()
    asked = {}
    monkeypatch.setattr(project, 'mesh_solids', lambda *_a, **kw: asked.update(kw) or {
        'nodes': 0, 'elements': 0, 'replaced': 0, 'groups': []})
    params = {'solids': str(both), 'size': 0.01, 'anyway': True}
    with pytest.raises((KeyError, TypeError)):
        run(project, 'project.mesh_solids', **params)
    assert asked['sizes'] is None
    with pytest.raises((KeyError, TypeError)):
        run(project, 'project.mesh_solids', sizes=f'{bar}: 0.005', **params)
    assert asked['sizes'] == {bar: 0.005}
    refused(project, 'project.mesh_solids', 'a piece and', sizes='3', **params)
    refused(project, 'project.mesh_solids', 'a piece and', sizes='3: 0', **params)


def test_detect_fills_each_piece_and_the_sizes_reach_the_mesh(monkeypatch):
    project, both, cut, bar = _union_in_pieces()
    asked = {}
    monkeypatch.setattr(project, 'mesh_solids', lambda *_a, **kw: asked.update(kw) or {
        'nodes': 0, 'elements': 0, 'replaced': 0, 'groups': []})
    with pytest.raises((KeyError, TypeError)):
        run(project, 'project.mesh_solids', solids=str(both), size=0.01, detect=True,
            anyway=True)
    likely = asked['sizes']
    assert set(likely) == {cut, bar} and all(size > 0 for size in likely.values())
    with pytest.raises((KeyError, TypeError)):
        run(project, 'project.mesh_solids', solids=str(both), size=0.01, sizes=f'{bar}: 0.005', anyway=True)
    assert asked['sizes'] == {bar: 0.005}


def _meshed_block(project, size=0.5, **given):
    run(project, 'project.add_solid_block', width_x=1, width_y=1, width_z=1, **given)
    run(project, 'project.mesh_solids', solids='1', size=size)
    return project['Part']


@pytest.mark.parametrize('rename', ['group', 'solid'])
def test_meshing_again_after_a_rename_replaces_the_mesh(project, rename):
    geometry = _meshed_block(project)
    assert len(geometry.elem_id) == 8
    if rename == 'group':
        run(project, 'project.set_entity', kind='groups', entity=1, field='name', value='Housing')
    else:
        run(project, 'project.edit_solid', solid=1, name='Housing')
    run(project, 'project.mesh_solids', size=0.25)
    assert len(geometry.elem_id) == 64, 'the old mesh does not stay beside the new one'
    assert len(geometry.group_id) == 1
    assert geometry.group_solids == {1: (1,)}


def test_a_group_renamed_keeps_its_name_and_material_when_meshed_again(project):
    geometry = _meshed_block(project)
    run(project, 'project.set_entity', kind='groups', entity=1, field='name', value='Housing')
    held = GroupProperties(material('6061-T6'))
    geometry.group_properties[1] = held
    run(project, 'project.mesh_solids', size=0.25)
    assert list(geometry.group_name) == ['Housing']
    assert geometry.group_properties[1] == held


def test_two_parts_of_one_name_meshed_in_turn_keep_their_own_meshes(project):
    geometry = project['Part']
    for x in (0.0, 5.0):
        run(project, 'project.add_solid_block', x=x, width_x=1, width_y=1, width_z=1, name='bar')
    run(project, 'project.mesh_solids', solids='1', size=0.5)
    run(project, 'project.mesh_solids', solids='2', size=0.5)
    assert len(geometry.elem_id) == 16
    assert geometry.node_xyz[:, 0].min() == -0.5 and geometry.node_xyz[:, 0].max() == 5.5
    assert geometry.group_solids == {1: (1,), 2: (2,)}
    run(project, 'project.mesh_solids', solids='1', size=0.25)
    assert len(geometry.elem_id) == 64 + 8, 'the other part is left alone'


def test_two_parts_of_one_name_meshed_together_are_one_group_meshed_together(project):
    geometry = project['Part']
    for x in (0.0, 1.0):
        run(project, 'project.add_solid_block', x=x, width_x=1, width_y=1, width_z=1, name='bar')
    run(project, 'project.mesh_solids', size=0.5)
    assert list(geometry.group_name) == ['bar'] and geometry.group_solids == {1: (1, 2)}
    refused(project, 'project.mesh_solids', 'also holds the mesh of bar', solids='1', size=0.25)
    refused(project, 'project.mesh_solids', 'also holds the mesh of bar', solids='1', size=0.25)
    assert len(geometry.elem_id) == 16
    run(project, 'project.mesh_solids', size=0.25)
    assert len(geometry.elem_id) == 128


def test_a_solid_deleted_leaves_its_mesh_for_a_new_solid_of_its_id(project):
    geometry = _meshed_block(project)
    run(project, 'project.delete_solids', solids='1')
    assert geometry.group_solids == {1: ()}
    new = run(project, 'project.add_solid_block', x=5.0, width_x=1, width_y=1, width_z=1)
    assert new and geometry.solids[0].id == 1, 'the id is taken again'
    run(project, 'project.mesh_solids', size=0.5)
    assert len(geometry.elem_id) == 16 and len(geometry.group_id) == 2


def test_a_mesh_stays_tied_to_its_solid_through_a_save(project, tmp_path):
    import h5py

    from hertzforge.io import native

    geometry = _meshed_block(project)
    path = tmp_path / 'part.hforge'
    native.save(geometry, path)
    assert native.load(path).group_solids == {1: (1,)}
    with h5py.File(path, 'r+') as f:
        holder = next(g for g in f.values() if 'group_solids' in g.attrs)
        del holder.attrs['group_solids']
    older = native.load(path)
    assert older.group_solids == {}, 'a file from before the tie reads back with none'
    project.add('Older', older)
    run(project, 'project.mesh_solids', geometry='Older', size=0.25)
    assert len(older.elem_id) == 64, 'the 8 meshed before are replaced'
    assert older.group_solids == {1: (1,)}, 'found by its name, and tied from then on'


def test_a_merged_or_renumbered_group_keeps_what_it_was_meshed_from(project):
    geometry = project['Part']
    for x in (0.0, 5.0):
        run(project, 'project.add_solid_block', x=x, width_x=1, width_y=1, width_z=1)
    run(project, 'project.mesh_solids', size=0.5)
    run(project, 'project.set_entity', kind='groups', entity=2, field='id', value='7')
    assert geometry.group_solids == {1: (1,), 7: (2,)}
    geometry.merge_groups([1, 7])
    assert geometry.group_solids == {1: (1, 2)}
    geometry.delete_groups([1])
    assert geometry.group_solids == {}


def test_renumbering_a_group_takes_its_properties_with_it(project):
    geometry = _meshed_block(project)
    held = GroupProperties(material('6061-T6'))
    geometry.group_properties[1] = held
    run(project, 'project.set_entity', kind='groups', entity=1, field='id', value='7')
    assert geometry.group_properties == {7: held}
    assert geometry.add_group('new', 1) == 1
    assert 1 not in geometry.group_properties, 'a later group of the old id inherits nothing'


@pytest.fixture
def undefined():
    from hertzforge.core.geometry import Geometry

    project = Project()
    project.add('Raw', Geometry(node_id=[], node_xyz=np.empty((0, 3))))
    assert not project['Raw'].units_defined
    return project


@pytest.mark.parametrize('kind', ['block', 'cylinder', 'section'])
def test_a_single_turned_solid_meshes_swept_along_its_own_axis(project, kind):
    given = {'angle_x': 30.0, 'angle_z': 20.0}
    if kind == 'block':
        block(project, width_x=1.0, width_y=0.5, width_z=0.25, **given)
    elif kind == 'cylinder':
        run(project, 'project.add_solid_cylinder', radius=0.25, length=1.0, **given)
    else:
        run(project, 'project.add_solid_section', shape='rectangular tube',
            dimensions='1 0.5 0.1', **given)
    found = project.mesh_solids('Part', [1], 0.1)
    assert found['turned'] and found['axis'] is None, 'swept along its own axis'
    assert project['Part'].num_nodes


@pytest.mark.parametrize('add', [
    {'op': 'project.add_solid_plate', 'width_x': 2.0, 'width_y': 1.0},
    {'op': 'project.add_solid_disk', 'radius': 0.5}], ids=['plate', 'disk'])
def test_move_part_moves_a_flat_part_and_gives_it_a_gizmo(project, add):
    given = dict(add)
    run(project, given.pop('op'), **given)
    (view,) = run(project, 'project.move_solids', solids='1')
    assert view['gizmo']['origin'] == [0.0, 0.0, 0.0], 'the middle of its extent'
    (view,) = run(project, 'project.move_solids', solids='1', offset_z=1.0)
    assert project['Part'].solids[0].center == (0.0, 0.0, 1.0)
    assert view['gizmo']['origin'] == [0.0, 0.0, 1.0]
    run(project, 'project.move_solids', solids='1', turn_x=90.0)
    assert project['Part'].solids[0].center == pytest.approx((0.0, 0.0, 1.0))


def test_a_flat_part_turned_reaches_as_far_as_its_edges(project):
    from hertzforge.core import solids

    run(project, 'project.add_solid_plate', width_x=2.0, width_y=1.0, angle_x=90.0)
    run(project, 'project.add_solid_disk', radius=0.5, x=5.0, angle_y=90.0)
    low, high = solids._box(project['Part'], 1)
    assert low == pytest.approx([-1.0, 0.0, -0.5]) and high == pytest.approx([1.0, 0.0, 0.5])
    low, high = solids._box(project['Part'], 2)
    assert low == pytest.approx([5.0, -0.5, -0.5]) and high == pytest.approx([5.0, 0.5, 0.5])


def test_detect_on_parts_that_mesh_as_one_says_so(project):
    for x in (0.0, 5.0):
        run(project, 'project.add_solid_block', x=x, width_x=1, width_y=1, width_z=1)
    (reading, *_rest) = run(project, 'project.mesh_solids', detect=True, size=0.5)
    detect = rows(reading)['Detect']
    assert 'mesh as one' in detect['value'] and detect['status'] == 'warn', \
        'Detect asked for and not done per piece is said'
    assert len(project['Part'].elem_id) == 16


def test_a_size_for_a_piece_that_is_not_there_is_refused(project):
    run(project, 'project.add_solid_block', width_x=1, width_y=1, width_z=1)
    refused(project, 'project.mesh_solids', 'mesh as one', sizes='1: 0.5', size=0.5)
    in_pieces, both, _cut, _bar = _union_in_pieces()
    refused(in_pieces, 'project.mesh_solids', 'no piece 99', solids=str(both), size=0.01,
            sizes='99: 0.01')


def _spheres(project):
    for x in (0.0, 5.0):
        run(project, 'project.add_solid_sphere', x=x, radius=0.5)


def test_the_fold_check_reads_each_piece_at_its_own_size(project):
    _spheres(project)
    refused(project, 'project.mesh_solids', 'at 0.3 m the fit is likely to fold', size=0.1,
            sizes='1: 0.3')
    views = run(project, 'project.mesh_solids', size=0.2, sizes='1: 0.1, 2: 0.1')
    assert [view['view'] for view in views] == ['shape3d'], \
        'both pieces are given a size they hold at'
    assert project['Part'].num_nodes


def test_sizes_detected_can_be_kept_without_asking_to_mesh_anyway(monkeypatch, project):
    _spheres(project)
    asked = {}
    monkeypatch.setattr(project, 'mesh_solids', lambda *_a, **kw: asked.update(kw) or {
        'bricks': 0, 'nodes': 0, 'axis': 0, 'added': 0, 'shared': 0, 'replaced': 0, 'groups': []})
    run(project, 'project.mesh_solids', size=0.2, detect=True)
    assert set(asked['sizes']) == {1, 2}


def test_a_piece_with_a_hole_reads_its_likely_size_with_the_hole(monkeypatch, project):
    from hertzforge.core import solids
    from hertzforge.service import ops_solids

    block(project, width_x=0.4, width_y=0.2, width_z=0.1)
    block(project, x=0.1, z=0.1, width_x=0.1, width_y=0.2, width_z=0.1)
    run(project, 'project.combine_solids', solids='1, 2', operation='union')
    run(project, 'project.add_solid_cylinder', x=-0.1, radius=0.005, length=0.3, angle_x=30.0)
    run(project, 'project.combine_solids', solids='3, 4', operation='subtract')
    geometry = project['Part']
    likely = {}
    for entry in solids.piece_plan(geometry, [5], 0.02):
        likely[entry['id']] = ops_solids._likely_of_piece(geometry, [5], entry)
        assert entry['cuts'] and likely[entry['id']] < ops_solids._likely_size(
            geometry, [entry['id']], 'hexes')
    asked = {}
    monkeypatch.setattr(project, 'mesh_solids', lambda *_a, **kw: asked.update(kw) or {
        'nodes': 0, 'elements': 0, 'replaced': 0, 'groups': []})
    with pytest.raises((KeyError, TypeError)):
        run(project, 'project.mesh_solids', solids='5', size=0.02, detect=True)
    assert asked['sizes'] == likely, 'Detect sizes each piece as it is read with its hole'


def test_an_undefined_geometry_meshes_in_the_file_s_numbers(undefined):
    from hertzforge.service import display_units

    project = undefined
    assert not project['Raw'].units_defined
    run(project, 'project.add_solid_block', geometry='Raw', width_x=100.0, width_y=100.0,
        width_z=10.0)
    views = run(project, 'project.mesh_solids', geometry='Raw', size=10.0)
    assert [view['view'] for view in views] == ['shape3d'] and views[0]['unit'] == 'undefined'
    display_units.convert(views, display_units.START)
    assert np.ptp(views[0]['nodes'][:, 0]) == pytest.approx(100.0)


def test_an_undefined_geometry_says_a_refusal_and_a_fold_without_a_unit(undefined):

    project = undefined
    for x in (0.0, 5.0):
        run(project, 'project.add_solid_sphere', geometry='Raw', x=x, radius=0.5)
    with pytest.raises(ProtocolError, match='likely to fold') as error:
        run(project, 'project.mesh_solids', geometry='Raw', size=0.3)
    assert ' m ' not in error.value.message
    run(project, 'project.mesh_solids', geometry='Raw', size=0.1, anyway=True)
    with pytest.raises(ProtocolError) as error:
        run(project, 'project.mate_surfaces', geometry='Raw', tolerance=0.001)
    assert not str(error.value).endswith(' m')


def test_meshing_again_says_the_mated_surfaces_it_deleted(project):
    for z in (0.0, 1.0):
        run(project, 'project.add_solid_block', z=z, width_x=1, width_y=1, width_z=1)
    run(project, 'project.mesh_solids', size=0.5, parts_apart=True)
    run(project, 'project.mate_surfaces', keep=True)
    assert len(project['Part'].mates) == 1
    (reading, *_rest) = run(project, 'project.mesh_solids', size=0.5, parts_apart=True)
    assert not project['Part'].mates
    said = rows(reading)['Mated surfaces']
    assert '1 on the old mesh' in said['value'] and said['status'] == 'warn'


def _mated(project, **held):
    for z in (0.0, 1.0):
        run(project, 'project.add_solid_block', z=z, width_x=1, width_y=1, width_z=1)
    run(project, 'project.mesh_solids', size=0.5, parts_apart=True)
    run(project, 'project.mate_surfaces', keep=True, **held)
    return project['Part']


def _edit(project, **given):
    """The pane on mate 1, filled as the action table fills it, with `given` changed."""
    held = {'name': 'base on lid', 'normal': 'stiffness', 'normal_stiffness': 2e6,
            'shear': 'free'}
    return run(project, 'project.mate_surfaces', mate=1, keep=True, **{**held, **given})


def test_renaming_a_mated_set_leaves_how_it_is_held(project):
    geometry = _mated(project, normal='stiffness', normal_stiffness=2e6, shear='free')
    _edit(project, name='Renamed')
    held = geometry.mates[0]
    assert (held.name, held.normal_mode, held.normal_stiffness, held.shear_mode) == (
        'Renamed', 'stiffness', 2e6, 'free')


def test_a_mated_set_changes_only_what_the_form_changes(project):
    geometry = _mated(project, normal='stiffness', normal_stiffness=2e6, shear='free')
    nodes = geometry.mates[0].nodes_a
    _edit(project, normal_stiffness=3e6)
    assert geometry.mates[0].normal_stiffness == 3e6 and geometry.mates[0].shear_mode == 'free'
    assert geometry.mates[0].nodes_a == nodes
    _edit(project, normal='rigid')
    assert geometry.mates[0].normal_mode == 'rigid' and geometry.mates[0].normal_stiffness == 0.0
    _edit(project, normal='rigid', shear='stiffness', shear_stiffness=1e5)
    assert geometry.mates[0].shear_stiffness == 1e5


def test_a_node_clicked_on_a_mate_side_is_a_node_of_that_side(project):
    geometry = _mated(project)
    refused(project, 'project.mate_surfaces', 'is not an id|is not a node id', mate=1,
            toggle='abc')
    held = geometry.mates[0]
    other = next(int(n) for n in geometry.nodes_in([held.groups[1]])
                 if int(n) not in geometry.nodes_in([held.groups[0]]))
    refused(project, 'project.mate_surfaces', 'is not on the surface', mate=1, side='a',
            toggle=str(other))
    side = geometry.nodes_in([held.groups[0]])
    far = max(side, key=lambda n: abs(geometry.node_xyz[geometry.node_index([n])[0], 2] - 1.0))
    refused(project, 'project.mate_surfaces', 'where it mates', mate=1, side='a',
            toggle=str(far))
    views = run(project, 'project.mate_surfaces', mate=1, side='a',
                toggle=str(held.nodes_a[0]))
    assert geometry.mates[0] == held, 'a click changes nothing until Apply'
    pick = next(view for view in views if view['view'] == 'shape3d')['pick']
    run(project, 'project.mate_surfaces', **{**pick['params'], 'keep': True})
    assert len(geometry.mates[0].nodes_a) == len(held.nodes_a) - 1


def test_picks_made_for_one_patch_are_dropped_for_another(project):
    _mated(project)
    project.delete_mated_surfaces('Part', [1])
    views = run(project, 'project.mate_surfaces')
    pick = next(view for view in views if view['view'] == 'shape3d')['pick']
    views = run(project, 'project.mate_surfaces',
                **{**pick['params'], 'toggle': str(pick['ids'][0])})
    after = next(view for view in views if view['view'] == 'shape3d')['pick']['params']
    moved = {**after, 'groups': '2, 1'}
    views = run(project, 'project.mate_surfaces', **moved)
    shown = next(view for view in views if view['view'] == 'shape3d')['pick']
    assert shown['off'] == pick['off'], 'the groups changed: the picks went with them'


def test_whole_numbers_are_whole(project):
    _mated(project)
    run(project, 'project.pattern_solids', solids='1', count_x=2, spacing_x=3.0)
    pattern = project['Part'].solids[-1].id
    refused(project, 'project.edit_solid', 'not a whole count', solid=pattern, counts='2.9,1,1')
    refused(project, 'project.mate_surfaces', 'there is no patch 2', patch=2)


def test_two_groups_a_mate_joins_are_not_merged(project):
    from hertzforge.service.ops_model import OPERATIONS as MODEL

    geometry = _mated(project)
    (merge,) = [op for op in MODEL if op.id == 'project.merge_element_groups']
    with pytest.raises(ProtocolError, match='mated surfaces .* join them'):
        merge.run(project, {'geometry': 'Part', 'groups': '1, 2'})
    assert len(geometry.group_id) == 2 and geometry.mates[0].groups == (1, 2)
    project.delete_mated_surfaces('Part', [1])
    merge.run(project, {'geometry': 'Part', 'groups': '1, 2'})
    assert len(geometry.group_id) == 1


@pytest.mark.parametrize('op', ['project.merge_coincident_nodes', 'project.mate_surfaces'])
def test_a_geometry_with_no_nodes_is_refused_in_words_and_the_row_is_off(op):
    from hertzforge.service.ops_model import OPERATIONS as MODEL

    state = projects.new({'title': 'x'})
    project = state.project
    project.new_geometry('Part', unit='m')
    run(project, 'project.add_solid_block')
    groups = {'name': 'Part', 'kind': 'group', 'id': 1, 'ids': [1, 2]}
    table = projects.actions(state, {'objects': ['Part'], 'part': groups})['table']
    row = next(row for row in table['columns'][0]['rows'] if row['id'] == op)
    assert not row['ready'] and 'no nodes' in row['reason']
    by_id = {o.id: o for o in (*OPERATIONS, *MODEL)}
    values = by_id[op].resolve({}, objects={'Part': 'geometry'})
    with pytest.raises(ProtocolError, match='has no nodes') as error:
        by_id[op].run(project, values)
    assert error.value.code == 'refused'


def test_picking_an_element_group_fills_merge_and_delete():
    state = projects.new({'title': 'x'})
    project = state.project
    project.new_geometry('Part', unit='m')
    run(project, 'project.add_solid_block')
    run(project, 'project.mesh_solids', size=0.5)
    table = projects.actions(state, {'objects': ['Part'], 'part': {'kind': 'group', 'id': 1}})
    own, together = table['table']['columns']
    assert {row['id']: row for row in own['rows']}['project.delete_element_groups']['fill'] == {
        'geometry': 'Part', 'groups': '1'}
    merge = {row['id']: row for row in together['rows']}['project.merge_element_groups']
    assert together['parts'] == ['group', 'group']
    assert merge['candidates'] == [] and 'no other group' in merge['reason']


def test_looking_at_a_view_first_operation_leaves_the_project_unmodified(tmp_path):
    import asyncio

    from .test_projects import _exchange

    block = {'geometry': 'Part', 'width_x': 1.0, 'width_y': 1.0, 'width_z': 1.0}
    looks = [
        ('project.merge_coincident_nodes', {'geometry': 'Part'}),
        ('project.align_meshes', {'geometry': 'Part'}),
        ('project.tie_elements', {'geometry': 'Part'}),
        ('project.tie_elements', {'geometry': 'Part', 'add': '1'}),
        ('project.mate_surfaces', {'geometry': 'Part', 'mate': 1}),
        ('project.move_solids', {'geometry': 'Part', 'solids': '1'})]
    acts = [
        ('project.move_solids', {'geometry': 'Part', 'solids': '1', 'offset_x': 0.5}),
        ('project.mate_surfaces', {'geometry': 'Part', 'mate': 1, 'name': 'seat',
                                   'keep': True}),
        ('project.mesh_solids', {'geometry': 'Part', 'size': 0.5, 'parts_apart': True}),
        ('project.merge_coincident_nodes', {'geometry': 'Part', 'keep': True})]
    target = str(tmp_path / 'p.hforge')
    steps = [
        ('project.new', {}),
        ('op.run', {'op': 'project.new_geometry', 'params': {'name': 'Part', 'unit': 'm'}}),
        ('op.run', {'op': 'project.add_solid_block', 'params': block}),
        ('op.run', {'op': 'project.add_solid_block', 'params': {**block, 'z': 1.0}}),
        ('op.run', {'op': 'project.mesh_solids', 'params': {
            'geometry': 'Part', 'size': 0.5, 'parts_apart': True}}),
        ('op.run', {'op': 'project.mate_surfaces', 'params': {'geometry': 'Part', 'keep': True}})]
    for op, params in looks:
        steps += [('project.save', {'path': target}),
                  ('op.run', {'op': op, 'params': params})]
    for op, params in acts:
        steps += [('project.save', {'path': target}),
                  ('op.run', {'op': op, 'params': params})]
    answers = [got[-1] for got in asyncio.run(_exchange(steps))]
    start = 6
    for k, (op, params) in enumerate(looks):
        answer = answers[start + 2 * k + 1]
        assert 'error' not in answer, (op, answer)
        assert not answer['result']['project']['modified'], (op, params)
    start += 2 * len(looks)
    for k, (op, params) in enumerate(acts):
        answer = answers[start + 2 * k + 1]
        assert 'error' not in answer, (op, answer)
        assert answer['result']['project']['modified'], (op, params)


def test_an_edit_and_its_preview_answer_with_the_drawing_alone():
    """No box saying Changed (or Would change) over the solid being edited (Brandon, 2026-11-13)."""
    import asyncio

    from .test_projects import _exchange

    responses = asyncio.run(_exchange([
        ('project.new', {}),
        ('op.run', {'op': 'project.new_geometry', 'params': {'name': 'Part', 'unit': 'm'}}),
        ('op.run', {'op': 'project.add_solid_block', 'params': {'geometry': 'Part'}}),
        ('op.run', {'op': 'project.edit_solid', 'params': {
            'geometry': 'Part', 'solid': 1, 'radius': '', 'widths': '2 2 2'}, 'preview': True}),
        ('op.run', {'op': 'project.edit_solid', 'params': {
            'geometry': 'Part', 'solid': 1, 'widths': '2 2 2'}})]))
    for got in responses[3:]:
        assert [view['view'] for view in got[-1]['result']['views']] == ['shape3d']


def test_a_part_taken_apart_and_remade_under_its_name_replaces_its_mesh(project):
    geometry = project['Part']
    run(project, 'project.add_solid_block', width_x=1, width_y=1, width_z=1, name='raw')
    run(project, 'project.add_solid_cylinder', radius=0.1, length=2.0, name='bore')
    run(project, 'project.combine_solids', solids='1, 2', operation='subtract', name='plate')
    run(project, 'project.mesh_solids', solids='3', size=0.25)
    before = len(geometry.elem_id)
    run(project, 'project.separate_solids', solids='3')
    assert geometry.group_solids == {}
    run(project, 'project.combine_solids', solids='1, 2', operation='subtract', name='plate')
    run(project, 'project.mesh_solids', solids='3', size=0.25)
    assert len(geometry.elem_id) == before, 'the old mesh is replaced, not kept beside it'
    assert len(geometry.group_id) == 1


SHAPES_ADDED = ('project.add_solid_block', 'project.add_solid_cylinder', 'project.add_solid_cone',
                'project.add_solid_sphere', 'project.add_solid_plate', 'project.add_solid_disk',
                'project.add_solid_section')


def _edit_row(state, solid_id, system):
    from hertzforge.service import display_units

    table = display_units.running_in(system, projects.actions, state, {
        'objects': ['Part'], 'part': {'kind': 'solid', 'id': solid_id}})
    return next(row for row in table['table']['columns'][0]['rows'] if row['verb'] == 'Edit')


def _run_typed(project, op_id, fill, system):
    """An Edit form's values run as the server runs them: typed in `system`."""
    from hertzforge.service import display_units

    op = next(o for o in OPERATIONS if o.id == op_id)
    kinds = {p.name: p.type for p in op.params}
    # as the shell reads a row's fill into its form
    fill = {key: int(text) if kinds.get(key) == 'integer' else float(text)
            if kinds.get(key) == 'number' else text for key, text in fill.items()}
    values = op.resolve(fill, objects={'Part': 'geometry'})
    values = display_units.typed_lengths(op.params, values, system, project['Part'])
    return op.run(project, values)


@pytest.mark.parametrize('op_id', SHAPES_ADDED)
def test_a_solid_picked_is_edited_in_its_own_add_form(op_id):
    from hertzforge.service.display_units import running_in

    state = projects.new({'title': 'x'})
    project = state.project
    project.new_geometry('Part', unit='m')
    views = run(project, op_id)
    item = project['Part'].solids[-1]
    (view,) = views
    assert 'gizmo' in view and 'bodies' in view, 'the new solid lit, the gimbal on it'
    before = item.as_record()
    edit = _edit_row(state, item.id, 'in-slinch-lbf-s (g)')
    assert edit['id'] == op_id and edit['live'] and edit['fill']['solid'] == str(item.id)
    # its own values, run as typed in inches, leave it as it was
    running_in('in-slinch-lbf-s (g)', _run_typed, project, op_id, edit['fill'], 'in-slinch-lbf-s (g)')
    after = project['Part'].solids[-1].as_record()
    for key, value in before.items():
        assert after[key] == (pytest.approx(value, abs=1e-9) if key not in ('name', 'kind') else value), key
    moved = {**edit['fill'], 'x': '2'}
    views = _run_typed(project, op_id, moved, 'in-slinch-lbf-s (g)')
    assert project['Part'].solids[-1].center[0] == pytest.approx(2 * 0.0254)
    assert len(project['Part'].solids) == 1, 'changed, not added'
    assert 'gizmo' in views[0]


def test_the_solids_row_adds_at_once_and_a_wrong_kind_is_refused():
    state = projects.new({'title': 'x'})
    project = state.project
    project.new_geometry('Part', unit='m')
    rows = projects.actions(state, {'objects': ['Part'], 'part': {'kind': 'solids', 'id': 0}})
    rows = rows['table']['columns'][0]['rows']
    assert all(row.get('instant') for row in rows if row['id'] in SHAPES_ADDED)
    run(project, 'project.add_solid_block')
    with pytest.raises(ProtocolError, match='is a block, not a sphere'):
        run(project, 'project.add_solid_sphere', solid=1)


def test_a_new_geometry_asks_for_nothing_and_takes_the_display_length():
    from hertzforge.service.display_units import running_in

    state = projects.new({'title': 'x'})
    rows = projects.actions(state, {'objects': []})['table']['columns'][0]['rows']
    for made in ('project.new_geometry', 'project.new_channel_table'):
        assert next(row for row in rows if row['id'] == made)['instant'], made
    op = OPS['project.new_geometry']
    running_in('in-slinch-lbf-s (g)', op.run, state.project, op.resolve({}, objects={}))
    running_in('mm-kg-N-s', op.run, state.project, op.resolve({}, objects={}))
    assert [state.project[n].length_unit for n in ('Geometry', 'Geometry (2)')] == ['in', 'mm']


def test_a_pattern_is_previewed_as_it_is_typed():
    import asyncio

    from .test_projects import _exchange

    out = asyncio.run(_exchange([
        ('op.run', {'op': 'project.new_geometry', 'params': {}}),
        ('op.run', {'op': 'project.add_solid_block', 'params': {'geometry': 'Geometry'}}),
        ('op.run', {'op': 'project.pattern_solids', 'preview': True, 'params': {
            'geometry': 'Geometry', 'solids': '1', 'count_x': 3, 'spacing_x': 2.0}}),
        ('project.children', {'name': 'Geometry'}),
    ]))
    preview = out[2][-1]['result']
    assert [view['view'] for view in preview['views']] == ['shape3d'], 'nothing said'
    drawn = next(view for view in preview['views'] if view['view'] == 'shape3d')
    assert 'bodies' in drawn, 'the pattern drawn lit'
    # nothing reached the project: its one block stands alone
    assert [solid['id'] for solid in out[3][-1]['result']['solids']] == [1]
