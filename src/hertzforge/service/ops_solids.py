"""Solid modeling on the connection's project: a new geometry, the solids it is made of (blocks,
cylinders, cones, spheres, sections, plates, disks and holes), the
booleans and patterns of them, Mesh Solids, Mate Surfaces and the view a
geometry opens on.

Each operation runs the `Project` verb the panes run (`add_solid_block`,
`combine_solids`, `mesh_solids` and the rest) and answers with the
geometry drawn, its meshed elements and its parts' surfaces as the
kernel tessellates them (`core.solids.shape_triangles`), and a table of the
solids. Solids are named by id from that table ("1, 3").
Lengths are typed in the connection's display system, marked `length` here and taken to
meters by the server before an operation runs, so every operation reads meters; angles are
in degrees about the geometry's X, then Y, then Z axes.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import replace
from typing import Any

import numpy as np

from . import views as v
from .descriptors import Operation, Option, Param
from .mesh import drawn_mesh, sides_view
from .op_progress import hook
from .parsing import parse_id_runs, parse_numbers
from .protocol import refused

GEOMETRY = Param('geometry', 'Geometry', 'enum', options_from='objects:geometry')
NAME = Param('name', 'Name', 'string', default='')
SOLIDS = Param('solids', 'Solids (ids, as "1, 3")', 'string', default='')
PARTS = Param('solids', 'Solids (ids, as "1, 3"; empty for every part)', 'string', default='')


def _length(name: str, label: str, default: float, low: float | None = None) -> Param:
    return Param(name, label, 'number', default=default, unit='m', min=low, step=0.01,
                 length=True)


def _angle(axis: str, prefix: str = 'angle', label: str = 'Angle') -> Param:
    return Param(f'{prefix}_{axis}', f'{label} about {axis.upper()}', 'number', default=0.0,
                 unit='deg', step=1.0)


CENTER = tuple(_length(axis, f'Center {axis.upper()}', 0.0) for axis in 'xyz')
ANGLES = tuple(_angle(axis) for axis in 'xyz')
WIDTHS = tuple(_length(f'width_{axis}', f'Width along {axis.upper()}', 1.0, 0.0)
               for axis in 'xyz')
CORNER = _length('corner', 'Corner radius', 0.0, 0.0)
#: on each shape's Add: 0 adds one; a solid's id changes that solid instead, the form
#: its Edit (Brandon, 2026-11-10: adding and editing a solid are one form)
SOLID_ID = Param('solid', 'Solid (0: a new one)', 'integer', default=0, min=0, step=1)


def _section_shapes() -> tuple[Option, ...]:
    from hertzforge.core.solids import SECTIONS

    return tuple(Option(shape, f'{shape[:1].upper()}{shape[1:]}') for shape in SECTIONS)


#: a section's dimensions, in the order
#: `core.solids.SECTIONS` names them
SECTION_DEFAULTS = {'I-beam': (1.0, 0.5, 0.1, 0.1), 'channel': (1.0, 0.5, 0.1, 0.1),
                    'tee': (1.0, 1.0, 0.1, 0.1), 'angle': (1.0, 1.0, 0.1),
                    'rectangular tube': (1.0, 0.5, 0.1), 'round tube': (1.0, 0.1)}


def _kind_label(kind: str) -> str:
    return {'subtract': 'Subtraction', 'intersect': 'Intersection'}.get(
        kind, f'{kind[:1].upper()}{kind[1:]}')


def _label(item: Any) -> str:
    """A solid: its name, or its kind and id."""
    return item.name.strip() or f'{_kind_label(item.kind)} {item.id}'


def _geometry(project: Any, params: dict[str, Any]) -> tuple[str, Any]:
    name = params['geometry']
    return name, project[name]


def _numbers(text: str, what: str) -> list[float]:
    return parse_numbers(text, f'{what}: numbers separated by commas or spaces')


def _ids(geometry: Any, text: str, every: list[int] | None = None) -> list[int]:
    """The solids `text` names by id ("1, 3"), each one the geometry has;
    `every` when it names none and that is allowed."""
    known = [s.id for s in geometry.solids]
    if not text.strip():
        if every is not None and every:
            return every
        if not known:
            raise refused('the geometry has no solids yet; add one first')
        raise refused(f'name the solids by id, as "1, 3"; the geometry has '
                      f'{", ".join(map(str, known))}')
    chosen = parse_id_runs(text, 'a solid id')
    for number in chosen:
        if number not in known:
            raise refused(f'the geometry has solids {", ".join(map(str, known)) or "none"}; '
                          f'not {number}')
    return chosen


def _parts(geometry: Any) -> list[int]:
    from hertzforge.core import solids

    return [s.id for s in solids.top_level(geometry)]


def _call(make: Callable[[], Any]) -> Any:
    """What `make` returns, a refusal of the core's said in its own words."""
    try:
        return make()
    except (ValueError, KeyError) as error:
        text = error.args[0] if isinstance(error, KeyError) and error.args else str(error)
        raise refused(str(text)) from None


# ---- the geometry drawn, and its solids listed ---------------------------------

def _f(value: float) -> str:
    return f'{float(value):.4g}'


def _meters(geometry: Any) -> str | None:
    """The unit of a length row: an undefined geometry's numbers are the file's own."""
    return 'm' if geometry.units_defined else None


def _in_meters(geometry: Any) -> str:
    """The same, to follow a length in words."""
    return ' m' if geometry.units_defined else ''


#: the color a solid being added, edited or picked is drawn in
HIGHLIGHT = (88, 166, 255)


def _drawn(title: str, geometry: Any, defined: bool | None = None,
           highlight: Any = ()) -> tuple[dict, list[dict]]:
    """The geometry as a `shape3d`: its elements as `view_geometry` draws
    them, and each part's surface as the kernel tessellates it
    (`solids.shape_triangles`). A part the
    kernel cannot make is left out and said in a row. `defined` says
    whether its lengths are meters, when it is a mesh not yet joined to
    the geometry it was made from. The parts in `highlight`, or the parts
    holding those solids, are drawn in their own color (`bodies`)."""
    from hertzforge.core import solids

    used, triangles, lines = drawn_mesh(geometry)
    nodes = [np.asarray(geometry.node_xyz, dtype=float).reshape(-1, 3)[used]]
    faces = [triangles.reshape(-1, 3)]
    start = len(used)
    notes = []
    lit = _parts_holding(geometry, highlight)
    shown: list[np.ndarray] = []
    for item in solids.top_level(geometry):
        try:
            soup = solids.shape_triangles(solids.shape(geometry, item.id))
        except ValueError as error:
            notes.append(v.row(_label(item), f'not drawn: {error}', None, 'warn'))
            continue
        if item.id in lit:
            shown.append(soup.reshape(-1, 3))
            continue
        count = 3 * len(soup)
        nodes.append(soup.reshape(-1, 3))
        faces.append(np.arange(start, start + count).reshape(-1, 3))
        start += count
    defined = geometry.units_defined if defined is None else defined
    view = v.shape3d(title, 'm' if defined else 'undefined',
                     np.concatenate(nodes), [], triangles=np.concatenate(faces), lines=lines)
    if shown:
        corners = np.concatenate(shown).astype(np.float32)
        view['bodies'] = {
            'nodes': corners, 'triangles': np.arange(len(corners), dtype=np.uint32).reshape(-1, 3),
            'colors': np.tile(np.asarray(HIGHLIGHT, dtype=np.uint8), (len(corners), 1)),
            'follow': np.zeros(len(corners), dtype=np.uint32)}
    return view, notes


def _parts_holding(geometry: Any, ids: Any) -> set[int]:
    """The standing parts these solids are, or are inside of."""
    from hertzforge.core import solids

    within = solids.used_by(geometry)
    held = set()
    for solid_id in ids:
        solid_id = int(solid_id)
        while solid_id in within:
            solid_id = within[solid_id]
        held.add(solid_id)
    return held


