"""The run folder: one control run as the control service records it.

A run folder (schema `hertzforge-run/1`) is how a control run is meant
to reach the analysis side: one folder per run, valid at every moment,
needing no other file and no running service to be read. The control
service's Random, MIMO Random, Sine, Mixed Mode, Classical Shock, SRS
shock, TWR and Explorer modules and its Recorder write one for every run
(`docs/run-folder.md`); MIMO Shock does not yet.
`hertzforge.io.run_convert` turns a folder into an ordinary project
file. Its members:

- `run.json`, the manifest: the schema, the test, the run's identity,
  the operator's words, the source (`SIM` or `HARDWARE`, never absent),
  the station, the build, the start, the streams and results it
  declares, and `complete`;
- `table.json`, the channel table as armed, every row in the protocol's
  spelling for a channel row, with any further columns under `extra`;
- `setup.json`, the configuration as accepted (`config`) and what the
  service resolved it to (`resolved`);
- `law/`, the law's `law.json` and, for a law that is a file, its bytes;
- `log.jsonl`, the run's log, one JSON object per line;
- `streams/<name>/`, `telemetry.f32` (little-endian float32,
  frame-interleaved, engineering units, no header) and `stream.json`;
- `results/<name>.json` describing named arrays held in `<name>.bin`
  (little-endian; complex as real and imaginary pairs), with
  `<name>.jsonl` holding a line per record of a result that repeats;
- `end.json`, written once at a clean end.

`read` takes a folder whole, including one whose run did not end: a run
with `complete` false or no `end.json`, a stream whose length is what
its file holds (a frame cut short is left out), a log whose last line
was torn. What it lacked is listed (`RunFolder.missing`), never filled
in. A folder with no source label is refused, since whether a run was
simulated is never guessed, and so is a schema newer than this reader.
Members a reader does not know are ignored and kept.
"""

from __future__ import annotations

import json
import os
from collections.abc import Iterator, Mapping
from dataclasses import dataclass, field
from typing import Any

import numpy as np

#: the schema name every manifest names, before its number
SCHEMA_NAME = 'hertzforge-run'

#: the newest run folder schema this reader reads
SCHEMA_NUMBER = 1

#: the manifest file that makes a folder a run folder
MANIFEST = 'run.json'

#: the labels a run's source may have
SOURCES = ('SIM', 'HARDWARE')

#: the kinds of stream a run declares
STREAM_KINDS = ('inputs', 'drives', 'recording')

#: the bytes of one sample in a stream
SAMPLE_BYTES = 4


class RunFolderError(ValueError):
    """A folder that is not a run folder this reader can take, with the
    reason."""


@dataclass
class Stream:
    """One stream of a run: its description and where its samples are.

    `frames` is what the file holds, whole frames only; `channels` are
    the table indexes of its slots in order, and `units` the unit each
    is recorded in.
    """

    name: str
    kind: str
    path: str
    description: dict[str, Any]
    channels: list[int]
    units: list[str]
    rate: float
    frames: int

    def blocks(self, frames_per_block: int) -> Iterator[tuple[int, np.ndarray]]:
        """The samples, a block of frames at a time, as read from the
        file: (first frame, frames by channels, float32). Only one block
        is in memory at once.

        Parameters
        ----------
        frames_per_block : int
            The most frames in one block.

        Returns
        -------
        iterator of (int, numpy.ndarray)
            Each block's first frame and its values.
        """
        width = len(self.channels)
        step = max(1, int(frames_per_block))
        with open(self.path, 'rb') as f:
            for first in range(0, self.frames, step):
                count = min(step, self.frames - first)
                values = np.fromfile(f, dtype='<f4', count=count * width)
                if values.size != count * width:
                    raise RunFolderError(f'{self.path} ended at frame '
                                         f'{first + values.size // width} of {self.frames}')
                yield first, values.reshape(count, width)


@dataclass
class Result:
    """One result of a run: its description and its arrays.

    A result that repeats has a record per line of its `.jsonl`, and
    each array holds the records first.
    """

    name: str
    kind: str
    description: dict[str, Any]
    arrays: dict[str, np.ndarray]
    records: list[dict[str, Any]] | None = None


