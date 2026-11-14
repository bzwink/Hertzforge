"""Editing a geometry and what is drawn on it, and the project's own
structure: the entity tables, the coordinate system gizmo,
the DOF arrows and sensors of the 3-D view,
Work Up, channel roles and DOF names, a channel table's
control loops, and the object groups.

A geometry's nodes, coordinate systems, elements and element groups are
added, set and deleted through the geometry's own methods
(`core.entities.VERBS`), and elements and element groups are deleted
through the project's, which keep the nodes data is measured at. Each
edit returns the geometry drawn and the table of that kind of entity,
the columns of the entity tables. A `shape3d` has no
markers, so a coordinate system's axes and a DOF arrow are drawn as
extra nodes and lines of the mesh; a sensor's body is a solid of its own
(`bodies`).
"""

from __future__ import annotations

import math
from typing import Any

import numpy as np

from . import group_cells
from . import views as v
from .descriptors import Operation, Option, Param, Workspace
from .mesh import drawn_mesh
from .ops_files import SAVED_KINDS
from .ops_project import DATA_KINDS
from .parsing import parse_id_runs
from .protocol import refused

ANY = 'objects:' + '|'.join(SAVED_KINDS)

GEOMETRY = Param('geometry', 'Geometry', 'enum', options_from='objects:geometry')
KIND = Param('kind', 'Entities', 'enum', default='nodes', options=(
    Option('nodes', 'Nodes'), Option('coordinate_systems', 'Coordinate systems'),
    Option('elements', 'Elements'), Option('groups', 'Element groups')))

LABELS = {'nodes': 'node', 'coordinate_systems': 'coordinate system',
          'elements': 'element', 'groups': 'element group'}
ID_ARRAYS = {'nodes': 'node_id', 'coordinate_systems': 'cs_id', 'elements': 'elem_id',
             'groups': 'group_id'}


# ---- drawing ------------------------------------------------------------------------

def _extent(points: np.ndarray) -> float:
    """How far the farthest point is from their center: the size an
    arrow or an axis is drawn against."""
    if not len(points):
        return 1.0
    return float(np.sqrt(((points - points.mean(axis=0)) ** 2).sum(axis=1).max())) or 1.0


def _drawn(title: str, geometry: Any, extra_nodes: Any = None,
           extra_lines: Any = None) -> dict:
    """The geometry with every node, its faces and lines, and `extra_nodes`
    joined by `extra_lines` (indices into `extra_nodes`): the axes, arrows
    or bodies drawn on it, which a `shape3d` has no markers for."""
    used, triangles, lines = drawn_mesh(geometry)
    xyz = np.asarray(geometry.node_xyz, dtype=float).reshape(-1, 3)
    nodes = xyz
    lines = used[lines].reshape(-1, 2)
    triangles = used[triangles].reshape(-1, 3)
    if extra_nodes is not None and len(extra_nodes):
        nodes = np.vstack([xyz, np.asarray(extra_nodes, dtype=float).reshape(-1, 3)])
        lines = np.vstack([lines, np.asarray(extra_lines, dtype=np.int64).reshape(-1, 2)
                           + len(xyz)])
    # the solids not meshed yet, after every node so a node's row stays its own: the model
    # looks the same whatever part of it is picked (Brandon, 2026-11-10)
    soup = _solid_surfaces(geometry)
    if len(soup):
        triangles = np.vstack([triangles, len(nodes) + np.arange(len(soup)).reshape(-1, 3)])
        nodes = np.vstack([nodes, soup])
    return v.shape3d(title, 'm' if geometry.units_defined else 'undefined', nodes, [],
                     triangles=triangles, lines=lines)


def _solid_surfaces(geometry: Any) -> np.ndarray:
    """The corners of the triangles of every standing solid's surface, three to a triangle;
    a solid the kernel cannot make is left out."""
    from hertzforge.core import solids

    soups = []
    for item in solids.top_level(geometry):
        try:
            soups.append(solids.shape_triangles(solids.shape(geometry, item.id)).reshape(-1, 3))
        except ValueError:
            continue
    return np.concatenate(soups) if soups else np.empty((0, 3))


def frames(geometry: Any, drawn: Any = None) -> list[dict[str, Any]]:
    """Each coordinate system's origin and three axes, and how long to draw them, as a
    `shape3d`'s `frames`: the view draws them as the gimbal is drawn, X red, Y green and
    Z blue arrows (Brandon, 2026-11-10). Their length is a share of the model's size, the
    points `drawn` (solids too) when given, else its nodes."""
    matrices = np.asarray(geometry.cs_matrix, dtype=float).reshape(-1, 4, 3)
    model = np.asarray(geometry.node_xyz if drawn is None else drawn, dtype=float).reshape(-1, 3)
    points = np.vstack([model, matrices[:, 3]])
    length = 0.25 * _extent(points)
    return [{'origin': [float(c) for c in matrix[3]],
             'axes': [[float(c) for c in matrix[axis]] for axis in range(3)],
             'length': float(length)} for matrix in matrices]


def _arrow(start: np.ndarray, end: np.ndarray, nodes: list, lines: list) -> None:
    """A shaft from `start` to `end` and a head of two barbs at `end`."""
    along = end - start
    length = float(np.linalg.norm(along))
    if not length > 0:
        return
    unit = along / length
    side = np.cross(unit, (0.0, 0.0, 1.0) if abs(unit[2]) < 0.9 else (1.0, 0.0, 0.0))
    side /= np.linalg.norm(side)
    base = len(nodes)
    nodes += [start, end, end - 0.25 * length * unit + 0.12 * length * side,
              end - 0.25 * length * unit - 0.12 * length * side]
    lines += [(base, base + 1), (base + 1, base + 2), (base + 1, base + 3)]


# ---- tables -------------------------------------------------------------------------

def id_runs_text(ids: Any) -> str:
    """Ids as sorted runs without repeats, `[1,2,3,7,9,10]` as
    '1-3 7 9-10'."""
    ids = np.unique(np.asarray(list(ids), dtype=np.int64))
    if not len(ids):
        return ''
    runs, start, previous = [], int(ids[0]), int(ids[0])
    for value in (int(i) for i in ids[1:]):
        if value == previous + 1:
            previous = value
            continue
        runs.append((start, previous))
        start = previous = value
    runs.append((start, previous))
    return ' '.join(str(a) if a == z else f'{a}-{z}' for a, z in runs)


def _group_label(geometry: Any, row: int) -> str:
    """An element group's name, or 'element group N' when it has none."""
    name = geometry.group_name[row].strip()
    return name or f'element group {int(geometry.group_id[row])}'


def _number(value: Any) -> str:
    return f'{float(value):.6g}'


#: past this many rows an entity table sends its first rows and its count, and the shell
#: asks for the rest as they scroll into view (`project.entity_rows`): a million elements
#: whole took 2 s to write and 80 MB to send (2026-11-10)
PAGE_ROWS = 1000


def picked_rows(geometry: Any, kind: str, family: str = '', groups: str = '') -> np.ndarray | None:
    """The rows, by index, of the part an entity table is about: a family's elements, or
    element groups' elements or the groups themselves. None for every row."""
    from hertzforge.core.geometry import element_family

    if kind == 'elements' and family:
        codes = np.asarray(geometry.elem_type, dtype=int)
        kept = [c for c in np.unique(codes) if element_family(int(c)) == family]
        return np.flatnonzero(np.isin(codes, kept))
    if kind in ('elements', 'groups') and groups:
        wanted = parse_id_runs(groups)
        held = geometry.elem_group if kind == 'elements' else geometry.group_id
        return np.flatnonzero(np.isin(np.asarray(held, dtype=int), wanted))
    return None


def entity_columns(geometry: Any, kind: str) -> list[str]:
    """An entity table's column titles, lengths in the geometry's stored unit."""
    unit = 'm' if geometry.units_defined else 'units undefined'
    if kind == 'nodes':
        return ['Node', f'X [{unit}]', f'Y [{unit}]', f'Z [{unit}]', 'Placement CS',
                'Displacement CS']
    if kind == 'coordinate_systems':
        return ['CS', 'Name', 'Type', *(f'Origin {a} [{unit}]' for a in 'XYZ'),
                *(f'Rotation {a} [°]' for a in 'XYZ')]
    if kind == 'elements':
        return ['Element', 'Type', 'Element group', 'Nodes']
    return ['Element group', 'Name', 'Elements', 'Elements are', 'Count',
            *group_cells.titles(geometry)]


