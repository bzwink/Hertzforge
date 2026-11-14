"""A run folder becomes an ordinary project file.

`convert` reads a run folder (`hertzforge.io.run_folder`) and writes a
`.hforge` project every analysis tool opens with `Project.open`, by
default `<run name>.hforge` inside the folder. The control service's
Random, MIMO Random, Sine and Classical Shock modules and its Recorder
write a run folder for every run; the other control modules do not yet.
This is the analysis side of that bridge.

The project holds, in this order:

- the channel table as armed, a `ChannelTable` with its controller
  columns (`channel_table_from_rows`, the one mapping between a control
  channel row and the analysis table, used both ways);
- a `TimeHistory` per stream, in SI with the declared unit kept, its
  roles (a drive is a reference), the run's source label, the averaging
  the setup names, and a run identity whose source is the controller.
  A recording's time is measured from its trigger, so its pre-trigger
  samples are at negative times; a pulse module's stream is one record
  per shot, timed from its firing, with a history for each kind of shot
  (a Classical Shock run's identification, its pulses). The samples are
  copied a block at a time, float32 as recorded
  (`native.StreamedHistory`);
- the `Specification` a Random run was controlled to, from the setup's
  resolved profile, with its alarm and abort bands as warning and abort
  limits; for a Sine sweep or dwell, the `SineSweepSpecification` it was
  controlled to, one tone over the control channels, from the profile
  as the service resolved it on the run's band (`results/reference`),
  with its alarm and abort bands as the tone's warning and abort limits;
  for a MIMO Random run, the reference spectral density matrix it was
  controlled to (`results/reference`, kind `sdm_reference`), a
  `Specification` with its cross terms, each controlled channel's
  alarm and abort lines as its warning and abort limits (a reference
  in virtual degrees of freedom, which are not channels, stays in the
  run record as arrays);
- for a Sine sweep or step test, a `SineLevelSet` of the inputs' tracked
  levels (a result of kind `sine_levels`): the last one-way sweep at the
  highest level the run reached, as one `SineLevel` of that tone over
  every enabled input, complex (the tracked amplitude at the phase the
  run measured against the drive), with the time each line was read;
  every point of every pass stays in the run record as arrays;
- a `Psd`, `Frf`, `Coherence` or `MultipleCoherence` per result of kind
  `psd`, `cpsd`, `frf` or `coherence`, converted to SI, cross terms as
  written (the control service writes them in the file's convention,
  conj(X) Y, so nothing is conjugated here);
- an `Srs` per result of kind `srs` (a record per pulse), and a
  `ShockSpecification` per result of kind `srs_reference`, its band as
  the warning limits, in SI at the result's Q;
- the `RunRecord`: the manifest, the table and setup as written, the
  law, the log, the end, each stream's and result's description, the
  arrays of results no analysis class holds, and what the folder
  lacked.

All of them are linked in one object group named for the run. A gap in
a stream is not padded: the samples after it follow on, and each record
of the history says where the gap was and how many frames it lost.

The file is written atomically (a failed conversion leaves no partial
file), with no timestamps and in a fixed order, so one folder converted
twice gives the same bytes.
"""

from __future__ import annotations

import datetime as _dt
import functools
import os
from collections.abc import Callable, Iterator, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any

import numpy as np

from . import native, run_folder
from .rattlesnake import _stream_offsets, _unit_scale

#: the project type each test is a run of; a test not here sets none
PROJECT_TYPES = {'random': 'Random Vibration', 'mimo_random': 'Random Vibration',
                 'sine': 'Sine Sweep', 'classical_shock': 'Shock', 'srs': 'Shock',
                 'mimo_shock': 'Shock', 'twr': 'Transient'}

#: the result kinds that become an analysis object; any other is kept in
#: the run record as named arrays
ANALYSIS_KINDS = ('psd', 'cpsd', 'frf', 'coherence', 'srs', 'srs_reference', 'sine_levels',
                  'sdm_reference')

#: the arrays of a result kind its analysis object holds (its abscissa
#: besides); the rest are kept in the run record
_HELD_ARRAYS = {'srs_reference': ('reference', 'band_low', 'band_high'),
                'sdm_reference': ('alarm_upper', 'alarm_lower', 'abort_upper', 'abort_lower')}

#: a MIMO Random reference's limit arrays, by the limit each becomes
_SDM_LIMITS = {'warning_upper': 'alarm_upper', 'warning_lower': 'alarm_lower',
               'abort_upper': 'abort_upper', 'abort_lower': 'abort_lower'}

#: the name of the one tone a Sine run's specification and levels hold
SINE_TONE = 'Tone 1'


@dataclass
class Conversion:
    """What `convert` wrote: the file, the objects in it by name, and
    what the run lacked, in words."""

    path: str
    objects: list[str] = field(default_factory=list)
    lacked: list[str] = field(default_factory=list)


# ---- the channel table: one mapping, both ways -----------------------------

def _number(value: Any) -> Any:
    return '' if value is None else value