def _result(name: str, geometry: Any, title: str, rows: list[dict],
            picked: list[int] | None = None) -> list[dict]:
    """What a solids operation answers with: the geometry drawn, and what it
    did only where something warns or fails, those rows alone. The drawing
    says the rest (Brandon, 2026-11-13 and 11-14). The solids `picked` (one
    added or edited) are highlighted and carry the gimbal."""
    view, notes = _drawn(name, geometry, highlight=picked or ())
    if picked:
        view['gizmo'] = _solids_gizmo(name, geometry, picked)
    said = [row for row in rows + notes if row['status'] in ('warn', 'fail')]
    return [v.reading(title, said), view] if said else [view]


# ---- New Geometry and Solids ---------------------------------------------------

GEOMETRY_NAME = Param('name', 'Name', 'string', default='Geometry')
UNIT = Param('unit', 'Length unit', 'enum', default='', options=tuple(
    Option(unit, label) for unit, label in (('', 'The display units'), ('m', 'm'),
                                            ('mm', 'mm'), ('in', 'in'), ('ft', 'ft'))))


def new_geometry(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.new_geometry`: an empty geometry to build a part in from
    solids (`Project.new_geometry`), recording a length unit, the display
    system's unless one is named; its coordinates are held in SI either way.
    The app runs it at once, asking nothing (Brandon, 2026-11-10): a name is
    changed in the tree."""
    from .display_units import words_in

    unit = params['unit'] or words_in('length')[0]
    if unit not in ('m', 'mm', 'in', 'ft'):
        unit = 'm'
    added = _call(lambda: project.new_geometry(params['name'].strip() or 'Geometry', unit=unit))
    return [v.reading(f'New geometry {added}', [
        v.row('Added', added, None, 'pass'),
        v.row('Length unit', unit, None, 'info'),
        v.row('Next', 'add a plate, a block or another solid to it', None, 'info')])]


def part_solids(name: str, geometry: Any, ids: list[int] | None = None) -> list[dict]:
    """What the Solids row or solids picked in the tree show: each part's surface
    drawn with the mesh, before it is meshed too (Brandon, 2026-11-10). Solids picked
    (`ids`) carry the gimbal, whose drag runs Move Part on them. A part the kernel
    cannot make is said above it."""
    view, notes = _drawn(name, geometry, highlight=ids or ())
    if ids:
        view['gizmo'] = _solids_gizmo(name, geometry, ids)
    return [*([v.reading(name, notes)] if notes else []), view]


def view_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.solids`: a geometry's solids drawn; it changes nothing."""
    name, geometry = _geometry(project, params)
    return _result(name, geometry, name, [])


# ---- adding a solid -------------------------------------------------------------

def _center(params: dict[str, Any]) -> tuple[float, float, float]:
    return (params['x'], params['y'], params['z'])


def _angles(params: dict[str, Any], prefix: str = 'angle') -> tuple[float, float, float]:
    return tuple(params[f'{prefix}_{axis}'] for axis in 'xyz')


#: each shape's Add by the kind of solid it makes; on a solid of that kind picked in the
#: tree it is its Edit, the same form with the solid's own values
ADD_OF_KIND = {'block': 'project.add_solid_block', 'cylinder': 'project.add_solid_cylinder',
               'cone': 'project.add_solid_cone', 'sphere': 'project.add_solid_sphere',
               'plate': 'project.add_solid_plate', 'disk': 'project.add_solid_disk',
               **{shape: 'project.add_solid_section' for shape in SECTION_DEFAULTS}}


def _recipe(kind: str, params: dict[str, Any]) -> dict[str, Any]:
    """What an Add form's values set on a solid of `kind`, as `Project.edit_solid` takes them."""
    from hertzforge.core.solids import SECTIONS

    out: dict[str, Any] = {'center': _center(params)}
    if kind != 'sphere':
        out['angles'] = _angles(params)
    if kind == 'block':
        out['widths'] = [params[f'width_{a}'] for a in 'xyz']
        out['corner'] = params['corner']
    elif kind == 'plate':
        out['widths'] = [params['width_x'], params['width_y']]
    if kind in ('cylinder', 'cone', 'sphere', 'disk'):
        out['radius'] = params['radius']
    if kind == 'cylinder':
        out['length'] = params['length']
    elif kind == 'cone':
        out['top_radius'] = params['top_radius']
        out['length'] = params['height']
    elif kind in SECTIONS:
        out['length'] = params['length']
        if params['dimensions'].strip():
            out['dimensions'] = _numbers(params['dimensions'], 'the dimensions')
    if params['name'].strip():
        out['name'] = params['name'].strip()
    return out


def solid_fill(item: Any, scale: float) -> dict[str, str]:
    """An Add form's values for the solid `item`, its Edit: lengths times `scale`, which
    takes meters into the units the form is typed in."""
    from hertzforge.core.solids import SECTIONS

    def length(value: float) -> str:
        return f'{value * scale:.6g}'

    fill = {'solid': str(int(item.id)), 'name': item.name}
    fill.update({axis: length(c) for axis, c in zip('xyz', item.center, strict=True)})
    if item.kind != 'sphere':
        fill.update({f'angle_{axis}': f'{a:.6g}' for axis, a in zip('xyz', item.angles, strict=True)})
    if item.kind in ('block', 'plate'):
        fill.update({f'width_{axis}': length(w) for axis, w in zip('xyz', item.widths, strict=False)
                     if item.kind == 'block' or axis != 'z'})
    if item.kind == 'block':
        fill['corner'] = length(item.corner)
    if item.kind in ('cylinder', 'cone', 'sphere', 'disk'):
        fill['radius'] = length(item.radius)
    if item.kind == 'cylinder':
        fill['length'] = length(item.length)
    elif item.kind == 'cone':
        fill['top_radius'] = length(item.dimensions[0] if item.dimensions else 0.0)
        fill['height'] = length(item.length)
    elif item.kind in SECTIONS:
        fill['shape'] = item.kind
        fill['length'] = length(item.length)
        fill['dimensions'] = ', '.join(length(d) for d in item.dimensions)
    return fill


def _solid_values(item: Any) -> dict[str, Any]:
    """A solid's values under the names `_recipe` gives them."""
    held: dict[str, Any] = {'center': item.center, 'angles': item.angles, 'name': item.name,
                            'radius': item.radius, 'length': item.length, 'corner': item.corner,
                            'dimensions': item.dimensions}
    held['widths'] = item.widths[:2] if item.kind == 'plate' else item.widths
    if item.kind == 'cone':
        held['top_radius'] = item.dimensions[0] if item.dimensions else 0.0
    return held


def _same(value: Any, held: Any) -> bool:
    """Whether a form's value is the solid's, to the six figures the form shows it in."""
    if held is None:
        return False
    if isinstance(value, str) or isinstance(held, str):
        return value == held
    a, b = np.ravel(np.asarray(value, dtype=float)), np.ravel(np.asarray(held, dtype=float))
    return a.shape == b.shape and bool(np.allclose(a, b, rtol=1e-5, atol=1e-12))


def _edited(project: Any, name: str, geometry: Any, params: dict[str, Any],
            kind: str) -> list[dict]:
    """An Add form run on a solid already made (`solid`): that solid set to the form's
    values, highlighted, with the gimbal on it."""
    from hertzforge.core import solids

    solid_id = int(params['solid'])
    try:
        item = solids.solid(geometry, solid_id)
    except KeyError:
        raise refused(f'{name} has no solid {solid_id}') from None
    if item.kind != kind:
        raise refused(f'solid {solid_id} is a {item.kind}, not a {kind}: add a new one')
    # only what differs from the solid: a form shows a value to six figures, so the
    # fields not touched come back a hair off and must not move it
    now = _solid_values(item)
    changes = {key: value for key, value in _recipe(kind, params).items()
               if not _same(value, now.get(key))}
    if changes:
        _call(lambda: project.edit_solid(name, solid_id, **changes))
    rows = [v.row('Changed' if changes else 'Unchanged',
                  f'{_label(solids.solid(geometry, solid_id))}, solid {solid_id}', None,
                  'pass' if changes else 'info')]
    if geometry.num_nodes:
        rows.append(v.row('Mesh', 'Mesh Solids again to mesh the change', None, 'info'))
    return _result(name, geometry, f'Edited {name}', rows, [solid_id])


def _add(project: Any, params: dict[str, Any], make: Callable[[str], int],
         kind: str = '') -> list[dict]:
    """Add the solid `make` makes in the picked geometry, and say it: what it is, and
    its volume, or a flat one's area. With `solid` set, change that solid instead."""
    from hertzforge.core import solids

    name, geometry = _geometry(project, params)
    if params.get('solid'):
        return _edited(project, name, geometry, params, kind)
    new = _call(lambda: make(name))
    item = solids.solid(geometry, new)
    rows = [v.row('Added', f'{_label(item)}, solid {new}', None, 'pass')]
    try:
        if item.kind in solids.FLAT:
            rows.append(v.row('Area', solids.area(geometry, new),
                              'm²' if geometry.units_defined else None, 'info'))
            rows.append(v.row('Thickness', 'set on the element group once it is meshed '
                              'into shells', None, 'info'))
        else:
            rows.append(v.row('Volume', solids.volume(geometry, new),
                              'm³' if geometry.units_defined else None, 'info'))
    except ValueError as error:
        rows.append(v.row('Volume', f'not read: {error}', None, 'warn'))
    return _result(name, geometry, f'Added to {name}', rows, [new])


def add_solid_block(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_block`: a block, its widths along its own axes,
    centered and turned (`Project.add_solid_block`)."""
    return _add(project, params, lambda name: project.add_solid_block(
        name, _center(params), [params[f'width_{a}'] for a in 'xyz'], _angles(params),
        params['name'], corner=params['corner']), 'block')


def add_solid_cylinder(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_cylinder`: a cylinder about its own Z
    (`Project.add_solid_cylinder`)."""
    return _add(project, params, lambda name: project.add_solid_cylinder(
        name, _center(params), params['radius'], params['length'], _angles(params),
        params['name']), 'cylinder')


def add_solid_cone(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_cone`: a cone or frustum along its own Z, its
    base below (`Project.add_solid_cone`)."""
    return _add(project, params, lambda name: project.add_solid_cone(
        name, _center(params), params['radius'], params['top_radius'], params['height'],
        _angles(params), params['name']), 'cone')


def add_solid_sphere(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_sphere`: a sphere about its center
    (`Project.add_solid_sphere`)."""
    return _add(project, params, lambda name: project.add_solid_sphere(
        name, _center(params), params['radius'], params['name']), 'sphere')


def add_solid_section(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_section`: a structural section along its own Z
    (`Project.add_solid_section`), its dimensions typed in the order
    `core.solids.SECTIONS` names them, or the opening ones when none are
    typed."""
    from hertzforge.core.solids import SECTIONS

    shape = params['shape']
    typed = params['dimensions']
    dimensions = (_numbers(typed, 'the dimensions') if typed.strip()
                  else SECTION_DEFAULTS[shape])
    if len(dimensions) != len(SECTIONS[shape]):
        raise refused(f'{"an" if shape[0] in "aeiouI" else "a"} {shape} takes '
                      f'{", ".join(SECTIONS[shape])}: {len(SECTIONS[shape])} numbers')
    return _add(project, params, lambda name: project.add_solid_section(
        name, shape, _center(params), dimensions, params['length'], _angles(params),
        params['name']), shape)


def add_solid_plate(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_plate`: a flat rectangle in its own XY plane,
    meshed later into shells (`Project.add_solid_plate`)."""
    return _add(project, params, lambda name: project.add_solid_plate(
        name, _center(params), (params['width_x'], params['width_y']), _angles(params),
        params['name']), 'plate')


def add_solid_disk(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_disk`: a flat circle in its own XY plane,
    meshed later into shells (`Project.add_solid_disk`)."""
    return _add(project, params, lambda name: project.add_solid_disk(
        name, _center(params), params['radius'], _angles(params), params['name']), 'disk')


def add_solid_hole(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.add_solid_hole`: a hole entering at its center and running
    down its own Z, through when its depth is 0, counterbored when given
    one (`Project.add_solid_hole`); subtract it from a part to cut it."""
    bore = (params['counterbore_diameter'], params['counterbore_depth'])
    return _add(project, params, lambda name: project.add_solid_hole(
        name, _center(params), params['diameter'], params['depth'] or None,
        _angles(params), params['name'], counterbore=bore if any(bore) else None))


# ---- changing solids ------------------------------------------------------------

SOLID = Param('solid', 'Solid (id)', 'integer', default=1, min=1, step=1)
KEEP_NOTE = 'empty keeps it'


def _field(name: str, label: str) -> Param:
    return Param(name, f'{label} ({KEEP_NOTE})', 'string', default='', length='[m]' in label)


EDIT_FIELDS = (
    _field('center', 'Center, x, y, z [m]'), _field('widths', 'Widths [m]'),
    _field('angles', 'Angles about X, Y, Z [deg]'), _field('radius', 'Radius [m]'),
    _field('top_radius', 'Top radius [m]'), _field('length', 'Length or height [m]'),
    _field('corner', 'Corner radius [m]'), _field('dimensions', 'Dimensions [m]'),
    _field('counts', 'Counts along X, Y, Z'), _field('spacing', 'Spacing [m]'),
    _field('name', 'Name'))


def edit_solid(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.edit_solid`: a solid changed after it is made, only the
    fields typed (`Project.edit_solid`); a boolean it is part of changes
    with it, and a mesh made from it waits for Mesh Solids again."""
    from hertzforge.core import solids

    name, geometry = _geometry(project, params)
    solid_id = params['solid']
    changes: dict[str, Any] = {}
    for key in ('center', 'widths', 'angles', 'dimensions', 'spacing'):
        if params[key].strip():
            changes[key] = _numbers(params[key], f'the {key}')
    for key in ('radius', 'top_radius', 'length', 'corner'):
        if params[key].strip():
            values = _numbers(params[key], f'the {key.replace("_", " ")}')
            if len(values) != 1:
                raise refused(f'the {key.replace("_", " ")} is one number')
            changes[key] = values[0]
    if params['counts'].strip():
        changes['counts'] = parse_id_runs(params['counts'], 'a whole count')
    if params['name'].strip():
        changes['name'] = params['name'].strip()
    if not changes:
        raise refused('type what changes: a field left empty keeps it')
    _call(lambda: project.edit_solid(name, solid_id, **changes))
    item = solids.solid(geometry, solid_id)
    rows = [v.row('Changed', f'{_label(item)}, solid {solid_id}: {", ".join(changes)}',
                  None, 'pass')]
    if geometry.num_nodes:
        rows.append(v.row('Mesh', 'Mesh Solids again to mesh the change', None, 'info'))
    return _result(name, geometry, f'Edited {name}', rows)


OFFSET = tuple(_length(f'offset_{axis}', f'Move along {axis.upper()}', 0.0) for axis in 'xyz')
TURN = tuple(_angle(axis, 'turn', 'Turn') for axis in 'xyz')


def move_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.move_solids`: parts moved whole, each with every solid it
    is made of, turned about the middle of their extent and then moved
    (`Project.move_solids`): Move Part, its gizmo's drag typed."""
    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'])
    offset = tuple(params[f'offset_{a}'] for a in 'xyz')
    turn = _angles(params, 'turn')
    if any(offset) or any(turn):
        _call(lambda: project.move_solids(name, ids, offset, turn))
    views = _result(name, geometry, f'Moved in {name}', [])
    views[-1]['gizmo'] = _solids_gizmo(name, geometry, ids)
    return views


def _solids_gizmo(name: str, geometry: Any, ids: list[int]) -> dict:
    """The arrows and rings at the middle of the parts' extent, along the model's axes:
    dragging them runs Move Part."""
    from hertzforge.core.tetmesh import _extent

    low, high = _extent(geometry, ids)
    held = {'geometry': name, 'solids': ', '.join(map(str, ids))}
    return v.gizmo((np.asarray(low) + np.asarray(high)) / 2.0, np.eye(3),
                   {'op': 'project.move_solids', 'params': held, 'in_display': True,
                    'values': {axis: f'offset_{axis}' for axis in 'xyz'}},
                   {'op': 'project.move_solids', 'params': held,
                    'values': {axis: f'turn_{axis}' for axis in 'xyz'}})


INTO = Param('into', 'Into geometry', 'enum', options_from='objects:geometry')


def copy_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.copy_solids`: solids copied, recipe and all, into the same
    geometry or another (`Project.copy_solids`): Copy and Paste on
    solid rows."""
    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'])
    into = params['into']
    _call(lambda: project.copy_solids(name, ids, into=into))
    return _result(into, project[into], f'Copied into {into}', [])


WITHIN = Param('within', 'Within (a boolean\'s or pattern\'s id; 0 for the parts)',
               'integer', default=0, min=0, step=1)


def reorder_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.reorder_solids`: the parts, or one boolean's solids, put in
    a new order, every one of them named (`Project.reorder_solids`): the
    drag in the tree. A subtraction cuts the rest from the first."""
    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'])
    within = params['within'] or None
    _call(lambda: project.reorder_solids(name, ids, within=within))
    return _result(name, geometry, f'Reordered in {name}', [])


OPERATION = Param('operation', 'Operation', 'enum', default='union', options=(
    Option('union', 'Union'), Option('subtract', 'Subtract (the rest from the first)'),
    Option('intersect', 'Intersect')))


def combine_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.combine_solids`: two or more parts made one by a union, a
    subtraction of the rest from the first, or an intersection
    (`Project.combine_solids`)."""
    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'])
    _call(lambda: project.combine_solids(name, params['operation'], ids,
                                         params['name'].strip()))
    return _result(name, geometry, f'Combined in {name}', [])


def separate_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.separate_solids`: booleans and patterns taken apart, the
    solids they were made of parts again (`Project.separate_solids`):
    Take Apart."""
    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'])
    _call(lambda: project.separate_solids(name, ids))
    return _result(name, geometry, f'Taken apart in {name}', [])


def delete_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.delete_solids`: solids deleted, each with every solid it is
    made of (`Project.delete_solids`); a mesh made from them stays."""
    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'])
    _call(lambda: project.delete_solids(name, ids))
    return _result(name, geometry, f'Deleted from {name}', [])


COUNTS = tuple(Param(f'count_{axis}', f'How many along {axis.upper()}', 'integer',
                     default=2 if axis == 'x' else 1, min=1, step=1) for axis in 'xyz')
SPACING = tuple(_length(f'spacing_{axis}', f'Spacing along {axis.upper()}',
                        1.0 if axis == 'x' else 0.0) for axis in 'xyz')


def pattern_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.pattern_solids`: parts copied in rows, columns and layers
    along X, Y and Z, the originals counted, as one solid whose copies
    follow them (`Project.pattern_solids`)."""
    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'])
    counts = [params[f'count_{a}'] for a in 'xyz']
    copies = counts[0] * counts[1] * counts[2] - 1
    if not copies:
        raise refused('one of each along every axis: no copies yet')
    made = _call(lambda: project.pattern_solids(
        name, ids, counts, [params[f'spacing_{a}'] for a in 'xyz']))
    return _result(name, geometry, f'Patterned in {name}', [], [made])


# ---- Mesh Solids ----------------------------------------------------------------

ELEMENTS = Param('elements', 'Elements', 'enum', default='hexes', options=(
    Option('hexes', 'Hexes'), Option('tetras', 'Tetras'),
    Option('quads', 'Shells (quads)'), Option('triangles', 'Shells (triangles)')))
SIZE = Param('size', 'Element size', 'number', default=0.0, unit='m', min=0.0, step=0.001,
             length=True, suggest='project.mesh_size')
APART = Param('apart', 'Keep apart from other groups', 'boolean', default=False)
PARTS_APART = Param('parts_apart', 'Keep the parts apart from each other', 'boolean',
                    default=False)
ANYWAY = Param('anyway', 'Mesh even where the fit is likely to fold', 'boolean',
               default=False)
PIECE_SIZES = Param('sizes', 'A size per piece [m], as "3: 0.01, 5: 0.002" (hexes)', 'string',
                    default='', length=True)
DETECT_SIZES = Param('detect', 'Detect a size for every piece', 'boolean', default=False)



def likely_size(geometry: Any, ids: list[int], elements: str) -> float:
    """The element size the parts `ids` are likely to hold at as `elements`, in meters:
    what Mesh's size opens on, and what its Detect button fills in."""
    return _likely_size(geometry, ids, elements)


def _likely_size(geometry: Any, ids: list[int], elements: str) -> float:
    """The size the pane opens on: an extrusion's half the thinnest
    feature for bricks, a flat part's across its plane for shells, and
    otherwise `hexmesh.suggested_size`."""
    from hertzforge.core import hexmesh, shells, solids

    if elements in ('quads', 'triangles'):
        return shells.mesh_size(geometry, ids)
    if elements == 'hexes':
        try:
            solids.extrusion_axis(geometry, ids)
            return solids.mesh_size(geometry, ids)
        except solids.NotAnExtrusion:
            pass
    return hexmesh.suggested_size(geometry, ids, elements)


def _fold_check(geometry: Any, ids: list[int], size: float,
                sizes: dict[int, float] | None = None) -> str:
    """The question before a fit is paid for, as words, or '' when the
    bricks are swept or the fit is likely to hold
    (`hexmesh.likely_to_fold` on each fitted piece)."""
    from hertzforge.core import hexmesh, solids

    try:
        solids.extrusion_axis(geometry, ids)
        return ''
    except solids.NotAnExtrusion:
        pass
    for entry in solids.piece_plan(geometry, ids, size, sizes):
        if entry['method'] != 'overlay':
            continue
        piece = ids if entry['id'] is None else [entry['id']]
        with solids.meshed_as(geometry, piece[0], entry['cuts']) as read_as:
            shown = [read_as, *piece[1:]]
            read = hexmesh.likely_to_fold(geometry, shown, entry['size'])
            if not read['likely']:
                continue
            # the size it is likely to hold at, counted as the limit counts:
            # bricks by volume and pillow, and the grid
            bricks = solids.fitted_brick_count(geometry, shown, read['suggested'])
            cells = hexmesh.grid_cells(geometry, shown, read['suggested'], piece=True)
        why = []
        if np.isfinite(read['across']) and read['across'] < hexmesh.ACROSS - 0.5:
            why.append(f'the smallest round feature is {read["across"]:.1f} bricks across '
                       f'its radius, and fits have held at {hexmesh.ACROSS:g} or more')
        if np.isfinite(read['through']) and read['through'] < hexmesh.THROUGH - 0.5:
            why.append(f'the thinnest wall is {read["through"]:.1f} bricks through, '
                       f'and a fit needs {hexmesh.THROUGH:g}')
        if read['flat']:
            why.append(f'{read["flat"]:,} bricks are flat before any fitting')
        unit = _in_meters(geometry)
        said = (f'{entry["name"]}: at {entry["size"]:.4g}{unit} the fit is likely to fold: '
                f'{"; ".join(why)}. {read["suggested"]:.3g}{unit} is likely to hold')
        if bricks > solids.MESH_LIMIT or cells > solids.GRID_LIMIT:
            said += (', but that is too many bricks: the part wants a finer size on its '
                     'thin piece than on the rest, which one fit cannot give; Tetras '
                     'can mesh it')
        return said
    return ''


def _made_rows(geometry: Any, made: dict, earlier: int) -> list[dict]:
    """What a mesh holds."""
    rows = []
    unit = _in_meters(geometry)
    method = made.get('method')
    if 'tets' in made:
        rows += [v.row('Ten-node tetrahedra', float(made['tets']), '', 'pass'),
                 v.row('Nodes', float(made['nodes']), '', 'info'),
                 v.row('Settled in', float(made['steps']), 'steps', 'info'),
                 v.row('Worst radius ratio', float(made['quality']), '',
                       'warn' if made['poor'] else 'pass'),
                 v.row('Under 0.3', float(made['poor']), '', 'info')]
    elif method == 'shells':
        rows += [v.row('Shells', float(made['elements']), '', 'pass'),
                 v.row('Nodes', float(made['nodes']), '', 'info'),
                 v.row('Where', ' and '.join(w for w, n in (
                     ('on their plane', made['flat']),
                     ('on the outer surface', made['surfaces'])) if n), None, 'info'),
                 v.row('Thickness', 'set on the element group', None, 'info')]
    else:
        rows += [v.row('Bricks', float(made['bricks']), '', 'pass'),
                 v.row('Nodes', float(made['nodes']), '', 'info')]
        if method == 'pieces':
            for piece in made['pieces']:
                how = ('fitted' if piece['method'] == 'overlay' else
                       'swept along its own axis' if piece.get('turned') else
                       f'swept along {"XYZ"[piece["axis"]]}')
                rows.append(v.row(piece['name'], f'{how} at {piece["size"]:.4g}{unit}, '
                                  f'{piece["bricks"]:,} bricks', None, 'info'))
            rows.append(v.row('Shared at the seams', float(made['seam_shared']), 'nodes',
                              'info'))
            if made['ties']:
                rows.append(v.row('Joined rigidly at the seams', float(made['ties']),
                                  'nodes', 'info'))
        elif method == 'overlay':
            rows += [v.row('Method', 'a grid fitted to the shape (not an extrusion)',
                           None, 'info'),
                     v.row('Worst scaled Jacobian', float(made['quality']), '',
                           'warn' if made['poor'] else 'pass'),
                     v.row('Under 0.3', float(made['poor']), '', 'info')]
        else:
            rows.append(v.row('Method', 'swept along its own axis' if made.get('turned')
                              else f'extruded along {"XYZ"[made["axis"]]}', None, 'info'))
            if made.get('rounded'):
                rows.append(v.row('Nodes on circles', float(made['rounded']), '', 'info'))
    if 'shortest' in made:
        rows.append(v.row('Shortest edge', float(made['shortest']), _meters(geometry), 'info'))
    if earlier:
        rows.append(v.row('Replaces', f'the {earlier:,} elements meshed before', None,
                          'warn'))
    return rows


def _piece_sizes(text: str) -> dict[int, float]:
    """The sizes '3: 0.01, 5: 0.002' names, by piece."""
    out: dict[int, float] = {}
    for entry in re.sub(r'\s*:\s*', ':', text).replace(',', ' ').split():
        piece, colon, size = entry.partition(':')
        try:
            out[int(piece)] = float(size)
        except ValueError:
            raise refused(f'{entry!r} is not a piece and its size, such as 3: 0.01') from None
        if not colon or out[int(piece)] <= 0.0:
            raise refused(f'{entry!r} is not a piece and a size above 0, such as 3: 0.01')
    return out


def _likely_or_refused(geometry: Any, ids: list[int]) -> float:
    return _call(lambda: _likely_size(geometry, ids, 'hexes'))


def _likely_of_piece(geometry: Any, ids: list[int], entry: dict) -> float:
    """The size a `piece_plan` entry is likely to hold at, read as it is meshed: with what
    is cut from it."""
    from hertzforge.core import solids

    if entry['id'] is None:
        return _likely_or_refused(geometry, ids)
    with solids.meshed_as(geometry, entry['id'], entry['cuts']) as read_as:
        return _likely_or_refused(geometry, [read_as])


def mesh_solids(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.mesh_solids`: parts meshed into hexes, tetras or shells at
    an element size, each part an element group (`Project.mesh_solids`).
    There is no preview: meshing is quick, and a preview that meshed cost as
    much (Brandon, 2026-11-13). A union meshed in pieces takes a size per
    piece, typed or detected. A fitted brick mesh likely to fold
    (`hexmesh.likely_to_fold`) is refused, saying why, unless asked to mesh
    `anyway`. The size sets the time: a fine size on a large part is seconds
    to minutes."""
    from hertzforge.core import solids

    name, geometry = _geometry(project, params)
    ids = _ids(geometry, params['solids'], every=_parts(geometry))
    elements = params['elements']
    shell = elements in ('quads', 'triangles')
    flat = [i for i in ids if solids.is_flat(geometry, i)]
    if flat and not shell:
        raise refused(f'{solids.piece_name(geometry, flat[0])} is flat: a plate or a disk '
                      'is meshed into shells, quads or triangles')
    size = params['size'] or _call(lambda: _likely_size(geometry, ids, elements))
    sizes = _piece_sizes(params['sizes']) if elements == 'hexes' else {}
    said: list[dict] = []
    fold = ''
    if elements == 'hexes':
        plan = _call(lambda: solids.piece_plan(geometry, ids, size))
        in_pieces = len(plan) > 1
        wrong = sorted(set(sizes) - {entry['id'] for entry in plan} if in_pieces else sizes)
        if wrong:
            raise refused(f'there is no piece {wrong[0]}: these parts mesh in pieces '
                          f'{", ".join(str(entry["id"]) for entry in plan)}' if in_pieces else
                          'these parts mesh as one, at the element size: a size per piece is '
                          'for a union meshed in pieces')
        if params['detect'] and in_pieces:
            sizes = {int(entry['id']): _likely_of_piece(geometry, ids, entry) for entry in plan}
        elif params['detect']:
            likely = _likely_of_piece(geometry, ids, plan[0])
            said.append(v.row('Detect', f'these parts mesh as one, likely to hold at '
                              f'{likely:.4g}{_in_meters(geometry)}', None, 'warn'))
        fold = _call(lambda: _fold_check(geometry, ids, size, sizes))
    if fold:
        said.append(v.row('Fit', fold, None, 'warn'))
    if fold and not params['anyway']:
        raise refused(f'{fold}. Mesh at the size suggested, or ask to mesh anyway')
    found = _call(lambda: project.mesh_solids(
        name, ids, size, apart=params['apart'],
        parts_apart=params['parts_apart'] and elements == 'hexes', elements=elements,
        sizes=sizes or None, progress=hook()))
    rows = _made_rows(geometry, found, 0)
    if found['replaced']:
        rows.append(v.row('Replaced', f'the {found["replaced"]:,} elements meshed before',
                          None, 'info'))
    # a union meshed in pieces brings its seams again
    lost = found.get('mates_dropped', 0) - found.get('seams', 0)
    if lost > 0:
        rows.append(v.row('Mated surfaces', f'{lost} on the old mesh deleted with it: Mate '
                          'Surfaces again', None, 'warn'))
    rows += [v.row('Nodes added', float(found['added']), '', 'info'),
             v.row('Nodes shared with the geometry', float(found['shared']), '', 'info'),
             v.row('Nodes in all', float(geometry.num_nodes), '', 'info')]
    rows += [v.row(f'Group {group}', f'{held} taken off: solids take a material alone',
                   None, 'warn') for group, held in (found.get('cleared') or {}).items()]
    return _result(name, geometry, f'Meshed {name}', said + rows)


# ---- Mate Surfaces --------------------------------------------------------------

# Brandon, 2026-11-11: one pane makes a mate and edits it, the nodes of
# either side picked in the view with how the mate holds; Edit Mates and
# Edit Sides were folded into it.

MATE_PICKED = Param('mate', 'Mated surfaces (id, 0 for a new mate)', 'integer', default=0,
                    min=0, step=1)
GROUPS = Param('groups', 'Element groups (empty for every group of plates or solids)',
               'string', default='', shown_with=('mate', 0))
MATE_TOLERANCE = Param('tolerance', 'Tolerance (0 for a quarter of the finest spacing)',
                       'number', default=0.0, unit='m', min=0.0, step=0.0001, length=True,
                       shown_with=('mate', 0))
PATCH = Param('patch', 'Patch (0 keeps every patch found as it is)', 'integer', default=0,
              min=0, step=1, shown_with=('mate', 0))
MATE_NAME = Param('name', 'Name (empty for "<first group> on <second group>")', 'string',
                  default='')
HELD = tuple(Option(mode, label) for mode, label in (
    ('rigid', 'Rigid'), ('stiffness', 'Stiffness'), ('free', 'Free')))
NORMAL = Param('normal', 'Normal', 'enum', default='rigid', options=HELD)
SHEAR = Param('shear', 'Shear', 'enum', default='rigid', options=HELD)
#: a stiffness, typed in the display system
STIFFNESS = 'force/length'
NORMAL_STIFFNESS = Param('normal_stiffness', 'Normal stiffness', 'number', default=1e6,
                         unit='N/m', min=0.0, quantity=STIFFNESS,
                         shown_with=('normal', 'stiffness'))
SHEAR_STIFFNESS = Param('shear_stiffness', 'Shear stiffness', 'number', default=1e5,
                        unit='N/m', min=0.0, quantity=STIFFNESS,
                        shown_with=('shear', 'stiffness'))
MATE_SIDE = Param('side', 'A click picks a node of', 'enum', default='a', options=(
    Option('a', 'The first side'), Option('b', 'The second side')))
#: what the view's clicks have left of each side, and what they were picked on; set from
#: the view, not typed
NODES_A = Param('nodes_a', 'First side', 'string', default='', in_view=True)
NODES_B = Param('nodes_b', 'Second side', 'string', default='', in_view=True)
NODES_FOR = Param('nodes_for', 'Picked on', 'string', default='', in_view=True)
MATE_NODE = Param('toggle', 'Node clicked', 'string', default='', in_view=True)
MATE_KEEP = Param('keep', 'Keep: add the mate, or save it', 'boolean', default=False)


def _group_ids(geometry: Any, text: str) -> list[int] | None:
    known = [int(g) for g in geometry.group_id]
    if not text.strip():
        return None
    chosen = parse_id_runs(text, 'an element group number')
    for group in chosen:
        if group not in known:
            raise refused(f'the geometry has element groups '
                          f'{", ".join(map(str, known)) or "none"}; not {group}')
    return chosen


def _group_name(geometry: Any, group: int) -> str:
    names = dict(zip((int(g) for g in geometry.group_id), geometry.group_name, strict=True))
    return str(names.get(int(group), '')).strip() or f'element group {group}'


def _held(params: dict[str, Any]) -> dict[str, Any]:
    return {'normal_mode': params['normal'], 'shear_mode': params['shear'],
            'normal_stiffness': params['normal_stiffness']
            if params['normal'] == 'stiffness' else 0.0,
            'shear_stiffness': params['shear_stiffness']
            if params['shear'] == 'stiffness' else 0.0}


def _mates_table(name: str, geometry: Any) -> dict:
    """The geometry's Mated Surfaces."""
    rows = []
    for mated in geometry.mates:
        how = mated.describe()
        springs = [f'{what} {value!r} N/m' for what, mode, value in (
            ('normal', mated.normal_mode, mated.normal_stiffness),
            ('shear', mated.shear_mode, mated.shear_stiffness)) if mode == 'stiffness']
        first, second = (_group_name(geometry, g) for g in mated.groups)
        rows.append([str(mated.id), mated.name, f'{first} on {second}',
                     f'{len(mated.nodes_a):,} on {len(mated.nodes_b):,}',
                     how + (f' ({", ".join(springs)})' if springs else '')])
    return v.table(f'Mated Surfaces of {name}', ['Id', 'Name', 'Groups', 'Nodes', 'Held'],
                   rows)


def _picked_on(params: dict[str, Any]) -> str:
    """What a pane's picks belong to: the mate edited, or the groups and the patch a new
    mate is found on. Picks made on something else are dropped."""
    if params['mate']:
        return f'mate {params["mate"]}'
    return f'groups {params["groups"].strip()} patch {params["patch"]}'


def _picks(params: dict[str, Any]) -> tuple[tuple[int, int], list[int], list[int]] | None:
    """The groups and the two sides the view's clicks have left, when they were made on
    what the form names now; None when there are none yet."""
    key, _, pair = params['nodes_for'].rpartition(' on ')
    if key != _picked_on(params) or not params['nodes_a'].strip():
        return None
    first, second = parse_id_runs(pair, 'an element group number')
    return ((first, second), parse_id_runs(params['nodes_a'], 'a node id'),
            parse_id_runs(params['nodes_b'], 'a node id'))


def _toggled(geometry: Any, groups: tuple[int, int], side: str, held: list[int],
             offered: tuple[int, ...], text: str) -> list[int]:
    """`held` with each node clicked taken away, or added where the side may hold it."""
    group = groups[0] if side == 'a' else groups[1]
    for node in parse_id_runs(text, 'a node id'):
        if node in held:
            held = [n for n in held if n != node]
        elif node in offered:
            held = [*held, node]
        else:
            raise refused(f'node {node} is not on the surface of {_group_name(geometry, group)} '
                          'where it mates')
    return held


def mate_surfaces(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.mate_surfaces`: a mate between two element groups, made or edited. A new
    mate (`mate` 0) is found where the groups' surfaces mate (`Project.detect_mated_surfaces`,
    `core.mates.detect`); a kept one (`mate`) opens on what it holds. The view draws the
    model see-through with the nodes each side may hold (`core.mates.offered`), those it
    holds bright, and a click adds a node of the side chosen or takes it away. With `keep`,
    the mate is added (`Project.add_mated_surfaces`), every patch found when `patch` is 0
    and there are several, or the kept one saved (`Project.edit_mated_surfaces`)."""
    from hertzforge.core import mates as core_mates

    name, geometry = _geometry(project, params)
    if not geometry.num_nodes:
        raise refused(f'{name} has no nodes to mate: mesh it first')
    rows: list[dict] = []
    views: list[dict] = []
    mated = None
    found: list[dict] = []
    picks = _picks(params)
    if params['mate']:
        known = {m.id: m for m in geometry.mates}
        mated = known.get(params['mate'])
        if mated is None:
            raise refused(f'the geometry has mated surfaces '
                          f'{", ".join(map(str, known)) or "none"}; not {params["mate"]}')
        groups, base_a, base_b = mated.groups, list(mated.nodes_a), list(mated.nodes_b)
    elif picks is None or not params['toggle'].strip():
        chosen = _group_ids(geometry, params['groups'])
        tolerance = params['tolerance']
        if not tolerance:
            spacing = geometry.node_spacing(chosen)
            low, high = geometry.extent
            tolerance = (0.25 * spacing if spacing
                         else 1e-6 * float(np.linalg.norm(high - low) or 1.0))
        found = _call(lambda: project.detect_mated_surfaces(name, chosen, tolerance))
        if not found:
            raise refused(f'no surface of these element groups lies on another within '
                          f'{tolerance:.4g}{_in_meters(geometry)}')
        if params['patch'] > len(found):
            raise refused(f'{len(found)} patch{"es were" if len(found) > 1 else " was"} found; '
                          f'there is no patch {params["patch"]}')
        rows += [v.row('Tolerance', float(tolerance), _meters(geometry), 'info'),
                 v.row('Patches found', float(len(found)), '', 'pass')]
        views.append(v.table(f'Patches of {name}', ['Patch', 'First side (follows)',
                                                    'Second side', 'Nodes'], [
            [str(k + 1), _group_name(geometry, p['groups'][0]),
             _group_name(geometry, p['groups'][1]),
             f'{len(p["nodes_a"]):,} on {len(p["nodes_b"]):,}'] for k, p in enumerate(found)]))
        patch = found[max(params['patch'], 1) - 1]
        groups, base_a, base_b = patch['groups'], list(patch['nodes_a']), list(patch['nodes_b'])
    else:
        groups, base_a, base_b = picks
    groups = (int(groups[0]), int(groups[1]))
    every = not params['mate'] and params['patch'] == 0 and len(found) > 1
    if every:
        held_a = sorted({n for p in found for n in p['nodes_a']})
        held_b = sorted({n for p in found for n in p['nodes_b']})
        offered_a, offered_b = tuple(held_a), tuple(held_b)
    else:
        held_a, held_b = (picks[1], picks[2]) if picks else (base_a, base_b)
        offered_a, offered_b = _call(lambda: core_mates.offered(geometry, groups))
        offered_a = tuple(sorted({*offered_a, *base_a, *held_a}))
        offered_b = tuple(sorted({*offered_b, *base_b, *held_b}))
        if params['toggle'].strip():
            if params['side'] == 'a':
                held_a = _toggled(geometry, groups, 'a', held_a, offered_a, params['toggle'])
            else:
                held_b = _toggled(geometry, groups, 'b', held_b, offered_b, params['toggle'])
    first, second = (_group_name(geometry, g) for g in groups)
    rows += [v.row('First side', f'{first}, {len(held_a):,} nodes', None, 'info'),
             v.row('Second side', f'{second}, {len(held_b):,} nodes', None, 'info')]
    if every:
        rows.append(v.row('Patch', 'every patch as found; choose one to pick its nodes',
                          None, 'info'))
    if params['keep']:
        held = _held(params)
        label = params['name'].strip() or None
        if mated is not None:
            changes = {key: value for key, value in (
                ('nodes_a', tuple(held_a)), ('nodes_b', tuple(held_b)), *held.items(),
                ('name', label)) if value is not None and getattr(mated, key) != value}
            if changes:
                _call(lambda: project.edit_mated_surfaces(name, mated.id, **changes))
            (mated,) = [m for m in geometry.mates if m.id == params['mate']]
        elif every:
            for p in found:
                _call(lambda p=p: project.add_mated_surfaces(
                    name, p['groups'], p['nodes_a'], p['nodes_b'], **held))
        else:
            _call(lambda: project.add_mated_surfaces(name, groups, held_a, held_b,
                                                     name=label, **held))
    side = 'b' if params['side'] == 'b' and not every else 'a'
    what = mated.name if mated is not None else f'{first} on {second}'
    picked = {'geometry': name, 'mate': params['mate'], 'groups': params['groups'],
              'patch': params['patch'], 'side': side,
              'nodes_a': ', '.join(map(str, held_a)), 'nodes_b': ', '.join(map(str, held_b)),
              'nodes_for': f'{_picked_on(params)} on {groups[0]}, {groups[1]}'}
    hint = ('Choose a patch to pick its nodes' if every else
            f'Click a node of {first if side == "a" else second} to add it to this side '
            'or take it away')
    sides = [(offered_a, set(held_a)), (offered_b, set(held_b))]
    if side == 'b':
        sides.reverse()
    view = sides_view(geometry, f'{what}, {"second" if side == "b" else "first"} side',
                      'project.mate_surfaces', picked, sides[0], sides[1], hint,
                      pickable=not every)
    said = [] if params['keep'] else [v.reading(f'Mate Surfaces of {name}', rows)]
    return [*said, *views, view, _mates_table(name, geometry)]



MATES = Param('mates', 'Mated surfaces (ids, as "1, 3")', 'string', default='')


def delete_mated_surfaces(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.delete_mated_surfaces`: kept sets deleted by id
    (`Project.delete_mated_surfaces`)."""
    name, geometry = _geometry(project, params)
    known = [m.id for m in geometry.mates]
    ids = parse_id_runs(params['mates'], 'a mated surfaces id')
    if not ids:
        raise refused(f'name the mated surfaces by id; the geometry has '
                      f'{", ".join(map(str, known)) or "none"}')
    wrong = [i for i in ids if i not in known]
    if wrong:
        raise refused(f'the geometry has mated surfaces {", ".join(map(str, known)) or "none"}; '
                      f'not {wrong[0]}')
    project.delete_mated_surfaces(name, ids)
    return [_mates_table(name, geometry)]


# ---- Set Default View -----------------------------------------------------------

EYE = tuple(Param(f'eye_{axis}', f'Eye toward {axis.upper()}', 'number',
                  default=0.0, step=1.0) for axis in 'xyz')
UP = tuple(Param(f'up_{axis}', f'Up along {axis.upper()}', 'number',
                 default=1.0 if axis == 'z' else 0.0, step=1.0) for axis in 'xyz')


def set_view(project: Any, params: dict[str, Any]) -> list[dict]:
    """`project.set_view`: the view a geometry opens on, where the eye is
    seen from the model's center and which way is up (`Project.set_view`
    with a `View`); an eye of 0, 0, 0 returns to the default isometric
    view. It is typed."""
    from hertzforge.core.geometry import View

    name, geometry = _geometry(project, params)
    eye = tuple(params[f'eye_{a}'] for a in 'xyz')
    up = tuple(params[f'up_{a}'] for a in 'xyz')
    view = None if not any(eye) else _call(lambda: View(eye=eye, up=up))
    project.set_view(name, view)
    return [_drawn(name, geometry)[0]]


OPERATIONS = (
    Operation(
        id='project.new_geometry', title='New Geometry', workspace='geometry',
        summary='An empty geometry to build a part in from solids, in the display units '
                'unless a length unit is named.',
        params=(GEOMETRY_NAME, UNIT), outputs=('reading',), run=new_geometry),
    Operation(
        id='project.solids', title='Solids', workspace='geometry',
        summary='A geometry\'s solids drawn.',
        params=(GEOMETRY,), outputs=('shape3d',), run=view_solids),
    Operation(
        id='project.add_solid_block', title='Add Block', workspace='geometry',
        summary='A block, its widths along its own axes, centered and turned; a corner radius '
                'rounds its four edges along its own Z.',
        params=(GEOMETRY, SOLID_ID, *CENTER, *WIDTHS, *ANGLES, CORNER, NAME),
        outputs=('reading', 'shape3d'), run=add_solid_block, edits=True, previews=True),
    Operation(
        id='project.add_solid_cylinder', title='Add Cylinder', workspace='geometry',
        summary='A cylinder of a radius about its own Z and a length along it, centered and '
                'turned; subtracted from a part it makes a hole.',
        params=(GEOMETRY, SOLID_ID, *CENTER, _length('radius', 'Radius', 0.25, 0.0),
                _length('length', 'Length', 1.0, 0.0), *ANGLES, NAME),
        outputs=('reading', 'shape3d'), run=add_solid_cylinder, edits=True, previews=True),
    Operation(
        id='project.add_solid_cone', title='Add Cone', workspace='geometry',
        summary='A cone or frustum along its own Z, a radius at its base and its top (either '
                'zero for a point), centered and turned.',
        params=(GEOMETRY, SOLID_ID, *CENTER, _length('radius', 'Base radius', 0.5, 0.0),
                _length('top_radius', 'Top radius', 0.25, 0.0),
                _length('height', 'Height', 1.0, 0.0), *ANGLES, NAME),
        outputs=('reading', 'shape3d'), run=add_solid_cone, edits=True, previews=True),
    Operation(
        id='project.add_solid_sphere', title='Add Sphere', workspace='geometry', summary='A sphere of a radius about its center.',
        params=(GEOMETRY, SOLID_ID, *CENTER, _length('radius', 'Radius', 0.5, 0.0), NAME),
        outputs=('reading', 'shape3d'), run=add_solid_sphere, edits=True, previews=True),
    Operation(
        id='project.add_solid_section', title='Add Section', workspace='geometry',
        summary='A structural section along its own Z. Dimensions in order: I-beam and '
                'channel depth, flange width, flange thickness, web thickness; tee the same; '
                'angle long leg, short leg, thickness; rectangular tube width, height, wall; '
                'round tube outer diameter, wall. Empty for the shape\'s opening ones.',
        params=(GEOMETRY, SOLID_ID, Param('shape', 'Shape', 'enum', default='I-beam',
                                options=_section_shapes()),
                Param('dimensions', 'Dimensions [m], in order', 'string', default='',
                      length=True),
                _length('length', 'Length', 4.0, 0.0), *CENTER, *ANGLES, NAME),
        outputs=('reading', 'shape3d'), run=add_solid_section, edits=True, previews=True),
    Operation(
        id='project.add_solid_plate', title='Add Plate', workspace='geometry',
        summary='A flat rectangle in its own XY plane, centered and turned; it is meshed '
                'into shells, whose element group takes the thickness.',
        params=(GEOMETRY, SOLID_ID, *CENTER, *WIDTHS[:2], *ANGLES, NAME),
        outputs=('reading', 'shape3d'), run=add_solid_plate, edits=True, previews=True),
    Operation(
        id='project.add_solid_disk', title='Add Disk', workspace='geometry',
        summary='A flat circle in its own XY plane, centered and turned; it is meshed into '
                'shells, whose element group takes the thickness.',
        params=(GEOMETRY, SOLID_ID, *CENTER, _length('radius', 'Radius', 0.5, 0.0), *ANGLES, NAME),
        outputs=('reading', 'shape3d'), run=add_solid_disk, edits=True, previews=True),
    Operation(
        id='project.add_solid_hole', title='Add Hole', workspace='geometry',
        summary='A hole entering at its center and running down its own Z, a depth of 0 '
                'through the part, a counterbore diameter of 0 none; subtract it from a part.',
        params=(GEOMETRY, *CENTER, _length('diameter', 'Diameter', 0.25, 0.0),
                _length('depth', 'Depth (0 through)', 0.0, 0.0),
                _length('counterbore_diameter', 'Counterbore diameter (0 none)', 0.0, 0.0),
                _length('counterbore_depth', 'Counterbore depth', 0.0, 0.0), *ANGLES, NAME),
        outputs=('reading', 'shape3d'), run=add_solid_hole, edits=True, previews=True),
    Operation(
        id='project.edit_solid', title='Edit Solid', workspace='geometry',
        summary='Change a solid after it is made: only the fields typed change, lists as '
                '"0, 0, 0.1". A boolean it is part of changes with it; Mesh Solids again to '
                'mesh the change.',
        params=(GEOMETRY, SOLID, *EDIT_FIELDS), outputs=('reading', 'shape3d'),
        run=edit_solid, edits=True, previews=True),
    Operation(
        id='project.move_solids', title='Move Part', workspace='geometry',
        summary='Move parts whole, each with every solid it is made of: turned about the '
                'middle of their extent, then moved.',
        params=(GEOMETRY, SOLIDS, *OFFSET, *TURN), outputs=('reading', 'shape3d'),
        run=move_solids, edits=True,
        edits_when=lambda params: any(params[p.name] for p in (*OFFSET, *TURN))),
    Operation(
        id='project.copy_solids', title='Copy Solids', workspace='geometry',
        summary='Copy solids, recipe and all, into the same geometry or another; a boolean '
                'or pattern comes with every solid it is made of.',
        params=(GEOMETRY, SOLIDS, INTO), outputs=('reading', 'shape3d'),
        run=copy_solids, edits=True),
    Operation(
        id='project.reorder_solids', title='Reorder Solids', workspace='geometry',
        summary='Put the parts, or one boolean\'s solids, in a new order, naming every one; '
                'a subtraction cuts the rest from the first.',
        params=(GEOMETRY, SOLIDS, WITHIN), outputs=('reading', 'shape3d'),
        run=reorder_solids, edits=True),
    Operation(
        id='project.combine_solids', title='Combine Solids', workspace='geometry',
        summary='Make two or more parts one: their union, the rest subtracted from the '
                'first, or their intersection.',
        params=(GEOMETRY, OPERATION, SOLIDS, NAME), outputs=('reading', 'shape3d'),
        run=combine_solids, edits=True),
    Operation(
        id='project.separate_solids', title='Take Apart', workspace='geometry',
        summary='Undo booleans or patterns: the solids they were made of stand on their own '
                'again.',
        params=(GEOMETRY, SOLIDS), outputs=('reading', 'shape3d'),
        run=separate_solids, edits=True),
    Operation(
        id='project.delete_solids', title='Delete Solids', workspace='geometry',
        summary='Delete solids, each with every solid it is made of; a mesh made from them '
                'stays.',
        params=(GEOMETRY, SOLIDS), outputs=('reading', 'shape3d'),
        run=delete_solids, edits=True),
    Operation(
        id='project.pattern_solids', title='Pattern', workspace='geometry',
        summary='Copy parts in rows, columns and layers along X, Y and Z, the originals '
                'counted, as one solid whose copies follow them.',
        params=(GEOMETRY, SOLIDS, *COUNTS, *SPACING), outputs=('reading', 'shape3d'),
        run=pattern_solids, edits=True, previews=True),
    Operation(
        id='project.mesh_solids', title='Mesh Solids', workspace='geometry',
        summary='Mesh parts into hexes, tetras or shells, each part an element group. The size '
                'sets the time: a fine size is slow.',
        params=(GEOMETRY, PARTS, ELEMENTS, SIZE, PIECE_SIZES, DETECT_SIZES, APART, PARTS_APART,
                ANYWAY),
        outputs=('reading', 'shape3d'), run=mesh_solids, edits=True),
    Operation(
        id='project.mate_surfaces', title='Mate Surfaces', workspace='geometry',
        summary='Join two element groups where their surfaces mate, or edit a kept mate: the '
                'nodes of each side picked in the view, and the normal and the shear held '
                'rigidly, by a stiffness or not at all. Preview shows it; Apply keeps it.',
        params=(GEOMETRY, MATE_PICKED, GROUPS, MATE_TOLERANCE, PATCH, MATE_NAME, NORMAL,
                NORMAL_STIFFNESS, SHEAR, SHEAR_STIFFNESS, MATE_SIDE, NODES_A, NODES_B,
                NODES_FOR, MATE_NODE, MATE_KEEP),
        outputs=('reading', 'table', 'shape3d', 'table'), run=mate_surfaces, edits=True,
        edits_when=lambda params: params['keep'], previews=True, commits='keep'),
    Operation(
        id='project.delete_mated_surfaces', title='Delete Mated Surfaces', workspace='geometry', summary='Delete kept sets of mated surfaces, by id.',
        params=(GEOMETRY, MATES), outputs=('reading', 'table'), run=delete_mated_surfaces,
        edits=True),
    Operation(
        id='project.set_view', title='Set Default View', workspace='geometry',
        summary='The view a geometry opens on: where the eye is, seen from the model\'s '
                'center, and which way is up. An eye of 0, 0, 0 is the default isometric view.',
        params=(GEOMETRY, *EYE, *UP), outputs=('reading', 'shape3d'), run=set_view,
        edits=True),
)

OPERATIONS = tuple(replace(op, says_lengths=True) for op in OPERATIONS)
