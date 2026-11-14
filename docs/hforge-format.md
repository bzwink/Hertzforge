# The .hforge File, Laid Out

A `.hforge` file is a standard HDF5 file with a documented layout inside;
the extension does not mean a new container. Rename one to `.h5` and
HDFView browses it, `h5py` walks it, and MATLAB's `h5read` reads every
dataset. This page writes the layout down so that a file can be read
without reading the source. In the code the layout is defined in
`hertzforge.io.native` (the breakpoint table files in
`hertzforge.io.tables`), and
`tests/test_hforge_corpus.py` holds the code to it with the frozen files
in `testdata/hforge_corpus/`.

The layout started as the `.vdyn` schema 3, and every
object is stored the way `.vdyn` stores it. Three things differ: the
root stamp, the root attribute that holds the object groups, and the
names of a geometry's element-group fields. Hertzforge also opens every
`.vdyn` file and saves it as `.hforge`, so an older project
moves across by opening it and saving it.

## The Version Number

Every file stamps one root attribute:

    hertzforge_schema = 1, 2 or 3   (integer)

Schema 1 holds everything `.vdyn` schema 3 held. Schema 2 adds objects
held by reference to another file (below), and only a file with such an
object is stamped 2. Schema 3 adds the run record (`run_record/`,
below), and only a file holding one, or referring to one, is stamped 3.

The schema changes by addition. New datasets and attributes are
optional, with a default when they are absent, and existing ones are
never renamed or given a new meaning. A file is stamped with the
earliest version that holds everything in it, so an older reader still
opens it. The reader follows three rules:

- a stamp newer than the reader knows is refused with a message saying
  the file was written by a newer Hertzforge and to update to open it;
  the file is never half loaded;
- a file with neither this stamp nor the `.vdyn` stamp
  is refused as not a Hertzforge file, since every writer stamps;
- an object of a kind the reader does not know is refused by its name
  and kind, saying the file was written by a newer Hertzforge;
- an older stamp always loads.

While Hertzforge is an alpha this is a practice, not a promise: the
format may still change, and the promise that a later version opens
every earlier file starts with the first release that is not an alpha.

## Two Kinds of File

A single object saved on its own is one group at the root, named for
its kind: `geometry/`, `data/`, `shapes/`, `report/`, `photos/`,
`matches/`, `channel_table/`, `sine_specification/`, `sine_levels/`,
`predictions/`, `run_log_extract/` or `run_record/`. The loader recognizes the file by
the group it finds.

A breakpoint table file is a single-object file too, holding one of a
test's tables rather than an object a project holds: `breakpoint_table/`
(below). It is opened from the table it belongs to; opening or importing
one as a project is refused, saying so. A test's channel table is the
`channel_table/` object, saved on its own like any other.

A whole project is an `objects/` container of numbered groups (`0000`,
`0001`, and so on, which keep the order). Each group has two
attributes, `name` (the object's name, which may contain anything HDF5
would otherwise read as a path) and `kind` (one of the group names
above), and holds the object's own layout. The project's identity is in
root attributes:

    test_name         the project's name
    project_type      '' or e.g. 'Modal Test', 'Random Vibration'
    active_geometry   '' or the name unlinked objects are read against
    object_groups     JSON, the project's object groups: [{"members":
                      [names...], "role": null, "Basis" or "FEM",
                      "name": ...}, ...]; "name" is present only for a
                      named group
    provenance        JSON: {name: {"verb", "source", "params",
                      "state"}, ...}, how each derived object was
                      computed, with the settings fingerprint the
                      staleness check compares against. Optional key
                      "source_print", the digest of the source's values
                      when the fingerprint holds settings only; absent,
                      only the settings are compared

A project whose data index holds anything also has one dataset at the
root (absent otherwise):

    data_index        JSON, one string: [{"name", "path", "size",
                      "sha256", "holds", ...}, ...], every file the
                      project rests on, in the order indexed. Optional
                      keys: "format", "role" ("referenced" or
                      "attached"), "source" (what added the entry),
                      "objects" (the names read from the file),
                      "indexed", "status" ("matches", "changed" or
                      "not found"), "checked" and "found" (the last
                      check), and "text" (an attached text file's
                      contents). A dataset rather than an attribute
                      because an attached file's text can pass the
                      64 KiB an HDF5 attribute holds

