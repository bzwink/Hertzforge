"""One data object as a `waterfall` view, for any operation that offers one.

The numbers are `hertzforge.drawing.viz.waterfall`'s, the arithmetic of
the 3-D data view, asked for values rather than log10
exponents because the shell takes the log itself. Values are SI, as the
service's other views are.
"""

from __future__ import annotations

from typing import Any

from . import views as v
from .descriptors import Param
from .protocol import refused

#: the components a complex frequency object can be drawn as, and their
#: labels; real data ignores the choice
COMPONENTS = {'magnitude': 'Magnitude', 'real': 'Real part',
              'imag': 'Imaginary part', 'phase': 'Phase'}
#: the waterfall's quantity, chosen in the view among the groups it lists;
#: empty is the largest group
QUANTITY = Param('quantity', 'Quantity', 'string', default='', in_view=True)


def quantity_key(key: tuple[str, str | None]) -> str:
    """A (dimension, hint) group as the `quantity` param names it."""
    dimension, hint = key
    return f'{dimension}|{hint or ""}'


def _group(data: Any, value: str) -> tuple[str, str | None] | None:
    """The group `value` names among `data`'s, or None for the largest:
    an empty value, or one the object does not have, which is a choice
    kept from another object and not a statement about this one."""
    from hertzforge.drawing.viz.waterfall import waterfall_groups

    for key, _records in waterfall_groups(data):
        if quantity_key(key) == value:
            return key
    return None


def unit_text(dimension: str) -> str | None:
    """A dimension's SI unit as the application writes it ('m/s²'), or
    None when it is undefined or a bare ratio ('1')."""
    import hertzforge as hz

    try:
        text = hz.SI.label_text(dimension) or None
    # a dimension the core cannot label is drawn without a unit, not refused
    except Exception:  # noqa: BLE001
        return None
    return None if text == '1' else text


def axes(data: Any, first: int, component: str,
         arrays: dict[str, Any]) -> tuple[dict, dict]:
    """The abscissa and value axes of `data`'s records as `waterfall_arrays`
    drew them, labeled by the first drawn record's quantity."""
    from hertzforge.core.data import TimeHistory
    from hertzforge.drawing.viz.waterfall import quantity_label
    from hertzforge.names import display_name

    signed = component != 'magnitude' and data.ordinate.dtype.kind == 'c'
    if isinstance(data, TimeHistory):
        label = quantity_label((data.ordinate_dim[first],
                                data.dimension_hint[first])).capitalize()
    else:
        label = display_name(type(data).__name__)
    unit = unit_text(data.ordinate_dim[first])
    if signed and component == 'phase':
        label, unit = f'{label} phase', 'deg'
    elif signed:
        label = f'{label}, {COMPONENTS[component].lower()}'
    x = v.axis(data.abscissa_dim.capitalize(), unit_text(data.abscissa_dim),
               'log' if arrays['log_abscissa'] else 'linear')
    z = v.axis(label, unit, 'log' if arrays['log_scaled'] else 'linear')
    return x, z


def waterfall_view(title: str, data: Any, page: int = 1,
                   component: str = 'magnitude',
                   quantity: str = '', records: list[int] | None = None) -> dict[str, Any]:
    """`data`'s records at their own depths, as one `waterfall` view.

    `page` is 1-based and lands on the last page when past it. A time
    history is peak-decimated to the point budget, a PSD keeps every tread
    of its steps, and a specification fills in along its power law.
    `component` applies to complex frequency data: a signed part or the
    phase reads on a linear axis. One quantity is drawn, the
    group `quantity` names (`quantity_key`) or else the largest; when the
    object has more than one, the view lists them as `quantities` for the
    shell to offer. `records` draws those alone (the sub-items picked in
    the tree). An object with no records is refused."""
    import hertzforge as hz
    from hertzforge.drawing.viz.waterfall import (
        drawn_span,
        quantity_label,
        waterfall_arrays,
        waterfall_groups,
    )

    arrays = waterfall_arrays(data, records, unit_system=hz.SI, exponents=False,
                              component=component, page=int(page) - 1,
                              quantity=_group(data, quantity))
    if not arrays['drawn']:
        raise refused(f'{title} has no records to draw.')
    first = arrays['drawn'][0]
    signed = component != 'magnitude' and data.ordinate.dtype.kind == 'c'
    x, z = axes(data, first, component, arrays)
    limits = getattr(data, 'ordinate_limits', None)
    pages = arrays['pages']
    view = v.waterfall(
        title, x, z, arrays['labels'], arrays['curves'],
        drawn_span(arrays['curves']),
        z_range=None if limits is None or arrays['log_scaled'] or signed
        else limits,
        page={'number': arrays['page'] + 1, 'count': pages,
              'records': arrays['of_quantity']} if pages > 1 else None)
    groups = waterfall_groups(data)
    if len(groups) > 1:
        view['quantities'] = [
            {'value': quantity_key(key), 'label': quantity_label(key).capitalize(),
             'records': len(records)} for key, records in groups]
        view['quantity'] = quantity_key((data.ordinate_dim[first],
                                         data.dimension_hint[first]))
    return view