@dataclass
class RunFolder:
    """A run folder as read: every member parsed, the streams sized from
    their files, the results' arrays read, and what the folder lacked."""

    path: str
    manifest: dict[str, Any]
    table: dict[str, Any]
    setup: dict[str, Any]
    log: list[dict[str, Any]] = field(default_factory=list)
    end: dict[str, Any] | None = None
    law: dict[str, Any] | None = None
    law_file: tuple[str, bytes] | None = None
    streams: dict[str, Stream] = field(default_factory=dict)
    results: dict[str, Result] = field(default_factory=dict)
    missing: list[str] = field(default_factory=list)

    @property
    def name(self) -> str:
        """The run's name: the folder's."""
        return os.path.basename(os.path.normpath(self.path))

    @property
    def source(self) -> str:
        """'SIM' or 'HARDWARE'."""
        return str(self.manifest['source'])

    @property
    def test(self) -> str:
        """The test the run was ('random', 'recorder', ...)."""
        return str(self.manifest['test'])

    @property
    def rows(self) -> list[dict[str, Any]]:
        """The channel table's rows as armed."""
        return list(self.table.get('rows', []))


def folder_of(path: str | os.PathLike) -> str | None:
    """The run folder `path` names, or None when it names none: the
    folder itself, or the `run.json` inside one.

    Parameters
    ----------
    path : str or os.PathLike
        A folder, or a file.

    Returns
    -------
    str or None
        The folder's absolute path, when it holds a manifest naming the
        run folder schema (of any number, so a newer one is refused by
        `read` rather than taken for something else).
    """
    path = os.path.abspath(os.path.expanduser(str(path)))
    if os.path.basename(path) == MANIFEST and os.path.isfile(path):
        folder = os.path.dirname(path)
    elif os.path.isdir(path):
        folder = path
    else:
        return None
    manifest = os.path.join(folder, MANIFEST)
    if not os.path.isfile(manifest):
        return None
    try:
        with open(manifest, encoding='utf-8') as f:
            schema = json.load(f).get('schema', '')
    except (OSError, ValueError, AttributeError):
        return None
    return folder if str(schema).startswith(SCHEMA_NAME + '/') else None


def _json(path: str, what: str) -> Any:
    try:
        with open(path, encoding='utf-8') as f:
            return json.load(f)
    except FileNotFoundError:
        raise RunFolderError(f'{path}: {what} is missing') from None
    except ValueError as error:
        raise RunFolderError(f'{path}: {what} is not JSON ({error})') from None


def _schema_number(manifest: Mapping[str, Any], path: str) -> int:
    schema = str(manifest.get('schema', ''))
    name, _, number = schema.partition('/')
    if name != SCHEMA_NAME or not number.isdigit():
        raise RunFolderError(f'{path}: the schema {schema!r} is not '
                             f'{SCHEMA_NAME}/<number>')
    if int(number) > SCHEMA_NUMBER:
        raise RunFolderError(
            f'{path} was written by a newer Hertzforge (run folder schema '
            f'{number}; this build reads up to {SCHEMA_NUMBER}). Update '
            'Hertzforge to convert it.')
    return int(number)


def _log(path: str, missing: list[str]) -> list[dict[str, Any]]:
    if not os.path.isfile(path):
        missing.append('the run has no log (log.jsonl)')
        return []
    with open(path, 'rb') as f:
        raw = f.read()
    lines = raw.split(b'\n')
    entries = []
    for k, line in enumerate(lines):
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            # the last line, written when the run stopped, may be cut short
            if k == len(lines) - 1:
                missing.append(f'the last line of the log was cut short '
                               f'({len(line)} bytes), so it is left out')
                continue
            raise RunFolderError(f'{path}: line {k + 1} is not JSON') from None
        if not isinstance(entry, dict):
            raise RunFolderError(f'{path}: line {k + 1} is not a JSON object')
        entries.append(entry)
    return entries


def _law(folder: str) -> tuple[dict[str, Any] | None, tuple[str, bytes] | None]:
    held = os.path.join(folder, 'law')
    if not os.path.isdir(held):
        return None, None
    law = _json(os.path.join(held, 'law.json'), 'the law (law.json)')
    named = law.get('file') if isinstance(law, dict) else None
    others = sorted(name for name in os.listdir(held) if name != 'law.json')
    if isinstance(named, str) and named in others:
        others = [named]
    if len(others) > 1:
        raise RunFolderError(f'{held} holds {len(others)} files beside law.json and '
                             'law.json names none of them')
    if not others:
        return law, None
    with open(os.path.join(held, others[0]), 'rb') as f:
        return law, (others[0], f.read())