## Objects Held by Reference

A project can hold an object by reference to a `.hforge` file instead
of a copy, so a project that analyzes a long run does not store the run
a second time. Hertzforge does this for every object of 64 MiB or more
imported from another `.hforge` file, as long as the object has not
been edited since it was read. An edited object is saved whole.

A referenced object is a numbered group under `objects/` with its
`name` and `kind` like any other, no datasets, and these attributes:

    reference_path         the file, relative to the folder the project
                           file is in, with '/' between folders; the
                           absolute path when there is no relative one
                           (another drive, on Windows)
    reference_absolute     the file's absolute path when it was saved
    reference_object       the object's name in that file; '' when the
                           file holds a single object (the root group
                           named for `kind`)
    reference_size         the file's size in bytes
    reference_sha256       the file's SHA-256, 64 lower-case hex digits
    reference_modified_ns  the file's modification time, in nanoseconds

The reader looks for the file by its relative path, then its absolute
path, then by its name in the project's folder. A file with the recorded
size and modification time is taken as it is; one with another time is
hashed, and taken when the SHA-256 matches, since a copied or moved file
keeps its bytes. The object is then read from that file, its large
arrays mapped as they would be from the project itself. When the file
is not found, or every candidate's checksum differs, the project opens
without the object, and saving it keeps the reference unchanged, so the
object comes back once the file is in place again. A referenced file
that holds the object by reference in turn is followed, up to eight
files deep.

HDF5's external links are not used, because they break with no error
when a file moves and carry no checksum. The file is also recorded in
the project's `data_index`, as every imported file is.

## Conventions

- Strings are UTF-8 (`h5py.string_dtype('utf-8')`), one dataset
  per field and one element per record. `''` means undefined: an
  ordinate unit of `''` is a channel whose units were never declared,
  and it reloads as undefined, not as dimensionless.
- Ragged arrays (element connectivity) are two datasets:
  `<name>_flat`, the values run together, and `<name>_offsets`, one
  longer than the count, so entry *i* is
  `flat[offsets[i]:offsets[i+1]]`.
- An optional field is left out when it is not set, with no
  placeholder value. A time history with no averaging has no
  `averaging_*` attributes, and a shape set that never carried complex
  modal damping has no `modal_damping` dataset.
- Numeric datasets are whatever numpy held (float64, complex128,
  int64) at full precision. Nothing is narrowed.
- Arrays are stored uncompressed unless the save asked for compression
  (`Project.save(path, compress=True)`). Then each numeric array of 64 KB
  or more is chunked along its last axis (time, for a time history) and
  stored with gzip at level 1 and the shuffle filter. Every HDF5 build
  reads both, so nothing about reading the file changes.
- A cross term is stored as conj(X)·Y, as Bendat and Piersol write it.
  The controller and sdynpy hold X·conj(Y), its elementwise conjugate,
  so the phase of one is the reverse of the other. The Rattlesnake
  specification file, the controller's `.nc4` and the sdynpy data array
  file are converted on the way in and on the way out. Every other
  format that can hold a cross term (the universal file, MATLAB, ADF,
  ESCDF) has the values as they are stored here, because none of them
  says which form it holds. A project saved before this conversion was
  added (2026-11-04) keeps a CPSD or specification imported from those
  files in the controller's form, and nothing in the file marks it, so
  import it again to correct it. A specification `.npz` exported before
  that date reads back conjugated, for the same reason.