def channel_table_from_rows(rows: Sequence[Mapping[str, Any]]) -> Any:
    """The `ChannelTable` a control channel table's rows describe.

    Each row is in the protocol's spelling (`index`, `enabled`,
    `description`, `role`, `physical`, `interface`, `iepe_bias_v`,
    `units`, `sensitivity_mv_per_eu`, `range_max`, `polarity`,
    `control_group`, `abort`), with any further columns as text under
    `extra` (node, direction, serial number, make, model and the rest,
    by any name the table reads). The row's own fields win over an
    `extra` of the same column. A channel's loop role is three columns:
    a drive is `drive`, a control channel `control`, and a reference a
    `role` of reference. Its analysis role is the `extra` role where it
    gives one (a monitor is never a reference), else reference for a
    drive or a reference and response for the rest. A channel with no
    node is given its channel number as its node, a node-only identity,
    so no two channels share a DOF.

    Parameters
    ----------
    rows : sequence of mapping
        The control table's rows.

    Returns
    -------
    ChannelTable
        The table, with the controller's columns.
    """
    from ..core.channel_table import ChannelTable, canonical_name

    columns: dict[str, list[Any]] = {}
    count = len(rows)

    def put(name: str, k: int, value: Any) -> None:
        columns.setdefault(name, [''] * count)[k] = value

    for k, row in enumerate(rows):
        extra = {canonical_name(str(key)): value
                 for key, value in (row.get('extra') or {}).items()}
        for name, value in extra.items():
            put(name, k, '' if value is None else value)
        role = str(row.get('role', 'monitor'))
        stated = str(extra.get('role', '') or '').strip().lower()
        if role == 'reference':
            analysis = 'reference'
        elif stated in ChannelTable.ROLES and not (role == 'monitor'
                                                    and stated == 'reference'):
            analysis = stated
        else:
            analysis = 'reference' if role == 'drive' else 'response'
        physical = row.get('physical')
        abort = row.get('abort') or {}
        node = str(extra.get('node', '') or '').strip()
        put('channel', k, int(row['index']))
        put('node', k, node or str(int(row['index'])))
        put('direction', k, str(extra.get('direction', '') or ''))
        put('comment', k, str(row.get('description', '') or ''))
        put('role', k, analysis)
        put('control', k, role == 'control')
        put('drive', k, role == 'drive')
        put('enabled', k, bool(row.get('enabled', True)))
        put('physical', k, (f'{physical["device"]}/{physical["channel"]}'
                            if isinstance(physical, Mapping) else ''))
        put('iepe', k, row.get('interface') == 'iepe')
        put('iepe_bias', k, _number(row.get('iepe_bias_v')))
        put('unit', k, str(row.get('units', '') or ''))
        put('sensitivity', k, _number(row.get('sensitivity_mv_per_eu')))
        put('range_max', k, _number(row.get('range_max')))
        put('polarity', k, str(row.get('polarity', '') or ''))
        put('control_group', k, _number(row.get('control_group')))
        put('abort_accel_g', k, _number(abort.get('accel_g')))
        put('abort_velocity_ips', k, _number(abort.get('velocity_ips')))
        put('abort_displacement_mil', k, _number(abort.get('displacement_mil')))
    if not rows:
        columns = {name: [] for name in ('channel', 'node', 'direction', 'unit',
                                         *ChannelTable.CONTROLLER)}
    return ChannelTable(columns)


def rows_from_channel_table(table: Any) -> list[dict[str, Any]]:
    """The control channel rows a `ChannelTable` with controller columns
    describes: `channel_table_from_rows` the other way.

    A cell the table leaves blank takes a new row's value (a drive in V
    at 1000 mV/V and 10 V full scale, any other channel in g at 100 mV/g
    and 100 g). The measurement's columns other than the row's own
    (node, direction, serial number and the rest) go under `extra`.

    Parameters
    ----------
    table : ChannelTable
        A table carrying the controller's columns.

    Returns
    -------
    list of dict
        One row per channel, in the protocol's spelling.

    Raises
    ------
    ValueError
        If the table has no controller columns.
    """
    if not table.has_controller_settings:
        raise ValueError('the table has no controller columns; convert it to carry them first')

    def cell(name: str, k: int) -> str:
        return str(table[name][k]).strip()

    def number(name: str, k: int, default: Any = None) -> Any:
        text = cell(name, k)
        return default if not text else (int(text) if name == 'control_group'
                                         else float(text))

    own = {'channel', 'comment', 'role', 'control', 'drive', 'unit', 'sensitivity',
           'channel_type', *table.CONTROLLER}
    out = []
    for k in range(table.num_channels):
        if cell('drive', k) == 'True':
            role = 'drive'
        elif cell('control', k) == 'True':
            role = 'control'
        elif cell('role', k) == 'reference':
            role = 'reference'
        else:
            role = 'monitor'
        drive = role == 'drive'
        physical = cell('physical', k)
        device, slash, channel = physical.partition('/')
        out.append({
            'index': int(table['channel'][k]),
            'enabled': cell('enabled', k) != 'False',
            'description': cell('comment', k),
            'role': role,
            'physical': ({'device': device, 'channel': channel}
                         if slash and device and channel else None),
            'interface': 'iepe' if cell('iepe', k) == 'True' else 'single_ended',
            'iepe_bias_v': number('iepe_bias', k),
            'units': cell('unit', k) or ('V' if drive else 'g'),
            'sensitivity_mv_per_eu': number('sensitivity', k, 1000.0 if drive else 100.0),
            'range_max': number('range_max', k, 10.0 if drive else 100.0),
            'polarity': 'negative' if cell('polarity', k) == 'negative' else 'positive',
            'control_group': number('control_group', k),
            'abort': {'accel_g': number('abort_accel_g', k),
                      'velocity_ips': number('abort_velocity_ips', k),
                      'displacement_mil': number('abort_displacement_mil', k)},
            'extra': {name: cell(name, k) for name in table.column_names
                      if name not in own and cell(name, k)},
        })
    return out


# ---- streams -----------------------------------------------------------

def _start_text(manifest: Mapping[str, Any]) -> str:
    start = manifest.get('start') or {}
    unix = start.get('unix') if isinstance(start, Mapping) else None
    if not isinstance(unix, (int, float)):
        return ''
    zone = _dt.timezone(_dt.timedelta(seconds=float(start.get('utc_offset_s') or 0)))
    return _dt.datetime.fromtimestamp(float(unix), zone).isoformat()


def _identity(run: run_folder.RunFolder) -> Any:
    from ..core.runs import FROM_CONTROLLER, RunIdentity, entered

    values = entered(run.name, run=str(run.manifest.get('run_id', '') or '') or None,
                     start=_start_text(run.manifest) or None)
    known = {name: value for name, value in values.items() if value is not None}
    return RunIdentity(**known, sources=tuple((name, FROM_CONTROLLER) for name in known))