def _declared(manifest: Mapping[str, Any], member: str) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for entry in manifest.get(member) or []:
        if isinstance(entry, Mapping) and entry.get('name'):
            out[str(entry['name'])] = dict(entry)
    return out


def _stream(folder: str, name: str, missing: list[str]) -> Stream | None:
    where = os.path.join(folder, 'streams', name)
    side = os.path.join(where, 'stream.json')
    if not os.path.isfile(side):
        missing.append(f'the stream {name!r} is declared but has no stream.json')
        return None
    description = _json(side, f'the stream {name!r} (stream.json)')
    kind = str(description.get('kind', ''))
    if kind not in STREAM_KINDS:
        raise RunFolderError(f'{side}: the kind {kind!r} is not one of '
                             f'{", ".join(STREAM_KINDS)}')
    channels = description.get('channels') or []
    try:
        indexes = [int(c['index']) for c in channels]
        units = [str(c.get('unit', '')) for c in channels]
        rate = float(description['fs_hz'])
    except (KeyError, TypeError, ValueError):
        raise RunFolderError(f'{side}: a stream names its rate (fs_hz) and each '
                             'channel\'s table index and unit') from None
    if not indexes or rate <= 0:
        raise RunFolderError(f'{side}: a stream has channels and a rate above zero')
    samples = os.path.join(where, 'telemetry.f32')
    size = os.path.getsize(samples) if os.path.isfile(samples) else 0
    if not os.path.isfile(samples):
        missing.append(f'the stream {name!r} has no samples (telemetry.f32)')
    frame_bytes = SAMPLE_BYTES * len(indexes)
    frames, torn = divmod(size, frame_bytes)
    if torn:
        missing.append(f'the stream {name!r} ends {torn} bytes into a frame, '
                       'which is left out')
    stated = description.get('frames')
    if isinstance(stated, int) and stated != frames:
        missing.append(f'the stream {name!r} says {stated} frames and its file '
                       f'holds {frames}; the file is taken')
    if description.get('complete') is False:
        missing.append(f'the stream {name!r} did not complete')
    if description.get('fault'):
        missing.append(f'the stream {name!r} reports a fault: {description["fault"]}')
    return Stream(name, kind, samples, description, indexes, units, rate, frames)


_DTYPES = {'f8': '<f8', 'float64': '<f8', 'c16': '<c16', 'complex128': '<c16',
           'f4': '<f4', 'float32': '<f4', 'i8': '<i8', 'int64': '<i8',
           'i4': '<i4', 'int32': '<i4', 'u1': 'u1', 'uint8': 'u1'}


def _result(folder: str, name: str, missing: list[str]) -> Result | None:
    held = os.path.join(folder, 'results')
    side = os.path.join(held, f'{name}.json')
    if not os.path.isfile(side):
        missing.append(f'the result {name!r} is declared but has no {name}.json')
        return None
    description = _json(side, f'the result {name!r}')
    layout = []
    for array in description.get('arrays') or []:
        try:
            dtype = np.dtype(_DTYPES[str(array['dtype'])])
            shape = tuple(int(n) for n in array['shape'])
            layout.append((str(array['name']), dtype, shape, int(array['offset'])))
        except (KeyError, TypeError, ValueError):
            raise RunFolderError(f'{side}: each array has a name, a dtype ('
                                 f'{", ".join(sorted(_DTYPES))}), a shape and an '
                                 'offset') from None
    lines_path = os.path.join(held, f'{name}.jsonl')
    repeating = 'records' in description or os.path.isfile(lines_path)
    records = _log(lines_path, []) if repeating else None
    values_path = os.path.join(held, f'{name}.bin')
    data = b''
    if layout:
        if not os.path.isfile(values_path):
            missing.append(f'the result {name!r} has no values ({name}.bin)')
            return None
        with open(values_path, 'rb') as f:
            data = f.read()
    record_bytes = max((offset + dtype.itemsize * int(np.prod(shape))
                        for _n, dtype, shape, offset in layout), default=0)
    arrays: dict[str, np.ndarray] = {}
    if records is None:
        if len(data) < record_bytes:
            missing.append(f'the result {name!r} holds {len(data)} bytes of the '
                           f'{record_bytes} its arrays need, so it is left out')
            return None
        for array_name, dtype, shape, offset in layout:
            count = int(np.prod(shape))
            arrays[array_name] = np.frombuffer(
                data, dtype=dtype, count=count, offset=offset).reshape(shape).copy()
    else:
        whole = len(data) // record_bytes if record_bytes else len(records)
        count_records = min(whole, len(records))
        if whole != len(records):
            missing.append(f'the result {name!r} lists {len(records)} records and '
                           f'holds the values of {whole}; {count_records} are taken')
        records = records[:count_records]
        for array_name, dtype, shape, offset in layout:
            count = int(np.prod(shape))
            arrays[array_name] = np.stack([
                np.frombuffer(data, dtype=dtype, count=count,
                              offset=k * record_bytes + offset).reshape(shape)
                for k in range(count_records)]) if count_records else np.empty(
                    (0, *shape), dtype=dtype)
    return Result(name, str(description.get('kind', '')), description, arrays, records)