## `geometry/`

    attrs   dimension, length_unit ('' = undeclared)
            from_deck and mass_unit, for a geometry read from a
              Nastran deck (only when set)
            mass_point, and mass and inertia when the rigid-body modes
              are to be mass normalized (only when set)
            view_eye, view_up      the view it opens on (only when set)
            solids                 JSON, the solids as their numbers
                                   were typed (only when there are any)
            mates                  JSON, the mated surfaces (only when
                                   there are any)
            group_solids           JSON, {element group id: the solids it
                                   was meshed from} (only when there are
                                   any); a group absent is found by its name
    dsets   node_id, node_xyz, node_def_cs, node_disp_cs, node_color,
            cs_id, cs_name, cs_type, cs_matrix,
            elem_id, elem_type, elem_color, elem_conn_flat / _offsets,
            elem_group         the element group of each element
            group_id, group_name   the element groups
    group_properties/<group id>/   (only element groups given properties)
            attrs   material_name, youngs_modulus, density, poissons_ratio,
                    modulus_of_rigidity (when set), thickness (plates) or
                    section_name + section [area, iy, iz, j] (beams),
                    and section_shape + section_dimensions when the
                    section was built from a shape (meters, in the
                    constructor's order),
                    orientation (beams, when set);
                    mass alone, kg, for a group of point masses;
                    stiffness, six values with NaN where a direction is
                    free, for a group of springs;
                    ground, the directions held ('X Y Z RX RY RZ'), for
                    a group of ground points

A `.vdyn` file writes `elem_group`, `group_id`, `group_name` and
`group_properties` as `elem_block`, `block_id`, `block_name` and
`block_properties`, and the object groups as `links`. The `.vdyn` reader
takes care of those, and of the tracelines in the oldest of those files; none of them appear in a `.hforge` file.

## `data/`, Every Measurement Class

One layout serves TimeHistory, Spectrum, Frf, Psd, Srs, Coherence,
MultipleCoherence, Specification, ShockSpecification,
TransientSpecification, SineLevel and SineTarget. The concrete class is
stored by name, because a Specification has the PSD function type and
the function type alone cannot say which class to rebuild:

    attrs   function_type        UFF dataset 58 code
            data_class           the class name, e.g. 'Psd'
    dsets   abscissa             one shared axis
            ordinate             records × points, real or complex;
                                 float64, or float32 for a time
                                 history converted from a control run,
                                 the precision it was recorded at
            response_dof         strings, one per record
            ordinate_dim, comment, ordinate_unit, reference_unit,
            dimension_hint       strings, '' = undefined / not stated

Present only where they mean something:

    dsets   reference_dof     records that have a reference (FRF, coherence)
            block             which repeat a record is, for stacked captures
            limit_<name>      a specification's warning and abort lines
            bandwidth         band widths, where the bins are not the
                              midpoints between lines (octave bands)
            statistical_dof   a PSD's statistical degrees of freedom,
                              one per record, where it was computed
                              from a time history
            line_correlation  where it was computed from a time history,
                              how its window ties neighboring lines:
                              the correlation at 0, 1, 2, ... lines
                              apart, starting at 1, one for every record
            shocks            (start, duration) rows, where events were marked
            segments, segment_kinds, segment_names   (start, end) rows,
                              where segments were marked, with each
                              mark's kind and name (strings, '' = unnamed)
            roles             strings, DOF, quantity and role in threes,
                              flat: each channel's role (reference,
                              response or monitor) where something has
                              set one. A channel not listed, or a file
                              without the dataset, takes the default for
                              its quantity (a force is a reference, a
                              motion a response, anything else a monitor)
            sine_seconds      a sine level's clock
            sine_floor, sine_below_floor   the noise floor beside each
                              sine reading, and where the tone was under it
    attrs   srs_q, srs_kind          what an SRS was computed with
            interpolation            how a specification reads between
                                     its lines
            band_constraints         JSON, how a specification's bands
                                     move together when edited
            sine_tone, sine_onset, sine_drift_hz   which tone a sine
                                     level belongs to, and where its
                                     sweep began
            sine_cycles (when set), sine_target_db, sine_refine,
            sine_chosen (when set)   the sine extraction setting
            source                   'SIM' for values from a simulated
                                     plant, 'HARDWARE' for values measured
                                     on real hardware; absent when not
                                     known. A Rattlesnake run sets it from
                                     its hardware, and what a project
                                     derives from it carries the same
            scale_db                 a comparison scale set by hand only;
                                     automatic scaling is never saved
            narrowbands              JSON, the narrow bands of random a
                                     mixed test lays over this floor
                                     (each a center, a width, a level and
                                     a sweep), when it has any
            tone_exclusion           the width, Hz, left out of this
                                     floor either side of a mixed test's
                                     tones, when set
            run_identity             JSON, a time history's run: axis,
                                     run, level_db, start and comment
                                     where known, "sources" (where each
                                     came from) and "refused" (what was
                                     not read, and why); absent when
                                     nothing has said. The level the
                                     comparison detects is never saved
            averaging_frame_length, _overlap, _window, _frames, _start,
            _detrend, _start_set, and _window_parameter when the window
            takes one
            filtering_low, _high (each when set), _order   the filter
                                     reading
            truncation_start, _stop  the truncate reading, when set

