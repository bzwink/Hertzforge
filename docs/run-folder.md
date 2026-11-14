# The Run Folder

A control run is kept on disk as a run folder: one folder per run, written by the
control service while the run goes, that needs no other file and no running
service to be read. The analysis side converts it into an ordinary project file
(`hertzforge convert-run FOLDER`, or open the folder or its `run.json`), so a
finished run reaches every analysis tool with its channel table, its setup, its
log and its measurements. Today the Random, Sine, Classical Shock, SRS shock and
TWR modules and the Recorder write one for every run; the other control modules
do not yet.

The exact form is the reader's, `hertzforge.io.run_folder`; the conversion is
`hertzforge.io.run_convert`, described in `docs/hforge-format.md` ("Run Folders
and Their Conversion"). This guide says what is in a folder and the rules it is
written by.

## Where It Goes

Under the service's data folder: the folder `HERTZFORGE_DATA_ROOT` names, or
`Hertzforge_Data` in the home folder. A Random run's folder is named
`<YYYYMMDD-HHMMSS>_random_<run id>`, a Sine run's
`<YYYYMMDD-HHMMSS>_sine_<run id>`, a Classical Shock run's
`<YYYYMMDD-HHMMSS>_classical_shock_<run id>`, an SRS shock run's
`<YYYYMMDD-HHMMSS>_srs_<run id>` and a TWR run's
`<YYYYMMDD-HHMMSS>_twr_<run id>`, the time it armed in UTC. A Recorder run's
folder is the plain name its setup gives (`capture.run_name`). The run's state
names the folder from arm on (`random.state`, `sine.state`, `shock.state`,
`srs.state`, `twr.state` and `recorder.state`, `record.folder`).

## Layout

```
<run name>/
  run.json            the manifest
  table.json          the channel table the run armed with
  setup.json          the configuration as accepted, and what it resolved to
  law/law.json        the control law in use
  log.jsonl           the run's log, one entry per line
  streams/<name>/     telemetry.f32 and stream.json
  results/<name>.json, <name>.bin, <name>.jsonl
  end.json            written once, at the end
```

### `run.json`

The manifest: `schema` (`hertzforge-run/1`), `test`, `run_uid` (unique across
restarts and machines: the time it armed, or a Recorder run started, and eight
hexadecimal digits),
`run_id` (the service's own id, which repeats after a restart), `title`,
`operator`, `article`, `notes`, `specification` (`document`, `clause`, when
given), `source` (`SIM` or `HARDWARE`, from the station's class, never absent),
`sim` for a simulated run (the plant's `model`, its `parameters` and its
`seed`), `station` (`kind`, `class`, `devices` each with `id`, `model` and
`serial`, and `clock`), `grid` (the rate asked for and the rate the station
programmed, the FFT length, averages, line spacing and lines), `build`
(`version`, `commit`, `dirty`), `armed` and `start` (`unix` seconds,
`utc_offset_s`, and `start.station_frame`, the station frame at that instant),
`streams` and `results` (what the folder holds, each `name` and `kind`, a
stream also its channels), `recording` (`streams` `on` or `off`, and `why` when
off) and `complete`, false until the run has ended and `end.json` is written.

A Recorder run's manifest has no `armed` (its `start` is when acquisition
began), adds `metadata` (the fields its setup names, each `key`, `label` and
`value`) and, on the simulator, `sim.source`, the signal its inputs carry in
words. Its `grid` is the rate asked and programmed, the station's block and
batch, the input group delay in frames, and the analysis rate, line spacing and
lines. Its `streams` lists each recording as it begins.

The station frame counts the input frames the station has delivered since it
started; every stream and log entry is placed by it.

### `table.json`

Every row of the channel table the run armed with, enabled or not, in the
protocol's own spelling for a channel row (`channels.get`), with the table's
`revision` and the SHA-256 of its rows (`sha256`), so two runs on one table can
be told to be so.

### `setup.json`

`config`, the configuration exactly as the service accepted it; `resolved`,
what the service made of it (the profile as anchors in g²/Hz, each with the
alarm and abort bands that hold from it to the next; the level schedule; the
tolerances; every limit and every abort in force with its channel; the control
settings; the control channels; the rate asked for and programmed); `derived`,
the derived values with the grid the run used; and `feasibility`, its inputs
and its verdict.

### `law/law.json`

The control law in use: its `id` and `name`, `builtin`, its parameters and the
guards it runs under. A law that is a file has its bytes beside it.

### `log.jsonl`

One JSON object per line, in order, each appended as the entry is made: `seq`,
`at_unix`, `frame` (the station frame, or null before the station has
delivered one), `kind`, `text` (the words the operator saw) and, where it has
any, `data`: the level of a level change, an alarm's lines and worst excursion,
a trip's facts, an overload's channel, an acknowledgement's words, the action
an operator took and who took it, what a `record` entry could not write. Every
state change, level change, alarm, trip, overload, override and operator action
is an entry.

In a Recorder run's log, `frame` is the station frame when the run's thread
passed the entry to the record (it does so ten times a second), not when the
entry was made, and null for the entries made before the first batch; a mark's
entry (`mark` or `overload`) has the mark's own station frame and the mark as its
`data`.

### `streams/<name>/`

`telemetry.f32`: little-endian float32, frame-interleaved, in engineering
units (each channel in its table unit, the drive in volts), no header.
`stream.json`: `kind` (`inputs`, `drives` or `recording`), `fs_hz`, `channels`
in slot order (each with its table `index` and `unit`), `first_station_frame`,
`frames`, `gaps` (each `at_frame`, its place in the file, and
`dropped_frames`), `complete` and `fault`. A drive stream also says its
`output_latency_frames`, how many frames after being written a drive sample is
played and whether that is `declared` by the station or `measured`; a Random
run adds `pipeline_lag_frames`, the drive-to-response lag its system
identification measured. A gap is never padded: the samples after it follow on.
A golden folder whose streams were cut to their first frames says so with
`cut_from_frames`, the frames the run recorded (`frames` is what the file holds).

A stream of kind `recording` is one Recorder recording, `streams/rec_NNNN/`,
numbered in the order they began. Its `stream.json` adds the recording's own
facts: its `name` and `trigger` (`start`, `manual` or `level`),
`requested_fs_hz`, `group_delay_frames`, `rows` and `units` (the channels'
indexes and units again, in slot order), `trigger_station_frame` (time zero,
the file frame `pre_frames`), `pre_frames` and `pre_frames_asked` (the
pre-window held and asked), `marks` (each mark that fell inside it, as
`recorder.marks` gives them), `t0_convention`, `n_channels`, `config` (the
acquisition and analysis selection it was made at) and, once it is finalized,
`writer` (the capture writer's ring, chunks written and dropped, flushes, and
the free space at its start and the floor it kept). Its `frames` is 0 until it
is finalized; the file is the authority on its length. Beside it,
`capture.json` is the capture writer's own record of the same recording.

A pulse module's stream is a run of shots, and its `stream.json` lists them in
`shots`, in the order they were fired: each shot's `first_frame` (its place in
the file) and `frames` (what the file holds of it), `first_station_frame`,
`pre_frames` (the frames of the shot before its firing), `station_frame` (the
station frame it fired at, the first plus `pre_frames`), its `level_db`, and
`what` it was (`identification` or `pulse`) with its `pulse` (from 0) and
`origin`. The marks travel in order with the samples, so a shot's place is the
file's own, a gap included.

### `results/`

A result is a set of named arrays: `<name>.json` describes it (`kind`,
`abscissa`, `channels` by table index, `convention` for a cross term, and each
array's `name`, `dtype`, `shape`, `unit` and byte `offset`), `<name>.bin` holds
the values (little-endian float64; complex as real and imaginary pairs). A
result that repeats appends a record's arrays to `<name>.bin` and a line to
`<name>.jsonl` each time. Cross terms are in the file's convention, conj(X)·Y,
converted once by the control service. Values are in the channels' own units;
the analysis side converts them to SI. A result kept in fixed-length records (a
Sine run's `channel_levels` and `control_trace`) fills its last record out with
NaN, and each record's line says how many it holds (`points`, `calls`).

### `end.json`

`run_uid`, `end` exactly as the run's state reports it (`outcome`, `message`,
`at_s`, `level_db`, `trip`), `at_unix`, `station_frames`, each stream's
`frames`, `drops`, `gaps` and `fault`, and a closing summary. Written once;
`run.json` is rewritten with `complete: true` after it. A Recorder run's adds
`run_id`, `rows` (each recorded row's `index`, `mean`, `rms`, `peak`, `crest`
and `kurtosis` over the whole run) and each recording's `complete`; its
`station_frames` are the frames acquired.

## The Rules

- **One folder per run, never reused.** The folder is claimed at arm by
  creating it with a call that fails when it exists; a name that is taken gets
  the next free `<name>-<n>`. A run never writes into another's folder.
- **Valid at every moment.** `run.json`, `table.json`, `setup.json` and the law
  are on disk before the first sample; small files are written whole to a
  temporary name and renamed into place; the log and the streams are appended
  and synced to the disk every five seconds. A run that stops any way, the
  service killed included, leaves a folder the reader takes whole, with
  `complete: false` and no `end.json` when it did not end cleanly.
- **All writing on one thread.** The record has its own writer thread, and
  every file operation happens there. The run hands it entries and results with
  a send that never waits. The streams are taken where the station's blocks
  pass, into buffers made before the run; when the writer has fallen behind and
  no buffer is free, the block is dropped, counted, and kept as a gap where it
  happened. Nothing on the control loop's thread touches a file, takes a lock a
  slow disk can hold, or waits.
- **The Recorder's recordings are the exception that proves it.** The run
  folder's own files are written by the record's writer thread as above; the
  recordings under `streams/` are written by the Recorder's own record thread,
  behind a queue of 2 s of frames, and the thread that reads the station hands
  each batch to it without waiting. Where a control run drops a block and goes
  on, the Recorder ends its run (`docs/recorder.md`, "What Ends a Run"): the
  recording is what it is for.
- **Self-describing.** Every array has a unit, every stream names its channels
  by table index, and SIM or HARDWARE is stated, never guessed.
- **It grows by optional members.** A reader ignores members it does not know,
  and refuses a schema number newer than its own.

## When the Disk Fails

- **At arm.** When the folder, `run.json`, `table.json`, `setup.json` or the
  law cannot be written (a missing or read-only data folder), the arm is
  refused `record_unwritable`, naming the path and the reason. Nothing has been
  driven.
- **Below the free-space floor at arm** (512 MB kept free), the run goes on
  without its streams: `run.json` says `recording.streams` `off` and why, the
  log has a `record` entry, and the state's `record.streams` is `off`.
- **While the run goes.** A stream that cannot be written, or the free space
  falling under the floor (512 MB plus what the streams already hold, which the
  converted file needs again), stops that stream: its `stream.json` names the
  fault, the log has a `record` entry, and the state's `record.problem` says it
  in words. A log line or a result that cannot be written is reported the same
  way. The run goes on: recording never stops, stalls or aborts a control run.
- **The end.** When `end.json` cannot be written, the folder keeps
  `complete: false`, and the reader says the run did not end cleanly.
- **A Recorder run** refuses its start under its own free-space floor
  (`not_enough_space`, before anything is made) and refuses it `arm_refused`,
  naming the path and the reason, when the folder, `run.json`, `table.json`,
  `setup.json` or `streams/` cannot be made. While it records, a recording
  that cannot be written, the free space falling under the floor, or a disk so
  far behind that a batch cannot be queued ends the run (`write_failed` or
  `write_behind`), the recording finalized with what reached the disk and its
  `fault` or its gap stated. A log line the record cannot write is a `record`
  entry and the state's `record.problem`, and the run goes on.

## What a Classical Shock Run Saves

From arm: `run.json`, `table.json`, `setup.json` (the configuration as
accepted; resolved, the nominal pulse with its shape, amplitude, duration,
polarity, rise and fall and compensation, the tolerance standard and its bands,
the level schedule and pulse counts, the spacing, the identification, the
control settings and the controller's own, the limits and monitors, the SRS
grid and the rates; the derived values; feasibility), `law/law.json` (the law's
identity, parameters and guards) and `log.jsonl` (every state, level, pulse
fired with its level and verdict, alarm, drive-limit action, trip, hold and
operator action, each with its facts in `data`), and these files, each checked
against the golden run folder (`schema/golden/runs/classical_shock/`) by
`tests/test_shock_run_folder_golden.py`:

| File | What |
|---|---|
| `streams/inputs` | Every enabled input as read from the station, in table order, a shot for the identification and one for each pulse (`kind` `inputs`) |
| `streams/drives` | The drive as the device accepted it, in volts, shot for shot with the inputs (`kind` `drives`) |
| `results/pulse_reference` | The nominal pulse at full level and its time-domain tolerance band, time from the main pulse's start (`kind` `pulse_reference`, kept as arrays) |
| `results/srs_reference` | The nominal pulse's SRS at full level and its informational band (`kind` `srs_reference`) |
| `results/identification_frf` | The identification's complex transfer function, drive to control response, on the correction band (`kind` `frf`) |
| `results/identification_coherence` | Its ordinary coherence, and the lines the control trusts (`kind` `coherence`) |
| `results/pulse_srs` | Each pulse's SRS, with the reference's at the level fired and its band (`kind` `srs`, one record per pulse measured) |
| `results/pulses` | Each pulse as judged: its level, scheduled level, limit scale, drive peak, peak, peak ratio, velocity change and its ratio, peak velocity, peak-to-peak displacement, error, the three verdicts and the overall one, and its SRS bands (`kind` `pulse_results`, kept as arrays, one record per pulse) |
| `end.json` | The end, as the run's state reports it |

Each pulse's results are written when the pulse is judged, the last pulse of
the run included; a record's line names the pulse, its level, its origin and
the station frame. A run that ends before its identification has no
identification and no pulses; one held by identification has its identification
and no pulses. With `record.streams` false there is no `streams/` folder. The
analysis side keeps the nominal pulse and its time-domain tolerance as the
setup's `resolved.pulse` and `resolved.tolerance` and as the arrays of
`pulse_reference` in the run record, since no analysis class holds a nominal
pulse; the verdicts and kinematics of `pulses` are arrays there too.

The golden folder is written by `cargo test -p sds-control --test
classical_shock_golden_run` from a short run on the simulator, with fixed seeds,
its wall times and build fields normalized; `scripts/make_golden_runs.sh`
rewrites it.

## What an SRS Run Saves

From arm: `run.json`, `table.json`, `setup.json` (the configuration as
accepted; resolved, the target as given and on the band centres, the bands and
their Q, the envelope's alarm and abort bands, the synthesis settings with the
offline synthesis's result, its pulse's kinematics and its fitted table, the
ring-out, the level schedule and pulse counts, the spacing, the identification,
the control settings and the controller's own, the limits, monitors and limit
channels, and the rates; the derived values; feasibility), `law/law.json` (the
law's identity, parameters and guards) and `log.jsonl` (every state, level,
pulse fired with its level and verdict, alarm, drive-limit action, notch, unused
law call, trip, hold and operator action, each with its facts in `data`), and
these files, each checked against the golden run folder
(`schema/golden/runs/srs/`) by `tests/test_srs_run_folder_golden.py`:

| File | What |
|---|---|
| `streams/inputs` | Every enabled input as read from the station, in table order, a shot for the identification and one for each pulse (`kind` `inputs`) |
| `streams/drives` | The drive as the device accepted it, in volts, shot for shot with the inputs (`kind` `drives`) |
| `results/srs_reference` | The target SRS at full level on the run's bands, with the envelope's alarm band and abort band (`kind` `srs_reference`) |
| `results/synthesis` | The pulse the configuration was synthesized to, at full level and the configured rate, with its velocity, displacement and SRS on the bands (`kind` `srs_synthesis`, kept as arrays) |
| `results/identification_frf` | The identification's complex transfer function, drive to control response, on the band (`kind` `frf`) |
| `results/identification_coherence` | Its ordinary coherence (`kind` `coherence`) |
| `results/pulse_srs` | Each pulse's SRS, with the target at the level fired and the envelope's bands (`kind` `srs`, one record per pulse measured) |
| `results/drive_tables` | The drive table after each pulse: each component's frequency and drive amplitude, and the law's gains (`kind` `srs_drive_table`, kept as arrays, one record per pulse) |
| `results/pulses` | Each pulse as judged: its level, scheduled level, limit scale, drive peak, peak, peak velocity, peak-to-peak displacement, worst band error and its frequency, bands outside the alarm and abort bands, the limit channels' cut and its verdict (`kind` `pulse_results`, kept as arrays, one record per pulse) |
| `end.json` | The end, as the run's state reports it |

Each pulse's results are written when the pulse is judged, the last pulse of
the run included; a record's line names the pulse, its level and the station
frame, and the law call it played. Every SRS shot fires at its first frame. A
shot no pulse was judged on (a pulse withheld in the wait before it played, when
nobody held control, or one stopped part way) is named `unjudged`; a withheld
one holds the drive committed for it and no input frames, and says `played`
false. A run that ends before its identification has no identification and no
pulses; one held by identification has its identification and no pulses. With
`record.streams` false there is no `streams/` folder. The limit channels' own
identified gains are not saved. The analysis side keeps the synthesis, the drive
tables and the verdicts and kinematics of `pulses` as arrays in the run record,
since no analysis class holds them: the synthesized pulse is a design the drive
started from, not a waveform the test was controlled to.

The golden folder is written by `cargo test -p sds-control --test
srs_golden_run` from a short run on the simulator, with fixed seeds, its wall
times, its law calls' times and build fields normalized;
`scripts/make_golden_runs.sh` rewrites it.

## What a TWR Run Saves

From arm: `run.json`, `table.json`, `setup.json` (the configuration as accepted;
resolved, the reference as stored with its name, SHA-256, rate, length, units
and preparation, the record it was prepared into at the rate the station runs
at with its section and pads, the identification, the inverse and the
correction, the control law and its guards, the level schedule with its gate,
its iterations and the playback's play count, the limits and the feasibility
ceilings, and the rates; the derived values; feasibility), `law/law.json` (the
law's identity, parameters and guards) and `log.jsonl` (every state, level,
play with its level, error and how it ended, alarm, trip, hold and operator
action, each with its facts in `data`), and these files, each checked against
the golden run folder (`schema/golden/runs/twr/`) by
`tests/test_twr_run_folder_golden.py`:

| File | What |
|---|---|
| `streams/inputs` | Every enabled input as read from the station, in table order, a shot for the identification and one for each play (`kind` `inputs`) |
| `streams/drives` | The drive as the device accepted it, in volts, shot for shot with the inputs, with the lag every play is aligned at (`kind` `drives`) |
| `results/reference` | The reference as imported, at its own rate (`kind` `waveform`, kept as arrays) |
| `results/target` | The target at full level: the prepared section between its pads on the record's samples, time from the section's start, with the alarm and abort bands' share of its peak (`kind` `transient_reference`) |
| `results/identification_frf` | The identification's complex transfer function, drive to control response, on the identification band (`kind` `frf`) |
| `results/identification_coherence` | Its ordinary coherence, and the lines the correction trusts (`kind` `coherence`) |
| `results/plays` | Each play: its level, kind and iteration, its rms, peak and spectral error, whether it met the gate, its clipped samples and clamped bins, its drive and response peaks, its lag and drift, its start and length in station frames, and whether it tripped or was cancelled (`kind` `play_results`, kept as arrays, one record per play) |
| `results/law_calls` | Every call of the control law: the drive it returned at full level on the record's samples, the level and error it was handed, its caps, whether it was used and how long it took (`kind` `law_calls`, kept as arrays, one record per call) |
| `end.json` | The end, as the run's state reports it |

A play is one start and one stop of the station: its shot holds the record and
the rest pad after it, and its `pre_frames` are the pad before the section, so
the section starts at time zero. Each play's results are written when it is
judged, the last play of the run included; the play a protection ends the run
on, and one a stop or an abort cuts short, are recorded as far as the station
acquired them, their shot and their record saying `tripped` or `cancelled`.
The law is called at the seed, after an iteration play that misses its gate,
and after a playback play only when correction goes on during playback; a call
that gave no drive has NaN for it. A run held by its identification has its
identification shot and no plays. With `record.streams` false there is no
`streams/` folder. The analysis side keeps the target as a
`TransientSpecification`, which has no limits, so the alarm and abort shares
stay in its result's description; the reference as imported, each play's
measures and every law call stay in the run record as named arrays.

The golden folder is written by `cargo test -p sds-control --test
twr_golden_run` from a short run on the simulator, with fixed seeds, its wall
times, build fields and each law call's measured duration normalized, and each
shot of its streams cut to its first 128 frames (`cut_from_frames`, on the
stream and on each shot): every shot carries the station's rest pad of 1024
frames; `scripts/make_golden_runs.sh` rewrites it.

## What a Random Run Saves

From arm: `run.json`, `table.json`, `setup.json`, `law/law.json` (the built-in
control law and its guards) and `log.jsonl`, and these files, each checked
against the golden run folder (`schema/golden/runs/random/`) by
`tests/test_run_folder_golden.py`:

| File | What |
|---|---|
| `streams/inputs` | Every enabled input as read from the station, in table order (`kind` `inputs`) |
| `streams/drives` | Every enabled drive as the device accepted it, in volts (`kind` `drives`) |
| `results/reference` | The reference and its alarm and abort bands at 0 dB on the run's lines (`kind` `envelope`, kept as arrays) |
| `results/identification_frf` | The system identification's complex transfer function, drive to control response (`kind` `frf`) |
| `results/identification_coherence` | Its ordinary coherence (`kind` `coherence`) |
| `results/control_psd` | Each control channel's spectrum and the composite (`kind` `psd`, one record per keeping) |
| `results/response_psd` | Every enabled input's spectrum (`kind` `psd`, one record per keeping) |
| `results/drive_psd` | The drive's spectrum (`kind` `psd`, one record per keeping) |
| `end.json` | The end, as the run's state reports it |

The spectra are kept from the last control loop before each level change and
from the last loop of the run; each record's line says which (`why`), the level
the loop ran at, the loop and the station frame. A run with a limit adds
`results/limit_ch<n>` for each limited channel, its ceiling on the lines it
covers (`kind` `limit`). With several control channels the identification is of
their composite, written as `composite_frf` and `composite_coherence` and kept
as arrays. With `record.streams` false there is no `streams/` folder. A run that
ends before its system identification has no identification or spectra.

The golden folder is written by `cargo test -p sds-control --test
random_golden_run` from a short run on the simulator, with fixed seeds, its wall
times and build fields normalized; `scripts/make_golden_runs.sh` rewrites it.

## What a Sine Run Saves

From arm: `run.json`, `table.json`, `setup.json` (with `resolved.test`, the
sweep, dwell, step or resonance search as resolved, `resolved.profile`, the
breakpoints with the quantity each was given in and the acceleration the
controller makes of it there, `resolved.limits` with the notch in
`resolved.aborts`, and `resolved.controller`, the controller's own
configuration), `law/law.json` (the law's identity, its parameters and its
guards; a law file is named by its SHA-256, its bytes are not copied) and
`log.jsonl`, and these files, each checked against the golden run folder
(`schema/golden/runs/sine/`) by `tests/test_run_folder_golden_sine.py`:

| File | What |
|---|---|
| `streams/inputs` | Every enabled input as read from the station, in table order (`kind` `inputs`), the identification's burst included |
| `streams/drives` | The drive as the device accepted it, in volts, and the reference output's when there is one (`kind` `drives`) |
| `results/reference` | The profile and its alarm and abort bands at 0 dB, peak, on the run's band: its breakpoints and points between them (`kind` `envelope`) |
| `results/identification_frf` | The identification's complex transfer function, drive to control response, on the band the gate judges, the response taken the lag it measured later (`kind` `frf`) |
| `results/identification_coherence` | Its ordinary coherence (`kind` `coherence`) |
| `results/channel_levels` | Each input's tracked level, peak, and phase against the drive, with the tone's frequency, the scheduled level, the direction (+1 rising, -1 falling, 0 standing), the pass (the one-way sweep, counted from 0, which changes when the sweep turns or the level changes), and the control point's target, measured value and drive, as the run went (`kind` `sine_levels`, about twenty points a second, records of 64) |
| `results/control_trace` | Every call of the control law: time from engage, frequency, target, measured, the drive in force, the law's output, the drive the core took, the prediction, why the core held, a miss, a cap, the call's time and the law's declared values (`kind` `sine_control`, records of 64 calls) |
| `end.json` | The end, as the run's state reports it |

A run with a limit channel adds `results/limit_ch<n>`, its lines on the run's
band: the level the notch holds it to and its abort bound (`kind` `limit`). With
`record.streams` false there is no `streams/` folder. A run that ends before
identification has no identification; one that ends before it engages has no
levels and no trace.

On the analysis side a sweep or a dwell becomes a `SineSweepSpecification` of
one tone over the control channels (a sweep's tone is one pass across the band
in the direction the run started, at its rate and law), and a sweep's or a step
test's `channel_levels` a `SineLevelSet`: the last one-way sweep at the highest
level the run reached, where the tone moved. Every point of every pass, a
dwell's levels against time, the trace, the reference and the limit lines stay
in the run record as named arrays. A step test and a resonance search have no
specification: their profile stays in the setup.

The golden folder is written by `cargo test -p sds-control --test
sine_golden_run` from a short sweep on the simulator, with fixed seeds, its wall
times, build fields and each law call's measured duration normalized, and its
streams cut to their first 1024 frames (`cut_from_frames`): the identification's
burst alone is 131 072 frames;
`scripts/make_golden_runs.sh` rewrites it.

## What a Recorder Run Saves

From start: `run.json`, `table.json` (every row, enabled or not), `setup.json`
(`config` as accepted; `resolved`, the acquisition at the programmed rate, the
recorded rows with their ranges, the outputs (none), the trigger with its
hysteresis and its frames, the analysis and the capture with its floor;
`derived` at the programmed rate; and `overload`, the overload indicator's
fraction and definition) and `log.jsonl`, and these, each checked against the
golden run folder (`schema/golden/runs/recorder/`) by
`tests/test_run_folder_golden_recorder.py`:

| File | What |
|---|---|
| `streams/rec_NNNN` | One recording of every recorded row, in table order, at the programmed rate (`kind` `recording`): `telemetry.f32`, `stream.json` and `capture.json` |
| `end.json` | The end, as the run's state reports it, each row's statistics over the run, and each recording's frames, gaps and fault |

A Recorder run has no law and no results: its analysis is live only. Monitoring
saves no samples, so a run that made no recording has no `streams/` entries. A
recording's pre-window is its first `pre_frames` frames, before its trigger, and
converts to negative time.

The golden folder is written by `cargo test -p sds-control --test
recorder_golden_run` from a short level-triggered run on the simulator, its wall
times, build fields and what depends on when the stop arrived (the frames
acquired, the rows' statistics) normalized; `scripts/make_golden_runs.sh`
rewrites it. It holds no gap: a gap needs a disk that falls behind, which a test
cannot make the same every time, so gaps are tested on the Recorder's writer and
on the reader.