def read(path: str | os.PathLike) -> RunFolder:
    """Read a run folder whole, or the folder whose `run.json` `path` is.

    Parameters
    ----------
    path : str or os.PathLike
        The run folder, or its `run.json`.

    Returns
    -------
    RunFolder
        Every member parsed, and what the folder lacked in `missing`.

    Raises
    ------
    RunFolderError
        For a folder with no manifest, a schema newer than this reader
        (`SCHEMA_NUMBER`) or not this schema at all, a manifest with no
        source label or a test not named, a missing channel table or
        setup, or a member that cannot be read as the layout says.
    """
    given = os.path.abspath(os.path.expanduser(str(path)))
    folder = os.path.dirname(given) if os.path.basename(given) == MANIFEST else given
    manifest = _json(os.path.join(folder, MANIFEST), 'the manifest (run.json)')
    if not isinstance(manifest, dict):
        raise RunFolderError(f'{folder}: run.json is not a JSON object')
    _schema_number(manifest, folder)
    source = manifest.get('source')
    if source not in SOURCES:
        said = 'has no source label' if source in (None, '') else f'has the source {source!r}'
        raise RunFolderError(
            f'{folder}: the run {said}. A run is SIM or HARDWARE, and which is '
            'never guessed, so the folder is not converted.')
    if not manifest.get('test'):
        raise RunFolderError(f'{folder}: run.json does not name the test')
    table = _json(os.path.join(folder, 'table.json'), 'the channel table (table.json)')
    if not isinstance(table, dict) or not isinstance(table.get('rows'), list):
        raise RunFolderError(f'{folder}: table.json holds no rows')
    setup = _json(os.path.join(folder, 'setup.json'), 'the setup (setup.json)')
    if not isinstance(setup, dict):
        raise RunFolderError(f'{folder}: setup.json is not a JSON object')
    missing: list[str] = []
    if not manifest.get('run_uid'):
        missing.append('the manifest has no run_uid')
    if not manifest.get('complete'):
        missing.append('the manifest says the run is not complete')
    end_path = os.path.join(folder, 'end.json')
    end = _json(end_path, 'the end (end.json)') if os.path.isfile(end_path) else None
    if end is None:
        missing.append('the run did not end cleanly: there is no end.json')
    log = _log(os.path.join(folder, 'log.jsonl'), missing)
    law, law_file = _law(folder)
    declared = _declared(manifest, 'streams')
    on_disk = (sorted(os.listdir(os.path.join(folder, 'streams')))
               if os.path.isdir(os.path.join(folder, 'streams')) else [])
    streams: dict[str, Stream] = {}
    for name in [*declared, *(n for n in on_disk if n not in declared)]:
        stream = _stream(folder, name, missing)
        if stream is not None:
            streams[name] = stream
    declared = _declared(manifest, 'results')
    results_folder = os.path.join(folder, 'results')
    on_disk = (sorted(name[:-5] for name in os.listdir(results_folder)
                      if name.endswith('.json'))
               if os.path.isdir(results_folder) else [])
    results: dict[str, Result] = {}
    for name in [*declared, *(n for n in on_disk if n not in declared)]:
        result = _result(folder, name, missing)
        if result is not None:
            results[name] = result
    return RunFolder(folder, manifest, table, setup, log=log, end=end, law=law,
                     law_file=law_file, streams=streams, results=results,
                     missing=missing)