## `shapes/`

    attrs   mass_unit ('' = undeclared), unscaled (no drive point, so
            the modal masses are a convention, not a measurement)
    dsets   frequency, damping, shape_matrix (modes × DOFs, complex
            where a source carried complex modes), modal_mass
            (complex where measured), coordinate, comment, description
            modal_damping        only where a source carried one

## `channel_table/`

One table for a test and its analysis: what the analysis reads (each
channel's DOF, role, sensor and calibration) and, when the table has
been converted to carry them, the settings a controller runs each
channel at.

    attrs   column_order         every column, in the file's order
    dsets   channel              integers (the schema's one 'int' column)
            <every other column> strings, verbatim (node included: a
                                 node nobody recorded is blank, since 0
                                 cannot stand for "not stated")
    environments/<k>/            the control loops, when there are any,
                                 numbered because an environment's name
                                 is free text and may hold a slash
            attrs   name
            dsets   controls, drives   channel numbers
                    response_transformation, reference_transformation
                                       matrices, when set

Columns are read by the type they were stored as, so a column the
schema has since dropped still reads.

The columns are `channel`, `node`, `direction`, `role`, `control`,
`channel_type`, `unit`, `sensitivity` (mV per unit), `daq_range`,
`sensor_range` (volts), `serial_number`, `make`, `model`, `triax_dof`,
`expiration` and `comment`.

The controller's settings are an optional group of columns after them,
all present or none: `enabled` ('True' or 'False'), `drive` ('True' for
an output to a shaker amplifier), `physical` (the hardware channel,
'<device>/<channel>', blank when unbound), `iepe` ('True' for an IEPE
input) and `iepe_bias` (volts), `polarity` ('positive' or 'negative',
blank for positive), `range_max` (the input's full scale in the
channel's unit), `control_group`, and the abort limits `abort_accel_g`,
`abort_velocity_ips` and `abort_displacement_mil`, blank where none is
set. Their presence in `column_order` is the record that the table was
converted to carry them; a table that never was has none of them and
reads exactly as before. A channel's role in a control loop is read
from three columns: a drive is `drive`, a control channel is `control`,
a reference is a `role` of reference, and any other input is a monitor.
Converting a table adds the group at its defaults: every channel on,
none a drive, each full scale its clip level in volts over its
sensitivity where both are stated, and the rest blank.

## `breakpoint_table/`

One breakpoint table behind a test's profile, target or limits.

    attrs   column_order         the columns, in the file's order
            table_kind           which table it is: 'random_profile',
                                 'sine_profile', 'sine_limit',
                                 'srs_target', 'limit' or 'drive_gain'
            x_unit, y_unit       the units of the x column and of the
                                 levels ('' where the rows say, as a
                                 Sine profile's quantity does)
    dsets   <every column>       float64, NaN where the row has none;
                                 a text column (a Sine profile's
                                 quantity) as strings

The x column is `freq_hz`. The other columns are the table kind's: a
Random profile's `psd_g2_hz` and `slope_db_oct` (one of the two on each
row), a Sine profile's `quantity` (`accel_g`, `velocity_ips` or
`disp_pp_in`) and `value`, a Sine limit's `quantity` and `bound`, an
SRS target's `accel_g`, a limit's `ceiling`, a drive gain's `gain`
(g/V, from the drive to a limit channel), and on a profile's rows the
bands `alarm_below_db`, `alarm_above_db`, `abort_below_db` and
`abort_above_db`, in dB.

## `report/`, `photos/`, `matches/`

    report/   attrs: title, marking, marking_color ('ink' | 'red';
              marking '' = no strips), and when set: features (JSON
              list) and identity (JSON, the identification fields and
              the issue record);  dsets: blocks (one JSON string)
    photos/   dsets: names, formats, image_0000, ... (each photograph
              file's own bytes, verbatim, never re-encoded)
    matches/  attrs: first, second, first_geometry, second_geometry
              dsets: pairs (n × 2 mode indices), macs

## `sine_specification/`, `sine_levels/`

    sine_specification/  attrs: ordinate_dim, ordinate_unit, comment
                         dsets: response_dof, tone_order
                         tones/<k>/: attrs name, start_time; dsets
                         frequency, amplitude, phase, segment_type,
                         segment_rate, and any warning or abort limits
                         (the file's own breakpoints, per tone)
    sine_levels/         attrs: sine_cycles (when set)
                         dsets: tone_order; tones/<k>/ each an
                         ordinary data/ layout (the set only groups
                         them)

## `predictions/`

    predictions/  attrs: comment, count
                  <k>/: attrs record (one JSON string: channel,
                  quantity, unit, level_db, reference, source, customer,
                  frequency_uncertainty, level_uncertainty, and peaks,
                  each [frequency, level, Q, band] with null where not
                  stated); dsets curve_frequency, curve_values (a
                  predicted curve, when there is one)

A report's blocks may include one of kind `prediction`, naming the
predictions (`source`) and the measurement (`measured`) with the level
it was run at (`level`, dB relative to full level, or null).

## `run_log_extract/`

    run_log_extract/  attrs: path, test, upto, taken, log_size, after,
                      after_in_test, problems (a JSON list of strings)
                      dset log (one UTF-8 string: the snapshot, the
                      run log's own JSON Lines)

One test of a run log (`.hflog`, or an older `.vdlog`), frozen at
entry `upto`: `log` holds the log's header line and the lines of the
entries kept, exactly as the log file had them, so it reads with the run
log's own reader for its version and its chains verify. `path` is where
the log was, relative to the project file (with forward slashes) when
the log was in the project file's folder or one beneath it, absolute
otherwise. `taken` is when the snapshot was taken (ISO 8601 with its
offset), `log_size` the log file's size in bytes then, `after` and
`after_in_test` how many entries the file held after `upto` then, in all
and of the test, and `problems` the lines before `upto` skipped as not
entries.

## `run_record/`

    run_record/  attrs: test, run_uid, run_id, title, complete, outcome,
                 source ('SIM' or 'HARDWARE', when the run said)
                 dsets (each one UTF-8 string of JSON):
                   manifest   the run folder's run.json, whole
                   table      its table.json, whole: every channel row in
                              the control service's spelling, with any
                              column the channel table has none for
                   setup      its setup.json: config as accepted,
                              resolved, derived, feasibility, seeded_from
                   streams    {stream: its stream.json}, gaps and shots
                              included
                   results    {result: its <name>.json}, with a repeating
                              result's records under "records"
                   missing    [what the folder lacked, in words]
                   log        the run's log, JSON Lines, in order
                   end        its end.json (absent for a run that did not
                              end cleanly)
                   law        its law/law.json (absent when no law ran)
                 law_file     the law file's bytes, uint8; attr name
                              (absent unless the law is a file)
                 arrays/<k>/  one group per result no analysis class
                              holds (and a mapped result's other arrays);
                              attr result (its name); dsets <j>, each with
                              attr name, the array as written, a
                              repeating result's records first

The control service's own record of one run, kept whole beside the
objects the run converts to (below), so nothing in the run folder is
lost. `complete` is true for a run whose manifest says so and that has
an end; `outcome` is the end's own (`completed`, `aborted`,
`operator_stop`, and so on), '' when it has none.

## Run Folders and Their Conversion

A run folder (`hertzforge.io.run_folder`) is the layout a control run is
recorded in, one folder per run: `run.json` (the manifest, `schema`
`hertzforge-run/1`), `table.json`, `setup.json`, `law/`, `log.jsonl`,
`streams/<name>/` (`telemetry.f32`, little-endian float32,
frame-interleaved, in engineering units, and `stream.json`),
`results/<name>.json`, `.bin` and `.jsonl`, and `end.json`. The control
service's Random and Sine modules and its Recorder write one for every run, as
`docs/run-folder.md` describes; the other control modules do not yet.

`hertzforge.io.run_convert.convert` (and `hertzforge convert-run
FOLDER [-o OUTPUT]`) writes it as an ordinary project file, by default
`<run name>.hforge` inside the folder. Opening or importing the folder,
or its `run.json`, opens or imports that file, converting first when
there is none or the manifest was written after it. The project holds,
in this order, all in one object group named for the run:

- `Channel Table`, a `channel_table/` with the controller's columns.
  Each row maps so: `index` is `channel`, `description` is `comment`,
  `units` is `unit`, `sensitivity_mv_per_eu` is `sensitivity`,
  `physical` is `"<device>/<channel>"`, `interface` `iepe` sets `iepe`,
  `iepe_bias_v` is `iepe_bias`, the three `abort` limits are the abort
  columns, a `drive` row sets `drive` and a `control` row `control`; a
  row's `extra` columns fill the table's columns by name. A channel with
  no node has its channel number as its node.
- One time history per stream, named for the stream: values in SI,
  float32, each channel's declared unit kept, the run's source, its
  roles (a drive is a reference), the averaging the setup names, and a
  run identity whose run and start come from the controller. Time is
  from the run's start frame, or for a recording from its trigger, so
  its pre-trigger samples are at negative times. A gap is not padded:
  the samples after it follow on, and every record's comment says where
  it was and how many frames it lost. A stream of shots is one record
  per shot (`block` 'shot k'), each shot's first sample at minus its
  `pre_frames`, or one history per shot when the shots differ in
  length or timing.
- `Specification`, from a Random setup's resolved profile (each
  breakpoint's `freq_hz` and `psd_g2_hz`, with `alarm_db` and `abort_db`
  as `[below, above]` held from that breakpoint to the next) on the
  enabled control channels, its bands as warning and abort limits. For a
  Sine sweep or dwell, a `SineSweepSpecification` of one tone (`Tone 1`)
  on the enabled control channels in m/s**2, its breakpoints the points
  of `results/reference` (the profile as the service resolved it on the
  run's band) and its alarm and abort bands as the tone's warning and
  abort limits; a sweep's tone is one pass across the band in the
  direction the run started, at its rate and law (`resolved.test`), a
  dwell's holds its frequency for its dwell.
- `Sine Levels`, a `SineLevelSet`, for a Sine sweep's or step test's
  result of kind `sine_levels`: one `SineLevel` of `Tone 1` over the
  result's channels, complex (`values` at `phase`), in SI, its lines the
  points of the last pass at the highest level the run reached where
  the tone moved, ascending in frequency, with the time each was read.
  The result's arrays stay in the run record whole.
- A `Psd` for a result of kind `psd` or `cpsd`, an `Frf` for `frf`, a
  `Coherence` or `MultipleCoherence` for `coherence`, from the array
  named `values` (records last on the frequency axis), in SI by the
  channels' units. Cross terms are taken as written, already in this
  format's convention. A result whose stated unit is not the one its
  channels give, or of any other kind, is kept in the run record as
  arrays. A repeating result is one record per line of its `.jsonl`
  (`block` 'record k').
- `Run Record`, the `run_record/` above.

The file is written to a temporary name and renamed into place, with no
timestamps and in a fixed order: a failed conversion leaves no file, and
one folder converted twice gives the same bytes. A stream is copied a
block of at most 16 MiB at a time, so a run larger than memory
converts; opening the file reads a float32 stream into a float64 array,
twice its size on disk.

## MIMO Random Runs

A MIMO Random run is a run folder and converts as one ("Run Folders and Their
Conversion"): its reference becomes a `Specification` with its cross terms, its
identification an `Frf`, a `MultipleCoherence` and spectral matrices, its loops `Psd`
records, and the rest stays in the run record (`docs/run-folder.md`, "What a MIMO
Random Run Saves"). The separate `run/` group a MIMO run was once converted into is
retired, with its converter.

## Reading One Without Hertzforge

Because the layout is documented HDF5, this works:

```python
import h5py

with h5py.File('modal.hforge') as f:
    for key in sorted(f['objects']):
        group = f['objects'][key]
        print(group.attrs['name'], group.attrs['kind'])
    frf = f['objects/0003']                    # say it's the FRF
    H = frf['ordinate'][()]                    # records x frequencies
    freq = frf['abscissa'][()]
```

MATLAB reads the same file with `h5info('modal.hforge')` and `h5read`.
Nothing in a `.hforge` file needs Hertzforge to get it back out.