def _averaging(setup: Mapping[str, Any], samples: int, rate: float,
               lacked: list[str], stream: str) -> Any:
    """The averaging the setup names: a continuous test's FFT length and
    averages per loop (`acquisition`), or the Recorder's analysis block
    (`analysis`), as many frames as the setup says and the record holds."""
    from ..core.averaging import Averaging

    config = setup.get('config') or {}
    acquisition = config.get('acquisition') or {}
    analysis = config.get('analysis') or {}
    derived = (setup.get('derived') or {}).get('analysis') or {}
    if 'n_pts' in acquisition:
        length, frames = int(acquisition['n_pts']), int(acquisition.get('n_avg') or 1)
        window = str(acquisition.get('window') or 'hann')
        overlap = float(acquisition.get('overlap_pct') or 0) / 100.0
    elif 'block' in analysis:
        length = int(analysis['block'])
        window = str(analysis.get('window') or 'hann')
        overlap = float(analysis.get('overlap_pct', derived.get('overlap_pct')) or 0) / 100.0
        span = derived.get('averages_s')
        hop = max(1, round(length * (1.0 - overlap)))
        frames = (1 + max(0, round(float(span) * rate) - length) // hop
                  if isinstance(span, (int, float)) else 1)
    else:
        return None
    try:
        averaging = Averaging(frame_length=length, overlap=overlap, window=window,
                              frames=max(1, frames))
    except ValueError as error:
        lacked.append(f'the averaging the setup names for {stream!r} is not one the '
                      f'analysis holds ({error}), so it is left unset')
        return None
    room = averaging.most_frames(samples, rate)
    if room == 0:
        return None
    from dataclasses import replace

    return replace(averaging, frames=min(averaging.frames, room))


def _gap_note(stream: run_folder.Stream) -> str:
    gaps = [(int(g.get('at_frame', 0)), int(g.get('dropped_frames', 0)))
            for g in stream.description.get('gaps') or [] if isinstance(g, Mapping)]
    if not gaps:
        return ''
    said = ', '.join(f'{lost} frames at sample {at}' for at, lost in gaps)
    return (f'gaps in the recording, not padded (the samples after each follow on): '
            f'{said}')


class _Copy:
    """How one stream's samples are copied: read a block of frames, put
    them in records by channel, scale them to SI. Its methods are what a
    `native.StreamedHistory` calls for its blocks."""

    def __init__(self, stream: run_folder.Stream, scales: np.ndarray,
                 offsets: np.ndarray, frames_per_block: int,
                 tick: Callable[[int], None]) -> None:
        self.stream: run_folder.Stream = stream
        self.scales: np.ndarray = scales[:, np.newaxis]
        self.offsets: np.ndarray = offsets[:, np.newaxis]
        self.frames_per_block: int = frames_per_block
        self.tick: Callable[[int], None] = tick

    def _scaled(self, values: np.ndarray) -> np.ndarray:
        # in place after the one copy, so a block costs its float64 copy
        # and no temporaries beside it
        out = values.T.astype(np.float64)
        out *= self.scales
        out += self.offsets
        return out

    def continuous(self) -> Iterator[native.StreamBlock]:
        for first, values in self.stream.blocks(self.frames_per_block):
            yield slice(None), first, self._scaled(values)
            self.tick(len(values))

    def shots(self, spans: Sequence[tuple[int, int]]) -> Iterator[native.StreamBlock]:
        """Each shot's frames as a column of records: shot `s` of `count`
        is record `c * count + s` of channel `c`."""
        width, count = len(self.stream.channels), len(spans)
        with open(self.stream.path, 'rb') as f:
            for s, (at, frames) in enumerate(spans):
                for first in range(0, frames, self.frames_per_block):
                    n = min(self.frames_per_block, frames - first)
                    f.seek((at + first) * width * run_folder.SAMPLE_BYTES)
                    values = np.fromfile(f, dtype='<f4', count=n * width)
                    yield ([c * count + s for c in range(width)], first,
                           self._scaled(values.reshape(n, width)))
                    self.tick(n)


def _first_instant(run: run_folder.RunFolder, stream: run_folder.Stream) -> float:
    """Where a stream's first sample is, in seconds: a recording's from its
    trigger (negative before it), any other stream's from the run's start
    frame, or 0 when the frames are not stated."""
    side = stream.description
    first = side.get('first_station_frame')
    if stream.kind == 'recording':
        trigger = side.get('trigger_station_frame')
        if isinstance(first, int) and isinstance(trigger, int):
            return (first - trigger) / stream.rate
        if isinstance(side.get('pre_frames'), int):
            return -side['pre_frames'] / stream.rate
        return 0.0
    start = (run.manifest.get('start') or {}).get('station_frame')
    if isinstance(first, int) and isinstance(start, int):
        return (first - start) / stream.rate
    return 0.0


def _header(run: run_folder.RunFolder, dofs: Sequence[str], dims: Sequence[str],
            units: Sequence[str | None], roles: Mapping[tuple[str, str], str],
            identity: Any, comments: list[str], block: list[str] | None,
            repeat: int) -> Any:
    """A time history with every field a stream's carries and no samples
    yet: `repeat` records per channel, channel by channel."""
    from ..core.data import TimeHistory

    history = TimeHistory(
        abscissa=np.zeros(0), ordinate=np.zeros((len(dofs) * repeat, 0)),
        response_dof=[d for d in dofs for _ in range(repeat)],
        ordinate_dim=[d for d in dims for _ in range(repeat)],
        ordinate_unit=[u for u in units for _ in range(repeat)],
        comment=comments, block=block)
    history.roles = dict(roles)
    history.source = run.source
    history.run_identity = identity
    return history


def _shot_spans(stream: run_folder.Stream
                ) -> list[tuple[int, int, float, Any, Mapping[str, Any]]]:
    """Each shot of a pulse stream as (first frame, frames, time of its
    first sample from its firing, level, the shot as described). A shot's
    `pre_frames`, the frames kept before it fired, put its first sample
    at a negative time; without it the first sample is the firing."""
    spans = []
    for shot in stream.description.get('shots') or []:
        if not isinstance(shot, Mapping):
            continue
        at = int(shot.get('first_frame', 0))
        frames = max(0, min(int(shot.get('frames', 0)), stream.frames - at))
        pre = shot.get('pre_frames')
        t0 = -pre / stream.rate if isinstance(pre, int) else 0.0
        spans.append((at, frames, t0, shot.get('level_db'), shot))
    return spans


def _shot_label(k: int, shot: Mapping[str, Any]) -> str:
    """A shot's record label: its pulse ('pulse 3') where it names one,
    else its place in the stream ('shot 1')."""
    pulse = shot.get('pulse')
    return f'pulse {pulse + 1}' if isinstance(pulse, int) else f'shot {k + 1}'


def _shot_groups(spans: Sequence[tuple[int, int, float, Any, Mapping[str, Any]]]
                 ) -> list[list[int]]:
    """The shots that make one history: those that share a length, a
    place relative to their firing and what they were (an identification
    is not a pulse), in the order the first of each was fired."""
    groups: dict[tuple[int, float, str], list[int]] = {}
    for k, (_a, frames, t0, _l, shot) in enumerate(spans):
        groups.setdefault((frames, t0, str(shot.get('what') or '')), []).append(k)
    return list(groups.values())