def entity_rows(geometry: Any, kind: str, indices: Any) -> list[list[str]]:
    """The cells of the entities at `indices`, and of no others: a page of a long table
    is written without writing the rest."""
    from hertzforge.core.geometry import CS_TYPES, ELEMENT_TYPES
    from hertzforge.rotate import angles_of

    indices = [int(k) for k in indices]
    if kind == 'nodes':
        # in each node's placement system, as the column beside them says, an angle in
        # degrees (Brandon, 2026-11-11)
        xyz = np.zeros((0, 3)) if not len(indices) else geometry.node_local_xyz(indices)
        return [[str(int(geometry.node_id[k])), *(_number(c) for c in xyz[n]),
                 str(int(geometry.node_def_cs[k])), str(int(geometry.node_disp_cs[k]))]
                for n, k in enumerate(indices)]
    if kind == 'coordinate_systems':
        matrices = np.asarray(geometry.cs_matrix).reshape(-1, 4, 3)
        return [[str(int(geometry.cs_id[k])), geometry.cs_name[k],
                 CS_TYPES.get(int(geometry.cs_type[k]), 'cartesian'),
                 *(_number(c) for c in matrices[k][3]),
                 *(_number(a) for a in angles_of(matrices[k]))] for k in indices]
    if kind == 'elements':
        labels = {int(g): _group_label(geometry, j) for j, g in enumerate(geometry.group_id)}
        return [[str(int(geometry.elem_id[k])),
                 ELEMENT_TYPES.get(int(geometry.elem_type[k]), ('unknown',))[0],
                 labels.get(int(geometry.elem_group[k]), ''),
                 ' '.join(str(int(n)) for n in geometry.elem_conn[k])] for k in indices]
    elem_group = np.asarray(geometry.elem_group)
    rows = []
    for k in indices:
        group = int(geometry.group_id[k])
        members = np.asarray(geometry.elem_id)[elem_group == group]
        rows.append([str(group), geometry.group_name[k], id_runs_text(members),
                     group_cells.family(geometry, group) or 'empty', str(len(members)),
                     *group_cells.cells(geometry, group)])
    return rows


def entity_table(geometry: Any, kind: str, title: str, name: str | None = None,
                 family: str = '', groups: str = '') -> dict:
    """One kind of a geometry's entities, lengths in the
    geometry's stored unit: meters once its units are declared. With the
    geometry's `name`, its cells are typed into in place
    (`project.set_entity_cell`), a column per field of `FIELDS`, Delete
    removes its rows (`project.delete_entities`), and past `PAGE_ROWS` rows
    it sends its first and its count, the rest asked for as they are
    scrolled to. `family` or `groups` keeps a part's rows (`picked_rows`),
    and its edits and pages say the same part, so a row is the part's."""
    picked = picked_rows(geometry, kind, family, groups)
    count = len(getattr(geometry, ID_ARRAYS[kind])) if picked is None else len(picked)
    sent = min(count, PAGE_ROWS) if name is not None else count
    indices = np.arange(sent) if picked is None else picked[:sent]
    rows = entity_rows(geometry, kind, indices)
    part = {key: value for key, value in (('family', family), ('groups', groups)) if value}
    edit = None if name is None else v.editing(
        'project.set_entity_cell', {'geometry': name, 'kind': kind, **part}, FIELDS[kind],
        group_cells.choices() if kind == 'groups' else None)
    if edit is not None:
        # the rows are the entities, so Delete takes rows, not cells: each by its id,
        # the first column
        edit['remove'] = {'op': 'project.delete_entities',
                          'params': {'geometry': name, 'kind': kind}, 'column': 0}
    table = v.table(title, entity_columns(geometry, kind), rows, edit)
    if name is not None and count > sent:
        table['total'] = count
        table['more'] = {'method': 'project.entity_rows',
                         'params': {'geometry': name, 'kind': kind, **part}}
    return table


def _pick(name: str, geometry: Any, kind: str, element: dict[str, Any] | None) -> dict | None:
    """What a click in the view adds: a
    node or a coordinate system where the click lands, or an element on the
    nodes clicked, `element` saying its family, type and group (Add
    Element's own params, or its defaults)."""
    ids = [int(n) for n in geometry.node_id]
    if kind == 'nodes':
        return v.picking('project.add_node', {'geometry': name}, 'point',
                         'Click in the view to place a node', ids=ids)
    if kind == 'coordinate_systems':
        return v.picking('project.add_coordinate_system', {'geometry': name}, 'point',
                         'Click in the view to place a coordinate system', ids=ids)
    if kind != 'elements':
        return None
    from hertzforge.core.geometry import ELEMENT_TYPES

    element = element or {'family': FAMILY.default, 'type': '', 'group': 0}
    family = element['family']
    code, count = FAMILY_ADD_TYPES[family]
    chained = ELEMENT_TYPES[code][2] == 'line'
    hint = (f'Click nodes in order, then press Enter for the line of {family}' if chained
            else f'Click {count} node{"s" * (count != 1)} to make one of the {family}')
    return v.picking('project.add_element', {'geometry': name, **element}, 'nodes', hint,
                     ids=ids, count=0 if chained else count,
                     param='nodes')


def _shown(name: str, geometry: Any, kind: str, said: str = '',
           element: dict[str, Any] | None = None, **part: str) -> list[dict]:
    """The geometry drawn, with its coordinate systems' axes when they are
    the entities in hand and what a click in it adds (`_pick`), and the
    table of `kind` (of a part's rows, when `family` or `groups` names one)."""
    words = {'nodes': 'Nodes', 'coordinate_systems': 'Coordinate systems',
             'elements': 'Elements', 'groups': 'Element groups'}[kind]
    title = f'{words} of {name}' + (f': {said}' if said else '')
    drawn = _drawn(name, geometry)
    if kind == 'coordinate_systems':
        drawn['frames'] = frames(geometry, drawn['nodes'])
    pick = _pick(name, geometry, kind, element)
    if pick is not None:
        drawn['pick'] = pick
    return [drawn, entity_table(geometry, kind, title, name, **part)]


def part_entities(name: str, geometry: Any, part: dict[str, Any]) -> list[dict] | None:
    """What a part of a geometry picked in the tree shows: the geometry drawn, with the
    coordinate systems' axes when they are the part, and no table: a part's table is its
    Edit verb (`view_entities`), not its plot (Brandon, 2026-11-13). None for a part
    drawn another way (solids, mated surfaces)."""
    from .verbs import part_key

    where = part_key(part)
    if where not in ('part:nodes', 'part:coordinate_systems', 'family', 'group'):
        return None
    drawn = _drawn(name, geometry)
    if where == 'part:coordinate_systems':
        drawn['frames'] = frames(geometry, drawn['nodes'])
    return [drawn]


# ---- View Entities and the edits ----------------------------------------------------