def _histories(run: run_folder.RunFolder, table: Any, lacked: list[str],
               progress: Callable[[int, int], None] | None
               ) -> list[tuple[str, native.StreamedHistory]]:
    """A `native.StreamedHistory` per stream, or per shot of a pulse
    stream whose shots differ in length or timing."""
    from ..core.runs import level_text

    rows = {int(i): k for k, i in enumerate(table['channel'])}
    dofs = table.dof_strings()
    table_roles = table.roles()
    identity = _identity(run)
    total = sum(s.frames for s in run.streams.values())
    done = [0]

    def tick(frames: int) -> None:
        done[0] += frames
        if progress is not None:
            progress(done[0], total)

    out = []
    for stream in run.streams.values():
        unknown = [i for i in stream.channels if i not in rows]
        if unknown:
            raise run_folder.RunFolderError(
                f'the stream {stream.name!r} names channels {unknown} the table does not')
        width = len(stream.channels)
        frames_per_block = max(1, native.STREAM_BLOCK_BYTES
                               // (run_folder.SAMPLE_BYTES * width))
        scale_dims = [_unit_scale(unit) for unit in stream.units]
        for i, unit, (_s, _d, read) in zip(stream.channels, stream.units, scale_dims):
            if read is None:
                lacked.append(f'channel {i} of {stream.name!r} is in {unit!r}, which '
                              'is not a unit the analysis reads, so it is unit-less')
        dofs_here = [dofs[rows[i]] for i in stream.channels]
        dims = [d for _s, d, _u in scale_dims]
        units = [u for _s, _d, u in scale_dims]
        roles = {(dof, dim): ('reference' if stream.kind == 'drives'
                              else table_roles[rows[i]] or 'response')
                 for i, dof, dim in zip(stream.channels, dofs_here, dims)}
        copy = _Copy(stream, np.array([s for s, _d, _u in scale_dims]),
                     _stream_offsets(units), frames_per_block, tick)
        note = _gap_note(stream)
        spans = _shot_spans(stream)
        if not spans:
            history = _header(run, dofs_here, dims, units, roles, identity,
                              [note] * width, None, 1)
            history.averaging = _averaging(run.setup, stream.frames, stream.rate, lacked,
                                           stream.name)
            out.append((stream.name, native.StreamedHistory(
                history, stream.frames, _first_instant(run, stream), stream.rate,
                copy.continuous)))
            continue
        # a pulse module's stream: one record per shot for the shots that
        # share a length, a place relative to their firing and what they
        # were, one history for each such group
        groups = _shot_groups(spans)
        whats = [str(spans[g[0]][4].get('what') or '') for g in groups]
        for members, what in zip(groups, whats, strict=True):
            labels = [_shot_label(k, spans[k][4]) for k in members]
            said = [label + (f' ({what})' if what and what != 'pulse' else '')
                    + (f' at {level_text(float(spans[k][3]))}'
                       if isinstance(spans[k][3], (int, float)) else '')
                    for k, label in zip(members, labels, strict=True)]
            comments = ['; '.join(part for part in (shot, note) if part)
                        for _c in range(width) for shot in said]
            history = _header(run, dofs_here, dims, units, roles, identity, comments,
                              [label for _c in range(width) for label in labels],
                              len(members))
            if len(groups) == 1:
                name = stream.name
            elif what and whats.count(what) == 1:
                name = f'{stream.name} {what}'
            else:
                name = f'{stream.name} {labels[0]}'
            out.append((name, native.StreamedHistory(
                history, spans[members[0]][1], spans[members[0]][2], stream.rate,
                functools.partial(copy.shots, [spans[k][:2] for k in members]))))
    return out


# ---- the specification -------------------------------------------------

def _specification(run: run_folder.RunFolder, table: Any, lacked: list[str]) -> Any:
    """The Random profile as resolved, as the analysis holds a target: a
    `Specification` on the control channels, in SI, its alarm band as the
    warning limits and its abort band as the abort limits. A row's bands
    hold from that row to the next (`SpecificationDraft` segments)."""
    from ..core.author import SpecificationDraft
    from ..units import si_transform

    profile = run.setup.get('resolved', {}).get('profile') if isinstance(
        run.setup.get('resolved'), Mapping) else None
    if not isinstance(profile, Mapping) or not profile.get('breakpoints'):
        if run.test == 'random':
            lacked.append('the setup has no resolved profile, so the run has no specification')
        return None
    points = [p for p in profile['breakpoints'] if isinstance(p, Mapping)]
    try:
        frequencies = [float(p['freq_hz']) for p in points]
        levels = [float(p['psd_g2_hz']) for p in points]
    except (KeyError, TypeError, ValueError):
        lacked.append('the resolved profile does not give every breakpoint as freq_hz and '
                      'psd_g2_hz, so the run has no specification')
        return None
    controls = [k for k in range(table.num_channels)
                if str(table['control'][k]) == 'True' and str(table['enabled'][k]) != 'False']
    if not controls:
        lacked.append('no enabled control channel holds the profile, so the run has no '
                      'specification')
        return None
    scale = si_transform('g')[0] ** 2

    def band(name: str, default: Sequence[float]) -> list[tuple[float, float]]:
        # the protocol's [below, above] in positive dB, as the draft holds
        # a band: signed, below the target negative
        own = profile.get(name, default)
        return [(-float(below), float(above))
                for below, above in (p.get(name) or own for p in points[:-1])]

    warning, abort = band('alarm_db', (3.0, 3.0)), band('abort_db', (6.0, 6.0))
    dofs = table.dof_strings()
    draft = SpecificationDraft(
        channels=[dofs[k] for k in controls], dims=['acceleration'] * len(controls),
        frequencies=frequencies, levels=[[v * scale for v in levels] for _ in controls],
        bands=[{'warning': warning, 'abort': abort,
                'symmetric': all(lo == -hi for lo, hi in warning + abort),
                'uniform': len(set(warning)) == 1 and len(set(abort)) == 1}
               for _ in controls])
    spec = draft.make('the Random profile as the control service resolved it')
    spec.ordinate_unit = ['g'] * spec.num_records
    return spec


def _sine_tone_arrays(run: run_folder.RunFolder, lacked: list[str]) -> Any:
    """The profile as the service resolved it on the run's band, in SI:
    (frequencies, the target, the alarm and abort bands as
    {limit name: values}, the ordinate's dimension), or None."""
    reference = run.results.get('reference')
    if reference is None or 'frequency' not in reference.arrays:
        lacked.append('the run has no resolved profile (results/reference), so it has no '
                      'sine specification')
        return None
    units = {a.get('name'): str(a.get('unit') or '') for a in
             reference.description.get('arrays') or []}
    scale, dim, _unit = _unit_scale(units.get('reference', ''))
    frequencies = np.asarray(reference.arrays['frequency'], dtype=np.float64)
    names = {'warning_lower': 'alarm_low', 'warning_upper': 'alarm_high',
             'abort_lower': 'abort_low', 'abort_upper': 'abort_high'}
    limits = {limit: np.asarray(reference.arrays[array], dtype=np.float64) * scale
              for limit, array in names.items() if array in reference.arrays}
    target = np.asarray(reference.arrays['reference'], dtype=np.float64) * scale
    return frequencies, target, limits, dim


def _sine_specification(run: run_folder.RunFolder, table: Any, lacked: list[str]) -> Any:
    """The Sine profile as the analysis holds a requirement: one tone,
    asked of every enabled control channel, in SI. A sweep's tone is one
    pass across the band in the direction the run started, at the rate
    and on the law it swept at; a dwell's holds its frequency for its
    dwell. The tone's breakpoints are the points the service resolved
    the profile at (`results/reference`), so the interpolation between
    them follows the profile the run followed."""
    from ..core.sine import LINEAR, LOG, SineSweepSpecification, SineTone

    resolved = run.setup.get('resolved') if isinstance(run.setup.get('resolved'),
                                                       Mapping) else {}
    test = resolved.get('test') if isinstance(resolved.get('test'), Mapping) else {}
    kind = test.get('kind')
    if kind not in ('sweep', 'dwell'):
        lacked.append(f'a {kind or "Sine"} test\'s profile is no tone the analysis holds, so '
                      'the run has no sine specification; the profile stays in the setup')
        return None
    held = _sine_tone_arrays(run, lacked)
    if held is None:
        return None
    frequencies, target, limits, dim = held
    controls = [k for k in range(table.num_channels)
                if str(table['control'][k]) == 'True' and str(table['enabled'][k]) != 'False']
    if not controls:
        lacked.append('no enabled control channel holds the profile, so the run has no sine '
                      'specification')
        return None
    if dim != 'acceleration':
        lacked.append(f'the profile is in a unit of {dim!r}, not acceleration, so the run has '
                      'no sine specification')
        return None
    m = len(controls)

    def columns(values: np.ndarray) -> np.ndarray:
        return np.tile(np.asarray(values, dtype=np.float64)[:, np.newaxis], (1, m))

    if kind == 'dwell':
        dwell = test.get('dwell') or {}
        frequency = float(test['freq_hz'])
        seconds = (float(dwell['seconds']) if 'seconds' in dwell
                   else float(dwell.get('cycles', 0)) / frequency)
        tone = SineTone.dwell(SINE_TONE, frequency, target[:1].repeat(m), seconds,
                              **{k: v[:1].repeat(m) for k, v in limits.items()})
    else:
        rate = test.get('rate') or {}
        law = LOG if rate.get('law') == 'log' else LINEAR
        speed = float(rate['oct_per_min'] if law == LOG else rate['hz_per_s'])
        order = np.argsort(frequencies)
        if test.get('direction') == 'down':
            order, speed = order[::-1], -speed
        tone = SineTone(SINE_TONE, 0.0, frequencies[order], columns(target[order]),
                        [law] * (len(order) - 1), [speed] * (len(order) - 1),
                        **{k: columns(v[order]) for k, v in limits.items()})
    dofs = table.dof_strings()
    return SineSweepSpecification(
        [tone], [dofs[k] for k in controls], ordinate_dim='acceleration',
        ordinate_unit='m/s**2',
        comment='the Sine profile as the control service resolved it on the run\'s band')


def _sine_levels(result: run_folder.Result, table: Any, units: Mapping[int, str],
                 test: str | None) -> tuple[Any, str]:
    """The inputs' tracked levels as a `SineLevelSet`, or (None, why
    not): the last one-way sweep at the highest level the run reached,
    its lines where the tone moved (a hold and the stop's ramp, where it
    stood still, are left to the arrays)."""
    from ..core.sine import SineLevel, SineLevelSet

    if test not in ('sweep', 'step'):
        return None, (f'a {test or "Sine"} test\'s levels are against time, which no '
                      'analysis class holds')
    channels = [int(i) for i in (result.description.get('channels') or {}).get(
        'responses') or []]
    unknown = [i for i in channels if i not in units]
    if not channels or unknown:
        return None, 'it does not name its channels in the table'
    a = result.arrays
    flat = {name: np.asarray(a[name], dtype=np.float64).reshape(-1)
            for name in ('time', 'frequency', 'level_db', 'pass', 'direction') if name in a}
    if len(flat) < 5 or 'values' not in a or 'phase' not in a:
        return None, 'it does not hold the arrays a sine level is read from'
    # records first, then a channel's points: channels by points, in time order
    values = np.concatenate(list(np.asarray(a['values'], dtype=np.float64)), axis=-1)
    phase = np.concatenate(list(np.asarray(a['phase'], dtype=np.float64)), axis=-1)
    kept = np.isfinite(flat['time']) & np.isfinite(flat['frequency'])
    if test == 'sweep':
        # a sweep's lines are where the tone moved: not a hold, nor the stop's ramp
        kept &= flat['direction'] != 0
    if not kept.any():
        return None, 'it holds no point'
    level = np.where(kept, flat['level_db'], -np.inf)
    top = level.max()
    at_top = kept & (flat['level_db'] == top)
    last = flat['pass'][at_top].max()
    chosen = at_top & (flat['pass'] == last)
    order = np.argsort(flat['frequency'][chosen], kind='stable')
    scale_dims = [_unit_scale(units[i]) for i in channels]
    ordinate = np.array([values[c][chosen][order] * scale_dims[c][0]
                         * np.exp(1j * np.deg2rad(phase[c][chosen][order]))
                         for c in range(len(channels))])
    dofs = table.dof_strings()
    row = {int(i): k for k, i in enumerate(table['channel'])}
    seconds = flat['time'][chosen][order]
    said = f'pass {int(last) + 1} at {top:g} dB'
    level_object = SineLevel(
        abscissa=flat['frequency'][chosen][order], ordinate=ordinate,
        response_dof=[dofs[row[i]] for i in channels],
        ordinate_dim=[d for _s, d, _u in scale_dims],
        ordinate_unit=[u for _s, _d, u in scale_dims],
        comment=[f'{SINE_TONE} at channel {i}: the tracked level, {said}' for i in channels],
        tone=SINE_TONE, onset=float(seconds.min()), seconds=seconds)
    return SineLevelSet([level_object]), ''


# ---- results -----------------------------------------------------------

def _abscissa(result: run_folder.Result, lines: int) -> np.ndarray | None:
    from ..units import si_transform

    axis = result.description.get('abscissa') or {}
    unit = str(axis.get('unit') or 'Hz')
    if isinstance(axis.get('array'), str) and axis['array'] in result.arrays:
        values = np.asarray(result.arrays[axis['array']], dtype=np.float64)
        values = values[-1] if values.ndim > 1 else values
    elif 'start' in axis and 'step' in axis:
        values = float(axis['start']) + float(axis['step']) * np.arange(lines)
    else:
        return None
    return values * si_transform(unit)[0] if unit else values


def _analysis_object(result: run_folder.Result, table: Any,
                     units: Mapping[int, str]) -> tuple[Any, str]:
    """The analysis object a result is, or (None, why not)."""
    from ..core.data import Coherence, Frf, MultipleCoherence, Psd
    from ..units import UNKNOWN, si_transform

    channels = result.description.get('channels') or {}
    responses = [int(i) for i in channels.get('responses') or []]
    references = [int(i) for i in channels.get('references') or []] or None
    unknown = [i for i in responses + (references or []) if i not in units]
    if not responses or unknown:
        return None, ('it does not name its response channels in the table'
                      if not responses else f'it names channels {unknown} the table does not')
    values_name = 'values'
    if values_name not in result.arrays:
        return None, 'it holds no array named values'
    described = next((a for a in result.description.get('arrays') or []
                      if a.get('name') == values_name), {})
    values = np.asarray(result.arrays[values_name])
    repeated = result.records is not None
    stack = values if repeated else values[np.newaxis]
    lines = stack.shape[-1]
    abscissa = _abscissa(result, lines)
    if abscissa is None or len(abscissa) != lines:
        return None, 'its abscissa is not stated, or not as long as its values'
    dofs = table.dof_strings()
    row = {int(i): k for k, i in enumerate(table['channel'])}
    scale_dims = {i: _unit_scale(units[i]) for i in units}
    kind = result.kind
    pairs: list[tuple[int, int | None]]
    if kind == 'psd' and stack.ndim == 3:
        pairs = [(i, None) for i in responses]
        flat = stack
    elif kind in ('cpsd', 'frf') or (kind == 'coherence' and stack.ndim == 4):
        refs = references if references is not None else (responses if kind == 'cpsd'
                                                          else None)
        if refs is None:
            return None, 'it does not name its reference channels'
        if stack.shape[1:3] != (len(responses), len(refs)):
            return None, (f'its values are {stack.shape[1:]} and it names '
                          f'{len(responses)} responses by {len(refs)} references')
        pairs = [(i, j) for i in responses for j in refs]
        flat = stack.reshape(stack.shape[0], -1, lines)
    elif kind == 'coherence' and stack.ndim == 3:
        pairs = [(i, None) for i in responses]
        flat = stack
    else:
        return None, f'its values have {values.ndim} axes, which a {kind} result does not'
    if flat.shape[1] != len(pairs):
        return None, (f'its values hold {flat.shape[1]} curves and it names '
                      f'{len(pairs)} channels')
    expected = 1.0
    records, response_dof, reference_dof, dims, ordinate_units, reference_units = (
        [], [], [], [], [], [])
    known = all(scale_dims[i][1] != UNKNOWN for pair in pairs for i in pair if i is not None)
    for c, (i, j) in enumerate(pairs):
        si, di, ui = scale_dims[i]
        sj, dj, uj = scale_dims[j] if j is not None else (si, di, ui)
        if kind in ('psd', 'cpsd'):
            factor = si * sj
            dim = f'{di}**2/frequency' if (i == j or j is None) and di == dj else (
                f'{di}*{dj}/frequency')
            ref_unit = uj if j is not None and ui != uj else None
        elif kind == 'frf':
            factor, dim, ref_unit = si / sj, f'{di}/{dj}', uj
        else:
            factor, dim, ref_unit = 1.0, None, None
        if c == 0:
            expected = factor
        response_dof.append(dofs[row[i]])
        reference_dof.append(dofs[row[j]] if j is not None else None)
        dims.append(dim if known else UNKNOWN)
        ordinate_units.append(ui if known and kind != 'coherence' else None)
        reference_units.append(ref_unit if known else None)
        records.append(flat[:, c] * (factor if known else 1.0))
    if kind != 'coherence' and known:
        declared = str(described.get('unit') or '').replace('^', '**')
        try:
            stated = si_transform(declared)[0] if declared else None
        except Exception:  # noqa: BLE001 - an unreadable unit is reported, not raised
            stated = None
        first_i, first_j = pairs[0]
        if stated is None or not np.isclose(stated, expected, rtol=1e-9):
            return None, (f'its unit {described.get("unit")!r} is not the one channel '
                          f'{first_i}' + (f' against {first_j}' if first_j is not None
                                          else '') + ' gives')
    count = flat.shape[0]
    record_labels = [f'record {r.get("index", k)}' for k, r in enumerate(result.records)
                     ] if repeated else None
    # channel-major: each channel's records together, in record order
    ordinate = np.concatenate([np.asarray(r) for r in records], axis=0)
    expand = (lambda seq: [v for v in seq for _ in range(count)])
    kwargs: dict[str, Any] = {
        'abscissa': abscissa, 'ordinate': ordinate, 'response_dof': expand(response_dof),
        'comment': result.description.get('description', '') or result.name}
    if repeated:
        kwargs['block'] = [label for _ in pairs for label in record_labels]
    if kind == 'coherence':
        if pairs[0][1] is None:
            return MultipleCoherence(**kwargs), ''
        return Coherence(reference_dof=expand(reference_dof), **kwargs), ''
    kwargs.update(ordinate_dim=expand(dims), ordinate_unit=expand(ordinate_units),
                  reference_unit=expand(reference_units))
    if kind == 'frf':
        return Frf(reference_dof=expand(reference_dof), **kwargs), ''
    if pairs[0][1] is None:
        return Psd(**kwargs), ''
    return Psd(reference_dof=expand(reference_dof), **kwargs), ''


def _srs_object(result: run_folder.Result, table: Any,
                units: Mapping[int, str]) -> tuple[Any, str]:
    """An SRS result as the analysis holds it, or (None, why not): a
    result of kind `srs` an `Srs`, a record per pulse where it repeats; a
    result of kind `srs_reference` (a nominal SRS and its tolerance band)
    a `ShockSpecification` with the band as its warning limits. In SI,
    at the result's Q and type."""
    from ..core.data import ShockSpecification, Srs
    from ..units import UNKNOWN, si_transform

    channels = result.description.get('channels') or {}
    responses = [int(i) for i in channels.get('responses') or []]
    if len(responses) != 1 or responses[0] not in units:
        return None, 'it does not name one response channel the table holds'
    channel = responses[0]
    spec = result.kind == 'srs_reference'
    name = 'reference' if spec else 'values'
    if name not in result.arrays:
        return None, f'it holds no array named {name}'
    values = np.asarray(result.arrays[name], dtype=np.float64)
    repeated = result.records is not None and not spec
    stack = values.reshape(values.shape[0], -1) if repeated else values.reshape(1, -1)
    lines = stack.shape[-1]
    abscissa = _abscissa(result, lines)
    if abscissa is None or len(abscissa) != lines:
        return None, 'its abscissa is not stated, or not as long as its values'
    scale, dim, unit = _unit_scale(units[channel])
    known = dim != UNKNOWN
    described = next((a for a in result.description.get('arrays') or []
                      if a.get('name') == name), {})
    if known:
        declared = str(described.get('unit') or '').replace('^', '**')
        try:
            stated = si_transform(declared)[0] if declared else None
        except Exception:  # noqa: BLE001 - an unreadable unit is reported, not raised
            stated = None
        if stated is None or not np.isclose(stated, scale, rtol=1e-9):
            return None, (f'its unit {described.get("unit")!r} is not the one channel '
                          f'{channel} gives')
    factor = scale if known else 1.0
    dof = table.dof_strings()[{int(i): k for k, i in enumerate(table['channel'])}[channel]]
    count = stack.shape[0]
    kwargs: dict[str, Any] = {
        'abscissa': abscissa, 'ordinate': stack * factor, 'response_dof': [dof] * count,
        'ordinate_dim': [dim if known else UNKNOWN] * count,
        'ordinate_unit': [unit if known else None] * count,
        'comment': result.description.get('description', '') or result.name,
        'q': result.description.get('q'),
        'kind': str(result.description.get('srs_kind') or 'maximax')}
    if repeated:
        kwargs['block'] = [(f'pulse {r["pulse"] + 1}' if isinstance(r.get('pulse'), int)
                            else f'record {r.get("index", k)}')
                           for k, r in enumerate(result.records or [])]
    if not spec:
        return Srs(**kwargs), ''
    limits = {}
    for array, limit in (('band_low', 'warning_lower'), ('band_high', 'warning_upper')):
        if array in result.arrays:
            limits[limit] = np.asarray(result.arrays[array], dtype=np.float64).reshape(
                1, -1) * factor
    return ShockSpecification(**kwargs, **limits), ''


def _sdm_specification(result: run_folder.Result, table: Any,
                       units: Mapping[int, str]) -> tuple[Any, str]:
    """A MIMO Random reference (kind `sdm_reference`) as the analysis
    holds a target, or (None, why not): a `Specification` with a record
    per autospectrum and per cross term above the diagonal, in SI and in
    the file's convention as written, its controlled channels' alarm and
    abort lines (spectral densities) as the warning and abort limits of
    their autospectra; a cross term has no limit (NaN)."""
    from ..core.data import Specification
    from ..units import UNKNOWN

    channels = result.description.get('channels') or {}
    responses = [int(i) for i in channels.get('responses') or []]
    if not responses or any(i not in units for i in responses):
        return None, ('it names degrees of freedom, which are not channels of the table'
                      if channels.get('dofs') else
                      'it does not name its controlled channels in the table')
    values = np.asarray(result.arrays.get('values'))
    n = len(responses)
    if values.ndim != 3 or values.shape[:2] != (n, n):
        return None, f'its values are {values.shape}, not {n} by {n} by lines'
    lines = values.shape[-1]
    abscissa = _abscissa(result, lines)
    if abscissa is None or len(abscissa) != lines:
        return None, 'its abscissa is not stated, or not as long as its values'
    scales = [_unit_scale(units[i]) for i in responses]
    if any(s[1] == UNKNOWN for s in scales):
        return None, 'a controlled channel is in a unit the analysis does not read'
    dofs = table.dof_strings()
    row = {int(i): k for k, i in enumerate(table['channel'])}
    pairs = [(a, b) for a in range(n) for b in range(a, n)]
    ordinate = np.array([values[a, b] * scales[a][0] * scales[b][0] for a, b in pairs])
    limits = {}
    for limit, array in _SDM_LIMITS.items():
        if array not in result.arrays:
            continue
        given = np.asarray(result.arrays[array], dtype=np.float64)
        limits[limit] = np.array([given[a] * scales[a][0] ** 2 if a == b
                                  else np.full(lines, np.nan) for a, b in pairs])
    dims = [(f'{scales[a][1]}**2/frequency' if scales[a][1] == scales[b][1]
             else f'{scales[a][1]}*{scales[b][1]}/frequency') for a, b in pairs]
    spec = Specification(
        abscissa=abscissa, ordinate=ordinate,
        response_dof=[dofs[row[responses[a]]] for a, _b in pairs],
        reference_dof=[dofs[row[responses[b]]] for _a, b in pairs],
        ordinate_dim=dims, ordinate_unit=[scales[a][2] for a, _b in pairs],
        reference_unit=[scales[b][2] if scales[a][2] != scales[b][2] else None
                        for a, b in pairs],
        comment=result.description.get('description', '') or result.name, **limits)
    spec.interpolation = Specification.reading_of(abscissa)
    return spec, ''


# ---- the conversion ----------------------------------------------------

def default_output(folder: str | os.PathLike) -> str:
    """Where a run folder's project file goes unless told otherwise:
    `<run name>.hforge` inside the folder.

    Parameters
    ----------
    folder : str or os.PathLike
        The run folder.

    Returns
    -------
    str
        The path.
    """
    folder = os.path.abspath(str(folder))
    return os.path.join(folder, os.path.basename(os.path.normpath(folder)) + native.SUFFIX)


def _unique(name: str, taken: Mapping[str, Any]) -> str:
    if name not in taken:
        return name
    # numbered as a project numbers a clash (`Project.add`)
    k = 2
    while f'{name} ({k})' in taken:
        k += 1
    return f'{name} ({k})'


def convert(path: str | os.PathLike, output: str | os.PathLike | None = None,
            progress: Callable[[int, int], None] | None = None) -> Conversion:
    """Convert a run folder into a project file.

    Parameters
    ----------
    path : str or os.PathLike
        The run folder, or its `run.json`.
    output : str or os.PathLike, optional
        The file to write; `default_output` when omitted. `.hforge` is
        added when it is not there.
    progress : callable, optional
        Called as ``progress(frames copied, frames in every stream)``.

    Returns
    -------
    Conversion
        The file written, its objects, and what the run lacked.

    Raises
    ------
    RunFolderError
        For a folder `run_folder.read` refuses, or a stream naming a
        channel the table does not hold. Nothing is written then.
    """
    from ..core.run_record import RunRecord

    run = run_folder.read(path)
    lacked = list(run.missing)
    table = channel_table_from_rows(run.rows)
    objects: dict[str, Any] = {'Channel Table': table}
    for name, history in _histories(run, table, lacked, progress):
        objects[_unique(name, objects)] = history
    units = {int(row['index']): str(row.get('units', '') or '') for row in run.rows}
    if run.test == 'sine':
        spec = _sine_specification(run, table, lacked)
    elif run.test == 'mimo_random':
        spec = None
        reference = run.results.get('reference')
        if reference is None:
            lacked.append('the run has no reference (results/reference), so it has no '
                          'specification')
        elif reference.kind == 'sdm_reference':
            spec, why = _sdm_specification(reference, table, units)
            if spec is None:
                lacked.append(f'the reference is kept as arrays in the run record: {why}')
    else:
        spec = _specification(run, table, lacked)
    if spec is not None:
        objects[_unique('Specification', objects)] = spec
    descriptions: dict[str, dict[str, Any]] = {}
    unmapped: dict[str, dict[str, np.ndarray]] = {}
    for name, result in run.results.items():
        descriptions[name] = dict(result.description)
        if result.records is not None:
            descriptions[name]['records'] = result.records
        why = f'its kind {result.kind!r} has no analysis class'
        held = None
        if result.kind == 'sine_levels':
            test = ((run.setup.get('resolved') or {}).get('test') or {}).get('kind')
            held, why = _sine_levels(result, table, units, test)
            if held is not None:
                for level in held:
                    level.source = run.source
                # the set holds one pass; every point of every pass stays as arrays
                objects[_unique('Sine Levels', objects)] = held
                unmapped[name] = dict(result.arrays)
                continue
            unmapped[name] = dict(result.arrays)
            continue
        if result.kind == 'sdm_reference':
            # the run's own reference is the Specification above; another
            # (the reference after controls were excluded) is one of its own
            if name == 'reference' and spec is not None:
                extra = {k: v for k, v in result.arrays.items()
                         if k not in ('values', *_HELD_ARRAYS['sdm_reference'])}
                if extra:
                    unmapped[name] = extra
                continue
            if name == 'reference':
                unmapped[name] = dict(result.arrays)
                continue
            held, why = _sdm_specification(result, table, units)
        elif result.kind in ('srs', 'srs_reference'):
            held, why = _srs_object(result, table, units)
        elif result.kind in ANALYSIS_KINDS:
            held, why = _analysis_object(result, table, units)
        if held is None:
            if result.kind in ANALYSIS_KINDS:
                lacked.append(f'the result {name!r} is kept as arrays in the run record: {why}')
            unmapped[name] = dict(result.arrays)
            continue
        if result.kind != 'sdm_reference':
            # a reference is what the run was asked for, not a measurement
            held.source = run.source
        objects[_unique(name, objects)] = held
        abscissa = (result.description.get('abscissa') or {}).get('array')
        kept = ('values', abscissa, *_HELD_ARRAYS.get(result.kind, ()))
        extra = {k: v for k, v in result.arrays.items() if k not in kept}
        if extra:
            unmapped[name] = extra
    record = RunRecord(run.manifest, run.table, run.setup, log=run.log, end=run.end,
                       law=run.law, law_file=run.law_file,
                       streams={name: s.description for name, s in run.streams.items()},
                       results=descriptions, arrays=unmapped, missing=lacked)
    objects[_unique('Run Record', objects)] = record
    target = native.hforge_path(output if output is not None else default_output(run.path))
    title = str(run.manifest.get('title') or '') or run.name
    native.save_test(target, title, objects,
                     project_type=PROJECT_TYPES.get(run.test),
                     object_groups=[{'members': list(objects), 'role': None,
                                     'name': run.name}])
    return Conversion(target, list(objects), lacked)


def converted(path: str | os.PathLike) -> str:
    """The project file of the run folder `path` names, converting the
    folder when it has none yet, or when its manifest was written after
    the file (the run went on, or ended, after a conversion). A file
    converted since is opened as it is, edits and all.

    Parameters
    ----------
    path : str or os.PathLike
        The run folder, or its `run.json`.

    Returns
    -------
    str
        The project file, `default_output` of the folder.

    Raises
    ------
    RunFolderError
        For a folder `run_folder.read` refuses.
    """
    folder = run_folder.folder_of(path)
    if folder is None:
        raise run_folder.RunFolderError(f'{path} is not a run folder')
    target = default_output(folder)
    manifest = os.path.join(folder, run_folder.MANIFEST)
    if not os.path.isfile(target) or os.path.getmtime(manifest) > os.path.getmtime(target):
        convert(folder, target)
    return target