def view_entities(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.view_entities`, a part's Edit: its entities as a table beside the geometry
    drawn, cells typed over and rows deleted in place, and a click in the view adding one
    (Brandon, 2026-11-13: the table is editing's, not the plot's). The nodes or coordinate
    systems; a family's elements (`family`); element groups and their elements, with
    their connections (`groups`)."""
    from hertzforge.core.geometry import FAMILY_LABELS

    name = params['geometry']
    geometry = project[name]
    family, runs = params['family'].strip(), params['groups'].strip()
    if runs:
        groups = parse_id_runs(runs)
        runs = id_runs_text(groups)
        said = ('element group ' if len(groups) == 1 else 'element groups ') + runs
        drawn, elements = _shown(name, geometry, 'elements', said, groups=runs)
        return [drawn, entity_table(geometry, 'groups', f'Element groups of {name}: ' + runs,
                                    name, groups=runs), elements]
    if family:
        element = {'family': family, 'type': '', 'group': 0} if family in FAMILY_ADD_TYPES else None
        return _shown(name, geometry, 'elements', FAMILY_LABELS.get(family, family).lower(),
                      element, family=family)
    return _shown(name, geometry, params['kind'])


def _optional_id(value: int) -> int | None:
    return int(value) or None


def _system(geometry: Any, cs: int) -> int:
    """`cs` when the geometry has it, its first system for 0."""
    if not cs:
        return int(geometry.cs_id[0]) if len(geometry.cs_id) else 1
    if cs not in {int(c) for c in geometry.cs_id}:
        raise refused(f'no coordinate system {cs}; the geometry has '
                      + ', '.join(str(int(c)) for c in geometry.cs_id))
    return cs


X = Param('x', 'X', 'number', default=0.0, unit='m', step=0.001)
Y = Param('y', 'Y', 'number', default=0.0, unit='m', step=0.001)
Z = Param('z', 'Z', 'number', default=0.0, unit='m', step=0.001)
NODE_ID = Param('id', 'Id (0: one more than the highest)', 'integer', default=0, min=0,
                step=1)
PLACEMENT_CS = Param('placement_cs', 'Placement CS (0: the first)', 'integer', default=0,
                     min=0, step=1)
DISPLACEMENT_CS = Param('displacement_cs', 'Displacement CS (0: the first)', 'integer',
                        default=0, min=0, step=1)


def add_node(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_node`: a node at typed coordinates
    (`Geometry.add_node`). The coordinates are the geometry's stored length unit,
    meters once its units are declared."""
    name = params['geometry']
    geometry = project[name]
    placed = _system(geometry, params['placement_cs'])
    moved = _system(geometry, params['displacement_cs'])
    try:
        node = geometry.add_node((params['x'], params['y'], params['z']),
                                 node_id=_optional_id(params['id']), def_cs=placed,
                                 disp_cs=moved)
    except ValueError as error:
        raise refused(str(error)) from None
    project.record_call(name, 'add_node', (params['x'], params['y'], params['z']),
                        node_id=int(node), def_cs=placed, disp_cs=moved)
    return _shown(name, geometry, 'nodes', f'added node {node}')


RX = Param('rx', 'Rotation about X', 'number', default=0.0, unit='°', step=1.0)
RY = Param('ry', 'Rotation about Y', 'number', default=0.0, unit='°', step=1.0)
RZ = Param('rz', 'Rotation about Z', 'number', default=0.0, unit='°', step=1.0)
CS_NAME = Param('name', 'Name', 'string', default='')
CS_TYPE = Param('type', 'Type', 'enum', default='cartesian', options=(
    Option('cartesian', 'Cartesian'), Option('cylindrical', 'Cylindrical'),
    Option('spherical', 'Spherical')))


def add_coordinate_system(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_coordinate_system`: a coordinate system at a typed
    origin, turned by angles about the fixed X, then Y, then Z axes, the
    convention of the table's Rotation columns
    (`rotate.frame_from_angles`, `Geometry.add_coordinate_system`)."""
    from hertzforge.core.geometry import CS_TYPES
    from hertzforge.rotate import frame_from_angles

    name = params['geometry']
    geometry = project[name]
    code = next(c for c, word in CS_TYPES.items() if word == params['type'])
    frame = frame_from_angles((params['rx'], params['ry'], params['rz']))
    try:
        cs = geometry.add_coordinate_system(
            origin=(params['x'], params['y'], params['z']), rotation=frame[:3],
            cs_id=_optional_id(params['id']), name=params['name'], cs_type=code)
    except ValueError as error:
        raise refused(str(error)) from None
    project.record_call(name, 'add_coordinate_system',
                        origin=(params['x'], params['y'], params['z']),
                        rotation=np.asarray(frame[:3]).tolist(), cs_id=int(cs),
                        name=params['name'], cs_type=code)
    return _shown(name, geometry, 'coordinate_systems', f'added coordinate system {cs}')


#: what an element added to a family is: (type code, node count), Visual
#: Dynamics' `FAMILY_ADD_TYPES`
FAMILY_ADD_TYPES = {'beams': (21, 2), 'triangles': (41, 3), 'quads': (44, 4),
                    'tetras': (111, 4), 'wedges': (112, 6), 'hexes': (115, 8),
                    'pyramids': (201, 5), 'points': (161, 1)}
FAMILY = Param('family', 'Family', 'enum', default='quads', options=tuple(
    Option(family, family.capitalize()) for family in FAMILY_ADD_TYPES))
ELEMENT_TYPE = Param('type', 'Type (blank: the family\'s own)', 'string', default='')
NODES = Param('nodes', 'Nodes, in order', 'string', default='')
GROUP = Param('group', 'Element group (0: the family\'s first, or a new one)', 'integer',
              default=0, min=0, step=1)


def _element_code(family: str, text: str, count: int) -> int | None:
    """The type the element is made as: the family's own when its node
    count fits, or the type named, which must be of the family and take
    that many nodes; None lets the core fit the count."""
    from hertzforge.core.geometry import ELEMENT_TYPES, element_family

    if not text.strip():
        code, takes = FAMILY_ADD_TYPES[family]
        return code if count == takes or ELEMENT_TYPES[code][2] == 'line' else None
    wanted = text.strip().lower()
    code = next((c for c, (word, _n, _r) in ELEMENT_TYPES.items() if word == wanted), None)
    if code is None:
        raise refused(f'{text!r} is not an element type; the {family} are '
                      + ', '.join(word for c, (word, _n, _r) in ELEMENT_TYPES.items()
                                  if element_family(c) == family))
    if element_family(code) != family:
        raise refused(f'a {wanted} is one of the {element_family(code)}, not the {family}')
    takes = ELEMENT_TYPES[code][1]
    if ELEMENT_TYPES[code][2] != 'line' and count != takes:
        raise refused(f'a {wanted} joins {takes} nodes; {count} are named')
    return code


def add_element(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_element`: an element of a family on named nodes
    (`Geometry.add_element`), or for a family of lines the chain of
    elements through every node named (`Geometry.add_beams`)."""
    from hertzforge.core.geometry import ELEMENT_TYPES

    name = params['geometry']
    geometry = project[name]
    nodes = parse_id_runs(params['nodes'])
    family = params['family']
    least = 1 if FAMILY_ADD_TYPES[family][1] == 1 else 2
    if len(nodes) < least:
        raise refused(f'name the nodes the element joins, in order: {least} at least')
    code = _element_code(family, params['type'], len(nodes))
    group = params['group'] or None
    try:
        if ELEMENT_TYPES[code or FAMILY_ADD_TYPES[family][0]][2] == 'line':
            held = geometry.add_beams(nodes, group=group,
                                      elem_type=code or FAMILY_ADD_TYPES[family][0])
            project.record_call(name, 'add_beams', nodes, group=group,
                                elem_type=code or FAMILY_ADD_TYPES[family][0])
            said = (f'added {len(nodes) - 1} element{"s" * (len(nodes) != 2)} through '
                    f'{len(nodes)} nodes in element group {held}')
        else:
            row = geometry.add_element(nodes, elem_type=code, group=group)
            project.record_call(name, 'add_element', nodes, elem_type=code, group=group)
            said = (f'added {ELEMENT_TYPES[int(geometry.elem_type[row])][0]} element '
                    f'{int(geometry.elem_id[row])}')
    except ValueError as error:
        raise refused(str(error)) from None
    return _shown(name, geometry, 'elements', said,
                  {key: params[key] for key in ('family', 'type', 'group')})


GROUP_NAME = Param('name', 'Name', 'string', default='')


def add_element_group(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_element_group`: an empty element group
    (`Geometry.add_group`), added outright since
    an element group is not placed in space."""
    name = params['geometry']
    geometry = project[name]
    try:
        group = geometry.add_group(params['name'], _optional_id(params['id']))
    except ValueError as error:
        raise refused(str(error)) from None
    project.record_call(name, 'add_group', params['name'], int(group))
    return _shown(name, geometry, 'groups', f'added element group {group}')


ENTITY = Param('entity', 'Id', 'integer', default=0, min=0, step=1)
FIELDS = {
    'nodes': ('id', 'x', 'y', 'z', 'placement_cs', 'displacement_cs'),
    'coordinate_systems': ('id', 'name', 'type', 'origin_x', 'origin_y', 'origin_z',
                           'rotation_x', 'rotation_y', 'rotation_z'),
    'elements': ('id', 'type', 'group', 'nodes'),
    'groups': ('id', 'name', 'elements', None, None, *group_cells.KEYS),
}
FIELD = Param('field', 'Field', 'enum', default='id', options=tuple(
    Option(key, label) for key, label in (
        ('id', 'Id'), ('x', 'X'), ('y', 'Y'), ('z', 'Z'),
        ('placement_cs', 'Placement CS'), ('displacement_cs', 'Displacement CS'),
        ('name', 'Name'), ('type', 'Type'), ('origin_x', 'Origin X'),
        ('origin_y', 'Origin Y'), ('origin_z', 'Origin Z'), ('rotation_x', 'Rotation X (°)'),
        ('rotation_y', 'Rotation Y (°)'), ('rotation_z', 'Rotation Z (°)'),
        ('group', 'Element group'), ('nodes', 'Nodes'), ('elements', 'Elements'),
        *((key, title) for key, title, *_rest in group_cells.COLUMNS))))
VALUE = Param('value', 'Value', 'string', default='')


def _row(geometry: Any, kind: str, entity: int) -> int:
    ids = [int(i) for i in getattr(geometry, ID_ARRAYS[kind])]
    if entity not in ids:
        shown = id_runs_text(ids) or 'none'
        raise refused(f'no {LABELS[kind]} {entity}; the geometry has {shown}')
    return ids.index(entity)


def _whole(text: str, least: int | None = None) -> int:
    value = int(float(text))
    if least is not None and value < least:
        raise ValueError(f'must be at least {least}')
    return value


def _finite(text: str) -> float:
    value = float(text)
    if not math.isfinite(value):
        raise ValueError('must be a finite number')
    return value


def _check_nodes(code: int, nodes: list[int]) -> None:
    """An element of this type holds its count of nodes, and more than one place."""
    from hertzforge.core.geometry import ELEMENT_TYPES

    word, count, _render = ELEMENT_TYPES[code]
    if count and len(nodes) != count:
        raise ValueError(f'a {word} takes {count} nodes, not {len(nodes)}')
    if len(nodes) > 1 and len(set(nodes)) == 1:
        raise ValueError(f'a {word} needs more than one node, not {nodes[0]} over and over')


def _set_field(geometry: Any, kind: str, row: int, field: str, text: str) -> None:
    """One cell of an entity table written: an id that others refer to is
    renumbered by the geometry, a coordinate system a node names must
    exist, an element group takes the elements named into it."""
    from hertzforge.core.geometry import CS_TYPES, ELEMENT_TYPES
    from hertzforge.rotate import angles_of

    if kind == 'groups' and field in group_cells.KEYS:
        group_cells.set_cell(geometry, int(geometry.group_id[row]), field, text)
        return
    if kind == 'nodes':
        if field == 'id':
            geometry.renumber_node(row, _whole(text, 1))
        elif field in ('x', 'y', 'z'):
            # in the node's placement system (Brandon, 2026-11-11)
            local = geometry.node_local_xyz([row])[0]
            local['xyz'.index(field)] = _finite(text)
            geometry.set_node_local_xyz(row, local)
        elif field == 'placement_cs':
            # the same numbers in the other system, so the node moves to them
            geometry.set_node_placement(row, _whole(text))
        else:
            cs = _whole(text)
            if cs not in {int(c) for c in geometry.cs_id}:
                raise ValueError(f'no coordinate system {cs}')
            geometry.node_disp_cs[row] = cs
    elif kind == 'coordinate_systems':
        if field == 'id':
            geometry.renumber_coordinate_system(row, _whole(text, 1))
        elif field == 'name':
            geometry.cs_name[row] = text
        elif field == 'type':
            wanted = text.strip().lower()
            codes = [c for c, word in CS_TYPES.items() if word == wanted]
            if not codes:
                raise ValueError(f'expected one of {sorted(CS_TYPES.values())}')
            # the nodes placed in it keep their numbers, so they move
            geometry.set_coordinate_system_type(int(geometry.cs_id[row]), codes[0])
        elif field.startswith('origin'):
            # through `place_coordinate_system`, so its nodes move with it
            origin = np.array(geometry.cs_matrix[row, 3], dtype=float)
            origin['xyz'.index(field[-1])] = _finite(text)
            geometry.place_coordinate_system(int(geometry.cs_id[row]), origin=origin)
        else:
            angles = list(angles_of(geometry.cs_matrix[row]))
            angles['xyz'.index(field[-1])] = _finite(text)
            geometry.place_coordinate_system(int(geometry.cs_id[row]), angles=angles)
    elif kind == 'elements':
        if field == 'id':
            geometry.elem_id[row] = _whole(text, 1)
        elif field == 'type':
            wanted = text.strip().lower()
            codes = [c for c, (word, _n, _r) in ELEMENT_TYPES.items() if word == wanted]
            if not codes:
                raise ValueError(f'unknown element type {text!r}')
            _check_nodes(codes[0], [int(n) for n in geometry.elem_conn[row]])
            geometry.elem_type[row] = codes[0]
        elif field == 'group':
            wanted = text.strip()
            for k, group in enumerate(geometry.group_id):
                if wanted in (_group_label(geometry, k), geometry.group_name[k].strip(),
                              str(int(group))):
                    geometry.elem_group[row] = int(group)
                    return
            raise ValueError(f'no element group {wanted!r}; add it first')
        else:
            nodes = parse_id_runs(text, 'a node id')
            missing = set(nodes) - set(geometry.node_id.tolist())
            if missing:
                raise ValueError(f'unknown nodes {sorted(missing)}')
            _check_nodes(int(geometry.elem_type[row]), nodes)
            geometry.elem_conn[row] = np.asarray(nodes, dtype=np.int64)
    else:
        if field == 'id':
            geometry.renumber_group(row, _whole(text, 1))
        elif field == 'name':
            geometry.group_name[row] = text
        else:
            _set_group_elements(geometry, row, text)


def _set_group_elements(geometry: Any, row: int, text: str) -> None:
    """Claim the elements named for this element group: an element named
    leaves the group it was in, and one left out that would be in no
    group is refused, since every element is in exactly one."""
    wanted = parse_id_runs(text)
    known = {int(i) for i in geometry.elem_id}
    missing = sorted(set(wanted) - known)
    if missing:
        raise ValueError(f'unknown elements {missing}')
    group = int(geometry.group_id[row])
    here = {int(i) for i in geometry.elem_id[geometry.elem_group == group]}
    dropped = sorted(here - set(wanted))
    if dropped:
        shown = ', '.join(str(i) for i in dropped[:4])
        more = f' (and {len(dropped) - 4} more)' if len(dropped) > 4 else ''
        raise ValueError(f'element {shown}{more} would be left with no element group; '
                         'name it in the element group it belongs in instead')
    if wanted:
        geometry.elem_group[np.isin(geometry.elem_id, wanted)] = group


def _snapshot(geometry: Any) -> dict[str, Any]:
    names = ('node_id', 'node_xyz', 'node_def_cs', 'node_disp_cs', 'cs_id', 'cs_type',
             'cs_matrix', 'elem_id', 'elem_type', 'elem_group', 'group_id')
    held = {name: np.array(getattr(geometry, name), copy=True) for name in names}
    held['cs_name'] = list(geometry.cs_name)
    held['group_name'] = list(geometry.group_name)
    held['group_properties'] = dict(geometry.group_properties)
    held['group_solids'] = dict(geometry.group_solids)
    held['elem_conn'] = [np.array(c, copy=True) for c in geometry.elem_conn]
    return held


def _restore(geometry: Any, held: dict[str, Any]) -> None:
    for name, value in held.items():
        setattr(geometry, name, value)


def set_entity(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.set_entity`: one field of one entity written, a cell of
    the entity table typed over. The geometry is checked
    after (`Geometry.validate`) and left as it was when the edit would
    make it inconsistent: an element given a type of another family than
    its group's, say."""
    name = params['geometry']
    return _set_at_row(project, name, params['kind'], _row(project[name], params['kind'],
                                                           params['entity']),
                       params['field'], params['value'])


def _set_at_row(project: Any, name: str, kind: str, row: int, field: str, value: str
                ) -> list[dict]:
    """One field of the entity on `row` written; ids may repeat, so the row says which."""
    geometry = project[name]
    if field not in FIELDS[kind]:
        raise refused(f'a {LABELS[kind]} has the fields {", ".join(filter(None, FIELDS[kind]))}; '
                      f'not {field}')
    entity = int(getattr(geometry, ID_ARRAYS[kind])[row])
    if kind == 'nodes' and field == 'id' and entity in project._data_nodes():
        raise refused(f'data in the project is measured at node {entity}; delete or rename '
                      'those channels first')
    held = _snapshot(geometry)
    try:
        _set_field(geometry, kind, row, field, value)
        geometry.validate()
    except (ValueError, KeyError) as error:
        _restore(geometry, held)
        said = str(error).strip(chr(39))
        if said.startswith(('could not convert', 'invalid literal')):
            said = f'{value!r} is not a number'
        raise refused(f'{LABELS[kind]} {entity}, {field}: {said}') from None
    return _shown(name, geometry, kind, f'{LABELS[kind]} {entity}, {field} set')


def set_entity_cell(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.set_entity_cell`: a cell of an entity table typed over in
    place, by row (from 1) and column (a field of `FIELDS`): Set Entity
    for the entity on that row."""
    geometry = project[params['geometry']]
    kind = params['kind']
    part = {key: params.get(key, '') for key in ('family', 'groups')}
    picked = picked_rows(geometry, kind, part['family'], part['groups'])
    count = len(getattr(geometry, ID_ARRAYS[kind])) if picked is None else len(picked)
    if not 1 <= params['row'] <= count:
        raise refused(f'the {LABELS[kind]} table has {count} rows; there is no row '
                      f'{params["row"]}')
    at = params['row'] - 1 if picked is None else int(picked[params['row'] - 1])
    views = _set_at_row(project, params['geometry'], kind, at, params['column'],
                        params['value'])
    part = {key: value for key, value in part.items() if value}
    if part:
        # the part's own table again, not every row of the kind
        views[-1] = entity_table(geometry, kind, views[-1]['title'], params['geometry'], **part)
    return views


IDS = Param('ids', 'Ids, as 1-3, 7', 'string', default='')


def delete_entities(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.delete_entities`: entities deleted by id, as Delete does
    on the rows of the entity table. Nodes go with the
    elements that use them (`Geometry.delete_nodes`), and a node some
    data in the project is measured or solved at is refused, by the
    project's own list of them; coordinate systems go through
    `Geometry.delete_coordinate_systems`, which refuses the last one;
    elements and element groups through the project's
    `delete_elements` and `delete_element_groups`, which keep the nodes
    data is measured at."""
    name = params['geometry']
    geometry = project[name]
    kind = params['kind']
    ids = parse_id_runs(params['ids'])
    if not ids:
        raise refused(f'name the {LABELS[kind]}s to delete, such as 1-3, 7')
    known = {int(i) for i in getattr(geometry, ID_ARRAYS[kind])}
    unknown = sorted(set(ids) - known)
    if unknown:
        raise refused(f'{name} has no {LABELS[kind]} {id_runs_text(unknown)}')
    try:
        if kind == 'nodes':
            measured = sorted(set(ids) & project._data_nodes())
            if measured:
                raise ValueError(f'data in the project is measured at node '
                                 f'{id_runs_text(measured)}; delete or rename those '
                                 'channels first')
            report = geometry.delete_nodes(ids)
        elif kind == 'coordinate_systems':
            report = geometry.delete_coordinate_systems(ids)
        elif kind == 'elements':
            report = project.delete_elements(name, ids)
        else:
            report = project.delete_element_groups(name, ids)
    except ValueError as error:
        raise refused(str(error)) from None
    singular = {'nodes': 'node', 'elements': 'element', 'groups': 'element group',
                'coordinate_systems': 'coordinate system'}
    removed = ', '.join(f'{count} {singular[key]}{"s" * (count != 1)}'
                        for key, count in report.items() if count and key in singular)
    return _shown(name, geometry, kind, f'removed {removed}' if removed else '')


# ---- turning and sliding a coordinate system ----------------------------------------

CS = Param('cs', 'Coordinate system (0: the first)', 'integer', default=0, min=0, step=1)
AXIS = Param('axis', 'About its own axis', 'enum', default='z', options=(
    Option('x', 'X'), Option('y', 'Y'), Option('z', 'Z')))
SLIDE_AXIS = Param('axis', 'Along its own axis', 'enum', default='x', options=(
    Option('x', 'X'), Option('y', 'Y'), Option('z', 'Z')))
ANGLE = Param('angle', 'Angle', 'number', default=0.0, unit='°', step=1.0)
DISTANCE = Param('distance', 'Distance', 'number', default=0.0, unit='m', step=0.001)


def _frame(geometry: Any, cs: int) -> tuple[int, np.ndarray]:
    cs = _system(geometry, cs)
    row = [int(c) for c in geometry.cs_id].index(cs)
    return cs, np.asarray(geometry.cs_matrix[row], dtype=float)


def _with_gizmo(views: list[dict], name: str, geometry: Any, cs: int) -> list[dict]:
    """The drawn geometry with the arrows and rings on coordinate system `cs`, along its
    own axes: dragging them runs Slide or Rotate."""
    _cs, matrix = _frame(geometry, cs)
    held = {'geometry': name, 'cs': cs}
    views[0]['gizmo'] = v.gizmo(
        matrix[3], matrix[:3],
        {'op': 'project.slide_coordinate_system', 'params': held,
         'values': dict.fromkeys('xyz', 'distance'), 'adds': {a: {'axis': a} for a in 'xyz'}},
        {'op': 'project.rotate_coordinate_system', 'params': held,
         'values': dict.fromkeys('xyz', 'angle'), 'adds': {a: {'axis': a} for a in 'xyz'}})
    return views


def rotate_coordinate_system(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.rotate_coordinate_system`: a coordinate system turned by
    a typed angle about one of its own axes (`rotate.rotate_frame`) and
    placed there (`Geometry.place_coordinate_system`)."""
    from hertzforge.rotate import angles_of, rotate_frame

    name = params['geometry']
    geometry = project[name]
    cs, matrix = _frame(geometry, params['cs'])
    turned = rotate_frame(matrix, 'xyz'.index(params['axis']), np.radians(params['angle']))
    geometry.place_coordinate_system(cs, angles=angles_of(turned))
    project.record_call(name, 'place_coordinate_system', cs,
                        angles=[float(a) for a in angles_of(turned)])
    return _with_gizmo(_shown(name, geometry, 'coordinate_systems',
                              f'turned {cs} {params["angle"]:+g}° about its '
                              f'{params["axis"].upper()}'), name, geometry, cs)


def slide_coordinate_system(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.slide_coordinate_system`: a coordinate system's origin
    moved a typed distance along one of its own axes
    (`rotate.translate_frame`, `Geometry.place_coordinate_system`). The distance is in the
    geometry's stored length unit, meters once its units are declared."""
    from hertzforge.rotate import translate_frame

    name = params['geometry']
    geometry = project[name]
    cs, matrix = _frame(geometry, params['cs'])
    moved = translate_frame(matrix, 'xyz'.index(params['axis']), params['distance'])
    geometry.place_coordinate_system(cs, origin=moved[3])
    project.record_call(name, 'place_coordinate_system', cs,
                        origin=[float(x) for x in moved[3]])
    return _with_gizmo(_shown(name, geometry, 'coordinate_systems',
                              f'moved {cs} {params["distance"]:+g} along its '
                              f'{params["axis"].upper()}'), name, geometry, cs)


def reset_rotation(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.reset_rotation`: a coordinate system set back to no
    rotation, keeping its origin (`rotate.identity_frame`, placed with
    `Geometry.place_coordinate_system`)."""
    from hertzforge.rotate import angles_of, identity_frame

    name = params['geometry']
    geometry = project[name]
    cs, matrix = _frame(geometry, params['cs'])
    angles = [float(a) for a in angles_of(identity_frame(matrix))]
    geometry.place_coordinate_system(cs, angles=angles)
    project.record_call(name, 'place_coordinate_system', cs, angles=angles)
    return _shown(name, geometry, 'coordinate_systems', f'rotation of {cs} reset')


# ---- DOF arrows and sensors ---------------------------------------------------------

#: past this many nodes a geometry's own arrows hide it
DOF_ARROW_NODE_LIMIT = 300
MARKED = ('geometry', 'channel_table', *DATA_KINDS)
MARKED_SOURCE = Param('source', 'DOFs of', 'enum', options_from='objects:' + '|'.join(MARKED))
QUANTITY = Param('measured', 'Quantity (blank: the first the data measures)', 'string',
                 default='')
ROLE = Param('role', 'Channels of a table', 'enum', default='all', options=(
    Option('all', 'All channels'), Option('control', 'Control channels'),
    Option('reference', 'References'), Option('response', 'Responses'),
    Option('monitor', 'Monitors')))


def _home(project: Any, name: str) -> tuple[str, Any]:
    from hertzforge.core.geometry import Geometry

    if isinstance(project[name], Geometry):
        return name, project[name]
    found = project.geometry_for(name)
    if found is None:
        raise refused(f'{name} has no geometry to draw on; link one')
    return found


def _marked_dofs(project: Any, name: str, params: dict[str, Any], geometry: Any
                 ) -> tuple[list[str], list[bool], str]:
    """The DOFs to mark, whether each points in, and what they are:
    a geometry's every node X, Y and Z; a channel table's channels in a
    role (`ChannelTable.chosen`), a force or a drive voltage pointing in; or the DOFs data
    measures as a quantity (`core.report.series_quantity_dofs`)."""
    from hertzforge.core.channel_table import ChannelTable
    from hertzforge.core.geometry import Geometry
    from hertzforge.core.report import (
        EXCITATION_QUANTITIES,
        series_dof_quantities,
        series_quantity_dofs,
    )

    obj = project[name]
    if isinstance(obj, Geometry):
        if obj.num_nodes > DOF_ARROW_NODE_LIMIT:
            raise refused(f'no DOF arrows on {obj.num_nodes:,} nodes: past '
                          f'{DOF_ARROW_NODE_LIMIT} they hide the model; pick data with it '
                          'to see the DOFs it measures')
        dofs = [f'{int(node)}{axis}' for node in obj.node_id for axis in ('X+', 'Y+', 'Z+')]
        return dofs, [False] * len(dofs), 'every node\'s X, Y and Z'
    if isinstance(obj, ChannelTable):
        try:
            rows = obj.chosen(params['role'])
        except ValueError as error:
            raise refused(str(error)) from None
        dofs, types = obj.dof_strings(), obj.types()
        return ([dofs[i] for i in rows], [types[i] in EXCITATION_QUANTITIES for i in rows],
                {o.value: o.label for o in ROLE.options}[params['role']].lower())
    series = [(name, obj, None)]
    quantities = series_dof_quantities(series)
    quantity = params['measured'].strip().lower().replace(' ', '_')
    if not quantities:
        raise refused(f'{name} measures no quantity at a DOF; declare its units first')
    if not quantity:
        quantity = quantities[0]
    elif quantity not in quantities:
        raise refused(f'{name} measures {", ".join(quantities)} at its DOFs; not {quantity}')
    dofs = series_quantity_dofs(series, quantity)
    return dofs, [quantity in EXCITATION_QUANTITIES] * len(dofs), quantity.replace('_', ' ')


def dof_arrows(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.dof_arrows`: an arrow at each DOF on the geometry the
    source is linked with, along that axis of the node's displacement
    coordinate system (`drawing.viz.geometry.dof_arrow_vectors`), all
    one length (`dof_arrow_length`); a force points in and ends on its node. The arrows are extra
    nodes and lines of the mesh, a shaft and a head of two barbs, since
    a `shape3d` has no markers, and their table lists each DOF and where
    it points."""
    from hertzforge.drawing.viz.geometry import dof_arrow_length, dof_arrow_vectors

    name = params['source']
    home, geometry = _home(project, name)
    dofs, incoming, what = _marked_dofs(project, name, params, geometry)
    if not dofs:
        raise refused(f'{name} has no {what} to mark')
    found = dof_arrow_vectors(geometry, dofs)
    xyz = np.asarray(geometry.node_xyz, dtype=float).reshape(-1, 3)
    drawn = [(dof, into, hit) for dof, into, hit in zip(dofs, incoming, found, strict=True)
             if hit is not None]
    nodes: list = []
    lines: list = []
    if drawn:
        length = dof_arrow_length(xyz[[hit[0] for _d, _i, hit in drawn]], _extent(xyz))
        for _dof, into, (row, vector, _axis) in drawn:
            at = xyz[row]
            if into:
                _arrow(at - length * vector, at, nodes, lines)
            else:
                _arrow(at, at + length * vector, nodes, lines)
    rows = []
    for dof, into, hit in zip(dofs, incoming, found, strict=True):
        if hit is None:
            rows.append([dof, '', 'not drawn: no such node or direction on ' + home])
            continue
        row, vector, _axis = hit
        rows.append([dof, ' '.join(f'{c:+.3f}' for c in vector),
                     'in, ending on the node' if into else 'out from the node'])
    missing = len(dofs) - len(drawn)
    title = (f'DOFs of {name} on {home}: {what}, {len(drawn)} arrow'
             f'{"s" * (len(drawn) != 1)}'
             + (f'; {missing} of {len(dofs)} have none' if missing else ''))
    return [_drawn(f'{home}, DOFs of {name}', geometry, nodes, lines),
            v.table(title, ['DOF', 'Direction (global)', 'Arrow'], rows)]


TABLE = Param('table', 'Channel table', 'enum', options_from='objects:channel_table')
#: a multiple of true size, since a 6 mm
#: sensor on a 2 m structure is a speck
SIZE = Param('size', 'Drawn at', 'enum', default=1, options=tuple(
    Option(factor, label) for label, factor in (
        ('True size', 1), ('×2', 2), ('×5', 5), ('×10', 10), ('×20', 20), ('×50', 50),
        ('×100', 100))))
def sensors(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.sensors`: the sensors a channel table places on its
    geometry (`Project.sensors`, `core.sensors.place`), each centered on
    its node and turned into its frame, at a multiple of true size. Each
    body is built from its catalog entry's shape and size, with its
    connector and cable, and a triax's letters or a uniaxial's arrows on
    its faces (`drawing.viz.sensors`), as `bodies` on the `shape3d`."""
    from hertzforge.drawing.viz.sensors import rows_of, sensor_bodies

    name = params['table']
    try:
        placed, notes = project.sensors(name)
    except (TypeError, ValueError) as error:
        raise refused(str(error)) from None
    if not placed:
        raise refused(f'{name} names no sensor model at a node of its geometry'
                      + (f': {notes[0]}' if notes else ''))
    home, geometry = project.geometry_for(name)
    bodies, said = sensor_bodies(placed, geometry, rows_of(geometry.node_id),
                                 float(params['size']))
    notes = [*notes, *said]
    rows = [[str(sensor.node), sensor.model or 'none named', sensor.kind,
             ' '.join(str(r + 1) for r in sensor.rows),
             ' × '.join(f'{s:g}' for s in sensor.entry.size) + ' mm',
             'generic body' if sensor.generic else ''] for sensor in placed]
    readings = [v.row('Sensors', float(len(placed)), '', 'info'),
                v.row('Drawn at', f'{params["size"]} × true size', None, 'info')]
    readings += [v.row('Note', note, None, 'warn') for note in notes]
    view = _drawn(f'{home}, sensors of {name}', geometry)
    view['bodies'] = bodies
    return [view,
            v.table(f'Sensors of {name} on {home}',
                    ['Node', 'Model', 'Kind', 'Channels', 'Body', 'Note'], rows),
            v.reading(f'Sensors of {name}', readings)]


# ---- Work Up ------------------------------------------------------------------------

def _still_missing(project: Any) -> list[dict]:
    """What the reports are still without: the slots required, those left
    optional, and the blocks with nothing to show."""
    from hertzforge.core.report import Report

    slots = project.missing()
    rows = [v.row('Missing', label, None, 'warn')
            for label, _cls, _icon, _n, optional, _side in slots if not optional]
    rows = rows or [v.row('Missing', 'nothing required', None, 'pass')]
    rows += [v.row('Optional', label, None, 'info')
             for label, _cls, _icon, _n, optional, _side in slots if optional]
    for name, held in project.items():
        count = len(held.unbound(project, project.object_groups)) if isinstance(held, Report) else 0
        if count:
            rows.append(v.row('Unbound', f'{count} block{"s" * (count != 1)} of {name}', None,
                              'info'))
    return rows


def _is_a_run(project: Any, members: Any) -> bool:
    """Whether objects in one group are a test's: they hold a recording,
    or they hold neither a geometry nor shapes, which make a model of it."""
    from hertzforge.core.data import TimeHistory, TransientSpecification
    from hertzforge.core.geometry import Geometry
    from hertzforge.core.shapes import ShapeSet

    held = [project[name] for name in members]
    return (any(isinstance(obj, TimeHistory) and not isinstance(obj, TransientSpecification)
                for obj in held)
            or (bool(held) and not any(isinstance(obj, (Geometry, ShapeSet)) for obj in held)))


def _place_the_run(project: Any) -> list[str]:
    """A file's import group into the Basis when the Basis holds no run: a
    Basis of a geometry alone leaves every other slot looking for its object
    in a group that has none of them. A second run stays where it is."""
    basis = project.object_group_with_role('Basis')
    if basis is not None and _is_a_run(project, basis['members']):
        return []
    groups = [group for group in project.object_groups if group is not basis]
    from hertzforge.core.data import TimeHistory

    run = next((group for group in groups
                if any(isinstance(project[name], TimeHistory) for name in group['members'])
                and _is_a_run(project, group['members'])),
               next((group for group in groups if _is_a_run(project, group['members'])), None))
    if run is None:
        return []
    members = list(run['members'])
    if basis is None:
        project.set_role(members[0], 'Basis')
        return members
    try:
        project.link(*basis['members'], *members, role='Basis')
    except (ValueError, KeyError) as error:
        return [f'not placed: {str(error).strip(chr(39))}']
    return members


def _place_basis(project: Any, test: str) -> list[str]:
    """The run's group placed in the Basis, then the Basis slots of a kind of
    test filled from the objects that satisfy them
    (`core.report.expectation_satisfiers`), each placed in the Basis group
    (`Project.place`) unless it is already in another group. Returns what
    was placed, or the core's refusal of a placement."""
    from hertzforge.core.report import expectation_satisfiers

    moved = _place_the_run(project)
    placed = project.sides()
    members = expectation_satisfiers(test, dict(project.items()),
                                     placed).get('Basis', [])
    elsewhere = {name for group in project.object_groups if group['role'] != 'Basis'
                 for name in group['members']}
    here = set(placed.get('Basis', ()))
    for name in members:
        if name in elsewhere or name in here:
            continue
        try:
            project.place(name, 'Basis')
        except (ValueError, KeyError) as error:
            return [*moved, f'not placed: {str(error).strip(chr(39))}']
        moved.append(name)
    return moved


def _srs_bands(project: Any, added: list[str]) -> list[dict]:
    """Where a low-pass stopped an SRS Work Up made, since nothing else in
    its reading would say the top of the band was left off."""
    rows = []
    for name in added:
        made = project.provenance.get(name) or {}
        corner = (project.srs_ceiling(made['source']) if made.get('verb') == 'compute_srs'
                  else None)
        if corner is not None:
            rows.append(v.row('SRS band', f'{name} stops at {corner:g} Hz, the low-pass corner '
                              'of the record it was read from; the filter takes 6 dB at the '
                              'corner, so the top of the band reads low', None, 'info'))
    return rows


def _work_up(test: str) -> Any:
    def run(project: Any, params: dict[str, Any]) -> list[dict]:
        """Work Up in one module, which says the kind of test: the objects
        that fill its Basis placed there, then every missing object it
        expects computed from what is loaded, the report last
        (`Project.work_up`), and what is still missing after it."""
        if not project.names:
            raise refused('Nothing is loaded to work up')
        moved = _place_basis(project, test)
        try:
            added = project.work_up(test)
        except ValueError as error:
            raise refused(str(error)) from None
        rows = ([v.row('Added', name, None, 'pass') for name in added]
                or [v.row('Added', 'nothing to compute', None, 'info')])
        if moved:
            rows.append(v.row('Placed in the Basis', ', '.join(moved), None, 'info'))
        rows += _srs_bands(project, added)
        rows += [v.row('Not computed', f'{slot}: {why}', None, 'warn')
                 for slot, why in dict.fromkeys(project.last_work_up_skipped)]
        return [v.reading(f'Work Up, {test}', rows + _still_missing(project))]
    return run


# ---- channel roles and DOF names ----------------------------------------------------

ROLE_SOURCE = Param('source', 'Time history or channel table', 'enum',
                    options_from='objects:time_history|channel_table')
RECORD = Param('record', 'Channel', 'integer', default=1, min=1, step=1)
CHANNEL_ROLE = Param('role', 'Role', 'enum', default='response', options=(
    Option('reference', 'Reference: drives the structure'),
    Option('response', 'Response: measured on the structure'),
    Option('monitor', 'Monitor: recorded, not analyzed')))
RENAMED = ('channel_table', *DATA_KINDS)
RENAME_SOURCE = Param('source', 'Object', 'enum', options_from='objects:' + '|'.join(RENAMED))
OLD = Param('old', 'DOF', 'string', default='')
NEW = Param('new', 'New DOF', 'string', default='')
RENAME_QUANTITY = Param('measured', 'Quantity (blank: every channel at the DOF)', 'string',
                        default='')


def channels_table(project: Any, name: str, title: str) -> dict:
    """An object's channels as a table: a channel table's own columns
    (`core.tables.table_of`), or a data object's records with each one's
    DOF, reference, quantity and, for a time history, role."""
    from hertzforge.core.channel_table import ChannelTable
    from hertzforge.core.data import TimeHistory
    from hertzforge.core.tables import table_of

    obj = project[name]
    if isinstance(obj, ChannelTable):
        home = project.geometry_for(name)
        headers, rows = table_of(obj, geometry=None if home is None else home[1])
        return v.table(title, headers, rows)
    references = obj.reference_dof
    roles = obj.channel_roles() if isinstance(obj, TimeHistory) else None
    headers = ['Record', 'DOF', *(['Reference'] if references is not None else []),
               'Quantity', *(['Role'] if roles is not None else [])]
    rows = []
    for i in range(obj.num_records):
        dof = str(obj.response_dof[i])
        row = [str(i + 1), dof]
        if references is not None:
            row.append(str(references[i]))
        row.append(obj.known_dim(i).replace('_', ' '))
        if roles is not None:
            row.append(roles.get((dof, str(obj.ordinate_dim[i])), ''))
        rows.append(row)
    return v.table(title, headers, rows)


def set_channel_role(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.set_channel_role`: one channel made a reference, a
    response or a monitor (`Project.set_channel_role`), on a time
    history or a channel table and on what is linked with it. The channel is the record, or
    the table's row, counted from 1."""
    from hertzforge.core.data import TimeHistory

    name = params['source']
    obj = project[name]
    count = obj.num_records if isinstance(obj, TimeHistory) else obj.num_channels
    if params['record'] > count:
        raise refused(f'{name} has {count} channels; there is no channel {params["record"]}')
    index = params['record'] - 1
    if isinstance(obj, TimeHistory):
        dof, quantity = str(obj.response_dof[index]), str(obj.ordinate_dim[index])
    else:
        dof, quantity = obj.identities()[index]
    try:
        changed = project.set_channel_role(name, dof, quantity, params['role'])
    except (ValueError, TypeError) as error:
        raise refused(f'{name}: {error}') from None
    said = f'{dof} ({quantity}) is a {params["role"]} on {", ".join(changed)}'
    histories = [n for n in changed if isinstance(project[n], TimeHistory)]
    if histories:
        roles = list(project[histories[0]].channel_roles().values())
        counts = [sum(r == kind for r in roles) for kind in ('reference', 'response',
                                                             'monitor')]
        said += (f': {counts[0]} reference{"s" * (counts[0] != 1)}, {counts[1]} '
                 f'response{"s" * (counts[1] != 1)}, {counts[2]} monitor'
                 f'{"s" * (counts[2] != 1)}')
    return [channels_table(project, name, said)]


def rename_dof(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.rename_dof`: a channel's coordinate corrected on an object
    and on every object derived from it (`Project.rename_dof`), the
    channel and not the point: with a quantity, only that channel at the
    DOF moves."""
    name = params['source']
    old, new = params['old'].strip(), params['new'].strip()
    if not old or not new:
        raise refused('name the DOF as it is and the DOF to give it, such as 101Z+')
    quantity = params['measured'].strip().lower().replace(' ', '_') or None
    try:
        changed = project.rename_dof(name, old, new, quantity)
    except (ValueError, TypeError) as error:
        raise refused(f'{name}: {error}') from None
    what = f'{old} ({quantity.replace("_", " ")})' if quantity else old
    return [channels_table(project, name, f'{what} is now {new} on {", ".join(changed)}'),
            v.reading(f'Renamed {old}', [v.row('Changed', changed_name, None, 'info')
                                         for changed_name in changed])]


# ---- a channel table's control loops ------------------------------------------------

ENVIRONMENT = Param('environment', 'Environment (blank: the first)', 'string', default='')
FORM = Param('form', 'View', 'enum', default='channels', options=(
    Option('channels', 'Channels'), Option('response', 'Response transformation'),
    Option('reference', 'Reference transformation')))
CONTROL = Param('control', 'Control', 'boolean', default=True)


def _environment(table: Any, text: str) -> str | None:
    loops = list(table.environments)
    if not text.strip():
        return loops[0] if loops else None
    if text.strip() not in loops:
        held = ', '.join(loops) if loops else 'none'
        raise refused(f'the table\'s environments are {held}; not {text.strip()!r}')
    return text.strip()


def channel_table_view(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.channel_table_view`: a channel table in one of its control
    loops: its channels with
    the Control column of that environment (`core.tables.table_of`), or
    what the controller put over them, the response transformation (a
    row per controlled response over the control channels) or the
    reference transformation over the drives
    (`core.tables.transformation_table`)."""
    from hertzforge.core.tables import table_of, transformation_table

    name = params['table']
    table = project[name]
    environment = _environment(table, params['environment'])
    form = params['form']
    if form == 'channels':
        home = project.geometry_for(name)
        headers, rows = table_of(table, geometry=None if home is None else home[1],
                                 environment=environment)
        title = name + (f', control channels of {environment}' if environment else '')
        return [v.table(title, headers, rows)]
    if environment is None:
        raise refused(f'{name} has no control loops, so no transformation')
    try:
        headers, rows = transformation_table(table, environment, form)
    except ValueError as error:
        raise refused(str(error)) from None
    return [v.table(f'{name}, {environment}, {form} transformation', headers, rows)]


def set_control(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.set_control`: Control checked or cleared on one channel
    of one control loop (`ChannelTable.set_control`), the Control column
    of the table in that environment; a table with no loops
    has its own Control column, written as a cell (`set_cell`). The
    channel is the table's row, counted from 1."""
    name = params['table']
    table = project[name]
    if params['record'] > table.num_channels:
        raise refused(f'{name} has {table.num_channels} channels; there is no channel '
                      f'{params["record"]}')
    environment = _environment(table, params['environment'])
    row = params['record'] - 1
    try:
        if environment is None:
            table.set_cell('control', row, params['control'])
            project.record_call(name, 'set_cell', 'control', row, params['control'])
        else:
            table.set_control(environment, row, params['control'])
            project.record_call(name, 'set_control', environment, row, params['control'])
    except ValueError as error:
        raise refused(str(error)) from None
    return channel_table_view(project, {**params, 'form': 'channels'})


# ---- object groups ------------------------------------------------------------------

OBJECT = Param('object', 'Object', 'enum', options_from=ANY)
FIRST = Param('first', 'Object', 'enum', options_from=ANY)
SECOND = Param('second', 'Linked with', 'enum', options_from=ANY)
MORE = Param('more', 'And (names, comma separated)', 'string', default='')
LINK_NAME = Param('name', 'Group name (blank: keep it)', 'string', default='')
GROUP_ROLE = Param('role', 'Role', 'enum', default='Basis', options=(
    Option('Basis', 'Basis of comparisons'), Option('none', 'None')))
SIDE = Param('side', 'Into', 'enum', default='Basis', options=(
    Option('Basis', 'The Basis'), Option('other', 'The other group')))
NEW_NAME = Param('name', 'Name (blank: remove it)', 'string', default='')


def groups_table(project: Any, said: str = '') -> dict:
    """The project's object groups: each one's name, role, members, and
    whether comparisons are made against it."""
    rows = [[str(k + 1), group.get('name', ''), group['role'] or '',
             ', '.join(group['members']), 'yes' if group['role'] == 'Basis' else '']
            for k, group in enumerate(project.object_groups)]
    return v.table('Object groups' + (f': {said}' if said else ''),
                   ['Group', 'Name', 'Role', 'Members', 'Basis'], rows)


def _grouped(call: Any, *args: Any, **kwargs: Any) -> Any:
    try:
        return call(*args, **kwargs)
    except (ValueError, KeyError) as error:
        raise refused(str(error).strip("'")) from None


def link(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.link`: objects linked into one group, merging the groups
    they are in (`Project.link`), which refuses a second geometry and a
    member the group's geometry cannot carry."""
    names = [params['first'], params['second'],
             *(n.strip() for n in params['more'].split(',') if n.strip())]
    if len(set(names)) < 2:
        raise refused('linking takes two or more different objects')
    members = _grouped(project.link, *dict.fromkeys(names), name=params['name'].strip() or None)
    return [groups_table(project, 'linked ' + ', '.join(members))]


def unlink(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.unlink`: an object taken out of its group, a group left
    with one member dissolved unless it has a role (`Project.unlink`)."""
    name = params['object']
    if project.object_group_of(name) is None:
        raise refused(f'{name} is in no object group')
    project.unlink(name)
    return [groups_table(project, f'unlinked {name}')]


def set_basis(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.set_basis`: an object's group made the Basis, the group
    comparisons are made against (`Project.set_basis`); the Basis is
    unique, so any other group loses it."""
    name = params['object']
    _grouped(project.set_basis, name)
    return [groups_table(project, f'the group of {name} is the Basis')]


def set_role(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.set_role`: the role of an object's group set or cleared
    (`Project.set_role`), the bracket menu's Basis of Comparisons
    checked or not."""
    name = params['object']
    role = None if params['role'] == 'none' else params['role']
    _grouped(project.set_role, name, role)
    return [groups_table(project, f'the group of {name} has role {role or "none"}')]


def place(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.place`: one object put into the Basis or the other
    group, the group made when there is none (`Project.place`), as a
    drop on a gray slot of the tree does."""
    name = params['object']
    role = None if params['side'] == 'other' else params['side']
    _grouped(project.place, name, role)
    return [groups_table(project, f'placed {name} in '
                         + ('the Basis' if role else 'the other group'))]


def name_object_group(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.name_object_group`: the group an object is in named, or
    its name removed when blank (`Project.name_object_group`); a named
    group exports as an activity of that name."""
    name, given = params['object'], params['name'].strip()
    _grouped(project.name_object_group, name, given or None)
    return [groups_table(project, f'group named {given!r}' if given else 'group name removed')]


# ---- descriptors --------------------------------------------------------------------

def _model(id_: str, title: str, summary: str, params: tuple, outputs: tuple, run: Any,
           edits: bool = True, workspace: Workspace = 'geometry') -> Operation:
    """An act on a geometry: in the Geometry module unless it is the finite
    element model's (element groups, solving)."""
    return Operation(id=id_, title=title, workspace=workspace,
                     summary=summary, params=params, outputs=outputs, run=run, edits=edits)


def _shared(id_: str, title: str, summary: str, params: tuple, outputs: tuple, run: Any,
            edits: bool = True, workspace: Workspace = 'signals') -> Operation:
    """An act on the project every module offers: the groups, a channel's
    role or DOF. `workspace` is its home."""
    return Operation(id=id_, title=title, workspace=workspace,
                     summary=summary, params=params, outputs=outputs, run=run, edits=edits,
                     everywhere=True)


DRAWN = ('shape3d', 'table')
GROUPS = ('table',)

OPERATIONS = (
    _model('project.view_entities', 'Edit',
           'A geometry\'s nodes, coordinate systems, a family\'s elements or element groups '
           'as a table to edit, beside the geometry drawn.',
           (GEOMETRY, KIND, Param('family', 'Family whose elements the table shows', 'string',
                                  default=''),
            Param('groups', 'Element groups the table shows', 'string', default='')),
           DRAWN, view_entities, edits=False),
    _model('project.add_node', 'Add Node',
           'A node at typed coordinates, in meters once the geometry\'s units are '
           'declared.',
           (GEOMETRY, X, Y, Z, NODE_ID, PLACEMENT_CS, DISPLACEMENT_CS), DRAWN, add_node),
    _model('project.add_coordinate_system', 'Add Coordinate System',
           'A coordinate system at a typed origin, turned about the fixed X, then Y, then '
           'Z axes.',
           (GEOMETRY, X, Y, Z, RX, RY, RZ, CS_NAME, CS_TYPE, NODE_ID), DRAWN,
           add_coordinate_system),
    _model('project.add_element', 'Add Element',
           'An element of a family on the nodes named, in order; for beams, the chain of '
           'elements through every node named.',
           (GEOMETRY, FAMILY, NODES, ELEMENT_TYPE, GROUP), DRAWN, add_element),
    _model('project.add_element_group', 'Add Element Group', 'An empty element group.',
           (GEOMETRY, GROUP_NAME, NODE_ID), DRAWN, add_element_group, workspace='geometry'),
    _model('project.set_entity', 'Set Entity',
           'One field of a node, coordinate system, element or element group, as a cell '
           'of its table is typed over.',
           (GEOMETRY, KIND, ENTITY, FIELD, VALUE), DRAWN, set_entity),
    _model('project.delete_entities', 'Delete Entities',
           'Nodes, coordinate systems, elements or element groups deleted by id; nodes '
           'data is measured at, and the last coordinate system, are kept.',
           (GEOMETRY, KIND, IDS), (*DRAWN, 'reading'), delete_entities),
    _model('project.rotate_coordinate_system', 'Rotate Coordinate System',
           'A coordinate system turned by a typed angle about one of its own axes.',
           (GEOMETRY, CS, AXIS, ANGLE), DRAWN, rotate_coordinate_system),
    _model('project.slide_coordinate_system', 'Slide Coordinate System',
           'A coordinate system\'s origin moved a typed distance along one of its own '
           'axes.',
           (GEOMETRY, CS, SLIDE_AXIS, DISTANCE), DRAWN, slide_coordinate_system),
    _model('project.reset_rotation', 'Reset Rotation',
           'A coordinate system set back to no rotation, keeping its origin.',
           (GEOMETRY, CS), DRAWN, reset_rotation),
    _model('project.dof_arrows', 'DOF Arrows',
           'An arrow at each DOF on the geometry: every node\'s X, Y and Z, a channel '
           'table\'s channels in a role, or what data measures as one quantity.',
           (MARKED_SOURCE, QUANTITY, ROLE), DRAWN, dof_arrows, edits=False),
    _model('project.sensors', 'Sensors',
           'The sensors a channel table names, centered on their nodes and turned as the '
           'table says, at a multiple of true size.',
           (TABLE, SIZE), (*DRAWN, 'reading'), sensors, edits=False),
    _shared('project.set_channel_role', 'Set Channel Role',
             'One channel made a reference, a response or a monitor, on a time history or '
             'channel table and on what is linked with it.',
             (ROLE_SOURCE, RECORD, CHANNEL_ROLE), ('table',), set_channel_role),
    _shared('project.rename_dof', 'Rename DOF',
             'A channel\'s DOF corrected on an object and on everything derived from it; '
             'with a quantity, only that channel at the DOF.',
             (RENAME_SOURCE, OLD, RENAME_QUANTITY, NEW), ('table', 'reading'), rename_dof),
    _shared('project.channel_table_view', 'Channel Table View',
             'A channel table in one control loop: its channels with that loop\'s Control '
             'column, or the loop\'s response or reference transformation.',
             (TABLE, ENVIRONMENT, FORM), ('table',), channel_table_view, edits=False,
             workspace='geometry'),
    _shared('project.set_control', 'Set Control',
             'Control checked or cleared on one channel of one control loop.',
             (TABLE, ENVIRONMENT, RECORD, CONTROL), ('table',), set_control,
             workspace='geometry'),
    _shared('project.link', 'Link', 'Objects linked into one group, merging the groups '
             'they are in.', (FIRST, SECOND, MORE, LINK_NAME), GROUPS, link),
    _shared('project.unlink', 'Unlink', 'An object taken out of its group.',
             (OBJECT,), GROUPS, unlink),
    _shared('project.set_basis', 'Basis of Comparisons',
             'An object\'s group made the Basis, the group the others are compared against.',
             (OBJECT,), GROUPS, set_basis),
    _shared('project.place', 'Place',
             'One object put into the Basis or the other group.', (OBJECT, SIDE), GROUPS,
             place),
    _shared('project.set_role', 'Set Group Role',
             'The role of an object\'s group set, or cleared.', (OBJECT, GROUP_ROLE),
             GROUPS, set_role),
    _shared('project.name_object_group', 'Name Object Group',
             'The group an object is in named, or its name removed.',
             (OBJECT, NEW_NAME), GROUPS, name_object_group),
    Operation(
        id='project.set_entity_cell', title='Set Entity Cell', workspace='geometry', everywhere=True,
        summary='One cell of an entity table typed over in place, by row and column; the '
                'tables edit through it.',
        params=(GEOMETRY, KIND, Param('row', 'Row', 'integer', default=1, min=1, step=1),
                Param('column', 'Column', 'string', default='id'),
                Param('value', 'Value', 'string', default=''),
                Param('family', 'Family whose elements the table shows', 'string', default=''),
                Param('groups', 'Element groups the table shows', 'string', default='')),
        outputs=('shape3d', 'table'), run=set_entity_cell, edits=True),
    *(Operation(
        id=f'project.work_up_{module}', title=f'Work Up {test}', workspace=module,
        summary=f'Every object a {test} test expects computed from what is loaded, the report '
                'last.',
        params=(), outputs=('reading',), run=_work_up(test))
      for module, test in (('sysid', 'System ID'), ('random', 'Random Vibration'),
                           ('shock', 'Shock'), ('transient', 'Transient'),
                           ('sine', 'Sine Sweep'), ('modal', 'Modal Test'))),
)
